//! Makes chdman, dolphinrvz and cemuwua loadable before anything calls them.
//!
//! romexplorer `dlopen`s each from `<app root>/lib/<os>/<arch>/`. A package that ships that
//! folder beside the executable needs nothing from here. A single-file build (the
//! `bundled-natives` feature) carries the libraries inside the executable instead; this
//! unpacks them once into the user's cache and points romexplorer's loader there through
//! `ROMEXPLORER_EXE_ROOT`, which `native_lib::exe_root()` checks before anything else.

#[cfg(feature = "bundled-natives")]
mod bundled {
    include!(concat!(env!("OUT_DIR"), "/natives.rs"));
}

/// Call once, first thing in `main`, before any thread exists (it may set an environment
/// variable).
pub fn ensure() -> Result<(), String> {
    #[cfg(feature = "bundled-natives")]
    unpack()?;
    Ok(())
}

#[cfg(feature = "bundled-natives")]
fn unpack() -> Result<(), String> {
    use crate::settings::{data_dir, exe_dir};

    // An explicit root, or a `lib/` beside the executable, wins over the embedded copy.
    if std::env::var_os("ROMEXPLORER_EXE_ROOT").is_some() || std::env::var_os("ROMEXPLORER_APP_ROOT").is_some() {
        return Ok(());
    }
    if exe_dir().is_some_and(|dir| dir.join("lib").is_dir()) {
        return Ok(());
    }

    // Keyed by version and by the packed sizes, so a rebuilt library under the same version
    // never loads a stale unpacked copy.
    let key = bundled::FILES.iter().fold(0u64, |acc, (_, raw, packed)| acc.wrapping_mul(1_000_003).wrapping_add(*raw).wrapping_add(packed.len() as u64));
    let root = data_dir().join(format!("natives-{}-{key:x}", crate::VERSION));
    let dir = root.join("lib").join(bundled::OS).join(bundled::ARCH);
    std::fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;

    for (name, raw_len, packed) in bundled::FILES {
        let path = dir.join(name);
        if std::fs::metadata(&path).is_ok_and(|meta| meta.len() == *raw_len) {
            continue;
        }
        let bytes = zstd::decode_all(*packed).map_err(|err| format!("unpacking {name}: {err}"))?;
        // Written under a per-process name and renamed into place, so two chdlite processes
        // starting together (the batch runner's workers) never see a half-written library.
        let partial = dir.join(format!("{name}.{}.part", std::process::id()));
        std::fs::write(&partial, bytes).map_err(|err| format!("{}: {err}", partial.display()))?;
        if std::fs::rename(&partial, &path).is_err() {
            // Another process won the race, or holds the library open: its copy is the same.
            let _ = std::fs::remove_file(&partial);
            if !path.is_file() {
                return Err(format!("could not install {}", path.display()));
            }
        }
    }
    std::env::set_var("ROMEXPLORER_EXE_ROOT", &root);
    Ok(())
}
