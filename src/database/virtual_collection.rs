//! A synthetic *owned* collection built from catalog data alone: rows in `local_files`/
//! `local_rom`/`local_game` (+ `local_rom_files`/`local_game_roms`/`local_release_games`,
//! `content_contents` for archives, `local_game.playlist_file_id` for m3u sets) pointing at
//! releases the DATs already describe,
//! with made-up paths under a clearly fake root. Nothing on disk is read or written.
//!
//! Purpose: give the GUI/REST sorting-and-filtering work realistic shapes to develop
//! against before the scanner exists -- single-ROM cartridges (some zipped, with the
//! archive's own `content` row and `content_contents` child), cue/bin and gdi track sets,
//! plain `.iso`, `.chd` and `.rvz` containers, multi-disc sets (one `rom` per disc, most
//! with an `.m3u`), and a couple of duplicate copies of a multi-disc set with one disc
//! missing (separate `game` rows, same `content`, different `files` paths -- exactly the
//! "never merged in storage" rule from docs/plan/database.md).
//!
//! Deterministic for a given `seed` and catalog state (a tiny xorshift, no `rand` dep).

use std::collections::{BTreeMap, HashMap, HashSet};

use lazy_static::lazy_static;
use regex::Regex;
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use crate::common::models::Platform;

use super::error::DatabaseError;
use super::import::sha1_hex;

#[derive(Debug, Clone)]
pub struct VirtualCollectionOptions {
    /// Target number of `game` rows (before duplicates).
    pub games: usize,
    /// How many platforms to spread them over.
    pub platforms: usize,
    pub seed: u64,
    /// Fake filesystem root every path starts with.
    pub root: String,
    /// Multi-disc sets to duplicate with one disc missing.
    pub duplicates_missing_disc: usize,
    /// Fraction of single-file cartridge games stored as a `.zip` (own content row +
    /// `content_contents` child).
    pub zipped_fraction: f32,
    /// Fraction of multi-disc sets that get an `.m3u` playlist file.
    pub playlist_fraction: f32,
    /// Fraction of picks on disc platforms that should be multi-disc sets (when available).
    pub multi_disc_fraction: f32,
}

impl Default for VirtualCollectionOptions {
    fn default() -> Self {
        Self { games: 300, platforms: 10, seed: 20260923, root: "/virtual/romexplorer".to_string(), duplicates_missing_disc: 2, zipped_fraction: 0.35, playlist_fraction: 0.7, multi_disc_fraction: 0.35 }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PlatformSummary {
    pub platform: Platform,
    pub games: usize,
    pub multi_disc: usize,
    /// format tag -> file count (cue, bin, gdi, iso, chd, rvz, zip, m3u, rom)
    pub formats: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Default)]
pub struct VirtualCollectionSummary {
    pub per_platform: Vec<PlatformSummary>,
    pub games: usize,
    pub roms: usize,
    pub files: usize,
    pub multi_disc_games: usize,
    pub duplicates: Vec<String>,
    pub zipped: usize,
    pub playlists: usize,
}

/// Preferred platform order: disc systems first (formats), then cartridges.
const PREFERRED: &[Platform] = &[
    Platform::PlayStation,
    Platform::PlayStation2,
    Platform::GameCube,
    Platform::Dreamcast,
    Platform::SegaSaturn,
    Platform::Wii,
    Platform::GameBoyAdvance,
    Platform::Nes,
    Platform::Snes,
    Platform::SegaGenesis,
    Platform::Nintendo64,
    Platform::GameBoy,
    Platform::GameBoyColor,
    Platform::NintendoDs,
    Platform::SegaMasterSystem,
    Platform::PlayStationPortable,
    Platform::TurboGrafx16,
    Platform::Sega32x,
];

struct XorShift(u64);

impl XorShift {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
    fn chance(&mut self, p: f32) -> bool {
        (self.next() % 10_000) as f32 / 10_000.0 < p
    }
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.below(i + 1);
            v.swap(i, j);
        }
    }
}

