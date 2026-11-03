//! Collection › Patches: every patch file the catalogue knows of, wherever it is from.
//!
//! | `from` | what | where it lives |
//! |---|---|---|
//! | `ra` | RetroAchievements' compatibility patches, downloaded or not | `ra_game_hashes.patch_url`, `ra_patches` |
//! | `dat` | releases a DAT lists that are nothing but patch files | `release.material = 'patch'` |
//! | `own` | files in a directory of type `patch` that are neither of those | `local_files` |
//!
//! One list, because that is what a user with a folder of `.ips` files thinks of them as.
//! RetroAchievements' stay the most integrated: a row carries the hash its download button
//! needs (`POST /api/v1/ra/patches/download`), the same one its game page uses.

use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::catalog::Page;
use super::{App, Error, Result};

pub const FROM: [&str; 3] = ["ra", "dat", "own"];

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PatchQuery {
    /// One of [`FROM`]; absent or `all` for every one.
    pub from: Option<String>,
    /// Substring on the patch's name or on what it patches.
    pub search: Option<String>,
    /// Only the patches of this title's releases: a title page's own list.
    pub title_id: Option<u64>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PatchRow {
    pub from: String,
    pub name: String,
    pub platform_id: Option<u16>,
    pub platform: Option<String>,
    pub languages: Option<String>,
    /// `guessed` when the language was inferred from a folder or a loose word.
    pub languages_state: Option<String>,
    /// What it patches, as far as is known: the game a DAT names (`release.base_hint`), or
    /// the patched release RetroAchievements' patch produces.
    pub target: Option<String>,
    /// `dat`: the patch's own release. `ra`: the patched release, when it is in the catalogue.
    pub release_id: Option<u64>,
    pub title_id: Option<u64>,
    pub ra_game_id: Option<u32>,
    /// `ra`: the hash of the patched ROM -- what the download is asked for by.
    pub ra_md5: Option<String>,
    /// The file is here: downloaded (`ra`), matched to a local file (`dat`), or it is one (`own`).
    pub held: bool,
    /// `ra`: a download of it is queued or running, and how far it is.
    pub job: Option<String>,
    pub file_id: Option<u64>,
    pub path: Option<String>,
    pub size: Option<u64>,
}

/// RetroAchievements: a hash it lists a patch for, with the patched release when a DAT or
/// an earlier download has told us which that is.
const RA_SELECT: &str = "SELECT 'ra' AS origin, COALESCE(h.name, p.patched_name, h.md5) AS name, r.app_platform_id AS platform_id, pl.name AS platform, r.languages, r.languages_state, \
     COALESCE(r.name, p.patched_name) AS target, r.id AS release_id, (SELECT trx.title_id FROM title_releases trx WHERE trx.release_id = r.id LIMIT 1) AS title_id, \
     h.ra_game_id, h.md5 AS ra_md5, (p.id IS NOT NULL) AS held, NULL AS file_id, p.rel_path AS path, p.size AS size \
     FROM ra_game_hashes h \
     LEFT JOIN ra_patches p ON p.md5 = h.md5 \
     LEFT JOIN `release` r ON r.id = COALESCE(p.release_id, (SELECT rc.release_id FROM hashes hx JOIN release_contents rc ON rc.content_id = hx.content_id WHERE hx.algorithm IN ('md5', 'ra_hash') AND hx.value_hex = h.md5 LIMIT 1)) \
     LEFT JOIN platforms pl ON pl.id = r.app_platform_id \
     WHERE h.patch_url IS NOT NULL";

const DAT_SELECT: &str = "SELECT 'dat' AS origin, r.name AS name, r.app_platform_id AS platform_id, pl.name AS platform, r.languages, r.languages_state, \
     r.base_hint AS target, r.id AS release_id, (SELECT trx.title_id FROM title_releases trx WHERE trx.release_id = r.id LIMIT 1) AS title_id, \
     NULL AS ra_game_id, NULL AS ra_md5, EXISTS (SELECT 1 FROM release_contents rch JOIN local_files lfh ON lfh.content_id = rch.content_id AND lfh.deleted = FALSE WHERE rch.release_id = r.id) AS held, NULL AS file_id, NULL AS path, \
     (SELECT CAST(SUM(c.size) AS UNSIGNED) FROM release_contents rcs JOIN content c ON c.id = rcs.content_id WHERE rcs.release_id = r.id) AS size \
     FROM `release` r LEFT JOIN platforms pl ON pl.id = r.app_platform_id \
     WHERE r.material = 'patch' AND r.deleted = FALSE";

/// A user's own: under a patch directory, and not a file either of the others accounts for.
const OWN_SELECT: &str = "SELECT 'own' AS origin, SUBSTRING_INDEX(REPLACE(lf.path, CHAR(92), '/'), '/', -1) AS name, d.app_platform_id AS platform_id, pl.name AS platform, NULL AS languages, NULL AS languages_state, \
     NULL AS target, NULL AS release_id, NULL AS title_id, NULL AS ra_game_id, NULL AS ra_md5, TRUE AS held, lf.id AS file_id, lf.path AS path, lf.size AS size \
     FROM local_files lf LEFT JOIN local_directories d ON d.id = lf.directory_id LEFT JOIN platforms pl ON pl.id = d.app_platform_id \
     WHERE lf.deleted = FALSE \
       AND EXISTS (SELECT 1 FROM local_directories pd WHERE pd.dir_type = 'patch' AND pd.deleted = FALSE AND LEFT(lf.path, CHAR_LENGTH(pd.path)) = pd.path) \
       AND NOT EXISTS (SELECT 1 FROM ra_patches rp WHERE rp.content_id = lf.content_id) \
       AND NOT EXISTS (SELECT 1 FROM release_contents rco JOIN `release` ro ON ro.id = rco.release_id WHERE rco.content_id = lf.content_id AND ro.material = 'patch' AND ro.deleted = FALSE)";

fn selects(from: Option<&str>) -> Result<Vec<&'static str>> {
    Ok(match from.map(str::trim).filter(|f| !f.is_empty()).unwrap_or("all") {
        "all" => vec![RA_SELECT, DAT_SELECT, OWN_SELECT],
        "ra" => vec![RA_SELECT],
        "dat" => vec![DAT_SELECT],
        "own" => vec![OWN_SELECT],
        other => return Err(Error::invalid(format!("from {other:?} must be all, {}", FROM.join(", ")))),
    })
}

