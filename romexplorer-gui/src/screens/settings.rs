//! Config › Settings: a structured editor per known key (radios, pulldowns, drag-orderable
//! lists, color pickers), raw JSON for unknown keys or via the "raw JSON" toggle (the
//! console's `settingEditor` / `showSettings`).

use dioxus::prelude::*;
use serde_json::{json, Map, Value};

use crate::api::{self, s, u};
use crate::html::{self, badge, esc};
use crate::state;
use crate::ui::{self, Html, Explain};

fn vocab_strs(path: &str) -> Vec<String> {
    state::vocab().pointer(path).and_then(|p| p.as_array()).into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect()
}

fn codes(kind: &str) -> Vec<String> {
    api::arr(&state::vocab(), kind).iter().map(|x| s(x, "code")).collect()
}

fn str_list(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect()
}

fn with_flag(flag: String, text: &str) -> String {
    format!("{}{}", if flag.is_empty() { String::new() } else { format!("<span class=\"flag\">{flag}</span>") }, esc(text))
}

fn region_label(c: &str) -> String {
    with_flag(html::region_flag(c), &format!("{c} — {}", state::region_name(c)))
}

fn lang_label(c: &str) -> String {
    with_flag(html::lang_flag(c), &format!("{c} — {}", state::language_label(c)))
}

const EDITORS: &[&str] = &[
    "provider_priority", "provider_sync_scope", "region_priority", "canonical_name_priority", "language_priority", "raw_response_retention", "cache_ttl_days", "language_display",
    "grid_media_type", "hasheous_parallel_downloads", "playmatch_batch_size", "media_cache_tier1_max_mb", "media_cache_tier2_max_mb", "media_thumb_px", "media_thumb_quality",
    "ra_include_beaten_default", "allow_remote_launch", "release_edition_order", "release_kinds_default", "release_unlicensed", "ra_base_filter_default", "ra_source_default", "view_defaults",
    "media_thumb_format", "temp_dir_margin", "title_display_mode", "metadata_source_priority", "ui_colors", "ui_icons",
    "sheet_style", "chd_reader", "media_sync_after_pull", "media_provider_fallback", "search_translations", "backup_dir", "backup_keep", "backup_catalog", "scan_hashes", "scan_heuristics", "serial_match", "scan_pkg_titles", "media_reuse_existing",
    "copy_recent_destinations", "copy_populate_hash", "integrity_populate_hash", "copy_verify_default", "copy_lossy_warning", "ui_language", "platform_name_language",
    "media_download", "media_priority", "media_show", "media_client_keep", "media_list_cap",
    "audio_dsd_cache_format", "audio_cache_max_mb", "audio_client_format", "audio_client_bitrate_kbps", "audio_fallback", "audio_title_sources", "audio_rom_kinds", "audio_rom_not_owned", "audio_keep_playing", "audio_rom_loop_min_s", "audio_rom_bgm_min_s", "audio_rom_jingle_min_s", "collection_default_tab", "collection_hidden_tabs", "copy_definitive", "definitive_fetch_versions", "platform_priority", "definitive_pc_max_age", "definitive_two_cover_layout",
    "scan_parallel_files", "copy_parallel_files", "media_export_prefer_region", "media_export_cache_max_mb", "media_export_priority",
    "merge_review_order", "gemini_model", "dat_sync_platforms",
];

/// Which media types each provider's sync can download (the vocabulary's `media.providers`).
fn media_providers() -> Vec<(String, Vec<String>)> {
    api::arr(&state::vocab()["media"], "providers").iter().map(|p| (s(p, "name"), api::arr(p, "types").iter().filter_map(|t| t.as_str().map(str::to_string)).collect())).collect()
}

/// [`media_providers`] whose art downloads on its own: `media_download` has nothing to say
/// about a provider fetched only by hand ("download art"), so it isn't listed there.
fn auto_media_providers() -> Vec<(String, Vec<String>)> {
    let manual_only: Vec<String> = api::arr(&state::vocab()["media"], "providers").iter().filter(|p| p.get("manual_only").and_then(Value::as_bool).unwrap_or(false)).map(|p| s(p, "name")).collect();
    media_providers().into_iter().filter(|(p, _)| !manual_only.contains(p)).collect()
}

/// Export artwork's slots, in the order RetroArch lists them.
const EXPORT_SLOTS: [&str; 4] = ["box", "title", "snap", "logo"];

/// A slot's order when `media_export_priority` does not say: the server's default (the
/// vocabulary's `media.export_priority`), then any other provider with art of the slot's types.
fn export_slot_default(slot: &str) -> Vec<String> {
    let mut order = vocab_strs(&format!("/media/export_priority/{slot}"));
    if order.is_empty() {
        order.push("manual".to_string());
    }
    let types = vocab_strs(&format!("/media/export_slots/{slot}"));
    for (provider, offered) in media_providers() {
        if offered.iter().any(|t| types.contains(t)) && !order.contains(&provider) {
            order.push(provider);
        }
    }
    order
}

/// The media types a client chooses between keeping and streaming: all of them.
fn keepable_media_types() -> Vec<String> {
    vocab_strs("/media_types")
}

/// Keys whose editor lives on another screen; the settings form only points there.
fn edited_elsewhere(key: &str) -> Option<(crate::Route, &'static str)> {
    match key {
        "instance_mode" | "catalog_from" => Some((crate::Route::ConfigMode {}, "Config › Mode")),
        "copy_folder_scheme" | "copy_platform_combine" | "copy_output_defaults" | "copy_decide_on_copy" | "copy_header_preference" | "copy_wiiu_preference" | "copy_format_order" | "copy_prefer_decrypted" => Some((crate::Route::ConfigCopy {}, "Config › Copy")),
        _ => None,
    }
}

/// The on/off settings: (key, default, label when on, label when off).
const BOOL_KEYS: &[(&str, bool, &str, &str)] = &[
    ("media_sync_after_pull", true, "fetch new art from the main server after each catalogue sync (default)", "leave art to be fetched on demand"),
    ("media_provider_fallback", true, "while the main server is offline, fetch art from the provider directly (default)", "wait for the main server"),
    ("search_translations", true, "searches also try names in the other language from the name dictionary (default)", "search names only as typed"),
    ("backup_catalog", false, "also back up the catalogue with each database backup", "database only (default)"),
    ("copy_verify_default", false, "read every copy back and check it", "trust the write (default; halves the reads)"),
    ("copy_lossy_warning", true, "ask before choosing a lossy output: wua, zar, xiso (default)", "don't ask"),
    ("media_export_prefer_region", true, "exported art: the release's own region first, whatever the provider order (default)", "exported art follows the provider order"),
    ("scan_pkg_titles", false, "read the title inside PS3 and PSP .pkg files", "name them from the content id only (default)"),
    ("media_reuse_existing", true, "take art already in the media folder instead of downloading it again (default)", "always download"),
    ("audio_keep_playing", true, "keep a title's music playing on other screens until another title page opens (default)", "stop the music when the page is left"),
];

/// Hashes a file scan can compute, in the order they are shown. Default: all but sha256.
const SCAN_HASHES: &[(&str, bool)] = &[("crc32", true), ("xxh3_128", true), ("md5", true), ("sha1", true), ("sha256", false), ("ra_hash", true)];
/// `serial_match`: key, default, label -- the same list as the web console's.
const SERIAL_MATCH: &[(&str, bool, &str)] = &[
    ("cia", true, ".cia (3DS)"),
    ("xiso", true, ".xiso (Xbox)"),
    ("wua", true, ".wua (Wii U)"),
    ("zar", true, ".zar (Xbox 360)"),
    ("vpk", true, ".vpk (Vita)"),
    ("vita_folder", true, "Vita app folders"),
    ("nsp", true, ".nsp / .nsz (Switch; by the ticket or the [title id] in the name)"),
    ("pkg", false, ".pkg (PS3, PSP, Vita; No-Intro hashes these)"),
    ("psv", false, ".psv and game-card images (Vita)"),
    ("ps_disc", false, "PS1, PS2 and PS3 discs, PS3 game folders (Redump hashes these)"),
    ("manifest", true, "the serial in a manifest this app wrote"),
    ("file_name", false, "a serial in the file name"),
];
/// `serial_match.file_name_platforms`: the platforms that can be set one by one (IGDB ids,
/// `database::settings::SERIAL_FILE_NAME_PLATFORMS`; the console lists the same).
const SERIAL_FILE_NAME_PLATFORMS: &[u16] = &[37, 137, 41, 11, 12, 46, 7, 8, 9, 38, 30009, 130];
const QUICK_HASHES: &[&str] = &["xxh3_128", "crc32", "md5", "sha1", "sha256"];

const SPELL: [(&str, &str); 3] = [("US", "USA"), ("EU", "EUR"), ("JP", "JPN")];

fn spell(c: &str) -> String {
    SPELL.iter().find(|(a, _)| *a == c).map(|(_, b)| b.to_string()).unwrap_or_else(|| c.to_string())
}

fn unspell(c: &str) -> String {
    SPELL.iter().find(|(_, b)| *b == c).map(|(a, _)| a.to_string()).unwrap_or_else(|| c.to_string())
}

