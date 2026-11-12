//! Resolving a selection down to files, and deciding where each one lands.
//!
//! No side effects: `POST /api/v1/copy/preview` is literally this, and the run calls the
//! same function before it moves a byte. A preview that came from different code would be
//! a preview of something else.
//!
//! Three stages. Catalog ids (sets, titles, releases) narrow to releases; a release picks
//! exactly one holder's game; a game expands to its roms and their files. Ownership ids
//! (`game_ids` / `rom_ids` / `file_ids`) join at whichever stage they belong to.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use romexplorer_types::copy::{ConflictPolicy, CopyFilesRequest, CopyPlan, OutputFormat, OwnedRef, PlannedFile, SourcePreference};
use sqlx::Row;

use crate::common::models::Platform;

use super::super::{App, Error, Result};
use super::mount::{self, Resolved};
use super::{apply_combine, format_of_tag, sanitize_segment, with_dedup_suffix};

/// A planned file's `skip` when a different file is at its destination: left alone in a mode
/// that never overwrites, or unanswered where the mode asks -- reported as a failure.
pub(super) const BLOCKED: &str = "something else is already there";
/// ... and when the user (or the default, skip) chose to keep the file that is there.
pub(super) const KEPT: &str = "kept the file already there";

/// One file to copy, source resolved.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// `None` = this machine's `local_files`.
    pub owner: Option<u16>,
    pub file_id: u64,
    /// Where to read it on this machine -- already through the mount when it is remote.
    pub path: PathBuf,
    /// The path as its owner stores it. A directory bundle's shape is measured against
    /// `CopyItem::main_path`, which is also the owner's, so the two have to be comparable;
    /// `path` has been rewritten and is not.
    pub raw_path: String,
    pub content_id: u64,
    pub size: u64,
    pub archive_format: Option<String>,
    /// Set for one member of a zip/7z being unpacked (`OutputFormat::Raw`): its path inside
    /// the archive at `path`. `content_id` and `size` are then the member's own.
    pub member: Option<String>,
}

/// One playable unit -- a single ROM, a cue+bin set, a directory dump. **The unit of work.**
///
/// Every file of an item lands in the same destination directory, computed once, which is
/// what keeps a cue with its tracks and a directory dump with its tree.
#[derive(Debug, Clone)]
pub struct CopyItem {
    pub rom: Option<OwnedRef>,
    pub platform: Platform,
    pub game_name: String,
    /// The release this came from, when the selection named one. Registration links the new
    /// local game to it, which is what makes a copied game show as held without a hash match.
    pub release_id: Option<u64>,
    /// `local_rom.main_path` -- the relative shape of a directory bundle is measured from
    /// its parent, so the tree survives the copy.
    pub main_path: String,
    pub files: Vec<SourceFile>,
    /// The game's `.m3u`, which hangs off the game rather than any one rom.
    pub playlist: Option<SourceFile>,
    /// The game this rom is a disc of, when it came in as part of one: the playlist rides
    /// with the first disc, and its lines are rewritten from what every disc produced.
    pub game: Option<OwnedRef>,
}

/// One file with its destination decided.
#[derive(Debug, Clone)]
pub struct PlannedCopy {
    pub source: SourceFile,
    /// Bytes this entry accounts for on the progress bar. Its own size, except for the one
    /// file that carries a whole-disc conversion: a cue+bin -> chd reads every track, so the
    /// carrier (a two-line `.cue`) would otherwise report a gigabyte of work as nothing.
    pub weight: u64,
    pub destination: PathBuf,
    pub platform: Platform,
    pub format: OutputFormat,
    pub source_format: OutputFormat,
    /// Which item it belongs to, so a partial failure can name the game.
    pub item: usize,
    pub is_playlist: bool,
    /// This file's bytes travel *inside* another entry's output -- a track of a cue+bin that
    /// became one CHD. It is not copied on its own and is not missing: the item is whole
    /// without it.
    pub folded_into_carrier: bool,
    /// Set when it will not be copied, with the reason the summary shows.
    pub skip: Option<String>,
}

/// The full plan: what to do, plus everything the preview reports.
pub struct Planned {
    pub items: Vec<CopyItem>,
    pub files: Vec<PlannedCopy>,
    pub view: CopyPlan,
    /// Content written to more than one destination path, and where.
    pub duplicated_content: Vec<(u64, Vec<String>)>,
}

fn owned(source_id: Option<u16>, id: u64) -> OwnedRef {
    OwnedRef { source_id, id }
}

/// Every release the selection names, in the order it named them. Its games, roms and files
/// are planned separately, from the very copy that was picked.
/// A release the user combined others into brings them along, so every disc of the game is
/// copied (`release_combine`).
async fn releases_for(app: &App, req: &CopyFilesRequest, warnings: &mut Vec<String>) -> Result<Vec<u64>> {
    let named = crate::api::selection::named_releases(app, &req.selection, req.titles_all_releases, warnings).await?;
    crate::api::release_combine::with_combined(&app.pool, &named).await
}

/// A candidate holder of one release.
struct Holder {
    source_id: Option<u16>,
    game_id: u64,
    name: String,
    playlist_file_id: Option<u64>,
    role: String,
    present: i64,
    required: i64,
    /// Among this machine's copies, its place in the release's order (manual, then directory
    /// priority -- `catalog::order_local_games`); 0 for a remote one.
    rank: usize,
    /// Its place in the user's own order of the release's copies, when it has one.
    manual: Option<i64>,
    /// An encrypted dump ("Encrypted" in a folder or file name).
    encrypted: bool,
    /// The release's contents its files are, or hold (a CHD of a disc's tracks).
    covers: HashSet<u64>,
    /// The scanned directory its files sit in (the first, for one spread over several).
    directory: Option<u64>,
    /// The folder one of its files sits in.
    folder: Option<String>,
}

/// A destination as the plan's collision map keys it: case-folded (Unicode-aware), because
/// the destination may be FAT, exFAT, NTFS or SMB, where two names differing only in case
/// are one file and the second copy would silently replace the first.
fn taken_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// The folder a path sits in, on either kind of separator.
fn parent_folder(path: &str) -> Option<String> {
    path.rfind(['/', '\\']).map(|i| path[..i].to_string())
}

/// Which machine's copy of a release to take.
///
/// Completeness is weighed before locality on purpose: copying the local half-dump when the
/// server has the whole thing is never what anyone meant. After that it is the user's
/// preference, then main before peer, then lowest id -- fully deterministic, because a copy
/// run twice must produce the same result.
#[cfg(test)]
fn pick_holder(holders: Vec<Holder>, preference: SourcePreference) -> Option<Holder> {
    pick_holder_with(holders, preference, true)
}

/// [`pick_holder`]; `prefer_decrypted`: a decrypted copy before an encrypted one, after the
/// user's own order of the release's copies.
#[cfg(test)]
fn pick_holder_with(holders: Vec<Holder>, preference: SourcePreference, prefer_decrypted: bool) -> Option<Holder> {
    order_holders(holders, preference, prefer_decrypted).into_iter().next()
}

/// The copies to take of one release: [`pick_holder_with`]'s, and when that holds only some of
/// the release's discs, the copies that hold the others. A scan that did not join a game's
/// discs (no `.m3u`) leaves one game per disc, and taking one game took one disc. A copy is
/// added only for discs nothing taken so far holds, and only when it repeats none of them, so a
/// second copy of disc 1 is never taken. The same source and folder go first.
fn pick_holders_with(holders: Vec<Holder>, preference: SourcePreference, prefer_decrypted: bool) -> Vec<Holder> {
    let mut rest = order_holders(holders, preference, prefer_decrypted);
    if rest.is_empty() {
        return Vec::new();
    }
    let lead = rest.remove(0);
    let mut covered = lead.covers.clone();
    let required = lead.required;
    let wanted = !covered.is_empty();
    let mut out = vec![lead];
    // Re-checked each round: once the copies taken hold every disc, no more are added.
    while wanted && (covered.len() as i64) < required {
        let lead = &out[0];
        let next = rest
            .iter()
            .enumerate()
            .filter(|(_, h)| !h.covers.is_empty() && h.covers.is_disjoint(&covered))
            .min_by_key(|(i, h)| (h.source_id != lead.source_id, h.folder != lead.folder, h.directory != lead.directory, *i))
            .map(|(i, _)| i);
        let Some(i) = next else { break };
        let h = rest.remove(i);
        covered.extend(h.covers.iter().copied());
        out.push(h);
    }
    out
}

/// A release's copies, best first.
fn order_holders(mut holders: Vec<Holder>, preference: SourcePreference, prefer_decrypted: bool) -> Vec<Holder> {
    if let SourcePreference::Source(want) = preference {
        holders.retain(|h| h.source_id == Some(want));
    }
    holders.sort_by_key(|h| {
        // `present` counts the release's files a copy has or holds (a CHD holds its tracks).
        let incomplete = u8::from(h.required > 0 && h.present < h.required);
        let preferred = match preference {
            SourcePreference::LocalFirst => u8::from(h.source_id.is_some()),
            SourcePreference::MainFirst => u8::from(!(h.source_id.is_some() && h.role == "main")),
            SourcePreference::Source(_) => 0,
        };
        let peer = u8::from(h.source_id.is_some() && h.role != "main");
        let encrypted = u8::from(prefer_decrypted && h.encrypted);
        (incomplete, preferred, peer, h.source_id.unwrap_or(0), h.manual.is_none(), h.manual.unwrap_or(0), encrypted, h.rank, h.game_id)
    });
    holders
}

/// Which of a release's contents a local game's files are, or hold (`content_contents`).
const LOCAL_COVERS: &str = "SELECT rc.content_id FROM release_contents rc WHERE rc.release_id = ? AND (rc.content_id IN (\
       SELECT lf.content_id FROM local_game_roms gr JOIN local_rom_files rf ON rf.rom_id = gr.rom_id JOIN local_files lf ON lf.id = rf.file_id AND lf.deleted = FALSE WHERE gr.game_id = ?) \
     OR rc.content_id IN (SELECT cc.child_content_id FROM content_contents cc WHERE cc.parent_content_id IN (\
       SELECT lf2.content_id FROM local_game_roms gr2 JOIN local_rom_files rf2 ON rf2.rom_id = gr2.rom_id JOIN local_files lf2 ON lf2.id = rf2.file_id AND lf2.deleted = FALSE WHERE gr2.game_id = ?)))";

