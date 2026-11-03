//! Columns that hold a credential, and the rules every generic table reader follows for them.
//!
//! The routes built for one table already keep their secrets in (`sources` returns
//! `has_token`, never the token). The generic readers are what walk past that: the DB viewer,
//! the backup and the mobile SQLite cache each take any table and every column, so a
//! secret has to be named here once and each of them has to consult this list.
//!
//! - **Out:** an export writes `NULL` in its place ([`projection_sql`]), and the viewer shows
//!   whether one is set but never its value, and refuses to filter or sort on it -- a
//!   `LIKE 'a%'` filter answers one character at a time, so hiding the column from the output
//!   alone would not be enough.
//! - **Back in:** a restore never lets that `NULL` overwrite a secret the database already
//!   holds ([`assignment`]), so restoring a backup over a live install keeps its tokens.
//!
//! A new credential column added by a migration and not listed here fails the tripwire test
//! below, rather than quietly being exported.

/// `(table, column)` for every column holding a credential.
pub const SECRET_COLUMNS: &[(&str, &str)] = &[
    // The bearer token for each peer or main server this machine syncs with.
    ("remote_sources", "token"),
    // Account passwords, as argon2id hashes. Not reversible, but a copy would let someone
    // guess offline at their own pace, so a backup doesn't carry them.
    ("users", "password_hash"),
    // Signed-in devices, as SHA-256 of the token. Masked in the viewer; the table itself is
    // never backed up.
    ("user_tokens", "token_sha256"),
];

/// Columns whose names look like a credential and are not one, each with the reason.
#[cfg(test)]
const NOT_A_SECRET: &[(&str, &str, &str)] = &[];

pub fn is_secret(table: &str, column: &str) -> bool {
    SECRET_COLUMNS.iter().any(|(t, c)| *t == table && *c == column)
}

