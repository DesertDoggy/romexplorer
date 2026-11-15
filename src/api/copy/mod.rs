//! Copying ROMs out of a collection to a folder, converting or re-archiving on the way.
//!
//! The model is `docs/plan/api.md`'s: the machine that holds the collection has its root
//! mounted here (SMB, NFS, whatever the user set up), and copying is plain OS file copy of
//! the resolved path. Bytes never travel through REST. What makes that work is
//! [`mount`], which turns a `remote_files.path` -- absolute as the *source* machine writes
//! it -- into a path on this one.
//!
//! One job type covers four directions (`CopyMode`); two are implemented. The direction
//! picks only whether content this machine already holds is skipped, whether the result is
//! registered, and which holder wins a tie. Everything else -- selection, layout, dedup,
//! conversion, resume -- is shared.
//!
//! Three stages, in three files:
//! - [`plan`] resolves a selection down to files and decides every destination path. Pure
//!   of side effects, so `POST /api/v1/copy/preview` is the same code the run uses.
//! - [`run`] moves the bytes.
//! - [`mount`] is the path arithmetic underneath both.
//!
//! **Naming divergence from `docs/plan/api.md`:** that file calls this `CopyToLocal`. The
//! job is `copy_files` here because the same machinery copies local files onto a USB stick
//! and (later) up to a server; direction is a payload field, not the job's identity. The
//! plan also has the job write `local_files` rows itself; this writes none and enqueues a
//! `file_scan` instead -- see [`run`].

pub mod content_check;
pub mod mount;
pub mod plan;
pub mod run;
pub mod upload;

use std::path::Path;

pub use romexplorer_types::copy::*;

use crate::common::models::Platform;
use crate::common::platform_names::{combine_group_of, combined_platform, COMBINE_GROUPS};
use crate::database::settings as db_settings;
use crate::common::models::HashAlgorithms;
use crate::rom::models::{ArchiveArgs, ChdArgs, ChdCodecs, ChdPresets, RvzArgs, RvzBlockSize, RvzCodecs, SevenZipArgs, SevenZipCodecs, ZipArgs, ZipCodecs};
use crate::rom::platforms::helpers::{is_chd_platform, is_disc_console, is_rvz_platform};

use super::{App, Error, Result};

// --- settings ---

pub const KEY_FOLDER_SCHEME: &str = "copy_folder_scheme";
pub const KEY_PLATFORM_COMBINE: &str = "copy_platform_combine";
pub const KEY_OUTPUT_DEFAULTS: &str = "copy_output_defaults";
pub const KEY_DECIDE_ON_COPY: &str = "copy_decide_on_copy";
pub const KEY_RECENT_DESTINATIONS: &str = "copy_recent_destinations";

pub use romexplorer_types::copy::{is_quick_hash, QUICK_HASHES};

/// How many destinations the copy dialog remembers.
pub const RECENT_DESTINATIONS_MAX: usize = 8;

pub async fn folder_scheme(app: &App) -> Result<FolderScheme> {
    let raw: Option<String> = db_settings::get(&app.pool, KEY_FOLDER_SCHEME).await?;
    Ok(raw.as_deref().and_then(FolderScheme::from_name).unwrap_or_default())
}

/// Which combine groups are on. A group the setting doesn't mention keeps its compiled-in
/// default, so a fresh install needs no row at all.
pub async fn combine_map(app: &App) -> Result<std::collections::HashMap<String, bool>> {
    let stored: Option<std::collections::HashMap<String, bool>> = db_settings::get(&app.pool, KEY_PLATFORM_COMBINE).await?;
    let stored = stored.unwrap_or_default();
    Ok(COMBINE_GROUPS.iter().map(|g| (g.key.to_string(), stored.get(g.key).copied().unwrap_or(g.default_on))).collect())
}

/// [`combined_platform`] bound to a loaded map.
pub fn apply_combine(platform: Platform, combine: &std::collections::HashMap<String, bool>) -> Platform {
    combined_platform(platform, &|key| combine.get(key).copied().unwrap_or(false))
}

pub async fn output_defaults(app: &App) -> Result<OutputSettings> {
    Ok(db_settings::get::<OutputSettings>(&app.pool, KEY_OUTPUT_DEFAULTS).await?.unwrap_or_default())
}

pub async fn decide_on_copy(app: &App) -> Result<bool> {
    Ok(db_settings::get::<bool>(&app.pool, KEY_DECIDE_ON_COPY).await?.unwrap_or(false))
}

pub async fn recent_destinations(app: &App) -> Result<Vec<String>> {
    Ok(db_settings::get::<Vec<String>>(&app.pool, KEY_RECENT_DESTINATIONS).await?.unwrap_or_default())
}

/// Most recent first, deduplicated, capped. Best-effort: a copy is not failed because the
/// dialog's history could not be written.
pub async fn remember_destination(app: &App, destination: &str) {
    let mut list = recent_destinations(app).await.unwrap_or_default();
    list.retain(|d| d != destination);
    list.insert(0, destination.to_string());
    list.truncate(RECENT_DESTINATIONS_MAX);
    if let Err(e) = db_settings::set(&app.pool, KEY_RECENT_DESTINATIONS, &list).await {
        log::warn!("copy: couldn't record the destination: {e}");
    }
}

pub async fn defaults(app: &App) -> Result<CopyDefaults> {
    Ok(CopyDefaults {
        scheme: folder_scheme(app).await?,
        output: output_defaults(app).await?,
        decide_on_copy: decide_on_copy(app).await?,
        populate_hash: populate_hash_name(app).await?,
        verify_default: verify_default(app).await?,
        header_preference: header_preference(app).await?,
        wiiu_preference: wiiu_preference(app).await?,
        format_order: format_order(app).await?,
        prefer_decrypted: prefer_decrypted(app).await?,
        lossy_warning: db_settings::get::<bool>(&app.pool, db_settings::KEY_COPY_LOSSY_WARNING).await?.unwrap_or(true),
        recent_destinations: recent_destinations(app).await?,
        unavailable_formats: unavailable_formats(),
        modes: available_modes(app).await?,
    })
}

/// The modes this machine can actually run, in the order the dialog offers them.
///
/// The dialog could offer all five and let `validate_mode` refuse three of them, but a radio
/// button that always errors is worse than no radio button: it reads as a feature that is
/// broken rather than one that does not apply here.
pub async fn available_modes(app: &App) -> Result<Vec<CopyMode>> {
    let client = crate::database::settings::instance_mode(&app.pool).await? == crate::database::settings::InstanceMode::Client;
    let mounted = !mount::load(app, None).await?.is_empty();
    Ok(CopyMode::ALL
        .into_iter()
        .filter(|m| match m {
            // The server's own collection is only this machine's to rearrange when the
            // catalogue is this machine's too.
            CopyMode::InServer => !client,
            // Nowhere to write without a share to write through.
            CopyMode::Upload => mounted,
            _ => true,
        })
        .collect())
}

pub async fn set_defaults(app: &App, update: &DefaultsUpdate) -> Result<CopyDefaults> {
    if let Some(output) = &update.output {
        validate_output(output)?;
        db_settings::set(&app.pool, KEY_OUTPUT_DEFAULTS, output).await?;
    }
    if let Some(on) = update.decide_on_copy {
        db_settings::set(&app.pool, KEY_DECIDE_ON_COPY, &on).await?;
    }
    if let Some(name) = &update.populate_hash {
        if !is_quick_hash(name) {
            return Err(Error::invalid(format!("copy_populate_hash must be one of {}", QUICK_HASHES.join(" | "))));
        }
        db_settings::set(&app.pool, db_settings::KEY_COPY_POPULATE_HASH, name).await?;
    }
    if let Some(on) = update.verify_default {
        db_settings::set(&app.pool, db_settings::KEY_COPY_VERIFY_DEFAULT, &on).await?;
    }
    if let Some(p) = update.header_preference {
        db_settings::set(&app.pool, db_settings::KEY_COPY_HEADER_PREFERENCE, &p.name()).await?;
    }
    if let Some(p) = update.wiiu_preference {
        db_settings::set(&app.pool, db_settings::KEY_COPY_WIIU_PREFERENCE, &p.name()).await?;
    }
    if let Some(order) = &update.format_order {
        let order = clean_format_order(order)?;
        db_settings::set(&app.pool, db_settings::KEY_COPY_FORMAT_ORDER, &order).await?;
    }
    if let Some(on) = update.prefer_decrypted {
        db_settings::set(&app.pool, db_settings::KEY_COPY_PREFER_DECRYPTED, &on).await?;
    }
    if let Some(on) = update.lossy_warning {
        db_settings::set(&app.pool, db_settings::KEY_COPY_LOSSY_WARNING, &on).await?;
    }
    defaults(app).await
}

