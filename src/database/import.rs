//! Loads a [`ParsedDatFile`] into the catalog tables: `release` (one per SKU, multi-disc
//! entries rolled up), `content` + `hashes` (deduped on the strongest hash), the
//! `release_contents` junction, `title` + `title_releases`, and a `dat_imports` record.
//!
//! Dedup rule (docs/plan/database.md): two DAT lines are the same `content` row only when
//! they agree on the strongest hash both carry (sha256 > sha1 > md5); a crc32-only line
//! never dedups by hash -- it is only ever reused through its own `(release, file_name)`
//! slot on re-import. Titles are keyed by normalized base name *within a platform* at
//! import time (cross-platform merging of simultaneous releases vs. keeping ports apart is
//! a later, provider-informed step, not something a DAT can tell us).
//!
//! Everything is batched: per platform group, per chunk of `batch_size` games, one
//! multi-row `INSERT ... ON DUPLICATE KEY UPDATE` per table and one `IN (...)` select per
//! lookup, inside one transaction per DAT. Multi-row inserts rely on MariaDB handing out
//! consecutive auto-increment ids for a single statement (`innodb_autoinc_lock_mode=1`,
//! which `server.rs` sets).

use std::collections::{HashMap, HashSet};

use lazy_static::lazy_static;
use regex::Regex;
use sha1::{Digest, Sha1};
use sqlx::mysql::{MySql, MySqlPool};
use sqlx::{Row, Transaction};

use crate::common::models::{Language, Platform, Region};

use super::dat_download::Source;
use super::dat_parse::{ParsedDatFile, ParsedFile, ParsedGame};
use super::error::DatabaseError;
use super::matching::name_parse::Edition;
use super::matching::{self, Variant};
use super::patch_names;
use super::platform_map;

#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// Force every game in the DAT onto one platform (otherwise mapped from the header, or
    /// per game for retrobios).
    pub platform_override: Option<Platform>,
    pub batch_size: usize,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self { platform_override: None, batch_size: 500 }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ImportSummary {
    pub platform: Vec<Platform>,
    pub games: u64,
    pub files: u64,
    pub releases_upserted: u64,
    pub content_new: u64,
    pub content_reused: u64,
    /// Files whose hash changed upstream under the same name: recorded as new content, the
    /// old one superseded (below).
    pub content_replaced: u64,
    /// Files this DAT no longer lists for a release it still lists (a bad dump replaced, a
    /// track dropped), moved to `release_contents_superseded`.
    pub content_superseded: u64,
    /// Superseded files this DAT lists again, moved back onto their release.
    pub content_restored: u64,
    /// Why nothing was superseded although this DAT leaves files out: it was cut short, or it
    /// would take off too many at once. The files stay on their releases.
    pub supersede_held: Option<String>,
    pub hashes_inserted: u64,
    pub titles_new: u64,
    pub ra_hashes: u64,
    /// Releases an earlier import left on no platform, folded into this one's.
    pub platformless_folded: u64,
}

pub fn sha1_hex(text: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(text.as_bytes());
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn placeholders(groups: usize, per_group: usize) -> String {
    let group = format!("({})", vec!["?"; per_group].join(","));
    vec![group; groups].join(",")
}

fn in_list(n: usize) -> String {
    vec!["?"; n].join(",")
}

/// Ensures `name` has a `sources` row and returns its id.
pub async fn ensure_source(pool: &MySqlPool, name: &str) -> Result<u8, DatabaseError> {
    Ok(super::lookup_or_insert_name(pool, "sources", name).await? as u8)
}

lazy_static! {
    static ref RE_DISC_TAG: Regex = Regex::new(r"(?i)\s*\((?:Disc|Disk|CD|Side|Cart|Tape)\s+[^)]*\)").unwrap();
}

/// A DAT game name with its "(Disc N)"/"(Side A)" group removed -- the key several
/// per-disc DAT lines share when they are one shipped release.
pub fn release_name_of(game_name: &str) -> String {
    RE_DISC_TAG.replace_all(game_name, "").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The platform a DAT game belongs to: override > per-game (retrobios path) > header.
///
/// `pub` (not `pub(crate)`): reused as-is by `tools/biosdb_gen.rs`, a separate `[[bin]]`
/// target that depends on this crate like any external caller would -- `pub(crate)`
/// would not be visible to it.
pub fn platform_for(dat: &ParsedDatFile, game: &ParsedGame, source: Source, options: &ImportOptions) -> Platform {
    if let Some(p) = options.platform_override {
        return p;
    }
    color_system(dat_platform(dat, game, source), game)
}

fn dat_platform(dat: &ParsedDatFile, game: &ParsedGame, source: Source) -> Platform {
    // A DAT that covers several systems names each entry's (`hacks.dat`).
    if let Some(system) = game.system.as_deref() {
        return platform_map::platform_from_dat_name(system);
    }
    if source == Source::Retrobios {
        if let Some(path) = &game.description {
            let parts: Vec<&str> = path.split('/').collect();
            if parts.len() >= 3 {
                return platform_map::platform_from_dat_name(&format!("{} - {}", parts[1], parts[2]));
            }
        }
        return Platform::Unknown;
    }
    platform_map::platform_from_dat_name(&dat.header.name)
}

/// Handhelds whose colour model a DAT may list together with the original: RetroAchievements
/// has one WonderSwan and one Neo Geo Pocket for both, so its WonderSwan Color games came in as
/// WonderSwan. The colour model's own file kind says which: (system, extension, colour model).
pub const COLOR_SYSTEMS: [(Platform, &str, Platform); 2] = [(Platform::WonderSwan, "wsc", Platform::WonderSwanColor), (Platform::NeoGeoPocket, "ngc", Platform::NeoGeoPocketColor)];

/// `platform`, or its colour model when every file of `game` is that model's kind.
pub fn color_system(platform: Platform, game: &ParsedGame) -> Platform {
    for (mono, ext, color) in COLOR_SYSTEMS {
        if platform == mono && !game.files.is_empty() && game.files.iter().all(|f| f.name.rsplit_once('.').is_some_and(|(_, e)| e.eq_ignore_ascii_case(ext))) {
            return color;
        }
    }
    platform
}

/// What `source`'s DAT lists when it is not software (`platform_map::material_from_dat_name`),
/// by its header name or, failing that, the name of the file it came in. The file name counts
/// so that [`unclassified_members`], which has only file names to go by, never keeps finding a
/// DAT that importing would then not stamp.
pub fn dat_material(source: Source, header_name: &str, entry_name: &str) -> Option<&'static str> {
    let tosec_style = matches!(source, Source::Tosec | Source::MameRedump);
    let member = entry_name.rsplit_once("::").map_or(entry_name, |(_, member)| member);
    // Lost Level Archive's "(Extras)" DATs are the manuals, maps and box scans of its games
    // (1,889 of their 2,400 files are PDFs): documents, whatever one entry holds.
    if source == Source::LostLevel && [header_name, member].iter().any(|n| n.to_ascii_lowercase().contains("(extras)")) {
        return Some("document");
    }
    platform_map::material_from_dat_name(header_name, tosec_style).or_else(|| platform_map::material_from_dat_name(member, tosec_style))
}

/// The DATs in `path` that were imported before releases recorded what kind of material they
/// are, and that list something other than software: their pack member names (the file's own
/// name for a plain DAT). Importing just those again stamps their releases; an import is an
/// upsert, so nothing else about them changes.
pub async fn unclassified_members(pool: &MySqlPool, source: Source, path: &std::path::Path) -> Result<Vec<String>, DatabaseError> {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let is_zip = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("zip"));
    let members: Vec<String> = if is_zip { super::dat_parse::list_dat_members(path)? } else { vec![file_name.clone()] };
    let wanted: Vec<String> = members.into_iter().filter(|m| dat_material(source, "", m).is_some()).collect();
    if wanted.is_empty() {
        return Ok(wanted);
    }
    let Some(row) = sqlx::query("SELECT id FROM sources WHERE name = ?").bind(source.name()).fetch_optional(pool).await? else { return Ok(Vec::new()) };
    let source_id: u8 = row.try_get("id")?;
    let unstamped: std::collections::HashSet<String> = sqlx::query_scalar("SELECT file_hash FROM dat_imports WHERE source_id = ? AND material IS NULL").bind(source_id).fetch_all(pool).await?.into_iter().collect();
    Ok(wanted.into_iter().filter(|m| unstamped.contains(&sha1_hex(&if is_zip { format!("{file_name}::{m}") } else { m.clone() }))).collect())
}

fn tosec_category(header_name: &str) -> Option<String> {
    header_name.rsplit_once(" - ").map(|(_, c)| c.trim().to_string())
}