pub async fn list(app: &App, q: &PatchQuery) -> Result<Page<PatchRow>> {
    let pool = &app.pool;
    let union = selects(q.from.as_deref())?.join(" UNION ALL ");
    let search = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| format!("%{}%", s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
    let mut filter = if search.is_some() { " WHERE (x.name LIKE ? OR x.target LIKE ?)".to_string() } else { " WHERE TRUE".to_string() };
    if let Some(title) = q.title_id {
        // A number, so it can go into the statement as text.
        filter.push_str(&format!(" AND x.title_id = {title}"));
    }
    let (limit, offset) = (q.limit.unwrap_or(50).clamp(1, 500), q.offset.unwrap_or(0));

    let count_sql = format!("SELECT COUNT(*) FROM ({union}) x{filter}");
    let mut count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(count_sql));
    if let Some(s) = &search {
        count = count.bind(s.clone()).bind(s.clone());
    }
    let total = count.fetch_one(pool).await? as u64;

    // Stacked lists leave the type of a column to whichever list is first and to how NULL
    // meets a number, so each is said here.
    let page_sql = format!(
        "SELECT x.origin, x.name, CAST(x.platform_id AS UNSIGNED) AS platform_id, x.platform, x.languages, x.languages_state, x.target, CAST(x.release_id AS UNSIGNED) AS release_id,          CAST(x.title_id AS UNSIGNED) AS title_id, CAST(x.ra_game_id AS UNSIGNED) AS ra_game_id, x.ra_md5, CAST(x.held AS SIGNED) AS held, CAST(x.file_id AS UNSIGNED) AS file_id, x.path, CAST(x.size AS UNSIGNED) AS size          FROM ({union}) x{filter} ORDER BY x.name, x.origin, x.ra_md5, x.release_id, x.file_id LIMIT ? OFFSET ?"
    );
    let mut page = sqlx::query(sqlx::AssertSqlSafe(page_sql));
    if let Some(s) = &search {
        page = page.bind(s.clone()).bind(s.clone());
    }
    let rows = page.bind(limit).bind(offset).fetch_all(pool).await?;

    // RetroAchievements downloads under way, by the hash they are for.
    let mut jobs: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if rows.iter().any(|r| r.try_get::<String, _>("origin").is_ok_and(|o| o == "ra")) {
        for r in sqlx::query("SELECT payload, status, progress_message FROM jobs WHERE job_type = 'ra_patch_download' AND status IN ('pending', 'in_progress')").fetch_all(pool).await? {
            let payload: serde_json::Value = r.try_get("payload")?;
            if let Some(md5) = payload.get("md5").and_then(|m| m.as_str()) {
                let status: String = r.try_get("status")?;
                jobs.insert(md5.to_ascii_lowercase(), r.try_get::<Option<String>, _>("progress_message")?.unwrap_or(status));
            }
        }
    }

    let items = rows
        .iter()
        .map(|r| -> Result<PatchRow> {
            let ra_md5: Option<String> = r.try_get("ra_md5")?;
            Ok(PatchRow {
                from: r.try_get("origin")?,
                name: r.try_get::<Option<String>, _>("name")?.unwrap_or_default(),
                platform_id: r.try_get::<Option<u64>, _>("platform_id")?.map(|id| id as u16),
                platform: r.try_get("platform")?,
                languages: r.try_get("languages")?,
                languages_state: r.try_get("languages_state")?,
                target: r.try_get("target")?,
                release_id: r.try_get("release_id")?,
                title_id: r.try_get("title_id")?,
                ra_game_id: r.try_get::<Option<u64>, _>("ra_game_id")?.map(|id| id as u32),
                job: ra_md5.as_ref().and_then(|m| jobs.get(&m.to_ascii_lowercase()).cloned()),
                ra_md5,
                held: r.try_get::<i64, _>("held")? != 0,
                file_id: r.try_get("file_id")?,
                path: r.try_get("path")?,
                size: r.try_get("size")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Page { items, total, limit, offset })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_picks_the_lists() {
        assert_eq!(selects(None).unwrap().len(), 3);
        assert_eq!(selects(Some("all")).unwrap().len(), 3);
        assert_eq!(selects(Some("dat")).unwrap(), vec![DAT_SELECT]);
        assert!(selects(Some("mine")).is_err());
    }
}