/// A known key's editor state from its stored value (defaults filled in).
fn initial(key: &str, value: &Value) -> Value {
    let providers = vocab_strs("/jobs/providers");
    match key {
        "provider_priority" => {
            let stored: Vec<String> = str_list(value).into_iter().filter(|p| providers.contains(p)).collect();
            let mut o = stored.clone();
            o.extend(providers.iter().filter(|p| !stored.contains(p)).cloned());
            json!(o)
        }
        "region_priority" => {
            let cs = codes("regions");
            let stored: Vec<String> = str_list(value).into_iter().filter(|c| cs.contains(c)).collect();
            let mut o = stored.clone();
            o.extend(["US", "WORLD", "EU", "JP", "ASIA", "UK"].iter().map(|c| c.to_string()).filter(|c| cs.contains(c) && !stored.contains(c)));
            json!(o)
        }
        "language_priority" => {
            let cs = codes("languages");
            let stored: Vec<String> = str_list(value).into_iter().filter(|c| cs.contains(c)).collect();
            let mut o = stored.clone();
            o.extend(["en", "ja"].iter().map(|c| c.to_string()).filter(|c| cs.contains(c) && !stored.contains(c)));
            json!(o)
        }
        "canonical_name_priority" => {
            let regions: Vec<String> = codes("regions").iter().map(|c| spell(c)).collect();
            let langs = codes("languages");
            let ok = |c: &String| regions.contains(c) || langs.contains(c);
            let stored: Vec<String> = str_list(value).iter().map(|c| spell(c)).filter(ok).collect();
            if stored.is_empty() {
                json!(["USA", "EUR", "WORLD", "ASIA", "en", "JPN"].iter().map(|c| c.to_string()).filter(ok).collect::<Vec<_>>())
            } else {
                json!(stored)
            }
        }
        "metadata_source_priority" => {
            let mut sources = vocab_strs("/jobs/dat_sources");
            for p in providers {
                if !sources.contains(&p) {
                    sources.push(p);
                }
            }
            let mut out = Map::new();
            for f in META_FIELDS {
                // Only the sources that can supply this field (the server's list); every
                // source, as before, against a server too old to say.
                let able = vocab_strs(&format!("/jobs/metadata_field_sources/{f}"));
                let able = if able.is_empty() { sources.clone() } else { able };
                let stored: Vec<String> = str_list(value.get(f).unwrap_or(&Value::Null)).into_iter().filter(|x| able.contains(x)).collect();
                let mut o = stored.clone();
                o.extend(able.iter().filter(|x| !stored.contains(x)).cloned());
                out.insert(f.to_string(), json!(o));
            }
            Value::Object(out)
        }
        "provider_sync_scope" | "view_defaults" | "ui_colors" | "ui_icons" => if value.is_object() { value.clone() } else { json!({}) },
        "media_download" => {
            let mut m = Map::new();
            for (provider, types) in auto_media_providers() {
                // Unlisted: on for what a sync always fetched (box, cover), off for the rest.
                let default_on = vocab_strs("/media/default_on");
                let flags: Map<String, Value> = types.iter().map(|t| (t.clone(), json!(value.pointer(&format!("/{provider}/{t}")).and_then(Value::as_bool).unwrap_or(default_on.contains(t))))).collect();
                m.insert(provider, Value::Object(flags));
            }
            Value::Object(m)
        }
        "media_priority" => {
            let providers = media_providers();
            let mut m = Map::new();
            // Your uploads ("manual") and every provider that can supply the type; unplaced
            // uploads come first, as the server reads it, unplaced providers last.
            for media_type in vocab_strs("/media/types").into_iter().filter(|t| t != "bgm") {
                let able: Vec<String> = providers.iter().filter(|(_, types)| types.contains(&media_type)).map(|(p, _)| p.clone()).collect();
                if able.is_empty() {
                    continue;
                }
                let stored: Vec<String> = str_list(&value[media_type.as_str()]).into_iter().filter(|p| p == "manual" || able.contains(p)).collect();
                let mut order: Vec<String> = if stored.iter().any(|p| p == "manual") { Vec::new() } else { vec!["manual".to_string()] };
                order.extend(stored.iter().cloned());
                order.extend(able.into_iter().filter(|p| !stored.contains(p)));
                m.insert(media_type, json!(order));
            }
            Value::Object(m)
        }
        "media_export_priority" => {
            let mut m = Map::new();
            for slot in EXPORT_SLOTS {
                let able = export_slot_default(slot);
                // A stored order keeps its place; anything it doesn't name follows, as the default has it.
                let order: Vec<String> = match value[slot].as_array() {
                    Some(_) => {
                        let stored: Vec<String> = str_list(&value[slot]).into_iter().filter(|p| able.contains(p)).collect();
                        stored.iter().cloned().chain(able.into_iter().filter(|p| !stored.contains(p))).collect()
                    }
                    None => able,
                };
                m.insert(slot.to_string(), json!(order));
            }
            Value::Object(m)
        }
        "media_show" => Value::Object(vocab_strs("/media_types").into_iter().map(|t| { let on = value.get(&t).and_then(Value::as_bool).unwrap_or(true); (t, json!(on)) }).collect()),
        "media_client_keep" => Value::Object(keepable_media_types().into_iter().map(|t| { let on = value.get(&t).and_then(Value::as_bool).unwrap_or(t != "video" && t != "bgm"); (t, json!(on)) }).collect()),
        "temp_dir_margin" => {
            let mut m = json!({ "percent": 10, "min_bytes": 1073741824u64 });
            if let Some(o) = value.as_object() {
                for (k, v) in o {
                    m[k] = v.clone();
                }
            }
            m
        }
        "title_display_mode" => if value.is_null() { json!("english") } else { value.clone() },
        "sheet_style" => if value.is_null() { json!("redump") } else { value.clone() },
        "chd_reader" => if value.is_null() { json!("checked") } else { value.clone() },
        "dat_sync_platforms" => json!({
            "mode": value.get("mode").and_then(Value::as_str).filter(|m| ["all", "scanned", "scanned_strict", "manual"].contains(m)).unwrap_or("all"),
            "platforms": value.get("platforms").and_then(Value::as_array).cloned().unwrap_or_default(),
            "extras": value.get("extras").and_then(Value::as_array).cloned(),
        }),
        "ui_language" | "platform_name_language" => if value.is_null() { json!("en") } else { value.clone() },
        "copy_populate_hash" | "integrity_populate_hash" => if value.is_null() { json!("xxh3_128") } else { value.clone() },
        "backup_dir" => json!(value.as_str().unwrap_or("")),
        "backup_keep" => if value.is_null() { json!(0) } else { value.clone() },
        "copy_recent_destinations" => json!(str_list(value)),
        "scan_hashes" => {
            let mut m = Map::new();
            for (name, def) in SCAN_HASHES {
                m.insert(name.to_string(), json!(value.get(name).and_then(Value::as_bool).unwrap_or(*def)));
            }
            Value::Object(m)
        }
        "scan_parallel_files" | "copy_parallel_files" => {
            let mut m = json!({ "mode": "auto" });
            if let Some(o) = value.as_object() {
                for (k, v) in o {
                    m[k] = v.clone();
                }
            }
            m
        }
        "scan_heuristics" => json!({ "ext": value.get("ext").and_then(Value::as_bool).unwrap_or(true), "path": value.get("path").and_then(Value::as_bool).unwrap_or(true) }),
        "serial_match" => {
            let mut m = Map::new();
            for (name, def, _) in SERIAL_MATCH {
                m.insert(name.to_string(), json!(value.get(name).and_then(Value::as_bool).unwrap_or(*def)));
            }
            m.insert("file_name_platforms".to_string(), value.get("file_name_platforms").filter(|v| v.is_object()).cloned().unwrap_or_else(|| json!({})));
            Value::Object(m)
        }
        k if BOOL_KEYS.iter().any(|(b, ..)| *b == k) => {
            let def = BOOL_KEYS.iter().find(|(b, ..)| *b == k).map(|(_, d, ..)| *d).unwrap_or(false);
            json!(value.as_bool().unwrap_or(def))
        }
        "raw_response_retention" => if value.is_null() { json!("keep_latest") } else { value.clone() },
        "merge_review_order" => if value.is_null() { json!("regional") } else { value.clone() },
        "gemini_model" => if value.is_null() { json!("") } else { value.clone() },
        "cache_ttl_days" => if value.is_null() { json!(90) } else { value.clone() },
        "language_display" => if value.is_null() { json!("code") } else { value.clone() },
        "grid_media_type" => if value.is_null() { json!("box") } else { value.clone() },
        "ra_include_beaten_default" => if value.is_null() { json!(true) } else { value.clone() },
        "allow_remote_launch" => json!(value == &json!(true)),
        "release_edition_order" => json!(edition_order(value)),
        "audio_dsd_cache_format" => if value.as_str() == Some("flac") { json!("flac") } else { json!("aac") },
        "audio_client_format" => if matches!(value.as_str(), Some("aac" | "xhe_aac" | "none")) { value.clone() } else { json!("aac") },
        "audio_fallback" => if matches!(value.as_str(), Some("random" | "generic_bgm")) { value.clone() } else { json!("none") },
        "audio_title_sources" => if matches!(value.as_str(), Some("tagged" | "rom")) { value.clone() } else { json!("both") },
        "audio_rom_kinds" => if matches!(value.as_str(), Some("bgm_jingle" | "all")) { value.clone() } else { json!("bgm") },
        "audio_rom_not_owned" => if matches!(value.as_str(), Some("random" | "none")) { value.clone() } else { json!("related") },
        "collection_default_tab" => if value.is_null() { json!("titles") } else { value.clone() },
        "collection_hidden_tabs" => json!(str_list(value)),
        "copy_definitive" => if value.as_str() == Some("definitive_only") { value.clone() } else { json!("best_held") },
        "definitive_fetch_versions" => if matches!(value.as_str(), Some("per_title" | "whole_platform")) { value.clone() } else { json!("off") },
        // Ids, in order; unset is the server's default order (in the vocabulary).
        "platform_priority" => if value.is_array() { value.clone() } else { state::vocab().get("platform_priority_default").cloned().unwrap_or_else(|| json!([])) },
        "release_kinds_default" => if value.is_null() { json!("all") } else { value.clone() },
        "release_unlicensed" => if value.as_str() == Some("retail") { json!("retail") } else { json!("separate") },
        "ra_base_filter_default" => if value.is_null() { json!("console") } else { value.clone() },
        "ra_source_default" => if value.as_str() == Some("local") { json!("local") } else { json!("all") },
        "media_thumb_format" => if value.is_null() { json!("avif") } else { value.clone() },
        "definitive_two_cover_layout" => if matches!(value.as_str(), Some("diagonal" | "left_right")) { value.clone() } else { json!("top_bottom") },
        k => match num_spec(k) {
            Some((def, ..)) if value.is_null() => json!(def),
            _ => value.clone(),
        },
    }
}

/// What gets saved: empty maps become "no value" (the default).
fn finalize(key: &str, cur: &Value) -> Value {
    match key {
        "provider_sync_scope" | "view_defaults" => {
            if cur.as_object().is_some_and(|o| o.is_empty()) {
                Value::Null
            } else {
                cur.clone()
            }
        }
        "ui_colors" | "ui_icons" => {
            let mut out = Map::new();
            for (k, m) in cur.as_object().into_iter().flatten() {
                let clean: Map<String, Value> = m.as_object().into_iter().flatten().filter(|(_, v)| v.as_str().is_some_and(|x| !x.trim().is_empty())).map(|(a, b)| (a.clone(), b.clone())).collect();
                if !clean.is_empty() {
                    out.insert(k.clone(), Value::Object(clean));
                }
            }
            if out.is_empty() {
                Value::Null
            } else {
                Value::Object(out)
            }
        }
        // An order only says something where two providers can supply the type.
        "media_priority" => {
            let kept: Map<String, Value> = cur.as_object().into_iter().flatten().filter(|(_, order)| order.as_array().is_some_and(|o| o.len() > 1)).map(|(k, o)| (k.clone(), o.clone())).collect();
            if kept.is_empty() { Value::Null } else { Value::Object(kept) }
        }
        "media_export_priority" => {
            if EXPORT_SLOTS.iter().all(|slot| str_list(&cur[*slot]) == export_slot_default(slot)) { Value::Null } else { cur.clone() }
        }
        "backup_dir" => if cur.as_str().is_some_and(|d| d.trim().is_empty()) { Value::Null } else { json!(cur.as_str().unwrap_or("").trim()) },
        "copy_recent_destinations" => if cur.as_array().is_some_and(|a| a.is_empty()) { Value::Null } else { cur.clone() },
        _ => cur.clone(),
    }
}

