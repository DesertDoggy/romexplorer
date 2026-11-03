//! Collection › BIOS / firmware: a machine's own system files, wherever they are known from.
//!
//! | `from` | what | where it lives |
//! |---|---|---|
//! | `dat` | releases a DAT lists as a BIOS, firmware or system update | `release.material = 'bios'` |
//! | `own` | every system file you hold: the DAT entries a local file matches, and the files a scan identified that no DAT lists | `release` + `local_files.system_type` |
//!
//! `all` is the DAT entries and the files no DAT lists, each once. The files no DAT lists are
//! where a NAND or SD-card dump shows up, and a BIOS a DAT does not have. None has a title: a
//! title is a game (`platform_map::STANDALONE`).

use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::catalog::Page;
use super::{App, Error, Result};

pub const FROM: [&str; 2] = ["dat", "own"];

/// What a row is, as the list's kind filter offers it.
pub const KINDS: [&str; 8] = ["bios", "firmware", "nand", "sdcard", "dsp", "font", "kanji", "other"];

#[derive(Debug, Clone, Default, Deserialize)]
pub struct BiosQuery {
    /// One of [`FROM`]; absent or `all` for both.
    pub from: Option<String>,
    /// One of [`KINDS`].
    pub kind: Option<String>,
    /// An app platform id.
    pub platform: Option<u16>,
    /// Several, comma-separated: any of them.
    pub platforms: Option<String>,
    /// Substring on the name or the file's path.
    pub search: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BiosRow {
    pub from: String,
    pub name: String,
    /// One of [`KINDS`]: for a `dat` row from its name and category, for an `own` row what
    /// the scan identified the file as.
    pub kind: String,
    pub platform_id: Option<u16>,
    pub platform: Option<String>,
    /// `dat`: the release.
    pub release_id: Option<u64>,
    /// The file is here: matched to a local file (`dat`), or it is one (`own`).
    pub held: bool,
    /// A local file of it: the one matched (`dat`), or the file itself (`own`).
    pub file_id: Option<u64>,
    pub path: Option<String>,
    pub size: Option<u64>,
    /// `dat`: the DAT source(s) listing it.
    pub source: Option<String>,
}

/// A DAT's entries. The kind is read from the name, the way the DATs spell it. retrobios
/// carries an emulator's support files beside the BIOS dumps (PCSX2's `patches.zip`, a
/// system's `message.wav`): an archive, a sound, a text or an image is `other`, not a BIOS.
///
/// retrobios also files a console's BIOS a second time under MAME (52) or Neo Geo MVS (79)
/// when an arcade driver needs it -- the Famicom Disk System's `fdsbios.nes`. That copy is
/// left out when a console's entry has the same bytes: the file is the console's.
const DAT_SELECT: &str = "SELECT 'dat' AS origin, r.name AS name, \
     CASE WHEN r.name LIKE '%NAND%' THEN 'nand' WHEN r.name LIKE '%DSP%' THEN 'dsp' \
          WHEN r.name LIKE '%kanji%' THEN 'kanji' WHEN r.name LIKE '%font%' OR r.name LIKE '%cgrom%' THEN 'font' \
          WHEN r.category = 'Firmware' OR r.name LIKE '%Firmware%' OR r.name LIKE '%System Software%' OR r.name LIKE '%System Update%' OR r.name LIKE '%.pup' THEN 'firmware' \
          WHEN r.name REGEXP '[.](zip|7z|rar|wav|mp3|ogg|flac|txt|xml|json|cfg|ini|png|jpg|bmp|pdf|html?)$' THEN 'other' \
          ELSE 'bios' END AS kind, \
     r.app_platform_id AS platform_id, pl.name AS platform, r.id AS release_id, \
     (SELECT lfh.id FROM release_contents rch JOIN local_files lfh ON lfh.content_id = rch.content_id AND lfh.deleted = FALSE WHERE rch.release_id = r.id ORDER BY lfh.id LIMIT 1) AS file_id, \
     (SELECT lfp.path FROM release_contents rcp JOIN local_files lfp ON lfp.content_id = rcp.content_id AND lfp.deleted = FALSE WHERE rcp.release_id = r.id ORDER BY lfp.id LIMIT 1) AS path, \
     (SELECT CAST(SUM(c.size) AS UNSIGNED) FROM release_contents rcs JOIN content c ON c.id = rcs.content_id WHERE rcs.release_id = r.id) AS size, \
     (SELECT GROUP_CONCAT(DISTINCT s.name ORDER BY s.name SEPARATOR ', ') FROM release_sources rs JOIN sources s ON s.id = rs.source_id WHERE rs.release_id = r.id) AS source \
     FROM `release` r LEFT JOIN platforms pl ON pl.id = r.app_platform_id \
     WHERE r.material = 'bios' AND r.deleted = FALSE \
       AND NOT (r.app_platform_id IN (52, 79) AND EXISTS (SELECT 1 FROM release_contents rca \
            JOIN release_contents rcb ON rcb.content_id = rca.content_id AND rcb.release_id <> rca.release_id \
            JOIN `release` rb ON rb.id = rcb.release_id AND rb.material = 'bios' AND rb.deleted = FALSE AND (rb.app_platform_id IS NULL OR rb.app_platform_id NOT IN (52, 79)) \
            WHERE rca.release_id = r.id))";

/// What a scan identified, and no BIOS release accounts for.
const OWN_SELECT: &str = "SELECT 'own' AS origin, SUBSTRING_INDEX(REPLACE(lf.path, CHAR(92), '/'), '/', -1) AS name, lf.system_type AS kind, \
     lf.system_platform AS platform_id, pl.name AS platform, NULL AS release_id, lf.id AS file_id, lf.path AS path, lf.size AS size, NULL AS source \
     FROM local_files lf LEFT JOIN platforms pl ON pl.id = lf.system_platform \
     WHERE lf.deleted = FALSE AND lf.system_type IS NOT NULL \
       AND NOT EXISTS (SELECT 1 FROM release_contents rco JOIN `release` ro ON ro.id = rco.release_id WHERE rco.content_id = lf.content_id AND ro.material = 'bios' AND ro.deleted = FALSE)";

fn selects(from: Option<&str>) -> Result<Vec<String>> {
    Ok(match from.map(str::trim).filter(|f| !f.is_empty()).unwrap_or("all") {
        "all" => vec![DAT_SELECT.to_string(), OWN_SELECT.to_string()],
        "dat" => vec![DAT_SELECT.to_string()],
        // Everything held: the DAT entries a file of yours matches, and the files no DAT lists.
        "own" => vec![format!("SELECT * FROM ({DAT_SELECT}) held WHERE held.file_id IS NOT NULL"), OWN_SELECT.to_string()],
        other => return Err(Error::invalid(format!("from {other:?} must be all, {}", FROM.join(", ")))),
    })
}

pub async fn list(app: &App, q: &BiosQuery) -> Result<Page<BiosRow>> {
    let pool = &app.pool;
    let union = selects(q.from.as_deref())?.join(" UNION ALL ");
    let kind = q.kind.as_deref().map(str::trim).filter(|k| !k.is_empty());
    if let Some(k) = kind {
        if !KINDS.contains(&k) {
            return Err(Error::invalid(format!("kind {k:?} must be one of {}", KINDS.join(", "))));
        }
    }
    let search = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| format!("%{}%", s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
    // The kind is one of KINDS and the platform a number: both can go in as text.
    let mut filter = String::from(" WHERE TRUE");
    if search.is_some() {
        filter.push_str(" AND (x.name LIKE ? OR x.path LIKE ?)");
    }
    if let Some(k) = kind {
        filter.push_str(&format!(" AND x.kind = '{k}'"));
    }
    // Any of the chosen platforms, each with the platforms shown as one (NES and Famicom), as
    // the other lists do.
    let mut chosen: Vec<u16> = q.platform.into_iter().collect();
    chosen.extend(q.platforms.as_deref().unwrap_or("").split(',').filter_map(|p| p.trim().parse::<u16>().ok()));
    let ids: Vec<String> = chosen.iter().flat_map(|p| super::catalog::platform_filter_ids(*p)).map(|p| p.to_string()).collect();
    if !ids.is_empty() {
        filter.push_str(&format!(" AND x.platform_id IN ({})", ids.join(",")));
    }
    let (limit, offset) = (q.limit.unwrap_or(50).clamp(1, 500), q.offset.unwrap_or(0));

    let mut count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM ({union}) x{filter}")));
    if let Some(s) = &search {
        count = count.bind(s.clone()).bind(s.clone());
    }
    let total = count.fetch_one(pool).await? as u64;

    // Stacked lists leave a column's type to the first list and to how NULL meets a number,
    // so each is said here.
    let page_sql = format!(
        "SELECT x.origin, x.name, x.kind, CAST(x.platform_id AS UNSIGNED) AS platform_id, x.platform, CAST(x.release_id AS UNSIGNED) AS release_id, \
         CAST(x.file_id AS UNSIGNED) AS file_id, x.path, CAST(x.size AS UNSIGNED) AS size, x.source \
         FROM ({union}) x{filter} ORDER BY x.platform, x.name, x.origin, x.release_id, x.file_id LIMIT ? OFFSET ?"
    );
    let mut page = sqlx::query(sqlx::AssertSqlSafe(page_sql));
    if let Some(s) = &search {
        page = page.bind(s.clone()).bind(s.clone());
    }
    let rows = page.bind(limit).bind(offset).fetch_all(pool).await?;
    let items = rows
        .iter()
        .map(|r| -> Result<BiosRow> {
            let file_id: Option<u64> = r.try_get("file_id")?;
            Ok(BiosRow {
                from: r.try_get("origin")?,
                name: r.try_get::<Option<String>, _>("name")?.unwrap_or_default(),
                kind: r.try_get::<Option<String>, _>("kind")?.unwrap_or_else(|| "bios".to_string()),
                platform_id: r.try_get::<Option<u64>, _>("platform_id")?.map(|id| id as u16),
                platform: r.try_get("platform")?,
                release_id: r.try_get("release_id")?,
                held: file_id.is_some(),
                file_id,
                path: r.try_get("path")?,
                size: r.try_get("size")?,
                source: r.try_get("source")?,
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
        assert_eq!(selects(None).unwrap().len(), 2);
        let own = selects(Some("own")).unwrap();
        assert!(own.len() == 2 && own[0].contains("file_id IS NOT NULL") && own[1] == OWN_SELECT, "my files: the held DAT entries too");
        assert!(selects(Some("ra")).is_err());
    }

    /// Every kind a scan writes (`api::scan::system_type_name`) can be filtered on.
    #[test]
    fn the_kind_filter_knows_every_kind_a_scan_writes() {
        for kind in ["bios", "firmware", "nand", "sdcard", "font", "kanji", "dsp", "other"] {
            assert!(KINDS.contains(&kind), "{kind}");
        }
    }
}
