//! What a selection (sets, titles, releases, games, roms, files -- `CopySelection`) means in
//! releases. Copy plans games, roms and files itself, so it takes [`named_releases`]; the
//! art and export jobs only need releases, and take [`release_ids`].

use std::collections::HashSet;

use sqlx::Row;

use romexplorer_types::copy::{CopySelection, OwnedRef};

use super::{App, Result};

/// Every release the selection names through its sets, releases and titles, in the order it
/// named them. `titles_all`: every release of a selected title, not just its top-ranked one.
pub async fn named_releases(app: &App, selection: &CopySelection, titles_all: bool, warnings: &mut Vec<String>) -> Result<Vec<u64>> {
    let mut out = Ordered::default();
    if !selection.set_ids.is_empty() {
        // Its titles and games resolved to the best release there is now; a deleted set
        // names nothing.
        super::sets::resolve(app, &selection.set_ids).await?;
        let sql = format!(
            "SELECT csr.release_id FROM collection_set_releases csr JOIN collection_sets cs ON cs.id = csr.set_id AND cs.deleted = FALSE \
             JOIN `release` r ON r.id = csr.release_id AND r.deleted = FALSE WHERE csr.set_id IN ({}) ORDER BY csr.set_id, csr.position, csr.release_id",
            marks(selection.set_ids.len())
        );
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        for id in &selection.set_ids {
            q = q.bind(id);
        }
        for row in q.fetch_all(&app.pool).await? {
            out.push(row.try_get("release_id")?);
        }
    }
    for id in &selection.release_ids {
        out.push(*id);
    }
    if !selection.title_ids.is_empty() {
        if titles_all {
            let sql = format!("SELECT tr.release_id FROM title_releases tr WHERE tr.title_id IN ({}) ORDER BY tr.title_id, tr.sort_weight, tr.release_id", marks(selection.title_ids.len()));
            let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
            for id in &selection.title_ids {
                q = q.bind(id);
            }
            for row in q.fetch_all(&app.pool).await? {
                out.push(row.try_get("release_id")?);
            }
        } else {
            // The same "which release does this title mean" answer the rest of the UI gives
            // -- manual order, kind, region/language priority, edition. Re-deriving it here
            // would drift from it within a release.
            let picked = super::sets::top_releases(app, &selection.title_ids).await?;
            if picked.len() < selection.title_ids.len() {
                warnings.push(format!("{} of the selected titles have no release", selection.title_ids.len() - picked.len()));
            }
            for id in picked {
                out.push(id);
            }
        }
    }
    let mut title_rows = selection.title_rows.clone();
    title_rows.extend(crate::database::release_buckets::decode_row_ids(&app.pool, &selection.title_row_ids).await?);
    if !title_rows.is_empty() {
        // A row is one kind of the title's releases: only those, and nothing in their place
        // when it has none -- a copy of a title's retail row never takes its demo.
        let rank = super::catalog::ReleaseRank::load(app).await?;
        let mut empty = 0;
        for row in &title_rows {
            let Some(in_row) = crate::database::release_buckets::in_row_sql("r", &row.row_key, rank.unlicensed_is_retail) else {
                return Err(super::Error::invalid(format!("title row {:?} is not a row key", row.row_key)));
            };
            let sql = format!(
                "SELECT r.id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE tr.title_id = ? AND {in_row} ORDER BY {}{}",
                rank.sql("r"),
                if titles_all { "" } else { " LIMIT 1" }
            );
            let ids: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql)).bind(row.title_id).fetch_all(&app.pool).await?;
            if ids.is_empty() {
                empty += 1;
            }
            for id in ids {
                out.push(id);
            }
        }
        if empty > 0 {
            warnings.push(format!("{empty} of the selected title rows have no release of their kind; nothing else was taken in their place"));
        }
    }
    Ok(out.list)
}

/// [`named_releases`], then the releases of the selected games, roms and files: everything
/// the selection is about, as releases.
pub async fn release_ids(app: &App, selection: &CopySelection, titles_all: bool, warnings: &mut Vec<String>) -> Result<Vec<u64>> {
    let mut out = Ordered::default();
    for id in named_releases(app, selection, titles_all, warnings).await? {
        out.push(id);
    }
    let mut games: Vec<OwnedRef> = selection.game_ids.clone();
    for rom in &selection.rom_ids {
        games.extend(games_of_rom(app, *rom).await?);
    }
    for file in &selection.file_ids {
        for rom in roms_of_file(app, *file).await? {
            games.extend(games_of_rom(app, rom).await?);
        }
    }
    for game in games {
        let ids: Vec<u64> = match game.source_id {
            None => sqlx::query_scalar("SELECT release_id FROM local_release_games WHERE game_id = ? ORDER BY release_id").bind(game.id).fetch_all(&app.pool).await?,
            Some(sid) => sqlx::query_scalar("SELECT release_id FROM remote_release_games WHERE source_id = ? AND game_id = ? ORDER BY release_id").bind(sid).bind(game.id).fetch_all(&app.pool).await?,
        };
        for id in ids {
            out.push(id);
        }
    }
    Ok(out.list)
}

async fn games_of_rom(app: &App, rom: OwnedRef) -> Result<Vec<OwnedRef>> {
    let ids: Vec<u64> = match rom.source_id {
        None => sqlx::query_scalar("SELECT game_id FROM local_game_roms WHERE rom_id = ? ORDER BY game_id").bind(rom.id).fetch_all(&app.pool).await?,
        Some(sid) => sqlx::query_scalar("SELECT game_id FROM remote_game_roms WHERE source_id = ? AND rom_id = ? ORDER BY game_id").bind(sid).bind(rom.id).fetch_all(&app.pool).await?,
    };
    Ok(ids.into_iter().map(|id| OwnedRef { source_id: rom.source_id, id }).collect())
}

async fn roms_of_file(app: &App, file: OwnedRef) -> Result<Vec<OwnedRef>> {
    let ids: Vec<u64> = match file.source_id {
        None => sqlx::query_scalar("SELECT rom_id FROM local_rom_files WHERE file_id = ? ORDER BY rom_id").bind(file.id).fetch_all(&app.pool).await?,
        Some(sid) => sqlx::query_scalar("SELECT rom_id FROM remote_rom_files WHERE source_id = ? AND file_id = ? ORDER BY rom_id").bind(sid).bind(file.id).fetch_all(&app.pool).await?,
    };
    Ok(ids.into_iter().map(|id| OwnedRef { source_id: file.source_id, id }).collect())
}

fn marks(n: usize) -> String {
    vec!["?"; n].join(",")
}

/// Ids in first-seen order, each once.
#[derive(Default)]
struct Ordered {
    list: Vec<u64>,
    seen: HashSet<u64>,
}

impl Ordered {
    fn push(&mut self, id: u64) {
        if self.seen.insert(id) {
            self.list.push(id);
        }
    }
}
