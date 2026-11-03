//! A release's display data (`release_display`): what each field is, from which source, and
//! the system's default where no source has said (`common::display`).

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::common::display::{self, display_default, FIELDS, SOURCE_RANK};
use crate::common::models::Platform;

use super::{App, Error, Result};

/// One field of a release's display, as the pages show it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DisplayValue {
    pub field: String,
    /// `None` where the system has nothing to say either (a handheld's video modes).
    pub value: Option<String>,
    /// Who said it: `user`, `sfo`, `mame`, `pcsx2`; `None` for the system default.
    pub source: Option<String>,
    /// `default` (the system's, nothing recorded) or `confirmed` (a source recorded it).
    pub state: String,
    /// For `modes`: every resolution the system can draw in the release's region (what the
    /// editor offers); empty for the other fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub system: Vec<String>,
    /// For `refresh`: the system's documented exact rate, e.g. `60.0988`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hz: Option<String>,
    /// For `aspect`: what the page's screenshot comparison shows beside 4:3
    /// (`common::display::aspect_comparison`): `8:7` or `square`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare: Option<String>,
}

/// The four fields of `release_id`, each from the highest-ranked source that has it
/// (`SOURCE_RANK`), else the default of its platform and region.
pub async fn values(app: &App, release_id: u64, platform_id: u16, region: Option<&str>) -> Result<Vec<DisplayValue>> {
    let rows: Vec<(String, String, String)> = sqlx::query_as("SELECT CAST(field AS CHAR), value, source FROM release_display WHERE release_id = ?").bind(release_id).fetch_all(&app.pool).await?;
    let platform = Platform::from_igdb_id(platform_id).unwrap_or(Platform::Unknown);
    let defaults = display_default(platform, region.unwrap_or(""));
    Ok(FIELDS
        .iter()
        .map(|field| {
            let best = rows.iter().filter(|(f, _, _)| f == field).min_by_key(|(_, _, s)| SOURCE_RANK.iter().position(|r| r == s).unwrap_or(usize::MAX));
            let system = if *field == "modes" { defaults.system.iter().map(|s| s.to_string()).collect() } else { Vec::new() };
            let hz = if *field == "refresh" { defaults.hz.map(str::to_string) } else { None };
            let compare = if *field == "aspect" { display::aspect_comparison(platform).map(str::to_string) } else { None };
            match best {
                Some((_, value, source)) => DisplayValue { field: field.to_string(), value: Some(value.clone()), source: Some(source.clone()), state: "confirmed".into(), system, hz, compare },
                None => DisplayValue { field: field.to_string(), value: defaults.value(field), source: None, state: "default".into(), system, hz, compare },
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// The same answer in SQL, for the lists' filters, sorting and columns
// ---------------------------------------------------------------------------

/// Region codes of a PAL / NTSC market as a REGEXP over `release.region` (comma list, spaces
/// dropped), the same lists `common::display::is_pal` reads.
const PAL_REGEXP: &str = "(^|,)(EU|UK|AU|NZ|DE|FR|ES|IT|NL|SE|DK|NO|FI|PT|GR|PL|RU|SCANDINAVIA|EUROPE)(,|$)";
const NTSC_REGEXP: &str = "(^|,)(US|JP|KR|CA|BR|TW|HK|ASIA|USA|JAPAN|WORLD)(,|$)";

fn sql_text(v: Option<&str>) -> String {
    match v {
        Some(v) => format!("'{}'", v.replace('\'', "")),
        None => "NULL".to_string(),
    }
}

/// The system default of `field` for the release aliased `{A}`, as one SQL expression: a CASE
/// over its platform, PAL or NTSC by its region where they differ -- what [`display_default`]
/// answers, generated from it so the two cannot drift.
fn default_template(field: &str) -> String {
    let region = "REPLACE(COALESCE({A}.region, ''), ' ', '')";
    let (pal, ntsc) = (format!("{region} REGEXP '{PAL_REGEXP}'"), format!("{region} REGEXP '{NTSC_REGEXP}'"));
    // expression -> the platforms it is for, in first-seen order.
    let mut groups: Vec<(String, Vec<u16>)> = Vec::new();
    for platform in crate::database::connection::all_platforms() {
        let value = |region: &str| display_default(platform, region).value(field);
        let (us, eu, none) = (value("US"), value("EU"), value(""));
        let expr = if us == eu {
            sql_text(us.as_deref())
        } else if none == eu {
            // Sold mostly in PAL markets: PAL unless it names an NTSC region.
            format!("IF({ntsc}, {}, {})", sql_text(us.as_deref()), sql_text(eu.as_deref()))
        } else {
            format!("IF({pal} AND NOT {ntsc}, {}, {})", sql_text(eu.as_deref()), sql_text(us.as_deref()))
        };
        match groups.iter_mut().find(|g| g.0 == expr) {
            Some(g) => g.1.push(platform.igdb_id()),
            None => groups.push((expr, vec![platform.igdb_id()])),
        }
    }
    let whens: String = groups.iter().filter(|(e, _)| e != "NULL").map(|(e, ids)| format!(" WHEN {{A}}.app_platform_id IN ({}) THEN {e}", ids.iter().map(u16::to_string).collect::<Vec<_>>().join(","))).collect();
    format!("(CASE{whens} ELSE NULL END)")
}

fn templates() -> &'static [(String, String); 4] {
    static T: std::sync::OnceLock<[(String, String); 4]> = std::sync::OnceLock::new();
    T.get_or_init(|| FIELDS.map(|f| (f.to_string(), default_template(f))))
}

/// The SQL `FIELD(...)` order of the sources, unknown ones last.
fn rank_sql(col: &str) -> String {
    let list = SOURCE_RANK.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", ");
    format!("FIELD({col}, {list}) = 0, FIELD({col}, {list})")
}

/// Every resolution some system draws, in either region: the display filter's choices.
pub fn all_resolutions() -> Vec<String> {
    let mut all: Vec<&'static str> = Vec::new();
    for platform in crate::database::connection::all_platforms() {
        for region in ["US", "EU", ""] {
            all.extend(display_default(platform, region).system);
        }
    }
    all.sort_unstable();
    all.dedup();
    display::normalize_resolutions(&all.join(",")).map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_default()
}

/// The lists' sort keys and the field each sorts by.
pub const FIELDS_SORT: [(&str, &str); 4] = [("aspect", "aspect"), ("widescreen", "widescreen"), ("resolutions", "modes"), ("refresh", "refresh")];

/// What a list shows for `field` of the release aliased `alias`: the highest-ranked source's
/// value, else the system default -- the same answer [`values`] gives. `field` must be one of
/// [`FIELDS`] (it goes into the statement).
pub fn effective_sql(field: &str, alias: &str) -> String {
    let Some((_, default)) = templates().iter().find(|(f, _)| f == field) else { return "NULL".to_string() };
    let rd = format!("rdx_{field}");
    format!(
        "COALESCE((SELECT {rd}.value FROM release_display {rd} WHERE {rd}.release_id = {alias}.id AND {rd}.field = '{field}' ORDER BY {} LIMIT 1), {})",
        rank_sql(&format!("{rd}.source")),
        default.replace("{A}", alias)
    )
}

/// Whether `field` of the release aliased `alias` was recorded by a source (Confirmed).
pub fn confirmed_sql(field: &str, alias: &str) -> String {
    format!("EXISTS (SELECT 1 FROM release_display rdc_{field} WHERE rdc_{field}.release_id = {alias}.id AND rdc_{field}.field = '{field}')")
}

/// A sort key for `field`: the value itself, except resolutions, which sort by their
/// highest line count (the last entry, as `normalize_resolutions` keeps them).
pub fn sort_sql(field: &str, alias: &str) -> String {
    let e = effective_sql(field, alias);
    match field {
        "modes" => format!("CAST(SUBSTRING_INDEX(SUBSTRING_INDEX({e}, ',', -1), 'x', -1) AS UNSIGNED)"),
        "refresh" => format!("CAST({e} AS UNSIGNED)"),
        _ => e,
    }
}

/// The display filters of a list query, as conditions on the release aliased `alias` (each
/// starting ` AND `), with their binds. `display_state` is `field:default` or
/// `field:confirmed`.
pub fn filter_sql(alias: &str, aspect: Option<&str>, widescreen: Option<&str>, refresh: Option<&str>, resolution: Option<&str>, scan: Option<&str>, state: Option<&str>) -> Result<(String, Vec<String>)> {
    let mut sql = String::new();
    let mut binds = Vec::new();
    let mut eq = |field: &str, v: Option<&str>| -> Result<()> {
        if let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) {
            sql.push_str(&format!(" AND {} = ?", effective_sql(field, alias)));
            binds.push(display::normalize(field, v).map_err(Error::invalid)?);
        }
        Ok(())
    };
    eq("aspect", aspect)?;
    eq("widescreen", widescreen)?;
    eq("refresh", refresh)?;
    if let Some(r) = resolution.map(str::trim).filter(|v| !v.is_empty()) {
        sql.push_str(&format!(" AND FIND_IN_SET(?, {})", effective_sql("modes", alias)));
        binds.push(display::normalize_resolutions(r).map_err(Error::invalid)?);
    }
    match scan.map(str::trim).filter(|v| !v.is_empty()) {
        None => {}
        Some("p") => sql.push_str(&format!(" AND {} REGEXP 'p(,|$)'", effective_sql("modes", alias))),
        Some("i") => sql.push_str(&format!(" AND {} REGEXP 'i(,|$)'", effective_sql("modes", alias))),
        Some(other) => return Err(Error::invalid(format!("display_scan {other:?}: p or i"))),
    }
    if let Some(s) = state.map(str::trim).filter(|v| !v.is_empty()) {
        let (field, which) = s.split_once(':').unwrap_or((s, "default"));
        if !FIELDS.contains(&field) {
            return Err(Error::invalid(format!("display_state {s:?}: field:default or field:confirmed, field one of {}", FIELDS.join(", "))));
        }
        match which {
            "confirmed" => sql.push_str(&format!(" AND {}", confirmed_sql(field, alias))),
            "default" => sql.push_str(&format!(" AND NOT {}", confirmed_sql(field, alias))),
            other => return Err(Error::invalid(format!("display_state {other:?}: default or confirmed"))),
        }
    }
    Ok((sql, binds))
}

/// `POST /api/v1/catalog/releases/display`: the user's value of one field for releases, or
/// (`value: null`) the user's value taken back -- other sources' or the default show again.
#[derive(Debug, Clone, Deserialize)]
pub struct DisplaySet {
    pub release_ids: Vec<u64>,
    pub field: String,
    #[serde(default)]
    pub value: Option<String>,
}

/// Writes the user's value; returns how many releases it touched.
pub async fn set(app: &App, req: &DisplaySet) -> Result<u64> {
    if !FIELDS.contains(&req.field.as_str()) {
        return Err(Error::invalid(format!("unknown display field {:?}", req.field)));
    }
    if req.release_ids.is_empty() || req.release_ids.len() > 5000 {
        return Err(Error::invalid("give 1-5000 release ids"));
    }
    let value = match &req.value {
        Some(v) => Some(display::normalize(&req.field, v).map_err(Error::invalid)?),
        None => None,
    };
    let mut changed = 0;
    for chunk in req.release_ids.chunks(1000) {
        let list = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        changed += match &value {
            Some(v) => sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO release_display (release_id, field, value, source) SELECT id, ?, ?, 'user' FROM `release` WHERE id IN ({list}) AND deleted = FALSE \
                 ON DUPLICATE KEY UPDATE value = VALUES(value), noted_at = CURRENT_TIMESTAMP"
            )))
            .bind(&req.field)
            .bind(v)
            .execute(&app.pool)
            .await?
            .rows_affected(),
            None => sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM release_display WHERE release_id IN ({list}) AND field = ? AND source = 'user'"))).bind(&req.field).execute(&app.pool).await?.rows_affected(),
        };
    }
    Ok(changed)
}

/// Replaces what `source` says about `field` for `release_id` (never the user's rows).
pub async fn record(app: &App, release_id: u64, field: &str, source: &str, value: &str) -> Result<()> {
    sqlx::query("INSERT INTO release_display (release_id, field, value, source) VALUES (?, ?, ?, ?) ON DUPLICATE KEY UPDATE value = VALUES(value), noted_at = CURRENT_TIMESTAMP")
        .bind(release_id)
        .bind(field)
        .bind(value)
        .bind(source)
        .execute(&app.pool)
        .await?;
    Ok(())
}

/// The platform and region of a release (for its defaults).
pub async fn platform_region(app: &App, release_id: u64) -> Result<Option<(u16, Option<String>)>> {
    Ok(sqlx::query("SELECT app_platform_id, region FROM `release` WHERE id = ?").bind(release_id).fetch_optional(&app.pool).await?.map(|r| Ok::<_, sqlx::Error>((r.try_get("app_platform_id")?, r.try_get("region")?))).transpose()?)
}
