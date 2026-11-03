//! ROM hacks, translations, homebrew and the tools and saves around a console are catalogued
//! by collectors in Logiqx DATs that no site serves, laid out for a file manager rather than
//! for a catalogue: one DAT per system, a "machine" per *folder* -- `Hacks/Some Game` holding
//! forty unrelated hacks of it, `Trainers/Some Game`, `Dashboards/Skins/...` -- and every
//! version of a patch as a ROM of its own. Imported as they are, a folder would be one
//! release of forty files and nothing would say which entries are games.
//!
//! [`convert_folders`] reads such folders and writes one DAT per kind, each covering every
//! system:
//!
//! | file | holds |
//! |---|---|
//! | `hacks.dat` | hacks, mods and translations |
//! | `homebrew.dat` | homebrew, arcade ports, WiiWare |
//! | `etc.dat` | map packs, DLC installers, tools |
//! | `trainers.dat`, `saves.dat`, `source.dat` | what they say |
//!
//! In them an entry is one *work* -- every version of a patch together, each ROM marked with
//! the version it is (`variant`) -- and says outright what the import would otherwise have to
//! guess: its system, its kind (`<category>`), the game it was made from (`cloneof`), its
//! region and, for a translation, its language. What a file was converted from is not
//! recorded in it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use lazy_static::lazy_static;
use regex::Regex;

use super::dat_parse::{self, ParsedFile, ParsedGame};
use super::error::DatabaseError;
use super::patch_names::{self, PatchKind};
use super::platform_map;
use crate::common::models::Platform;

/// What an entry is: the `<category>` it is written with, and the file it goes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Hack,
    Mod,
    Translation,
    Homebrew,
    Arcade,
    WiiWare,
    MapPack,
    Dlc,
    Tool,
    Trainer,
    Save,
    Source,
}

impl Kind {
    /// The word `platform_map::kind_from_category` reads.
    pub fn category(self) -> &'static str {
        match self {
            Self::Hack => "Hack",
            Self::Mod => "Mod",
            Self::Translation => "Translation",
            Self::Homebrew => "Homebrew",
            Self::Arcade => "Arcade",
            Self::WiiWare => "WiiWare",
            Self::MapPack => "Map Pack",
            Self::Dlc => "DLC",
            Self::Tool => "Tool",
            Self::Trainer => "Trainer",
            Self::Save => "Save",
            Self::Source => "Source",
        }
    }

    /// The DAT it is written to, without `.dat`.
    pub fn file(self) -> &'static str {
        match self {
            Self::Hack | Self::Mod | Self::Translation => "hacks",
            Self::Homebrew | Self::Arcade | Self::WiiWare => "homebrew",
            Self::MapPack | Self::Dlc | Self::Tool => "etc",
            Self::Trainer => "trainers",
            Self::Save => "saves",
            Self::Source => "source",
        }
    }

    /// Its folders can be a game's, holding many works made for it (forty hacks of one
    /// game), where a folder of anything else is one thing in several files.
    fn holds_many(self) -> bool {
        matches!(self, Self::Hack | Self::Mod | Self::Translation | Self::Trainer | Self::Save)
    }

    /// Made by changing an existing game.
    fn is_patch(self) -> bool {
        matches!(self, Self::Hack | Self::Mod | Self::Translation)
    }

    /// Filed by what it is for (`Dashboards/Skins/Avalaunch/3D Anime`), so that the last
    /// folder alone does not name it.
    fn named_for_its_path(self) -> bool {
        matches!(self, Self::MapPack | Self::Dlc | Self::Tool | Self::Source)
    }

    /// Made for an existing game, which the folder names.
    fn has_base(self) -> bool {
        matches!(self, Self::Hack | Self::Mod | Self::Translation | Self::Trainer | Self::Save | Self::MapPack | Self::Dlc)
    }
}

/// The kind a top-level folder of a collector's DAT stands for. Plain words: what such
/// folders are commonly called.
pub fn kind_of_folder(folder: &str) -> Option<Kind> {
    Some(match folder.trim().to_ascii_lowercase().as_str() {
        "hacks" | "unofficial hacks" | "undub" | "undubs" | "fixes" | "unofficial" => Kind::Hack,
        "mods" => Kind::Mod,
        "translations" | "unofficial translations" | "hacks - translations" | "translated - hacks" => Kind::Translation,
        "homebrews" | "homebrew" => Kind::Homebrew,
        "arcade" => Kind::Arcade,
        "wiiware" => Kind::WiiWare,
        "maps packs" | "map packs" | "maps" => Kind::MapPack,
        "custom dlc installers" | "dlc" => Kind::Dlc,
        "dashboards" | "emulators" | "game save editors" | "offline installers" | "mods pc tools" | "mods files" | "packs" | "tools" => Kind::Tool,
        "trainers" => Kind::Trainer,
        "gamesaves" | "game saves" | "saves" => Kind::Save,
        "source codes" | "source code" | "sources" => Kind::Source,
        _ => return None,
    })
}

lazy_static! {
    static ref RE_GROUP: Regex = Regex::new(r"\s*(\[[^\]]*\]|\([^)]*\))").unwrap();
    static ref RE_SPACES: Regex = Regex::new(r"\s{2,}").unwrap();
    /// GoodTools' country letters in a group of their own: `(U)`, `(UE)`, `(JUE)`.
    static ref RE_GOOD_REGION: Regex = Regex::new(r"\(([UJEWKBACFGSI]{1,4})\)").unwrap();
    /// A file that is one part of a build, not a build of its own: `(Disk 2)`, `(Side B)`.
    /// or a folder called `CD2`.
    static ref RE_PART: Regex = Regex::new(r"(?i)\([^)]*\b(?:disc|disk|side|tape|part)\b[^)]*\)|^(?:cd|disc|disk) ?\d+$").unwrap();
    static ref RE_TRAILING_GROUPS: Regex = Regex::new(r"(?:\s*(?:\[[^\]]*\]|\([^)]*\)))+\s*$").unwrap();
    /// `Some Game by Someone`: who made a trainer, after the game it is for.
    static ref RE_BY: Regex = Regex::new(r"(?i)\s+by\s+[^()\[\]]+$").unwrap();
}