/// [`LOCAL_COVERS`] for a server's game.
const REMOTE_COVERS: &str = "SELECT rc.content_id FROM release_contents rc WHERE rc.release_id = ? AND (rc.content_id IN (\
       SELECT f.content_id FROM remote_game_roms gr JOIN remote_rom_files rf ON rf.source_id = gr.source_id AND rf.rom_id = gr.rom_id JOIN remote_files f ON f.source_id = rf.source_id AND f.id = rf.file_id AND f.deleted = FALSE WHERE gr.source_id = ? AND gr.game_id = ?) \
     OR rc.content_id IN (SELECT cc.child_content_id FROM content_contents cc WHERE cc.parent_content_id IN (\
       SELECT f2.content_id FROM remote_game_roms gr2 JOIN remote_rom_files rf2 ON rf2.source_id = gr2.source_id AND rf2.rom_id = gr2.rom_id JOIN remote_files f2 ON f2.source_id = rf2.source_id AND f2.id = rf2.file_id AND f2.deleted = FALSE WHERE gr2.source_id = ? AND gr2.game_id = ?)))";

async fn covers_of(app: &App, release_id: u64, source_id: Option<u16>, game_id: u64) -> Result<HashSet<u64>> {
    let ids: Vec<u64> = match source_id {
        None => sqlx::query_scalar(LOCAL_COVERS).bind(release_id).bind(game_id).bind(game_id).fetch_all(&app.pool).await?,
        Some(sid) => sqlx::query_scalar(REMOTE_COVERS).bind(release_id).bind(sid).bind(game_id).bind(sid).bind(game_id).fetch_all(&app.pool).await?,
    };
    Ok(ids.into_iter().collect())
}

async fn holders_for(app: &App, release_id: u64, mounted: &HashSet<u16>) -> Result<Vec<Holder>> {
    let mut out = Vec::new();
    let local = sqlx::query(
        "SELECT g.id, g.name, g.playlist_file_id, \
            (SELECT MIN(lf4.path) FROM local_game_roms gr4 JOIN local_rom_files rf4 ON rf4.rom_id = gr4.rom_id JOIN local_files lf4 ON lf4.id = rf4.file_id WHERE gr4.game_id = g.id) AS any_path, \
            (SELECT COUNT(*) FROM release_contents rcp WHERE rcp.release_id = lrg.release_id AND (rcp.content_id IN (\
               SELECT lf.content_id FROM local_game_roms gr JOIN local_rom_files rf ON rf.rom_id = gr.rom_id JOIN local_files lf ON lf.id = rf.file_id AND lf.deleted = FALSE WHERE gr.game_id = g.id) \
             OR rcp.content_id IN (SELECT cc.child_content_id FROM content_contents cc WHERE cc.parent_content_id IN (\
               SELECT lf2.content_id FROM local_game_roms gr2 JOIN local_rom_files rf2 ON rf2.rom_id = gr2.rom_id JOIN local_files lf2 ON lf2.id = rf2.file_id AND lf2.deleted = FALSE WHERE gr2.game_id = g.id)))) AS present, \
            (SELECT COUNT(*) FROM release_contents rc WHERE rc.release_id = ?) AS required \
         FROM local_release_games lrg JOIN local_game g ON g.id = lrg.game_id AND g.deleted = FALSE WHERE lrg.release_id = ?",
    )
    .bind(release_id)
    .bind(release_id)
    .fetch_all(&app.pool)
    .await?;
    // This machine's copies in the release's order (completeness is weighed separately).
    let places = super::super::catalog::local_game_places(&app.pool, release_id).await?;
    let keys = crate::database::directories::priority_keys(&app.pool).await?;
    let mut order: Vec<(u64, Option<bool>, Option<u64>, Option<i64>)> = places.iter().map(|(id, (dir, pos))| (*id, None, *dir, *pos)).collect();
    super::super::catalog::order_local_games(&mut order, &keys);
    for r in &local {
        let game_id: u64 = r.try_get("id")?;
        let rank = order.iter().position(|o| o.0 == game_id).unwrap_or(usize::MAX);
        let manual = places.get(&game_id).and_then(|(_, pos)| *pos);
        let any_path: Option<String> = r.try_get("any_path")?;
        let encrypted = any_path.as_deref().is_some_and(super::is_encrypted_path);
        let folder = any_path.as_deref().and_then(parent_folder);
        out.push(Holder { source_id: None, game_id, name: r.try_get("name")?, playlist_file_id: r.try_get("playlist_file_id")?, role: "local".into(), present: r.try_get("present")?, required: r.try_get("required")?, rank, manual, encrypted, covers: HashSet::new(), directory: places.get(&game_id).and_then(|(dir, _)| *dir), folder });
    }
    let remote = sqlx::query(
        "SELECT rs.id AS source_id, rs.role, g.id, g.name, g.playlist_file_id, \
            (SELECT MIN(rf5.path) FROM remote_game_roms gr5 JOIN remote_rom_files rrf5 ON rrf5.source_id = gr5.source_id AND rrf5.rom_id = gr5.rom_id JOIN remote_files rf5 ON rf5.source_id = rrf5.source_id AND rf5.id = rrf5.file_id WHERE gr5.source_id = g.source_id AND gr5.game_id = g.id) AS any_path, \
            (SELECT MIN(rf3.directory_id) FROM remote_game_roms gr3 JOIN remote_rom_files rrf3 ON rrf3.source_id = gr3.source_id AND rrf3.rom_id = gr3.rom_id JOIN remote_files rf3 ON rf3.source_id = rrf3.source_id AND rf3.id = rrf3.file_id WHERE gr3.source_id = g.source_id AND gr3.game_id = g.id) AS directory_id, \
            (SELECT COUNT(*) FROM release_contents rcp WHERE rcp.release_id = rrg.release_id AND (rcp.content_id IN (\
               SELECT rf2.content_id FROM remote_game_roms gr JOIN remote_rom_files rrf ON rrf.source_id = gr.source_id AND rrf.rom_id = gr.rom_id JOIN remote_files rf2 ON rf2.source_id = rrf.source_id AND rf2.id = rrf.file_id AND rf2.deleted = FALSE WHERE gr.source_id = g.source_id AND gr.game_id = g.id) \
             OR rcp.content_id IN (SELECT cc.child_content_id FROM content_contents cc WHERE cc.parent_content_id IN (\
               SELECT rf4.content_id FROM remote_game_roms gr4 JOIN remote_rom_files rrf4 ON rrf4.source_id = gr4.source_id AND rrf4.rom_id = gr4.rom_id JOIN remote_files rf4 ON rf4.source_id = rrf4.source_id AND rf4.id = rrf4.file_id AND rf4.deleted = FALSE WHERE gr4.source_id = g.source_id AND gr4.game_id = g.id)))) AS present, \
            (SELECT COUNT(*) FROM release_contents rc WHERE rc.release_id = ?) AS required \
         FROM remote_release_games rrg \
         JOIN remote_game g ON g.source_id = rrg.source_id AND g.id = rrg.game_id AND g.deleted = FALSE \
         JOIN remote_sources rs ON rs.id = rrg.source_id AND rs.enabled = TRUE AND rs.deleted = FALSE \
         WHERE rrg.release_id = ?",
    )
    .bind(release_id)
    .bind(release_id)
    .fetch_all(&app.pool)
    .await?;
    // A server's copies in that server's directory order (Config > Directories there).
    let mut remote_keys: HashMap<u16, HashMap<u64, Vec<i64>>> = HashMap::new();
    let mut ranked: Vec<(Vec<i64>, Holder)> = Vec::new();
    for r in &remote {
        let source_id: u16 = r.try_get("source_id")?;
        // A source with no mount cannot be copied from, so it is not a candidate at all --
        // otherwise it could win the holder vote and strand the release.
        if !mounted.contains(&source_id) {
            continue;
        }
        if !remote_keys.contains_key(&source_id) {
            remote_keys.insert(source_id, crate::database::directories::remote_priority_keys(&app.pool, source_id).await?);
        }
        let dir: Option<u64> = r.try_get("directory_id")?;
        let key = dir.and_then(|d| remote_keys[&source_id].get(&d).cloned()).unwrap_or_else(|| vec![i64::MAX]);
        let any_path: Option<String> = r.try_get("any_path")?;
        let encrypted = any_path.as_deref().is_some_and(super::is_encrypted_path);
        let folder = any_path.as_deref().and_then(parent_folder);
        ranked.push((key, Holder { source_id: Some(source_id), game_id: r.try_get("id")?, name: r.try_get("name")?, playlist_file_id: r.try_get("playlist_file_id")?, role: r.try_get("role")?, present: r.try_get("present")?, required: r.try_get("required")?, rank: 0, manual: None, encrypted, covers: HashSet::new(), directory: dir, folder }));
    }
    ranked.sort_by(|a, b| (a.1.source_id, &a.0, a.1.game_id).cmp(&(b.1.source_id, &b.0, b.1.game_id)));
    for (i, (_, mut h)) in ranked.into_iter().enumerate() {
        h.rank = i;
        out.push(h);
    }
    // Only where there is a choice: which discs each copy holds (`pick_holders_with`).
    if out.len() > 1 {
        for h in out.iter_mut() {
            h.covers = covers_of(app, release_id, h.source_id, h.game_id).await?;
        }
    }
    Ok(out)
}

fn file_from_row(row: &sqlx::mysql::MySqlRow, owner: Option<u16>, mounts: &[Resolved], no_mount: &mut HashSet<String>, source_name: &str) -> Result<Option<SourceFile>> {
    let raw: String = row.try_get("path")?;
    let path = match owner {
        None => PathBuf::from(raw.clone()),
        Some(_) => match mount::rewrite(mounts, &raw) {
            Some(p) => p,
            None => {
                no_mount.insert(source_name.to_string());
                return Ok(None);
            }
        },
    };
    Ok(Some(SourceFile { owner, file_id: row.try_get("id")?, path, raw_path: raw, content_id: row.try_get("content_id")?, size: row.try_get("size")?, archive_format: row.try_get("archive_format")?, member: None }))
}