/// The format families as given, tidied: extensions lower-case without a dot, empty ones
/// dropped, a family of fewer than two left out.
fn clean_format_order(order: &[Vec<String>]) -> Result<Vec<Vec<String>>> {
    if order.len() > 32 {
        return Err(Error::invalid("at most 32 format families"));
    }
    let mut out = Vec::new();
    for family in order {
        let mut exts: Vec<String> = Vec::new();
        for e in family {
            let e = e.trim().trim_start_matches('.').to_ascii_lowercase();
            if e.is_empty() {
                continue;
            }
            if e.len() > 8 || !e.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err(Error::invalid(format!("{e:?} is not a file extension")));
            }
            if !exts.contains(&e) {
                exts.push(e);
            }
        }
        if exts.len() >= 2 {
            out.push(exts);
        }
    }
    Ok(out)
}

pub async fn format_order(app: &App) -> Result<Vec<Vec<String>>> {
    Ok(db_settings::get::<Vec<Vec<String>>>(&app.pool, db_settings::KEY_COPY_FORMAT_ORDER).await?.unwrap_or_else(romexplorer_types::copy::default_format_order))
}

pub async fn prefer_decrypted(app: &App) -> Result<bool> {
    Ok(db_settings::get::<bool>(&app.pool, db_settings::KEY_COPY_PREFER_DECRYPTED).await?.unwrap_or(true))
}

/// Whether a path names an encrypted dump ("Encrypted" in a folder or file name, and not
/// "Decrypted").
pub fn is_encrypted_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.contains("encrypted") && !p.contains("decrypted")
}

/// One file of a copy, as [`chosen_files`] weighs it.
pub struct Candidate<'a> {
    /// Its own file name, as it lands at the destination.
    pub name: &'a str,
    /// Where it sits: its rom's own name and its path inside that rom. Two files are two dumps
    /// of one thing only when this is the same -- two discs whose folders hold the same file
    /// names, or two `ICON0.PNG` in one bundle's subfolders, are different files.
    pub place: String,
    /// A headerless dump (`.unh`, or the line a DAT names `.unh`).
    pub headerless: bool,
    /// Its directory's priority key (lower first).
    pub directory: Vec<i64>,
}

/// Which files of one copy a copy takes: one per name -- of two dumps under one name, the
/// header preference, then directory priority -- and of one game in several formats under
/// one name and different extensions, the first in its format family (`.nes` / `.unh` by the
/// header preference). Files of no common family (a disc's cue and bin) all go. A Wii U game
/// held as a `.wua` and as a disc image keeps the one `wiiu` prefers; when that is the `.wua`,
/// the disc's `.key` stays behind with it.
pub fn chosen_files(files: &[Candidate<'_>], header: romexplorer_types::copy::HeaderPreference, wiiu: romexplorer_types::copy::WiiuPreference, order: &[Vec<String>]) -> Vec<bool> {
    let mut keep = vec![true; files.len()];
    let split = |name: &str| -> (String, String) {
        match name.rsplit_once('.') {
            Some((stem, ext)) => (stem.to_lowercase(), ext.to_ascii_lowercase()),
            None => (name.to_lowercase(), String::new()),
        }
    };
    // One per name.
    for i in 0..files.len() {
        for j in 0..i {
            if keep[j] && keep[i] && files[i].name == files[j].name && files[i].place == files[j].place {
                let rank = |k: usize| (header.rank(files[k].headerless), files[k].directory.clone());
                if rank(i) < rank(j) {
                    keep[j] = false;
                } else {
                    keep[i] = false;
                }
            }
        }
    }
    // One format per game.
    let nes: Vec<String> = vec!["nes".into(), "unh".into()];
    let mut stems: std::collections::BTreeMap<String, Vec<usize>> = std::collections::BTreeMap::new();
    for (i, f) in files.iter().enumerate() {
        if keep[i] {
            stems.entry(split(f.name).0).or_default().push(i);
        }
    }
    // Wii U: a `.wua` and a disc image of one game, plus the disc's `.key` beside it.
    for members in stems.values_mut().filter(|m| m.len() > 1) {
        let discs: Vec<usize> = members.iter().copied().filter(|&i| wiiu.rank(&split(files[i].name).1).is_some()).collect();
        let has_wua = discs.iter().any(|&i| split(files[i].name).1 == "wua");
        if discs.len() < 2 || !has_wua {
            continue;
        }
        let best = discs.iter().copied().min_by_key(|&i| (wiiu.rank(&split(files[i].name).1), files[i].directory.clone(), i)).unwrap_or(discs[0]);
        let wua_won = split(files[best].name).1 == "wua";
        for &i in members.iter() {
            let ext = split(files[i].name).1;
            if (discs.contains(&i) && i != best) || (wua_won && ext == "key") {
                keep[i] = false;
            }
        }
        members.retain(|&i| keep[i]);
    }
    for members in stems.values().filter(|m| m.len() > 1) {
        let exts: Vec<String> = members.iter().map(|&i| split(files[i].name).1).collect();
        let Some(family) = std::iter::once(&nes).chain(order.iter()).find(|fam| exts.iter().all(|e| fam.contains(e))) else { continue };
        let is_nes = family == &nes;
        let rank = |i: usize| {
            let ext = split(files[i].name).1;
            let first = if is_nes { header.rank(files[i].headerless || ext == "unh") as usize } else { family.iter().position(|e| *e == ext).unwrap_or(usize::MAX) };
            (first, files[i].directory.clone(), i)
        };
        let best = members.iter().copied().min_by_key(|&i| rank(i)).unwrap_or(members[0]);
        for &i in members {
            if i != best {
                keep[i] = false;
            }
        }
    }
    keep
}

pub async fn wiiu_preference(app: &App) -> Result<romexplorer_types::copy::WiiuPreference> {
    Ok(db_settings::get::<String>(&app.pool, db_settings::KEY_COPY_WIIU_PREFERENCE).await?.and_then(|n| romexplorer_types::copy::WiiuPreference::from_name(&n)).unwrap_or_default())
}

pub async fn header_preference(app: &App) -> Result<romexplorer_types::copy::HeaderPreference> {
    Ok(db_settings::get::<String>(&app.pool, db_settings::KEY_COPY_HEADER_PREFERENCE).await?.and_then(|n| romexplorer_types::copy::HeaderPreference::from_name(&n)).unwrap_or_default())
}

/// The contents among `ids` a DAT lists as a headerless dump: No-Intro names those `.unh`.
pub async fn headerless_contents(app: &App, ids: &[u64]) -> Result<std::collections::HashSet<u64>> {
    let mut out = std::collections::HashSet::new();
    for chunk in ids.chunks(1000) {
        let list = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        let found: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT DISTINCT content_id FROM release_contents WHERE content_id IN ({list}) AND file_name LIKE '%.unh'"))).fetch_all(&app.pool).await?;
        out.extend(found);
    }
    Ok(out)
}

pub async fn populate_hash_name(app: &App) -> Result<String> {
    Ok(db_settings::get::<String>(&app.pool, db_settings::KEY_COPY_POPULATE_HASH).await?.filter(|n| is_quick_hash(n)).unwrap_or_else(default_quick_hash))
}

pub async fn verify_default(app: &App) -> Result<bool> {
    Ok(db_settings::get::<bool>(&app.pool, db_settings::KEY_COPY_VERIFY_DEFAULT).await?.unwrap_or(false))
}

/// xxh3_128: the fastest of the five by a wide margin, and 128 bits is far more than enough
/// for the failures a read-back is actually for -- a truncated write, a full card, a bad
/// cable. Nothing here is a security claim.
pub fn default_quick_hash() -> String {
    "xxh3_128".to_string()
}

/// The `hashes.algorithm` value for an algorithm, and back.
pub fn algorithm_name(algorithm: HashAlgorithms) -> &'static str {
    crate::database::lookup::algorithm_column(algorithm)
}

