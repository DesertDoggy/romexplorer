//! Typed access to the `settings` table: every known key with its description, plus
//! validation of the shapes the known keys must have.

use chrono::NaiveDateTime;
use serde::Serialize;
use serde_json::Value;

use crate::database::providers::Provider;
use crate::database::settings as db_settings;

use super::{App, Error, Result};

#[derive(Debug, Clone, Serialize)]
pub struct SettingEntry {
    pub key: String,
    pub value: Option<Value>,
    pub description: Option<String>,
    /// What the form shows behind the setting's "!" instead of `description`
    /// (`db_settings::EXPLAIN`); empty when the description is it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub explain: Vec<String>,
    /// The value an unset key behaves as, where that depends on the build
    /// (`db_settings::platform_default`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    pub updated_at: Option<NaiveDateTime>,
    /// The form's section for it (`db_settings::SETTING_GROUPS`) and that section's heading.
    pub group: String,
    pub group_label: String,
}

fn group_fields(key: &str) -> (String, String) {
    let id = db_settings::group_of(key);
    let label = db_settings::SETTING_GROUPS.iter().find(|(g, _)| *g == id).map_or("Other", |(_, l)| *l);
    (id.to_string(), label.to_string())
}

/// Every setting, section by section (`db_settings::SETTING_GROUPS` order); within a section,
/// known keys in their declared order, then any other stored keys.
pub async fn list(app: &App) -> Result<Vec<SettingEntry>> {
    let stored = db_settings::list_all(&app.pool).await?;
    let mut out: Vec<SettingEntry> = db_settings::KNOWN_KEYS
        .iter()
        .map(|(key, description)| {
            let found = stored.iter().find(|(k, _, _)| k == key);
            let (group, group_label) = group_fields(key);
            SettingEntry {
                key: key.to_string(),
                value: found.map(|(_, v, _)| v.clone()),
                description: Some(description.to_string()),
                explain: db_settings::explain(key).iter().map(|p| p.to_string()).collect(),
                default: db_settings::platform_default(key),
                updated_at: found.map(|(_, _, t)| *t), group, group_label }
        })
        .collect();
    for (key, value, updated_at) in stored {
        if !db_settings::KNOWN_KEYS.iter().any(|(k, _)| *k == key) {
            let (group, group_label) = group_fields(&key);
            out.push(SettingEntry { key, value: Some(value), description: None, explain: Vec::new(), default: None, updated_at: Some(updated_at), group, group_label });
        }
    }
    let rank = |g: &str| db_settings::SETTING_GROUPS.iter().position(|(id, _)| *id == g).unwrap_or(usize::MAX);
    out.sort_by_key(|e| rank(&e.group));
    Ok(out)
}

/// `dat_sync_platforms` as it stands now: `{"mode", "platforms": [ids] | null, "extras":
/// [keys] | null, "extras_offered": [{key, label}]}`, the
/// ids worked out for the scan-based modes (null = every platform). What the provider
/// download's platform checkboxes start from.
pub async fn dat_platforms(app: &App) -> Result<Value> {
    let (mode, ids) = db_settings::dat_sync_platforms(&app.pool).await?;
    let extras = db_settings::dat_sync_extras(&app.pool).await?;
    let offered: Vec<Value> = crate::database::dat_download::DAT_EXTRAS.iter().map(|(k, l)| serde_json::json!({ "key": k, "label": l })).collect();
    Ok(serde_json::json!({ "mode": mode, "platforms": ids, "extras": extras, "extras_offered": offered }))
}

pub async fn get(app: &App, key: &str) -> Result<Option<Value>> {
    Ok(db_settings::get(&app.pool, key).await?)
}

pub async fn set(app: &App, key: &str, value: Value) -> Result<()> {
    validate(key, &value)?;
    if key == db_settings::KEY_UI_LANGUAGE {
        let offered = super::i18n::languages(app);
        if !value.as_str().is_some_and(|code| offered.iter().any(|l| l == code)) {
            return Err(Error::invalid(format!("ui_language must be one of the languages with UI text: {}", offered.join(", "))));
        }
    }
    db_settings::set(&app.pool, key, &value).await?;
    if key == db_settings::KEY_CHD_READER {
        db_settings::apply_chd_reader(&app.pool).await?;
    }
    if key == db_settings::KEY_SCAN_PKG_TITLES {
        db_settings::apply_scan_pkg_titles(&app.pool).await?;
    }
    // Which release of a title comes first: a title combined from several IGDB games
    // follows its first release's game.
    if [db_settings::KEY_REGION_PRIORITY, db_settings::KEY_LANGUAGE_PRIORITY, db_settings::KEY_RELEASE_EDITION_ORDER].contains(&key) {
        super::titles::sync_lead_igdb(app, None).await?;
    }
    // Which version of each family is its definitive one.
    if [db_settings::KEY_PLATFORM_PRIORITY, crate::database::title_family::KEY_PC_MAX_AGE, crate::database::release_buckets::KEY_UNLICENSED].contains(&key) {
        super::definitive::refresh_all_later(app);
    }
    // The versions are fetched by the title pass: queue one so the change shows.
    if key == crate::database::title_family::KEY_FETCH_VERSIONS && value.as_str() != Some("off") {
        if let Err(e) = super::jobs::enqueue_unless_queued(app, &super::jobs::JobRequest::TitleDedupe).await {
            log::warn!("definitive_fetch_versions: could not queue the title pass: {e}");
        }
    }
    Ok(())
}

