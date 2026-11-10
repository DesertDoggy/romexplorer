//! Nintendo 3DS titles: `.cia`, `.3ds` / `.cci`, `.cxi`, homebrew `.3dsx`, and Azahar's
//! compressed `.zcci` / `.zcia` / `.zcxi` / `.z3dsx`, decrypted into the format picked in the
//! settings ([`crate::settings::N3ds::format`]). The work is romexplorer's (`rom::n3ds`, on
//! cia-tool).
//!
//! A game becomes a cartridge image (`.cci` or `.3ds`), its main part alone (`.cxi`), or
//! either compressed (`.zcci`, `.zcxi`). An update, DLC, demo or system title is kept a CIA,
//! decrypted (`.cia`, or `.zcia` when the format compresses): its title id says which it is.
//!
//! No keys come with chdlite. `aes_keys.txt` (or `boot9.bin`) and `seeddb.bin` are looked
//! for beside each file, then beside chdlite and in its `assets` folder; nothing asks for
//! them, and a file whose keys are not there fails saying so.

use std::path::Path;

pub use romexplorer::rom::n3ds::Target;
use romexplorer::rom::n3ds::{self, KeyFiles, Keys};

/// What [`crate::settings::N3ds::format`] takes.
pub const FORMATS: [&str; 5] = ["cci", "3ds", "zcci", "cxi", "zcxi"];

pub fn is_3ds(path: &Path) -> bool {
    let ext = crate::classify::extension(path);
    n3ds::EXTENSIONS.contains(&ext.as_str())
}

/// The settings' format, or an error naming the choices.
pub fn target(format: &str) -> Result<Target, String> {
    Target::from_name(if format.trim().is_empty() { "cci" } else { format }).ok_or_else(|| format!("unknown 3DS format '{format}' ({})", FORMATS.join(", ")))
}

/// The keys for `input`, from the first key files found beside it, beside chdlite, or in
/// chdlite's data folder (the app's own folder on Android), each folder then its `assets`.
pub fn keys_for(input: &Path) -> Result<Keys, String> {
    let dirs = n3ds::key_dirs(input, &[crate::settings::data_dir()]);
    Keys::load(&KeyFiles::find(&dirs))
}

/// One line for the main window: "3DS .cci (updates / DLC .cia)".
pub fn summary(format: &str) -> String {
    match target(format) {
        Ok(t) if t.compresses() => format!("3DS .{} (updates / DLC .zcia)", t.name()),
        Ok(t) => format!("3DS .{} (updates / DLC .cia)", t.name()),
        Err(_) => format!("3DS '{format}'?"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_format_is_a_target_and_nothing_else_is() {
        for format in FORMATS {
            assert_eq!(target(format).unwrap().name(), format);
        }
        assert_eq!(target("").unwrap(), Target::Cci);
        assert!(target("zcia").is_err(), "a CIA is what an update stays, not a choice");
    }

    #[test]
    fn the_summary_says_where_updates_go() {
        assert_eq!(summary("zcci"), "3DS .zcci (updates / DLC .zcia)");
        assert_eq!(summary("3ds"), "3DS .3ds (updates / DLC .cia)");
    }
}