pub fn algorithm_from_name(name: &str) -> Option<HashAlgorithms> {
    [HashAlgorithms::Crc32, HashAlgorithms::Xxhash3_128, HashAlgorithms::Md5, HashAlgorithms::Sha1, HashAlgorithms::Sha256].into_iter().find(|a| algorithm_name(*a) == name)
}

/// Which hash to compare a file against: `prefer` when the catalogue records it for that
/// content, else the cheapest one this CPU can compute that it does record.
///
/// The order is [`crate::rom::hash::fallback_order`]'s, which already answers this question
/// for `FileData::integrity` -- crc32, then xxh3_128, then (with hardware SHA) sha1, sha256,
/// md5, or (without it) md5, sha1, sha256, since MD5 has no hardware path and beats software
/// SHA. Reusing it means there is one answer to "which hash is cheapest here", not two.
pub async fn comparison_hash(app: &App, content_id: u64, prefer: Option<HashAlgorithms>) -> Result<Option<(HashAlgorithms, String)>> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT algorithm, value_hex FROM hashes WHERE content_id = ?").bind(content_id).fetch_all(&app.pool).await?;
    Ok(prefer
        .iter()
        .chain(crate::rom::hash::fallback_order().iter())
        .find_map(|algo| rows.iter().find(|(name, _)| name == algorithm_name(*algo)).map(|(_, value)| (*algo, value.clone()))))
}

/// What reading a file back concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Matched {
        algorithm: HashAlgorithms,
        /// The populate hash was missing and has now been recorded.
        recorded: bool,
    },
    Mismatched {
        algorithm: HashAlgorithms,
        expected: String,
        got: String,
    },
    /// The catalogue records no hash at all for this content, so nothing can be concluded --
    /// and, importantly, **nothing is written**. Recording a hash computed from an unconfirmed
    /// copy would make that copy the catalogue's idea of what the content *is*, which would
    /// then declare the original file corrupt. Rare in practice: a `content` row is only ever
    /// created from a hash report.
    Unverifiable,
}

/// Reads `path` once and answers both questions at once: does it still match what the
/// catalogue records, and does the catalogue now also have the hash we want everything to
/// eventually have?
///
/// One read for both, because `HashFlags` is a set and the pipeline computes every requested
/// algorithm off the same pass -- so populating costs nothing beyond the comparison that was
/// happening anyway.
///
/// The populate hash is recorded **only after a match**. That ordering is the whole point:
/// until the bytes are confirmed against something the catalogue already knew, a hash taken
/// from them is a claim about this file, not about the content, and `hashes` has nowhere to
/// put a claim that narrow.
///
/// It compares against `populate` itself -- the hash the user chose (xxh3_128 by default:
/// far stronger than crc32 at nearly its speed) -- whenever the catalogue already records
/// it, and falls back to the cheapest recorded hash only when it does not.
pub async fn check_file(app: &App, path: &std::path::Path, content_id: u64, populate: HashAlgorithms) -> Result<(CheckOutcome, u64)> {
    use crate::rom::hash::HashFlags;

    let Some((algorithm, expected)) = comparison_hash(app, content_id, Some(populate)).await? else {
        return Ok((CheckOutcome::Unverifiable, 0));
    };
    let already_has_populate = recorded_hash(app, content_id, algorithm_name(populate)).await?.is_some();
    let mut flags = HashFlags::algorithm_to_flag(algorithm);
    if !already_has_populate {
        flags |= HashFlags::algorithm_to_flag(populate);
    }
    let owned = path.to_path_buf();
    let report = tokio::task::spawn_blocking(move || crate::rom::hash::hash(&owned, &crate::rom::hash::HashOptions { flags, ..Default::default() }, |_| {}))
        .await
        .map_err(|e| Error::internal(e.to_string()))?
        .map_err(|e| Error::internal(format!("{}: {e}", path.display())))?;
    let read = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    let got = report.get(algorithm).unwrap_or_default().to_string();
    if !got.eq_ignore_ascii_case(&expected) {
        return Ok((CheckOutcome::Mismatched { algorithm, expected, got }, read));
    }
    let mut recorded = false;
    if !already_has_populate {
        if let Some(value) = report.get(populate) {
            record_hash(app, content_id, algorithm_name(populate), value).await?;
            recorded = true;
        }
    }
    Ok((CheckOutcome::Matched { algorithm, recorded }, read))
}

/// What the catalogue records for `content_id` under `algorithm`, if anything.
pub async fn recorded_hash(app: &App, content_id: u64, algorithm: &str) -> Result<Option<String>> {
    Ok(sqlx::query_scalar("SELECT value_hex FROM hashes WHERE content_id = ? AND algorithm = ?").bind(content_id).bind(algorithm).fetch_optional(&app.pool).await?)
}

/// Records a hash the catalogue did not have, for content whose bytes have just been
/// confirmed against a hash it did have. `INSERT IGNORE`, so a concurrent writer wins
/// harmlessly -- they computed it from the same confirmed bytes.
pub async fn record_hash(app: &App, content_id: u64, algorithm: &str, value: &str) -> Result<()> {
    sqlx::query("INSERT IGNORE INTO hashes (content_id, algorithm, value_hex) VALUES (?, ?, ?)").bind(content_id).bind(algorithm).bind(value.to_ascii_lowercase()).execute(&app.pool).await?;
    Ok(())
}

// --- filesystem-safe names ---

/// Windows reserves these whatever the extension, and a file named for one cannot be
/// created at all -- including on a FAT/exFAT card formatted by Windows.
const DOS_RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// One path component the destination filesystem will accept.
///
/// Stricter than the source needs to be on purpose: a copy very often lands on exFAT or
/// FAT32 on an SD card, whose rules are tighter than the NTFS or ext4 it came from, and a
/// name that fails at write time fails after the bytes have been read.
pub fn sanitize_segment(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_underscore = false;
    for c in name.chars() {
        let bad = matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || (c as u32) < 0x20;
        if bad {
            if !last_underscore {
                out.push('_');
                last_underscore = true;
            }
        } else {
            out.push(c);
            last_underscore = false;
        }
    }
    // Windows silently drops trailing spaces and dots, which turns two different names into
    // one file.
    let trimmed = out.trim_end_matches([' ', '.']).trim_start();
    let mut out = if trimmed.is_empty() { "_".to_string() } else { trimmed.to_string() };
    if out == "." || out == ".." {
        out = "_".to_string();
    }
    let stem = out.split('.').next().unwrap_or(&out).to_ascii_uppercase();
    if DOS_RESERVED.contains(&stem.as_str()) {
        out = format!("{out}_");
    }
    truncate_keeping_extension(&out, 255)
}

/// `max` bytes at most, cutting the stem rather than the extension -- an `.chd` that became
/// `.ch` is a file nothing will open.
fn truncate_keeping_extension(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && name.len() - i <= 16 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let room = max.saturating_sub(ext.len());
    let mut cut = room.min(stem.len());
    while cut > 0 && !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{ext}", &stem[..cut])
}

/// ` (2)`, ` (3)` ... inserted before the extension.
pub fn with_dedup_suffix(name: &str, n: u32) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    format!("{stem} ({n}){ext}")
}

// --- output formats ---

/// The formats `platform` can actually be written as.
///
/// Built only from the predicates `rom::platforms::helpers` already exposes, so this stays
/// in step with what the archive code will actually accept rather than duplicating its
/// judgement. `detect::extensions_for_platform` is deliberately not used: it is an *input*
/// table (what a file of this platform may be named), not a statement about containers.
pub fn output_formats_for(platform: Platform) -> Vec<OutputFormat> {
    let mut v = vec![OutputFormat::Same, OutputFormat::Raw, OutputFormat::Zip, OutputFormat::SevenZip];
    if is_chd_platform(platform) {
        v.push(OutputFormat::Chd);
    }
    if is_rvz_platform(platform) {
        v.extend([OutputFormat::Rvz, OutputFormat::Gcz, OutputFormat::Wia]);
    }
    match platform {
        Platform::WiiU => v.push(OutputFormat::Wua),
        Platform::Xbox360 => v.push(OutputFormat::Zar),
        Platform::Xbox => v.push(OutputFormat::Xiso),
        Platform::PlayStation2 | Platform::PlayStationPortable => v.push(OutputFormat::Cso),
        Platform::Nintendo3ds | Platform::NewNintendo3ds => v.extend([OutputFormat::Cci, OutputFormat::ThreeDs, OutputFormat::Zcci, OutputFormat::Cxi, OutputFormat::Zcxi]),
        _ => {}
    }
    v
}