/// A DAT's name as the name of its system: `Nintendo - Game Boy Advance (2024)` and
/// `Nintendo - Game Boy Advance [Patched] Collection` are both the Game Boy Advance.
pub fn system_of(header_name: &str) -> String {
    let bare = RE_GROUP.replace_all(header_name, "");
    let bare = bare.replace("Collection", "");
    // "Sega - Mega CD Hacks": a DAT named for what it holds of a system.
    let bare = bare.trim().trim_end_matches(" Hacks").trim_end_matches(" Translations");
    RE_SPACES.replace_all(bare.trim().trim_end_matches('-').trim(), " ").into_owned()
}

/// `(U)` -> `USA`, `(UE)` -> `USA, Europe`: the region as the import reads it. `None` when
/// the name has no such group (a No-Intro name spells its region out, and the import reads
/// that itself).
pub fn goodtools_region(name: &str) -> Option<String> {
    let letters = RE_GOOD_REGION.captures(name)?;
    // Words that happen to be made of the letters: the import reads those itself.
    if matches!(&letters[1], "USA" | "UK") {
        return None;
    }
    let mut regions: Vec<&str> = Vec::new();
    for letter in letters[1].chars() {
        let region = match letter {
            'U' => "USA",
            'J' => "Japan",
            'E' => "Europe",
            'W' => "World",
            'K' => "Korea",
            'B' => "Brazil",
            'A' => "Australia",
            'C' => "China",
            'F' => "France",
            'G' => "Germany",
            'S' => "Spain",
            'I' => "Italy",
            _ => return None,
        };
        if !regions.contains(&region) {
            regions.push(region);
        }
    }
    Some(regions.join(", "))
}

/// The language a folder is named for (`Translations/Arabic/...`, `.../Brazil/...`).
fn language_of_folder(folder: &str) -> Option<&'static str> {
    match folder.trim().to_ascii_lowercase().as_str() {
        "portuguese (brazil)" => Some("pt"),
        other => patch_names::language_of_word(other),
    }
}

/// Files that describe a work rather than being it.
const NOTE_EXTENSIONS: [&str; 6] = [".txt", ".nfo", ".diz", ".url", ".md", ".htm"];
/// A file that names the other files of a set: with one present, the files are one work.
const SHEET_EXTENSIONS: [&str; 7] = [".cue", ".gdi", ".m3u", ".ccd", ".mds", ".msu", ".toc"];

fn has_extension(name: &str, extensions: &[&str]) -> bool {
    let lower = name.to_ascii_lowercase();
    extensions.iter().any(|e| lower.ends_with(e))
}

/// `gba`, `7z`, `001` -- not the `0]` of `v1.0]` or the `02` of `v1.02`.
fn is_extension(text: &str) -> bool {
    let Some(first) = text.bytes().next() else { return false };
    text.len() <= 7
        && text.bytes().all(|b| b.is_ascii_alphanumeric())
        && (first.is_ascii_alphabetic() || matches!(text.to_ascii_lowercase().as_str(), "7z" | "3ds" | "3dsx" | "32x") || (text.len() == 3 && text.bytes().all(|b| b.is_ascii_digit())))
}

fn stem(file_name: &str) -> &str {
    let leaf = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    match leaf.rfind('.') {
        Some(dot) if dot > 0 && is_extension(&leaf[dot + 1..]) => &leaf[..dot],
        _ => leaf,
    }
}

fn extension(file_name: &str) -> String {
    let leaf = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    leaf[stem(leaf).len()..].to_ascii_lowercase()
}

/// Folders that hold translations and hacks alike.
const MIXED_FOLDERS: [&str; 2] = ["hacks - translations", "translated - hacks"];

/// File names that say nothing about what the file is.
const GENERIC_STEMS: [&str; 6] = ["default", "trainer", "boot", "eboot", "game", "patch"];

/// A folder that sorts by television standard: the region of what is under it.
fn region_of_folder(folder: &str) -> Option<&'static str> {
    match folder.trim().to_ascii_lowercase().as_str() {
        "ntsc" | "usa" => Some("USA"),
        "pal" | "europe" => Some("Europe"),
        "japan" | "jap" | "ntsc-j" => Some("Japan"),
        _ => None,
    }
}

/// A name without the groups at its end and without who made it: the game a trainer named
/// `Some Game by Someone (U) (lives)` is for.
fn bare_name(name: &str) -> Option<String> {
    let bare = RE_TRAILING_GROUPS.replace(name, "");
    let bare = RE_BY.replace(bare.trim(), "");
    Some(bare.trim().to_string()).filter(|b| !b.is_empty())
}

/// The folder a file of a machine sits in, when it sits in one.
fn top_folder(file_name: &str) -> Option<&str> {
    file_name.split_once(['/', '\\']).map(|(folder, _)| folder)
}

/// One build of a work: the files that together are one version of it.
#[derive(Debug, Clone)]
pub struct Build {
    /// The version, where the name states one.
    pub variant: Option<String>,
    /// What tells it from the work's other builds where the version does not: its file or
    /// folder.
    pub label: String,
    pub files: Vec<ParsedFile>,
}

/// One entry of an output DAT.
#[derive(Debug, Clone)]
pub struct Work {
    pub system: String,
    pub kind: Kind,
    pub name: String,
    pub base: Option<String>,
    pub region: Option<String>,
    /// (code, guessed)
    pub language: Option<(String, bool)>,
    pub builds: Vec<Build>,
}

impl Work {
    /// The same work met again -- another version in the same folder, the same name in
    /// another DAT: its builds join this one's.
    pub fn absorb(&mut self, other: Work) {
        self.builds.extend(other.builds);
        if self.base.is_none() {
            self.base = other.base;
        }
        if self.language.is_none() {
            self.language = other.language;
        }
        if self.region.is_none() {
            self.region = other.region;
        }
    }

    pub fn file_count(&self) -> usize {
        self.builds.iter().map(|b| b.files.len()).sum()
    }
}