/// One release row's worth of data, aggregated from every DAT game that rolls up into it.
struct ReleaseDraft {
    name: String,
    name_hash: String,
    norm_name: String,
    norm_hash: String,
    title_name: String,
    region: Option<String>,
    languages: Option<String>,
    revision: Option<String>,
    year: Option<u16>,
    serial: Option<String>,
    category: Option<String>,
    /// `platform_map::material_from_dat_name` of the DAT listing it: `None` for software.
    material: Option<&'static str>,
    /// `release.game_kind`: what a game is when it is not a commercial release.
    game_kind: Option<&'static str>,
    /// `release.languages_state` for a patched release: `confirmed` | `guessed`.
    languages_state: Option<&'static str>,
    /// The game it was made from, as the DAT names it (`release.base_hint`).
    base_hint: Option<String>,
    disc_count: u16,
    is_beta: bool,
    is_prototype: bool,
    is_demo: bool,
    is_unlicensed: bool,
    /// A pirate copy (`(Pirate)`, TOSEC `[p]`): unlicensed too, and listed apart from it.
    is_pirate: bool,
    community_patched: bool,
    patch_type: Option<&'static str>,
    is_special_edition: bool,
    is_budget: bool,
    is_aftermarket: bool,
    is_rerelease: bool,
    /// `name_parse::revision_sort` of `revision`: orders versions within a title.
    revision_sort: u32,
    /// (file, ra_hash of the game it came from -- attached to the first file's content)
    files: Vec<(ParsedFile, Option<String>)>,
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if !list.iter().any(|v| v.eq_ignore_ascii_case(&value)) {
        list.push(value);
    }
}

/// Structured `<release region=...>` / `language=...` attributes come in every style a
/// source likes: full names ("USA", "Europe"), codes ("JP,ASIA"), and RetroAchievements'
/// `region="English"` -- a language, not a region. Normalizes both to the codes name
/// parsing produces (`Region::code` / `Language::code`), moving any language word found
/// in the region attribute over to `languages`. Unrecognized words are kept as given.
pub fn normalize_structured_tags(region: Option<&str>, language: Option<&str>) -> (Option<String>, Option<String>) {
    let split = |s: &str| s.split([',', '/', ';', '+']).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect::<Vec<_>>();
    let mut regions: Vec<String> = Vec::new();
    let mut languages: Vec<String> = Vec::new();
    for token in region.map(split).unwrap_or_default() {
        if let Some(l) = Language::from_name(&token) {
            push_unique(&mut languages, l.code().to_string());
        } else if let Some(r) = Region::from_name(&token) {
            push_unique(&mut regions, r.code().to_string());
        } else {
            push_unique(&mut regions, token);
        }
    }
    for token in language.map(split).unwrap_or_default() {
        if let Some(l) = Language::from_name(&token) {
            push_unique(&mut languages, l.code().to_string());
        } else {
            let by_code = Language::from_code(&token);
            push_unique(&mut languages, if by_code != Language::Other { by_code.code().to_string() } else { token.to_ascii_lowercase() });
        }
    }
    let join = |v: Vec<String>| if v.is_empty() { None } else { Some(v.join(",")) };
    (join(regions), join(languages))
}

/// `release.serial`'s width (migration 0037).
const SERIAL_MAX: usize = 1024;

/// A DAT's serial, cut to what `release.serial` holds.
///
/// Redump gives every serial a disc was sold under in one field ("SLPS-00001, SLPS-00002,
/// ..."). The column is wide enough for every list in today's sets, so this normally returns
/// the serial unchanged; it exists because the database is strict and a value one character
/// too long fails the whole DAT -- which is what happened to eight Redump sets when the
/// column was 64 wide. Past the width, whole serials are kept, as many as fit, in the DAT's
/// own order; a single serial longer than the column is cut at a character boundary. A
/// serial is a label, never worth losing a DAT over.
fn fit_serial(serial: &str) -> Option<String> {
    let serial = serial.trim();
    if serial.is_empty() {
        return None;
    }
    if serial.chars().count() <= SERIAL_MAX {
        return Some(serial.to_string());
    }
    let mut kept = String::new();
    for part in serial.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let extra = if kept.is_empty() { 0 } else { 2 };
        if kept.chars().count() + extra + part.chars().count() > SERIAL_MAX {
            break;
        }
        if !kept.is_empty() {
            kept.push_str(", ");
        }
        kept.push_str(part);
    }
    if kept.is_empty() {
        kept = serial.chars().take(SERIAL_MAX).collect();
    }
    Some(kept)
}

/// See the Lost Level Archive arm of [`draft_release`]'s kind.
fn lost_level_homebrew(name: &str, parsed: &matching::ParsedName) -> bool {
    let lower = name.to_ascii_lowercase();
    parsed.edition == Some(Edition::Aftermarket) || lower.contains("(homebrew)") || (parsed.variant.is_none() && !lower.contains("bootleg"))
}

fn draft_release(game: &ParsedGame, header_name: &str, source: Source, material: Option<&'static str>) -> ReleaseDraft {
    let parsed = matching::parse_name(&game.name);
    // What the entry is: its DAT's category where that is a vocabulary (the manual sources;
    // elsewhere only "Hack", "Translation" and "Homebrew" are taken at their word), then
    // what its own name says. A translation is a translation even when filed as a hack.
    let said = platform_map::kind_from_category(game.category.as_deref(), source.is_manual());
    let patch = patch_names::analyze(&game.name);
    let game_kind = match (patch.kind, said.game_kind) {
        (Some(patch_names::PatchKind::Translation), _) => Some("translation"),
        (_, Some(kind)) => Some(kind),
        (Some(patch_names::PatchKind::Hack), None) => Some("hack"),
        // Lost Level Archive is what No-Intro and Redump have no entry for: mostly new games
        // for old machines. Half say so ("(Aftermarket)", "(Homebrew)") and most of the rest
        // do not say anything ("Some Game (2021) (v1.1) (Author)"), so homebrew is what an
        // entry is unless its name marks it a prototype, beta, demo, bootleg or unlicensed
        // release of its day -- those keep their own flags and no kind. (Of 4,569 entries:
        // 1,117 tagged aftermarket, about 140 marked otherwise.)
        (None, None) if source == Source::LostLevel && lost_level_homebrew(&game.name, &parsed) => Some("homebrew"),
        (None, None) => None,
    };
    // What is made around a game -- a trainer, a save, a map -- is not a game of any kind,
    // whatever game its name says it is for ("... (SMB1 Hack) [t1]"). A patch file keeps
    // the kind: it is what the patch is.
    let extra = source.is_manual() && said.material.is_some_and(|m| m != "patch");
    let game_kind = game_kind.filter(|_| !extra);
    let patched = game_kind.is_some_and(|k| platform_map::PATCHED_KINDS.contains(&k));
    // Every version of a patch is one release (`patch_names`): the name is the work's, and
    // each file says which build it is. Only a patch: a trainer keeps its "[t1]", without
    // which it would have the name of the game it is for and be taken for it.
    let versioned = patched;
    let name = release_name_of(if versioned { &patch.work } else { &game.name });
    // Under the game it was made from, when the DAT names it: the original's own title if it
    // is in the catalogue, else one title for everything made from that game.
    let base_hint = if versioned || matches!(said.material, Some("trainer" | "save" | "mappack" | "dlc" | "patch")) { game.cloneof.clone().or_else(|| patch.base.clone()).filter(|b| !b.trim().is_empty()) } else { None };
    let title_name = match base_hint.as_deref().map(matching::parse_name) {
        Some(base) if !base.base.is_empty() => base.base,
        _ if parsed.base.is_empty() => name.clone(),
        _ => parsed.base.clone(),
    };
    // A RetroAchievements subset ("Game [Subset - Speedrun]") is a patched ROM: a title of its
    // own, never one with the game it was made from.
    let title_name = matching::with_subset_tag(&title_name, &game.name);
    let norm_name = matching::normalize_title(&title_name).to_lowercase();
    let (structured_region, structured_language) = normalize_structured_tags(game.region.as_deref(), game.language.as_deref());
    let region = structured_region.or_else(|| {
        if parsed.regions.is_empty() {
            None
        } else {
            Some(parsed.regions.iter().map(|r| r.code()).collect::<Vec<_>>().join(","))
        }
    });
    // A translation's language is the one it was translated into, not the original's --
    // which is what the name's own "(Japan)" or "(En,Fr)" is about. How far it is to be
    // believed is recorded (`languages_state`): stated by the DAT or the name, or a guess.
    let stated = structured_language.clone().map(|l| (l, !game.language_guessed)).or_else(|| patch.language.clone().filter(|_| patched));
    let languages_state = match (&stated, patched) {
        (Some((_, true)), true) => Some("confirmed"),
        (Some((_, false)), true) => Some("guessed"),
        _ => None,
    };
    let languages = stated.map(|(l, _)| l).or_else(|| {
        if parsed.languages.is_empty() || game_kind == Some("translation") {
            None
        } else {
            Some(parsed.languages.iter().map(|l| l.code()).collect::<Vec<_>>().join(","))
        }
    });
    let serial = game.files.iter().find_map(|f| f.serial.clone()).or_else(|| game.serial.clone()).or_else(|| parsed.serial.clone()).and_then(|s| fit_serial(&s));
    let revision = game.files.iter().find_map(|f| f.version.clone()).or_else(|| parsed.revision.clone());
    let category = game.category.clone().or_else(|| if source == Source::Tosec { tosec_category(header_name) } else { None });
    let disc_count = parsed.disc_total.or(parsed.disc).unwrap_or(1).max(1) as u16;
    // Every tag the name carries: "(Demo) (Unl)" is a demo and unlicensed both.
    let has = |v: Variant| parsed.has_variant(v);
    let (is_beta, is_prototype, is_demo, is_unlicensed) = (has(Variant::Beta), has(Variant::Proto), has(Variant::Demo) || has(Variant::Sample), has(Variant::Unlicensed) || has(Variant::Pirate));
    let is_pirate = has(Variant::Pirate);
    let (community_patched, patch_type) = if has(Variant::Translation) {
        (true, Some("translation"))
    } else if has(Variant::Hack) {
        (true, Some("gameplay"))
    } else {
        (false, None)
    };
    let (community_patched, patch_type) = match game_kind {
        Some("translation") => (true, Some("translation")),
        Some("hack" | "mod") => (true, patch_type.or(Some("gameplay"))),
        _ if extra => (false, None),
        _ => (community_patched, patch_type),
    };
    let game_kind = game_kind.or(match patch_type {
        Some("translation") => Some("translation"),
        Some(_) => Some("hack"),
        None => None,
    });
    let edition = parsed.edition;
    let revision_sort = crate::database::matching::name_parse::revision_sort(revision.as_deref());
    let mut files = Vec::with_capacity(game.files.len());
    for (index, file) in game.files.iter().enumerate() {
        let mut file = file.clone();
        if versioned && file.variant.is_none() {
            file.variant = patch.variant.clone();
        }
        files.push((file, if index == 0 { game.ra_hash.clone() } else { None }));
    }
    ReleaseDraft {
        name_hash: sha1_hex(&name),
        norm_hash: sha1_hex(&norm_name),
        name,
        norm_name,
        title_name,
        region,
        languages,
        revision,
        year: parsed.year,
        serial,
        // The DAT's kind first, then what the files plainly are: a magazine scan named
        // "... (Update)" is still a document.
        // A manual source's category is the whole answer -- a hack called "... (Update)" is
        // a hack. Anywhere, an entry of nothing but patch files is a patch.
        material: if source.is_manual() {
            said.material.or_else(|| platform_map::patch_from_release(game.files.iter().map(|f| f.name.as_str())))
        } else {
            // A machine's own system file first: "[BIOS] PS3 System Software Update (Update)"
            // is firmware, not an update to a game.
            (if source == Source::Retrobios { Some("bios") } else { platform_map::bios_from_release(&game.name, category.as_deref(), header_name) })
                .or(material)
                .or_else(|| platform_map::document_from_release(game.files.iter().map(|f| f.name.as_str()), category.as_deref()))
                .or_else(|| platform_map::patch_from_release(game.files.iter().map(|f| f.name.as_str())))
                .or_else(|| if game_kind.is_some() { None } else { platform_map::tagged_material(&game.name) })
                .or_else(|| if game_kind.is_some() { None } else { platform_map::addon_from_release(&game.name, category.as_deref()) })
        },
        game_kind,
        languages_state,
        base_hint: base_hint.map(|b| b.chars().take(512).collect()),
        category,
        disc_count,
        is_beta,
        is_prototype,
        is_demo,
        is_unlicensed,
        is_pirate,
        community_patched,
        patch_type,
        is_special_edition: edition == Some(Edition::Special),
        is_budget: edition == Some(Edition::Budget),
        is_aftermarket: edition == Some(Edition::Aftermarket),
        is_rerelease: edition == Some(Edition::Rerelease),
        revision_sort,
        files,
    }
}