/// The `rom::n3ds` target a 3DS output format stands for.
pub fn n3ds_target(format: OutputFormat) -> Option<crate::rom::n3ds::Target> {
    use crate::rom::n3ds::Target;
    Some(match format {
        OutputFormat::Cci => Target::Cci,
        OutputFormat::ThreeDs => Target::ThreeDs,
        OutputFormat::Zcci => Target::Zcci,
        OutputFormat::Cxi => Target::Cxi,
        OutputFormat::Zcxi => Target::Zcxi,
        _ => return None,
    })
}

/// True for the disc consoles neither CHD nor RVZ covers (Wii U, PS3 and later, the Xbox
/// family): zipping a multi-gigabyte disc image is legal and almost never wanted, so the
/// config tab says so rather than hiding the option. (A Wii U disc has .wua instead, which
/// is not compressing the image but replacing it.)
pub fn compressing_is_pointless(platform: Platform) -> bool {
    is_disc_console(platform) && !is_chd_platform(platform) && !is_rvz_platform(platform)
}

/// Formats this machine has no native library to produce. A copy asking for one of them
/// degrades to a plain byte copy and says so in its summary; the preview greys them out.
pub fn unavailable_formats() -> Vec<OutputFormat> {
    let caps = crate::rom::native_lib::detect_capabilities(crate::rom::native_lib::LibVariant::Streaming);
    let mut out = Vec::new();
    if caps.sevenzip.path.is_none() {
        out.extend([OutputFormat::Zip, OutputFormat::SevenZip, OutputFormat::Raw]);
    }
    if caps.chdman.path.is_none() {
        out.push(OutputFormat::Chd);
    }
    if caps.dolphin.path.is_none() {
        out.extend([OutputFormat::Rvz, OutputFormat::Gcz, OutputFormat::Wia]);
    }
    if !crate::rom::cemu_wrapper::available() {
        out.extend([OutputFormat::Wua, OutputFormat::Zar]);
    }
    if !crate::rom::xbox::xiso_available() {
        out.push(OutputFormat::Xiso);
    }
    if !crate::rom::zlib_ng::available() {
        out.push(OutputFormat::Cso);
    }
    out
}

/// The archive format tag `local_files.archive_format` stores, as an [`OutputFormat`], so
/// "is it already what we want?" is one comparison.
pub fn format_of_tag(tag: Option<&str>) -> OutputFormat {
    match tag.unwrap_or("").to_ascii_lowercase().as_str() {
        "zip" => OutputFormat::Zip,
        "seven_zip" | "7z" => OutputFormat::SevenZip,
        "chd" => OutputFormat::Chd,
        "rvz" => OutputFormat::Rvz,
        "gcz" => OutputFormat::Gcz,
        "wia" => OutputFormat::Wia,
        "wua" => OutputFormat::Wua,
        "zar" => OutputFormat::Zar,
        "xiso" => OutputFormat::Xiso,
        "cso" => OutputFormat::Cso,
        _ => OutputFormat::Same,
    }
}

fn zip_codec(name: &str) -> Result<ZipCodecs> {
    serde_json::from_value(serde_json::Value::String(name.to_ascii_lowercase())).map_err(|_| Error::invalid(format!("unknown zip codec {name:?}")))
}

fn seven_zip_codec(name: &str) -> Result<SevenZipCodecs> {
    serde_json::from_value(serde_json::Value::String(name.to_ascii_lowercase())).map_err(|_| Error::invalid(format!("unknown 7z codec {name:?}")))
}

fn rvz_codec(name: &str) -> Result<RvzCodecs> {
    serde_json::from_value(serde_json::Value::String(name.to_ascii_lowercase())).map_err(|_| Error::invalid(format!("unknown rvz codec {name:?}")))
}

fn rvz_block(name: &str) -> Result<RvzBlockSize> {
    let key = format!("block_{}", name.to_ascii_lowercase().trim_start_matches("block_"));
    serde_json::from_value(serde_json::Value::String(key)).map_err(|_| Error::invalid(format!("unknown rvz block size {name:?} (32k, 64k, 128k, 256k, 512k, 1m, 2m)")))
}

/// The Dolphin arguments for an RVZ, GCZ or WIA output, each from its own settings. GCZ has no
/// codec (Dolphin ignores it); WIA's block is always 2 MiB.
fn disc_args(settings: &OutputSettings, format: OutputFormat) -> Result<RvzArgs> {
    Ok(match format {
        OutputFormat::Gcz => RvzArgs { block_size: rvz_block(&settings.gcz.block_size)?, ..RvzArgs::default() },
        OutputFormat::Wia => {
            let codec = settings.wia.codec.to_ascii_lowercase();
            if !romexplorer_types::copy::WIA_CODECS.contains(&codec.as_str()) {
                return Err(Error::invalid(format!("WIA can't be written with {}; one of {}", settings.wia.codec, romexplorer_types::copy::WIA_CODECS.join(", "))));
            }
            RvzArgs { algo: rvz_codec(&codec)?, block_size: RvzBlockSize::Block_2m, compression_level: settings.wia.level, remove_junk_data: false }
        }
        _ => RvzArgs { algo: rvz_codec(&settings.rvz.codec)?, block_size: rvz_block(&settings.rvz.block_size)?, compression_level: settings.rvz.level, remove_junk_data: settings.rvz.remove_junk_data },
    })
}

fn chd_codec(name: &str) -> Result<ChdCodecs> {
    serde_json::from_value(serde_json::Value::String(name.to_ascii_lowercase())).map_err(|_| Error::invalid(format!("unknown chd codec {name:?}")))
}

/// An empty list is "let chdman choose", not "no compression".
fn chd_codecs(names: &[String]) -> Result<Option<Vec<ChdCodecs>>> {
    if names.is_empty() {
        return Ok(None);
    }
    names.iter().map(|c| chd_codec(c)).collect::<Result<Vec<_>>>().map(Some)
}

fn chd_preset(preset: ChdPreset) -> ChdPresets {
    match preset {
        ChdPreset::ChdmanDefault => ChdPresets::ChdmanDefault,
        ChdPreset::Pc => ChdPresets::AppRecommendPc,
        ChdPreset::Mobile => ChdPresets::AppRecommendAndroid,
        ChdPreset::Custom => ChdPresets::Custom,
    }
}

/// Rejects a stored or submitted [`OutputSettings`] that would fail at archive time, so a
/// bad compression level is a 400 rather than a job that dies an hour in.
pub fn validate_output(settings: &OutputSettings) -> Result<()> {
    for (id, format) in &settings.per_platform {
        let platform = Platform::from_igdb_id(*id).ok_or_else(|| Error::invalid(format!("unknown platform id {id}")))?;
        if !output_formats_for(platform).contains(format) {
            return Err(Error::invalid(format!("{} can't be written as {}", platform.display_name(), format.name())));
        }
    }
    // Only what can be written: the codec enums also name ones 7-Zip reads but won't create.
    for (format, codec, offered) in [("zip", &settings.zip.codec, &romexplorer_types::copy::ZIP_CODECS[..]), ("7z", &settings.seven_zip.codec, &romexplorer_types::copy::SEVEN_ZIP_CODECS[..])] {
        if !offered.contains(&codec.to_ascii_lowercase().as_str()) {
            return Err(Error::invalid(format!("{format} can't be written with {codec}; one of {}", offered.join(", "))));
        }
    }
    // Build each one and let the rom layer's own range checks speak.
    let mut zip = ZipArgs { codec: zip_codec(&settings.zip.codec)?, compression_level: settings.zip.level.max(0) as u8 };
    if !zip.check_compression_level() {
        let wanted = zip.compression_level;
        zip.correct_compression_level();
        return Err(Error::invalid(format!("zip level {wanted} is out of range for {} (try {})", settings.zip.codec, zip.compression_level)));
    }
    let mut sz = SevenZipArgs { codec: seven_zip_codec(&settings.seven_zip.codec)?, compression_level: settings.seven_zip.level.max(0) as u8 };
    if !sz.check_compression_level() {
        let wanted = sz.compression_level;
        sz.correct_compression_level();
        return Err(Error::invalid(format!("7z level {wanted} is out of range for {} (try {})", settings.seven_zip.codec, sz.compression_level)));
    }
    for (name, format, codec) in [("rvz", OutputFormat::Rvz, &settings.rvz.codec), ("wia", OutputFormat::Wia, &settings.wia.codec)] {
        let mut args = disc_args(settings, format)?;
        if !args.check_compression_level() {
            let wanted = args.compression_level;
            args.correct_compression_level();
            return Err(Error::invalid(format!("{name} level {wanted} is out of range for {codec} (try {})", args.compression_level)));
        }
    }
    disc_args(settings, OutputFormat::Gcz)?;
    for (media, codecs) in [("cd", &settings.chd.codecs), ("dvd", &settings.chd.dvd_codecs)] {
        if codecs.len() > romexplorer_types::copy::CHD_MAX_CODECS {
            return Err(Error::invalid(format!("a {media} chd takes at most {} codecs, not {}", romexplorer_types::copy::CHD_MAX_CODECS, codecs.len())));
        }
        for c in codecs {
            chd_codec(c)?;
        }
    }
    Ok(())
}

