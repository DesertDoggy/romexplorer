//! Build script: compiles the vendored LZMA SDK decoder (`LzmaDec.c`) plus a thin shim
//! ([`native/lzma_shim.c`]) into a static library and links it in, so CHD's `lzma`/`cdlz` codecs
//! can be decoded natively (see `src/modules/lzma_native.rs`) instead of shelling out to an
//! external tool.
//!
//! ## x86_64 hand-written ASM hook
//!
//! The upstream LZMA SDK ships an optional hand-written MASM-syntax x86_64 decode loop
//! (`external/libchdr/deps/lzma-26.02/Asm/x86/LzmaDecOpt.asm`,
//! `LzmaDec_DecodeReal_3`) that's faster than the portable C version compiled here. It requires a
//! MASM-compatible assembler (e.g. `uasm`/`ml64`; NASM syntax is NOT compatible) and only helps on
//! x86_64 -- it does nothing for the arm64 hosts this project targets today.
//!
//! Rather than wire that in now (it would add a new external assembler dependency for zero
//! benefit on the current target), this build script exposes the *hook* for it:
//! - Enable it with `--features lzma_dec_asm` (only takes effect on `target_arch = "x86_64"`).
//! - If enabled, this script looks for a MASM-compatible assembler on PATH (`uasm64`, `uasm`,
//!   `ml64`). If found, it assembles `LzmaDecOpt.asm` and compiles `LzmaDec.c` with
//!   `Z7_LZMA_DEC_OPT` defined so it calls into the ASM routine instead of its own C
//!   implementation.
//! - If no assembler is found (or the feature is off, or the target isn't x86_64), the build
//!   silently falls back to the portable C path -- this is never a hard error.

use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let libchdr_root = manifest_dir.join("external/libchdr");
    let lzma_root = libchdr_root.join("deps/lzma-26.02");
    let lzma_include = lzma_root.join("include");
    let lzma_src = lzma_root.join("src");

    let mut build = cc::Build::new();
    build
        .include(&lzma_include)
        .file(lzma_src.join("LzmaDec.c"))
        .file(manifest_dir.join("native/lzma_shim.c"))
        .include(manifest_dir.join("native"))
        .warnings(false);

    if try_enable_x86_64_asm_hook(&mut build, &lzma_root) {
        println!("cargo:warning=lzma_dec_asm: using hand-written x86_64 ASM decode loop");
    }

    build.compile("romexplorer_lzma");

    println!("cargo:rerun-if-changed=native/lzma_shim.c");
    println!("cargo:rerun-if-changed={}", lzma_src.join("LzmaDec.c").display());

    let libchdr_include = libchdr_root.join("include");
    let libchdr_src = libchdr_root.join("src");

    cc::Build::new()
        .include(&libchdr_include)
        .file(libchdr_src.join("libchdr_flac.c"))
        .file(manifest_dir.join("native/flac_shim.c"))
        .warnings(false)
        .compile("romexplorer_flac");

    println!("cargo:rerun-if-changed=native/flac_shim.c");
    println!("cargo:rerun-if-changed={}", libchdr_src.join("libchdr_flac.c").display());
}

/// Attempts to wire up the optional x86_64 ASM decode loop. Returns `true` if it was enabled.
/// Never fails the build -- falls back to the portable C path on any missing prerequisite.
fn try_enable_x86_64_asm_hook(build: &mut cc::Build, lzma_root: &Path) -> bool {
    if !cfg!(feature = "lzma_dec_asm") {
        return false;
    }
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("x86_64") {
        println!(
            "cargo:warning=lzma_dec_asm feature is enabled but target_arch is not x86_64; falling back to portable C LZMA decoder"
        );
        return false;
    }

    let Some(assembler) = find_masm_compatible_assembler() else {
        println!(
            "cargo:warning=lzma_dec_asm feature is enabled but no MASM-compatible assembler (uasm64/uasm/ml64) was found on PATH; falling back to portable C LZMA decoder"
        );
        return false;
    };

    let asm_file = lzma_root.join("Asm/x86/LzmaDecOpt.asm");
    if !asm_file.exists() {
        println!("cargo:warning=lzma_dec_asm: expected ASM source not found at {}", asm_file.display());
        return false;
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR set by cargo"));
    let obj_file = out_dir.join("LzmaDecOpt.o");

    // uasm/ml64-style invocation: assemble to an ELF/Mach-O object directly.
    // NOTE: this is a hook, not a validated end-to-end build -- the exact assembler flags may
    // need adjustment (e.g. -win64/-elf64/-macho64) once this is actually exercised on an x86_64
    // host with a MASM-compatible assembler installed.
    let status = std::process::Command::new(&assembler)
        .arg(format!("-D{}", target_os_abi_define()))
        .arg("-D_LZMA_DEC_OPT")
        .arg(format!("-Fo{}", obj_file.display()))
        .arg(&asm_file)
        .status();

    match status {
        Ok(status) if status.success() && obj_file.exists() => {
            build.define("Z7_LZMA_DEC_OPT", None);
            println!("cargo:rustc-link-arg={}", obj_file.display());
            true
        }
        _ => {
            println!(
                "cargo:warning=lzma_dec_asm: found assembler '{}' but assembling {} failed; falling back to portable C LZMA decoder",
                assembler.display(),
                asm_file.display()
            );
            false
        }
    }
}

fn find_masm_compatible_assembler() -> Option<PathBuf> {
    ["uasm64", "uasm", "ml64"]
        .into_iter()
        .find_map(|name| which(name))
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn target_os_abi_define() -> &'static str {
    if cfg!(target_os = "linux") {
        "ABI_LINUX"
    } else {
        "ABI_MACHO"
    }
}