const LOCAL_ROM_FILES: &str = "SELECT r.id AS rom_id, r.app_platform_id, r.main_path, f.id, f.path, f.content_id, f.size, f.archive_format, f.directory_id \
     FROM local_game_roms gr JOIN local_rom r ON r.id = gr.rom_id AND r.deleted = FALSE \
     JOIN local_rom_files rf ON rf.rom_id = r.id JOIN local_files f ON f.id = rf.file_id AND f.deleted = FALSE \
     WHERE gr.game_id = ? ORDER BY r.disc_number, r.id, f.id";

const REMOTE_ROM_FILES: &str = "SELECT r.id AS rom_id, r.app_platform_id, r.main_path, f.id, f.path, f.content_id, f.size, f.archive_format, f.directory_id \
     FROM remote_game_roms gr JOIN remote_rom r ON r.source_id = gr.source_id AND r.id = gr.rom_id AND r.deleted = FALSE \
     JOIN remote_rom_files rf ON rf.source_id = r.source_id AND rf.rom_id = r.id \
     JOIN remote_files f ON f.source_id = rf.source_id AND f.id = rf.file_id AND f.deleted = FALSE \
     WHERE gr.source_id = ? AND gr.game_id = ? ORDER BY r.disc_number, r.id, f.id";

/// A game's roms, each with its files, as [`CopyItem`]s.
async fn items_for_game(app: &App, owner: Option<u16>, game_id: u64, name: &str, playlist_file_id: Option<u64>, release_id: Option<u64>, mounts: &[Resolved], source_name: &str, no_mount: &mut HashSet<String>) -> Result<Vec<CopyItem>> {
    let rows = match owner {
        None => sqlx::query(LOCAL_ROM_FILES).bind(game_id).fetch_all(&app.pool).await?,
        Some(sid) => sqlx::query(REMOTE_ROM_FILES).bind(sid).bind(game_id).fetch_all(&app.pool).await?,
    };
    let mut by_rom: Vec<CopyItem> = Vec::new();
    let mut dir_of: HashMap<u64, Option<u64>> = HashMap::new();
    for row in &rows {
        let rom_id: u64 = row.try_get("rom_id")?;
        let Some(file) = file_from_row(row, owner, mounts, no_mount, source_name)? else { continue };
        dir_of.insert(file.file_id, row.try_get("directory_id")?);
        match by_rom.iter_mut().find(|i| i.rom == Some(owned(owner, rom_id))) {
            Some(item) => item.files.push(file),
            None => {
                let platform_id: Option<u16> = row.try_get("app_platform_id")?;
                by_rom.push(CopyItem {
                    rom: Some(owned(owner, rom_id)),
                    platform: platform_id.and_then(Platform::from_igdb_id).unwrap_or(Platform::Unknown),
                    game_name: name.to_string(),
                    release_id,
                    main_path: row.try_get("main_path")?,
                    files: vec![file],
                    playlist: None,
                    game: Some(owned(owner, game_id)),
                });
            }
        }
    }
    // Each rom's files in directory priority (the owner's Config > Directories order; stable,
    // so one folder's tracks keep their order). When a rom holds two dumps of one file -- a
    // headered and a headerless cartridge a scan joined -- the plan copies the first only.
    // Headered or headerless first when the user said which (Config > Copy). And of a game
    // held in several formats under one name (`.fds` + `.qd`, `.z64` + `.v64`), the copy takes
    // the format first in its family (`super::chosen_files`).
    if rows.len() > 1 {
        let keys = match owner {
            None => crate::database::directories::priority_keys(&app.pool).await?,
            Some(sid) => crate::database::directories::remote_priority_keys(&app.pool, sid).await?,
        };
        let preference = super::header_preference(app).await?;
        let order = super::format_order(app).await?;
        let wiiu = super::wiiu_preference(app).await?;
        let headerless = if preference == romexplorer_types::copy::HeaderPreference::Priority {
            HashSet::new()
        } else {
            super::headerless_contents(app, &by_rom.iter().flat_map(|i| i.files.iter().map(|f| f.content_id)).collect::<Vec<_>>()).await?
        };
        let dir_key = |f: &SourceFile| dir_of.get(&f.file_id).copied().flatten().and_then(|d| keys.get(&d).cloned()).unwrap_or_else(|| vec![i64::MAX]);
        for item in by_rom.iter_mut() {
            item.files.sort_by_cached_key(|f| (preference.rank(headerless.contains(&f.content_id)), dir_key(f)));
        }
        let names: Vec<(usize, usize, String)> = by_rom.iter().enumerate().flat_map(|(i, item)| item.files.iter().enumerate().map(move |(j, f)| (i, j, last_segment(&f.raw_path)))).collect();
        // A file's place: its rom's name, and inside a bundle its path below the bundle.
        let place = |item: &CopyItem, f: &SourceFile| -> String {
            let inside = bundle_relative(&item.main_path, &f.raw_path).map(|parts| parts.join("/")).unwrap_or_else(|| last_segment(&f.raw_path));
            format!("{}/{inside}", last_segment(&item.main_path))
        };
        let candidates: Vec<super::Candidate<'_>> = names
            .iter()
            .map(|(i, j, name)| {
                let f = &by_rom[*i].files[*j];
                super::Candidate { name: name.as_str(), place: place(&by_rom[*i], f), headerless: headerless.contains(&f.content_id), directory: dir_key(f) }
            })
            .collect();
        let keep = super::chosen_files(&candidates, preference, wiiu, &order);
        let dropped: HashSet<(usize, usize)> = names.iter().zip(&keep).filter(|(_, k)| !**k).map(|((i, j, _), _)| (*i, *j)).collect();
        for (i, item) in by_rom.iter_mut().enumerate() {
            let mut j = 0;
            item.files.retain(|_| {
                let k = !dropped.contains(&(i, j));
                j += 1;
                k
            });
        }
        by_rom.retain(|item| !item.files.is_empty());
    }
    // The `.m3u` belongs to the game, not to any one rom; it rides with the first item so it
    // lands beside the discs it lists.
    if let (Some(pid), Some(first)) = (playlist_file_id, by_rom.first().map(|_| ())) {
        let _ = first;
        let row = match owner {
            None => sqlx::query("SELECT id, path, content_id, size, archive_format FROM local_files WHERE id = ? AND deleted = FALSE").bind(pid).fetch_optional(&app.pool).await?,
            Some(sid) => sqlx::query("SELECT id, path, content_id, size, archive_format FROM remote_files WHERE source_id = ? AND id = ? AND deleted = FALSE").bind(sid).bind(pid).fetch_optional(&app.pool).await?,
        };
        if let Some(row) = row {
            if let Some(f) = file_from_row(&row, owner, mounts, no_mount, source_name)? {
                if let Some(item) = by_rom.first_mut() {
                    item.playlist = Some(f);
                }
            }
        }
    }
    Ok(by_rom)
}

/// Files that are a whole game on their own: one of them goes into the platform folder
/// whatever folder it sits in (a DSi `.dsi` / `.nds`, a `.3ds`, a disc image).
const SELF_CONTAINED: [&str; 18] = ["nds", "dsi", "3ds", "cia", "cci", "cxi", "nsp", "xci", "iso", "chd", "rvz", "wbfs", "cso", "zso", "pbp", "gcz", "wia", "zip"];

/// A folder named for a title id: PSP / PS3 / Vita `NPJH60015`, a 16- or 8-digit Nintendo one.
fn is_title_id(name: &str) -> bool {
    let b = name.as_bytes();
    let sony = b.len() == 9 && b[..4].iter().all(|c| c.is_ascii_uppercase()) && b[4..].iter().all(|c| c.is_ascii_digit());
    let hex = (b.len() == 16 || b.len() == 8) && b.iter().all(|c| c.is_ascii_hexdigit());
    sony || hex
}

/// A file name that says nothing about the game it belongs to: digital titles' `00000001`,
/// `tmd.0`, `title.tmd`, `*.app`, `*.edat`, `*.pkg` -- only its folder names it.
fn is_generic_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let stem = lower.split('.').next().unwrap_or("");
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    (stem.len() >= 8 && stem.chars().all(|c| c.is_ascii_hexdigit()))
        || ["tmd", "cetk", "ticket", "title", "content", "param", "icon"].contains(&stem)
        || ["app", "edat", "pkg", "sfo", "tik", "tmd", "cert", "h3"].contains(&ext)
        || !lower.contains('.')
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The folder of its own a game keeps at the destination, as path segments from the owner's
/// root down to it: a digital title in a folder per game (DSi / Wii / 3DS from the CDN, PSP
/// and Vita DLC), a computer game in a folder per disk. `None`: the files go flat.
///
/// Walking up from the folder the item's files share, a folder is the game's own when it is
/// named like the game (`names`: its release, its game, or one of its files), named for a
/// title id, or -- right above files with generic names (`00000001`) -- carries a "(...)" tag.
/// Failing those, a folder with a "(...)" / "[...]" tag is the game's when `exclusive` says
/// nothing else is in it (the copy lists the folder: the game's record has its files, not
/// which folders hold only them). Collection folders ("NEC - PC-98") are none of these, so
/// they are never kept; a single self-contained file (`SELF_CONTAINED`) always goes flat.
pub fn game_folder(files: &[&str], names: &[&str], exclusive: &dyn Fn(&[String]) -> bool) -> Option<Vec<String>> {
    let segs: Vec<Vec<String>> = files.iter().map(|f| split_path(f)).collect();
    let first = segs.first()?;
    if segs.len() == 1 {
        let ext = first.last().and_then(|n| n.rsplit_once('.')).map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        if SELF_CONTAINED.contains(&ext.as_str()) {
            return None;
        }
    }
    // The folder every file is under.
    let mut common: Vec<String> = first[..first.len().saturating_sub(1)].to_vec();
    for s in &segs[1..] {
        let parent = &s[..s.len().saturating_sub(1)];
        let n = common.iter().zip(parent).take_while(|(a, b)| a == b).count();
        common.truncate(n);
    }
    let mut wanted: HashSet<String> = names.iter().map(|n| norm(n)).filter(|n| !n.is_empty()).collect();
    for s in &segs {
        if let Some(name) = s.last() {
            wanted.insert(norm(name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem)));
        }
    }
    let generic = segs.iter().all(|s| s.last().is_some_and(|n| is_generic_name(n)));
    let mut kept = None;
    let mut level = common.len();
    for depth in 0..3 {
        if level == 0 {
            break;
        }
        let name = &common[level - 1];
        let tagged = name.contains('(') || name.contains('[');
        let own = wanted.contains(&norm(name)) || is_title_id(name) || (depth == 0 && generic && tagged) || (tagged && exclusive(&common[..level]));
        if !own {
            break;
        }
        kept = Some(level);
        level -= 1;
    }
    kept.map(|l| common[..l].to_vec())
}