/// The `ArchiveArgs` for writing `format`, or `None` when nothing needs archiving.
///
/// `None` is the fast path and the safe one: a source already in the target format with
/// re-archiving off is copied byte for byte, never decoded and re-encoded. Deciding that
/// here rather than inside `rom::operation` matters, because `Single` would otherwise
/// extract and re-detect the whole file before discovering it had nothing to do.
pub fn archive_args(settings: &OutputSettings, platform: Platform, format: OutputFormat, source_format: OutputFormat) -> Result<Option<ArchiveArgs>> {
    let re_archive = match format {
        OutputFormat::Zip => settings.zip.re_archive,
        OutputFormat::SevenZip => settings.seven_zip.re_archive,
        OutputFormat::Chd => settings.chd.re_archive,
        OutputFormat::Rvz => settings.rvz.re_archive,
        OutputFormat::Gcz => settings.gcz.re_archive,
        OutputFormat::Wia => settings.wia.re_archive,
        // The rest are converters of their own (or no conversion at all).
        _ => false,
    };
    if format == OutputFormat::Same {
        return Ok(None);
    }
    if matches!(format, OutputFormat::Raw | OutputFormat::Wua | OutputFormat::Zar | OutputFormat::Xiso | OutputFormat::Cso) || format.is_3ds() {
        // Unpacking is an extract, and .wua, .zar, XISO, CSO and 3DS have converters of their own,
        // not the archiver's: no ArchiveArgs, all handled by the run.
        return Ok(None);
    }
    if format == source_format && !re_archive {
        return Ok(None);
    }
    let mut args = ArchiveArgs::default();
    match format {
        OutputFormat::Zip => {
            args.do_zip = true;
            args.repack_zip = re_archive;
            args.zip_args = ZipArgs { codec: zip_codec(&settings.zip.codec)?, compression_level: settings.zip.level.max(0) as u8 };
            args.zip_args.correct_compression_level();
        }
        OutputFormat::SevenZip => {
            args.do_7z = true;
            args.repack_7z = re_archive;
            args.seven_zip_args = SevenZipArgs { codec: seven_zip_codec(&settings.seven_zip.codec)?, compression_level: settings.seven_zip.level.max(0) as u8 };
            args.seven_zip_args.correct_compression_level();
        }
        OutputFormat::Chd => {
            args.do_chd = true;
            args.repack_chd = re_archive;
            let mut chd = ChdArgs {
                preset: chd_preset(settings.chd.preset),
                is_cd: false,
                num_processors: None,
                hunk_size: settings.chd.hunk_size.map(|n| n as usize),
                unit_size: settings.chd.unit_size.map(|n| n as usize),
                codecs: chd_codecs(&settings.chd.codecs)?,
                dvd_hunk_size: settings.chd.dvd_hunk_size.map(|n| n as usize),
                dvd_codecs: chd_codecs(&settings.chd.dvd_codecs)?,
            };
            // `is_cd` is the input's own layout, which only the operation knows; it sets it
            // before calling `apply_preset`, so leave the preset unapplied here.
            chd.is_cd = false;
            args.chd_args = chd;
        }
        OutputFormat::Rvz | OutputFormat::Gcz | OutputFormat::Wia => {
            // One set of Dolphin arguments, filled from the format's own settings.
            args.rvz_args = disc_args(settings, format)?;
            args.rvz_args.correct_compression_level();
            match format {
                OutputFormat::Rvz => {
                    args.do_rvz = true;
                    args.repack_rvz = re_archive;
                }
                OutputFormat::Gcz => {
                    args.do_gcz = true;
                    args.repack_gcz = re_archive;
                }
                _ => {
                    args.do_wia = true;
                    args.repack_wia = re_archive;
                }
            }
        }
        _ => unreachable!("handled above"),
    }
    args.validate().map_err(|e| Error::invalid(e.to_string()))?;
    let _ = platform;
    Ok(Some(args))
}

// --- destination layout ---

/// The folder name `platform` gets under `scheme`, before any per-platform override.
pub fn scheme_folder(platform: Platform, scheme: FolderScheme, romm_slug: Option<&str>) -> String {
    let name = match scheme {
        FolderScheme::Dat => platform.dat_folder_name(),
        FolderScheme::Display => platform.display_name().to_string(),
        // IGDB has no platform for our own sentinels, so the short name is the fallback --
        // never an empty folder.
        FolderScheme::Slug => romm_slug.map(str::to_string).or_else(|| platform.romm_slug_seed().map(str::to_string)).unwrap_or_else(|| platform.es_folder().to_string()),
        FolderScheme::Es | FolderScheme::Custom => platform.es_folder().to_string(),
    };
    sanitize_segment(&name)
}

/// The whole layout decision for one platform: combine groups, then the scheme, then the
/// user's own override for whichever platform the combine landed on.
pub fn folder_for(platform: Platform, scheme: FolderScheme, combine: &std::collections::HashMap<String, bool>, overrides: &std::collections::HashMap<u16, String>, slugs: &std::collections::HashMap<u16, String>) -> String {
    let target = apply_combine(platform, combine);
    let id = target.igdb_id();
    if let Some(custom) = overrides.get(&id).filter(|s| !s.trim().is_empty()) {
        return sanitize_segment(custom);
    }
    scheme_folder(target, scheme, slugs.get(&id).map(String::as_str))
}

/// Whether `platform` is folded into another platform's folder right now.
pub fn combined_into(platform: Platform, combine: &std::collections::HashMap<String, bool>) -> Option<Platform> {
    let g = combine_group_of(platform)?;
    combine.get(g.key).copied().unwrap_or(g.default_on).then_some(g.parent)
}

/// `destination` must be somewhere we can actually write before a single byte is read.
pub fn check_destination(destination: &str) -> Result<std::path::PathBuf> {
    let path = Path::new(destination.trim());
    if destination.trim().is_empty() {
        return Err(Error::invalid("pick a destination folder"));
    }
    if !path.is_absolute() {
        return Err(Error::invalid(format!("the destination must be an absolute path: {destination:?}")));
    }
    if !path.exists() {
        return Err(Error::invalid(format!("{destination} doesn't exist")));
    }
    if !path.is_dir() {
        return Err(Error::invalid(format!("{destination} isn't a folder")));
    }
    Ok(path.to_path_buf())
}