/// The works one machine of a collector's DAT holds. `default_kind` is for a DAT with no
/// folders, whose own name says what it is.
///
/// A machine is one of two things, and only its shape says which. `Hacks/Some Game` holding
/// loose ROMs is a *game's folder*: each ROM is a hack of that game, a work of its own.
/// `Hacks/Some Game - Hard Mode [Hack v1.1]`, or a machine whose files sit in sub-folders,
/// or any machine of a DAT with no folders, is *one work* however many files it has -- its
/// discs, its data folders, or the same thing in two formats.
///
/// Saves and trainers are filed differently again: a save as `Gamesaves/Game/NTSC/What it
/// is`, always one save; a trainer under its own name (`Trainers/Game (+6 Trainer)`), or
/// loose in a folder that only sorts (`Trainers/Japan/No Menu`).
pub fn works_of(system: &str, default_kind: Kind, game: &ParsedGame) -> Result<Vec<Work>, String> {
    let segments: Vec<&str> = game.name.split(['/', '\\']).map(str::trim).filter(|s| !s.is_empty()).collect();
    let Some(leaf) = segments.last().copied() else { return Ok(Vec::new()) };
    let files: Vec<&ParsedFile> = game.files.iter().filter(|f| !has_extension(&f.name, &NOTE_EXTENSIONS)).collect();
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let foldered = segments.len() > 1;
    let kind = if foldered {
        let top = kind_of_folder(segments[0]).ok_or_else(|| segments[0].to_string())?;
        // `Translations/Undub/Some Game`: a folder of one kind of patch inside another's.
        let nested = segments[1..segments.len() - 1].iter().find_map(|s| kind_of_folder(s)).filter(|k| k.is_patch());
        if top.is_patch() { nested.unwrap_or(top) } else { top }
    } else if leaf.starts_with('_') {
        // "_System Docs": notes about the collection, not a work.
        return Ok(Vec::new());
    } else {
        default_kind
    };
    // `Translations/Arabic/Some Game`: a folder between the kind and the game names a language.
    let folder_language = if foldered { segments[1..segments.len() - 1].iter().find_map(|s| language_of_folder(s)) } else { None };

    let folders: BTreeSet<&str> = files.iter().filter_map(|f| top_folder(&f.name)).collect();
    let loose: Vec<&ParsedFile> = files.iter().copied().filter(|f| top_folder(&f.name).is_none()).collect();
    let sheet = files.iter().any(|f| has_extension(&f.name, &SHEET_EXTENSIONS));
    let tagged = |name: &str| patch_names::analyze(name).kind.is_some();
    // Loose files of different types and none named like a ROM: one thing in several files.
    let mixed = loose.iter().map(|f| extension(&f.name)).collect::<BTreeSet<_>>().len() > 1 && !loose.iter().any(|f| RE_GROUP.is_match(stem(&f.name)));
    // One work, or a game's folder? A sub-folder that is tagged as a patch where the machine
    // is not is a work inside a game's folder; any other sub-folder is part of the machine.
    let whole = !foldered
        || !kind.holds_many()
        || sheet
        || tagged(leaf)
        || (!folders.is_empty() && !folders.iter().any(|f| tagged(f)))
        // Its one file has its name: a folder made for the file, not for a game.
        || (files.len() == 1 && stem(&files[0].name).eq_ignore_ascii_case(leaf))
        || kind == Kind::Save
        || (kind == Kind::Trainer && (RE_GROUP.is_match(leaf) || mixed));
    // `Gamesaves/Some Game/NTSC/All Unlocked`: the folder that sorts by region, and the game
    // before it.
    let middle = if foldered { &segments[1..segments.len() - 1] } else { &segments[..0] };
    let folder_region = middle.iter().find_map(|s| region_of_folder(s));
    let filed_under = middle.iter().position(|s| region_of_folder(s).is_some()).and_then(|i| i.checked_sub(1)).map(|i| middle[i]).or_else(|| middle.first().copied().filter(|s| region_of_folder(s).is_none()));

    // (name, builds as (label, files), is the machine itself)
    let mut sets: Vec<(String, Vec<(String, Vec<&ParsedFile>)>, bool)> = Vec::new();
    let in_folder = |folder: &str| -> Vec<&ParsedFile> { files.iter().copied().filter(|f| top_folder(&f.name) == Some(folder)).collect() };
    if whole {
        let name = if foldered && kind.named_for_its_path() {
            segments[1..].join(" - ")
        } else if kind == Kind::Save && foldered {
            // Named for its game too: "All Unlocked" alone is every game's.
            let path: Vec<&str> = segments[1..].iter().copied().filter(|s| region_of_folder(s).is_none()).collect();
            match folder_region {
                Some(_) => format!("{} ({})", path.join(" - "), middle.iter().find(|s| region_of_folder(s).is_some()).copied().unwrap_or_default()),
                None => path.join(" - "),
            }
        } else {
            leaf.to_string()
        };
        // Its files are one build, unless they are plainly several: a folder each, or loose
        // files that are not each other's discs.
        let parts = folders.iter().any(|f| RE_PART.is_match(f)) || (!sheet && loose.iter().any(|f| RE_PART.is_match(&f.name)));
        let one_build = !foldered || !kind.holds_many() || files.len() == 1 || parts || (sheet && folders.len() < 2) || loose.len() > 8 || kind == Kind::Save || (kind == Kind::Trainer && mixed);
        let builds = if one_build {
            vec![(leaf.to_string(), files.clone())]
        } else {
            let mut builds: Vec<(String, Vec<&ParsedFile>)> = folders.iter().map(|folder| (folder.to_string(), in_folder(folder))).collect();
            if sheet && !loose.is_empty() {
                builds.push((leaf.to_string(), loose.clone()));
            } else {
                builds.extend(loose.iter().map(|f| (stem(&f.name).to_string(), vec![*f])));
            }
            builds
        };
        sets.push((name, builds, true));
    } else {
        // "default.xbe" is named by its folder.
        let named = |f: &ParsedFile| if GENERIC_STEMS.contains(&stem(&f.name).to_ascii_lowercase().as_str()) { leaf.to_string() } else { stem(&f.name).to_string() };
        sets.extend(loose.iter().map(|f| (named(f), vec![(stem(&f.name).to_string(), vec![*f])], false)));
        sets.extend(folders.iter().map(|folder| (folder.to_string(), vec![(folder.to_string(), in_folder(folder))], false)));
    }

    let mut works = Vec::with_capacity(sets.len());
    for (raw_name, builds, is_machine) in sets {
        let info = patch_names::analyze(&raw_name);
        // A translation filed among the hacks is a translation. An undub or a bug fix filed
        // among the translations is not one -- where the filing is not to be taken at its
        // word: a folder of both, or a DAT with no folders, named for most of what it holds.
        let kind = match (kind, info.kind) {
            (Kind::Hack | Kind::Mod | Kind::WiiWare, Some(PatchKind::Translation)) => Kind::Translation,
            (Kind::Translation, said) if said != Some(PatchKind::Translation) && folder_language.is_none() && (!foldered || MIXED_FOLDERS.contains(&segments[0].to_ascii_lowercase().as_str())) => Kind::Hack,
            (k, _) => k,
        };
        let patched = matches!(kind, Kind::Hack | Kind::Mod | Kind::Translation);
        // Versions are of patched games only: "[t1]" and "[t2]" are two trainers, and taking
        // the tag off would leave a trainer with the name of the game it is for.
        let (name, variant) = if patched { (info.work.clone(), info.variant.clone()) } else { (raw_name.clone(), None) };
        let language = match kind {
            Kind::Translation => info.language.clone().or_else(|| folder_language.map(|l| (l.to_string(), false))),
            _ => info.language.clone().filter(|(_, stated)| *stated),
        }
        .map(|(code, stated)| (code, !stated));
        let base = match kind {
            // The machine is the work, so its folder is not the game it was made from: that
            // is what its name says before the patch's own tags.
            _ if patched && is_machine => info.base.clone().or_else(|| if foldered { bare_name(&raw_name) } else { None }),
            Kind::Save if foldered => Some(filed_under.unwrap_or(leaf).to_string()),
            // A trainer in its game's folder, or else one that names its game itself.
            Kind::Trainer if foldered && !is_machine && raw_name.to_lowercase().starts_with(&leaf.to_lowercase()) => Some(leaf.to_string()),
            Kind::Trainer if foldered => bare_name(&raw_name),
            Kind::MapPack | Kind::Dlc if foldered => Some(filed_under.unwrap_or(leaf).to_string()),
            _ if foldered && kind.has_base() => Some(leaf.to_string()),
            _ => info.base.clone(),
        };
        works.push(Work {
            system: system.to_string(),
            kind,
            name,
            base,
            region: goodtools_region(&raw_name).or_else(|| folder_region.filter(|_| matches!(kind, Kind::Save | Kind::Trainer)).map(str::to_string)),
            language,
            builds: builds.into_iter().map(|(label, files)| Build { variant: variant.clone(), label, files: files.into_iter().cloned().collect() }).collect(),
        });
    }
    Ok(works)
}