pub async fn delete(app: &App, key: &str) -> Result<bool> {
    guard_identity(key)?;
    let gone = db_settings::delete(&app.pool, key).await?;
    if key == db_settings::KEY_SCAN_PKG_TITLES {
        db_settings::apply_scan_pkg_titles(&app.pool).await?;
    }
    if [db_settings::KEY_PLATFORM_PRIORITY, crate::database::title_family::KEY_PC_MAX_AGE, crate::database::release_buckets::KEY_UNLICENSED].contains(&key) {
        super::definitive::refresh_all_later(app);
    }
    Ok(gone)
}

/// The three keys that are not settings in the ordinary sense, refused on both `set` and
/// `delete`. `instance_mode` and `catalog_from` have their own endpoint (`PUT /api/v1/mode`,
/// `api::sources::set_mode`) because changing them can mean replacing the catalogue, and that
/// endpoint is where the consent for it travels; a raw write here would skip the check, the
/// clearing and the warning, and leave the machine claiming client mode with a locally built
/// catalogue still in place -- the mixed state the setting exists to make unrepresentable.
/// `instance_id` is generated once and is what "is this my own data coming back" is decided
/// by; deleting it would mint a new identity on the next read.
fn guard_identity(key: &str) -> Result<()> {
    match key {
        db_settings::KEY_INSTANCE_MODE | db_settings::KEY_CATALOG_FROM => Err(Error::invalid(format!(
            "{key} is not settable here; use PUT /api/v1/mode, which carries the catalogue-replacement consent this write would skip"
        ))),
        db_settings::KEY_INSTANCE_ID => Err(Error::invalid("instance_id is this installation's identity, generated once; it cannot be set or removed")),
        _ => Ok(()),
    }
}

fn string_list(value: &Value, what: &str) -> Result<Vec<String>> {
    value
        .as_array()
        .ok_or_else(|| Error::invalid(format!("{what} must be an array of strings")))?
        .iter()
        .map(|v| v.as_str().map(str::to_string).ok_or_else(|| Error::invalid(format!("{what} must be an array of strings"))))
        .collect()
}

