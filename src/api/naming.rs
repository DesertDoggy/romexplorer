//! What a copy's destination subdirectories are called, per platform.
//!
//! Four built-in schemes plus per-platform overrides (`romexplorer_types::copy::FolderScheme`).
//! The compiled-in tables live in [`crate::common::platform_names`], next to the `Platform`
//! enum so a new variant is a compile error until someone names it; this module is the part
//! that varies -- the chosen scheme, the combine groups, and whatever the user typed.
//!
//! Overrides are rows in `platform_output` rather than one JSON blob in `settings`, because
//! `settings::set` has no compare-and-swap: ~82 platforms in one document means two open
//! config tabs silently clobber each other. The same row carries the platform's output
//! format, since that is the same row of the same table in the UI.

use std::collections::HashMap;

pub use romexplorer_types::copy::{CombineGroupView, NamingUpdate, NamingView, OutputFormat, PlatformFolder};
use sqlx::Row;

use crate::common::models::Platform;
use crate::common::platform_names::COMBINE_GROUPS;
use crate::database::connection::all_platforms;
use crate::database::settings as db_settings;

use super::copy::{self, sanitize_segment};
use super::{App, Error, Result};

/// `app_platform_id` -> the user's folder name.
pub async fn folder_overrides(app: &App) -> Result<HashMap<u16, String>> {
    let rows = sqlx::query("SELECT app_platform_id, folder_name FROM platform_output WHERE deleted = FALSE AND folder_name IS NOT NULL").fetch_all(&app.pool).await?;
    rows.iter().map(|r| Ok((r.try_get("app_platform_id")?, r.try_get("folder_name")?))).collect()
}

/// `app_platform_id` -> the user's output format.
pub async fn format_overrides(app: &App) -> Result<HashMap<u16, OutputFormat>> {
    let rows = sqlx::query("SELECT app_platform_id, output_format FROM platform_output WHERE deleted = FALSE AND output_format IS NOT NULL").fetch_all(&app.pool).await?;
    let mut out = HashMap::new();
    for r in &rows {
        let id: u16 = r.try_get("app_platform_id")?;
        let name: String = r.try_get("output_format")?;
        if let Some(f) = OutputFormat::from_name(&name) {
            out.insert(id, f);
        }
    }
    Ok(out)
}

/// The IGDB slugs actually resolved for this install (`platforms.romm_slug`), which beat the
/// compiled-in seed because IGDB can rename one.
pub async fn live_slugs(app: &App) -> Result<HashMap<u16, String>> {
    let rows = sqlx::query("SELECT id, romm_slug FROM platforms WHERE romm_slug IS NOT NULL").fetch_all(&app.pool).await?;
    rows.iter().map(|r| Ok((r.try_get("id")?, r.try_get("romm_slug")?))).collect()
}

pub async fn view(app: &App) -> Result<NamingView> {
    let scheme = copy::folder_scheme(app).await?;
    let combine = copy::combine_map(app).await?;
    let overrides = folder_overrides(app).await?;
    let formats = format_overrides(app).await?;
    let slugs = live_slugs(app).await?;

    let combine_view: Vec<CombineGroupView> = COMBINE_GROUPS
        .iter()
        .map(|g| CombineGroupView {
            key: g.key.to_string(),
            label: format!("{} with {}", g.children.iter().map(|c| c.display_name()).collect::<Vec<_>>().join(", "), g.parent.display_name()),
            parent: g.parent.display_name().to_string(),
            children: g.children.iter().map(|c| c.display_name().to_string()).collect(),
            on: combine.get(g.key).copied().unwrap_or(g.default_on),
        })
        .collect();

    let mut platforms: Vec<PlatformFolder> = Vec::new();
    for p in all_platforms() {
        let id = p.igdb_id();
        let slug = slugs.get(&id).cloned().or_else(|| p.romm_slug_seed().map(str::to_string));
        let combined = copy::combined_into(p, &combine);
        platforms.push(PlatformFolder {
            app_platform_id: id,
            name: p.display_name().to_string(),
            dat: p.dat_folder_name(),
            display: p.display_name().to_string(),
            slug,
            es: p.es_folder().to_string(),
            custom: overrides.get(&id).cloned(),
            effective: copy::folder_for(p, scheme, &combine, &overrides, &slugs),
            combined_into: combined.map(|t| t.display_name().to_string()),
            output_format: formats.get(&id).copied().unwrap_or_default(),
            formats: copy::output_formats_for(p),
        });
    }
    platforms.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    // Two platforms in one folder is legal -- someone may genuinely want one `nintendo/` --
    // so it is reported rather than refused.
    let mut by_folder: HashMap<String, Vec<String>> = HashMap::new();
    for p in &platforms {
        by_folder.entry(p.effective.clone()).or_default().push(p.name.clone());
    }
    let mut shared_folders: Vec<(String, Vec<String>)> = by_folder.into_iter().filter(|(_, names)| names.len() > 1).collect();
    shared_folders.sort();

    Ok(NamingView { scheme, combine: combine_view, platforms, shared_folders })
}

