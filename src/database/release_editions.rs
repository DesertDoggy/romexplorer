//! One-time data migration for `0015`: fills `release.is_special_edition` / `is_budget` /
//! `is_aftermarket` / `revision_sort` (and `is_demo` for the Japanese trial-disc tags the
//! parser learned then) on releases imported before, by re-parsing their names. Imports
//! set them from then on.

use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;
use super::matching::name_parse::{parse_name, revision_sort, Edition, Variant};

const NAME: &str = "release_editions_0015";
const BATCH: u64 = 20_000;

/// Runs once per database (recorded in `data_migrations`). Returns how many releases changed.
pub async fn backfill(pool: &MySqlPool) -> Result<u64, DatabaseError> {
    let done: Option<String> = sqlx::query_scalar("SELECT name FROM data_migrations WHERE name = ?").bind(NAME).fetch_optional(pool).await?;
    if done.is_some() {
        return Ok(0);
    }
    let started = std::time::Instant::now();
    let mut conn = pool.acquire().await?;
    sqlx::query("CREATE TEMPORARY TABLE IF NOT EXISTS release_edition_fix (id BIGINT UNSIGNED PRIMARY KEY, special BOOLEAN, budget BOOLEAN, aftermarket BOOLEAN, rev INT UNSIGNED, demo BOOLEAN)")
        .execute(&mut *conn)
        .await?;
    let (mut last, mut changed) = (0u64, 0u64);
    loop {
        let rows = sqlx::query("SELECT id, name, revision FROM `release` WHERE id > ? ORDER BY id LIMIT ?").bind(last).bind(BATCH).fetch_all(&mut *conn).await?;
        let Some(tail) = rows.last() else { break };
        last = tail.try_get("id")?;
        let mut fixes: Vec<(u64, bool, bool, bool, u32, bool)> = Vec::new();
        for r in &rows {
            let name: String = r.try_get("name")?;
            let revision: Option<String> = r.try_get("revision")?;
            let parsed = parse_name(&name);
            let rev = revision_sort(revision.as_deref().or(parsed.revision.as_deref()));
            let demo = parsed.has_variant(Variant::Demo) || parsed.has_variant(Variant::Sample);
            if parsed.edition.is_some() || rev > 0 || demo {
                fixes.push((r.try_get("id")?, parsed.edition == Some(Edition::Special), parsed.edition == Some(Edition::Budget), parsed.edition == Some(Edition::Aftermarket), rev, demo));
            }
        }
        for chunk in fixes.chunks(2_000) {
            let values = vec!["(?, ?, ?, ?, ?, ?)"; chunk.len()].join(", ");
            let mut q = sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO release_edition_fix (id, special, budget, aftermarket, rev, demo) VALUES {values}")));
            for f in chunk {
                q = q.bind(f.0).bind(f.1).bind(f.2).bind(f.3).bind(f.4).bind(f.5);
            }
            q.execute(&mut *conn).await?;
        }
        changed += sqlx::query(
            "UPDATE `release` r JOIN release_edition_fix f ON f.id = r.id SET r.is_special_edition = (r.is_special_edition OR f.special), r.is_budget = (r.is_budget OR f.budget), \
             r.is_aftermarket = (r.is_aftermarket OR f.aftermarket), r.revision_sort = GREATEST(r.revision_sort, f.rev), r.is_demo = (r.is_demo OR f.demo)",
        )
        .execute(&mut *conn)
        .await?
        .rows_affected();
        sqlx::query("DELETE FROM release_edition_fix").execute(&mut *conn).await?;
    }
    sqlx::query("DROP TEMPORARY TABLE IF EXISTS release_edition_fix").execute(&mut *conn).await?;
    sqlx::query("INSERT IGNORE INTO data_migrations (name) VALUES (?)").bind(NAME).execute(&mut *conn).await?;
    if changed > 0 {
        log::info!("release editions: flagged / versioned {changed} release(s) in {:.1}s (one-time)", started.elapsed().as_secs_f64());
    }
    Ok(changed)
}

const RERELEASE_NAME: &str = "release_rereleases_0058";

/// One-time data migration for `0058`: flags `release.is_rerelease` on the releases imported
/// before the parser knew re-release tags ("(Virtual Console)", ...). Imports set it from then
/// on. Returns how many releases were flagged.
pub async fn backfill_rereleases(pool: &MySqlPool) -> Result<u64, DatabaseError> {
    let done: Option<String> = sqlx::query_scalar("SELECT name FROM data_migrations WHERE name = ?").bind(RERELEASE_NAME).fetch_optional(pool).await?;
    if done.is_some() {
        return Ok(0);
    }
    let started = std::time::Instant::now();
    let (mut last, mut changed) = (0u64, 0u64);
    loop {
        // Only names with a tag after the region can carry one.
        let rows = sqlx::query("SELECT id, name FROM `release` WHERE id > ? ORDER BY id LIMIT ?").bind(last).bind(BATCH).fetch_all(pool).await?;
        let Some(tail) = rows.last() else { break };
        last = tail.try_get("id")?;
        let mut ids: Vec<u64> = Vec::new();
        for r in &rows {
            let name: String = r.try_get("name")?;
            if name.matches('(').count() > 1 && parse_name(&name).edition == Some(Edition::Rerelease) {
                ids.push(r.try_get("id")?);
            }
        }
        for chunk in ids.chunks(2_000) {
            let list = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
            changed += sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE `release` SET is_rerelease = TRUE WHERE id IN ({list}) AND is_rerelease = FALSE"))).execute(pool).await?.rows_affected();
        }
    }
    sqlx::query("INSERT IGNORE INTO data_migrations (name) VALUES (?)").bind(RERELEASE_NAME).execute(pool).await?;
    if changed > 0 {
        log::info!("release editions: flagged {changed} re-release(s) in {:.1}s (one-time)", started.elapsed().as_secs_f64());
    }
    Ok(changed)
}

