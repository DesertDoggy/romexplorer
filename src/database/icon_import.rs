//! Platform icons from RomM (`submodules/romm`, upstream rommapp/romm, AGPL-3.0) for the
//! console's platform badges (`romexplorer_dbtool import-icons`).
//!
//! The console shows `app_root_ex/assets/icons/<slug of the platform name>.<ext>` in place
//! of its built-in silhouettes (`server::icon_files`, `platformGlyph` in the console). RomM
//! names its icons by IGDB platform slug (`dc.svg`, `ps2.svg`); our platforms carry their
//! IGDB id, so one IGDB `platforms` request maps id -> slug. What it answers is recorded on
//! `platforms.romm_slug`, falling back to the compiled-in seed in
//! [`crate::common::platform_names`] -- one table, keyed by platform id rather than by a
//! display name that can change, and the same value a copy's `slug` folder scheme uses.
//!
//! SVG only: RomM's uniform `systematic/` set first (derived from libretro's Systematic
//! theme, CC BY 4.0), then its main set's `*.svg`. A platform with neither keeps its
//! built-in silhouette. Existing files are kept unless `replace` (the user's own icons win).
//!
//! The RomM checkout is only ever read -- nothing is written inside it, not even temp
//! files. Output: the icons, `ATTRIBUTIONS.md` next to them, and RomM's license under
//! `app_root_ex/lib/LICENSES/romm.LICENSE`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;
use super::providers::ProviderClients;
use crate::common::models::Platform;

/// The console's slug of a platform name (`slug()` in console.html).
pub fn console_slug(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// The commit a git checkout (or submodule) is on, from its metadata files only.
pub fn checkout_commit(repo: &Path) -> Option<String> {
    let dot_git = repo.join(".git");
    let git_dir = if dot_git.is_file() {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        repo.join(text.trim().strip_prefix("gitdir:")?.trim())
    } else {
        dot_git
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(reference) = head.strip_prefix("ref:").map(str::trim) else { return Some(head.to_string()) };
    if let Ok(hash) = std::fs::read_to_string(git_dir.join(reference)) {
        return Some(hash.trim().to_string());
    }
    let packed = std::fs::read_to_string(git_dir.join("packed-refs")).ok()?;
    packed.lines().find_map(|l| l.strip_suffix(reference).map(|h| h.trim().to_string()))
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct IconImportSummary {
    pub copied: Vec<(String, String)>,
    pub kept_existing: Vec<String>,
    pub unmapped: Vec<String>,
    pub commit: String,
}

/// Copies RomM icons for every platform in `platforms` into `icons_dir`.
pub async fn import_romm_icons(pool: &MySqlPool, clients: &ProviderClients, romm_repo: &Path, icons_dir: &Path, licenses_dir: &Path, replace: bool) -> Result<IconImportSummary, DatabaseError> {
    let platforms_dir = romm_repo.join("frontend").join("assets").join("platforms");
    if !platforms_dir.is_dir() {
        return Err(DatabaseError::Config(format!("no RomM platform icons at {}", platforms_dir.display())));
    }
    let rows = sqlx::query("SELECT id, igdb_id, name FROM platforms ORDER BY name").fetch_all(pool).await?;
    let ours: Vec<(u16, Option<u16>, String)> = rows.iter().map(|r| Ok((r.try_get("id")?, r.try_get("igdb_id")?, r.try_get("name")?))).collect::<Result<_, sqlx::Error>>()?;
    // IGDB id -> slug, one request per 100 ids.
    let mut slugs: HashMap<u16, String> = HashMap::new();
    if let Some(igdb) = clients.igdb.as_ref() {
        let ids: Vec<i32> = ours.iter().filter_map(|(_, igdb, _)| igdb.map(i32::from)).collect();
        for chunk in ids.chunks(100) {
            for p in igdb.get_raw_by_ids("platforms", chunk.to_vec(), "slug").await? {
                if let (Some(id), Some(slug)) = (p.get("id").and_then(|v| v.as_u64()), p.get("slug").and_then(|v| v.as_str())) {
                    slugs.insert(id as u16, slug.to_string());
                }
            }
        }
    } else {
        log::warn!("import-icons: IGDB unavailable, only the override table maps platforms");
    }
    let find = |name: &str| -> Option<PathBuf> {
        [platforms_dir.join("systematic").join(format!("{name}.svg")), platforms_dir.join(format!("{name}.svg"))].into_iter().find(|p| p.is_file())
    };
    std::fs::create_dir_all(icons_dir)?;
    let mut s = IconImportSummary { commit: checkout_commit(romm_repo).unwrap_or_else(|| "unknown".to_string()), ..Default::default() };
    let mut attributed: Vec<(String, String)> = Vec::new();
    for (app_id, igdb_id, name) in &ours {
        // What IGDB says now, else the compiled-in seed next to the `Platform` enum. The
        // live answer is recorded on the row, because it is also one of the four destination
        // naming schemes a copy can use (`api::naming`) and that must work offline.
        let live = igdb_id.and_then(|id| slugs.get(&id).cloned());
        let seed = Platform::from_igdb_id(*app_id).and_then(|p| p.romm_slug_seed()).map(str::to_string);
        let candidates: Vec<String> = live.iter().chain(seed.iter()).cloned().collect();
        if let Some(resolved) = candidates.first() {
            sqlx::query("UPDATE platforms SET romm_slug = ? WHERE id = ?").bind(resolved).bind(app_id).execute(pool).await?;
        }
        let Some(source) = candidates.iter().find_map(|c| find(c)) else {
            s.unmapped.push(name.clone());
            continue;
        };
        let target = icons_dir.join(format!("{}.svg", console_slug(name)));
        let rel = source.strip_prefix(romm_repo).unwrap_or(&source).display().to_string().replace('\\', "/");
        if target.exists() && !replace {
            s.kept_existing.push(name.clone());
        } else {
            std::fs::copy(&source, &target)?;
            s.copied.push((name.clone(), rel.clone()));
        }
        // Attributed: every icon in the folder that is RomM's file, whichever run copied it
        // (a user's own icon under the same name is not).
        if std::fs::read(&target).ok() == std::fs::read(&source).ok() {
            attributed.push((name.clone(), rel));
        }
    }
    // Attribution and license (outside the RomM checkout).
    let mut md = format!(
        "# Platform icons\n\nThe `*.svg` platform icons in this folder listed below are from **RomM** \
         (<https://github.com/rommapp/romm>, commit `{}`), licensed under the GNU AGPL-3.0 (see \
         `app_root_ex/lib/LICENSES/romm.LICENSE`). Icons from RomM's `systematic/` set follow the design of \
         libretro's *Systematic* theme (<https://github.com/libretro/retroarch-assets>, CC BY 4.0).\n\n| icon | platform | RomM file |\n|---|---|---|\n",
        s.commit
    );
    for (name, source) in &attributed {
        md.push_str(&format!("| `{}.svg` | {name} | `{source}` |\n", console_slug(name)));
    }
    std::fs::write(icons_dir.join("ATTRIBUTIONS.md"), md)?;
    let license = romm_repo.join("LICENSE");
    if license.is_file() {
        std::fs::create_dir_all(licenses_dir)?;
        std::fs::copy(&license, licenses_dir.join("romm.LICENSE"))?;
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_match_the_console() {
        assert_eq!(console_slug("Sega Dreamcast"), "sega-dreamcast");
        assert_eq!(console_slug("Atari ST/STE"), "atari-st-ste");
        assert_eq!(console_slug("Arcade (MAME family)"), "arcade-mame-family");
        assert_eq!(console_slug("  Nintendo 64 "), "nintendo-64");
    }
}