pub fn validate(key: &str, value: &Value) -> Result<()> {
    if key.trim().is_empty() || key.len() > 64 {
        return Err(Error::invalid("setting key must be 1-64 characters"));
    }
    guard_identity(key)?;
    match key {
        db_settings::KEY_DAT_SYNC_PLATFORMS => {
            let mode = value.get("mode").and_then(Value::as_str).unwrap_or("");
            if !db_settings::DAT_SYNC_PLATFORM_MODES.contains(&mode) {
                return Err(Error::invalid("dat_sync_platforms.mode must be \"all\", \"scanned\", \"scanned_strict\" or \"manual\""));
            }
            if let Some(list) = value.get("platforms") {
                if !list.as_array().is_some_and(|a| a.iter().all(|x| x.as_u64().is_some_and(|n| n <= u16::MAX as u64))) {
                    return Err(Error::invalid("dat_sync_platforms.platforms must be an array of platform ids"));
                }
            }
            if let Some(list) = value.get("extras").filter(|e| !e.is_null()) {
                if !list.as_array().is_some_and(|a| a.iter().all(|x| x.as_str().is_some_and(|k| crate::database::dat_download::DAT_EXTRAS.iter().any(|(e, _)| *e == k)))) {
                    return Err(Error::invalid("dat_sync_platforms.extras must be an array of: audio, video, image, document, source, theme, other"));
                }
            }
        }
        crate::api::ai::KEY_PROVIDER => {
            if !value.as_str().is_some_and(|v| crate::api::ai::PROVIDERS.contains(&v)) {
                return Err(Error::invalid(format!("{key} must be one of {}", crate::api::ai::PROVIDERS.join(", "))));
            }
        }
        crate::api::ai::KEY_ASK_FIRST => {
            if !value.as_str().is_some_and(|v| crate::api::ai::ASK_LINKS.iter().any(|(id, _)| *id == v)) {
                return Err(Error::invalid(format!("{key} must be google, claude, chatgpt or perplexity")));
            }
        }
        crate::api::ai::KEY_CACHE_DAYS => {
            if !(value.is_null() || value.as_u64().is_some_and(|d| d <= 36_500)) {
                return Err(Error::invalid(format!("{key} must be a number of days (0: keep)")));
            }
        }
        crate::api::ai::KEY_PRICES => {
            if serde_json::from_value::<std::collections::BTreeMap<String, crate::api::ai::Price>>(value.clone()).is_err() {
                return Err(Error::invalid(format!("{key} must be {{\"model\": {{\"in\": <$ per M tokens>, \"out\": <$ per M tokens>}}}}")));
            }
        }
        k if k.starts_with("ai_model_") => {
            if !value.as_str().is_some_and(|v| v.len() <= 96 && !v.contains(char::is_whitespace)) {
                return Err(Error::invalid(format!("{key} must be a model id")));
            }
        }
        db_settings::KEY_CHD_READER => {
            if !value.as_str().is_some_and(|v| crate::rom::chd_info::ChdReaderMode::from_name(v).is_some()) {
                return Err(Error::invalid("chd_reader must be \"checked\", \"chdman\" or \"rust\""));
            }
        }
        db_settings::KEY_MEDIA_SYNC_AFTER_PULL | db_settings::KEY_MEDIA_PROVIDER_FALLBACK | db_settings::KEY_SEARCH_TRANSLATIONS | super::artwork::KEY_EXPORT_PREFER_REGION => {
            if !value.is_boolean() {
                return Err(Error::invalid(format!("{key} must be true or false")));
            }
        }
        db_settings::KEY_PROVIDER_PRIORITY => {
            for name in string_list(value, key)? {
                if Provider::from_name(&name).is_none() {
                    return Err(Error::invalid(format!("unknown provider {name:?}")));
                }
            }
        }
        db_settings::KEY_SYNC_SCOPE => {
            let map = value.as_object().ok_or_else(|| Error::invalid("provider_sync_scope must be an object provider -> scope"))?;
            for (provider, scope) in map {
                if Provider::from_name(provider).is_none() {
                    return Err(Error::invalid(format!("unknown provider {provider:?}")));
                }
                if serde_json::from_value::<db_settings::SyncScope>(scope.clone()).is_err() {
                    return Err(Error::invalid(format!("bad scope for {provider}: expected single|list|full|by_platform|by_keyword")));
                }
            }
        }
        db_settings::KEY_RAW_RETENTION => {
            if serde_json::from_value::<db_settings::RawRetention>(value.clone()).is_err() {
                return Err(Error::invalid("raw_response_retention must be keep_all|keep_latest|discard"));
            }
        }
        db_settings::KEY_CACHE_TTL_DAYS => {
            if !value.as_u64().is_some_and(|n| n > 0) {
                return Err(Error::invalid("cache_ttl_days must be a positive integer"));
            }
        }
        db_settings::KEY_TITLE_DISPLAY_MODE => {
            let ok = matches!(value.as_str(), Some("english") | Some("local")) || value.get("languages").map(|l| string_list(l, "languages")).transpose()?.is_some();
            if !ok {
                return Err(Error::invalid("title_display_mode must be \"english\", \"local\" or {\"languages\": [...]}"));
            }
        }
        db_settings::KEY_SCAN_PARALLEL | db_settings::KEY_COPY_PARALLEL => {
            crate::api::parallel::ParallelSetting::parse(value).map_err(|e| Error::invalid(format!("{key}: {e}")))?;
        }
        db_settings::KEY_SCAN_HASHES => {
            let map = value.as_object().ok_or_else(|| Error::invalid("scan_hashes must be an object algorithm -> true/false"))?;
            for (algorithm, on) in map {
                if !SCAN_HASH_ALGORITHMS.contains(&algorithm.as_str()) {
                    return Err(Error::invalid(format!("scan_hashes: {algorithm} must be one of {}", SCAN_HASH_ALGORITHMS.join(", "))));
                }
                if !on.is_boolean() {
                    return Err(Error::invalid(format!("scan_hashes: {algorithm} must be true or false")));
                }
            }
            // Content is identified by hash. With none of these on, every file collapses
            // onto the same empty fingerprint and nothing can be matched to a DAT.
            let any = map.iter().any(|(k, v)| *k != "ra_hash" && v.as_bool() == Some(true));
            let all_absent = SCAN_HASH_ALGORITHMS.iter().filter(|k| **k != "ra_hash").all(|k| map.contains_key(*k));
            if all_absent && !any {
                return Err(Error::invalid("scan_hashes: at least one of crc32, xxh3_128, md5, sha1, sha256 must stay on -- content is identified by hash"));
            }
        }
        db_settings::KEY_SCAN_HEURISTICS => {
            let map = value.as_object().ok_or_else(|| Error::invalid("scan_heuristics must be an object {\"ext\": bool, \"path\": bool}"))?;
            for (key, on) in map {
                if !matches!(key.as_str(), "ext" | "path") {
                    return Err(Error::invalid(format!("scan_heuristics: {key} must be ext or path")));
                }
                if !on.is_boolean() {
                    return Err(Error::invalid(format!("scan_heuristics: {key} must be true or false")));
                }
            }
        }
        db_settings::KEY_SERIAL_MATCH => {
            let keys: Vec<&str> = db_settings::SERIAL_MATCH_FORMATS.iter().map(|(f, _)| *f).chain(["manifest", "file_name"]).collect();
            let map = value.as_object().ok_or_else(|| Error::invalid(format!("serial_match must be an object {{{}: bool}}", keys.join(", "))))?;
            for (key, on) in map {
                if !keys.contains(&key.as_str()) {
                    return Err(Error::invalid(format!("serial_match: {key} must be one of {}", keys.join(", "))));
                }
                if !on.is_boolean() {
                    return Err(Error::invalid(format!("serial_match: {key} must be true or false")));
                }
            }
        }
        db_settings::KEY_METADATA_SOURCE_PRIORITY => {
            let map = value.as_object().ok_or_else(|| Error::invalid("metadata_source_priority must be an object field -> [sources]"))?;
            for (field, list) in map {
                string_list(list, field)?;
                // Unknown fields and sources are refused rather than stored: a list naming a
                // source that can never supply the field reads as a choice and is not one.
                let allowed = db_settings::metadata_field_sources(field)
                    .ok_or_else(|| Error::invalid(format!("metadata_source_priority: unknown field {field:?}; expected one of {}", db_settings::METADATA_FIELD_SOURCES.iter().map(|(f, _)| *f).collect::<Vec<_>>().join("|"))))?;
                for source in list.as_array().into_iter().flatten().filter_map(|v| v.as_str()) {
                    if !allowed.contains(&source) {
                        return Err(Error::invalid(format!("metadata_source_priority.{field}: {source:?} cannot supply {field}; expected any of {}", allowed.join(", "))));
                    }
                }
            }
        }
        db_settings::KEY_UI_COLORS => {
            let map = value.as_object().ok_or_else(|| Error::invalid("ui_colors must be an object kind -> {value: \"#RRGGBB\"}"))?;
            for (kind, colors) in map {
                if !UI_COLOR_KINDS.contains(&kind.as_str()) {
                    return Err(Error::invalid(format!("ui_colors: unknown kind {kind:?}; expected one of {}", UI_COLOR_KINDS.join("|"))));
                }
                let inner = colors.as_object().ok_or_else(|| Error::invalid(format!("ui_colors.{kind} must be an object value -> \"#RRGGBB\"")))?;
                for (name, color) in inner {
                    if !color.as_str().is_some_and(is_hex_color) {
                        return Err(Error::invalid(format!("ui_colors.{kind}.{name} must be a \"#RRGGBB\" string")));
                    }
                }
            }
        }
        db_settings::KEY_UI_ICONS => {
            let map = value.as_object().ok_or_else(|| Error::invalid("ui_icons must be an object kind -> {value: \"<url>\"}"))?;
            for (kind, icons) in map {
                if !UI_ICON_KINDS.contains(&kind.as_str()) {
                    return Err(Error::invalid(format!("ui_icons: unknown kind {kind:?}; expected one of {}", UI_ICON_KINDS.join("|"))));
                }
                let inner = icons.as_object().ok_or_else(|| Error::invalid(format!("ui_icons.{kind} must be an object value -> url")))?;
                for (name, url) in inner {
                    if !url.as_str().is_some_and(|u| !u.trim().is_empty() && u.len() <= 2048) {
                        return Err(Error::invalid(format!("ui_icons.{kind}.{name} must be a non-empty URL string")));
                    }
                }
            }
        }
        db_settings::KEY_REGION_PRIORITY => {
            for code in string_list(value, key)? {
                if !crate::common::models::Region::ALL.iter().any(|r| r.code().eq_ignore_ascii_case(&code)) {
                    return Err(Error::invalid(format!("region_priority: unknown region code {code:?}")));
                }
            }
        }
        db_settings::KEY_CANONICAL_NAME_PRIORITY => {
            for code in string_list(value, key)? {
                let known = if code.chars().any(|c| c.is_ascii_lowercase()) {
                    crate::common::models::Language::ALL.iter().any(|l| l.code() == code)
                } else {
                    let region = db_settings::canonical_region_code(&code);
                    crate::common::models::Region::ALL.iter().any(|r| r.code() == region)
                };
                if !known {
                    return Err(Error::invalid(format!("canonical_name_priority: {code:?} is neither a region code (upper case) nor a language code (lower case)")));
                }
            }
        }
        db_settings::KEY_LANGUAGE_PRIORITY => {
            for code in string_list(value, key)? {
                if !crate::common::models::Language::ALL.iter().any(|l| l.code().eq_ignore_ascii_case(&code)) {
                    return Err(Error::invalid(format!("language_priority: unknown language code {code:?}")));
                }
            }
        }
        db_settings::KEY_PLATFORM_NAME_LANGUAGE => {
            if !matches!(value.as_str(), Some("en") | Some("ja")) {
                return Err(Error::invalid(format!("{key} must be \"en\" or \"ja\"")));
            }
        }
        // Only the shape: which languages exist depends on the files beside the server, which
        // `set` checks (`i18n::languages`).
        db_settings::KEY_UI_LANGUAGE => {
            if value.as_str().is_none() {
                return Err(Error::invalid("ui_language must be a language code (\"en\", \"ja\")"));
            }
        }
        db_settings::KEY_LANGUAGE_DISPLAY => {
            if !matches!(value.as_str(), Some("code") | Some("english") | Some("native")) {
                return Err(Error::invalid("language_display must be \"code\", \"english\" or \"native\""));
            }
        }
        db_settings::KEY_GRID_MEDIA_TYPE => {
            if !value.as_str().is_some_and(|v| MEDIA_TYPES.contains(&v)) {
                return Err(Error::invalid(format!("grid_media_type must be one of {}", MEDIA_TYPES.join("|"))));
            }
        }
        db_settings::KEY_TEMP_DIR_MARGIN => {
            let map = value.as_object().ok_or_else(|| Error::invalid("temp_dir_margin must be an object {percent?, min_bytes?}"))?;
            if let Some(p) = map.get("percent") {
                if !p.as_f64().is_some_and(|p| (0.0..=90.0).contains(&p)) {
                    return Err(Error::invalid("temp_dir_margin.percent must be 0-90"));
                }
            }
            if let Some(b) = map.get("min_bytes") {
                if b.as_u64().is_none() {
                    return Err(Error::invalid("temp_dir_margin.min_bytes must be a non-negative integer"));
                }
            }
        }
        db_settings::KEY_HASHEOUS_PARALLEL => {
            if !value.as_u64().is_some_and(|n| (1..=16).contains(&n)) {
                return Err(Error::invalid("hasheous_parallel_downloads must be an integer 1-16"));
            }
        }
        db_settings::KEY_PLAYMATCH_BATCH => {
            if !value.as_i64().is_some_and(|n| n >= -1) {
                return Err(Error::invalid("playmatch_batch_size must be an integer >= -1 (N per background batch, 0 = unlimited, -1 = off)"));
            }
        }
        db_settings::KEY_MEDIA_CACHE_TIER1_MB | db_settings::KEY_MEDIA_CACHE_TIER2_MB => {
            if value.as_u64().is_none() {
                return Err(Error::invalid(format!("{key} must be a non-negative integer (MiB, 0 = no cap)")));
            }
        }
        db_settings::KEY_MEDIA_THUMB_PX => {
            if !value.as_u64().is_some_and(|n| (32..=2048).contains(&n)) {
                return Err(Error::invalid("media_thumb_px must be an integer 32-2048"));
            }
        }
        db_settings::KEY_MEDIA_THUMB_QUALITY => {
            if !value.as_u64().is_some_and(|n| (1..=100).contains(&n)) {
                return Err(Error::invalid("media_thumb_quality must be an integer 1-100"));
            }
        }
        db_settings::KEY_RA_INCLUDE_BEATEN => {
            if !value.is_boolean() {
                return Err(Error::invalid("ra_include_beaten_default must be true or false"));
            }
        }
        db_settings::KEY_ALLOW_REMOTE_LAUNCH => {
            if !value.is_boolean() {
                return Err(Error::invalid("allow_remote_launch must be true or false"));
            }
        }
        db_settings::KEY_COPY_FOLDER_SCHEME => {
            let name = value.as_str().ok_or_else(|| Error::invalid("copy_folder_scheme must be a string"))?;
            if romexplorer_types::copy::FolderScheme::from_name(name).is_none() {
                return Err(Error::invalid(format!("copy_folder_scheme must be one of {}", romexplorer_types::copy::FolderScheme::ALL.map(|s| s.name()).join(" | "))));
            }
        }
        db_settings::KEY_COPY_PLATFORM_COMBINE => {
            let map = value.as_object().ok_or_else(|| Error::invalid("copy_platform_combine must be an object group -> true/false"))?;
            for (group, on) in map {
                if !crate::common::platform_names::COMBINE_GROUPS.iter().any(|g| g.key == group) {
                    return Err(Error::invalid(format!("unknown combine group {group:?}")));
                }
                if !on.is_boolean() {
                    return Err(Error::invalid(format!("{group} must be true or false")));
                }
            }
        }
        // Through the same builder a copy uses, so an out-of-range compression level is a
        // 400 here rather than a job that dies an hour in.
        db_settings::KEY_COPY_OUTPUT_DEFAULTS => {
            let settings: romexplorer_types::copy::OutputSettings = serde_json::from_value(value.clone()).map_err(|e| Error::invalid(format!("copy_output_defaults: {e}")))?;
            crate::api::copy::validate_output(&settings)?;
        }
        db_settings::KEY_COPY_DECIDE_ON_COPY => {
            if !value.is_boolean() {
                return Err(Error::invalid("copy_decide_on_copy must be true or false"));
            }
        }
        db_settings::KEY_COPY_RECENT_DESTINATIONS => {
            string_list(value, key)?;
        }
        db_settings::KEY_COPY_POPULATE_HASH | db_settings::KEY_INTEGRITY_POPULATE_HASH => {
            let name = value.as_str().ok_or_else(|| Error::invalid(format!("{key} must be a string")))?;
            if !romexplorer_types::copy::is_quick_hash(name) {
                return Err(Error::invalid(format!("{key} must be one of {}", romexplorer_types::copy::QUICK_HASHES.join(" | "))));
            }
        }
        db_settings::KEY_COPY_VERIFY_DEFAULT | db_settings::KEY_COPY_LOSSY_WARNING | db_settings::KEY_SCAN_PKG_TITLES => {
            if !value.is_boolean() {
                return Err(Error::invalid(format!("{key} must be true or false")));
            }
        }
        db_settings::KEY_RELEASE_EDITION_ORDER => {
            // An ordered list of editions; the two older values are still accepted.
            let ok = matches!(value.as_str(), Some("budget_first" | "versions_first"))
                || value.as_array().is_some_and(|list| {
                    let kinds: Vec<&str> = list.iter().filter_map(|v| v.as_str()).collect();
                    kinds.len() == list.len() && kinds.iter().all(|k| db_settings::EDITION_KINDS.contains(k)) && kinds.iter().collect::<std::collections::HashSet<_>>().len() == kinds.len()
                });
            if !ok {
                return Err(Error::invalid(format!("release_edition_order must be a list of {:?} (each once)", db_settings::EDITION_KINDS)));
            }
        }
        db_settings::KEY_RELEASE_KINDS => {
            if !matches!(value.as_str(), Some("all" | "patches" | "commercial")) {
                return Err(Error::invalid("release_kinds_default must be all, patches or commercial"));
            }
        }
        crate::database::release_buckets::KEY_UNLICENSED => {
            if !matches!(value.as_str(), Some("separate" | "retail")) {
                return Err(Error::invalid("release_unlicensed must be separate or retail"));
            }
        }
        db_settings::KEY_RA_BASE_FILTER => {
            if !matches!(value.as_str(), Some("console" | "genre")) {
                return Err(Error::invalid("ra_base_filter_default must be console or genre"));
            }
        }
        db_settings::KEY_RA_SOURCE => {
            if !matches!(value.as_str(), Some("all" | "local")) {
                return Err(Error::invalid("ra_source_default must be all | local"));
            }
        }
        db_settings::KEY_VIEW_DEFAULTS => {
            let map = value.as_object().ok_or_else(|| Error::invalid("view_defaults must be an object list -> view"))?;
            for (list, view) in map {
                let view = view.as_str().unwrap_or("");
                let ok = match list.as_str() {
                    "titles" | "releases" | "games" | "title_releases" | "sets" => matches!(view, "list" | "grid"),
                    "ra" => matches!(view, "list" | "grid" | "icon" | "title" | "screenshot" | "badges"),
                    _ => return Err(Error::invalid(format!("view_defaults: unknown list {list:?} (titles, releases, games, title_releases, sets, ra)"))),
                };
                if !ok {
                    return Err(Error::invalid(format!("view_defaults.{list}: unknown view {view:?}")));
                }
            }
        }
        db_settings::KEY_AUDIO_DSD_CACHE_FORMAT => {
            if !matches!(value.as_str(), Some("aac" | "flac")) {
                return Err(Error::invalid("audio_dsd_cache_format must be aac or flac"));
            }
        }
        db_settings::KEY_AUDIO_CLIENT_FORMAT => {
            if !matches!(value.as_str(), Some("none" | "aac" | "xhe_aac")) {
                return Err(Error::invalid("audio_client_format must be none, aac or xhe_aac"));
            }
        }
        db_settings::KEY_PLATFORM_PRIORITY => {
            let ids: Option<Vec<u64>> = value.as_array().map(|list| list.iter().filter_map(|v| v.as_u64()).collect());
            let whole = ids.as_ref().is_some_and(|ids| ids.len() == value.as_array().map_or(0, Vec::len) && ids.iter().all(|id| *id <= u64::from(u16::MAX)));
            let distinct = ids.as_ref().is_some_and(|ids| ids.iter().collect::<std::collections::HashSet<_>>().len() == ids.len());
            if !whole || !distinct {
                return Err(Error::invalid("platform_priority must be a list of platform ids, each once"));
            }
        }
        crate::database::title_family::KEY_PC_MAX_AGE => {
            if !value.as_u64().is_some_and(|y| y <= 100) {
                return Err(Error::invalid("definitive_pc_max_age must be a number of years, 0 to 100"));
            }
        }
        crate::database::title_family::KEY_TWO_COVER_LAYOUT => {
            if !value.as_str().is_some_and(|l| crate::database::title_family::TWO_COVER_LAYOUTS.contains(&l)) {
                return Err(Error::invalid(format!("definitive_two_cover_layout must be one of {}", crate::database::title_family::TWO_COVER_LAYOUTS.join(", "))));
            }
        }
        db_settings::KEY_COLLECTION_DEFAULT_TAB => {
            if !value.as_str().is_some_and(|tab| db_settings::COLLECTION_TABS.contains(&tab)) {
                return Err(Error::invalid(format!("collection_default_tab must be one of {}", db_settings::COLLECTION_TABS.join(", "))));
            }
        }
        db_settings::KEY_COLLECTION_HIDDEN_TABS => {
            let tabs: Option<Vec<&str>> = value.as_array().and_then(|list| list.iter().map(|v| v.as_str()).collect());
            let ok = tabs.as_ref().is_some_and(|tabs| tabs.iter().all(|t| db_settings::COLLECTION_TABS.contains(t) && !db_settings::COLLECTION_FIXED_TABS.contains(t)));
            if !ok {
                return Err(Error::invalid(format!("collection_hidden_tabs must be a list of {} (titles and releases are always shown)", db_settings::COLLECTION_TABS.iter().filter(|t| !db_settings::COLLECTION_FIXED_TABS.contains(t)).copied().collect::<Vec<_>>().join(", "))));
            }
        }
        super::sets::KEY_COPY_DEFINITIVE => {
            if !value.as_str().is_some_and(|m| super::sets::COPY_DEFINITIVE.contains(&m)) {
                return Err(Error::invalid(format!("copy_definitive must be one of {}", super::sets::COPY_DEFINITIVE.join(", "))));
            }
        }
        crate::database::title_family::KEY_FETCH_VERSIONS => {
            if !value.as_str().is_some_and(|m| crate::database::title_family::FETCH_VERSIONS.contains(&m)) {
                return Err(Error::invalid(format!("definitive_fetch_versions must be one of {}", crate::database::title_family::FETCH_VERSIONS.join(", "))));
            }
        }
        db_settings::KEY_AUDIO_FALLBACK => {
            if value.as_str().and_then(super::audio::Fallback::parse).is_none() {
                return Err(Error::invalid("audio_fallback must be none, random or generic_bgm"));
            }
        }
        db_settings::KEY_AUDIO_CLIENT_BITRATE => {
            if !value.as_u64().is_some_and(|n| super::audio_convert::BITRATE_RANGE.contains(&(n as u32))) {
                return Err(Error::invalid("audio_client_bitrate_kbps must be an integer 48-320"));
            }
        }
        db_settings::KEY_AUDIO_CACHE_MAX_MB => {
            if value.as_u64().is_none() {
                return Err(Error::invalid("audio_cache_max_mb must be a whole number of MiB (0 = no cap)"));
            }
        }
        super::artwork::KEY_EXPORT_CACHE_MB => {
            if value.as_u64().is_none() {
                return Err(Error::invalid("media_export_cache_max_mb must be a whole number of MiB (0 = no cap)"));
            }
        }
        super::artwork::KEY_EXPORT_PRIORITY => {
            let map = value.as_object().ok_or_else(|| Error::invalid("media_export_priority must be an object slot -> [provider or manual, ...]"))?;
            for (slot, order) in map {
                if !super::artwork::SLOTS.contains(&slot.as_str()) {
                    return Err(Error::invalid(format!("media_export_priority: {slot:?} is not a slot ({})", super::artwork::SLOTS.join(", "))));
                }
                for name in string_list(order, &format!("media_export_priority.{slot}"))? {
                    if !db_settings::is_media_source(&name) {
                        return Err(Error::invalid(format!("media_export_priority.{slot}: unknown provider {name:?}")));
                    }
                }
            }
        }
        db_settings::KEY_MEDIA_LIST_CAP => {
            if !value.as_u64().is_some_and(|n| (1..=50).contains(&n)) {
                return Err(Error::invalid("media_list_cap must be an integer 1-50"));
            }
        }
        db_settings::KEY_MEDIA_DOWNLOAD => {
            let map = value.as_object().ok_or_else(|| Error::invalid("media_download must be an object provider -> {media type -> true/false}"))?;
            for (provider, types) in map {
                if provider == db_settings::MANUAL_SOURCE || !db_settings::is_media_source(provider) {
                    return Err(Error::invalid(format!("media_download: unknown provider {provider:?}")));
                }
                media_flags(types, &format!("media_download.{provider}"))?;
            }
        }
        db_settings::KEY_MEDIA_SHOW | db_settings::KEY_MEDIA_CLIENT_KEEP => media_flags(value, key)?,
        db_settings::KEY_MEDIA_PRIORITY => {
            let map = value.as_object().ok_or_else(|| Error::invalid("media_priority must be an object media type -> [provider or manual, ...]"))?;
            for (media_type, order) in map {
                if !MEDIA_TYPES.contains(&media_type.as_str()) || media_type == "bgm" {
                    return Err(Error::invalid(format!("media_priority: {media_type:?} is not an image or video media type")));
                }
                for name in string_list(order, &format!("media_priority.{media_type}"))? {
                    if !db_settings::is_media_source(&name) {
                        return Err(Error::invalid(format!("media_priority.{media_type}: unknown provider {name:?}")));
                    }
                }
            }
        }
        db_settings::KEY_MEDIA_THUMB_FORMAT => {
            if !matches!(value.as_str(), Some("avif" | "jpeg" | "webp" | "jxl")) {
                return Err(Error::invalid("media_thumb_format must be avif|jpeg|webp|jxl (webp/jxl fall back to jpeg for now)"));
            }
        }
        _ => {}
    }
    Ok(())
}