fn merge_draft(into: &mut ReleaseDraft, other: ReleaseDraft) {
    into.disc_count = into.disc_count.max(other.disc_count);
    if into.region.is_none() {
        into.region = other.region;
    }
    if into.languages.is_none() {
        into.languages = other.languages;
    }
    if into.serial.is_none() {
        into.serial = other.serial;
    }
    if into.year.is_none() {
        into.year = other.year;
    }
    let seen: HashSet<String> = into.files.iter().map(|(f, _)| f.name.clone()).collect();
    for (file, ra) in other.files {
        if !seen.contains(&file.name) {
            into.files.push((file, ra));
        }
    }
}

/// Imports every game in `dat` under `source`. `entry_name` identifies the DAT file (or
/// pack member) for `dat_imports`.
pub async fn import_dat(pool: &MySqlPool, source: Source, entry_name: &str, dat: &ParsedDatFile, options: &ImportOptions) -> Result<ImportSummary, DatabaseError> {
    import_dat_reporting(pool, source, entry_name, dat, options, None).await
}

/// Where an import is, for a job's row: `label` and how many entries are done, on a scale
/// of `total` entries of which `before` were other DATs'.
pub struct EntryProgress<'a> {
    pub progress: &'a super::jobs::Progress,
    pub label: String,
    pub before: u64,
    pub total: u64,
}

/// [`import_dat`], reporting as it goes. A DAT is imported a batch of entries at a time, and
/// one DAT can be the whole import: `hacks.dat` is 70,000 entries and half a minute, during
/// which a job that only counted DATs said "1 of 1" and nothing else.
pub async fn import_dat_reporting(pool: &MySqlPool, source: Source, entry_name: &str, dat: &ParsedDatFile, options: &ImportOptions, report: Option<&EntryProgress<'_>>) -> Result<ImportSummary, DatabaseError> {
    let source_id = ensure_source(pool, source.name()).await?;
    let mut summary = ImportSummary { games: dat.games.len() as u64, files: dat.file_count() as u64, ..Default::default() };

    let material = dat_material(source, &dat.header.name, entry_name);
    // Group games by platform, rolling per-disc lines up into one release draft each.
    let mut by_platform: HashMap<Platform, Vec<ReleaseDraft>> = HashMap::new();
    let mut index: HashMap<(Platform, String), usize> = HashMap::new();
    for game in &dat.games {
        if game.files.is_empty() {
            continue;
        }
        let platform = platform_for(dat, game, source, options);
        let draft = draft_release(game, &dat.header.name, source, material);
        let key = (platform, draft.name_hash.clone());
        let drafts = by_platform.entry(platform).or_default();
        match index.get(&key) {
            Some(&i) => merge_draft(&mut drafts[i], draft),
            None => {
                drafts.push(draft);
                index.insert(key, drafts.len() - 1);
            }
        }
    }

    let mut tx = pool.begin().await?;
    let mut platforms: Vec<Platform> = by_platform.keys().copied().collect();
    platforms.sort_by_key(|p| p.igdb_id());
    // Drafts are fewer than entries (a multi-disc game is one): scaled back to entries, so
    // the count shown ends at the DAT's own.
    let drafts_total: u64 = by_platform.values().map(|d| d.len() as u64).sum();
    let mut drafts_done = 0u64;
    // Every (release, content) this DAT lists, for what it no longer does.
    let mut listed: Vec<(u64, u64)> = Vec::new();
    let key = dat_key(&dat.header.name);
    for platform in &platforms {
        let drafts = &by_platform[platform];
        for chunk in drafts.chunks(options.batch_size.max(1)) {
            if let Some(r) = report {
                let entries = if drafts_total == 0 { 0 } else { summary.games * drafts_done / drafts_total };
                r.progress.report(r.before + entries, Some(r.total), &format!("{}, {entries} of {} entries", r.label, summary.games)).await;
            }
            import_chunk(&mut tx, source_id, source, *platform, chunk, &key, &mut summary, &mut listed).await?;
            drafts_done += chunk.len() as u64;
        }
    }

    supersede_unlisted(&mut tx, source_id, &key, dat, &listed, &mut summary).await?;

    // Record the import.
    let file_hash = sha1_hex(entry_name);
    // A DAT whose games split between two models (RetroAchievements' WonderSwan) is still the
    // system its name says: recorded as that, or `misplaced_members` would import it forever.
    let named = dat_platform(dat, dat.games.first().unwrap_or(&ParsedGame::default()), source);
    let main_platform = if platforms.len() == 1 {
        platforms[0]
    } else if let Some(p) = options.platform_override {
        p
    } else if platforms.contains(&named) {
        named
    } else {
        Platform::Unknown
    };
    sqlx::query(
        "INSERT INTO dat_imports (source_id, app_platform_id, file_name, file_hash, dat_name, dat_version, game_count, file_count, material) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON DUPLICATE KEY UPDATE app_platform_id = VALUES(app_platform_id), dat_name = VALUES(dat_name), dat_version = VALUES(dat_version), game_count = VALUES(game_count), file_count = VALUES(file_count), material = VALUES(material), imported_at = CURRENT_TIMESTAMP",
    )
    .bind(source_id)
    .bind(main_platform.igdb_id())
    .bind(entry_name)
    .bind(&file_hash)
    .bind(&dat.header.name)
    .bind(&dat.header.version)
    .bind(summary.games as u32)
    .bind(summary.files as u32)
    .bind(material)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    // A collector's DAT can list the original beside the patch; importing it again lists
    // it again, and it is not a version of the patch (`release_bases::link_by_content`).
    if source.is_manual() {
        super::release_bases::link_by_content(pool).await?;
    }
    // An earlier import of this DAT that could not tell its system left its releases on no
    // platform; the same names are now listed under one, and the old rows fold into them.
    for platform in platforms.iter().filter(|p| **p != Platform::Unknown) {
        let hashes: Vec<&str> = by_platform[platform].iter().map(|d| d.name_hash.as_str()).collect();
        summary.platformless_folded += fold_platformless(pool, source_id, *platform, &hashes).await?;
    }
    summary.platform = platforms;
    Ok(summary)
}