#[derive(Debug, Clone)]
struct CatalogFile {
    content_id: u64,
    file_name: String,
    size: Option<u64>,
}

#[derive(Debug, Clone)]
struct CatalogRelease {
    release_id: u64,
    name: String,
    region: Option<String>,
    category: Option<String>,
    source: String,
    files: Vec<CatalogFile>,
}

lazy_static! {
    static ref RE_DISC_IN_FILE: Regex = Regex::new(r"(?i)\((?:Disc|Disk|CD)\s+(\d+)").unwrap();
}

fn ext_of(name: &str) -> String {
    name.rsplit('.').next().map(|e| e.to_ascii_lowercase()).unwrap_or_default()
}

/// Which file leads a disc/rom group (what `rom.main_path` points at).
fn main_rank(ext: &str) -> u8 {
    match ext {
        "cue" | "gdi" | "ccd" | "toc" => 0,
        "chd" | "rvz" | "iso" | "wia" | "gcz" | "cso" | "pbp" => 1,
        "bin" | "img" | "raw" | "track" => 3,
        _ => 2,
    }
}

fn archive_format_tag(ext: &str) -> Option<&'static str> {
    match ext {
        "chd" => Some("chd"),
        "rvz" => Some("rvz"),
        "iso" => Some("raw_iso"),
        "zip" => Some("zip"),
        "7z" => Some("7z"),
        "cso" => Some("cso"),
        "pbp" => Some("pbp"),
        _ => None,
    }
}

fn safe_path_component(name: &str) -> String {
    name.replace(['/', '\\', ':'], "-")
}

async fn platform_candidates(pool: &MySqlPool) -> Result<HashMap<Platform, u64>, DatabaseError> {
    let rows = sqlx::query("SELECT app_platform_id, COUNT(*) AS n FROM `release` WHERE deleted = FALSE AND app_platform_id <> 0 AND category NOT IN ('BIOS','Firmware') OR (deleted = FALSE AND app_platform_id <> 0 AND category IS NULL) GROUP BY app_platform_id").fetch_all(pool).await?;
    let mut out = HashMap::new();
    for row in rows {
        let id: u16 = row.try_get("app_platform_id")?;
        let n: i64 = row.try_get("n")?;
        if let Some(p) = Platform::from_igdb_id(id) {
            out.insert(p, n as u64);
        }
    }
    Ok(out)
}

async fn releases_for(pool: &MySqlPool, platform: Platform, limit: u32) -> Result<Vec<CatalogRelease>, DatabaseError> {
    let rows = sqlx::query(
        "SELECT r.id, r.name, r.region, r.category, s.name AS source, rc.content_id, rc.file_name, c.size \
         FROM `release` r JOIN release_contents rc ON rc.release_id = r.id JOIN content c ON c.id = rc.content_id LEFT JOIN sources s ON s.id = rc.source_id \
         WHERE r.app_platform_id = ? AND r.deleted = FALSE \
           AND rc.source_id <=> (SELECT MIN(rc2.source_id) FROM release_contents rc2 WHERE rc2.release_id = r.id) AND r.is_demo = FALSE AND r.is_beta = FALSE AND r.is_prototype = FALSE AND r.is_unlicensed = FALSE \
           AND COALESCE(r.category, '') NOT IN ('BIOS','Firmware','Applications','Multimedia','Educational','Coverdiscs','Demos') \
         ORDER BY r.id, rc.file_name LIMIT ?",
    )
    .bind(platform.igdb_id())
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let mut by_id: BTreeMap<u64, CatalogRelease> = BTreeMap::new();
    for row in rows {
        let id: u64 = row.try_get("id")?;
        let entry = by_id.entry(id).or_insert_with(|| CatalogRelease {
            release_id: id,
            name: row.try_get("name").unwrap_or_default(),
            region: row.try_get("region").ok().flatten(),
            category: row.try_get("category").ok().flatten(),
            source: row.try_get::<Option<String>, _>("source").ok().flatten().unwrap_or_default(),
            files: Vec::new(),
        });
        entry.files.push(CatalogFile { content_id: row.try_get("content_id")?, file_name: row.try_get("file_name")?, size: row.try_get("size").ok().flatten() });
    }
    Ok(by_id.into_values().collect())
}

