//! The phone's synced copy, answering the browse routes without the server.
//!
//! The file is built by the server or client the phone syncs from (`romexplorer::api::mobile`)
//! and holds the routes' own JSON answers, row by row, beside the few columns filtered and
//! sorted on here. So the screens keep calling `/api/v1/catalog/titles?search=...` and get
//! the same shape whether this answered or the server did.
//!
//! What it answers ([`answer`]): the fixed responses the sync captured (vocabulary,
//! settings, translations, ...), the list routes with the filters a phone uses (search,
//! platforms, where held, a title's releases, an IGDB game's titles), the title and release
//! pages it holds, and the RetroAchievements lists. Anything else -- or a page that wasn't
//! synced -- is `None`, and the caller goes to the server.
//!
//! No Dioxus here: plain functions over a connection, so the tests run anywhere.

use rusqlite::{params_from_iter, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashMap;

pub use romexplorer_types::mobile::{FORMAT, THIS_MACHINE};

/// The list tabs the file can hold.
pub const LIST_TABS: [&str; 7] = [
    "crossplatform",
    "definitive",
    "titles",
    "releases",
    "games",
    "files",
    "contents",
];

/// Opens the file read-only and checks it is one this build understands.
pub fn open(path: &std::path::Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    let format: Option<String> = conn
        .query_row(
            "SELECT value FROM _mobile_meta WHERE key = 'format'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    match format.and_then(|f| f.parse::<u32>().ok()) {
        Some(FORMAT) => Ok(conn),
        Some(other) => Err(format!(
            "synced copy is format {other}, this app reads {FORMAT}: sync again"
        )),
        None => Err("not a synced copy".to_string()),
    }
}

/// One `meta` value.
pub fn meta(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM _mobile_meta WHERE key = ?1",
        [key],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// Stores a route's answer for later (the sync's fixed responses).
pub fn put_fixed(conn: &Connection, path: &str, body: &str) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO _mobile_fixed (path, body) VALUES (?1, ?2)",
        [path, body],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// A cover or RA image: `media/<id>` or `ra/<game>/boxart`.
pub fn image(conn: &Connection, key: &str) -> Option<(String, Vec<u8>)> {
    conn.query_row(
        "SELECT mime, bytes FROM _mobile_images WHERE key = ?1",
        [key],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .ok()
    .flatten()
}

/// Replaces the RetroAchievements rows (RA progress synced on its own).
pub fn replace_ra(conn: &mut Connection, rows: &[Value]) -> Result<usize, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    // Which machines hold each game came with the full sync (an RA-only sync doesn't know
    // the phone's machine choices): kept across the swap.
    let held: HashMap<i64, String> = {
        let mut stmt = tx
            .prepare("SELECT game_id, held FROM _mobile_ra")
            .map_err(|e| e.to_string())?;
        let map = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        map
    };
    tx.execute("DELETE FROM _mobile_ra", [])
        .map_err(|e| e.to_string())?;
    for (i, r) in rows.iter().enumerate() {
        let game = r.get("game_id").and_then(Value::as_i64).unwrap_or(0);
        tx.execute(
            "INSERT OR REPLACE INTO _mobile_ra (game_id, ord, title, console_id, local, held, num_awarded, highest_award_kind, most_recent_awarded, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                game,
                i as i64,
                r.get("title").and_then(Value::as_str).unwrap_or_default().to_lowercase(),
                r.get("console_id").and_then(Value::as_i64),
                r.get("local").and_then(Value::as_bool).unwrap_or(false) as i64,
                held.get(&game).cloned().unwrap_or_default(),
                r.get("num_awarded").and_then(Value::as_i64).unwrap_or(0),
                r.get("highest_award_kind").and_then(Value::as_str).unwrap_or_default(),
                r.get("most_recent_awarded").and_then(Value::as_str),
                r.to_string(),
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(rows.len())
}

/// Replaces every row of one of the MariaDB-shaped tables with `rows` (JSON objects keyed by
/// column, as the server's export gives them). A table the copy doesn't have is left alone.
pub fn replace_table_rows(
    conn: &mut Connection,
    table: &str,
    rows: &[Value],
) -> Result<usize, String> {
    let columns: Vec<String> = {
        let mut stmt = conn
            .prepare(&format!(
                "PRAGMA table_info(\"{}\")",
                table.replace('"', "")
            ))
            .map_err(|e| e.to_string())?;
        let names = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        names
    };
    if columns.is_empty() {
        return Ok(0);
    }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(&format!("DELETE FROM \"{table}\""), [])
        .map_err(|e| e.to_string())?;
    let names = columns
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let marks = vec!["?"; columns.len()].join(", ");
    {
        let mut stmt = tx
            .prepare(&format!(
                "INSERT OR REPLACE INTO \"{table}\" ({names}) VALUES ({marks})"
            ))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let values: Vec<rusqlite::types::Value> =
                columns.iter().map(|c| sql_value(row.get(c))).collect();
            stmt.execute(params_from_iter(values.iter()))
                .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(rows.len())
}

/// A JSON value as SQLite stores the server's rows: numbers as numbers, nested JSON as its
/// text (the same as the server's own export, `sqlite::copy_table`).
fn sql_value(v: Option<&Value>) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match v {
        None | Some(Value::Null) => V::Null,
        Some(Value::Bool(b)) => V::Integer(*b as i64),
        Some(Value::Number(n)) => n
            .as_i64()
            .map(V::Integer)
            .or_else(|| n.as_f64().map(V::Real))
            .unwrap_or(V::Null),
        Some(Value::String(s)) => V::Text(s.clone()),
        Some(other) => V::Text(other.to_string()),
    }
}

/// The ids of the main server among the machines the file was synced with, for `view=server`.
fn main_ids(conn: &Connection) -> Vec<String> {
    let machines: Value = meta(conn, "machines")
        .and_then(|m| serde_json::from_str(&m).ok())
        .unwrap_or(Value::Null);
    machines
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|m| m.get("role").and_then(Value::as_str) == Some("main"))
                .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn split(path: &str) -> (&str, HashMap<String, String>) {
    let (p, q) = path.split_once('?').unwrap_or((path, ""));
    let query = q
        .split('&')
        .filter(|kv| !kv.is_empty())
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            Some((decode(k), decode(v)))
        })
        .collect();
    (p, query)
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                Ok(b) => {
                    out.push(b);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Query keys the list answer understands. With any other key the answer would be wrong
/// rather than merely incomplete, so the caller asks the server instead (and only falls back
/// to this, ignoring those keys, when the server can't be reached: see [`answer`]).
const LIST_KEYS: [&str; 11] = [
    "search",
    "machine",
    "platform",
    "platforms",
    "view",
    "title_id",
    "igdb",
    "limit",
    "offset",
    "sort",
    "desc",
];

fn list_tab(path: &str) -> Option<&'static str> {
    let tab = path.strip_prefix("/api/v1/catalog/")?;
    LIST_TABS.iter().copied().find(|t| *t == tab)
}

fn has_tab(conn: &Connection, tab: &str) -> bool {
    let synced: Option<String> = meta(conn, "options");
    let opts: Value = synced
        .and_then(|o| serde_json::from_str(&o).ok())
        .unwrap_or(Value::Null);
    opts.pointer(&format!("/tabs/{tab}"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The answer for a GET of `path` from the synced copy, or `None` when it has none and the
/// server should be asked. `strict`: refuse list queries with keys this can't apply (the
/// server is reachable, so it can answer them properly).
pub fn answer(conn: &Connection, path: &str, strict: bool) -> Option<Result<String, String>> {
    if let Some(body) = conn
        .query_row(
            "SELECT body FROM _mobile_fixed WHERE path = ?1",
            [path],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
    {
        return Some(Ok(body));
    }
    let (route, q) = split(path);
    if let Some(tab) = list_tab(route) {
        if !has_tab(conn, tab) {
            return None;
        }
        if strict && q.keys().any(|k| !LIST_KEYS.contains(&k.as_str())) {
            return None;
        }
        return Some(list(conn, tab, &q));
    }
    for (prefix, kind) in [
        ("/api/v1/catalog/titles/", "title"),
        ("/api/v1/catalog/releases/", "release"),
    ] {
        if let Some(id) = route
            .strip_prefix(prefix)
            .and_then(|i| i.parse::<i64>().ok())
        {
            let body = conn
                .query_row(
                    "SELECT body FROM _mobile_pages WHERE kind = ?1 AND id = ?2",
                    rusqlite::params![kind, id],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .ok()
                .flatten();
            return body.map(Ok);
        }
    }
    if route == "/api/v1/ra/games" && has_tab(conn, "ra") {
        if strict
            && q.keys().any(|k| {
                ![
                    "list",
                    "q",
                    "console_id",
                    "limit",
                    "offset",
                    "source",
                    "include_beaten",
                ]
                .contains(&k.as_str())
            })
        {
            return None;
        }
        return Some(ra(conn, &q));
    }
    None
}

fn page_of(q: &HashMap<String, String>) -> (i64, i64) {
    let limit = q
        .get("limit")
        .and_then(|l| l.parse::<i64>().ok())
        .unwrap_or(50)
        .clamp(1, 500);
    let offset = q
        .get("offset")
        .and_then(|o| o.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0);
    (limit, offset)
}

fn like(s: &str) -> String {
    let escaped = s
        .trim()
        .to_lowercase()
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

fn list(conn: &Connection, tab: &str, q: &HashMap<String, String>) -> Result<String, String> {
    let mut wh = vec!["tab = ?".to_string()];
    let mut args: Vec<String> = vec![tab.to_string()];
    if let Some(s) = q.get("search").filter(|s| !s.trim().is_empty()) {
        wh.push("name LIKE ? ESCAPE '\\'".into());
        args.push(like(s));
    }
    let mut platforms: Vec<String> = q
        .get("platforms")
        .map(|p| {
            p.split(',')
                .filter(|x| !x.trim().is_empty())
                .map(|x| x.trim().to_string())
                .collect()
        })
        .unwrap_or_default();
    if let Some(p) = q.get("platform").filter(|p| !p.is_empty()) {
        platforms.push(p.clone());
    }
    if !platforms.is_empty() {
        wh.push(format!(
            "({})",
            vec!["platforms LIKE ?"; platforms.len()].join(" OR ")
        ));
        args.extend(platforms.iter().map(|p| format!("%,{p},%")));
    }
    if let Some(t) = q.get("title_id").filter(|t| !t.is_empty()) {
        wh.push("title_id = ?".into());
        args.push(t.clone());
    }
    if let Some(g) = q.get("igdb").filter(|g| !g.is_empty()) {
        wh.push("igdb = ?".into());
        args.push(g.clone());
    }
    let view = q.get("view").map(String::as_str).unwrap_or("");
    let mut held_first = matches!(view, "" | "collection_local_first" | "local_ra_local_first");
    match view {
        "local" => {
            wh.push("held LIKE ?".into());
            args.push(format!("%,{THIS_MACHINE},%"));
        }
        "held" => wh.push("held <> ''".into()),
        // Not held on any synced machine: the catalogue's titles not owned yet.
        "catalog" => wh.push("held = ''".into()),
        "server" | "missing_on_server" => {
            let mains = main_ids(conn);
            let cond = if mains.is_empty() {
                "0".to_string()
            } else {
                format!("({})", vec!["held LIKE ?"; mains.len()].join(" OR "))
            };
            if view == "server" {
                wh.push(cond);
            } else {
                wh.push(format!("held <> '' AND NOT {cond}"));
            }
            args.extend(mains.iter().map(|m| format!("%,{m},%")));
        }
        v if v.starts_with("on:") => {
            wh.push("held LIKE ?".into());
            args.push(format!("%,{},%", &v[3..]));
        }
        _ => {}
    }
    machine_filter(q, &mut wh, &mut args);
    // A page's own list of a title's releases is in the order the server gave, held or not.
    if q.contains_key("title_id") || q.contains_key("igdb") {
        held_first = false;
    }
    let desc = q.get("desc").is_some_and(|d| d == "true" || d == "1");
    let order = match (q.get("sort").map(String::as_str), held_first) {
        (Some("name"), _) => format!("name {}", if desc { "DESC" } else { "ASC" }),
        (_, true) => "(held = '') ASC, ord ASC".to_string(),
        _ => "ord ASC".to_string(),
    };
    let (limit, offset) = page_of(q);
    let cond = wh.join(" AND ");
    let total: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM _mobile_rows WHERE {cond}"),
            params_from_iter(args.iter()),
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare(&format!("SELECT body FROM _mobile_rows WHERE {cond} ORDER BY {order} LIMIT {limit} OFFSET {offset}")).map_err(|e| e.to_string())?;
    let items: Vec<String> = stmt
        .query_map(params_from_iter(args.iter()), |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();
    // The bodies are JSON already: spliced in, not parsed and re-serialised.
    Ok(format!(
        "{{\"items\":[{}],\"total\":{total},\"limit\":{limit},\"offset\":{offset}}}",
        items.join(",")
    ))
}

/// The Machine filter (`machine=`): held on that synced machine, or on this phone, which
/// holds nothing yet.
fn machine_filter(q: &HashMap<String, String>, wh: &mut Vec<String>, args: &mut Vec<String>) {
    match q.get("machine").map(String::as_str) {
        None | Some("") => {}
        Some("phone") => wh.push("0".into()),
        Some(m) => {
            wh.push("held LIKE ?".into());
            args.push(format!("%,{m},%"));
        }
    }
}

/// The RetroAchievements lists, with the server's own conditions (`api::ra::list_condition`).
fn ra_condition(list: &str, include_beaten: bool) -> &'static str {
    match list {
        "mastery" => "highest_award_kind = 'mastered'",
        "complete" => "highest_award_kind = 'completed'",
        "beat" => "highest_award_kind IN ('beaten-hardcore', 'beaten-softcore')",
        "in_progress" if include_beaten => {
            "num_awarded > 0 AND highest_award_kind NOT IN ('mastered', 'completed')"
        }
        "in_progress" => "num_awarded > 0 AND highest_award_kind = ''",
        "to_play" => "num_awarded = 0 AND highest_award_kind = ''",
        _ => "1",
    }
}

const RA_LISTS: [&str; 6] = [
    "titles",
    "mastery",
    "complete",
    "beat",
    "in_progress",
    "to_play",
];

fn ra(conn: &Connection, q: &HashMap<String, String>) -> Result<String, String> {
    let list = q
        .get("list")
        .map(String::as_str)
        .filter(|l| RA_LISTS.contains(l))
        .unwrap_or("titles");
    let include_beaten = q
        .get("include_beaten")
        .is_none_or(|b| b == "true" || b == "1");
    let mut wh: Vec<String> = Vec::new();
    let mut args: Vec<String> = Vec::new();
    if let Some(s) = q.get("q").filter(|s| !s.trim().is_empty()) {
        wh.push("title LIKE ? ESCAPE '\\'".into());
        args.push(like(s));
    }
    if let Some(c) = q.get("console_id").filter(|c| !c.is_empty()) {
        wh.push("console_id = ?".into());
        args.push(c.clone());
    }
    let local_only = q.get("source").is_some_and(|s| s == "local");
    if local_only {
        wh.push("local = 1".into());
    }
    match q.get("view").map(String::as_str) {
        Some("held") => wh.push("held <> ''".into()),
        Some("catalog") => wh.push("held = ''".into()),
        _ => {}
    }
    machine_filter(q, &mut wh, &mut args);
    let base = if wh.is_empty() {
        "1".to_string()
    } else {
        wh.join(" AND ")
    };
    let mut counts = serde_json::Map::new();
    for l in RA_LISTS {
        let n: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM _mobile_ra WHERE {base} AND {}",
                    ra_condition(l, include_beaten)
                ),
                params_from_iter(args.iter()),
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        counts.insert(l.to_string(), json!(n));
    }
    let cond = format!("{base} AND {}", ra_condition(list, include_beaten));
    let total = counts.get(list).and_then(Value::as_i64).unwrap_or(0);
    // The server's defaults: by title for the full list and to-play, most recent unlock first
    // for the others; held games first throughout.
    let order = if list == "titles" || list == "to_play" {
        "local DESC, title ASC"
    } else {
        "local DESC, most_recent_awarded DESC, title ASC"
    };
    let (limit, offset) = page_of(q);
    let mut stmt = conn.prepare(&format!("SELECT body FROM _mobile_ra WHERE {cond} ORDER BY {order} LIMIT {limit} OFFSET {offset}")).map_err(|e| e.to_string())?;
    let items: Vec<String> = stmt
        .query_map(params_from_iter(args.iter()), |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();
    Ok(format!(
        "{{\"items\":[{}],\"total\":{total},\"tab_counts\":{},\"source\":\"{}\",\"include_beaten\":{include_beaten}}}",
        items.join(","),
        Value::Object(counts),
        if local_only { "local" } else { "all" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The schema `romexplorer::api::mobile` writes, kept in step by hand: the server crate
    /// can't be a dependency of this one.
    const SCHEMA: &[&str] = &[
        "CREATE TABLE _mobile_meta (key TEXT PRIMARY KEY, value TEXT)",
        "CREATE TABLE _mobile_fixed (path TEXT PRIMARY KEY, body TEXT NOT NULL)",
        "CREATE TABLE _mobile_rows (tab TEXT NOT NULL, key TEXT NOT NULL, ord INTEGER NOT NULL, id INTEGER, title_id INTEGER, igdb TEXT, platforms TEXT NOT NULL DEFAULT '', name TEXT NOT NULL DEFAULT '', held TEXT NOT NULL DEFAULT '', body TEXT NOT NULL, PRIMARY KEY (tab, key))",
        "CREATE TABLE _mobile_pages (kind TEXT NOT NULL, id INTEGER NOT NULL, body TEXT NOT NULL, PRIMARY KEY (kind, id))",
        "CREATE TABLE _mobile_images (key TEXT PRIMARY KEY, mime TEXT NOT NULL, bytes BLOB NOT NULL)",
        "CREATE TABLE _mobile_ra (game_id INTEGER PRIMARY KEY, ord INTEGER NOT NULL, title TEXT NOT NULL DEFAULT '', console_id INTEGER, local INTEGER NOT NULL DEFAULT 0, held TEXT NOT NULL DEFAULT '', num_awarded INTEGER NOT NULL DEFAULT 0, highest_award_kind TEXT NOT NULL DEFAULT '', most_recent_awarded TEXT, body TEXT NOT NULL)",
    ];

    fn fixture() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        for s in SCHEMA {
            c.execute(s, []).unwrap();
        }
        c.execute(
            "INSERT INTO _mobile_meta VALUES ('format', ?1)",
            [FORMAT.to_string()],
        )
        .unwrap();
        c.execute(
            "INSERT INTO _mobile_meta VALUES ('options', ?1)",
            [r#"{"tabs":{"titles":true,"releases":true,"ra":true,"games":false}}"#],
        )
        .unwrap();
        c.execute(
            "INSERT INTO _mobile_meta VALUES ('machines', ?1)",
            [r#"[{"id":"local","role":"this"},{"id":"3","role":"main"}]"#],
        )
        .unwrap();
        let rows = [
            ("titles", "a", 1, 10, ",5,", "alpha quest", ",local,"),
            ("titles", "b", 2, 11, ",5,7,", "beta saga", ""),
            ("titles", "c", 3, 12, ",7,", "gamma quest", ",3,"),
        ];
        for (tab, key, ord, id, plats, name, held) in rows {
            c.execute(
                "INSERT INTO _mobile_rows (tab, key, ord, id, title_id, platforms, name, held, body) VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![tab, key, ord, id, plats, name, held, json!({ "id": id, "name": name }).to_string()],
            )
            .unwrap();
        }
        c.execute(
            "INSERT INTO _mobile_pages VALUES ('title', 10, '{\"id\":10}')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO _mobile_fixed VALUES ('/api/v1/vocabulary', '{\"v\":1}')",
            [],
        )
        .unwrap();
        c
    }

    fn ids(body: &str) -> Vec<u64> {
        let v: Value = serde_json::from_str(body).unwrap();
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_u64().unwrap())
            .collect()
    }

    #[test]
    fn lists_filter_and_put_held_first() {
        let c = fixture();
        let all = answer(&c, "/api/v1/catalog/titles?limit=60", true)
            .unwrap()
            .unwrap();
        assert_eq!(
            ids(&all),
            vec![10, 12, 11],
            "held first, then the server's order"
        );
        let quest = answer(&c, "/api/v1/catalog/titles?search=QUEST", true)
            .unwrap()
            .unwrap();
        assert_eq!(ids(&quest), vec![10, 12]);
        let p7 = answer(
            &c,
            "/api/v1/catalog/titles?platforms=7&view=collection",
            true,
        )
        .unwrap()
        .unwrap();
        assert_eq!(ids(&p7), vec![11, 12]);
        let here = answer(&c, "/api/v1/catalog/titles?view=local", true)
            .unwrap()
            .unwrap();
        assert_eq!(ids(&here), vec![10]);
        let main = answer(&c, "/api/v1/catalog/titles?view=server", true)
            .unwrap()
            .unwrap();
        assert_eq!(ids(&main), vec![12]);
        // Held and Machine are separate filters, and combine.
        let not_held = answer(&c, "/api/v1/catalog/titles?view=catalog", true).unwrap().unwrap();
        assert_eq!(ids(&not_held), vec![11]);
        let on_main = answer(&c, "/api/v1/catalog/titles?view=held&machine=3", true).unwrap().unwrap();
        assert_eq!(ids(&on_main), vec![12]);
        let on_phone = answer(&c, "/api/v1/catalog/titles?machine=phone", true).unwrap().unwrap();
        assert!(ids(&on_phone).is_empty(), "the phone holds nothing yet");
        let page: Value = serde_json::from_str(
            &answer(&c, "/api/v1/catalog/titles?limit=1&offset=1", true)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(page["total"], 3);
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn unsynced_tabs_unknown_filters_and_missing_pages_go_to_the_server() {
        let c = fixture();
        assert!(
            answer(&c, "/api/v1/catalog/games", true).is_none(),
            "games were not synced"
        );
        assert!(
            answer(&c, "/api/v1/catalog/titles?genres=RPG", true).is_none(),
            "a filter this can't apply"
        );
        assert!(
            answer(&c, "/api/v1/catalog/titles?genres=RPG", false).is_some(),
            "offline: the best it can do"
        );
        assert!(
            answer(&c, "/api/v1/catalog/titles/11", true).is_none(),
            "page not synced"
        );
        assert_eq!(
            answer(&c, "/api/v1/catalog/titles/10", true)
                .unwrap()
                .unwrap(),
            "{\"id\":10}"
        );
        assert_eq!(
            answer(&c, "/api/v1/vocabulary", true).unwrap().unwrap(),
            "{\"v\":1}"
        );
        assert!(answer(&c, "/api/v1/jobs", true).is_none());
    }

    #[test]
    fn ra_lists_follow_the_servers_conditions() {
        let mut c = fixture();
        let rows = vec![
            json!({ "game_id": 1, "title": "One", "local": true, "num_awarded": 10, "highest_award_kind": "mastered", "most_recent_awarded": "2026-01-01 00:00:00" }),
            json!({ "game_id": 2, "title": "Two", "local": false, "num_awarded": 3, "highest_award_kind": null }),
            json!({ "game_id": 3, "title": "Three", "local": false, "num_awarded": 0 }),
        ];
        assert_eq!(replace_ra(&mut c, &rows).unwrap(), 3);
        let v: Value = serde_json::from_str(
            &answer(&c, "/api/v1/ra/games?list=titles", true)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(v["total"], 3);
        assert_eq!(v["tab_counts"]["mastery"], 1);
        assert_eq!(v["tab_counts"]["in_progress"], 1);
        assert_eq!(v["tab_counts"]["to_play"], 1);
        assert_eq!(v["items"][0]["game_id"], 1, "held first");
    }

    #[test]
    fn real_tables_take_the_servers_rows() {
        let mut c = fixture();
        c.execute("CREATE TABLE ra_user_progress (ra_user TEXT, game_id INTEGER, num_awarded INTEGER, extra TEXT, PRIMARY KEY (ra_user, game_id))", []).unwrap();
        c.execute(
            "INSERT INTO ra_user_progress VALUES ('old', 1, 1, NULL)",
            [],
        )
        .unwrap();
        let rows = vec![
            json!({ "ra_user": "me", "game_id": 5, "num_awarded": 3, "extra": { "a": 1 }, "not_a_column": true }),
        ];
        assert_eq!(
            replace_table_rows(&mut c, "ra_user_progress", &rows).unwrap(),
            1
        );
        let (user, n, extra): (String, i64, String) = c
            .query_row(
                "SELECT ra_user, num_awarded, extra FROM ra_user_progress",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((user.as_str(), n, extra.as_str()), ("me", 3, "{\"a\":1}"));
        assert_eq!(
            replace_table_rows(&mut c, "no_such_table", &rows).unwrap(),
            0
        );
    }

    #[test]
    fn query_strings_decode() {
        let (p, q) = split("/api/v1/catalog/titles?search=hoge%20quest&platforms=5%2C7");
        assert_eq!(p, "/api/v1/catalog/titles");
        assert_eq!(q["search"], "hoge quest");
        assert_eq!(q["platforms"], "5,7");
        assert_eq!(like("50%_off"), "%50\\%\\_off%");
    }
}
