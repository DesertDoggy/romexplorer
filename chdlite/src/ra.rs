//! The RetroAchievements hash of an input, and whether RetroAchievements has achievements
//! for it, from a hash list shipped with chdlite.
//!
//! The list is `radb.bin`, written by `scripts/update_ra_db.sh` from RetroAchievements' web
//! API. Like `fcdb.bin`, a copy in `<app root>/radb.bin` or `<app root>/assets/radb.bin` wins
//! over the one compiled in (`app_root_ex/assets/radb.bin` at build time), so a newer list can
//! be dropped beside the app. Its date (`yyyyMMdd`) is the list's version. A build with no
//! list still prints the hash, and says there is nothing to look it up in.
//!
//! # `radb.bin` format (version 1)
//!
//! ```text
//! offset  size  field
//! 0       4     magic: b"RADB"
//! 4       1     version: 1
//! 5       8     date: ASCII yyyyMMdd
//! 13      4     game_count: u32 LE
//! 17      ...   game_count games, each:
//!                 4     id: u32 LE            -- RetroAchievements game id
//!                 2     console: u16 LE       -- RetroAchievements console id
//!                 2     achievements: u16 LE
//!                 2     title_len: u16 LE
//!                 ..    title: UTF-8
//! ...     4     hash_count: u32 LE
//! ...     ...   hash_count hashes, each:
//!                 16    hash: the RA hash's 16 bytes (an MD5)
//!                 4     game: u32 LE          -- index into the games above
//! ```

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use romexplorer::common::models::Platform;
use romexplorer::rom::detect::{hinted_metadata_cycle, HeuristicFlags};
use romexplorer::rom::hash::{self, RahashError, RahashOutcome};

use crate::cart::Cart;

mod bundled {
    include!(concat!(env!("OUT_DIR"), "/radb.rs"));
}

const MAGIC: &[u8; 4] = b"RADB";
const VERSION: u8 = 1;

pub struct Game {
    pub id: u32,
    pub console: u32,
    pub achievements: u32,
    pub title: String,
}

pub struct List {
    /// `yyyyMMdd`: the day the list was made.
    pub version: String,
    games: Vec<Game>,
    by_hash: HashMap<[u8; 16], Vec<usize>>,
}

fn parse_hex(value: &str) -> Option<[u8; 16]> {
    let value = value.trim();
    if value.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(value.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

impl List {
    pub fn parse(buf: &[u8]) -> Result<Self, String> {
        let bad = |msg: &str| format!("radb.bin: {msg}");
        let mut at = 0usize;
        let mut take = |n: usize| -> Result<&[u8], String> {
            let slice = buf.get(at..at + n).ok_or_else(|| bad("truncated"))?;
            at += n;
            Ok(slice)
        };
        if take(4)? != MAGIC {
            return Err(bad("bad magic (not a radb.bin file)"));
        }
        let version = take(1)?[0];
        if version != VERSION {
            return Err(bad(&format!("unsupported version {version} (expected {VERSION})")));
        }
        let date = String::from_utf8_lossy(take(8)?).into_owned();
        let u16_at = |b: &[u8]| u16::from_le_bytes([b[0], b[1]]);
        let u32_at = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let game_count = u32_at(take(4)?) as usize;
        let mut games = Vec::with_capacity(game_count.min(1 << 20));
        for _ in 0..game_count {
            let fixed = take(10)?;
            let (id, console, achievements, title_len) = (u32_at(&fixed[0..4]), u16_at(&fixed[4..6]), u16_at(&fixed[6..8]), u16_at(&fixed[8..10]));
            let title = String::from_utf8_lossy(take(title_len as usize)?).into_owned();
            games.push(Game { id, console: console.into(), achievements: achievements.into(), title });
        }
        let hash_count = u32_at(take(4)?) as usize;
        let mut by_hash: HashMap<[u8; 16], Vec<usize>> = HashMap::with_capacity(hash_count.min(1 << 22));
        for _ in 0..hash_count {
            let record = take(20)?;
            let game = u32_at(&record[16..20]) as usize;
            if game >= games.len() {
                return Err(bad("hash names a game that is not there"));
            }
            by_hash.entry(record[..16].try_into().unwrap()).or_default().push(game);
        }
        Ok(List { version: date, games, by_hash })
    }

    /// The games with this hash: on `console` when it is known, on any console otherwise.
    pub fn lookup(&self, value: &str, console: Option<u32>) -> Vec<&Game> {
        let Some(key) = parse_hex(value) else { return Vec::new() };
        self.by_hash
            .get(&key)
            .into_iter()
            .flatten()
            .map(|&i| &self.games[i])
            .filter(|game| console.is_none_or(|c| c == game.console))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.games.len()
    }

    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }
}

/// `<app root>/radb.bin`, then `<app root>/assets/radb.bin`: where an updated list goes.
fn on_disk() -> Option<Vec<u8>> {
    let root = romexplorer::rom::native_lib::exe_root()?;
    [root.join("radb.bin"), root.join("assets").join("radb.bin")].into_iter().find(|p| p.is_file()).and_then(|p| std::fs::read(p).ok())
}

/// The list: one beside the app, else the one compiled in, else none. A file that does not
/// read is reported once and passed over.
pub fn list() -> Option<&'static List> {
    static LIST: OnceLock<Option<List>> = OnceLock::new();
    LIST.get_or_init(|| {
        for bytes in [on_disk(), bundled::EMBEDDED.map(<[u8]>::to_vec)].into_iter().flatten() {
            match List::parse(&bytes) {
                Ok(list) => return Some(list),
                Err(err) => eprintln!("chdlite: {err}"),
            }
        }
        None
    })
    .as_ref()
}