/// The six `hashes.algorithm` values, as `scan_hashes` names them.
pub const SCAN_HASH_ALGORITHMS: [&str; 6] = ["crc32", "xxh3_128", "md5", "sha1", "sha256", "ra_hash"];

pub const UI_COLOR_KINDS: [&str; 6] = ["category", "platform", "region", "language", "source", "archive_format"];
pub const UI_ICON_KINDS: [&str; 4] = ["platform", "region", "language", "source"];
/// `media.media_type`'s ENUM values (migrations/0001_schema.sql).
pub const MEDIA_TYPES: [&str; 10] = ["cover", "box", "box_3d", "title_screen", "screenshot", "video", "bgm", "fanart", "logo", "icon"];

/// `{media type -> true/false}`, every key a real media type.
fn media_flags(value: &Value, what: &str) -> Result<()> {
    let map = value.as_object().ok_or_else(|| Error::invalid(format!("{what} must be an object media type -> true/false")))?;
    for (media_type, on) in map {
        if !MEDIA_TYPES.contains(&media_type.as_str()) {
            return Err(Error::invalid(format!("{what}: unknown media type {media_type:?} ({})", MEDIA_TYPES.join(", "))));
        }
        if !on.is_boolean() {
            return Err(Error::invalid(format!("{what}.{media_type} must be true or false")));
        }
    }
    Ok(())
}