fn disc_groups(release: &CatalogRelease) -> Vec<(Option<u16>, Vec<&CatalogFile>)> {
    let mut groups: BTreeMap<Option<u16>, Vec<&CatalogFile>> = BTreeMap::new();
    for f in &release.files {
        let disc = RE_DISC_IN_FILE.captures(&f.file_name).and_then(|c| c[1].parse::<u16>().ok());
        groups.entry(disc).or_default().push(f);
    }
    // A set where some files carry a disc tag and some don't (shared extras) -- treat the
    // untagged ones as part of disc 1.
    if groups.len() > 1 && groups.contains_key(&None) {
        let extras = groups.remove(&None).unwrap_or_default();
        if let Some(first) = groups.values_mut().next() {
            first.extend(extras);
        }
    }
    groups.into_iter().collect()
}

fn is_multi_disc(release: &CatalogRelease) -> bool {
    disc_groups(release).len() > 1
}

struct Writer<'a> {
    pool: &'a MySqlPool,
    root: String,
    rng: XorShift,
    options: VirtualCollectionOptions,
    summary: VirtualCollectionSummary,
}

impl Writer<'_> {
    async fn insert_content_stub(&mut self, platform: Platform, file_name: &str, size: u64) -> Result<u64, DatabaseError> {
        let result = sqlx::query("INSERT INTO content (app_platform_id, file_name, size, partial) VALUES (?, ?, ?, FALSE)").bind(platform.igdb_id()).bind(file_name).bind(size).execute(self.pool).await?;
        Ok(result.last_insert_id())
    }

    async fn insert_file(&mut self, path: &str, content_id: u64, archive_format: Option<&str>, size: u64) -> Result<u64, DatabaseError> {
        let result = sqlx::query("INSERT INTO local_files (path, path_hash, content_id, archive_format, size, last_modified, last_full_check) VALUES (?, ?, ?, ?, ?, NOW(), NOW())")
            .bind(path)
            .bind(sha1_hex(path))
            .bind(content_id)
            .bind(archive_format)
            .bind(size)
            .execute(self.pool)
            .await?;
        self.summary.files += 1;
        Ok(result.last_insert_id())
    }

    async fn insert_rom(&mut self, platform: Platform, disc_number: Option<u16>, main_path: &str, file_ids: &[u64]) -> Result<u64, DatabaseError> {
        let result = sqlx::query("INSERT INTO local_rom (app_platform_id, disc_number, main_path) VALUES (?, ?, ?)").bind(platform.igdb_id()).bind(disc_number).bind(main_path).execute(self.pool).await?;
        let rom_id = result.last_insert_id();
        for file_id in file_ids {
            sqlx::query("INSERT INTO local_rom_files (rom_id, file_id) VALUES (?, ?)").bind(rom_id).bind(file_id).execute(self.pool).await?;
        }
        self.summary.roms += 1;
        Ok(rom_id)
    }

    async fn insert_game(&mut self, release: &CatalogRelease, rom_ids: &[u64], playlist_file_id: Option<u64>) -> Result<u64, DatabaseError> {
        let result = sqlx::query("INSERT INTO local_game (name, playlist_file_id, region, category) VALUES (?, ?, ?, ?)").bind(&release.name).bind(playlist_file_id).bind(&release.region).bind(&release.category).execute(self.pool).await?;
        let game_id = result.last_insert_id();
        for rom_id in rom_ids {
            sqlx::query("INSERT INTO local_game_roms (game_id, rom_id) VALUES (?, ?)").bind(game_id).bind(rom_id).execute(self.pool).await?;
        }
        sqlx::query("INSERT IGNORE INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(release.release_id).bind(game_id).execute(self.pool).await?;
        self.summary.games += 1;
        Ok(game_id)
    }

    /// Writes one owned copy of `release` (all discs, or all but `skip_disc`) under
    /// `dir`. Returns the game id.
    async fn write_game(&mut self, platform: Platform, release: &CatalogRelease, dir: &str, skip_disc: Option<u16>, formats: &mut BTreeMap<String, usize>) -> Result<u64, DatabaseError> {
        let groups = disc_groups(release);
        let multi = groups.len() > 1;
        let mut rom_ids = Vec::new();
        let mut disc_main_paths = Vec::new();
        for (disc, files) in &groups {
            if skip_disc.is_some() && *disc == skip_disc {
                continue;
            }
            // Cartridge single file: maybe zipped.
            let single_plain = !multi && files.len() == 1 && archive_format_tag(&ext_of(&files[0].file_name)).is_none() && !matches!(ext_of(&files[0].file_name).as_str(), "cue" | "gdi" | "bin");
            if single_plain && self.rng.chance(self.options.zipped_fraction) {
                let inner = files[0];
                let inner_size = inner.size.unwrap_or(0);
                let zip_name = format!("{}.zip", release.name);
                let zip_size = (inner_size as f64 * 0.62) as u64 + 1024;
                let zip_content = self.insert_content_stub(platform, &zip_name, zip_size).await?;
                sqlx::query("INSERT IGNORE INTO content_contents (parent_content_id, child_content_id) VALUES (?, ?)").bind(zip_content).bind(inner.content_id).execute(self.pool).await?;
                let path = format!("{dir}/{}", safe_path_component(&zip_name));
                let file_id = self.insert_file(&path, zip_content, Some("zip"), zip_size).await?;
                let rom_id = self.insert_rom(platform, None, &path, &[file_id]).await?;
                rom_ids.push(rom_id);
                *formats.entry("zip".to_string()).or_default() += 1;
                self.summary.zipped += 1;
                continue;
            }
            let mut sorted: Vec<&CatalogFile> = files.clone();
            sorted.sort_by_key(|f| (main_rank(&ext_of(&f.file_name)), f.file_name.clone()));
            let sub_dir = if multi { format!("{dir}/Disc {}", disc.unwrap_or(1)) } else { dir.to_string() };
            let mut file_ids = Vec::new();
            let mut main_path = String::new();
            for f in &sorted {
                let ext = ext_of(&f.file_name);
                let path = format!("{sub_dir}/{}", safe_path_component(&f.file_name));
                let size = f.size.unwrap_or(0);
                let file_id = self.insert_file(&path, f.content_id, archive_format_tag(&ext), size).await?;
                file_ids.push(file_id);
                if main_path.is_empty() {
                    main_path = path.clone();
                }
                let tag = if ext.is_empty() { "rom".to_string() } else { ext };
                *formats.entry(tag).or_default() += 1;
            }
            let rom_id = self.insert_rom(platform, if multi { *disc } else { None }, &main_path, &file_ids).await?;
            rom_ids.push(rom_id);
            disc_main_paths.push(main_path);
        }
        let mut playlist_file_id = None;
        if multi && rom_ids.len() > 1 && self.rng.chance(self.options.playlist_fraction) {
            let m3u_name = format!("{}.m3u", release.name);
            let body: String = disc_main_paths.iter().map(|p| p.rsplit('/').next().unwrap_or(p).to_string()).collect::<Vec<_>>().join("\n");
            let m3u_content = self.insert_content_stub(platform, &m3u_name, body.len() as u64).await?;
            let path = format!("{dir}/{}", safe_path_component(&m3u_name));
            playlist_file_id = Some(self.insert_file(&path, m3u_content, None, body.len() as u64).await?);
            *formats.entry("m3u".to_string()).or_default() += 1;
            self.summary.playlists += 1;
        }
        if multi && skip_disc.is_none() {
            self.summary.multi_disc_games += 1;
        }
        self.insert_game(release, &rom_ids, playlist_file_id).await
    }
}