/// Whether the folder `folder` (the owner's path, as segments) holds nothing but the game's
/// own files (`own`: each file's owner segments and where it is read here) and folders on the
/// way to them -- listed through the first file under it. Unreadable: no.
fn only_the_games(folder: &[String], own: &[(Vec<String>, PathBuf)]) -> bool {
    let Some((segs, local)) = own.iter().find(|(s, _)| s.len() > folder.len() && s[..folder.len()] == *folder) else { return false };
    let Some(dir) = local.ancestors().nth(segs.len() - folder.len()) else { return false };
    let allowed: HashSet<&str> = own.iter().filter(|(s, _)| s.len() > folder.len() && s[..folder.len()] == *folder).map(|(s, _)| s[folder.len()].as_str()).collect();
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    let mut any = false;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !allowed.contains(name.as_str()) {
            return false;
        }
        any = true;
    }
    any
}

/// The destination path of one file of an item.
///
/// A game in a folder of its own (`keep`, from [`game_folder`]) keeps that folder and what is
/// under it. A directory bundle keeps its shape: the part of the path below the bundle root is
/// preserved, so `PS3_GAME/USRDIR/EBOOT.BIN` stays where the emulator expects it. Everything
/// else is a flat `<platform folder>/<the file's own name>`; the source collection's naming
/// is the user's and a copy has no business renaming it.
fn destination_for(root: &std::path::Path, folder: &str, item: &CopyItem, file: &SourceFile, keep: Option<&[String]>) -> PathBuf {
    let dir = root.join(folder);
    if let Some(top) = keep {
        let segs = split_path(&file.raw_path);
        if segs.len() > top.len() && segs[..top.len()] == *top {
            let mut out = dir;
            for c in &segs[top.len() - 1..] {
                out.push(sanitize_segment(c));
            }
            return out;
        }
    }
    match bundle_relative(&item.main_path, &file.raw_path) {
        // A directory bundle (Wii U, PS3, an extracted 3DS): keep its tree, or the emulator
        // will not find `PS3_GAME/USRDIR/EBOOT.BIN` where it expects it.
        Some(rel) if rel.len() > 1 => {
            let mut out = dir.join(sanitize_segment(&last_segment(&item.main_path)));
            for c in rel {
                out.push(sanitize_segment(&c));
            }
            out
        }
        _ => dir.join(sanitize_segment(&last_segment(&file.raw_path))),
    }
}

/// `files` with each zip/7z replaced by its members, as the scan recorded them
/// (`content_contents.member_path`). An archive with no member recorded stays whole.
async fn unpack_members(app: &App, files: &[SourceFile]) -> Result<Vec<SourceFile>> {
    let mut out = Vec::new();
    for file in files {
        if !matches!(format_of_tag(file.archive_format.as_deref()), OutputFormat::Zip | OutputFormat::SevenZip) {
            out.push(file.clone());
            continue;
        }
        let rows = sqlx::query(
            "SELECT cc.child_content_id, cc.member_path, c.size FROM content_contents cc JOIN content c ON c.id = cc.child_content_id \
             WHERE cc.parent_content_id = ? AND cc.member_path IS NOT NULL AND cc.member_path <> '' ORDER BY cc.member_path",
        )
        .bind(file.content_id)
        .fetch_all(&app.pool)
        .await?;
        if rows.is_empty() {
            out.push(file.clone());
            continue;
        }
        for row in rows {
            let member: String = row.try_get("member_path")?;
            out.push(SourceFile { content_id: row.try_get("child_content_id")?, size: row.try_get::<Option<u64>, _>("size")?.unwrap_or(0), member: Some(member), ..file.clone() });
        }
    }
    Ok(out)
}

fn split_path(p: &str) -> Vec<String> {
    p.split(['/', '\\']).filter(|c| !c.is_empty() && *c != ".").map(str::to_string).collect()
}

fn last_segment(p: &str) -> String {
    split_path(p).pop().unwrap_or_default()
}

/// `path`'s components below `bundle_root`, when it really is below it.
///
/// Both are the owner machine's own paths, so this is plain string work -- it must not touch
/// the filesystem, because for a remote item neither path exists here.
fn bundle_relative(bundle_root: &str, path: &str) -> Option<Vec<String>> {
    let root = split_path(bundle_root);
    let full = split_path(path);
    if root.is_empty() || full.len() <= root.len() {
        return None;
    }
    let same = |a: &String, b: &String| if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b };
    root.iter().zip(&full).all(|(a, b)| same(a, b)).then(|| full[root.len()..].to_vec())
}

/// Swaps the extension for the one the target format produces.
/// Whether a format is one whole-disc image rather than a container of separate files.
///
/// The distinction decides how many outputs a rom produces: a cue+bin becomes **one** CHD,
/// while the same two files zipped stay two entries. Converting per file would retarget both
/// tracks to the same `.chd` and collide with itself.
pub fn is_disc_container(format: OutputFormat) -> bool {
    matches!(format, OutputFormat::Chd | OutputFormat::Rvz | OutputFormat::Gcz | OutputFormat::Wia)
}

/// Which of a rom's files carries a whole-disc conversion, if it is one.
///
/// The rom's own `main_path` -- the cue, the gdi, the first track -- since that is what the
/// archiver is handed and what names the output. Falls back to the first file when the main
/// path is not among them, which a directory bundle's `main_path` (the directory itself) is
/// not.
///
/// A zip or 7z of a sheet is one too: handed the cue, the archiver puts the cue and its
/// tracks in one archive, so zipping each track again stored them twice and named a
/// single-track disc's two archives alike.
pub fn disc_carrier(format: OutputFormat, files: &[SourceFile], main_path: &str) -> Option<usize> {
    if files.is_empty() {
        return None;
    }
    if is_disc_container(format) {
        return Some(files.iter().position(|f| f.raw_path == main_path).unwrap_or(0));
    }
    let is_sheet = |p: &str| p.rsplit_once('.').is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("cue") || ext.eq_ignore_ascii_case("gdi"));
    if matches!(format, OutputFormat::Zip | OutputFormat::SevenZip) && files.len() > 1 && is_sheet(main_path) {
        return files.iter().position(|f| f.raw_path == main_path);
    }
    None
}

/// The extension a 3DS file gets as `format`, or `None` when it is copied as it is: not a 3DS
/// file on its own on the disk, not readable, or already decrypted into exactly that.
fn n3ds_destination(path: &Path, ext: &str, is_member: bool, format: OutputFormat) -> Option<&'static str> {
    use crate::rom::n3ds;
    if is_member || !n3ds::EXTENSIONS.contains(&ext) {
        return None;
    }
    let target = super::n3ds_target(format)?;
    let info = n3ds::inspect(path).ok()?;
    let wanted = n3ds::output_extension(&info, target);
    if !info.encrypted && wanted == ext {
        return None;
    }
    Some(wanted)
}

fn retarget(path: &std::path::Path, format: OutputFormat) -> PathBuf {
    match format.extension() {
        Some(ext) => path.with_extension(ext),
        None => path.to_path_buf(),
    }
}

/// The whole planning pass. Pure of side effects; the run calls it and so does the preview.
/// Whether two paths are one file on disk (the same path spelled differently, a link).
fn is_same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