/// The list's version (`yyyyMMdd`), or `None` without a list.
pub fn list_version() -> Option<&'static str> {
    list().map(|l| l.version.as_str())
}

/// One line for `--version` and the settings: the list's date and size, or that there is none.
pub fn list_summary() -> String {
    match list() {
        Some(list) => format!("RetroAchievements list {} ({} games)", list.version, list.len()),
        None => "no RetroAchievements list".to_string(),
    }
}

/// The platform RetroAchievements hashes `path` as: a cartridge dump by its extension, anything
/// else by romexplorer's detection.
fn platform_of(path: &Path) -> Platform {
    match Cart::of(path) {
        Some(Cart::Nes) => Platform::Nes,
        Some(Cart::N64) => Platform::Nintendo64,
        Some(Cart::Snes) => Platform::Snes,
        None => hinted_metadata_cycle(path, None, HeuristicFlags::ALL).platform,
    }
}

/// The Hash output's RetroAchievements lines for `path`. `file_md5` is the MD5 the same pass
/// worked out when the input is one file: for most cartridge systems that is the RA hash, and
/// for an input whose platform is not known it is looked up on every console.
pub fn lines(path: &Path, file_md5: Option<&str>) -> Vec<String> {
    let platform = platform_of(path);
    let console = hash::ra_console_id(platform);
    let shown = if platform == Platform::Unknown { "platform not recognised".to_string() } else { platform.display_name().to_string() };
    let value = match console {
        Some(console) if hash::ra_hash_is_file_md5(console) => match file_md5 {
            Some(md5) => md5.to_ascii_lowercase(),
            None => return vec!["  rahash  not worked out: no single file to hash".to_string()],
        },
        Some(console) => match romexplorer::rom::ra_disc::ra_hash(path, console) {
            Ok(RahashOutcome::Done(value)) if !value.is_empty() => value.to_ascii_lowercase(),
            Ok(_) => return vec![format!("  rahash  not worked out for this file ({shown})")],
            Err(RahashError::Unavailable(why)) => return vec![format!("  rahash  not available for {shown}: {why}")],
            Err(RahashError::Failed(why)) => return vec![format!("  rahash  failed ({shown}): {why}")],
        },
        None => match file_md5 {
            Some(md5) => md5.to_ascii_lowercase(),
            None => return vec!["  rahash  platform not recognised".to_string()],
        },
    };
    let mut lines = vec![format!("  rahash  {value}  ({shown})")];
    let Some(list) = list() else {
        lines.push("  ra      not checked: no RetroAchievements list (radb.bin)".to_string());
        return lines;
    };
    let version = &list.version;
    let games = list.lookup(&value, console);
    if games.is_empty() {
        lines.push(format!("  ra      no achievements for this hash (list {version})"));
    }
    for game in games {
        lines.push(format!("  ra      {} -- {} achievements (game {}, list {version})", game.title, game.achievements, game.id));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(out: &mut Vec<u8>, id: u32, console: u16, achievements: u16, title: &str) {
        out.extend(id.to_le_bytes());
        out.extend(console.to_le_bytes());
        out.extend(achievements.to_le_bytes());
        out.extend((title.len() as u16).to_le_bytes());
        out.extend(title.as_bytes());
    }

    #[test]
    fn the_list_is_read_with_its_date_and_looked_up_per_console() {
        let mut buf = b"RADB\x0120261110".to_vec();
        buf.extend(2u32.to_le_bytes());
        game(&mut buf, 1, 7, 40, "Some Game");
        game(&mut buf, 2, 3, 12, "Other Game");
        buf.extend(3u32.to_le_bytes());
        for (hex, index) in [("aaaa0000aaaa0000aaaa0000aaaa0000", 0u32), ("bbbb1111bbbb1111bbbb1111bbbb1111", 0), ("cccc2222cccc2222cccc2222cccc2222", 1)] {
            buf.extend(parse_hex(hex).unwrap());
            buf.extend(index.to_le_bytes());
        }
        let list = List::parse(&buf).unwrap();
        assert_eq!(list.version, "20261110");
        assert_eq!(list.len(), 2);
        assert_eq!(list.lookup("AAAA0000AAAA0000AAAA0000AAAA0000", Some(7)).first().map(|g| g.title.as_str()), Some("Some Game"));
        assert_eq!(list.lookup("bbbb1111bbbb1111bbbb1111bbbb1111", None).len(), 1);
        assert!(list.lookup("aaaa0000aaaa0000aaaa0000aaaa0000", Some(3)).is_empty(), "another console's game is not a match");
        assert!(list.lookup("dddd3333dddd3333dddd3333dddd3333", None).is_empty());
        assert!(List::parse(&buf[..buf.len() - 3]).is_err(), "a truncated file is refused");
    }
}