pub async fn update(app: &App, update: &NamingUpdate) -> Result<NamingView> {
    if let Some(scheme) = update.scheme {
        db_settings::set(&app.pool, copy::KEY_FOLDER_SCHEME, &scheme.name()).await?;
    }
    if !update.combine.is_empty() {
        let mut map = copy::combine_map(app).await?;
        for (key, on) in &update.combine {
            if !COMBINE_GROUPS.iter().any(|g| g.key == key) {
                return Err(Error::invalid(format!("unknown combine group {key:?}")));
            }
            map.insert(key.clone(), *on);
        }
        db_settings::set(&app.pool, copy::KEY_PLATFORM_COMBINE, &map).await?;
    }
    for (id, name) in &update.folders {
        let platform = Platform::from_igdb_id(*id).ok_or_else(|| Error::invalid(format!("unknown platform id {id}")))?;
        let trimmed = name.trim();
        if trimmed.is_empty() {
            clear_folder(app, *id).await?;
            continue;
        }
        // Rejected rather than silently rewritten: a user who typed `SNES/SFC` should see
        // why it can't be a folder name, not find `SNES_SFC` on their card later.
        if sanitize_segment(trimmed) != trimmed {
            return Err(Error::invalid(format!("{:?} can't be a folder name on every filesystem this may be copied to -- try {:?}", trimmed, sanitize_segment(trimmed))));
        }
        let _ = platform;
        sqlx::query("INSERT INTO platform_output (app_platform_id, folder_name, deleted) VALUES (?, ?, FALSE) ON DUPLICATE KEY UPDATE folder_name = VALUES(folder_name), deleted = FALSE")
            .bind(id)
            .bind(trimmed)
            .execute(&app.pool)
            .await?;
    }
    for (id, format) in &update.formats {
        let platform = Platform::from_igdb_id(*id).ok_or_else(|| Error::invalid(format!("unknown platform id {id}")))?;
        if !copy::output_formats_for(platform).contains(format) {
            return Err(Error::invalid(format!("{} can't be written as {}", platform.display_name(), format.name())));
        }
        let stored = (*format != OutputFormat::Same).then(|| format.name());
        sqlx::query("INSERT INTO platform_output (app_platform_id, output_format, deleted) VALUES (?, ?, FALSE) ON DUPLICATE KEY UPDATE output_format = VALUES(output_format), deleted = FALSE")
            .bind(id)
            .bind(stored)
            .execute(&app.pool)
            .await?;
    }
    view(app).await
}

async fn clear_folder(app: &App, id: u16) -> Result<()> {
    sqlx::query("UPDATE platform_output SET folder_name = NULL WHERE app_platform_id = ?").bind(id).execute(&app.pool).await?;
    // A row holding neither a name nor a format is nothing; soft-delete it so `db_optimize`
    // can take it, per CODING_RULES.
    sqlx::query("UPDATE platform_output SET deleted = TRUE WHERE app_platform_id = ? AND folder_name IS NULL AND output_format IS NULL").bind(id).execute(&app.pool).await?;
    Ok(())
}

/// Drops every custom folder name, leaving the formats alone -- they are a separate decision
/// and clearing them with one button would be a surprise.
pub async fn reset_folders(app: &App) -> Result<u64> {
    let n = sqlx::query("UPDATE platform_output SET folder_name = NULL WHERE folder_name IS NOT NULL").execute(&app.pool).await?;
    sqlx::query("UPDATE platform_output SET deleted = TRUE WHERE folder_name IS NULL AND output_format IS NULL").execute(&app.pool).await?;
    Ok(n.rows_affected())
}