pub async fn plan(app: &App, req: &CopyFilesRequest) -> Result<Planned> {
    let root = super::check_destination(&req.destination)?;
    let mut warnings: Vec<String> = Vec::new();
    let mut no_mount: HashSet<String> = HashSet::new();

    let mounts = mount::load(app, None).await?;
    let mounted: HashSet<u16> = mounts.iter().map(|m| m.source_id).collect();
    let source_names: HashMap<u16, String> = sqlx::query("SELECT id, name FROM remote_sources WHERE deleted = FALSE")
        .fetch_all(&app.pool)
        .await?
        .iter()
        .map(|r| Ok((r.try_get("id")?, r.try_get("name")?)))
        .collect::<Result<_>>()?;
    let name_of = |id: Option<u16>| id.and_then(|i| source_names.get(&i).cloned()).unwrap_or_else(|| "this machine".to_string());

    let scheme = match req.folder_scheme {
        Some(s) => s,
        None => super::folder_scheme(app).await?,
    };
    let combine = super::combine_map(app).await?;
    let settings = match &req.output {
        Some(o) => {
            super::validate_output(o)?;
            o.clone()
        }
        None => super::output_defaults(app).await?,
    };
    let overrides = super::super::naming::folder_overrides(app).await?;
    let formats = super::super::naming::format_overrides(app).await?;
    let slugs = super::super::naming::live_slugs(app).await?;

    // --- stage 1/2/3: selection -> items ---
    let mut items: Vec<CopyItem> = Vec::new();
    let releases = releases_for(app, req, &mut warnings).await?;
    let prefer_decrypted = super::prefer_decrypted(app).await?;
    for release_id in releases {
        let holders = holders_for(app, release_id, &mounted).await?;
        if holders.is_empty() {
            continue;
        }
        for h in pick_holders_with(holders, req.source_preference, prefer_decrypted) {
            let name = name_of(h.source_id);
            let source_mounts: Vec<Resolved> = mounts.iter().filter(|m| Some(m.source_id) == h.source_id).cloned().collect();
            items.extend(items_for_game(app, h.source_id, h.game_id, &h.name, h.playlist_file_id, Some(release_id), &source_mounts, &name, &mut no_mount).await?);
        }
    }
    for g in &req.selection.game_ids {
        let row = match g.source_id {
            None => sqlx::query("SELECT name, playlist_file_id FROM local_game WHERE id = ? AND deleted = FALSE").bind(g.id).fetch_optional(&app.pool).await?,
            Some(sid) => sqlx::query("SELECT name, playlist_file_id FROM remote_game WHERE source_id = ? AND id = ? AND deleted = FALSE").bind(sid).bind(g.id).fetch_optional(&app.pool).await?,
        };
        let Some(row) = row else { continue };
        let name = name_of(g.source_id);
        let source_mounts: Vec<Resolved> = mounts.iter().filter(|m| Some(m.source_id) == g.source_id).cloned().collect();
        let release_id: Option<u64> = match g.source_id {
            None => sqlx::query_scalar("SELECT release_id FROM local_release_games WHERE game_id = ? LIMIT 1").bind(g.id).fetch_optional(&app.pool).await?,
            Some(sid) => sqlx::query_scalar("SELECT release_id FROM remote_release_games WHERE source_id = ? AND game_id = ? LIMIT 1").bind(sid).bind(g.id).fetch_optional(&app.pool).await?,
        };
        items.extend(items_for_game(app, g.source_id, g.id, &row.try_get::<String, _>("name")?, row.try_get("playlist_file_id")?, release_id, &source_mounts, &name, &mut no_mount).await?);
    }
    for r in &req.selection.rom_ids {
        items.extend(item_for_rom(app, *r, &mounts, &name_of(r.source_id), &mut no_mount).await?);
    }
    for f in &req.selection.file_ids {
        items.extend(item_for_file(app, *f, &mounts, &name_of(f.source_id), &mut no_mount).await?);
    }

    // Two selections naming the same rom -- a release and one of its files, say -- are one
    // item, or its files would be planned twice and collide with themselves.
    let mut seen_rom: HashSet<(Option<u16>, u64)> = HashSet::new();
    items.retain(|i| match i.rom {
        Some(r) => seen_rom.insert((r.source_id, r.id)),
        None => true,
    });

    // --- destinations, dedup, conflicts ---
    // Whose holdings count as "already got it" depends on which way the copy runs: into this
    // machine, skip what this machine has; up to the server, skip what the server has. A
    // copy within one collection skips nothing -- a second copy in another format is the
    // whole point of it.
    let held: HashSet<u64> = if req.mode.skips_local_holdings() {
        held_content_ids(app).await?
    } else if req.mode.skips_source_holdings() {
        match upload_target(app, req).await? {
            Some(source_id) => source_held_content_ids(app, source_id).await?,
            None => HashSet::new(),
        }
    } else {
        HashSet::new()
    };
    let unavailable = super::unavailable_formats();

    let mut files: Vec<PlannedCopy> = Vec::new();
    let mut by_content: HashMap<u64, Vec<String>> = HashMap::new();
    let mut taken: HashMap<String, u64> = HashMap::new();
    // Which release put a file at each destination: a second, different file of the *same*
    // release under the same name is another dump of that one file, not a collision. One
    // copy of a release is one file per name -- the one in the priority directory, which
    // `items_for_game` put first.
    let mut taken_by: HashMap<String, Option<u64>> = HashMap::new();
    let mut renamed: Vec<(String, String)> = Vec::new();
    let mut conflicts: Vec<String> = Vec::new();
    let choices: HashMap<String, romexplorer_types::copy::ConflictChoice> = req.conflict_choices.iter().cloned().collect();
    let mut deduped = 0u64;
    let mut already_held = 0u64;
    let mut already_there = 0u64;
    let mut conversions = 0u64;
    let mut archiving_unavailable: HashSet<String> = HashSet::new();

    // Release names, for telling a game's own folder (`game_folder`).
    let mut release_names: HashMap<u64, String> = HashMap::new();
    let release_ids: Vec<u64> = items.iter().filter_map(|i| i.release_id).collect::<HashSet<_>>().into_iter().collect();
    for chunk in release_ids.chunks(1000) {
        let list = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        for (id, name) in sqlx::query_as::<_, (u64, String)>(sqlx::AssertSqlSafe(format!("SELECT id, name FROM `release` WHERE id IN ({list})"))).fetch_all(&app.pool).await? {
            release_names.insert(id, name);
        }
    }
    // Every file of each game, with where it is read here: a folder is the game's own only when
    // everything in it is one of these (`only_the_games`).
    let mut game_files: HashMap<(Option<u16>, u64), Vec<(Vec<String>, PathBuf)>> = HashMap::new();
    for item in &items {
        if let Some(g) = item.game {
            let entry = game_files.entry((g.source_id, g.id)).or_default();
            for f in item.files.iter().chain(item.playlist.iter()) {
                entry.push((split_path(&f.raw_path), f.path.clone()));
            }
        }
    }
    for (index, item) in items.iter().enumerate() {
        let platform = apply_combine(item.platform, &combine);
        let release_name = item.release_id.and_then(|r| release_names.get(&r)).map(String::as_str).unwrap_or("");
        let own: Vec<(Vec<String>, PathBuf)> = match item.game {
            Some(g) => game_files.get(&(g.source_id, g.id)).cloned().unwrap_or_default(),
            None => item.files.iter().map(|f| (split_path(&f.raw_path), f.path.clone())).collect(),
        };
        let exclusive = |folder: &[String]| only_the_games(folder, &own);
        let keep = game_folder(&item.files.iter().map(|f| f.raw_path.as_str()).collect::<Vec<_>>(), &[release_name, item.game_name.as_str()], &exclusive);
        let folder = match overrides.get(&platform.igdb_id()).filter(|s| !s.trim().is_empty()) {
            Some(custom) => sanitize_segment(custom),
            None => super::scheme_folder(platform, scheme, slugs.get(&platform.igdb_id()).map(String::as_str)),
        };
        let wanted = formats.get(&item.platform.igdb_id()).copied().unwrap_or_else(|| settings.format_for(item.platform.igdb_id()));
        // Unpack: each zip or 7z is planned as its members, read from what the scan recorded.
        // An archive with none recorded is copied as it is.
        let unpacked: Vec<SourceFile> = if wanted == OutputFormat::Raw { unpack_members(app, &item.files).await? } else { item.files.clone() };
        let members: Vec<(&SourceFile, bool)> = unpacked.iter().map(|f| (f, false)).chain(item.playlist.iter().map(|f| (f, true))).collect();
        // No carrier when the format can't be made here: the files are then copied as they
        // are, every one of them, not folded into an output that never comes.
        let carrier = if unavailable.contains(&wanted) { None } else { disc_carrier(wanted, &item.files, &item.main_path) };
        for (member_index, (file, is_playlist)) in members.into_iter().enumerate() {
            let ext = file.path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            // A .wua is Cemu's own archive, not one the scan records a tag for.
            let source_format = match ext.as_str() {
                "wua" => OutputFormat::Wua,
                "zar" => OutputFormat::Zar,
                "cso" => OutputFormat::Cso,
                _ => format_of_tag(file.archive_format.as_deref()),
            };
            // A playlist is a text file; converting it makes no sense. Unpacking leaves
            // whatever is not an archive member as it is.
            let mut format = if is_playlist || (wanted == OutputFormat::Raw && file.member.is_none()) { OutputFormat::Same } else { wanted };
            let source_format = if file.member.is_some() { OutputFormat::Same } else { source_format };
            // Only a Wii U disc image becomes a .wua; the rest of the item (an update folder,
            // a manual) is copied as it is -- except the disc's key, which the decrypted
            // archive no longer needs.
            let mut wua_key = false;
            if format == OutputFormat::Wua && !matches!(ext.as_str(), "wud" | "wux" | "wua") {
                wua_key = ext == "key" && item.files.iter().any(|f| f.path.with_extension("key") == file.path && f.path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("wud") || e.eq_ignore_ascii_case("wux")));
                format = OutputFormat::Same;
            }
            // .zar, XISO and CSO are made from a plain disc image; anything else of the item
            // (a zipped copy, a manual, a .zar already) is copied as it is.
            if matches!(format, OutputFormat::Zar | OutputFormat::Xiso | OutputFormat::Cso) && ext != "iso" {
                format = OutputFormat::Same;
            }
            // A 3DS title is decrypted on its own: a loose file the converter reads. Its output
            // extension is the title's own -- an update or DLC stays a .cia, homebrew a .3dsx --
            // and one already decrypted in that very form is copied as it is.
            let mut n3ds_ext = None;
            if format.is_3ds() {
                n3ds_ext = n3ds_destination(&file.path, &ext, file.member.is_some(), format);
                if n3ds_ext.is_none() {
                    format = OutputFormat::Same;
                }
            }
            // Every other file of a disc-container conversion is inside the one output.
            let folded_in = carrier.is_some_and(|c| !is_playlist && member_index != c);
            if format != OutputFormat::Same && unavailable.contains(&format) {
                archiving_unavailable.insert(format.name().to_string());
                format = OutputFormat::Same;
            }
            let base = destination_for(&root, &folder, item, file, keep.as_deref());
            // A member lands where its archive would have, under its own path inside it.
            let base = match &file.member {
                Some(member) => {
                    let mut out = base.parent().map(Path::to_path_buf).unwrap_or_else(|| root.join(&folder));
                    for segment in split_path(member) {
                        out.push(sanitize_segment(&segment));
                    }
                    out
                }
                None => base,
            };
            // Copied byte for byte, whether from a file or out of an archive.
            let plain = format == OutputFormat::Same || file.member.is_some();
            let mut destination = match n3ds_ext {
                Some(ext) if !plain => base.with_extension(ext),
                _ if plain || format == OutputFormat::Raw => base,
                _ => retarget(&base, format),
            };

            let mut skip = None;
            if wua_key && !unavailable.contains(&OutputFormat::Wua) {
                skip = Some("the disc's key, not needed by the .wua".to_string());
            }
            if folded_in {
                skip = Some(format!("inside the converted {}", wanted.name()));
                format = OutputFormat::Same;
            }
            // Held as a whole item only: redump releases share identical tracks across
            // regions, and skipping the shared one alone left the copied cue without it.
            if skip.is_none() && !held.is_empty() && held.contains(&file.content_id) && item.files.iter().all(|f| held.contains(&f.content_id)) {
                already_held += 1;
                skip = Some(if req.mode.skips_source_holdings() { "already on the server".to_string() } else { "already held on this machine".to_string() });
            }
            let key = taken_key(&destination);
            if skip.is_none() {
                match taken.get(&key) {
                    // The same bytes to the same place: copy once. This is the case a
                    // release appearing in several sets produces.
                    Some(content) if *content == file.content_id => {
                        deduped += 1;
                        skip = Some("duplicate of an earlier item".to_string());
                    }
                    // Different bytes, one name, one release: another dump of a file
                    // already planned (a headered and a headerless cartridge).
                    Some(_) if item.release_id.is_some() && taken_by.get(&key).copied().flatten() == item.release_id => {
                        deduped += 1;
                        skip = Some("another dump of this release's file (headered / headerless, or a lower-priority directory)".to_string());
                    }
                    // Different bytes, one name: a real collision.
                    Some(_) => match req.on_conflict {
                        ConflictPolicy::Fail => return Err(Error::conflict(format!("{} is wanted by two different files", destination.display()))),
                        // Overwrite is about what is on the disk: two files of this one job
                        // written to one path at once would only mix. The first keeps it.
                        ConflictPolicy::Overwrite => skip = Some("a different file already claims this name".to_string()),
                        // `skip` is about a file already on the disk. Two files of this one
                        // job wanting one name are both wanted, so the second is renamed.
                        ConflictPolicy::Skip | ConflictPolicy::Rename => {
                            let dir = destination.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| root.clone());
                            let name = destination.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                            let mut n = 2;
                            let chosen = loop {
                                let candidate = dir.join(with_dedup_suffix(&name, n));
                                if !taken.contains_key(&taken_key(&candidate)) {
                                    break candidate;
                                }
                                n += 1;
                            };
                            renamed.push((name, chosen.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()));
                            destination = chosen;
                        }
                    },
                    None => {}
                }
            }
            // What is on the disk already, before the name is claimed: a rename moves it.
            if skip.is_none() {
                if let Ok(meta) = std::fs::metadata(&destination) {
                    let answer = choices.get(&destination.to_string_lossy().to_string()).copied();
                    // Renamed when the user said so for this file, or by the `rename` policy
                    // in a mode that does not ask. A same-size plain file is the resume path,
                    // never a collision.
                    let collides = !(plain && meta.len() == file.size);
                    let wants = match answer {
                        Some(c) => c == romexplorer_types::copy::ConflictChoice::Rename,
                        None => plain && req.on_conflict == ConflictPolicy::Rename && !req.mode.asks_about_conflicts(),
                    };
                    let auto_rename = collides && wants && !req.mode.never_overwrites();
                    if is_same_file(&destination, &file.path) {
                        // Converting into the source's own folder under the same name: an
                        // overwrite would delete the source before reading it.
                        skip = Some("the destination is the source file itself".to_string());
                    } else if auto_rename {
                        // `rename` means nothing is lost: a different file already there keeps
                        // its name, and this one takes the next free one.
                        let dir = destination.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| root.clone());
                        let name = destination.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                        let mut n = 2;
                        let chosen = loop {
                            let candidate = dir.join(with_dedup_suffix(&name, n));
                            if !taken.contains_key(&taken_key(&candidate)) && std::fs::symlink_metadata(&candidate).is_err() {
                                break candidate;
                            }
                            n += 1;
                        };
                        renamed.push((name, chosen.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()));
                        // Still a collision the dialog lists, under the name that was taken,
                        // so the answer can be changed.
                        conflicts.push(destination.to_string_lossy().to_string());
                        destination = chosen;
                    }
                }
            }
            let key = destination.to_string_lossy().to_string();
            if skip.is_none() {
                taken.insert(taken_key(&destination), file.content_id);
                taken_by.insert(taken_key(&destination), item.release_id);
                by_content.entry(file.content_id).or_default().push(key.clone());
                if format != OutputFormat::Same && format != source_format {
                    conversions += 1;
                }
                // A destination of the same size is the resume path: counted here as the
                // cheap guess the preview can afford, and left to the run, which checks it
                // by hash and records it. Anything else already sitting there is a
                // collision the user has to answer.
                if let Ok(meta) = std::fs::metadata(&destination) {
                    if plain && meta.len() == file.size {
                        already_there += 1;
                    } else {
                        conflicts.push(key.clone());
                        // Never-overwrite modes do not ask: the file stays, and the run
                        // reports it as the failure it is.
                        use romexplorer_types::copy::ConflictChoice;
                        let answer = choices.get(&key).copied();
                        if req.mode.never_overwrites() {
                            skip = Some(BLOCKED.to_string());
                        } else if answer == Some(ConflictChoice::Skip) || (answer.is_none() && req.on_conflict == ConflictPolicy::Skip) {
                            // Asked for (or the default): the file there stays, and that is
                            // not a failure.
                            skip = Some(KEPT.to_string());
                        } else if req.mode.asks_about_conflicts() && answer != Some(ConflictChoice::Overwrite) {
                            // Unanswered means unanswered, not yes.
                            skip = Some(BLOCKED.to_string());
                        }
                    }
                }
            }
            let weight = match carrier {
                Some(c) if !is_playlist && member_index == c => item.files.iter().map(|f| f.size).sum(),
                Some(_) if !is_playlist => 0,
                _ => file.size,
            };
            files.push(PlannedCopy { source: file.clone(), weight, destination, platform, format, source_format, item: index, is_playlist, folded_into_carrier: folded_in, skip });
        }
    }

    let duplicated_content: Vec<(u64, Vec<String>)> = by_content.into_iter().filter(|(_, paths)| paths.len() > 1).collect();
    let bytes: u64 = files.iter().map(|f| f.weight).sum();
    let to_copy: Vec<&PlannedCopy> = files.iter().filter(|f| f.skip.is_none()).collect();
    let bytes_to_copy: u64 = to_copy.iter().map(|f| f.weight).sum();
    let free = super::super::storage::available_bytes(&root);
    let margin = super::super::storage::margin(app).await?;
    let required = bytes_to_copy.saturating_add(margin.bytes_for(bytes_to_copy));

    let sample: Vec<PlannedFile> = files
        .iter()
        .take(400)
        .map(|f| PlannedFile {
            source: f.source.path.display().to_string(),
            destination: f.destination.display().to_string(),
            size: f.weight.max(f.source.size),
            from_source: f.source.owner.and_then(|id| source_names.get(&id).cloned()),
            format: f.format,
            skipped: f.skip.clone(),
        })
        .collect();

    let view = CopyPlan {
        destination: root.display().to_string(),
        mode: req.mode,
        items: items.len() as u64,
        files: files.len() as u64,
        bytes,
        files_to_copy: to_copy.len() as u64,
        bytes_to_copy,
        deduped,
        already_held,
        already_at_destination: already_there,
        conversions,
        renamed,
        conflicts,
        no_mount: no_mount.into_iter().collect(),
        archiving_unavailable: archiving_unavailable.into_iter().collect(),
        free_bytes: free,
        required_bytes: required,
        sample,
        warnings,
    };
    Ok(Planned { items, files, view, duplicated_content })
}