/// At most this share of a DAT's files may be superseded by one import (with a floor, so a
/// small DAT can still fix a dump or two). More than that is not a DAT that fixed some dumps:
/// it is a broken one, or a different cut of the set, and taking that many files off would
/// be hard to undo. 2% is well above what one release of a maintained DAT changes.
const SUPERSEDE_SHARE: f64 = 0.02;
const SUPERSEDE_FLOOR: usize = 10;

lazy_static! {
    /// A DAT name's trailing date or count, "(20240101-123456)" or "(298)".
    static ref RE_DAT_STAMP: Regex = Regex::new(r"(?:\s*\(\d[\d\s\-_.:]*\))+\s*$").unwrap();
}

/// Which DAT this is, across its releases: its header name without the date or count a new
/// release changes. Two DATs of one source that list the same release ("(Encrypted)",
/// "(Decrypted)") have different keys.
pub fn dat_key(header_name: &str) -> String {
    sha1_hex(&RE_DAT_STAMP.replace(header_name.trim(), "").to_lowercase())
}

/// Files this DAT (`dat_key`) listed for a release it still lists, which it no longer does: moved
/// from `release_contents` to `release_contents_superseded` (once no other source lists them
/// there either). Only releases this DAT lists are looked at, so a release it leaves out
/// entirely keeps everything. Nothing is taken off when the DAT was cut short, or when it
/// would take off more than [`SUPERSEDE_SHARE`] of its files: `summary.supersede_held` says why.
async fn supersede_unlisted(tx: &mut Transaction<'_, MySql>, source_id: u8, dat_key: &str, dat: &ParsedDatFile, listed: &[(u64, u64)], summary: &mut ImportSummary) -> Result<(), DatabaseError> {
    let listed_set: HashSet<(u64, u64)> = listed.iter().copied().collect();
    let releases: Vec<u64> = listed.iter().map(|(r, _)| *r).collect::<HashSet<_>>().into_iter().collect();
    let mut unlisted: Vec<(u64, u64)> = Vec::new();
    for part in releases.chunks(1000) {
        let sql = format!("SELECT release_id, content_id FROM release_content_sources WHERE source_id = ? AND dat_key = ? AND release_id IN ({})", in_list(part.len()));
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(source_id).bind(dat_key);
        for id in part {
            query = query.bind(id);
        }
        for row in query.fetch_all(&mut **tx).await? {
            let pair: (u64, u64) = (row.try_get("release_id")?, row.try_get("content_id")?);
            if !listed_set.contains(&pair) {
                unlisted.push(pair);
            }
        }
    }
    if unlisted.is_empty() {
        return Ok(());
    }
    unlisted.sort_unstable();
    let limit = supersede_limit(dat.file_count());
    if dat.truncated {
        summary.supersede_held = Some(format!("the DAT ends early (a download cut short?): {} file(s) it leaves out stay on their releases", unlisted.len()));
        return Ok(());
    }
    if unlisted.len() > limit {
        summary.supersede_held = Some(format!("the DAT leaves out {} file(s) its releases had, more than the {limit} one import may take off: they stay on their releases", unlisted.len()));
        return Ok(());
    }
    for (release_id, content_id) in unlisted {
        sqlx::query("DELETE FROM release_content_sources WHERE release_id = ? AND content_id = ? AND source_id = ?").bind(release_id).bind(content_id).bind(source_id).execute(&mut **tx).await?;
        let others: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM release_content_sources WHERE release_id = ? AND content_id = ?").bind(release_id).bind(content_id).fetch_one(&mut **tx).await?;
        if others > 0 {
            continue;
        }
        let moved = sqlx::query(
            "INSERT INTO release_contents_superseded (release_id, content_id, file_name, source_id, variant, dat_name, dat_version) \
             SELECT release_id, content_id, file_name, source_id, variant, ?, ? FROM release_contents WHERE release_id = ? AND content_id = ? \
             ON DUPLICATE KEY UPDATE file_name = VALUES(file_name), dat_name = VALUES(dat_name), dat_version = VALUES(dat_version), superseded_at = CURRENT_TIMESTAMP",
        )
        .bind(&dat.header.name)
        .bind(&dat.header.version)
        .bind(release_id)
        .bind(content_id)
        .execute(&mut **tx)
        .await?;
        if moved.rows_affected() > 0 {
            sqlx::query("DELETE FROM release_contents WHERE release_id = ? AND content_id = ?").bind(release_id).bind(content_id).execute(&mut **tx).await?;
            summary.content_superseded += 1;
        }
    }
    Ok(())
}

/// How many files one import of a DAT with `files` files may supersede.
fn supersede_limit(files: usize) -> usize {
    ((files as f64 * SUPERSEDE_SHARE) as usize).max(SUPERSEDE_FLOOR)
}

/// Releases on no platform (`Unknown`) from `source` that the release of the same name on
/// `platform` now lists in full: folded into it (`release_merge::fold_pairs`). Returns how many.
async fn fold_platformless(pool: &MySqlPool, source_id: u8, platform: Platform, name_hashes: &[&str]) -> Result<u64, DatabaseError> {
    let mut folded = 0u64;
    for chunk in name_hashes.chunks(500) {
        let sql = format!(
            "SELECT u.id, k.id FROM `release` k JOIN `release` u ON u.app_platform_id = 0 AND u.name_hash = k.name_hash AND u.source_id = k.source_id AND u.id <> k.id \
             WHERE k.app_platform_id = ? AND k.source_id = ? AND k.name_hash IN ({}) \
             AND NOT EXISTS (SELECT 1 FROM release_contents a WHERE a.release_id = u.id AND NOT EXISTS (SELECT 1 FROM release_contents b WHERE b.release_id = k.id AND b.content_id = a.content_id))",
            in_list(chunk.len())
        );
        let mut q = sqlx::query_as::<_, (u64, u64)>(sqlx::AssertSqlSafe(sql)).bind(platform.igdb_id()).bind(source_id);
        for h in chunk {
            q = q.bind(*h);
        }
        let pairs = q.fetch_all(pool).await?;
        if !pairs.is_empty() {
            super::release_merge::fold_pairs(pool, &pairs).await?;
            folded += pairs.len() as u64;
        }
    }
    Ok(folded)
}

/// The DATs in `path` imported before their system had a platform here (or before the name
/// was understood): their pack member names (the file's own name for a plain DAT). Importing
/// them again puts their releases on the platform, and the old rows fold into the new ones.
pub async fn misplaced_members(pool: &MySqlPool, source: Source, path: &std::path::Path) -> Result<Vec<String>, DatabaseError> {
    let Some(row) = sqlx::query("SELECT id FROM sources WHERE name = ?").bind(source.name()).fetch_optional(pool).await? else { return Ok(Vec::new()) };
    let source_id: u8 = row.try_get("id")?;
    let rows: Vec<(String, Option<String>)> = sqlx::query_as("SELECT file_hash, dat_name FROM dat_imports WHERE source_id = ? AND app_platform_id = 0").bind(source_id).fetch_all(pool).await?;
    // Retrobios and multi-system DATs name a system per game: never misplaced as a whole.
    let wanted: std::collections::HashSet<String> = rows.into_iter().filter(|(_, name)| name.as_deref().is_some_and(|n| platform_map::platform_from_dat_name(n) != Platform::Unknown)).map(|(h, _)| h).collect();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let is_zip = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("zip"));
    let members: Vec<String> = if is_zip { super::dat_parse::list_dat_members(path)? } else { vec![file_name.clone()] };
    Ok(members.into_iter().filter(|m| wanted.contains(&sha1_hex(&if is_zip { format!("{file_name}::{m}") } else { m.clone() }))).collect())
}