/// The argument list for `JSON_OBJECT(...)` over `columns`, with each secret column exported
/// as `NULL`. The key stays, so a row still has the same shape wherever it is imported.
pub fn projection_sql(table: &str, columns: &[String]) -> String {
    columns
        .iter()
        .map(|c| if is_secret(table, c) { format!("'{c}', NULL") } else { format!("'{c}', `{c}`") })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `ON DUPLICATE KEY UPDATE` assignment for one column. A secret arrives as `NULL` from any
/// export, so it only replaces the stored value when it actually carries one.
pub fn assignment(table: &str, column: &str) -> String {
    if is_secret(table, column) {
        format!("`{column}` = COALESCE(VALUES(`{column}`), `{column}`)")
    } else {
        format!("`{column}` = VALUES(`{column}`)")
    }
}

/// How the DB viewer selects a column: a secret shows whether it is set, never its value.
pub fn viewer_select(table: &str, column: &str) -> String {
    if is_secret(table, column) {
        format!("IF(`{column}` IS NULL, NULL, '********') AS `{column}`")
    } else {
        format!("CAST(`{column}` AS CHAR) AS `{column}`")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_a_listed_column_is_secret() {
        assert!(is_secret("remote_sources", "token"));
        assert!(!is_secret("remote_sources", "name"));
        // Same column name, different table: not a secret by accident.
        assert!(!is_secret("local_files", "token"));
    }

    #[test]
    fn an_export_nulls_the_secret_and_keeps_the_shape() {
        let sql = projection_sql("remote_sources", &cols(&["id", "name", "token", "role"]));
        assert_eq!(sql, "'id', `id`, 'name', `name`, 'token', NULL, 'role', `role`");
        // Every other table is untouched.
        assert_eq!(projection_sql("settings", &cols(&["key", "value"])), "'key', `key`, 'value', `value`");
    }

    /// Without this, restoring a backup over a live install would blank every peer token,
    /// because the backup carries `NULL` for each of them.
    #[test]
    fn a_restore_never_blanks_a_secret_it_does_not_carry() {
        assert_eq!(assignment("remote_sources", "token"), "`token` = COALESCE(VALUES(`token`), `token`)");
        assert_eq!(assignment("remote_sources", "name"), "`name` = VALUES(`name`)");
    }

    #[test]
    fn the_viewer_shows_that_a_secret_is_set_but_not_what_it_is() {
        let s = viewer_select("remote_sources", "token");
        assert!(s.contains("'********'") && !s.contains("CAST(`token`"), "{s}");
        assert_eq!(viewer_select("remote_sources", "name"), "CAST(`name` AS CHAR) AS `name`");
    }

    /// Every column a migration creates whose name suggests a credential, as `(table, column)`.
    ///
    /// A line-based reading of the migrations rather than a query, so it runs with no
    /// database: `CREATE TABLE` sets the current table, and a column definition is a line whose
    /// first word is a name followed by a type. `ALTER TABLE ... ADD [COLUMN]` is read too.
    fn credential_like_columns() -> Vec<(String, String)> {
        const HINTS: [&str; 4] = ["token", "secret", "passw", "api_key"];
        const TYPES: [&str; 17] = [
            "varchar", "char", "text", "tinytext", "mediumtext", "longtext", "blob", "binary", "varbinary", "json", "int", "bigint", "smallint",
            "tinyint", "boolean", "datetime", "enum",
        ];
        let looks_secret = |c: &str| HINTS.iter().any(|h| c.to_ascii_lowercase().contains(h));
        let ident = |s: &str| s.trim_matches('`').to_ascii_lowercase();

        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let mut files: Vec<_> = std::fs::read_dir(&dir).expect("migrations dir").filter_map(|e| e.ok()).map(|e| e.path()).collect();
        files.sort();
        let mut out = Vec::new();
        for file in files.iter().filter(|p| p.extension().is_some_and(|e| e == "sql")) {
            let text = std::fs::read_to_string(file).expect("read migration");
            let mut table: Option<String> = None;
            for raw in text.lines() {
                let line = raw.split("--").next().unwrap_or("").trim();
                let words: Vec<&str> = line.split_whitespace().collect();
                let upper: Vec<String> = words.iter().map(|w| w.to_ascii_uppercase()).collect();
                if upper.len() >= 3 && upper[0] == "CREATE" && upper[1] == "TABLE" {
                    let name = if upper.get(2).map(String::as_str) == Some("IF") { words.get(5) } else { words.get(2) };
                    table = name.map(|n| ident(n.trim_end_matches('(')));
                    continue;
                }
                if upper.len() >= 3 && upper[0] == "ALTER" && upper[1] == "TABLE" {
                    let t = ident(words[2]);
                    // `ALTER TABLE t ADD [COLUMN] [IF NOT EXISTS] c type`, possibly on one line.
                    if let Some(i) = upper.iter().position(|w| w == "ADD") {
                        let mut j = i + 1;
                        if upper.get(j).map(String::as_str) == Some("COLUMN") {
                            j += 1;
                        }
                        if upper.get(j).map(String::as_str) == Some("IF") {
                            j += 3;
                        }
                        if let Some(c) = words.get(j) {
                            let c = ident(c);
                            if looks_secret(&c) {
                                out.push((t.clone(), c));
                            }
                        }
                    }
                    table = Some(t);
                    continue;
                }
                let Some(t) = table.as_ref() else { continue };
                if words.len() >= 2 {
                    let ty = words[1].to_ascii_lowercase();
                    let ty = ty.split('(').next().unwrap_or("");
                    let first = ident(words[0]);
                    if TYPES.contains(&ty) && looks_secret(&first) {
                        out.push((t.clone(), first));
                    }
                }
            }
        }
        out
    }

    /// The migrations are read correctly: the one credential column that exists today is found.
    #[test]
    fn the_tripwire_finds_the_known_token_column() {
        let found = credential_like_columns();
        assert!(found.contains(&("remote_sources".to_string(), "token".to_string())), "{found:?}");
    }

    /// A migration adding a credential column must also add it to `SECRET_COLUMNS` (or to
    /// `NOT_A_SECRET` with a reason), or every generic reader exports it in plaintext.
    #[test]
    fn every_credential_column_is_classified() {
        for (table, column) in credential_like_columns() {
            let classified = is_secret(&table, &column) || NOT_A_SECRET.iter().any(|(t, c, _)| *t == table && *c == column);
            assert!(classified, "{table}.{column} looks like a credential: add it to SECRET_COLUMNS, or to NOT_A_SECRET with a reason");
        }
    }

    #[test]
    fn every_listed_secret_still_exists() {
        let found = credential_like_columns();
        for (t, c) in SECRET_COLUMNS {
            assert!(found.iter().any(|(ft, fc)| ft == t && fc == c), "{t}.{c} is listed but no migration creates it");
        }
    }
}