/// (default, min, max, unit, note) of the plain number settings.
fn num_spec(key: &str) -> Option<(i64, i64, Option<i64>, &'static str, &'static str)> {
    Some(match key {
        "audio_client_bitrate_kbps" => (128, 48, Some(320), "kbps", "Client mode: the bitrate a kept copy is converted to. A track already at or below it is copied as it is; DSD, ALAC and AIFF are always converted."),
        "audio_cache_max_mb" => (4096, 0, None, "MiB", "Size cap of the converted-audio cache; 0 = no cap. The least recently played conversions are removed first and made again when next needed."),
        "media_list_cap" => (6, 1, Some(50), "per game", "How many screenshots / fan art images are taken per game from one provider."),
        "hasheous_parallel_downloads" => (4, 1, Some(16), "zips", "Hasheous per-platform dump zips downloaded at once during a full sync."),
        "playmatch_batch_size" => (100, -1, None, "contents", "Background PlayMatch gap filler (runs only while no other sync job is queued, and only for providers with usable credentials): N = at most N contents per batch; 0 = unlimited (still 100 per request, rate-limited); -1 = off."),
        "media_cache_tier1_max_mb" => (0, 0, None, "MiB", "Size cap of the tier 1 media cache (least recently used full-size copies are evicted first, thumbnails last); 0 = no cap."),
        "media_cache_tier2_max_mb" => (0, 0, None, "MiB", "Size cap of the tier 2 media cache; 0 = no cap."),
        "media_thumb_px" => (320, 32, Some(2048), "px", "Longest edge of grid thumbnails."),
        "media_thumb_quality" => (60, 1, Some(100), "", "Lossy thumbnail quality (1-100)."),
        "backup_keep" => (0, 0, None, "per kind", "How many backups of each kind to keep; 0 = all. A database backup never prunes a catalogue one."),
        "definitive_pc_max_age" => (10, 0, Some(100), "years", "Collection › Definitive: a PC version less than this many years older than the newest version takes the \"PC (Definitive tab)\" place in the platform priority; an older one ranks by its own year. 0 = PC versions never move up."),
        "media_export_cache_max_mb" => (1024, 0, None, "MiB", "Size cap of the cache of pictures converted for emulators; 0 = no cap. The least recently used go first."),
        "audio_rom_loop_min_s" => (3, 0, None, "s", "ROM audio: a sound that loops is music from this length."),
        "audio_rom_bgm_min_s" => (20, 0, None, "s", "ROM audio: a sound that does not loop is music from this length."),
        "audio_rom_jingle_min_s" => (8, 0, None, "s", "ROM audio: a sound that does not loop is a jingle from this length; shorter is a sound effect."),
        _ => return None,
    })
}

const META_FIELDS: [&str; 4] = ["genre", "serial", "franchise", "release_name"];

// --- small editors ---