/// Scanned roms the scan could not place (platform `Unknown`) that are now linked to a release
/// with a platform take the release's. A hash match does this for the games it links one by
/// one; the ones linked another way -- a multi-disc game joined from its discs, a playlist,
/// a release that only later got its platform -- were left out. Returns how many.
pub async fn confirm_rom_platforms(pool: &MySqlPool) -> Result<u64, DatabaseError> {
    Ok(sqlx::query(
        "UPDATE local_rom lr JOIN (SELECT lgr.rom_id, MIN(r.app_platform_id) AS platform FROM local_rom x \
           JOIN local_game_roms lgr ON lgr.rom_id = x.id JOIN local_release_games lrg ON lrg.game_id = lgr.game_id \
           JOIN `release` r ON r.id = lrg.release_id AND r.deleted = FALSE AND r.app_platform_id <> 0 \
           WHERE x.app_platform_id = 0 AND x.deleted = FALSE GROUP BY lgr.rom_id) m ON m.rom_id = lr.id \
         SET lr.app_platform_id = m.platform",
    )
    .execute(pool)
    .await?
    .rows_affected())
}

async fn import_chunk(tx: &mut Transaction<'_, MySql>, source_id: u8, source: Source, platform: Platform, chunk: &[ReleaseDraft], dat_key: &str, summary: &mut ImportSummary, listed_pairs: &mut Vec<(u64, u64)>) -> Result<(), DatabaseError> {
    let platform_id = platform.igdb_id();
    // A release is keyed by platform and name, and a collection of hacks holds ROMs named
    // exactly as the game they are of ("Some Game (USA)", filed under Hacks). Whichever is
    // imported first, the game as it was sold stays that: a DAT of originals clears a kind
    // it does not state, and a DAT filled by hand does not give one to a release such a
    // DAT lists. (7 of No-Intro's 3,700 SNES releases were hacks without this.)
    let original = if source.lists_originals() { "VALUES(game_kind) IS NULL" } else { "FALSE" };
    let listed = if source.is_manual() {
        "EXISTS (SELECT 1 FROM release_sources rsx JOIN sources sx ON sx.id = rsx.source_id WHERE rsx.release_id = `release`.id AND sx.name IN ('no-intro', 'redump', 'mame-redump', 'tosec'))"
    } else {
        "FALSE"
    };
    let kind_rules = format!(
        "community_patched = CASE WHEN {original} THEN FALSE WHEN game_kind IS NULL AND {listed} THEN community_patched ELSE (community_patched OR VALUES(community_patched)) END, \
         patch_type = CASE WHEN {original} THEN NULL WHEN game_kind IS NULL AND {listed} THEN patch_type ELSE COALESCE(VALUES(patch_type), patch_type) END, \
         game_kind = CASE WHEN {original} THEN NULL WHEN game_kind IS NULL AND {listed} THEN NULL ELSE COALESCE(VALUES(game_kind), game_kind) END, "
    );

    // --- releases ---
    let mut sql = String::from(
        "INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, languages, revision, year, serial, category, material, disc_count, source_id, is_beta, is_prototype, is_demo, is_unlicensed, is_pirate, community_patched, patch_type, \
         is_special_edition, is_budget, is_aftermarket, is_rerelease, revision_sort, deleted, game_kind, languages_state, base_hint) VALUES ",
    );
    sql.push_str(&placeholders(chunk.len(), 30));
    // A game stays a game: a trainer, or the patch a hack is made with, listed under the hack's
    // own name by another DAT adds its files to the hack and does not turn it into one --
    // whichever of the two was imported first.
    sql.push_str(
        " ON DUPLICATE KEY UPDATE norm_name = VALUES(norm_name), norm_hash = VALUES(norm_hash), region = COALESCE(VALUES(region), region), languages = IF(languages_state <=> 'manual', languages, COALESCE(VALUES(languages), languages)), \
         languages_state = IF(languages_state <=> 'manual', languages_state, COALESCE(VALUES(languages_state), languages_state)), \
         base_hint = COALESCE(VALUES(base_hint), base_hint), \
         revision = COALESCE(VALUES(revision), revision), year = COALESCE(VALUES(year), year), serial = COALESCE(VALUES(serial), serial), category = COALESCE(VALUES(category), category), ",
    );
    sql.push_str(&kind_rules);
    sql.push_str(
        "material = CASE WHEN material IS NULL AND game_kind IS NOT NULL AND VALUES(material) IN ('trainer', 'save', 'mappack', 'tool', 'patch') THEN NULL              WHEN material = 'patch' AND VALUES(material) IS NULL AND VALUES(game_kind) IS NOT NULL THEN NULL ELSE COALESCE(VALUES(material), material) END, \
         disc_count = GREATEST(disc_count, VALUES(disc_count)), is_beta = (is_beta OR VALUES(is_beta)), is_prototype = (is_prototype OR VALUES(is_prototype)), is_demo = (is_demo OR VALUES(is_demo)), \
         is_unlicensed = (is_unlicensed OR VALUES(is_unlicensed)), is_pirate = (is_pirate OR VALUES(is_pirate)), is_special_edition = (is_special_edition OR VALUES(is_special_edition)), is_budget = (is_budget OR VALUES(is_budget)), is_aftermarket = (is_aftermarket OR VALUES(is_aftermarket)), is_rerelease = (is_rerelease OR VALUES(is_rerelease)), \
         revision_sort = GREATEST(revision_sort, VALUES(revision_sort)), deleted = FALSE",
    );
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
    for d in chunk {
        query = query
            .bind(platform_id)
            .bind(&d.name)
            .bind(&d.name_hash)
            .bind(&d.norm_name)
            .bind(&d.norm_hash)
            .bind(&d.region)
            .bind(&d.languages)
            .bind(&d.revision)
            .bind(d.year)
            .bind(&d.serial)
            .bind(&d.category)
            .bind(d.material)
            .bind(d.disc_count)
            .bind(source_id)
            .bind(d.is_beta)
            .bind(d.is_prototype)
            .bind(d.is_demo)
            .bind(d.is_unlicensed)
            .bind(d.is_pirate)
            .bind(d.community_patched)
            .bind(d.patch_type)
            .bind(d.is_special_edition)
            .bind(d.is_budget)
            .bind(d.is_aftermarket)
            .bind(d.is_rerelease)
            .bind(d.revision_sort)
            .bind(false)
            .bind(d.game_kind)
            .bind(d.languages_state)
            .bind(&d.base_hint);
    }
    query.execute(&mut **tx).await?;
    summary.releases_upserted += chunk.len() as u64;

    // One release per platform + name whichever DATs list it (`release_merge`); this
    // source is recorded in release_sources.
    let sql = format!("SELECT id, name_hash FROM `release` WHERE app_platform_id = ? AND name_hash IN ({})", in_list(chunk.len()));
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(platform_id);
    for d in chunk {
        query = query.bind(&d.name_hash);
    }
    let rows = query.fetch_all(&mut **tx).await?;
    let mut release_ids: HashMap<String, u64> = HashMap::with_capacity(rows.len());
    for row in rows {
        release_ids.insert(row.try_get("name_hash")?, row.try_get("id")?);
    }
    let ids: Vec<u64> = release_ids.values().copied().collect();
    for part in ids.chunks(1000) {
        let mut sql = String::from("INSERT IGNORE INTO release_sources (release_id, source_id) VALUES ");
        sql.push_str(&placeholders(part.len(), 2));
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
        for id in part {
            query = query.bind(*id).bind(source_id);
        }
        query.execute(&mut **tx).await?;
    }

    // --- content ---
    // Strongest available hash per file; (release_id, file_name) as the fallback identity.
    struct FileSlot<'a> {
        release_id: u64,
        file: &'a ParsedFile,
        ra_hash: Option<&'a str>,
        key: Option<(&'static str, String)>,
        content_id: Option<u64>,
    }
    let mut slots: Vec<FileSlot> = Vec::new();
    for d in chunk {
        let Some(&release_id) = release_ids.get(&d.name_hash) else { continue };
        for (file, ra) in &d.files {
            let key = file
                .sha256
                .as_deref()
                .map(|v| ("sha256", v.to_ascii_lowercase()))
                .or_else(|| file.sha1.as_deref().map(|v| ("sha1", v.to_ascii_lowercase())))
                .or_else(|| file.md5.as_deref().map(|v| ("md5", v.to_ascii_lowercase())));
            slots.push(FileSlot { release_id, file, ra_hash: ra.as_deref(), key, content_id: None });
        }
    }

    // Existing content by hash, strongest first -- every algorithm the line carries, not only
    // its strongest. A file scanned (or reported by Hasheous) before its DAT arrived has
    // crc32/md5/sha1 and no sha256, so a sha256-only lookup missed it and the DAT line became
    // a second content row for the same bytes: 1,306 local files on the live catalogue, none
    // of which could then match their No-Intro release.
    for algorithm in ["sha256", "sha1", "md5"] {
        let value_of = |s: &FileSlot| -> Option<String> {
            match algorithm {
                "sha256" => s.file.sha256.as_deref(),
                "sha1" => s.file.sha1.as_deref(),
                _ => s.file.md5.as_deref(),
            }
            .map(|v| v.to_ascii_lowercase())
        };
        let wanted: Vec<(usize, String)> = slots.iter().enumerate().filter(|(_, s)| s.content_id.is_none()).filter_map(|(i, s)| value_of(s).map(|v| (i, v))).collect();
        if wanted.is_empty() {
            continue;
        }
        let mut found: HashMap<String, u64> = HashMap::new();
        for part in wanted.chunks(1000) {
            let sql = format!("SELECT content_id, value_hex FROM hashes WHERE algorithm = ? AND content_id IS NOT NULL AND value_hex IN ({})", in_list(part.len()));
            let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(algorithm);
            for (_, v) in part {
                query = query.bind(v.as_str());
            }
            for row in query.fetch_all(&mut **tx).await? {
                found.insert(row.try_get("value_hex")?, row.try_get("content_id")?);
            }
        }
        for (i, v) in &wanted {
            slots[*i].content_id = found.get(v).copied();
        }
    }
    // Existing content by (release, file_name) -- re-imports of crc32-only lines, and any
    // line that gained a hash upstream.
    let mut replaced: Vec<(u64, u64)> = Vec::new();
    {
        let need: Vec<(u64, &str)> = slots.iter().filter(|s| s.content_id.is_none()).map(|s| (s.release_id, s.file.name.as_str())).collect();
        if !need.is_empty() {
            let ids: Vec<u64> = need.iter().map(|(r, _)| *r).collect::<HashSet<_>>().into_iter().collect();
            let mut found: HashMap<(u64, String), u64> = HashMap::new();
            for part in ids.chunks(1000) {
                let sql = format!("SELECT release_id, content_id, file_name FROM release_contents WHERE release_id IN ({})", in_list(part.len()));
                let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
                for id in part {
                    query = query.bind(id);
                }
                for row in query.fetch_all(&mut **tx).await? {
                    found.insert((row.try_get("release_id")?, row.try_get("file_name")?), row.try_get("content_id")?);
                }
            }
            // Only content whose hashes agree with the line: a line whose hash changed
            // upstream (a bad dump replaced by a good one under the same name) is new
            // content, and the old pair is retired below. Reusing the old row kept the old
            // hash for good, the new one dropped by the INSERT IGNORE on `hashes`.
            let candidates: Vec<u64> = found.values().copied().collect::<HashSet<_>>().into_iter().collect();
            let mut known: HashMap<u64, Vec<(String, String)>> = HashMap::new();
            for part in candidates.chunks(1000) {
                let sql = format!("SELECT content_id, algorithm, value_hex FROM hashes WHERE algorithm IN ('crc32', 'md5', 'sha1', 'sha256') AND content_id IN ({})", in_list(part.len()));
                let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
                for id in part {
                    query = query.bind(id);
                }
                for row in query.fetch_all(&mut **tx).await? {
                    known.entry(row.try_get("content_id")?).or_default().push((row.try_get("algorithm")?, row.try_get("value_hex")?));
                }
            }
            for slot in slots.iter_mut() {
                if slot.content_id.is_some() {
                    continue;
                }
                let Some(&candidate) = found.get(&(slot.release_id, slot.file.name.clone())) else { continue };
                let line = [("crc32", &slot.file.crc32), ("md5", &slot.file.md5), ("sha1", &slot.file.sha1), ("sha256", &slot.file.sha256)];
                let conflicts = known.get(&candidate).is_some_and(|have| {
                    have.iter().any(|(algorithm, value)| line.iter().any(|(a, v)| a == algorithm && v.as_ref().is_some_and(|v| !v.eq_ignore_ascii_case(value))))
                });
                if conflicts {
                    replaced.push((slot.release_id, candidate));
                } else {
                    slot.content_id = Some(candidate);
                }
            }
        }
    }
    // Content that existed before this DAT (a scan got there first) and still has no
    // platform takes the DAT's: the scan could not tell what a bare .bin or a CHD's inner
    // image was, the DAT can.
    {
        let known: Vec<u64> = slots.iter().filter_map(|s| s.content_id).collect::<HashSet<_>>().into_iter().collect();
        for part in known.chunks(1000) {
            let sql = format!("UPDATE content SET app_platform_id = ? WHERE app_platform_id = 0 AND id IN ({})", in_list(part.len()));
            let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(platform_id);
            for id in part {
                query = query.bind(id);
            }
            query.execute(&mut **tx).await?;
        }
    }
    // Within this chunk, two lines with the same strongest hash are one content.
    let mut chunk_keyed: HashMap<(&'static str, String), usize> = HashMap::new();
    let mut new_slot_indexes: Vec<usize> = Vec::new();
    for (i, slot) in slots.iter().enumerate() {
        if slot.content_id.is_some() {
            summary.content_reused += 1;
            continue;
        }
        if let Some(key) = &slot.key {
            if chunk_keyed.contains_key(key) {
                continue;
            }
            chunk_keyed.insert(key.clone(), i);
        }
        new_slot_indexes.push(i);
    }
    // How many files each release has, for the `partial` flag of a disk image.
    let mut files_of: HashMap<u64, usize> = HashMap::new();
    for slot in &slots {
        *files_of.entry(slot.release_id).or_default() += 1;
    }
    if !new_slot_indexes.is_empty() {
        for part in new_slot_indexes.chunks(500) {
            let mut sql = String::from("INSERT INTO content (app_platform_id, file_name, size, partial, source_id) VALUES ");
            sql.push_str(&placeholders(part.len(), 5));
            let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
            for &i in part {
                let slot = &slots[i];
                let lower = slot.file.name.to_ascii_lowercase();
                // A track of a multi-file disc: a .bin or .raw beside other files, or an .img
                // that is not a disk image of its own.
                let of_several = files_of.get(&slot.release_id).copied().unwrap_or(1) > 1;
                let partial = of_several && (lower.ends_with(".bin") || lower.ends_with(".raw") || (lower.ends_with(".img") && !slot.file.is_disk));
                query = query.bind(platform_id).bind(&slot.file.name).bind(slot.file.size).bind(partial).bind(source_id);
            }
            let result = query.execute(&mut **tx).await?;
            let first_id = result.last_insert_id();
            for (offset, &i) in part.iter().enumerate() {
                slots[i].content_id = Some(first_id + offset as u64);
            }
            summary.content_new += part.len() as u64;
        }
        // Lines that shared a strongest hash with a new line in this chunk.
        for slot in slots.iter_mut() {
            if slot.content_id.is_none() {
                if let Some(key) = &slot.key {
                    if let Some(&i) = chunk_keyed.get(key) {
                        let _ = i;
                    }
                }
            }
        }
        let resolved: HashMap<(&'static str, String), u64> = chunk_keyed.iter().filter_map(|(k, &i)| slots[i].content_id.map(|id| (k.clone(), id))).collect();
        for slot in slots.iter_mut() {
            if slot.content_id.is_none() {
                if let Some(key) = &slot.key {
                    slot.content_id = resolved.get(key).copied();
                }
            }
        }
    }

    // --- hashes (INSERT IGNORE: enriches existing content with algorithms it lacked) ---
    let mut hash_rows: Vec<(u64, &'static str, String)> = Vec::new();
    let mut seen_hash: HashSet<(u64, &'static str)> = HashSet::new();
    for slot in &slots {
        let Some(content_id) = slot.content_id else { continue };
        for (algorithm, value) in [("crc32", &slot.file.crc32), ("md5", &slot.file.md5), ("sha1", &slot.file.sha1), ("sha256", &slot.file.sha256)] {
            if let Some(v) = value {
                if seen_hash.insert((content_id, algorithm)) {
                    hash_rows.push((content_id, algorithm, v.to_ascii_lowercase()));
                }
            }
        }
        if let Some(ra) = slot.ra_hash {
            if seen_hash.insert((content_id, "ra_hash")) {
                hash_rows.push((content_id, "ra_hash", ra.to_ascii_lowercase()));
                summary.ra_hashes += 1;
            }
        }
    }
    for part in hash_rows.chunks(1000) {
        let mut sql = String::from("INSERT IGNORE INTO hashes (content_id, algorithm, value_hex) VALUES ");
        sql.push_str(&placeholders(part.len(), 3));
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
        for (content_id, algorithm, value) in part {
            query = query.bind(content_id).bind(*algorithm).bind(value);
        }
        let result = query.execute(&mut **tx).await?;
        summary.hashes_inserted += result.rows_affected();
    }

    // --- release_contents ---
    let rc_rows: Vec<(u64, u64, &str, Option<String>)> =
        slots.iter().filter_map(|s| s.content_id.map(|c| (s.release_id, c, s.file.name.as_str(), s.file.variant.as_ref().map(|v| v.chars().take(64).collect())))).collect();
    for part in rc_rows.chunks(1000) {
        // source_id: which DAT's file list (dump) this is; a file two sources share keeps
        // the first. variant: which build of a patched work it is -- learned from a later
        // DAT when the first to list the file did not say.
        let mut sql = String::from("INSERT INTO release_contents (release_id, content_id, file_name, source_id, variant) VALUES ");
        sql.push_str(&placeholders(part.len(), 5));
        sql.push_str(" ON DUPLICATE KEY UPDATE variant = COALESCE(variant, VALUES(variant))");
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
        for (release_id, content_id, file_name, variant) in part {
            query = query.bind(release_id).bind(content_id).bind(*file_name).bind(source_id).bind(variant);
        }
        query.execute(&mut **tx).await?;
        // ...and every source that lists it is kept here, the first one included, so the
        // release page can show them all (`release_content_sources`, migration 0033).
        let mut sql = String::from("INSERT INTO release_content_sources (release_id, content_id, source_id, dat_key) VALUES ");
        sql.push_str(&placeholders(part.len(), 4));
        sql.push_str(" ON DUPLICATE KEY UPDATE dat_key = VALUES(dat_key)");
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
        for (release_id, content_id, _, _) in part {
            query = query.bind(release_id).bind(content_id).bind(source_id).bind(dat_key);
        }
        query.execute(&mut **tx).await?;
    }
    // A superseded file listed again is the release's again.
    for part in rc_rows.chunks(1000) {
        let sql = format!("DELETE FROM release_contents_superseded WHERE (release_id, content_id) IN ({})", placeholders(part.len(), 2));
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
        for (release_id, content_id, _, _) in part {
            query = query.bind(release_id).bind(content_id);
        }
        summary.content_restored += query.execute(&mut **tx).await?.rows_affected();
    }
    listed_pairs.extend(rc_rows.iter().map(|(r, c, _, _)| (*r, *c)));
    // The old file of a line whose hash changed is no longer listed: `supersede_unlisted`
    // takes it off once the whole DAT is in.
    summary.content_replaced += replaced.len() as u64;

    // --- titles (keyed by norm_hash within this platform) ---
    // A release that already has a title keeps it (it may have been merged under another
    // name -- `title_merge`); only new releases are assigned by name.
    let mut titled: HashSet<u64> = HashSet::new();
    let known_ids: Vec<u64> = release_ids.values().copied().collect();
    for part in known_ids.chunks(1000) {
        let sql = format!("SELECT DISTINCT release_id FROM title_releases WHERE release_id IN ({})", in_list(part.len()));
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
        for id in part {
            query = query.bind(*id);
        }
        for row in query.fetch_all(&mut **tx).await? {
            titled.insert(row.try_get("release_id")?);
        }
    }
    // Disc media, documents and source code get none (`platform_map::STANDALONE`): a title
    // is a game, and these are linked to one by hand or not at all.
    let chunk: Vec<&ReleaseDraft> = chunk
        .iter()
        .filter(|d| !d.material.is_some_and(|m| platform_map::STANDALONE.contains(&m)))
        .filter(|d| release_ids.get(&d.name_hash).is_some_and(|id| !titled.contains(id)))
        .collect();
    let mut norm_hashes: Vec<&str> = chunk.iter().map(|d| d.norm_hash.as_str()).collect::<HashSet<_>>().into_iter().collect();
    norm_hashes.sort();
    let mut title_ids: HashMap<String, u64> = HashMap::new();
    for part in norm_hashes.chunks(1000) {
        // A title's own normalized name, or one of a merged title's (title_aliases).
        let sql = format!(
            "SELECT t.id, t.norm_hash FROM title t JOIN title_releases tr ON tr.title_id = t.id JOIN `release` r ON r.id = tr.release_id WHERE r.app_platform_id = ? AND t.norm_hash IN ({0}) \
             UNION SELECT a.title_id AS id, a.norm_hash FROM title_aliases a JOIN title_releases tr ON tr.title_id = a.title_id JOIN `release` r ON r.id = tr.release_id WHERE r.app_platform_id = ? AND a.norm_hash IN ({0}) \
             ORDER BY id",
            in_list(part.len())
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(platform_id);
        for h in part {
            query = query.bind(*h);
        }
        query = query.bind(platform_id);
        for h in part {
            query = query.bind(*h);
        }
        for row in query.fetch_all(&mut **tx).await? {
            let hash: String = row.try_get("norm_hash")?;
            let id: u64 = row.try_get("id")?;
            title_ids.entry(hash).or_insert(id);
        }
    }
    let mut new_titles: Vec<&ReleaseDraft> = Vec::new();
    let mut seen_new: HashSet<&str> = HashSet::new();
    for d in chunk.iter().copied() {
        if !title_ids.contains_key(&d.norm_hash) && seen_new.insert(d.norm_hash.as_str()) {
            new_titles.push(d);
        }
    }
    for part in new_titles.chunks(500) {
        let mut sql = String::from("INSERT INTO title (name, norm_name, norm_hash) VALUES ");
        sql.push_str(&placeholders(part.len(), 3));
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
        for d in part {
            query = query.bind(&d.title_name).bind(&d.norm_name).bind(&d.norm_hash);
        }
        let result = query.execute(&mut **tx).await?;
        let first_id = result.last_insert_id();
        for (offset, d) in part.iter().enumerate() {
            title_ids.insert(d.norm_hash.clone(), first_id + offset as u64);
        }
        summary.titles_new += part.len() as u64;
    }
    let tr_rows: Vec<(u64, u64)> = chunk.iter().filter_map(|d| Some((*title_ids.get(&d.norm_hash)?, *release_ids.get(&d.name_hash)?))).collect();
    for part in tr_rows.chunks(1000) {
        let mut sql = String::from("INSERT IGNORE INTO title_releases (title_id, release_id) VALUES ");
        sql.push_str(&placeholders(part.len(), 2));
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
        for (title_id, release_id) in part {
            query = query.bind(title_id).bind(release_id);
        }
        query.execute(&mut **tx).await?;
    }

    Ok(())
}

/// Convenience: parse and import one DAT file/pack from disk. `member_filter` picks pack
/// members (see [`super::dat_parse::parse_dat_path`]). Returns one summary per parsed dat.
/// Whether every DAT in `path` (the file itself, or each `.dat`/`.xml` member of a zip pack,
/// named as `import_path` records them) already has a `dat_imports` row for `source`. Lets
/// a `dat_sync` that was interrupted between download and import pick the downloaded but
/// never-imported files up again, instead of importing only freshly downloaded ones.
/// Members `wanted` leaves out (other platforms', under `dat_sync_platforms`) don't count.
pub async fn is_imported(pool: &MySqlPool, source: Source, path: &std::path::Path, wanted: &(dyn Fn(&str) -> bool + Sync)) -> Result<bool, DatabaseError> {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let names: Vec<String> = if path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
        let archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        archive
            .file_names()
            .filter(|m| {
                let lower = m.to_ascii_lowercase();
                (lower.ends_with(".dat") || lower.ends_with(".xml")) && wanted(m)
            })
            .map(|m| format!("{file_name}::{m}"))
            .collect()
    } else {
        vec![file_name]
    };
    if names.is_empty() {
        return Ok(true);
    }
    // Read-only: a source that has no row yet has imported nothing.
    let Some(row) = sqlx::query("SELECT id FROM sources WHERE name = ?").bind(source.name()).fetch_optional(pool).await? else { return Ok(false) };
    let source_id: u8 = row.try_get("id")?;
    let sql = format!("SELECT COUNT(*) AS n FROM dat_imports WHERE source_id = ? AND file_hash IN ({})", in_list(names.len()));
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(source_id);
    for name in &names {
        query = query.bind(sha1_hex(name));
    }
    let n: i64 = query.fetch_one(pool).await?.try_get("n")?;
    Ok(n as usize >= names.len())
}

pub async fn import_path(pool: &MySqlPool, source: Source, path: &std::path::Path, member_filter: &(dyn Fn(&str) -> bool + Sync), options: &ImportOptions) -> Result<Vec<(String, ImportSummary)>, DatabaseError> {
    import_path_reporting(pool, source, path, member_filter, options, None).await
}

/// [`import_path`], reporting as it goes: `(progress, label)` gets "`label`: <dat name>
/// (n of N), <done> of <its> entries", on a scale of every entry in `path`.
///
/// For a pack this is the only progress there is. No-Intro and TOSEC each arrive as one zip
/// of hundreds or thousands of DATs, so a job that counts *files* reports 0 of 1 for the
/// whole import -- its bar sat at the half-way mark, where the download ends, for as long as
/// the import took, and read as hung.
pub async fn import_path_reporting(
    pool: &MySqlPool,
    source: Source,
    path: &std::path::Path,
    member_filter: &(dyn Fn(&str) -> bool + Sync),
    options: &ImportOptions,
    progress: Option<(&super::jobs::Progress, &str)>,
) -> Result<Vec<(String, ImportSummary)>, DatabaseError> {
    // Checked here rather than only in the job layer, because this is also the CLI's entry
    // point (`romexplorer_dbtool import`) -- and a client whose catalogue came from its main
    // server must not gain rows the server has never heard of, however they are started.
    if super::settings::instance_mode(pool).await? == super::settings::InstanceMode::Client {
        return Err(DatabaseError::Config(
            "this installation is in client mode: its catalogue comes from its main server, so importing a DAT here would \
             create rows that server does not have. Import it on the main server and sync, or switch this machine to standalone mode."
                .to_string(),
        ));
    }
    let entries = super::dat_parse::parse_dat_path(path, member_filter)?;
    let mut out = Vec::with_capacity(entries.len());
    let total = entries.len() as u64;
    // Entries, not DATs, are the scale: the DATs of a pack run from ten entries to a hundred
    // thousand, and a file that is one DAT has no other scale at all.
    let all_entries: u64 = entries.iter().map(|e| e.dat.games.len() as u64).sum();
    let mut entries_done = 0u64;
    let mut failed: Vec<String> = Vec::new();
    for (n, entry) in entries.into_iter().enumerate() {
        let report = progress.map(|(progress, label)| EntryProgress {
            progress,
            label: if total > 1 { format!("{label}: {} ({} of {total})", entry.name, n + 1) } else { label.to_string() },
            before: entries_done,
            total: all_entries.max(1),
        });
        if let Some(r) = &report {
            r.progress.force(r.before, Some(r.total), &r.label).await;
        }
        let started = std::time::Instant::now();
        // Each DAT is one transaction, so one that lost a lock to another job (two DAT
        // syncs at once) was rolled back whole and is simply run again.
        let mut attempt = 0u32;
        let result = loop {
            match import_dat_reporting(pool, source, &entry.name, &entry.dat, options, report.as_ref()).await {
                Err(e) if e.is_lock_conflict() && attempt < 4 => {
                    attempt += 1;
                    log::info!("dat import: {} hit a lock held by another job ({e}); trying again ({attempt} of 4)", entry.name);
                    tokio::time::sleep(std::time::Duration::from_secs(2u64.pow(attempt))).await;
                }
                other => break other,
            }
        };
        // One DAT that will not go in does not stop the rest of a pack (TOSEC's has 4,700).
        let summary = match result {
            Ok(s) => s,
            Err(e) => {
                log::warn!("dat import: {} failed: {e}", entry.name);
                failed.push(format!("{}: {e}", entry.name));
                entries_done += entry.dat.games.len() as u64;
                continue;
            }
        };
        entries_done += entry.dat.games.len() as u64;
        // Where a slow import's time goes is otherwise invisible: a pack's summaries are
        // logged together when it ends, so nothing says which DAT took the minutes.
        let took = started.elapsed();
        if took.as_secs() >= 5 {
            log::info!("dat import: {} took {:.1}s ({} games, {} files, {} new content)", entry.name, took.as_secs_f64(), summary.games, summary.files, summary.content_new);
        }
        out.push((entry.name, summary));
    }
    if !failed.is_empty() {
        return Err(DatabaseError::Partial(format!("{} of {total} DAT(s) failed to import: {}", failed.len(), failed.join("; "))));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A new release of a DAT is the same DAT; its encrypted and decrypted cuts are not.
    #[test]
    fn a_dats_key_ignores_its_date_and_count_but_not_its_cut() {
        assert_eq!(dat_key("Sony - PlayStation 3 (Decrypted) (298)"), dat_key("Sony - PlayStation 3 (Decrypted) (301)"));
        assert_eq!(dat_key("Nintendo - Game Boy (20240101-123456)"), dat_key("Nintendo - Game Boy"));
        assert_ne!(dat_key("Sony - PlayStation 3 (Decrypted)"), dat_key("Sony - PlayStation 3 (Encrypted)"));
        assert_ne!(dat_key("Nintendo - Game Boy"), dat_key("Nintendo - Game Boy Color"));
    }

    /// 2% of a DAT's files, but never fewer than a handful.
    #[test]
    fn one_import_supersedes_at_most_a_small_share() {
        assert_eq!(supersede_limit(100), SUPERSEDE_FLOOR);
        assert_eq!(supersede_limit(60_000), 1_200);
    }

    #[test]
    fn a_colour_handhelds_own_files_take_it_to_the_colour_model() {
        let game = |files: &[&str]| ParsedGame { name: "Hoge Quest (Japan)".into(), files: files.iter().map(|f| crate::database::dat_parse::ParsedFile { name: f.to_string(), ..Default::default() }).collect(), ..Default::default() };
        assert_eq!(color_system(Platform::WonderSwan, &game(&["Hoge Quest (Japan).wsc"])), Platform::WonderSwanColor);
        assert_eq!(color_system(Platform::WonderSwan, &game(&["Hoge Quest (Japan).ws"])), Platform::WonderSwan);
        assert_eq!(color_system(Platform::NeoGeoPocket, &game(&["Hoge Quest (Japan).NGC"])), Platform::NeoGeoPocketColor);
        assert_eq!(color_system(Platform::NeoGeoPocket, &game(&["Hoge Quest (Japan).ngp"])), Platform::NeoGeoPocket);
        // Only when every file is the colour model's kind, and only from the original model.
        assert_eq!(color_system(Platform::WonderSwan, &game(&["a.wsc", "a.ws"])), Platform::WonderSwan);
        assert_eq!(color_system(Platform::GameBoy, &game(&["a.wsc"])), Platform::GameBoy);
    }

    #[test]
    fn structured_region_and_language_attributes_normalize_to_codes() {
        // RA writes a language into the region attribute.
        assert_eq!(normalize_structured_tags(Some("English"), None), (None, Some("en".into())));
        // No-Intro/Redump full names become the same codes name parsing produces.
        assert_eq!(normalize_structured_tags(Some("USA"), Some("En,Fr")), (Some("US".into()), Some("en,fr".into())));
        assert_eq!(normalize_structured_tags(Some("Japan, Asia"), Some("Japanese")), (Some("JP,ASIA".into()), Some("ja".into())));
        // Already-coded values pass through; an unknown word is kept, not collapsed to OTHER.
        assert_eq!(normalize_structured_tags(Some("JP,ASIA"), Some("ja")), (Some("JP,ASIA".into()), Some("ja".into())));
        assert_eq!(normalize_structured_tags(Some("Portugal"), None), (Some("Portugal".into()), None));
        // A language word mixed into the region list moves over, without duplicates.
        assert_eq!(normalize_structured_tags(Some("Europe,English"), Some("en")), (Some("EU".into()), Some("en".into())));
        assert_eq!(normalize_structured_tags(None, None), (None, None));
    }

    #[test]
    fn serials_are_cut_to_the_column_by_whole_serial() {
        assert_eq!(fit_serial("  "), None);
        assert_eq!(fit_serial("SLPS-00001").as_deref(), Some("SLPS-00001"));
        // A list the column holds is kept whole.
        let nine = (1..=9).map(|n| format!("SLPS-0000{n}")).collect::<Vec<_>>().join(", ");
        assert_eq!(fit_serial(&nine).as_deref(), Some(nine.as_str()));
        // Past the width, whole serials only: 12 characters each with the separator.
        let many = (0..200).map(|n| format!("SLPS-{n:05}")).collect::<Vec<_>>().join(", ");
        let kept = fit_serial(&many).unwrap();
        assert!(kept.chars().count() <= SERIAL_MAX);
        assert!(kept.starts_with("SLPS-00000, SLPS-00001") && kept.ends_with("SLPS-00084"), "{kept}");
        // One serial longer than the column on its own, and never split inside a character.
        let long = "あ".repeat(SERIAL_MAX + 40);
        assert_eq!(fit_serial(&long).unwrap().chars().count(), SERIAL_MAX);
    }

    #[test]
    fn disc_tags_roll_up() {
        assert_eq!(release_name_of("Example (USA) (Disc 1)"), "Example (USA)");
        assert_eq!(release_name_of("Example (USA) (Disc 2 of 3) (Rev A)"), "Example (USA) (Rev A)");
        assert_eq!(release_name_of("Example (Europe)"), "Example (Europe)");
    }

    #[test]
    fn placeholder_shapes() {
        assert_eq!(placeholders(2, 3), "(?,?,?),(?,?,?)");
        assert_eq!(in_list(3), "?,?,?");
    }
}