/// Generates the collection. Refuses to run on a database that already has `game` rows
/// unless `replace` (which wipes the ownership tables first -- catalog tables untouched).
pub async fn generate(pool: &MySqlPool, options: &VirtualCollectionOptions, replace: bool) -> Result<VirtualCollectionSummary, DatabaseError> {
    let existing: i64 = sqlx::query("SELECT COUNT(*) AS n FROM local_game").fetch_one(pool).await?.try_get("n")?;
    if existing > 0 {
        if !replace {
            return Err(DatabaseError::Config(format!("{existing} game rows already exist; pass replace to wipe the ownership tables first")));
        }
        clear_ownership(pool).await?;
    }

    let available = platform_candidates(pool).await?;
    let mut platforms: Vec<Platform> = PREFERRED.iter().copied().filter(|p| available.get(p).copied().unwrap_or(0) >= 10).collect();
    let mut rest: Vec<(Platform, u64)> = available.iter().filter(|(p, _)| !platforms.contains(p)).map(|(p, n)| (*p, *n)).collect();
    rest.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.igdb_id().cmp(&b.0.igdb_id())));
    platforms.extend(rest.into_iter().map(|(p, _)| p));
    platforms.truncate(options.platforms.max(1));
    if platforms.is_empty() {
        return Err(DatabaseError::Config("no platform in the catalog has enough releases; import some DATs first".to_string()));
    }

    let mut writer = Writer { pool, root: options.root.trim_end_matches('/').to_string(), rng: XorShift::new(options.seed), options: options.clone(), summary: VirtualCollectionSummary::default() };
    let quota = (options.games / platforms.len()).max(1);
    let mut multi_disc_pool: Vec<(Platform, CatalogRelease)> = Vec::new();
    let mut used: HashSet<u64> = HashSet::new();

    for platform in &platforms {
        let candidates = releases_for(pool, *platform, 60_000).await?;
        let (mut multi, mut single): (Vec<CatalogRelease>, Vec<CatalogRelease>) = candidates.into_iter().partition(is_multi_disc);
        writer.rng.shuffle(&mut multi);
        writer.rng.shuffle(&mut single);
        // Mix sources for format variety (Redump cue/bin vs mame-redump chd/rvz): interleave
        // by source within each bucket.
        let interleave = |v: Vec<CatalogRelease>| -> Vec<CatalogRelease> {
            let mut by_source: BTreeMap<String, Vec<CatalogRelease>> = BTreeMap::new();
            for r in v {
                by_source.entry(r.source.clone()).or_default().push(r);
            }
            let mut out = Vec::new();
            let mut queues: Vec<Vec<CatalogRelease>> = by_source.into_values().map(|mut q| {
                q.reverse();
                q
            }).collect();
            loop {
                let mut any = false;
                for q in queues.iter_mut() {
                    if let Some(r) = q.pop() {
                        out.push(r);
                        any = true;
                    }
                }
                if !any {
                    break;
                }
            }
            out
        };
        let multi = interleave(multi);
        let single = interleave(single);
        let want_multi = ((quota as f32) * options.multi_disc_fraction).round() as usize;
        let mut picks: Vec<CatalogRelease> = multi.iter().take(want_multi.min(multi.len())).cloned().collect();
        let mut single_iter = single.into_iter();
        while picks.len() < quota {
            match single_iter.next() {
                Some(r) => picks.push(r),
                None => break,
            }
        }
        // Not enough singles: top up with more multi-disc sets.
        if picks.len() < quota {
            for r in multi.iter().skip(want_multi) {
                if picks.len() >= quota {
                    break;
                }
                picks.push(r.clone());
            }
        }

        let mut ps = PlatformSummary { platform: *platform, ..Default::default() };
        let platform_dir = format!("{}/{}", writer.root, safe_path_component(platform.display_name()));
        for release in &picks {
            if !used.insert(release.release_id) {
                continue;
            }
            let dir = format!("{platform_dir}/{}", safe_path_component(&release.name));
            let mut formats = BTreeMap::new();
            writer.write_game(*platform, release, &dir, None, &mut formats).await?;
            ps.games += 1;
            if is_multi_disc(release) {
                ps.multi_disc += 1;
                multi_disc_pool.push((*platform, release.clone()));
            }
            for (k, v) in formats {
                *ps.formats.entry(k).or_default() += v;
            }
        }
        writer.summary.per_platform.push(ps);
    }

    // Duplicate copies of multi-disc sets with one disc missing.
    writer.rng.shuffle(&mut multi_disc_pool);
    for (platform, release) in multi_disc_pool.into_iter().take(options.duplicates_missing_disc) {
        let groups = disc_groups(&release);
        let Some((skip, _)) = groups.last() else { continue };
        let dir = format!("{}/duplicates/{}/{}", writer.root, safe_path_component(platform.display_name()), safe_path_component(&release.name));
        let mut formats = BTreeMap::new();
        writer.write_game(platform, &release, &dir, *skip, &mut formats).await?;
        writer.summary.duplicates.push(format!("{} [{platform:?}] -- copy missing disc {}", release.name, skip.unwrap_or(0)));
    }

    Ok(writer.summary)
}

