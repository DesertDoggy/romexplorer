use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/chdlite.ico");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    #[cfg(windows)]
    if target_os == "windows" {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/chdlite.ico");
        if let Err(err) = res.compile() {
            // No resource compiler on this machine: the tools still work, they just show the
            // default icon.
            println!("cargo:warning=chdlite icon not embedded: {err}");
        }
    }

    if env::var_os("CARGO_FEATURE_BUNDLED_NATIVES").is_some() {
        bundle_natives(&target_os);
    }
}

/// Writes `$OUT_DIR/natives.rs`: the target's chdman and dolphinrvz libraries, and the optional
/// ones (cemuwua, exiso, psxpbp, zlib-ng) that were built, zstd-compressed.
///
/// zstd -19, against CLAUDE.md's usual -3, because this runs once per build and is only ever
/// *decompressed* at run time, where the level costs nothing. Measured on the windows/x64
/// libraries (chdman.dll 4.47 MB + dolphinrvz.dll 13.39 MB):
///
/// | level | size    | compress | decompress |
/// |-------|---------|----------|------------|
/// | 3     | 7.11 MB | 0.05 s   | 24 ms      |
/// | 9     | 6.59 MB | 0.24 s   | 27 ms      |
/// | 19    | 5.90 MB | 4.3 s    | 30 ms      |
///
/// 1.2 MB off every shipped executable for four seconds of a release build.
fn bundle_natives(target_os: &str) {
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default().as_str() {
        "x86_64" => "x64".to_string(),
        "aarch64" => "arm64".to_string(),
        other => other.to_string(),
    };
    // The first two are required. The rest are optional: without one the executable still
    // converts everything else, and says which library is missing when it is needed --
    // cemuwua for Wii U discs and .zar, exiso for XISO, psxpbp for PBP, zlib-ng for CSO.
    const REQUIRED: usize = 2;
    let (os, names): (&str, [&str; 6]) = match target_os {
        "windows" => ("windows", ["chdman.dll", "dolphinrvz.dll", "cemuwua.dll", "exiso.dll", "psxpbp.dll", "zlib.dll"]),
        "macos" => ("macos", ["libchdman.dylib", "libdolphinrvz.dylib", "libcemuwua.dylib", "libexiso.dylib", "libpsxpbp.dylib", "libz.dylib"]),
        _ => ("linux", ["libchdman.so", "libdolphinrvz.so", "libcemuwua.so", "libexiso.so", "libpsxpbp.so", "libz.so"]),
    };
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let lib_dir = env::var_os("CHDLITE_NATIVE_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("..").join("app_root_ex").join("lib").join(os).join(&arch));
    println!("cargo:rerun-if-env-changed=CHDLITE_NATIVE_LIB_DIR");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let mut table = String::from("pub static FILES: &[(&str, u64, &[u8])] = &[\n");
    for (i, name) in names.iter().enumerate() {
        let source = lib_dir.join(name);
        println!("cargo:rerun-if-changed={}", source.display());
        if i >= REQUIRED && !source.is_file() {
            println!("cargo:warning=bundled-natives: {} not found, so this build goes without what it is for", source.display());
            continue;
        }
        let raw = fs::read(&source).unwrap_or_else(|err| {
            panic!(
                "bundled-natives: cannot read {} ({err}) -- run scripts/build_native_deps.sh and scripts/move_libs_to_app_root.sh for {os}/{arch}",
                source.display()
            )
        });
        let packed = zstd::encode_all(raw.as_slice(), 19).expect("zstd");
        let packed_path = out_dir.join(format!("{name}.zst"));
        fs::write(&packed_path, packed).expect("write packed native");
        table.push_str(&format!("    ({name:?}, {}, include_bytes!({:?})),\n", raw.len(), path_str(&packed_path)));
    }
    table.push_str("];\n");
    table.push_str(&format!("pub const OS: &str = {os:?};\npub const ARCH: &str = {arch:?};\n"));
    fs::write(out_dir.join("natives.rs"), table).expect("write natives.rs");
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