/// Content ids this machine already holds in a live file -- what `to_collection` skips.
async fn held_content_ids(app: &App) -> Result<HashSet<u64>> {
    let rows = sqlx::query("SELECT DISTINCT content_id FROM local_files WHERE deleted = FALSE").fetch_all(&app.pool).await?;
    rows.iter().map(|r| Ok(r.try_get("content_id")?)).collect()
}

/// Content ids one source already holds -- what an upload skips, since an upload is for what
/// is not up there yet.
async fn source_held_content_ids(app: &App, source_id: u16) -> Result<HashSet<u64>> {
    let rows = sqlx::query("SELECT DISTINCT content_id FROM remote_files WHERE source_id = ? AND deleted = FALSE").bind(source_id).fetch_all(&app.pool).await?;
    rows.iter().map(|r| Ok(r.try_get("content_id")?)).collect()
}

/// Which server an upload is aimed at: the one named, else the main source, else the only
/// mounted one.
pub async fn upload_target(app: &App, req: &CopyFilesRequest) -> Result<Option<u16>> {
    if let Some(id) = req.upload_source_id {
        return Ok(Some(id));
    }
    let main: Option<u16> = sqlx::query_scalar("SELECT id FROM remote_sources WHERE role = 'main' AND enabled = TRUE AND deleted = FALSE").fetch_optional(&app.pool).await?;
    if main.is_some() {
        return Ok(main);
    }
    let mounted: Vec<u16> = mount::load(app, None).await?.into_iter().map(|m| m.source_id).collect();
    let mut unique: Vec<u16> = mounted;
    unique.sort_unstable();
    unique.dedup();
    Ok((unique.len() == 1).then(|| unique[0]))
}

async fn item_for_rom(app: &App, r: OwnedRef, mounts: &[Resolved], source_name: &str, no_mount: &mut HashSet<String>) -> Result<Vec<CopyItem>> {
    let source_mounts: Vec<Resolved> = mounts.iter().filter(|m| Some(m.source_id) == r.source_id).cloned().collect();
    let rows = match r.source_id {
        None => {
            sqlx::query(
                "SELECT r.app_platform_id, r.main_path, f.id, f.path, f.content_id, f.size, f.archive_format, \
                    (SELECT g.name FROM local_game_roms gr JOIN local_game g ON g.id = gr.game_id WHERE gr.rom_id = r.id LIMIT 1) AS game_name \
                 FROM local_rom r JOIN local_rom_files rf ON rf.rom_id = r.id JOIN local_files f ON f.id = rf.file_id AND f.deleted = FALSE \
                 WHERE r.id = ? AND r.deleted = FALSE ORDER BY f.id",
            )
            .bind(r.id)
            .fetch_all(&app.pool)
            .await?
        }
        Some(sid) => {
            sqlx::query(
                "SELECT r.app_platform_id, r.main_path, f.id, f.path, f.content_id, f.size, f.archive_format, \
                    (SELECT g.name FROM remote_game_roms gr JOIN remote_game g ON g.source_id = gr.source_id AND g.id = gr.game_id WHERE gr.source_id = r.source_id AND gr.rom_id = r.id LIMIT 1) AS game_name \
                 FROM remote_rom r JOIN remote_rom_files rf ON rf.source_id = r.source_id AND rf.rom_id = r.id \
                 JOIN remote_files f ON f.source_id = rf.source_id AND f.id = rf.file_id AND f.deleted = FALSE \
                 WHERE r.source_id = ? AND r.id = ? AND r.deleted = FALSE ORDER BY f.id",
            )
            .bind(sid)
            .bind(r.id)
            .fetch_all(&app.pool)
            .await?
        }
    };
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for row in &rows {
        if let Some(f) = file_from_row(row, r.source_id, &source_mounts, no_mount, source_name)? {
            files.push(f);
        }
    }
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let first = &rows[0];
    let platform_id: Option<u16> = first.try_get("app_platform_id")?;
    let game_name: Option<String> = first.try_get("game_name")?;
    let release_id: Option<u64> = match r.source_id {
        None => sqlx::query_scalar("SELECT lrg.release_id FROM local_game_roms gr JOIN local_release_games lrg ON lrg.game_id = gr.game_id WHERE gr.rom_id = ? LIMIT 1").bind(r.id).fetch_optional(&app.pool).await?,
        Some(sid) => sqlx::query_scalar("SELECT rrg.release_id FROM remote_game_roms gr JOIN remote_release_games rrg ON rrg.source_id = gr.source_id AND rrg.game_id = gr.game_id WHERE gr.source_id = ? AND gr.rom_id = ? LIMIT 1").bind(sid).bind(r.id).fetch_optional(&app.pool).await?,
    };
    Ok(vec![CopyItem {
        rom: Some(r),
        platform: platform_id.and_then(Platform::from_igdb_id).unwrap_or(Platform::Unknown),
        game_name: game_name.unwrap_or_else(|| "(unnamed)".into()),
        release_id,
        main_path: first.try_get("main_path")?,
        files,
        playlist: None,
        game: None,
    }])
}