/// Deletes every ownership-side row (files/rom/game and their junctions, plus the stub
/// content rows the generator created for archives/playlists). Catalog tables untouched.
pub async fn clear_ownership(pool: &MySqlPool) -> Result<(), DatabaseError> {
    let mut tx = pool.begin().await?;
    for sql in [
        "UPDATE local_game SET cover_media_id = NULL, playlist_file_id = NULL",
        "DELETE FROM local_release_games",
        "DELETE FROM local_game_roms",
        "DELETE FROM local_game",
        "DELETE FROM local_rom_rahash",
        "DELETE FROM local_rom_files",
        "DELETE FROM local_rom",
        "DELETE FROM local_files",
        // Stub content rows (archive containers / m3u) are the ones with no hashes and no
        // catalog membership.
        "DELETE cc FROM content_contents cc JOIN content c ON c.id = cc.parent_content_id WHERE NOT EXISTS (SELECT 1 FROM hashes h WHERE h.content_id = c.id) AND NOT EXISTS (SELECT 1 FROM release_contents rc WHERE rc.content_id = c.id)",
        "DELETE FROM content WHERE NOT EXISTS (SELECT 1 FROM hashes h WHERE h.content_id = content.id) AND NOT EXISTS (SELECT 1 FROM release_contents rc WHERE rc.content_id = content.id) AND NOT EXISTS (SELECT 1 FROM content_contents cc WHERE cc.child_content_id = content.id)",
    ] {
        sqlx::query(sql).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xorshift_is_deterministic_and_bounded() {
        let mut a = XorShift::new(7);
        let mut b = XorShift::new(7);
        for _ in 0..100 {
            let x = a.below(10);
            assert_eq!(x, b.below(10));
            assert!(x < 10);
        }
    }

    #[test]
    fn disc_grouping() {
        let release = CatalogRelease {
            release_id: 1,
            name: "Example (USA)".to_string(),
            region: None,
            category: None,
            source: "redump".to_string(),
            files: vec![
                CatalogFile { content_id: 1, file_name: "Example (USA) (Disc 1).cue".to_string(), size: Some(1) },
                CatalogFile { content_id: 2, file_name: "Example (USA) (Disc 1) (Track 1).bin".to_string(), size: Some(1) },
                CatalogFile { content_id: 3, file_name: "Example (USA) (Disc 2).cue".to_string(), size: Some(1) },
                CatalogFile { content_id: 4, file_name: "Example (USA) (Disc 2) (Track 1).bin".to_string(), size: Some(1) },
            ],
        };
        let groups = disc_groups(&release);
        assert_eq!(groups.len(), 2);
        assert!(is_multi_disc(&release));
        assert_eq!(main_rank("cue"), 0);
        assert!(main_rank("bin") > main_rank("chd"));
        assert_eq!(archive_format_tag("rvz"), Some("rvz"));
    }
}