/// Write-and-delete probe. A read-only mount and a full disk both look fine to `is_dir`.
pub fn check_writable(destination: &Path) -> Result<()> {
    let probe = destination.join(".romexplorer-write-check");
    std::fs::write(&probe, b"romexplorer").map_err(|e| Error::invalid(format!("can't write to {}: {e}", destination.display())))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// What each mode needs of the machine it runs on, checked before a plan is built.
///
/// The five modes differ in where the bytes come from and go, and each of those has a
/// precondition the planner cannot discover for itself: an in-server copy reorganises a
/// collection this machine owns, so a client -- whose `local_*` rows are its own, but whose
/// catalogue is its server's -- is the wrong machine to run it from; an upload writes into
/// somebody else's collection, so it needs a mount to write through and a source to tell to
/// re-scan afterwards.
pub async fn validate_mode(app: &App, req: &CopyFilesRequest) -> Result<()> {
    match req.mode {
        CopyMode::InServer => {
            if crate::database::settings::instance_mode(&app.pool).await? == crate::database::settings::InstanceMode::Client {
                return Err(Error::invalid(
                    "an in-server copy rearranges the server's own collection, so it runs on the server. \
                     Open the server's console (or its web UI) and start it there -- from here, copy to this machine or upload to the server instead.",
                ));
            }
            Ok(())
        }
        CopyMode::Upload => {
            let target = plan::upload_target(app, req).await?.ok_or_else(|| {
                Error::invalid("there's no server to upload to -- add one under Config > Share and mark it as the main server")
            })?;
            let mounts = mount::load(app, Some(target)).await?;
            if mounts.is_empty() {
                return Err(Error::invalid(format!(
                    "source {target} has no mount on this machine, so there's nowhere to write the upload -- set one up under Config > Share first"
                )));
            }
            // The destination has to be *inside* that mount, or the files land on this
            // machine under a name that looks like the server's.
            let root = check_destination(&req.destination)?;
            let inside = mounts.iter().any(|m| {
                let local = Path::new(&m.local_path);
                root == local || root.starts_with(local)
            });
            if !inside {
                return Err(Error::invalid(format!(
                    "{} isn't inside a mount of source {target} -- an upload has to be written through the server's share, not next to it",
                    root.display()
                )));
            }
            Ok(())
        }
        CopyMode::ToCollection | CopyMode::ToExternal | CopyMode::InLocal => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wii_u_game_held_as_wua_and_wux_keeps_the_preferred_one() {
        use romexplorer_types::copy::{HeaderPreference, WiiuPreference};
        let order = romexplorer_types::copy::default_format_order();
        let c = |name: &'static str, dir: i64| Candidate { name, place: name.to_string(), headerless: false, directory: vec![dir] };
        let pick = |files: Vec<Candidate<'static>>, wiiu: WiiuPreference| -> Vec<&'static str> {
            let keep = chosen_files(&files, HeaderPreference::Priority, wiiu, &order);
            files.iter().zip(keep).filter(|(_, k)| *k).map(|(f, _)| f.name).collect()
        };
        let held = || vec![c("Hoge (USA).wux", 1), c("Hoge (USA).key", 1), c("Hoge (USA).wua", 2), c("Fuga (USA).wux", 1), c("Fuga (USA).key", 1)];
        // The .wua, and the disc's key goes with the disc. Another game is untouched.
        assert_eq!(pick(held(), WiiuPreference::Wua), vec!["Hoge (USA).wua", "Fuga (USA).wux", "Fuga (USA).key"]);
        assert_eq!(pick(held(), WiiuPreference::Wux), vec!["Hoge (USA).wux", "Hoge (USA).key", "Fuga (USA).wux", "Fuga (USA).key"]);
        // Folder priority: the .wux's folder comes first here.
        assert_eq!(pick(held(), WiiuPreference::Priority), vec!["Hoge (USA).wux", "Hoge (USA).key", "Fuga (USA).wux", "Fuga (USA).key"]);
        assert_eq!(pick(vec![c("Hoge (USA).wux", 2), c("Hoge (USA).wua", 1)], WiiuPreference::Priority), vec!["Hoge (USA).wua"]);
        // .wux before .wud when the disc is wanted.
        assert_eq!(pick(vec![c("Hoge.wud", 1), c("Hoge.wux", 2), c("Hoge.wua", 3)], WiiuPreference::Wux), vec!["Hoge.wux"]);
        // Without a .wua there is nothing to choose.
        assert_eq!(pick(vec![c("Hoge.wud", 1), c("Hoge.wux", 2)], WiiuPreference::Wua), vec!["Hoge.wud", "Hoge.wux"]);
    }

    #[test]
    fn one_format_per_game_and_one_file_per_name() {
        use romexplorer_types::copy::HeaderPreference;
        let order = romexplorer_types::copy::default_format_order();
        let c = |name: &'static str, headerless: bool, dir: i64| Candidate { name, place: name.to_string(), headerless, directory: vec![dir] };
        let pick = |files: Vec<Candidate<'static>>, header: HeaderPreference| -> Vec<&'static str> {
            let keep = chosen_files(&files, header, romexplorer_types::copy::WiiuPreference::default(), &order);
            files.iter().zip(keep).filter(|(_, k)| *k).map(|(f, _)| f.name).collect()
        };
        // FDS: fds before qd, whatever the folder order.
        assert_eq!(pick(vec![c("Hoge (Japan).qd", false, 1), c("Hoge (Japan).fds", false, 2)], HeaderPreference::Priority), vec!["Hoge (Japan).fds"]);
        assert_eq!(pick(vec![c("Hoge (Japan).v64", false, 1), c("Hoge (Japan).z64", false, 2)], HeaderPreference::Priority), vec!["Hoge (Japan).z64"]);
        assert_eq!(pick(vec![c("Hoge (USA).smc", false, 1), c("Hoge (USA).sfc", false, 2)], HeaderPreference::Priority), vec!["Hoge (USA).sfc"]);
        // NES: the header preference; with none, the folder first in priority.
        assert_eq!(pick(vec![c("Hoge (USA).nes", false, 2), c("Hoge (USA).unh", true, 1)], HeaderPreference::Headered), vec!["Hoge (USA).nes"]);
        assert_eq!(pick(vec![c("Hoge (USA).nes", false, 2), c("Hoge (USA).unh", true, 1)], HeaderPreference::Priority), vec!["Hoge (USA).unh"]);
        // A disc's sheet and tracks are not two formats of one thing; other games stay too.
        assert_eq!(pick(vec![c("Hoge (Japan).cue", false, 1), c("Hoge (Japan).bin", false, 1), c("Fuga (Japan).qd", false, 1)], HeaderPreference::Priority), vec!["Hoge (Japan).cue", "Hoge (Japan).bin", "Fuga (Japan).qd"]);
        // Two dumps under one name: the header preference, then the folder.
        assert_eq!(pick(vec![c("Hoge.nes", false, 1), c("Hoge.nes", true, 2)], HeaderPreference::Headerless), vec!["Hoge.nes"]);
        let keep = chosen_files(&[c("Hoge.nes", false, 2), c("Hoge.nes", false, 1)], HeaderPreference::Priority, romexplorer_types::copy::WiiuPreference::default(), &order);
        assert_eq!(keep, vec![false, true]);
        // One name in two places (two discs' folders, two subfolders of a bundle): both go.
        let at = |place: &str| Candidate { name: "track01.bin", place: place.to_string(), headerless: false, directory: vec![1] };
        let keep = chosen_files(&[at("Game (Disc 1)/track01.bin"), at("Game (Disc 2)/track01.bin")], HeaderPreference::Priority, romexplorer_types::copy::WiiuPreference::default(), &order);
        assert_eq!(keep, vec![true, true]);
        assert!(is_encrypted_path("/roms/Nintendo - Nintendo 3DS -Encrypted-/Hoge.3ds"));
        assert!(!is_encrypted_path("/roms/Nintendo - Nintendo 3DS -Decrypted-/Hoge.3ds"));
    }

    /// The settings forms offer exactly these; every one must be something the server takes.
    #[test]
    fn every_offered_archive_option_parses() {
        use romexplorer_types::copy::{CHD_CODECS, RVZ_BLOCK_SIZES, RVZ_CODECS, SEVEN_ZIP_CODECS, ZIP_CODECS};
        for c in ZIP_CODECS {
            zip_codec(c).unwrap();
        }
        for c in SEVEN_ZIP_CODECS {
            seven_zip_codec(c).unwrap();
        }
        for c in RVZ_CODECS {
            rvz_codec(c).unwrap();
        }
        // ...and every codec at its offered default level passes the rom layer's range check.
        for c in ZIP_CODECS {
            let (lo, hi, def) = romexplorer_types::copy::codec_levels(c);
            let args = ZipArgs { codec: zip_codec(c).unwrap(), compression_level: def as u8 };
            assert!(args.check_compression_level() && lo <= def && def <= hi, "zip {c}");
            assert!(ZipArgs { compression_level: hi as u8, ..args }.check_compression_level(), "zip {c} max");
        }
        for c in SEVEN_ZIP_CODECS {
            let (_, hi, def) = romexplorer_types::copy::codec_levels(c);
            let args = SevenZipArgs { codec: seven_zip_codec(c).unwrap(), compression_level: def as u8 };
            assert!(args.check_compression_level(), "7z {c}");
            assert!(SevenZipArgs { compression_level: hi as u8, ..args }.check_compression_level(), "7z {c} max");
        }
        for b in RVZ_BLOCK_SIZES {
            rvz_block(b).unwrap();
        }
        for c in CHD_CODECS {
            chd_codec(c).unwrap();
        }
    }

    #[test]
    fn a_codec_that_can_only_be_read_is_refused_up_front() {
        let mut s = OutputSettings::default();
        s.zip.codec = "deflate64".into();
        assert!(validate_output(&s).unwrap_err().message.contains("can't be written"));
        s.zip.codec = "zstd".into();
        s.seven_zip.codec = "ppmd".into();
        assert!(validate_output(&s).is_err());
    }

    #[test]
    fn gcz_and_wia_have_settings_of_their_own() {
        let mut s = OutputSettings::default();
        s.gcz.block_size = "32k".into();
        s.wia.codec = "lzma2".into();
        s.wia.level = 9;
        let gcz = archive_args(&s, Platform::GameCube, OutputFormat::Gcz, OutputFormat::Same).unwrap().unwrap();
        assert_eq!(gcz.rvz_args.block_size, RvzBlockSize::Block_32k);
        let wia = archive_args(&s, Platform::GameCube, OutputFormat::Wia, OutputFormat::Same).unwrap().unwrap();
        assert_eq!((wia.rvz_args.algo, wia.rvz_args.block_size, wia.rvz_args.compression_level), (RvzCodecs::Lzma2, RvzBlockSize::Block_2m, 9));
        let rvz = archive_args(&s, Platform::GameCube, OutputFormat::Rvz, OutputFormat::Same).unwrap().unwrap();
        assert_eq!((rvz.rvz_args.algo, rvz.rvz_args.block_size), (RvzCodecs::Zstd, RvzBlockSize::Block_128k), "rvz untouched by the others");
        s.wia.codec = "zstd".into();
        assert!(validate_output(&s).is_err(), "WIA has no zstd");
        for c in romexplorer_types::copy::WIA_CODECS {
            rvz_codec(c).unwrap();
        }
    }

    #[test]
    fn a_chd_takes_at_most_four_codecs() {
        let mut s = OutputSettings::default();
        s.chd.codecs = vec!["cdzs".into(), "cdfl".into(), "cdlz".into(), "cdzl".into()];
        validate_output(&s).unwrap();
        s.chd.codecs.push("zstd".into());
        assert!(validate_output(&s).is_err());
        s.chd.codecs.pop();
        s.chd.dvd_codecs = vec!["zstd".into(), "lzma".into(), "zlib".into(), "flac".into(), "huff".into()];
        assert!(validate_output(&s).is_err(), "the dvd list has the same limit");
    }

    /// Custom CHD values: the CD pair for a CD, the DVD pair for anything else, and every
    /// codec of the pair reaches chdman, not just the first.
    #[test]
    fn custom_chd_values_are_per_media_and_reach_chdman_whole() {
        use crate::rom::archive::ArchiveTarget;
        let mut s = OutputSettings::default();
        s.chd.preset = ChdPreset::Custom;
        s.chd.codecs = vec!["cdzs".into(), "cdfl".into()];
        s.chd.hunk_size = Some(19584);
        s.chd.dvd_codecs = vec!["lzma".into(), "zstd".into(), "huff".into()];
        s.chd.dvd_hunk_size = Some(4096);
        let args = archive_args(&s, Platform::PlayStation, OutputFormat::Chd, OutputFormat::Same).unwrap().unwrap();
        let cd = args.archive_options(ArchiveTarget::Chd, Platform::PlayStation, true).unwrap();
        assert_eq!(cd.hunk_size, Some(19584));
        assert_eq!(cd.chd_codecs.as_deref().map(<[_]>::len), Some(2));
        let dvd = args.archive_options(ArchiveTarget::Chd, Platform::PlayStation2, false).unwrap();
        assert_eq!(dvd.hunk_size, Some(4096));
        assert_eq!(dvd.chd_codecs.as_deref().map(<[_]>::len), Some(3));
    }

    #[test]
    fn sanitize_replaces_what_no_filesystem_takes() {
        assert_eq!(sanitize_segment("Sample 2 (USA)"), "Sample 2 (USA)");
        assert_eq!(sanitize_segment("a/b\\c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j");
        assert_eq!(sanitize_segment("x\u{1}y"), "x_y");
    }

    /// Windows drops these silently, which would merge two different names into one file.
    #[test]
    fn sanitize_trims_trailing_dots_and_spaces() {
        assert_eq!(sanitize_segment("game.  "), "game");
        assert_eq!(sanitize_segment("game..."), "game");
        assert_eq!(sanitize_segment("   "), "_");
    }

    #[test]
    fn sanitize_defuses_dos_reserved_names() {
        assert_eq!(sanitize_segment("CON"), "CON_");
        assert_eq!(sanitize_segment("nul.zip"), "nul.zip_");
        assert_eq!(sanitize_segment("COM9"), "COM9_");
        assert_eq!(sanitize_segment("CONTRA"), "CONTRA");
    }

    #[test]
    fn sanitize_refuses_dot_names() {
        assert_eq!(sanitize_segment("."), "_");
        assert_eq!(sanitize_segment(".."), "_");
    }

    #[test]
    fn sanitize_caps_length_without_eating_the_extension() {
        let long = format!("{}.chd", "a".repeat(400));
        let out = sanitize_segment(&long);
        assert!(out.len() <= 255, "{} bytes", out.len());
        assert!(out.ends_with(".chd"), "{out}");
    }

    #[test]
    fn dedup_suffix_goes_before_the_extension() {
        assert_eq!(with_dedup_suffix("game.chd", 2), "game (2).chd");
        assert_eq!(with_dedup_suffix("game", 3), "game (3)");
        assert_eq!(with_dedup_suffix(".hidden", 2), ".hidden (2)");
    }

    #[test]
    fn disc_platforms_offer_their_own_containers() {
        let ps1 = output_formats_for(Platform::PlayStation);
        assert!(ps1.contains(&OutputFormat::Chd));
        assert!(!ps1.contains(&OutputFormat::Rvz));
        let gc = output_formats_for(Platform::GameCube);
        assert!(gc.contains(&OutputFormat::Rvz) && gc.contains(&OutputFormat::Gcz) && gc.contains(&OutputFormat::Wia));
        assert!(!gc.contains(&OutputFormat::Chd));
        let snes = output_formats_for(Platform::Snes);
        assert_eq!(snes, vec![OutputFormat::Same, OutputFormat::Raw, OutputFormat::Zip, OutputFormat::SevenZip]);
        let n3ds = output_formats_for(Platform::Nintendo3ds);
        assert!([OutputFormat::Cci, OutputFormat::ThreeDs, OutputFormat::Zcci, OutputFormat::Cxi, OutputFormat::Zcxi].iter().all(|f| n3ds.contains(f)));
        assert!(!snes.iter().any(|f| f.is_3ds()));
    }

    #[test]
    fn every_platform_can_at_least_be_copied_and_zipped() {
        for p in crate::database::connection::all_platforms() {
            let v = output_formats_for(p);
            assert!(v.contains(&OutputFormat::Same) && v.contains(&OutputFormat::Zip), "{p:?}");
        }
    }

    #[test]
    fn ps3_and_friends_are_flagged_as_pointless_to_compress() {
        assert!(compressing_is_pointless(Platform::PlayStation3));
        assert!(compressing_is_pointless(Platform::WiiU));
        assert!(!compressing_is_pointless(Platform::PlayStation));
        assert!(!compressing_is_pointless(Platform::GameCube));
        assert!(!compressing_is_pointless(Platform::Snes));
    }

    /// The whole point of the flag: already-in-format plus re-archive off is a byte copy.
    #[test]
    fn already_in_format_skips_the_archiver_unless_re_archive_is_on() {
        let mut s = OutputSettings::default();
        assert!(archive_args(&s, Platform::Snes, OutputFormat::Zip, OutputFormat::Zip).unwrap().is_none());
        s.zip.re_archive = true;
        let args = archive_args(&s, Platform::Snes, OutputFormat::Zip, OutputFormat::Zip).unwrap().expect("re-archive builds args");
        assert!(args.do_zip && args.repack_zip);
    }

    #[test]
    fn same_and_raw_never_archive() {
        let s = OutputSettings::default();
        assert!(archive_args(&s, Platform::Snes, OutputFormat::Same, OutputFormat::Zip).unwrap().is_none());
        assert!(archive_args(&s, Platform::Snes, OutputFormat::Raw, OutputFormat::Zip).unwrap().is_none());
    }

    #[test]
    fn a_format_change_builds_args_even_with_re_archive_off() {
        let s = OutputSettings::default();
        let args = archive_args(&s, Platform::PlayStation, OutputFormat::Chd, OutputFormat::Same).unwrap().expect("iso -> chd archives");
        assert!(args.do_chd && !args.repack_chd);
        assert_eq!(args.chd_args.preset, ChdPresets::ChdmanDefault);
    }

    #[test]
    fn the_defaults_are_the_rom_layers_own() {
        let s = OutputSettings::default();
        let zip = archive_args(&s, Platform::Snes, OutputFormat::Zip, OutputFormat::Same).unwrap().unwrap();
        assert_eq!(zip.zip_args, ZipArgs::default());
        let sz = archive_args(&s, Platform::Snes, OutputFormat::SevenZip, OutputFormat::Same).unwrap().unwrap();
        assert_eq!(sz.seven_zip_args, SevenZipArgs::default());
        let rvz = archive_args(&s, Platform::GameCube, OutputFormat::Rvz, OutputFormat::Same).unwrap().unwrap();
        assert_eq!(rvz.rvz_args, RvzArgs::default());
    }

    #[test]
    fn validate_rejects_an_impossible_level_and_a_platform_that_cant_take_the_format() {
        let mut s = OutputSettings::default();
        s.zip.level = 99;
        assert!(validate_output(&s).is_err());
        let mut s = OutputSettings::default();
        s.per_platform.push((Platform::Snes.igdb_id(), OutputFormat::Chd));
        assert!(validate_output(&s).is_err());
        let mut s = OutputSettings::default();
        s.per_platform.push((Platform::PlayStation.igdb_id(), OutputFormat::Chd));
        assert!(validate_output(&s).is_ok());
    }

    #[test]
    fn algorithm_names_round_trip_and_cover_the_fallback_order() {
        for algo in crate::rom::hash::fallback_order() {
            let name = algorithm_name(*algo);
            assert_eq!(algorithm_from_name(name), Some(*algo), "{name}");
            assert!(is_quick_hash(name), "{name} must be offerable as a populate hash");
        }
        // `ra_hash` describes a playable unit, not a file's bytes, so it can never be either
        // side of this.
        assert!(algorithm_from_name("ra_hash").is_none());
        assert!(!is_quick_hash("ra_hash"));
    }

    /// The order is the one `FileData::integrity` already uses, and its whole point is that
    /// the cheap non-cryptographic hashes come first.
    #[test]
    fn the_cheapest_hashes_are_tried_first() {
        let order = crate::rom::hash::fallback_order();
        assert_eq!(algorithm_name(order[0]), "crc32");
        assert_eq!(algorithm_name(order[1]), "xxh3_128");
        assert_eq!(order.len(), QUICK_HASHES.len(), "every offerable hash is in the order");
    }

    #[test]
    fn archive_format_tags_round_trip() {
        assert_eq!(format_of_tag(Some("chd")), OutputFormat::Chd);
        assert_eq!(format_of_tag(Some("7z")), OutputFormat::SevenZip);
        assert_eq!(format_of_tag(Some("raw_iso")), OutputFormat::Same);
        assert_eq!(format_of_tag(None), OutputFormat::Same);
    }

    /// A registered copy writes `OutputFormat::archive_tag` into `local_files.archive_format`,
    /// which every other reader parses as an `ArchiveFormat`. The two spellings agreeing is
    /// what makes that legal, so it is asserted rather than assumed.
    #[test]
    fn output_format_tags_are_the_archive_formats_own() {
        use crate::common::models::ArchiveFormat;
        for (format, archive) in [
            (OutputFormat::Zip, ArchiveFormat::Zip),
            (OutputFormat::SevenZip, ArchiveFormat::SevenZip),
            (OutputFormat::Chd, ArchiveFormat::Chd),
            (OutputFormat::Rvz, ArchiveFormat::Rvz),
            (OutputFormat::Gcz, ArchiveFormat::Gcz),
            (OutputFormat::Wia, ArchiveFormat::Wia),
            (OutputFormat::Xiso, ArchiveFormat::Xiso),
            (OutputFormat::Cso, ArchiveFormat::Cso),
        ] {
            assert_eq!(format.archive_tag(), Some(archive.to_string().as_str()), "{format:?}");
            assert_eq!(format_of_tag(format.archive_tag()), format, "{format:?} must parse back");
        }
        assert_eq!(OutputFormat::Same.archive_tag(), None);
        assert_eq!(OutputFormat::Raw.archive_tag(), None);
    }

    fn combine(pairs: &[(&str, bool)]) -> std::collections::HashMap<String, bool> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn folder_names_follow_the_scheme() {
        let none = std::collections::HashMap::new();
        let slugs = std::collections::HashMap::new();
        let off = combine(&[("super_famicom", false)]);
        assert_eq!(folder_for(Platform::Snes, FolderScheme::Dat, &off, &none, &slugs), "Nintendo - Super Nintendo Entertainment System");
        assert_eq!(folder_for(Platform::Snes, FolderScheme::Display, &off, &none, &slugs), "Super Nintendo Entertainment System");
        assert_eq!(folder_for(Platform::Snes, FolderScheme::Slug, &off, &none, &slugs), "snes");
        assert_eq!(folder_for(Platform::Snes, FolderScheme::Es, &off, &none, &slugs), "snes");
        assert_eq!(folder_for(Platform::SegaGenesis, FolderScheme::Es, &off, &none, &slugs), "megadrive");
        assert_eq!(folder_for(Platform::SegaGenesis, FolderScheme::Slug, &off, &none, &slugs), "genesis");
    }

    #[test]
    fn a_live_slug_beats_the_compiled_seed() {
        let slugs: std::collections::HashMap<u16, String> = [(Platform::Snes.igdb_id(), "snes-renamed".to_string())].into_iter().collect();
        assert_eq!(folder_for(Platform::Snes, FolderScheme::Slug, &combine(&[]), &std::collections::HashMap::new(), &slugs), "snes-renamed");
    }

    #[test]
    fn combining_sends_a_child_to_its_parents_folder() {
        let slugs = std::collections::HashMap::new();
        let none = std::collections::HashMap::new();
        assert_eq!(folder_for(Platform::SuperFamicom, FolderScheme::Es, &combine(&[("super_famicom", true)]), &none, &slugs), "snes");
        assert_eq!(folder_for(Platform::SuperFamicom, FolderScheme::Es, &combine(&[("super_famicom", false)]), &none, &slugs), "sfc");
    }

    /// An override on the parent has to cover the children, or combining would send them to
    /// a folder the user never named.
    #[test]
    fn an_override_on_the_parent_covers_a_combined_child() {
        let slugs = std::collections::HashMap::new();
        let overrides: std::collections::HashMap<u16, String> = [(Platform::Nes.igdb_id(), "Famiclones".to_string())].into_iter().collect();
        assert_eq!(folder_for(Platform::Famicom, FolderScheme::Es, &combine(&[("famicom", true)]), &overrides, &slugs), "Famiclones");
    }

    #[test]
    fn a_custom_name_is_sanitized_like_any_other() {
        let slugs = std::collections::HashMap::new();
        let overrides: std::collections::HashMap<u16, String> = [(Platform::Snes.igdb_id(), "SNES/SFC".to_string())].into_iter().collect();
        assert_eq!(folder_for(Platform::Snes, FolderScheme::Custom, &combine(&[]), &overrides, &slugs), "SNES_SFC");
    }

    #[test]
    fn every_platform_gets_a_non_empty_folder_under_every_scheme() {
        let slugs = std::collections::HashMap::new();
        let none = std::collections::HashMap::new();
        for p in crate::database::connection::all_platforms() {
            for scheme in FolderScheme::ALL {
                let f = folder_for(p, scheme, &combine(&[]), &none, &slugs);
                assert!(!f.trim().is_empty(), "{p:?} under {}", scheme.name());
                assert_eq!(sanitize_segment(&f), f, "{p:?} under {} is not filesystem-safe", scheme.name());
            }
        }
    }
}