#[derive(Debug, Default, Clone)]
pub struct ConvertSummary {
    pub dats_read: usize,
    /// Output file -> (entries, ROMs).
    pub written: BTreeMap<String, (usize, usize)>,
    /// Entries per kind.
    pub kinds: BTreeMap<&'static str, usize>,
    /// Systems the app has no platform for, with how many entries each: imported as
    /// platform unknown until `platform_map` learns the name.
    pub unknown_systems: BTreeMap<String, usize>,
    /// Top-level folders that are no kind this knows, with how many machines: left out.
    pub unknown_folders: BTreeMap<String, usize>,
    /// Machines with nothing but notes, or that are notes about the collection.
    pub dropped: usize,
    /// Trainers and saves left out because their ROMs are a game's own.
    pub games_filed_as_extras: usize,
}

/// What a DAT with no folders holds, by its own name.
fn default_kind_of(header_name: &str) -> Kind {
    let lower = header_name.to_ascii_lowercase();
    if lower.contains("[t-") || lower.contains("translation") {
        Kind::Translation
    } else if lower.contains("homebrew") {
        Kind::Homebrew
    } else {
        Kind::Hack
    }
}

fn dat_files(folder: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            dat_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "dat" | "xml" | "zip")) {
            out.push(path);
        }
    }
}