/// A bare file with no rom: one item of one file, platform from the content itself.
async fn item_for_file(app: &App, r: OwnedRef, mounts: &[Resolved], source_name: &str, no_mount: &mut HashSet<String>) -> Result<Vec<CopyItem>> {
    let source_mounts: Vec<Resolved> = mounts.iter().filter(|m| Some(m.source_id) == r.source_id).cloned().collect();
    let row = match r.source_id {
        None => sqlx::query("SELECT f.id, f.path, f.content_id, f.size, f.archive_format, c.app_platform_id FROM local_files f JOIN content c ON c.id = f.content_id WHERE f.id = ? AND f.deleted = FALSE").bind(r.id).fetch_optional(&app.pool).await?,
        Some(sid) => sqlx::query("SELECT f.id, f.path, f.content_id, f.size, f.archive_format, c.app_platform_id FROM remote_files f JOIN content c ON c.id = f.content_id WHERE f.source_id = ? AND f.id = ? AND f.deleted = FALSE").bind(sid).bind(r.id).fetch_optional(&app.pool).await?,
    };
    let Some(row) = row else { return Ok(Vec::new()) };
    let Some(file) = file_from_row(&row, r.source_id, &source_mounts, no_mount, source_name)? else { return Ok(Vec::new()) };
    let platform_id: Option<u16> = row.try_get("app_platform_id")?;
    let main_path = file.path.display().to_string();
    Ok(vec![CopyItem { rom: None, platform: platform_id.and_then(Platform::from_igdb_id).unwrap_or(Platform::Unknown), game_name: file.path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(), release_id: None, main_path, files: vec![file], playlist: None, game: None }])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder(source_id: Option<u16>, game_id: u64, role: &str, present: i64, required: i64) -> Holder {
        Holder { source_id, game_id, name: "g".into(), playlist_file_id: None, role: role.into(), present, required, rank: 0, manual: None, encrypted: false, covers: HashSet::new(), directory: None, folder: None }
    }

    fn disc(game_id: u64, directory: u64, covers: &[u64], required: i64) -> Holder {
        let mut h = holder(None, game_id, "local", covers.len() as i64, required);
        h.covers = covers.iter().copied().collect();
        h.directory = Some(directory);
        h.folder = Some(format!("/roms/{directory}"));
        h
    }

    /// A release whose two discs a scan left as two games (and a second copy of each in
    /// another folder): both discs are taken, the second from the first one's folder, and
    /// each disc once.
    #[test]
    fn every_disc_of_a_release_is_taken_once() {
        let picked = pick_holders_with(vec![disc(1, 10, &[101, 102], 4), disc(2, 20, &[101, 102], 4), disc(3, 20, &[103, 104], 4), disc(4, 10, &[103, 104], 4)], SourcePreference::LocalFirst, true);
        assert_eq!(picked.iter().map(|h| h.game_id).collect::<Vec<_>>(), vec![1, 4]);
    }

    /// Two destinations differing only in case are one file on FAT, NTFS or SMB.
    #[test]
    fn collision_keys_ignore_case() {
        assert_eq!(taken_key(Path::new("/dst/gb/Game.gb")), taken_key(Path::new("/dst/GB/GAME.GB")));
        assert_eq!(taken_key(Path::new("/dst/Ärger.gb")), taken_key(Path::new("/dst/ärger.gb")));
        assert_ne!(taken_key(Path::new("/dst/a.gb")), taken_key(Path::new("/dst/b.gb")));
    }

    /// Taking stops as soon as the copies taken hold as many discs as the release needs,
    /// even when another copy repeats none of them.
    #[test]
    fn taking_stops_once_every_disc_is_held() {
        let picked = pick_holders_with(vec![disc(1, 10, &[101, 102], 4), disc(2, 10, &[103, 104], 4), disc(3, 10, &[105], 4)], SourcePreference::LocalFirst, true);
        assert_eq!(picked.iter().map(|h| h.game_id).collect::<Vec<_>>(), vec![1, 2]);
    }

    /// A copy holding every disc is taken alone, and so is one that knows nothing of its
    /// contents (no other copy can be said to complete it).
    #[test]
    fn a_whole_copy_or_an_unknown_one_is_taken_alone() {
        let picked = pick_holders_with(vec![disc(1, 10, &[101, 102, 103, 104], 4), disc(2, 10, &[103, 104], 4)], SourcePreference::LocalFirst, true);
        assert_eq!(picked.len(), 1);
        let picked = pick_holders_with(vec![disc(1, 10, &[], 4), disc(2, 10, &[103, 104], 4)], SourcePreference::LocalFirst, true);
        assert_eq!(picked.iter().map(|h| h.game_id).collect::<Vec<_>>(), vec![1]);
        // Another copy that repeats disc 1 is not added for its disc 2.
        let picked = pick_holders_with(vec![disc(1, 10, &[101, 102], 4), disc(2, 10, &[101, 102, 103], 4)], SourcePreference::LocalFirst, true);
        assert_eq!(picked.iter().map(|h| h.game_id).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn a_complete_remote_copy_beats_an_incomplete_local_one() {
        let picked = pick_holder(vec![holder(None, 1, "local", 1, 3), holder(Some(2), 9, "main", 3, 3)], SourcePreference::LocalFirst).unwrap();
        assert_eq!(picked.source_id, Some(2));
    }

    #[test]
    fn local_wins_when_both_are_complete() {
        let picked = pick_holder(vec![holder(Some(2), 9, "main", 3, 3), holder(None, 1, "local", 3, 3)], SourcePreference::LocalFirst).unwrap();
        assert_eq!(picked.source_id, None);
    }

    #[test]
    fn main_first_prefers_the_server() {
        let picked = pick_holder(vec![holder(None, 1, "local", 3, 3), holder(Some(2), 9, "main", 3, 3)], SourcePreference::MainFirst).unwrap();
        assert_eq!(picked.source_id, Some(2));
    }

    #[test]
    fn main_beats_peer_and_the_choice_is_deterministic() {
        let a = pick_holder(vec![holder(Some(5), 9, "peer", 3, 3), holder(Some(2), 4, "main", 3, 3)], SourcePreference::MainFirst).unwrap();
        let b = pick_holder(vec![holder(Some(2), 4, "main", 3, 3), holder(Some(5), 9, "peer", 3, 3)], SourcePreference::MainFirst).unwrap();
        assert_eq!((a.source_id, a.game_id), (Some(2), 4));
        assert_eq!((b.source_id, b.game_id), (Some(2), 4));
    }

    #[test]
    fn among_complete_local_copies_the_release_order_decides() {
        let mut first = holder(None, 9, "local", 3, 3);
        first.rank = 0;
        let mut second = holder(None, 1, "local", 3, 3);
        second.rank = 1;
        let picked = pick_holder(vec![second, first], SourcePreference::LocalFirst).unwrap();
        assert_eq!(picked.game_id, 9);
    }

    #[test]
    fn source_only_ignores_everyone_else() {
        assert!(pick_holder(vec![holder(None, 1, "local", 3, 3)], SourcePreference::Source(2)).is_none());
        let picked = pick_holder(vec![holder(None, 1, "local", 3, 3), holder(Some(2), 9, "main", 1, 3)], SourcePreference::Source(2)).unwrap();
        assert_eq!(picked.source_id, Some(2));
    }

    /// With no `release_contents` rows there is nothing to be incomplete against, so the
    /// preference decides rather than a spurious 0-of-0 penalty.
    #[test]
    fn an_unknown_required_count_does_not_penalise_anyone() {
        let picked = pick_holder(vec![holder(Some(2), 9, "main", 0, 0), holder(None, 1, "local", 0, 0)], SourcePreference::LocalFirst).unwrap();
        assert_eq!(picked.source_id, None);
    }

    #[test]
    fn a_3ds_file_takes_its_titles_own_extension() {
        use cia_tool::testkit::{make_cia, make_ncch, setup, smdh};
        let dir = std::env::temp_dir().join(format!("romexplorer_plan_n3ds_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (mut rng, keys, _) = setup();
        let cia = |rng: &mut _, tid: u64| {
            let (ncch, _) = make_ncch(rng, &keys, tid, 0x00, None, &smdh("x"));
            make_cia(rng, &keys, tid, &[(0, ncch)], &smdh("x"))
        };
        let game = dir.join("game.cia");
        std::fs::write(&game, cia(&mut rng, 0x0004_0000_0012_3400)).unwrap();
        let update = dir.join("update.cia");
        std::fs::write(&update, cia(&mut rng, 0x0004_000E_0012_3400)).unwrap();

        assert_eq!(n3ds_destination(&game, "cia", false, OutputFormat::Zcci), Some("zcci"));
        assert_eq!(n3ds_destination(&game, "cia", false, OutputFormat::ThreeDs), Some("3ds"));
        assert_eq!(n3ds_destination(&update, "cia", false, OutputFormat::Cci), Some("cia"), "an update stays a CIA, decrypted");
        assert_eq!(n3ds_destination(&update, "cia", false, OutputFormat::Zcxi), Some("zcia"));
        assert_eq!(n3ds_destination(&game, "cia", true, OutputFormat::Cci), None, "a zip member is copied as it is");
        assert_eq!(n3ds_destination(&game, "pdf", false, OutputFormat::Cci), None, "a manual beside it too");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retarget_swaps_the_extension() {
        assert_eq!(retarget(std::path::Path::new("/a/b/game.iso"), OutputFormat::Chd), PathBuf::from("/a/b/game.chd"));
        assert_eq!(retarget(std::path::Path::new("/a/b/game.iso"), OutputFormat::Same), PathBuf::from("/a/b/game.iso"));
    }

    #[test]
    fn a_bundles_tree_is_measured_on_the_owners_own_paths() {
        // Both sides are the SOURCE machine's paths, which for a remote item exist nowhere
        // on this one -- so this must never touch the filesystem.
        assert_eq!(bundle_relative("/srv/roms/ps3/Game", "/srv/roms/ps3/Game/PS3_GAME/USRDIR/EBOOT.BIN"), Some(vec!["PS3_GAME".to_string(), "USRDIR".to_string(), "EBOOT.BIN".to_string()]));
        assert_eq!(bundle_relative("D:\\Roms\\Game", "D:\\Roms\\Game\\sub\\a.bin"), Some(vec!["sub".to_string(), "a.bin".to_string()]));
        assert_eq!(bundle_relative("/srv/roms/ps3/Game", "/srv/roms/ps3/Other/x.bin"), None);
        assert_eq!(bundle_relative("/srv/roms/ps3/Game", "/srv/roms/ps3/Game"), None);
        // A cue and its bin sit beside each other, not under the cue.
        assert_eq!(bundle_relative("/srv/roms/ps1/Game.cue", "/srv/roms/ps1/Game.bin"), None);
    }

    #[test]
    fn a_flat_file_lands_under_its_platform_folder() {
        let item = CopyItem { rom: None, platform: Platform::Snes, game_name: "g".into(), release_id: None, main_path: "/srv/roms/Game.sfc".into(), files: Vec::new(), playlist: None, game: None };
        let file = SourceFile { owner: Some(1), file_id: 1, path: PathBuf::from("/mnt/roms/Game.sfc"), raw_path: "/srv/roms/Game.sfc".into(), content_id: 1, size: 1, archive_format: None, member: None };
        assert_eq!(destination_for(std::path::Path::new("/out"), "snes", &item, &file, None), PathBuf::from("/out/snes/Game.sfc"));
    }

    #[test]
    fn a_directory_bundle_keeps_its_shape_under_the_platform_folder() {
        let file = SourceFile { owner: Some(1), file_id: 1, path: PathBuf::from("/mnt/roms/Game/PS3_GAME/EBOOT.BIN"), raw_path: "/srv/roms/Game/PS3_GAME/EBOOT.BIN".into(), content_id: 1, size: 1, archive_format: None, member: None };
        let item = CopyItem { rom: None, platform: Platform::PlayStation3, game_name: "g".into(), release_id: None, main_path: "/srv/roms/Game".into(), files: vec![file.clone()], playlist: None, game: None };
        assert_eq!(destination_for(std::path::Path::new("/out"), "ps3", &item, &file, None), PathBuf::from("/out/ps3/Game/PS3_GAME/EBOOT.BIN"));
    }

    #[test]
    fn a_game_in_a_folder_of_its_own_keeps_it() {
        let never = |_: &[String]| false;
        let top = |files: &[&str], names: &[&str]| game_folder(files, names, &never).map(|s| s.last().cloned().unwrap_or_default());
        // DSi / Wii from the CDN: generic file names, the folder is the game.
        assert_eq!(top(&["/srv/Nintendo - Nintendo DSi -Digital-/Hoge Quest (Europe)/00000001", "/srv/Nintendo - Nintendo DSi -Digital-/Hoge Quest (Europe)/tmd.0"], &["", "00000001"]), Some("Hoge Quest (Europe)".into()));
        // Named otherwise than the release, still above generic names with a tag: kept.
        assert_eq!(top(&["/srv/DSi/Fuga - Hoge Quest (Europe)/00000001"], &["Hoge Quest - Fuga (Europe)", ""]), Some("Fuga - Hoge Quest (Europe)".into()));
        // PSP DLC: a title-id folder under the game's.
        // (The kept folder is the topmost one; the title-id folder stays under it.)
        assert_eq!(top(&["/srv/Sony - PSP-DLC-/Hoge Quest (Japan) (DLC)/NPJH60015/movie.mp4.edat", "/srv/Sony - PSP-DLC-/Hoge Quest (Japan) (DLC)/NPJH60015/content.dat.edat"], &["Hoge Quest (Japan) (DLC)", ""]), Some("Hoge Quest (Japan) (DLC)".into()));
        // Named like nothing of the game, but it holds only the game's files: kept.
        let only = |f: &[String]| f.last().is_some_and(|n| n == "Hoge Quest (Japan) [Extras]");
        assert_eq!(game_folder(&["/srv/PC-98/Hoge Quest (Japan) [Extras]/A.d88", "/srv/PC-98/Hoge Quest (Japan) [Extras]/B.d88"], &["Hoge Quest (Japan)"], &only).and_then(|s| s.last().cloned()), Some("Hoge Quest (Japan) [Extras]".into()));
        assert_eq!(top(&["/srv/PC-98/Hoge Quest (Japan) [Extras]/A.d88", "/srv/PC-98/Hoge Quest (Japan) [Extras]/B.d88"], &["Hoge Quest (Japan)"]), None);
        // A computer game in a folder per disk, named like its file.
        assert_eq!(top(&["/srv/NEC - PC-98/Hoge Quest (Japan) (Disk 1)/Hoge Quest (Japan) (Disk 1).dsk"], &[]), Some("Hoge Quest (Japan) (Disk 1)".into()));
        // A self-contained file goes flat, even in a folder named like it.
        assert_eq!(top(&["/srv/DSi/Hoge Quest (Japan)/Hoge Quest (Japan).nds"], &["Hoge Quest (Japan)"]), None);
        // A collection folder is never the game's.
        assert_eq!(top(&["/srv/Fujitsu - FM Towns/Hoge Quest (Japan) (Disk 1).scp"], &["Hoge Quest (Japan)"]), None);
        assert_eq!(top(&["/srv/Microsoft - MSX (TOSEC) (DSK)/Hoge Quest (1987)(Hoge Soft).dsk"], &["Hoge Quest"]), None);
        let file = SourceFile { owner: Some(1), file_id: 1, path: PathBuf::from("/mnt/DSi/Hoge Quest (Europe)/00000001"), raw_path: "/srv/DSi/Hoge Quest (Europe)/00000001".into(), content_id: 1, size: 1, archive_format: None, member: None };
        let item = CopyItem { rom: None, platform: Platform::NintendoDsi, game_name: "00000001".into(), release_id: None, main_path: "/srv/DSi/Hoge Quest (Europe)/00000001".into(), files: vec![file.clone()], playlist: None, game: None };
        let keep = game_folder(&[file.raw_path.as_str()], &["Hoge Quest (Europe)"], &never);
        assert_eq!(destination_for(std::path::Path::new("/out"), "dsi", &item, &file, keep.as_deref()), PathBuf::from("/out/dsi/Hoge Quest (Europe)/00000001"));
    }

    fn src(raw: &str) -> SourceFile {
        SourceFile { owner: None, file_id: 1, path: PathBuf::from(raw), raw_path: raw.into(), content_id: 1, size: 1, archive_format: None, member: None }
    }

    #[test]
    fn a_disc_conversion_has_one_carrier_and_the_rest_ride_inside() {
        let files = [src("/roms/Game.cue"), src("/roms/Game (Track 1).bin"), src("/roms/Game (Track 2).bin")];
        // The cue is what the archiver is handed, so it is what names the single .chd.
        assert_eq!(disc_carrier(OutputFormat::Chd, &files, "/roms/Game.cue"), Some(0));
        assert_eq!(disc_carrier(OutputFormat::Rvz, &files, "/roms/Game.cue"), Some(0));
    }

    /// A zip of a sheet holds the sheet and its tracks, so the cue carries them; files that
    /// are not a sheet's stay one archive each.
    #[test]
    fn a_file_archive_collapses_a_sheet_and_nothing_else() {
        let files = [src("/roms/Game.cue"), src("/roms/Game.bin")];
        assert_eq!(disc_carrier(OutputFormat::Zip, &files, "/roms/Game.cue"), Some(0));
        assert_eq!(disc_carrier(OutputFormat::SevenZip, &files, "/roms/Game.cue"), Some(0));
        assert_eq!(disc_carrier(OutputFormat::Same, &files, "/roms/Game.cue"), None);
        assert_eq!(disc_carrier(OutputFormat::Raw, &files, "/roms/Game.cue"), None);
        let loose = [src("/roms/Game.nes"), src("/roms/Game.txt")];
        assert_eq!(disc_carrier(OutputFormat::Zip, &loose, "/roms/Game.nes"), None);
    }

    /// A directory bundle's `main_path` is the directory, which is not one of its files.
    #[test]
    fn a_main_path_that_is_not_a_file_falls_back_to_the_first() {
        let files = [src("/roms/Game/a.bin"), src("/roms/Game/b.bin")];
        assert_eq!(disc_carrier(OutputFormat::Chd, &files, "/roms/Game"), Some(0));
        assert_eq!(disc_carrier(OutputFormat::Chd, &[], "/roms/Game"), None);
    }

    #[test]
    fn only_whole_disc_images_are_disc_containers() {
        for f in [OutputFormat::Chd, OutputFormat::Rvz, OutputFormat::Gcz, OutputFormat::Wia] {
            assert!(is_disc_container(f), "{f:?}");
        }
        for f in [OutputFormat::Same, OutputFormat::Raw, OutputFormat::Zip, OutputFormat::SevenZip] {
            assert!(!is_disc_container(f), "{f:?}");
        }
    }
}