#[component]
fn Radios(name: String, options: Vec<(String, String)>, current: String, on_change: EventHandler<String>) -> Element {
    rsx! {
        div { class: "radios",
            for (v, l) in options {
                label { input { r#type: "radio", name: "{name}", checked: v == current, onchange: { let v = v.clone(); move |_| on_change.call(v.clone()) } } " {l}" }
            }
        }
    }
}

#[component]
fn Pulldown(options: Vec<(String, String)>, current: String, on_change: EventHandler<String>) -> Element {
    rsx! {
        select { onchange: move |e| on_change.call(e.value()),
            for (v, l) in options { option { value: "{v}", selected: v == current, "{l}" } }
        }
    }
}

/// A drag-orderable list (▲▼ too). `labels`: HTML per item.
/// The editions `release_edition_order` orders (`settings::EDITION_KINDS`), default first.
const EDITION_KINDS: [&str; 5] = ["budget", "original", "rerelease", "special", "aftermarket"];

/// The edition order a stored value means: every edition, the stored list first; the two
/// older values as they were meant.
fn edition_order(value: &Value) -> Vec<String> {
    let mut order: Vec<String> = match value {
        Value::Array(list) => list.iter().filter_map(|v| v.as_str()).filter(|k| EDITION_KINDS.contains(k)).map(str::to_string).collect(),
        Value::String(s) if s == "versions_first" => vec!["original".into(), "budget".into()],
        _ => Vec::new(),
    };
    for k in EDITION_KINDS {
        if !order.iter().any(|o| o == k) {
            order.push(k.to_string());
        }
    }
    order
}

fn edition_label(kind: &str) -> &'static str {
    match kind {
        "budget" => "budget reissue (\"PlayStation 2 the Best\", \"Greatest Hits\")",
        "original" => "original release",
        "rerelease" => "re-release (Virtual Console, collections)",
        "special" => "special / limited edition",
        _ => "aftermarket / homebrew",
    }
}

#[component]
fn OrderList(items: Vec<String>, labels: Vec<String>, #[props(default)] removable: bool, on_change: EventHandler<Vec<String>>) -> Element {
    let mut drag = use_signal(|| None::<usize>);
    let mut over = use_signal(|| None::<usize>);
    let n = items.len();
    rsx! {
        ul { class: "olist",
            if items.is_empty() { li { class: "muted", {state::tr("empty")} } }
            for (i, it) in items.iter().enumerate() {
                {
                    let (a, b, c, d) = (items.clone(), items.clone(), items.clone(), items.clone());
                    let label = labels.get(i).cloned().unwrap_or_else(|| esc(it));
                    rsx! {
                        li { key: "{it}", draggable: true,
                            class: if drag() == Some(i) { "drag" } else if over() == Some(i) { "over" } else { "" },
                            ondragstart: move |_| drag.set(Some(i)),
                            ondragend: move |_| { drag.set(None); over.set(None); },
                            ondragover: move |e| { e.prevent_default(); over.set(Some(i)); },
                            ondragleave: move |_| over.set(None),
                            ondrop: move |e| {
                                e.prevent_default();
                                over.set(None);
                                let Some(from) = drag() else { return };
                                drag.set(None);
                                if from == i { return; }
                                let mut o = a.clone();
                                let m = o.remove(from);
                                o.insert(i, m);
                                on_change.call(o);
                            },
                            span { class: "grip", "⋮⋮" }
                            Html { class: "lab", html: label }
                            span { class: "spacer" }
                            button { class: "btn small", r#type: "button", onclick: move |_| { if i > 0 { let mut o = b.clone(); o.swap(i - 1, i); on_change.call(o); } }, "▲" }
                            button { class: "btn small", r#type: "button", onclick: move |_| { if i + 1 < n { let mut o = c.clone(); o.swap(i, i + 1); on_change.call(o); } }, "▼" }
                            if removable { button { class: "btn small danger", r#type: "button", onclick: move |_| { let mut o = d.clone(); o.remove(i); on_change.call(o); }, "×" } }
                        }
                    }
                }
            }
        }
    }
}

/// `platform_priority`'s entry for PC versions on Collection › Definitive
/// (`title_family::PC_DEFINITIVE`).
const PC_DEFINITIVE: u64 = 65001;

fn priority_label(id: u64) -> String {
    if id == PC_DEFINITIVE {
        html::badge(&state::tr("PC (Definitive tab)"), "warn")
    } else {
        html::cbadge("platform", &state::platform_name(id), None, None)
    }
}

/// `platform_priority`'s list: the add pulldown offers only the platforms that are in a game
/// on several platforms (asked of the server), since the rest never need ranking, and the
/// Definitive tab's PC entry.
#[component]
fn PlatformPriority(items: Vec<String>, labels: Vec<String>, on_change: EventHandler<Vec<String>>) -> Element {
    let offered = use_resource(|| async move { api::get("/api/v1/catalog/crossplatform/platforms").await.ok() });
    let mut add_options: Vec<(String, String)> = offered.read().clone().flatten().and_then(|v| v.as_array().cloned()).unwrap_or_default().iter().filter_map(Value::as_u64).map(|id| (id.to_string(), state::platform_name(id))).collect();
    add_options.push((PC_DEFINITIVE.to_string(), state::tr("PC (Definitive tab)")));
    // Remounted when the offer arrives: the pulldown's first pick is made when it mounts.
    rsx! { for n in [add_options.len()] { AddableList { key: "{n}", items: items.clone(), labels: labels.clone(), add_options: add_options.clone(), add_label: state::tr("add platform"), on_change } } }
}

/// An order list with an "add" pulldown (regions / languages).
#[component]
fn AddableList(items: Vec<String>, labels: Vec<String>, add_options: Vec<(String, String)>, add_label: String, on_change: EventHandler<Vec<String>>) -> Element {
    let mut pick = use_signal(|| add_options.first().map(|o| o.0.clone()).unwrap_or_default());
    let items2 = items.clone();
    rsx! {
        OrderList { items: items.clone(), labels, removable: true, on_change }
        div { class: "toolbar",
            select { onchange: move |e| pick.set(e.value()), for (v, l) in add_options { option { value: "{v}", "{l}" } } }
            button { class: "btn small", r#type: "button", onclick: move |_| { let c = pick(); if !items2.contains(&c) { let mut o = items2.clone(); o.push(c); on_change.call(o); } }, "{add_label}" }
        }
    }
}

fn color_values() -> Vec<(&'static str, Vec<String>)> {
    vec![
        ("category", html::CATEGORY_VALUES.iter().map(|x| x.to_string()).collect()),
        ("platform", state::platforms().iter().map(|p| s(p, "name")).filter(|n| !n.is_empty()).collect()),
        ("region", codes("regions")),
        ("language", codes("languages")),
        ("source", vocab_strs("/jobs/dat_sources")),
        ("archive_format", html::ARCHIVE_FORMAT_VALUES.iter().map(|x| x.to_string()).collect()),
    ]
}

/// `scan_parallel_files` / `copy_parallel_files`: auto, or a number per size tier, a tier left
/// empty keeping auto's number. The tiers' bounds are on the labels; what auto picks here
/// (`GET /api/v1/parallel/auto`) fills the placeholders. The drive row says what the disk is
/// when detection gets it wrong, and auto's numbers follow it. The same form as the web
/// console's (`src/server/console.html`).
#[component]
fn ParallelEditor(name: String, cur: Signal<Value>) -> Element {
    let mut cur = cur;
    let v = cur();
    let custom = s(&v, "mode") == "custom";
    let drive = s(&v, "drive");
    let drives = [("", "detect automatically"), ("ssd", "SSD / NVMe"), ("hdd", "HDD"), ("network", "network share")];
    let auto = use_resource(|| async { api::get("/api/v1/parallel/auto").await.ok() });
    // Auto's numbers for the drive chosen, or the one detected; an older server has no `drives`.
    let nums = auto().flatten().map(|a| {
        let kind = if drive.is_empty() { s(&a, "detected") } else { drive.clone() };
        a.get("drives").and_then(|d| d.get(&kind)).cloned().unwrap_or(a)
    });
    let lim = nums.as_ref().map(|a| if name == "scan_parallel_files" { a["scan"].clone() } else { a["copy"].clone() });
    let note = auto().flatten().map(|a| {
        let l = |x: &Value, t: &str| u(x, t).to_string();
        let lim = lim.clone().unwrap_or_default();
        let base = state::trf("auto here: {s} small / {m} medium / {l} large at once ({cores} cores, disk of {path})", &[
            ("s", &l(&lim, "small")), ("m", &l(&lim, "medium")), ("l", &l(&lim, "large")),
            ("cores", &u(&a, "cores").to_string()), ("path", &s(&a, "probed")),
        ]);
        let base = if name == "copy_parallel_files" {
            let c = nums.as_ref().map(|n| n["copy_converting"].clone()).unwrap_or_default();
            format!("{base} — {}", state::trf("a copy that converts: {s} / {m} / {l}", &[("s", &l(&c, "small")), ("m", &l(&c, "medium")), ("l", &l(&c, "large"))]))
        } else {
            base
        };
        let detected = s(&a, "detected");
        let found = drives.iter().find(|(k, _)| *k == detected).map(|(_, l)| state::tr(l)).unwrap_or(detected);
        format!("{base} — {}", state::trf("detected as {kind}", &[("kind", &found)]))
    });
    let tiers = [("small", "small (up to 16 MiB)"), ("medium", "medium (16 MiB – 1 GiB)"), ("large", "large (1 GiB and up)")];
    rsx! {
        div { class: "radios",
            label { input { r#type: "radio", name: "{name}-mode", checked: !custom, onchange: move |_| { let mut x = cur(); x["mode"] = json!("auto"); cur.set(x); } } " " {state::tr("auto")} }
            label { input { r#type: "radio", name: "{name}-mode", checked: custom, onchange: move |_| { let mut x = cur(); x["mode"] = json!("custom"); cur.set(x); } } " " {state::tr("custom")} }
        }
        div { class: "optgrid",
            for (tier, label) in tiers {
                {
                    let shown = v.get(tier).and_then(Value::as_u64).map(|n| n.to_string()).unwrap_or_default();
                    let placeholder = lim.as_ref().map(|l| state::trf("auto: {n}", &[("n", &u(l, tier).to_string())])).unwrap_or_else(|| state::tr("auto"));
                    rsx! {
                        label { key: "{tier}",
                            {state::tr(label)} " "
                            input { r#type: "number", min: "1", max: "64", step: "1", style: "width:70px", disabled: !custom, value: "{shown}", placeholder: "{placeholder}",
                                oninput: move |e| {
                                    let mut x = cur();
                                    match e.value().trim().parse::<u64>() {
                                        Ok(n) if n > 0 => x[tier] = json!(n.min(64)),
                                        _ => { if let Some(o) = x.as_object_mut() { o.remove(tier); } }
                                    }
                                    cur.set(x);
                                }
                            }
                        }
                    }
                }
            }
        }
        div { class: "radios",
            span { {state::tr("drive:")} }
            for (kind, label) in drives {
                label { key: "{kind}",
                    input { r#type: "radio", name: "{name}-drive", checked: drive == kind,
                        onchange: move |_| {
                            let mut x = cur();
                            if kind.is_empty() { if let Some(o) = x.as_object_mut() { o.remove("drive"); } } else { x["drive"] = json!(kind); }
                            cur.set(x);
                        }
                    }
                    " " {state::tr(label)}
                }
            }
        }
        if let Some(n) = note { div { class: "muted", "{n}" } }
    }
}

/// `dat_sync_platforms`: which platforms a DAT sync downloads and imports -- all, the
/// scanned ones (as the scan placed them, or strictly the hash-matched ones), or ticked here.
#[component]
fn DatPlatformsEditor(cur: Signal<Value>) -> Element {
    let mut cur = cur;
    let v = cur();
    let mode = v.get("mode").and_then(Value::as_str).unwrap_or("all").to_string();
    let checked = use_signal(|| v.get("platforms").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect::<std::collections::BTreeSet<u64>>()).unwrap_or_default());
    // Unset: every one in mode all, none in the others (a platform list is a whitelist).
    let extras = use_signal(|| {
        let all = v.get("mode").and_then(Value::as_str).is_none_or(|m| m == "all");
        v.get("extras").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_else(|| if all { super::widgets::DAT_EXTRAS.iter().map(|(k, _)| k.to_string()).collect::<std::collections::BTreeSet<String>>() } else { Default::default() })
    });
    let options: Vec<(String, String)> = [
        ("all", "all platforms (default)"),
        ("scanned", "on file scan (folder setting, hints and extensions count)"),
        ("scanned_strict", "on file scan, strict: only hash-matched platforms (not recommended)"),
        ("manual", "manual"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), state::tr(b)))
    .collect();
    rsx! {
        div { class: "muted", {state::tr("Which platforms a DAT sync downloads and imports.")} }
        Radios { name: "dat_sync_platforms".to_string(), options, current: mode.clone(), on_change: move |x: String| { let mut m = cur(); m["mode"] = json!(x); m["extras"] = json!(extras().into_iter().collect::<Vec<String>>()); cur.set(m); } }
        if mode == "manual" {
            super::widgets::PlatformPicker { checked, offered: None, on_change: move |_| { let mut m = cur(); m["platforms"] = json!(checked().into_iter().collect::<Vec<u64>>()); cur.set(m); } }
        }
        super::widgets::DatExtrasPicker { extras, on_change: move |_| { let mut m = cur(); m["extras"] = json!(extras().into_iter().collect::<Vec<String>>()); cur.set(m); } }
    }
}

/// The editor for a known key over `cur` (its editing state).
#[component]
fn Editor(name: String, cur: Signal<Value>) -> Element {
    let key = name;
    let mut cur = cur;
    let v = cur();
    let set = move |x: Value| {
        let mut c = cur;
        c.set(x);
    };
    let sv = |d: &str| v.as_str().unwrap_or(d).to_string();
    // A short hint stays in view; anything longer is the row's "!" (the server's `explain`),
    // so a setting has one "!" and not one per note.
    let muted = |t: &str| {
        let t = state::tr(t);
        if t.chars().count() >= 100 {
            rsx! {}
        } else {
            rsx! { div { class: "muted", "{t}" } }
        }
    };
    let radios = |opts: &[(&str, &str)], current: String| {
        let options: Vec<(String, String)> = opts.iter().map(|(a, b)| (a.to_string(), state::tr(b))).collect();
        rsx! { Radios { name: key.clone(), options, current, on_change: move |x: String| set(json!(x)) } }
    };
    match key.as_str() {
        "scan_parallel_files" | "copy_parallel_files" => rsx! { ParallelEditor { name: key.clone(), cur } },
        "dat_sync_platforms" => rsx! { DatPlatformsEditor { cur } },
        "provider_priority" => {
            let items = str_list(&v);
            let labels = items.iter().map(|p| format!("{}{}", html::provider_icon(p), esc(p))).collect();
            rsx! {
                {muted("Drag (or use the arrows) — first wins when providers overlap, and this is the query order.")}
                OrderList { items, labels, on_change: move |o: Vec<String>| set(json!(o)) }
            }
        }
        "provider_sync_scope" => {
            let scopes = ["single", "list", "full", "by_platform", "by_keyword"];
            rsx! {
                table { class: "grid",
                    thead { tr { th { {state::tr("provider")} } th { {state::tr("scope")} } } }
                    tbody {
                        for p in vocab_strs("/jobs/providers") {
                            {
                                let current = s(&v, &p);
                                let mut options = vec![(String::new(), state::tr("(default)"))];
                                options.extend(scopes.iter().map(|x| (x.to_string(), x.to_string())));
                                let p2 = p.clone();
                                rsx! {
                                    tr {
                                        td { Html { html: html::provider_icon(&p) } "{p}" }
                                        td { Pulldown { options, current, on_change: move |x: String| { let mut m = cur(); if let Some(o) = m.as_object_mut() { if x.is_empty() { o.remove(&p2); } else { o.insert(p2.clone(), json!(x)); } } cur.set(m); } } }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        "region_priority" => {
            let items = str_list(&v);
            let labels = items.iter().map(|c| region_label(c)).collect();
            let add_options = codes("regions").into_iter().map(|c| (c.clone(), c)).collect();
            rsx! {
                AddableList { items, labels, add_options, add_label: state::tr("add region"), on_change: move |o: Vec<String>| set(json!(o)) }
            }
        }
        "language_priority" => {
            let items = str_list(&v);
            let labels = items.iter().map(|c| lang_label(c)).collect();
            let add_options = api::arr(&state::vocab(), "languages").iter().map(|l| (s(l, "code"), format!("{} — {}", s(l, "code"), s(l, "english")))).collect();
            rsx! {
                {muted("Tie-break after region for which release of a title comes first (then RetroAchievements support).")}
                AddableList { items, labels, add_options, add_label: state::tr("add language"), on_change: move |o: Vec<String>| set(json!(o)) }
            }
        }
        "canonical_name_priority" => {
            let items = str_list(&v);
            let regions: Vec<String> = codes("regions").iter().map(|c| spell(c)).collect();
            let labels = items
                .iter()
                .map(|c| {
                    if regions.contains(c) && *c == c.to_uppercase() {
                        let code = unspell(c);
                        with_flag(html::region_flag(&code), &format!("{} {c} — {}", state::tr("region"), state::region_name(&code)))
                    } else {
                        with_flag(html::lang_flag(c), &format!("{} {c} — {}", state::tr("language"), state::language_label(c)))
                    }
                })
                .collect();
            let mut add_options: Vec<(String, String)> = regions.iter().map(|c| (c.clone(), format!("{} {c}", state::tr("region")))).collect();
            add_options.extend(api::arr(&state::vocab(), "languages").iter().map(|l| (s(l, "code"), format!("{} {} — {}", state::tr("language"), s(l, "code"), s(l, "english")))));
            rsx! {
                AddableList { items, labels, add_options, add_label: state::tr("add"), on_change: move |o: Vec<String>| set(json!(o)) }
            }
        }
        "raw_response_retention" => radios(&[("keep_all", "keep all"), ("keep_latest", "keep latest"), ("discard", "discard")], sv("keep_latest")),
        "merge_review_order" => radios(&[("regional", "regional cases first (default)"), ("english", "titles sharing an IGDB game first")], sv("regional")),
        "gemini_model" => rsx! {
            label { input { r#type: "text", placeholder: state::tr("newest Flash model (default)"), value: "{sv(\"\")}", oninput: move |e| set(json!(e.value().trim())) } }
        },
        "cache_ttl_days" => rsx! {
            label { input { r#type: "number", min: "1", value: "{v}", oninput: move |e| set(json!(e.value().parse::<i64>().unwrap_or(1).max(1))) } " " span { class: "muted", {state::tr("days")} } }
        },
        "language_display" => radios(&[("code", "code (en, ja)"), ("english", "English name (English, Japanese)"), ("native", "native name (English, 日本語)")], sv("code")),
        "grid_media_type" => {
            let options = vocab_strs("/media_types").into_iter().map(|m| (m.clone(), m)).collect();
            rsx! { Pulldown { options, current: sv("box"), on_change: move |x: String| set(json!(x)) } }
        }
        "ra_include_beaten_default" => rsx! {
            {muted("RetroAchievements tab, In progress: whether beaten games are listed by default.")}
            Radios { name: key.clone(), options: vec![("true".into(), state::tr("include beaten (default)")), ("false".into(), state::tr("exclude beaten"))], current: if v == json!(false) { "false".to_string() } else { "true".to_string() }, on_change: move |x: String| set(json!(x == "true")) }
        },
        "allow_remote_launch" => rsx! {
            Radios { name: key.clone(), options: vec![("false".into(), state::tr("only from this machine (default)")), ("true".into(), state::tr("also from other machines"))], current: if v == json!(true) { "true".to_string() } else { "false".to_string() }, on_change: move |x: String| set(json!(x == "true")) }
        },
        "audio_dsd_cache_format" => rsx! {
            {radios(&[("aac", "AAC, 256 kbps: small, plays everywhere (default)"), ("flac", "FLAC: lossless, several times larger")], sv("aac"))}
        },
        "audio_client_format" => rsx! {
            {radios(&[("aac", "AAC: plays everywhere (default on Linux)"), ("xhe_aac", "xHE-AAC: smallest; Windows 11, macOS, iOS, Android (default elsewhere)"), ("none", "no conversion: copies are the original files")], sv("aac"))}
        },
        "platform_priority" => {
            let items: Vec<String> = v.as_array().into_iter().flatten().filter_map(Value::as_u64).map(|id| id.to_string()).collect();
            let labels = items.iter().map(|id| priority_label(id.parse().unwrap_or(0))).collect();
            rsx! {
                {muted("Collection › Cross-platform: which platform's title leads a game's row, and the order its platforms are shown in. One list for every era — a row only compares the platforms its game is on. Platforms not listed come after, by name.")}
                {muted("PC (Definitive tab): where a PC version ranks on Collection › Definitive, when it is less than definitive_pc_max_age years older than the newest version.")}
                PlatformPriority { items, labels, on_change: move |o: Vec<String>| set(json!(o.iter().filter_map(|id| id.parse::<u64>().ok()).collect::<Vec<_>>())) }
            }
        }
        "definitive_two_cover_layout" => rsx! {
            {muted("Collection › Definitive, two-cover grid: how the definitive version's and the base game's covers share a card. Each cover is cropped to fill its half.")}
            {radios(&[("top_bottom", "top / bottom (default)"), ("diagonal", "diagonal (top left / bottom right)"), ("left_right", "left / right")], sv("top_bottom"))}
        },
        "collection_default_tab" => {
            let hidden = state::collection_hidden_tabs();
            let opts: Vec<(&str, &str)> = COLLECTION_TABS.iter().map(|(k, l)| (*k, if *k == "titles" { "Titles (default)" } else { *l })).filter(|(k, _)| !hidden.iter().any(|h| h == k)).collect();
            rsx! {
                {muted("Which sub-tab the Collection tab opens on.")}
                {radios(&opts, sv("titles"))}
            }
        }
        "collection_hidden_tabs" => {
            let hidden = str_list(&v);
            rsx! {
                {muted("Which Collection sub-tabs are shown. Titles and Releases are always shown.")}
                div { class: "radios",
                    for (tab, label) in COLLECTION_TABS {
                        {
                            let fixed = tab == "titles" || tab == "releases";
                            let checked = fixed || !hidden.iter().any(|h| h == tab);
                            let label = state::tr(label);
                            rsx! { label { input { r#type: "checkbox", checked, disabled: fixed, onchange: move |e| { let mut list: Vec<String> = str_list(&cur()).into_iter().filter(|h| h != tab).collect(); if !e.checked() { list.push(tab.to_string()); } list.sort_by_key(|h| COLLECTION_TABS.iter().position(|(k, _)| k == h)); cur.set(json!(list)); } } " {label}" } }
                        }
                    }
                }
            }
        }
        "copy_definitive" => rsx! {
            {muted("A set's definitive-version entry (added from Collection › Definitive): which release it puts in the set.")}
            {radios(&[("best_held", "the best version you hold, in the family's order (default)"), ("definitive_only", "the definitive version only: nothing of that game when a better version isn't held")], sv("best_held"))}
        },
        "definitive_fetch_versions" => rsx! {
            {muted("Collection › Definitive: where a game's other versions (older and newer, the base game too) come from.")}
            {radios(&[("off", "off: only what is here — the catalogue, and IGDB platforms you synced (default)"), ("per_title", "per title: for games held on disk, fetch their versions from IGDB, on any platform"), ("whole_platform", "whole platform (deprecated): per title, and sync each platform such a version is on")], sv("off"))}
        },
        "audio_fallback" => rsx! {
            {muted("What a title with no audio linked to it plays. generic_bgm is a tag of its own: put it on audio files from the Audio tab; a title cannot carry it.")}
            {radios(&[("none", "nothing: silent (default)"), ("random", "random tracks from the audio linked to other titles"), ("generic_bgm", "the audio files tagged generic_bgm")], sv("none"))}
        },
        "audio_title_sources" => rsx! {
            {muted("Where a title page's music comes from. ROM music is played from the ROM itself, like a CD player: run Index ROM audio on the Audio tab first.")}
            {radios(&[("both", "tagged audio files and the music in the title's ROMs (default)"), ("tagged", "tagged audio files only"), ("rom", "the music in the title's ROMs only")], sv("both"))}
        },
        "audio_rom_kinds" => rsx! {
            {muted("Which sounds from a ROM a title page plays. Sounds are sorted by whether they loop and how long they are.")}
            {radios(&[("bgm", "music only (default)"), ("bgm_jingle", "music and jingles"), ("all", "everything, sound effects too")], sv("bgm"))}
        },
        "audio_rom_not_owned" => rsx! {
            {muted("ROM music for a title with no ROM here. When this finds nothing, audio_fallback decides.")}
            {radios(&[("related", "another version you have, such as the original of a remake (default)"), ("random", "another title's, picked at random"), ("none", "none")], sv("related"))}
        },
        "release_edition_order" => {
            let items = edition_order(&v);
            let labels = items.iter().map(|k| esc(&state::tr(edition_label(k)))).collect();
            rsx! {
                {muted("Which edition of a game comes first within one region / language (your manual order on a title page beats this); within one edition, the latest version. A budget reissue is usually the latest data of the original release; a re-release is the game on later hardware or a service.")}
                OrderList { items, labels, on_change: move |o: Vec<String>| set(json!(o)) }
            }
        }
        "release_kinds_default" => rsx! {
            {radios(&[("all", "all releases, incl. patches & subsets (default)"), ("patches", "commercial + patches"), ("commercial", "commercial only")], sv("all"))}
        },
        "release_unlicensed" => rsx! {
            {muted("Unlicensed releases (Unl) with no other tag: a kind and Titles row of their own, or listed with the title's retail releases. An unlicensed demo or pirate copy stays a demo or pirate copy.")}
            {radios(&[("separate", "a kind of their own (default)"), ("retail", "with retail")], sv("separate"))}
        },
        "ra_base_filter_default" => rsx! {
            {muted("RetroAchievements tab: which base filter the lists start with (switchable on the page).")}
            {radios(&[("console", "by console (default)"), ("genre", "by genre")], sv("console"))}
        },
        "ra_source_default" => rsx! {
            Pulldown { options: vec![("all".into(), state::tr("all RetroAchievements games, local first (default)")), ("local".into(), state::tr("local only"))], current: sv("all"), on_change: move |x: String| set(json!(x)) }
        },
        "view_defaults" => {
            let lists: [(&str, &str, &[&str]); 6] = [
                ("titles", "Collection › Titles", &["list", "grid"]),
                ("releases", "Collection › Releases", &["list", "grid"]),
                ("games", "Collection › Games", &["list", "grid"]),
                ("title_releases", "a title’s releases", &["list", "grid"]),
                ("sets", "sets", &["list", "grid"]),
                ("ra", "RetroAchievements lists", &["list", "grid", "icon", "title", "screenshot", "badges"]),
            ];
            rsx! {
                {muted("Default view of each list; a view picked on a page lasts for the session.")}
                table { class: "grid", tbody {
                    for (k, label, views) in lists {
                        {
                            let def = if k == "ra" { "badges" } else { "grid" };
                            let options: Vec<(String, String)> = views.iter().map(|x| (x.to_string(), if *x == def { format!("{x} {}", state::tr("(default)")) } else { x.to_string() })).collect();
                            let current = api::so(&v, k).unwrap_or_else(|| def.to_string());
                            let label = state::tr(label);
                            rsx! {
                                tr {
                                    td { "{label}" }
                                    td { Pulldown { options, current, on_change: move |x: String| { let mut m = cur(); if let Some(o) = m.as_object_mut() { if x == def { o.remove(k); } else { o.insert(k.to_string(), json!(x)); } } cur.set(m); } } }
                                }
                            }
                        }
                    }
                } }
            }
        }
        "media_thumb_format" => rsx! {
            {radios(&[("avif", "AVIF"), ("jpeg", "JPEG"), ("webp", "WebP (→ JPEG)"), ("jxl", "JPEG XL (→ JPEG)")], sv("avif"))}
        },
        "temp_dir_margin" => {
            let gib = format!("{:.2}", u(&v, "min_bytes") as f64 / 1073741824.0);
            rsx! {
                div { class: "toolbar",
                    label { class: "muted", {state::tr("percent")} " " input { r#type: "number", min: "0", max: "90", step: "1", value: "{s(&v, \"percent\")}", oninput: move |e| { let mut m = cur(); m["percent"] = json!(e.value().parse::<f64>().unwrap_or(0.0)); cur.set(m); } } }
                    label { class: "muted", {state::tr("min GiB")} " " input { r#type: "number", min: "0", step: "0.5", value: "{gib}", oninput: move |e| { let mut m = cur(); m["min_bytes"] = json!((e.value().parse::<f64>().unwrap_or(0.0) * 1073741824.0).round() as u64); cur.set(m); } } }
                }
            }
        }
        "title_display_mode" => {
            let custom = v.is_object();
            let mode = if custom { "custom".to_string() } else { sv("english") };
            let langs = if custom { str_list(v.get("languages").unwrap_or(&Value::Null)) } else { vec!["en".to_string()] };
            let labels = langs.iter().map(|c| lang_label(c)).collect();
            let add_options = api::arr(&state::vocab(), "languages").iter().map(|l| (s(l, "code"), format!("{} — {}", s(l, "code"), s(l, "english")))).collect();
            let langs2 = langs.clone();
            rsx! {
                Radios { name: key.clone(), options: vec![("english".into(), state::tr("off — romanized names")), ("local".into(), state::tr("on — localized names")), ("custom".into(), state::tr("custom language priority"))], current: mode.clone(),
                    on_change: move |x: String| set(if x == "custom" { json!({ "languages": langs2.clone() }) } else { json!(x) }) }
                if custom {
                    div { style: "margin-top:6px",
                        AddableList { items: langs, labels, add_options, add_label: state::tr("add language"), on_change: move |o: Vec<String>| set(json!({ "languages": o })) }
                    }
                }
            }
        }
        "metadata_source_priority" => rsx! {
            for f in META_FIELDS {
                {
                    let items = str_list(v.get(f).unwrap_or(&Value::Null));
                    let providers = vocab_strs("/jobs/providers");
                    let labels = items.iter().map(|x| format!("{}{}", if providers.contains(x) { html::provider_icon(x) } else { html::cbadge("source", x, None, None) }, esc(x))).collect();
                    rsx! {
                        div { style: "margin:6px 0", b { "{f}" }
                            OrderList { items, labels, on_change: move |o: Vec<String>| { let mut m = cur(); m[f] = json!(o); cur.set(m); } }
                        }
                    }
                }
            }
        },
        "ui_colors" => rsx! {
            {muted("Pick a color to override the built-in default for that value; \"default\" removes the override.")}
            for (kind, values) in color_values() {
                details {
                    summary { "{kind} " span { class: "muted", "({values.len()})" } }
                    // Columns of small cells rather than a table row each: 82 platforms fit on a screen.
                    div { class: "colorgrid",
                        for val in values {
                            {
                                let over = v.get(kind).and_then(|m| m.get(&val)).and_then(|c| c.as_str()).map(str::to_string);
                                let def = html::hsl_to_hex(&html::default_color_of(kind, &val).unwrap_or_else(|| "#888888".into()));
                                let shown = over.clone().unwrap_or_else(|| def.clone());
                                let sample = match kind { "platform" => html::cbadge("platform", &val, None, None), "region" => html::region_badges(&val), "language" => html::lang_badges(&val), k => html::cbadge(k, &val, None, None) };
                                let (v1, v2) = (val.clone(), val.clone());
                                rsx! {
                                    div { class: "citem", key: "{val}",
                                        span { class: "clabel", Html { html: sample } }
                                        input { r#type: "color", value: "{shown}", oninput: move |e| { let mut m = cur(); if !m[kind].is_object() { m[kind] = json!({}); } m[kind][&v1] = json!(e.value()); cur.set(m); } }
                                        if over.is_some() { Html { html: badge(&state::tr("override"), "warn") } }
                                        button { class: "btn small", r#type: "button", title: state::tr("back to the built-in color"), onclick: move |_| { let mut m = cur(); if let Some(o) = m.get_mut(kind).and_then(|x| x.as_object_mut()) { o.remove(&v2); } cur.set(m); }, "↺" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        },
        "ui_icons" => rsx! { IconsEditor { cur } },
        "ui_language" | "platform_name_language" => rsx! {
            if key == "platform_name_language" { {muted("The language platform names are shown in, independent of the UI language.")} }
            Pulldown {
                options: if key == "ui_language" { super::language::ui_languages().into_iter().map(|c| { let t = super::language::choice_text(&c); (c, t) }).collect() } else { vec![("en".into(), "English".into()), ("ja".into(), "日本語 (Japanese)".into())] },
                current: sv("en"), on_change: move |x: String| set(json!(x))
            }
        },
        "sheet_style" => rsx! {
            {muted("How a disc's sheet is written and read: the cue/gdi rebuilt from a CHD when it is scanned, copied, converted or extracted, and the sheet kind of a GD-ROM. Redump (default, CRLF line endings, GD-ROMs as a cue) is the one every DAT is written against, so it is the only one a rebuild can be checked against. A loose cue on disk is always hashed as it is.")}
            {radios(&[("redump", "redump (default)"), ("redump_catalog", "redump, catalogue spelling"), ("chdman", "chdman"), ("mame", "MAME")], sv("redump"))}
        },
        "chd_reader" => rsx! {
            {muted("Who reads a CHD's header and track list.")}
            {radios(&[("checked", "built-in, checked against chdman (default)"), ("chdman", "chdman only"), ("rust", "built-in only (not recommended)")], sv("checked"))}
            if v.as_str() == Some("rust") {
                div { class: "warnbox", style: "border-left-color:#d33; font-weight:600",
                    {state::tr("Not recommended yet. The built-in reader is new, and with nothing to compare it against, a mistake in it would go unnoticed: wrong tracks recorded for a disc, and sheets rebuilt wrong from them. Use the default unless you need the speed, and switch back if anything looks off.")}
                }
            }
        },
        "copy_populate_hash" | "integrity_populate_hash" => rsx! {
            if key == "integrity_populate_hash" { {muted("The hash the integrity check compares against; where a file's content has no such hash yet, it falls back to the cheapest recorded one and records this one.")} }
            Pulldown { options: QUICK_HASHES.iter().map(|h| (h.to_string(), if *h == "xxh3_128" { format!("{h} {}", state::tr("(default)")) } else { h.to_string() })).collect(), current: sv("xxh3_128"), on_change: move |x: String| set(json!(x)) }
        },
        "backup_dir" => {
            let current = sv("");
            rsx! {
                {muted("Where backup zips are written. Empty = data/backups inside the install.")}
                input { r#type: "text", style: "min-width:420px", placeholder: "data/backups", value: "{current}", oninput: move |e| set(json!(e.value())) }
            }
        }
        "scan_hashes" => rsx! {
            div { class: "radios",
                for (name, _) in SCAN_HASHES {
                    label { input { r#type: "checkbox", checked: v.get(*name).and_then(Value::as_bool).unwrap_or(false), onchange: move |e| { let mut m = cur(); m[*name] = json!(e.checked()); cur.set(m); } } " {name}" }
                }
            }
        },
        "serial_match" => rsx! {
            {muted("Files no DAT hash matched, matched by the serial in their header instead. Formats a DAT hashes are off by default.")}
            div { class: "radios",
                for (name, _, label) in SERIAL_MATCH {
                    label { input { r#type: "checkbox", checked: v.get(*name).and_then(Value::as_bool).unwrap_or(false), onchange: move |e| { let mut m = cur(); m[*name] = json!(e.checked()); cur.set(m); } } " {state::tr(label)}" }
                }
            }
            // The console's same table: per platform, a serial in the file name or not; a
            // platform left at "(as above)" follows the file name box.
            {muted("A serial in the file name, per platform:")}
            table { class: "grid",
                for id in SERIAL_FILE_NAME_PLATFORMS {
                    {
                        let key = id.to_string();
                        let chosen = match v.pointer(&format!("/file_name_platforms/{id}")).and_then(Value::as_bool) {
                            Some(true) => "true",
                            Some(false) => "false",
                            None => "",
                        };
                        rsx! {
                            tr {
                                td { "{state::platform_name(*id as u64)}" }
                                td {
                                    select { value: "{chosen}",
                                        onchange: move |e| {
                                            let mut m = cur();
                                            if !m["file_name_platforms"].is_object() { m["file_name_platforms"] = json!({}); }
                                            match e.value().as_str() {
                                                "" => { if let Some(o) = m["file_name_platforms"].as_object_mut() { o.remove(&key); } }
                                                on => m["file_name_platforms"][&key] = json!(on == "true"),
                                            }
                                            cur.set(m);
                                        },
                                        option { value: "", selected: chosen.is_empty(), {state::tr("(as above)")} }
                                        option { value: "true", selected: chosen == "true", {state::tr("use it")} }
                                        option { value: "false", selected: chosen == "false", {state::tr("don't use it")} }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        },
        "scan_heuristics" => rsx! {
            {muted("Which guesses a scan may make about a file's platform when nothing identifies it.")}
            div { class: "radios",
                for (name, label) in [("ext", state::tr("by file extension")), ("path", state::tr("by the directory names above it"))] {
                    label { input { r#type: "checkbox", checked: v.get(name).and_then(Value::as_bool).unwrap_or(true), onchange: move |e| { let mut m = cur(); m[name] = json!(e.checked()); cur.set(m); } } " {label}" }
                }
            }
        },
        "media_download" => {
            let providers = auto_media_providers();
            rsx! {
                table { class: "grid",
                    for (provider, types) in providers {
                        tr {
                            td { Html { html: format!("{}{}", html::provider_icon(&provider), esc(&provider)) } }
                            td {
                                div { class: "radios",
                                    for media_type in types {
                                        {
                                            let checked = v.pointer(&format!("/{provider}/{media_type}")).and_then(Value::as_bool).unwrap_or(false);
                                            let (p, mt) = (provider.clone(), media_type.clone());
                                            rsx! { label { input { r#type: "checkbox", checked, onchange: move |e| { let mut m = cur(); m[p.as_str()][mt.as_str()] = json!(e.checked()); cur.set(m); } } " {media_type}" } }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        "media_priority" => {
            let rows: Vec<(String, Vec<String>)> = vocab_strs("/media/types").into_iter().map(|t| { let order = str_list(&v[t.as_str()]); (t, order) }).filter(|(_, order)| order.len() > 1).collect();
            rsx! {
                {muted("Per media type, whose comes first: the providers and your own uploads. For media a game has one of, the first that has one is used, and a lower provider's is not downloaded once a higher source has it. Your uploads are first unless you move them down. Screenshots, fan art and videos come from every provider; the order only decides which are shown first.")}
                if rows.is_empty() { {muted("No media type is offered by more than one provider yet.")} }
                for (media_type, order) in rows {
                    {
                        let labels = order.iter().map(|p| if p == "manual" { format!("<b>{}</b>", esc(&state::tr("your uploads"))) } else { format!("{}{}", html::provider_icon(p), esc(p)) }).collect();
                        let mt = media_type.clone();
                        rsx! {
                            div { style: "margin-top:6px", b { "{media_type}" } }
                            OrderList { items: order, labels, on_change: move |o: Vec<String>| { let mut m = cur(); m[mt.as_str()] = json!(o); cur.set(m); } }
                        }
                    }
                }
            }
        }
        "media_export_priority" => {
            let slot_label = |slot: &str| match slot {
                "box" => state::tr("box (Named_Boxarts, covers)"),
                "title" => state::tr("title screen (Named_Titles)"),
                "snap" => state::tr("screenshot (Named_Snaps)"),
                _ => state::tr("logo (Named_Logos)"),
            };
            rsx! {
                {muted("Export artwork: per slot, whose picture an emulator gets, best first. A box scan before a poster: TheGamesDB and MobyGames come before SteamGridDB by default.")}
                for slot in EXPORT_SLOTS {
                    {
                        let order = str_list(&v[slot]);
                        let labels = order.iter().map(|p| if p == "manual" { format!("<b>{}</b>", esc(&state::tr("your uploads"))) } else { format!("{}{}", html::provider_icon(p), esc(p)) }).collect();
                        rsx! {
                            div { style: "margin-top:6px", b { "{slot_label(slot)}" } }
                            OrderList { items: order, labels, on_change: move |o: Vec<String>| { let mut m = cur(); m[slot] = json!(o); cur.set(m); } }
                        }
                    }
                }
            }
        }
        "media_show" | "media_client_keep" => {
            let keep = key == "media_client_keep";
            let types = if keep { keepable_media_types() } else { vocab_strs("/media_types") };
            rsx! {
                if keep {
                    div { class: "warnbox", {state::tr("Caution: a downloaded video is kept in its original format (nothing here converts video), so it takes its full size. Streaming is recommended for video on a client. bgm (audio) off means the player streams the server's tracks; on, the audio_sync job fetches a copy of every tagged track in the format audio_client_format names.")} }
                } else {
                    {muted("Which media title and release pages show in their media strip. bgm is the audio player.")}
                }
                div { class: "radios",
                    for media_type in types {
                        {
                            let checked = v.get(&media_type).and_then(Value::as_bool).unwrap_or(!(keep && (media_type == "video" || media_type == "bgm")));
                            let mt = media_type.clone();
                            rsx! { label { input { r#type: "checkbox", checked, onchange: move |e| { let mut m = cur(); m[mt.as_str()] = json!(e.checked()); cur.set(m); } } " {media_type}" } }
                        }
                    }
                }
            }
        }
        "copy_recent_destinations" => {
            let items = str_list(&v);
            let labels = items.iter().map(|d| esc(d)).collect();
            rsx! {
                OrderList { items, labels, removable: true, on_change: move |o: Vec<String>| set(json!(o)) }
            }
        }
        k if BOOL_KEYS.iter().any(|(b, ..)| *b == k) => {
            let (_, _, on, off) = BOOL_KEYS.iter().find(|(b, ..)| *b == k).copied().unwrap();
            let current = if v == json!(true) { "true" } else { "false" }.to_string();
            rsx! { Radios { name: key.clone(), options: vec![("true".into(), state::tr(on)), ("false".into(), state::tr(off))], current, on_change: move |x: String| set(json!(x == "true")) } }
        }
        k => match num_spec(k) {
            Some((_, min, max, unit, note)) => rsx! {
                {muted(note)}
                label {
                    input { r#type: "number", min: "{min}", max: if let Some(m) = max { m.to_string() } else { String::new() }, step: "1", value: "{v}", oninput: move |e| set(json!(e.value().parse::<f64>().unwrap_or(0.0).trunc() as i64)) }
                    if !unit.is_empty() { span { class: "muted", " " {state::tr(unit)} } }
                }
            },
            None => rsx! {},
        },
    }
}

#[component]
fn IconsEditor(cur: Signal<Value>) -> Element {
    let mut cur = cur;
    let v = cur();
    let mut kind = use_signal(|| "platform".to_string());
    let values = color_values().into_iter().find(|(k, _)| *k == kind()).map(|(_, v)| v).unwrap_or_default();
    let mut val = use_signal(String::new);
    let chosen = if values.contains(&val()) { val() } else { values.first().cloned().unwrap_or_default() };
    let rows: Vec<(String, String, String)> = v.as_object().into_iter().flatten().flat_map(|(k, m)| m.as_object().into_iter().flatten().map(move |(x, url)| (k.clone(), x.clone(), url.as_str().unwrap_or("").to_string()))).collect();
    rsx! {
        table { class: "grid",
            thead { tr { th { {state::tr("kind")} } th { {state::tr("value")} } th { {state::tr("url")} } th {} } }
            tbody {
                for (k, x, url) in rows {
                    {
                        let (k1, x1, k2, x2) = (k.clone(), x.clone(), k.clone(), x.clone());
                        rsx! {
                            tr {
                                td { "{k}" } td { "{x}" }
                                td { input { r#type: "text", style: "min-width:320px", value: "{url}", oninput: move |e| { let mut m = cur(); m[&k1][&x1] = json!(e.value()); cur.set(m); } } }
                                td { button { class: "btn small danger", r#type: "button", onclick: move |_| { let mut m = cur(); if let Some(o) = m.get_mut(&k2).and_then(|y| y.as_object_mut()) { o.remove(&x2); } cur.set(m); }, "×" } }
                            }
                        }
                    }
                }
            }
        }
        div { class: "toolbar",
            select { onchange: move |e| kind.set(e.value()), for k in ["platform", "region", "language", "source"] { option { selected: k == kind(), "{k}" } } }
            select { onchange: move |e| val.set(e.value()), for x in values { option { selected: x == chosen, "{x}" } } }
            button { class: "btn small", r#type: "button",
                onclick: move |_| {
                    let (k, x) = (kind(), chosen.clone());
                    let mut m = cur();
                    if !m.is_object() { m = json!({}); }
                    if !m[&k].is_object() { m[&k] = json!({}); }
                    if m[&k].get(&x).is_none() { m[&k][&x] = json!(""); cur.set(m); }
                },
                {state::tr("add")}
            }
        }
    }
}

/// One setting. `json`: the raw-JSON page, where every value is a textarea; otherwise the
/// form, where a key without an editor only says where it can be edited.
#[component]
fn SettingCard(setting: Value, json: bool, on_change: EventHandler<()>) -> Element {
    let key = s(&setting, "key");
    let value = setting.get("value").cloned().unwrap_or(Value::Null);
    let elsewhere = edited_elsewhere(&key);
    // Not settable here -- the server refuses the write (the mode has its own screen, where
    // the consent to replacing the catalogue lives; the id is generated once and never set)
    // -- or, on the form, edited on the screen that owns the whole feature.
    if key == "instance_id" || (elsewhere.is_some() && (!json || key.starts_with("instance_") || key == "catalog_from")) {
        let shown = if value.is_null() { state::tr("(unset)") } else { value.to_string() };
        return rsx! {
            div { class: "card",
                h3 { "{key} " Html { html: if value.is_null() { badge(&state::tr("default"), "dim") } else { badge(&state::tr("set"), "ok") } } }
                div { class: "muted", {state::tr(&s(&setting, "description"))} }
                div { class: "editor",
                    span { class: "mono", "{shown}" }
                    " "
                    if key == "instance_id" { span { class: "muted", {state::tr("this installation's identity, generated once")} } }
                    else if let Some((to, label)) = elsewhere { span { class: "muted", {state::tr("set under ")} Link { to, {state::tr(label)} } } }
                }
            }
        };
    }
    let known = EDITORS.contains(&key.as_str());
    if !json && !known {
        return rsx! {
            div { class: "card",
                h3 { "{key} " Html { html: if value.is_null() { badge(&state::tr("default"), "dim") } else { badge(&state::tr("set"), "ok") } } }
                div { class: "muted", {state::tr(&s(&setting, "description"))} }
                div { class: "editor", span { class: "muted", {state::tr("no form for this one yet: edit it under ")} Link { to: crate::Route::ConfigSettingsJson {}, {state::tr("Settings (JSON)")} } } }
            }
        };
    }
    // An unset key whose default depends on the server's build starts on that default.
    let (k0, v0) = (key.clone(), if value.is_null() { setting.get("default").cloned().unwrap_or(Value::Null) } else { value.clone() });
    let cur = use_signal(move || initial(&k0, &v0));
    let raw = use_signal(|| json);
    let v1 = value.clone();
    let mut text = use_signal(move || if v1.is_null() { String::new() } else { serde_json::to_string_pretty(&v1).unwrap_or_default() });
    let (k_save, k_reset) = (key.clone(), key.clone());
    if !json {
        return rsx! { SettingRow { setting, key_name: key, cur, on_change } };
    }
    rsx! {
        div { class: "card",
            h3 { "{key} " Html { html: if value.is_null() { badge(&state::tr("default"), "dim") } else { badge(&state::tr("set"), "ok") } } }
            div { class: "muted", {state::tr(&s(&setting, "description"))} }
            if !raw() { div { class: "editor", Editor { name: key.clone(), cur } } }
            if raw() { textarea { value: "{text}", oninput: move |e| text.set(e.value()) } }
            div { class: "toolbar actions",
                button { class: "btn primary",
                    onclick: move |_| {
                        let v = if raw() {
                            if text().trim().is_empty() { Value::Null } else {
                                match serde_json::from_str::<Value>(&text()) { Ok(v) => v, Err(_) => { ui::toast(state::tr("not valid JSON"), true); return; } }
                            }
                        } else { finalize(&k_save, &cur()) };
                        let key = k_save.clone();
                        spawn(async move {
                            let r = if v.is_null() { api::delete(&format!("/api/v1/settings/{}", api::encode(&key))).await } else { api::put(&format!("/api/v1/settings/{}", api::encode(&key)), v.clone()).await };
                            match r {
                                Ok(_) => { ui::toast(if v.is_null() { state::trf("cleared {key}", &[("key", &key)]) } else { state::trf("saved {key}", &[("key", &key)]) }, false); state::reload_settings().await; on_change.call(()); }
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    },
                    {state::tr("save")}
                }
                button { class: "btn danger",
                    onclick: move |_| {
                        let key = k_reset.clone();
                        spawn(async move {
                            match api::delete(&format!("/api/v1/settings/{}", api::encode(&key))).await {
                                Ok(_) => { ui::toast(state::trf("reset {key}", &[("key", &key)]), false); state::reload_settings().await; on_change.call(()); }
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    },
                    {state::tr("reset to default")}
                }
                span { class: "muted", if let Some(t) = api::so(&setting, "updated_at") { {state::trf("updated {date}", &[("date", &html::fmt_date(&t))])} } }
            }
        }
    }
}

/// One setting on the form: its key, stored/default badge and a "!" for what it does; the
/// editor; save / reset. An editor that needs the width (`WIDE`) gets a line of its own.
#[component]
fn SettingRow(setting: Value, key_name: String, cur: Signal<Value>, on_change: EventHandler<()>) -> Element {
    let key = key_name;
    let value = setting.get("value").cloned().unwrap_or(Value::Null);
    let wide = WIDE.contains(&key.as_str());
    let (k_save, k_reset) = (key.clone(), key.clone());
    let description = s(&setting, "description");
    // The one "!": the setting's own explanation where it has one, else its description.
    let explain: Vec<String> = setting.get("explain").and_then(Value::as_array).map(|a| a.iter().filter_map(|p| p.as_str().map(str::to_string)).collect()).unwrap_or_default();
    let updated = api::so(&setting, "updated_at").map(|t| state::trf("updated {date}", &[("date", &html::fmt_date(&t))])).unwrap_or_default();
    rsx! {
        div { class: if wide { "setrow wide" } else { "setrow" },
            div { class: "setname", title: "{updated}",
                span { class: "mono", "{key}" } " "
                Html { html: if value.is_null() { badge(&state::tr("default"), "dim") } else { badge(&state::tr("set"), "ok") } }
                if !explain.is_empty() {
                    Explain { for p in explain.iter() { p { {state::tr(p)} } } }
                } else if !description.is_empty() {
                    Explain { {state::tr(&description)} }
                }
            }
            div { class: "editor", Editor { name: key.clone(), cur } }
            div { class: "setact",
                button { class: "btn small primary",
                    onclick: move |_| {
                        let v = finalize(&k_save, &cur());
                        let key = k_save.clone();
                        spawn(async move {
                            let r = if v.is_null() { api::delete(&format!("/api/v1/settings/{}", api::encode(&key))).await } else { api::put(&format!("/api/v1/settings/{}", api::encode(&key)), v.clone()).await };
                            match r {
                                Ok(_) => { ui::toast(if v.is_null() { state::trf("cleared {key}", &[("key", &key)]) } else { state::trf("saved {key}", &[("key", &key)]) }, false); state::reload_settings().await; on_change.call(()); }
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    },
                    {state::tr("save")}
                }
                button { class: "btn small danger", title: state::tr("reset to default"),
                    onclick: move |_| {
                        let key = k_reset.clone();
                        spawn(async move {
                            match api::delete(&format!("/api/v1/settings/{}", api::encode(&key))).await {
                                Ok(_) => { ui::toast(state::trf("reset {key}", &[("key", &key)]), false); state::reload_settings().await; on_change.call(()); }
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    },
                    {state::tr("reset")}
                }
            }
        }
    }
}

/// Editors that need the row's full width: lists, tables, grids of options.
const WIDE: &[&str] = &[
    "provider_priority", "provider_sync_scope", "region_priority", "canonical_name_priority", "language_priority", "metadata_source_priority", "ui_colors", "ui_icons",
    "platform_priority", "view_defaults", "media_download", "media_priority", "media_show", "media_client_keep", "copy_recent_destinations", "scan_parallel_files", "copy_parallel_files", "temp_dir_margin",
    "media_export_priority", "dat_sync_platforms", "collection_hidden_tabs",
];

/// The Collection sub-tabs, in their order on screen, with their labels.
pub const COLLECTION_TABS: [(&str, &str); 16] = [
    ("crossplatform", "Cross-platform"),
    ("definitive", "Definitive"),
    ("titles", "Titles"),
    ("releases", "Releases"),
    ("games", "Games"),
    ("roms", "Roms"),
    ("files", "Files"),
    ("contents", "Contents"),
    ("sets", "Sets"),
    ("audio", "Audio"),
    ("multimedia", "Multimedia"),
    ("documents", "Documents"),
    ("source", "Source code"),
    ("etc", "Etc"),
    ("patches", "Patches"),
    ("bios", "BIOS / firmware"),
];

thread_local! {
    /// The settings section last shown, for the app session.
    static SECTION: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// Config › Settings: every setting as a form.
#[component]
pub fn ConfigSettings() -> Element {
    rsx! { SettingsPage { json: false } }
}

/// Config › Settings (JSON): the same settings as raw JSON, plus any key the server does
/// not know about and a way to add one.
#[component]
pub fn ConfigSettingsJson() -> Element {
    rsx! { SettingsPage { json: true } }
}

#[component]
fn SettingsPage(json: bool) -> Element {
    let mut list = use_resource(|| async { api::get("/api/v1/settings").await.map(|v| api::items(&v)) });
    let mut new_key = use_signal(String::new);
    let mut new_val = use_signal(String::new);
    let mut gen = use_signal(|| 0u64);
    let filter = use_signal(String::new);
    // The section shown, remembered for the app session so a save or a visit elsewhere comes
    // back to it. Only one section at a time -- unless the filter has text, which searches all.
    let mut section = use_signal(|| SECTION.with(|s| s.borrow().clone()));
    let mut list_restart = move || list.restart();
    let items = match &*list.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(l)) => l.clone(),
    };
    let g = gen();
    let mut filter = filter;
    let q = filter().trim().to_lowercase();
    // Sections in the order the server lists them (it sorts by section), with every setting
    // for the tab counts; then only the chosen one, or the filter's matches across all.
    let mut all: Vec<(String, String, Vec<Value>)> = Vec::new();
    for x in items {
        let id = api::so(&x, "group").unwrap_or_else(|| "other".into());
        match all.iter_mut().find(|(g, ..)| *g == id) {
            Some((.., list)) => list.push(x),
            None => all.push((id, api::so(&x, "group_label").unwrap_or_else(|| "Other".into()), vec![x])),
        }
    }
    let active = if all.iter().any(|(g, ..)| *g == section()) { section() } else { all.first().map(|(g, ..)| g.clone()).unwrap_or_default() };
    let tabs: Vec<(String, String, usize)> = all.iter().map(|(g, l, list)| (g.clone(), l.clone(), list.len())).collect();
    let groups: Vec<(String, String, Vec<Value>)> = all
        .into_iter()
        .filter(|(g, ..)| !q.is_empty() || *g == active)
        .map(|(g, l, list)| (g, l, list.into_iter().filter(|x| q.is_empty() || format!("{} {}", s(x, "key"), s(x, "description")).to_lowercase().contains(&q)).collect::<Vec<_>>()))
        .filter(|(.., list)| !list.is_empty())
        .collect();
    let section_tabs = rsx! {
        nav { class: "subsub",
            for (g, l, n) in tabs {
                button { key: "{g}", class: if g == active { "active" } else { "" },
                    onclick: move |_| { section.set(g.clone()); SECTION.with(|s| *s.borrow_mut() = g.clone()); },
                    {state::tr(&l)} span { class: "n", "{n}" }
                }
            }
        }
    };
    if !json {
        return rsx! {
            {section_tabs}
            div { class: "card",
                h2 { {state::tr("Settings")} }
                Explain {
                    {state::tr("Changes apply after saving. \"reset to default\" removes the stored value. ")}
                    {state::tr("The ")} Link { to: crate::Route::ConfigSettingsJson {}, {state::tr("JSON view")} } {state::tr(" shows the stored values as they are.")}
                }
                div { class: "toolbar",
                    // Uncontrolled: the box keeps what is typed while the rows re-render, so fast typing
                    // never loses a character to a render that is still catching up.
                    input { r#type: "text", style: "min-width:240px", placeholder: state::tr("filter settings…"), oninput: move |e| filter.set(e.value()) }
                }
            }
            for (id, label, list) in groups {
                div { class: "card setgroup", key: "{id}",
                    h2 { {state::tr(&label)} }
                    div { class: "rows",
                        for x in list {
                            SettingCard { key: "{s(&x, \"key\")}-{s(&x, \"updated_at\")}-{g}", setting: x.clone(), json, on_change: move |_| { *gen.write() += 1; list_restart(); } }
                        }
                    }
                }
            }
        };
    }
    rsx! {
        div { class: "card",
            h2 { if json { {state::tr("Settings (JSON)")} } else { {state::tr("Settings")} } }
            Explain {
                {state::tr("Changes apply after saving. \"reset to default\" removes the stored value. ")}
                if json { {state::tr("Every value as the server stores it; the ")} Link { to: crate::Route::ConfigSettings {}, {state::tr("form")} } {state::tr(" covers the same keys.")} }
                else { {state::tr("The ")} Link { to: crate::Route::ConfigSettingsJson {}, {state::tr("JSON view")} } {state::tr(" shows the stored values as they are.")} }
            }
            {section_tabs}
            div { class: "toolbar",
                input { r#type: "text", style: "min-width:240px", placeholder: state::tr("filter settings…"), oninput: move |e| filter.set(e.value()) }
            }
            div {
                for x in groups.iter().flat_map(|(.., list)| list.clone()) {
                    // Keyed by the load and the stored value's time, so a saved card starts over from
                    // what was stored: the reload lands after the first render, and keying on the
                    // load alone kept the editor showing the value from before the save.
                    SettingCard { key: "{s(&x, \"key\")}-{s(&x, \"updated_at\")}-{g}", setting: x.clone(), json, on_change: move |_| { *gen.write() += 1; list.restart(); } }
                }
            }
            if json { form { class: "inline",
                onsubmit: move |e| {
                    e.prevent_default();
                    let v = match serde_json::from_str::<Value>(&new_val()) { Ok(v) => v, Err(_) => { ui::toast(state::tr("value must be JSON"), true); return; } };
                    let k = new_key();
                    spawn(async move {
                        match api::put(&format!("/api/v1/settings/{}", api::encode(&k)), v).await {
                            Ok(_) => { new_key.set(String::new()); new_val.set(String::new()); *gen.write() += 1; list.restart(); }
                            Err(e) => ui::toast(e.message, true),
                        }
                    });
                },
                label { {state::tr("new key")} input { r#type: "text", placeholder: state::tr("key"), value: "{new_key}", oninput: move |e| new_key.set(e.value()) } }
                label { {state::tr("JSON value")} input { r#type: "text", placeholder: state::tr("\"value\" or {\"a\":1}"), style: "min-width:300px", value: "{new_val}", oninput: move |e| new_val.set(e.value()) } }
                button { class: "btn", {state::tr("add")} }
            } }
        }
    }
}
