//! Makes the music libraries loadable in the Android app, before anything calls them.
//!
//! romexplorer `dlopen`s each from `<app root>/lib/<os>/<arch>/`. On a desktop that folder is
//! beside the executable and nothing here is needed. On Android the libraries are in the APK,
//! which the system extracts into the app's flat native library directory (`lib/arm64/`). That
//! folder is read-only and has no `<os>/<arch>/` levels, so the app's files directory gets a
//! `natives/lib/android/arm64/` of symlinks into it, and the root points there. `dlopen`
//! follows a symlink to its real path, which is the one place an Android app is allowed to
//! load its own code from. (The same scheme as chdlite's `natives.rs`.)

use romexplorer::rom::native_lib::{arch_dir_name, os_dir_name, EXE_ROOT_OVERRIDE_ENV};

/// The libraries romexplorer looks for, under the names it looks for them: the two players
/// first (required), then the disc readers (each only needed for some inputs).
const APK_LIBRARIES: [&str; 6] = ["librommusic.so", "librvgmstream.so", "libchdman.so", "libdolphinrvz.so", "libcemuwua.so", "libz.so"];
const REQUIRED: usize = 2;

/// Call once, first thing in the app's `main`, before any thread exists (it sets environment
/// variables).
pub fn ensure() -> Result<(), String> {
    if std::env::var_os(EXE_ROOT_OVERRIDE_ENV).is_some() {
        return Ok(());
    }
    // Android has no /tmp and sets no TMPDIR; chdman needs one to read a CD CHD. One in the
    // app's own storage, emptied at each start.
    let tmp = crate::android::files_dir().join("tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|err| format!("{}: {err}", tmp.display()))?;
    std::env::set_var("TMPDIR", &tmp);
    let Some(source) = crate::android::native_lib_dir() else {
        return Err("cannot find the app's native library folder".to_string());
    };
    let root = crate::android::files_dir().join("natives");
    let dir = root.join("lib").join(os_dir_name()).join(arch_dir_name());
    std::fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let mut missing = Vec::new();
    for (i, name) in APK_LIBRARIES.iter().enumerate() {
        let target = source.join(name);
        let link = dir.join(name);
        // Relinked every start: an update installs the APK under a new path.
        let _ = std::fs::remove_file(&link);
        if !target.is_file() {
            if i < REQUIRED {
                missing.push(*name);
            }
            continue;
        }
        std::os::unix::fs::symlink(&target, &link).map_err(|err| format!("{}: {err}", link.display()))?;
    }
    std::env::set_var(EXE_ROOT_OVERRIDE_ENV, &root);
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("not in this APK: {}", missing.join(", ")))
    }
}