/// Reads every DAT under `inputs` (sub-folders and zip packs included) and writes the DATs of
/// the module doc into `out`. Works of the same name on the same system are one entry,
/// whichever DATs they came from; a ROM listed twice is written once.
pub fn convert_folders(inputs: &[PathBuf], out: &Path) -> Result<ConvertSummary, DatabaseError> {
    let mut paths = Vec::new();
    for input in inputs {
        dat_files(input, &mut paths);
    }
    paths.sort();
    let mut summary = ConvertSummary::default();
    // (output file, system, name) -> work
    let mut merged: BTreeMap<(&'static str, String, String), Work> = BTreeMap::new();
    for path in &paths {
        for entry in dat_parse::parse_dat_path(path, &|_| true)? {
            summary.dats_read += 1;
            let system = system_of(&entry.dat.header.name);
            let default_kind = default_kind_of(&entry.dat.header.name);
            for game in &entry.dat.games {
                match works_of(&system, default_kind, game) {
                    Ok(works) if works.is_empty() => summary.dropped += 1,
                    Ok(works) => {
                        for work in works {
                            let key = (work.kind.file(), work.system.clone(), work.name.to_lowercase());
                            match merged.get_mut(&key) {
                                Some(existing) => existing.absorb(work),
                                None => {
                                    merged.insert(key, work);
                                }
                            }
                        }
                    }
                    Err(folder) => *summary.unknown_folders.entry(folder).or_default() += 1,
                }
            }
        }
    }

    // A hack kept beside its trainers is listed under both: it is the hack, not a trainer.
    let games: HashSet<String> = merged.values().filter(|w| w.kind.is_patch()).flat_map(|w| w.builds.iter()).flat_map(|b| b.files.iter()).filter_map(file_key).collect();

    std::fs::create_dir_all(out)?;
    let mut by_file: BTreeMap<&'static str, Vec<Work>> = BTreeMap::new();
    for ((file, _, _), mut work) in merged {
        if matches!(work.kind, Kind::Trainer | Kind::Save) {
            for build in &mut work.builds {
                build.files.retain(|f| !file_key(f).is_some_and(|k| games.contains(&k)));
            }
            if work.file_count() == 0 {
                summary.games_filed_as_extras += 1;
                continue;
            }
        }
        settle_variants(&mut work);
        if platform_map::platform_from_dat_name(&work.system) == Platform::Unknown {
            *summary.unknown_systems.entry(work.system.clone()).or_default() += 1;
        }
        *summary.kinds.entry(work.kind.category()).or_default() += 1;
        by_file.entry(file).or_default().push(work);
    }
    for (file, works) in &by_file {
        let roms: usize = works.iter().map(Work::file_count).sum();
        std::fs::write(out.join(format!("{file}.dat")), render(file, works))?;
        summary.written.insert(format!("{file}.dat"), (works.len(), roms));
    }
    Ok(summary)
}

/// What a ROM is known by: its strongest hash.
fn file_key(file: &ParsedFile) -> Option<String> {
    file.sha1.as_ref().or(file.md5.as_ref()).or(file.crc32.as_ref()).map(|h| h.to_ascii_lowercase())
}

/// Makes the builds of a work tell apart: a ROM listed twice is kept once, and when a work
/// has more than one build each has a variant of its own (its file's or folder's name where
/// the work's name gave none, or gave two builds the same), since files with the same
/// variant read as the parts of one build that all have to be held.
pub fn settle_variants(work: &mut Work) {
    let mut seen: HashSet<String> = HashSet::new();
    for build in &mut work.builds {
        build.files.retain(|f| seen.insert(file_key(f).unwrap_or_else(|| format!("{}:{:?}", f.name, f.size))));
    }
    work.builds.retain(|b| !b.files.is_empty());
    if work.builds.len() < 2 {
        return;
    }
    let mut count: HashMap<Option<String>, usize> = HashMap::new();
    for build in &work.builds {
        *count.entry(build.variant.clone()).or_default() += 1;
    }
    let mut used: HashSet<String> = HashSet::new();
    for build in &mut work.builds {
        let clash = build.variant.is_none() || count.get(&build.variant).copied().unwrap_or(0) > 1;
        let mut text = if clash { build.label.clone() } else { build.variant.clone().unwrap_or_default() };
        text.truncate(text.char_indices().nth(64).map_or(text.len(), |(i, _)| i));
        // Two builds whose names only differ past the column's width still need telling apart.
        let mut unique = text.clone();
        let mut n = 2;
        while !used.insert(unique.to_lowercase()) {
            let suffix = format!(" #{n}");
            let keep = text.char_indices().nth(64 - suffix.len()).map_or(text.len(), |(i, _)| i);
            unique = format!("{}{suffix}", &text[..keep]);
            n += 1;
        }
        build.variant = Some(unique);
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// One output DAT as Logiqx XML.
pub fn render(file: &str, works: &[Work]) -> String {
    let mut xml = String::with_capacity(works.len() * 400);
    xml.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<datafile>\n\t<header>\n");
    xml.push_str(&format!("\t\t<name>{file}</name>\n\t\t<description>{file}</description>\n\t</header>\n"));
    for work in works {
        xml.push_str(&format!("\t<game name=\"{}\"", escape(&work.name)));
        if let Some(base) = &work.base {
            xml.push_str(&format!(" cloneof=\"{}\"", escape(base)));
        }
        xml.push_str(">\n");
        xml.push_str(&format!("\t\t<description>{}</description>\n", escape(&work.name)));
        xml.push_str(&format!("\t\t<system>{}</system>\n", escape(&work.system)));
        xml.push_str(&format!("\t\t<category>{}</category>\n", work.kind.category()));
        if work.region.is_some() || work.language.is_some() {
            xml.push_str("\t\t<release");
            if let Some(region) = &work.region {
                xml.push_str(&format!(" region=\"{}\"", escape(region)));
            }
            if let Some((language, guessed)) = &work.language {
                xml.push_str(&format!(" language=\"{}\"", escape(language)));
                if *guessed {
                    xml.push_str(" guessed=\"1\"");
                }
            }
            xml.push_str("/>\n");
        }
        for (variant, f) in work.builds.iter().flat_map(|b| b.files.iter().map(move |f| (&b.variant, f))) {
            xml.push_str(&format!("\t\t<rom name=\"{}\"", escape(&f.name)));
            if let Some(size) = f.size {
                xml.push_str(&format!(" size=\"{size}\""));
            }
            for (attr, value) in [("crc", &f.crc32), ("md5", &f.md5), ("sha1", &f.sha1), ("sha256", &f.sha256)] {
                if let Some(value) = value {
                    xml.push_str(&format!(" {attr}=\"{}\"", escape(value)));
                }
            }
            if let Some(variant) = variant {
                xml.push_str(&format!(" variant=\"{}\"", escape(variant)));
            }
            xml.push_str("/>\n");
        }
        xml.push_str("\t</game>\n");
    }
    xml.push_str("</datafile>\n");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom(name: &str, sha1: &str) -> ParsedFile {
        ParsedFile { name: name.to_string(), size: Some(1), sha1: Some(sha1.to_string()), ..Default::default() }
    }

    fn machine(name: &str, roms: Vec<ParsedFile>) -> ParsedGame {
        ParsedGame { name: name.to_string(), files: roms, ..Default::default() }
    }

    #[test]
    fn a_folder_of_hacks_is_one_work_per_hack_and_one_work_per_patch() {
        let m = machine(
            "Hacks/Advance Wars 2 - Black Hole Rising",
            vec![
                rom("Advance Wars 2 - Black Hole Rising - Balance (V2) (Hack).gba", "a1"),
                rom("Advance Wars 2 - Black Hole Rising - Balance (V3.1) (Hack).gba", "a2"),
                rom("Advance Wars 2 - Black Hole Rising - Hard Mode (Hack).gba", "a3"),
                rom("readme.txt", "a4"),
            ],
        );
        let works = works_of("Nintendo - Game Boy Advance", Kind::Hack, &m).unwrap();
        // Three ROMs: the two versions of Balance are still two entries here (they are
        // folded by name when the whole folder has been read), Hard Mode a third.
        let names: Vec<(&str, Option<&str>)> = works.iter().map(|w| (w.name.as_str(), w.builds[0].variant.as_deref())).collect();
        assert_eq!(
            names,
            vec![("Advance Wars 2 - Black Hole Rising - Balance (Hack)", Some("V2")), ("Advance Wars 2 - Black Hole Rising - Balance (Hack)", Some("V3.1")), ("Advance Wars 2 - Black Hole Rising - Hard Mode (Hack)", None)]
        );
        assert!(works.iter().all(|w| w.kind == Kind::Hack && w.base.as_deref() == Some("Advance Wars 2 - Black Hole Rising")));
    }

    #[test]
    fn a_translation_says_its_language_and_how_sure_it_is() {
        let tagged = machine("Translations/Brazil/Batman - Rise of Sin Tzu", vec![rom("Batman - Rise of Sin Tzu (U) (M3) [T+Bra1.0_MestreSplinter].gba", "b1")]);
        let w = &works_of("Nintendo - Game Boy Advance", Kind::Hack, &tagged).unwrap()[0];
        assert_eq!((w.kind, w.language.clone(), w.region.as_deref()), (Kind::Translation, Some(("pt".to_string(), false)), Some("USA")), "the tag states it");
        assert_eq!((w.name.as_str(), w.builds[0].variant.as_deref()), ("Batman - Rise of Sin Tzu (U) (M3) [T+Bra_MestreSplinter]", Some("1.0")));
        assert_eq!(w.base.as_deref(), Some("Batman - Rise of Sin Tzu"));
        // Only the folder says so: a guess.
        let folder_only = machine("Translations/Arabic/Some Game", vec![rom("Some Game (U) (Arabic patch).gba", "b2")]);
        assert_eq!(works_of("X", Kind::Hack, &folder_only).unwrap()[0].language, Some(("ar".to_string(), true)));
        // A DAT with no folders, named for what it holds.
        let flat = machine("Chocobo Land (Japan) [T-En by Nezz v1.0]", vec![rom("Chocobo Land (Japan) [T-En by Nezz v1.0].gba", "b3")]);
        let w = &works_of("Nintendo - Game Boy Advance", Kind::Translation, &flat).unwrap()[0];
        assert_eq!((w.kind, w.name.as_str(), w.base.as_deref(), w.language.clone()), (Kind::Translation, "Chocobo Land (Japan) [T-En by Nezz]", Some("Chocobo Land (Japan)"), Some(("en".to_string(), false))));
        // A translation filed among the hacks is still a translation.
        let misfiled = machine("Hacks/Some Game", vec![rom("Some Game (J) [T-En by X].gba", "b4")]);
        assert_eq!(works_of("X", Kind::Hack, &misfiled).unwrap()[0].kind, Kind::Translation);
        // And an undub among the translations is a hack, in a DAT or a folder of both.
        let undub = machine("Some Game (USA) [Undub by X v1.01]", vec![rom("Some Game (USA) [Undub by X v1.01].iso", "b5")]);
        assert_eq!(works_of("X", Kind::Translation, &undub).unwrap()[0].kind, Kind::Hack);
        let both = machine("Hacks - Translations/Some Game (UNDUB)", vec![rom("Some Game (UNDUB).nds", "b6")]);
        assert_eq!(works_of("X", Kind::Hack, &both).unwrap()[0].kind, Kind::Hack);
        let filed = machine("Translations/Some Game", vec![rom("Some Game (J) (patched).gba", "b7")]);
        assert_eq!(works_of("X", Kind::Hack, &filed).unwrap()[0].kind, Kind::Translation, "a folder of translations is taken at its word");
        let nested = machine("Translations/Undub/Some Game (UNDUB)", vec![rom("Some Game (UNDUB).nds", "b8")]);
        let w = &works_of("X", Kind::Hack, &nested).unwrap()[0];
        assert_eq!((w.kind, w.name.as_str(), w.base.as_deref()), (Kind::Hack, "Some Game (UNDUB)", Some("Some Game")));
        // A trainer of a translation is a trainer, under its own name.
        let trainer = machine("Trainers/Japan/Menu", vec![rom("Some Game (J) [T+Eng_X] [t1].nes", "b9")]);
        let w = &works_of("X", Kind::Hack, &trainer).unwrap()[0];
        assert_eq!((w.kind, w.name.as_str(), w.builds[0].variant.as_deref()), (Kind::Trainer, "Some Game (J) [T+Eng_X] [t1]", None));
    }

    #[test]
    fn sets_tools_and_notes() {
        // A disc set is one work, named for its folder.
        let set = machine("Mods/Some Disc Game [Balanced by X]", vec![rom("game.cue", "c1"), rom("game (Track 1).bin", "c2"), rom("game (Track 2).bin", "c3")]);
        let works = works_of("Sony - PlayStation", Kind::Hack, &set).unwrap();
        assert_eq!((works.len(), works[0].builds.len(), works[0].file_count(), works[0].kind, works[0].name.as_str()), (1, 1, 3, Kind::Mod, "Some Disc Game [Balanced by X]"));
        // A tool is one work however many files, named for where it sits.
        let tool = machine("Dashboards/Skins/Avalaunch/3D Anime", vec![rom("skin.xml", "d1"), rom("back.png", "d2")]);
        let works = works_of("Microsoft - Xbox", Kind::Hack, &tool).unwrap();
        assert_eq!((works.len(), works[0].kind, works[0].name.as_str(), works[0].base.clone()), (1, Kind::Tool, "Skins - Avalaunch - 3D Anime", None));
        // Notes are not works; an unknown folder is said, not guessed.
        assert!(works_of("X", Kind::Translation, &machine("_Notes", vec![rom("a.txt", "e1")])).unwrap().is_empty());
        assert!(works_of("X", Kind::Hack, &machine("Hacks/Game", vec![rom("readme.txt", "e2")])).unwrap().is_empty());
        assert_eq!(works_of("X", Kind::Hack, &machine("Wallpapers/Game", vec![rom("a.bin", "e3")])).err(), Some("Wallpapers".to_string()));
    }

    #[test]
    fn versions_of_one_work_are_told_apart() {
        let file = |variant: Option<&str>, name: &str, sha1: &str| Build { variant: variant.map(str::to_string), label: stem(name).to_string(), files: vec![rom(name, sha1)] };
        let mut work = Work {
            system: "X".into(),
            kind: Kind::Hack,
            name: "Game - Balance (Hack)".into(),
            base: None,
            region: None,
            language: None,
            builds: vec![file(Some("V2"), "Game - Balance (V2) (Hack).gba", "f1"), file(Some("V3"), "Game - Balance (V3) (Hack).gba", "f2"), file(Some("V3"), "Game - Balance (V3) (Hack) [alt].gba", "f3"), file(Some("V2"), "copy.gba", "f1")],
        };
        settle_variants(&mut work);
        let variants: Vec<&str> = work.builds.iter().map(|b| b.variant.as_deref().unwrap()).collect();
        assert_eq!(variants, vec!["V2", "Game - Balance (V3) (Hack)", "Game - Balance (V3) (Hack) [alt]"], "the same ROM once; two builds both called V3 go by their file names");
        // One build needs no variant at all.
        let mut single = Work { builds: vec![file(None, "only.gba", "g1")], ..work.clone() };
        settle_variants(&mut single);
        assert_eq!(single.builds[0].variant, None);
        // A build of many files stays one build: its files share the variant.
        let folder = |label: &str, hashes: [&str; 2]| Build { variant: None, label: label.to_string(), files: hashes.iter().map(|h| rom(&format!("{label}/{h}.bin"), h)).collect() };
        let mut two = Work { builds: vec![folder("old", ["h1", "h2"]), folder("new", ["h1", "h3"])], ..work.clone() };
        settle_variants(&mut two);
        let files: Vec<(&str, usize)> = two.builds.iter().map(|b| (b.variant.as_deref().unwrap(), b.files.len())).collect();
        assert_eq!(files, vec![("old", 2), ("new", 1)], "what two builds share is listed with the first");
    }

    /// The shapes a machine comes in, each seen in real collections.
    #[test]
    fn a_machine_is_a_work_or_a_folder_of_them() {
        let names = |works: &[Work]| -> Vec<(String, Option<String>, usize, usize)> { works.iter().map(|w| (w.name.clone(), w.base.clone(), w.builds.len(), w.file_count())).collect() };
        let s = |v: &str| v.to_string();
        // Tagged as a patch itself: one work, and what it was made from is in its name, not
        // its folder. Its two disks are one build.
        let disks = machine("Translations/English/Some Game (Japan) [T-En by X v2.0] (Floppy)", vec![rom("Some Game (Japan) (Game Disk) [T-En by X v2.0].dsk", "a1"), rom("Some Game (Japan) (Data Disk).dsk", "a2")]);
        let works = works_of("X", Kind::Hack, &disks).unwrap();
        assert_eq!(names(&works), vec![(s("Some Game (Japan) [T-En by X]"), Some(s("Some Game (Japan)")), 1, 2)]);
        assert_eq!((works[0].kind, works[0].language.clone(), works[0].builds[0].variant.as_deref()), (Kind::Translation, Some((s("en"), false)), Some("v2.0 (Floppy)")));
        // Files in a sub-folder: one work named for the machine, however the folder is
        // called; the same thing loose in another format is another build of it.
        let wii = machine("Hacks/Some Hack Bros. Wii [ABCE01]", vec![rom("ABCE01/ABCE01.wbfs", "b1"), rom("Some Hack Bros. Wii [ABCE01].nkit.gcz", "b2")]);
        assert_eq!(names(&works_of("X", Kind::Hack, &wii).unwrap()), vec![(s("Some Hack Bros. Wii [ABCE01]"), Some(s("Some Hack Bros. Wii")), 2, 2)]);
        let mut mod_files = vec![rom("Some Game - Big Mod/default.xbe", "c0")];
        mod_files.extend((1..40).map(|i| rom(&format!("Some Game - Big Mod/data/{i}.bin"), &format!("c{i}"))));
        assert_eq!(names(&works_of("X", Kind::Hack, &machine("Mods/Some Game (Big Mod v1.5)", mod_files)).unwrap()), vec![(s("Some Game (Big Mod v1.5)"), Some(s("Some Game")), 1, 40)]);
        // A game's folder with a patch in a sub-folder of its own beside the loose ones.
        let mixed = machine("Translations/Some Game (Japan)", vec![rom("Some Game (Japan) [T-En by A v1.9].rom", "d1"), rom("Some Game (Japan) [T-En by B v2]/disk1.dsk", "d2"), rom("Some Game (Japan) [T-En by B v2]/disk2.dsk", "d3")]);
        assert_eq!(names(&works_of("X", Kind::Hack, &mixed).unwrap()), vec![(s("Some Game (Japan) [T-En by A]"), Some(s("Some Game (Japan)")), 1, 1), (s("Some Game (Japan) [T-En by B]"), Some(s("Some Game (Japan)")), 1, 2)]);
        // A DAT with no folders: a machine is a work, its disks parts of it. A version in
        // its name is not a file extension.
        let flat = machine("Some Game (Japan) [T-En by X v1.0]", vec![rom("Some Game (Japan) [T-En by X v1.0]", "e1")]);
        let works = works_of("X", Kind::Translation, &flat).unwrap();
        assert_eq!((names(&works), works[0].builds[0].variant.as_deref()), (vec![(s("Some Game (Japan) [T-En by X]"), Some(s("Some Game (Japan)")), 1, 1)], Some("v1.0")));
        let disks = machine("Some Port (1995) (Someone)", vec![rom("Some Port (1995) (Someone) (Disk 1).dsk", "f1"), rom("Some Port (1995) (Someone) (Disk 2).dsk", "f2")]);
        assert_eq!(names(&works_of("X", Kind::Homebrew, &disks).unwrap()), vec![(s("Some Port (1995) (Someone)"), None, 1, 2)]);
        // Homebrew is one thing in many files, named for its folder and not for theirs.
        let homebrew = machine("Homebrews/Some Shooter (v1.2)", vec![rom("apps/shooter/boot.dol", "g1"), rom("apps/shooter/icon.png", "g2"), rom("data/levels.bin", "g3")]);
        let works = works_of("X", Kind::Hack, &homebrew).unwrap();
        assert_eq!((names(&works), works[0].kind), (vec![(s("Some Shooter (v1.2)"), None, 1, 3)], Kind::Homebrew));
        // A save is filed under its game and a region, and is one save whatever it is made of.
        let save = machine("Gamesaves/Some Game/NTSC/All Unlocked", vec![rom("UDATA/4b4e0001/save.bin", "h1"), rom("UDATA/4b4e0001/meta.xbx", "h2"), rom("readme.jpg", "h3")]);
        let works = works_of("X", Kind::Hack, &save).unwrap();
        assert_eq!((names(&works), works[0].region.as_deref(), works[0].kind), (vec![(s("Some Game - All Unlocked (NTSC)"), Some(s("Some Game")), 1, 3)], Some("USA"), Kind::Save));
        // A trainer under its own name, its files saying nothing; trainers loose in a folder
        // that only sorts them; and one in its game's folder.
        let own = machine("Trainers/Some Game (+6 Trainer - Patch)", vec![rom("default.xbe", "i1"), rom("pjx", "i2")]);
        assert_eq!(names(&works_of("X", Kind::Hack, &own).unwrap()), vec![(s("Some Game (+6 Trainer - Patch)"), Some(s("Some Game")), 1, 2)]);
        let sorted = machine("Trainers/Japan/No Menu", vec![rom("Some Game by Someone (J) (lives).gba", "j1"), rom("Other Game (J) (time).gba", "j2")]);
        let works = works_of("X", Kind::Hack, &sorted).unwrap();
        assert_eq!((names(&works), works[0].region.as_deref()), (vec![(s("Some Game by Someone (J) (lives)"), Some(s("Some Game")), 1, 1), (s("Other Game (J) (time)"), Some(s("Other Game")), 1, 1)], Some("Japan")));
        let filed = machine("Trainers/Some Game", vec![rom("Some Game (J) [t1].gba", "k1")]);
        assert_eq!(names(&works_of("X", Kind::Hack, &filed).unwrap()), vec![(s("Some Game (J) [t1]"), Some(s("Some Game")), 1, 1)]);
        let generic = machine("Trainers/Xbox Trainers/somegame-pal", vec![rom("default.xbe", "l1")]);
        assert_eq!(names(&works_of("X", Kind::Hack, &generic).unwrap()), vec![(s("somegame-pal"), Some(s("somegame-pal")), 1, 1)]);
        // A map is named for where it is filed, and is for the game it is filed under.
        let map = machine("Maps Packs/Some Shooter/Dark Winter", vec![rom("deltatap.map", "m1")]);
        let works = works_of("X", Kind::Hack, &map).unwrap();
        assert_eq!((names(&works), works[0].kind), (vec![(s("Some Shooter - Dark Winter"), Some(s("Some Shooter")), 1, 1)], Kind::MapPack));
        assert_eq!((stem("Game v1.02"), stem("Game v2.0b"), stem("Game [T-En v1.0]"), stem("Game.7z"), stem("a/b.part.001")), ("Game v1.02", "Game v2.0b", "Game [T-En v1.0]", "Game", "b.part"));
    }

    #[test]
    fn systems_regions_and_the_written_file() {
        assert_eq!(system_of("Nintendo - Game Boy Advance (2024)"), "Nintendo - Game Boy Advance");
        assert_eq!(system_of("Nintendo - Game Boy Advance [Patched] Collection"), "Nintendo - Game Boy Advance");
        assert_eq!(system_of("Nintendo - Super Famicom - Speed Hacks"), "Nintendo - Super Famicom - Speed");
        assert_eq!(system_of("Sega - Mega CD Hacks (2024)"), "Sega - Mega CD");
        for name in ["Nintendo - Super Famicom - Speed", "Nintendo - Famicom", "NEC - PC Engine CD - TurboGrafx 16 CD", "Microsoft - MSX-Turbo-R", "Panasonic - 3DO Interactive Multiplayer", "NEC - PC-9801"] {
            assert_ne!(platform_map::platform_from_dat_name(name), Platform::Unknown, "{name}");
        }
        assert_eq!(goodtools_region("Game (UE) (M5) [T+Bra_X]").as_deref(), Some("USA, Europe"));
        assert_eq!(goodtools_region("Game (Japan) [T-En by X]"), None);
        assert_eq!(goodtools_region("Game (USA) [Undub]"), None, "a word, not three letters");
        assert_eq!(goodtools_region("Game (V1.0) (M3)"), None);

        let work = Work {
            system: "Nintendo - Game Boy Advance".into(),
            kind: Kind::Translation,
            name: "Tom & Jerry <Deluxe> (Japan) [T-En by X]".into(),
            base: Some("Tom & Jerry <Deluxe> (Japan)".into()),
            region: None,
            language: Some(("en".into(), false)),
            builds: vec![Build { variant: Some("v1.0".into()), label: "a".into(), files: vec![rom("a \"quoted\".gba", "h1")] }],
        };
        let xml = render("hacks", &[work]);
        let parsed = dat_parse::parse_dat_xml(&xml).unwrap();
        let g = &parsed.games[0];
        assert_eq!((parsed.header.name.as_str(), g.name.as_str(), g.cloneof.as_deref()), ("hacks", "Tom & Jerry <Deluxe> (Japan) [T-En by X]", Some("Tom & Jerry <Deluxe> (Japan)")));
        assert_eq!((g.system.as_deref(), g.category.as_deref(), g.language.as_deref(), g.language_guessed), (Some("Nintendo - Game Boy Advance"), Some("Translation"), Some("en"), false));
        assert_eq!((g.files[0].name.as_str(), g.files[0].variant.as_deref(), g.files[0].sha1.as_deref()), ("a \"quoted\".gba", Some("v1.0"), Some("h1")));
        // Nothing in the file but what it holds.
        assert!(!xml.contains("author") && !xml.contains("comment") && !xml.contains("homepage"));
    }
}