const VARIANTS_NAME: &str = "release_variants_0071";

/// One-time data migration for `0071`: the parser used to keep only a name's last variant tag,
/// so "(Demo) (Unl)" was imported as unlicensed and not a demo, "(Hack) (Unl)" as a plain
/// commercial release. Re-reads every name with more than one tag and adds the flags it was
/// missing (never clears one: a DAT's own flags stay), and flags pirate copies. A hack the
/// name alone says is one gets its game kind, as the import gives it. Returns releases changed.
pub async fn backfill_variants(pool: &MySqlPool) -> Result<u64, DatabaseError> {
    let done: Option<String> = sqlx::query_scalar("SELECT name FROM data_migrations WHERE name = ?").bind(VARIANTS_NAME).fetch_optional(pool).await?;
    if done.is_some() {
        return Ok(0);
    }
    let started = std::time::Instant::now();
    let mut conn = pool.acquire().await?;
    sqlx::query("CREATE TEMPORARY TABLE IF NOT EXISTS release_variant_fix (id BIGINT UNSIGNED PRIMARY KEY, beta BOOLEAN, proto BOOLEAN, demo BOOLEAN, unl BOOLEAN, pirate BOOLEAN, patch VARCHAR(16) NULL)")
        .execute(&mut *conn)
        .await?;
    let (mut last, mut changed) = (0u64, 0u64);
    loop {
        let rows = sqlx::query("SELECT id, name FROM `release` WHERE id > ? ORDER BY id LIMIT ?").bind(last).bind(BATCH).fetch_all(&mut *conn).await?;
        let Some(tail) = rows.last() else { break };
        last = tail.try_get("id")?;
        let mut fixes: Vec<(u64, bool, bool, bool, bool, bool, Option<&'static str>)> = Vec::new();
        for r in &rows {
            let name: String = r.try_get("name")?;
            let parsed = parse_name(&name);
            let has = |v: Variant| parsed.has_variant(v);
            // One tag was always read right; a pirate copy was only ever flagged unlicensed.
            if parsed.variants.len() < 2 && !has(Variant::Pirate) {
                continue;
            }
            let patch = if has(Variant::Translation) { Some("translation") } else if has(Variant::Hack) { Some("gameplay") } else { None };
            fixes.push((r.try_get("id")?, has(Variant::Beta), has(Variant::Proto), has(Variant::Demo) || has(Variant::Sample), has(Variant::Unlicensed) || has(Variant::Pirate), has(Variant::Pirate), patch));
        }
        for chunk in fixes.chunks(2_000) {
            let values = vec!["(?, ?, ?, ?, ?, ?, ?)"; chunk.len()].join(", ");
            let mut q = sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO release_variant_fix (id, beta, proto, demo, unl, pirate, patch) VALUES {values}")));
            for f in chunk {
                q = q.bind(f.0).bind(f.1).bind(f.2).bind(f.3).bind(f.4).bind(f.5).bind(f.6);
            }
            q.execute(&mut *conn).await?;
        }
        changed += sqlx::query(
            "UPDATE `release` r JOIN release_variant_fix f ON f.id = r.id SET r.is_beta = (r.is_beta OR f.beta), r.is_prototype = (r.is_prototype OR f.proto), r.is_demo = (r.is_demo OR f.demo), \
             r.is_unlicensed = (r.is_unlicensed OR f.unl), r.is_pirate = (r.is_pirate OR f.pirate), \
             r.community_patched = (r.community_patched OR (f.patch IS NOT NULL AND r.material IS NULL)), \
             r.patch_type = IF(r.material IS NULL, COALESCE(r.patch_type, f.patch), r.patch_type), \
             r.game_kind = IF(r.material IS NULL AND f.patch IS NOT NULL, COALESCE(r.game_kind, IF(f.patch = 'translation', 'translation', 'hack')), r.game_kind)",
        )
        .execute(&mut *conn)
        .await?
        .rows_affected();
        sqlx::query("DELETE FROM release_variant_fix").execute(&mut *conn).await?;
    }
    sqlx::query("DROP TEMPORARY TABLE IF EXISTS release_variant_fix").execute(&mut *conn).await?;
    sqlx::query("INSERT IGNORE INTO data_migrations (name) VALUES (?)").bind(VARIANTS_NAME).execute(&mut *conn).await?;
    if changed > 0 {
        log::info!("release variants: re-read the tags of {changed} release(s) in {:.1}s (one-time)", started.elapsed().as_secs_f64());
    }
    Ok(changed)
}