fn is_hex_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The bypass this guards against: both front ends render any key without a structured
    /// editor as a raw JSON box, so without this a user can flip `instance_mode` from the
    /// Settings screen and skip everything `PUT /api/v1/mode` exists to do.
    #[test]
    fn identity_keys_are_refused_on_set_and_delete() {
        for key in ["instance_mode", "catalog_from", "instance_id"] {
            let err = validate(key, &json!("client")).expect_err(key);
            assert!(err.to_string().contains(key), "{key}: error should name the key: {err}");
            assert!(guard_identity(key).is_err(), "{key}: delete path");
        }
        assert!(validate("instance_mode", &json!("standalone")).is_err(), "even the harmless value goes through /mode");
        assert!(guard_identity("backup_dir").is_ok(), "ordinary keys are unaffected");
        assert!(validate("instance_mode", &json!("client")).unwrap_err().to_string().contains("/api/v1/mode"));
    }

    #[test]
    fn known_keys_are_shape_checked() {
        assert!(validate("provider_priority", &json!(["igdb", "hasheous"])).is_ok());
        assert!(validate("provider_priority", &json!(["nope"])).is_err());
        assert!(validate("raw_response_retention", &json!("keep_all")).is_ok());
        assert!(validate("raw_response_retention", &json!("sometimes")).is_err());
        assert!(validate("cache_ttl_days", &json!(30)).is_ok());
        assert!(validate("cache_ttl_days", &json!(0)).is_err());
        assert!(validate("title_display_mode", &json!({"languages": ["ja", "en"]})).is_ok());
        assert!(validate("title_display_mode", &json!("klingon")).is_err());
        assert!(validate("metadata_source_priority", &json!({"genre": ["igdb"]})).is_ok());
        assert!(validate("metadata_source_priority", &json!({"genre": "igdb"})).is_err());
        assert!(validate("metadata_source_priority", &json!({"serial": ["detected", "redump", "no-intro"], "franchise": ["igdb"], "release_name": ["no-intro", "igdb"]})).is_ok());
        assert!(validate("metadata_source_priority", &json!({"genre": ["hasheous"]})).is_err(), "a hash-lookup service has no genre");
        assert!(validate("metadata_source_priority", &json!({"franchise": ["redump"]})).is_err(), "no DAT carries a franchise");
        assert!(validate("metadata_source_priority", &json!({"publisher": ["igdb"]})).is_err(), "not a field of this setting");
        assert!(validate("custom_anything", &json!({"free": "form"})).is_ok());
        assert!(validate("", &json!(1)).is_err());
        assert!(validate("ui_colors", &json!({"category": {"base_game": "#a1b2c3"}, "archive_format": {"chd": "#cce5ff"}})).is_ok());
        assert!(validate("ui_colors", &json!({"category": {"base_game": "blue"}})).is_err());
        assert!(validate("ui_colors", &json!({"mood": {}})).is_err());
        assert!(validate("ui_icons", &json!({"platform": {"PlayStation": "https://example.invalid/psx.png"}})).is_ok());
        assert!(validate("ui_icons", &json!({"platform": {"PlayStation": ""}})).is_err());
        assert!(validate("ui_icons", &json!({"category": {}})).is_err());
        assert!(validate("language_display", &json!("native")).is_ok());
        assert!(validate("language_display", &json!("emoji")).is_err());
        assert!(validate("grid_media_type", &json!("box")).is_ok());
        assert!(validate("grid_media_type", &json!("poster")).is_err());
        assert!(validate("temp_dir_margin", &json!({"percent": 15, "min_bytes": 1024})).is_ok());
        assert!(validate("temp_dir_margin", &json!({"percent": 150})).is_err());
    }
}
