//! Display-only HTML fragments, ported from the web console's helpers (badges, flags,
//! platform glyphs, sizes, dates, hashes): the same markup and class names, so the shared
//! stylesheet renders them the same. Interactive parts are Dioxus components elsewhere.

use serde_json::Value;

use crate::state;

pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

pub fn fmt_size(n: Option<u64>) -> String {
    let Some(n) = n else { return String::new() };
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", units[i])
    }
}

/// A set's total in GiB to a tenth, however large: in TiB to a tenth, removing a few hundred
/// small releases from a 2 TiB set showed no change at all.
pub fn fmt_size_fine(n: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    if (n as f64) < GIB {
        return fmt_size(Some(n));
    }
    let tenths = ((n as f64 / GIB) * 10.0).round() as u64;
    let whole = (tenths / 10).to_string();
    let mut grouped = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{grouped}.{} GiB", tenths % 10)
}

pub fn fmt_date(s: &str) -> String {
    s.replace('T', " ").chars().take(19).collect()
}

pub fn hashes_html(h: Option<&Value>) -> String {
    let Some(obj) = h.and_then(|h| h.as_object()) else { return String::new() };
    obj.iter().map(|(k, v)| format!(r#"<div class="hash"><b>{}</b> {}</div>"#, esc(k), esc(v.as_str().unwrap_or_default()))).collect()
}

pub fn badge(text: &str, cls: &str) -> String {
    format!(r#"<span class="badge {cls}">{}</span>"#, esc(text))
}

// --- colors (DEFAULT_COLORS / FAMILY_PALETTES / colorOf / textOn) ---

fn hash_n(s: &str, n: usize) -> usize {
    let mut h: u32 = 0;
    for c in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(u32::from(c));
    }
    (h as usize) % n
}

fn default_color(kind: &str, value: &str) -> Option<&'static str> {
    Some(match (kind, value) {
        ("category", "base_game") => "#5b8def",
        ("category", "port") => "#f2a65a",
        ("category", "remake") => "#b070e0",
        ("category", "remaster") => "#46b8a0",
        ("category", "edition") => "#5fc4d8",
        ("category", "standalone") => "#d0907a",
        ("category", "dlc") => "#e0c05a",
        ("category", "expansion") => "#e07a7a",
        ("category", "bundle") => "#7fb2ff",
        ("category", "mod") => "#a0a0a0",
        ("archive_format", "raw_iso") => "#f4f4f0",
        ("archive_format", "chd") => "#cfe6ff",
        ("archive_format", "rvz") => "#ffb3b3",
        ("archive_format", "zar") => "#b8f0c0",
        ("archive_format", "gcz") => "#dcc8ff",
        ("archive_format", "wia") => "#ffc8e6",
        ("archive_format", "zip") | ("archive_format", "7z") => "#fff3a0",
        ("archive_format", "cso") => "#d0e0ff",
        ("archive_format", "pbp") => "#e0d0ff",
        ("archive_format", "xiso") => "#e0e0e0",
        ("archive_format", "unknown") => "#d8d8d8",
        ("source", "redump") => "#cfe0ff",
        ("source", "no-intro") => "#d6f5d6",
        ("source", "tosec") => "#ffe2c4",
        ("source", "retroachievements") => "#fff0b8",
        ("source", "retrobios") => "#e8e0ff",
        ("source", "mame-redump") => "#f0d6e8",
        ("source", "lost-level-archive") => "#d8f0ea",
        ("source", "hacks") => "#ffd6d6",
        ("source", "homebrew") => "#e2f0c8",
        ("source", "etc") => "#e4e4e4",
        _ => return None,
    })
}

const PAL_NINTENDO: [&str; 6] = ["hsl(0, 70%, 80%)", "hsl(350, 75%, 84%)", "hsl(330, 65%, 83%)", "hsl(12, 75%, 80%)", "hsl(340, 60%, 78%)", "hsl(0, 55%, 74%)"];
const PAL_SONY: [&str; 6] = ["hsl(220, 70%, 82%)", "hsl(205, 80%, 82%)", "hsl(188, 70%, 80%)", "hsl(174, 60%, 78%)", "hsl(230, 55%, 80%)", "hsl(196, 65%, 76%)"];
const PAL_MICROSOFT: [&str; 5] = ["hsl(120, 45%, 78%)", "hsl(140, 50%, 80%)", "hsl(100, 50%, 80%)", "hsl(160, 45%, 78%)", "hsl(90, 45%, 78%)"];
const PAL_SEGA: [&str; 5] = ["hsl(50, 85%, 78%)", "hsl(42, 90%, 80%)", "hsl(58, 80%, 78%)", "hsl(36, 85%, 80%)", "hsl(48, 70%, 74%)"];
const PAL_MYCOM: [&str; 5] = ["hsl(50, 40%, 93%)", "hsl(0, 0%, 90%)", "hsl(200, 20%, 93%)", "hsl(60, 30%, 95%)", "hsl(0, 0%, 86%)"];
const PAL_OTHER: [&str; 6] = ["hsl(0, 10%, 62%)", "hsl(120, 8%, 60%)", "hsl(220, 10%, 62%)", "hsl(50, 10%, 60%)", "hsl(0, 0%, 58%)", "hsl(280, 7%, 60%)"];

fn is_mycom_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["pc-88", "pc88", "pc-98", "pc98", "pc-60", "pc60", "msx", "fm-7", "fm7", "fm towns", "x68000", "windows", "dos", "linux", "mac"].iter().any(|k| n.contains(k))
}

fn platform_default(name: &str) -> String {
    let fam = state::platform_family(name);
    let pal: &[&str] = if matches!(fam.as_str(), "msx" | "fujitsu" | "sharp" | "computer") || is_mycom_name(name) {
        &PAL_MYCOM
    } else {
        match fam.as_str() {
            "nintendo" => &PAL_NINTENDO,
            "sony" => &PAL_SONY,
            "microsoft" => &PAL_MICROSOFT,
            "sega" => &PAL_SEGA,
            _ => &PAL_OTHER,
        }
    };
    pal[hash_n(name, pal.len())].to_string()
}

pub fn color_of(kind: &str, value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    if let Some(user) = state::ui_color(kind, value) {
        return Some(user);
    }
    default_color_of(kind, value)
}

/// The values with a built-in color per kind (Config › Settings › ui_colors).
pub const CATEGORY_VALUES: [&str; 10] = ["base_game", "port", "remake", "remaster", "edition", "dlc", "expansion", "standalone", "bundle", "mod"];
pub const ARCHIVE_FORMAT_VALUES: [&str; 12] = ["raw_iso", "chd", "rvz", "zar", "gcz", "wia", "zip", "7z", "cso", "pbp", "xiso", "unknown"];

/// `hsl(h, s%, l%)` (or `#rrggbb`) as `#rrggbb`, for a color input.
pub fn hsl_to_hex(s: &str) -> String {
    if s.len() == 7 && s.starts_with('#') {
        return s.to_string();
    }
    let nums: Vec<f64> = s.trim_start_matches("hsl(").trim_end_matches(')').split(',').filter_map(|x| x.trim().trim_end_matches('%').parse().ok()).collect();
    if nums.len() != 3 {
        return "#888888".into();
    }
    let (h, sat, l) = (nums[0] / 360.0, nums[1] / 100.0, nums[2] / 100.0);
    let f = |n: f64| {
        let k = (n + h * 12.0) % 12.0;
        let a = sat * l.min(1.0 - l);
        let c = l - a * (k - 3.0).min(9.0 - k).min(1.0).max(-1.0);
        format!("{:02x}", (c * 255.0).round() as u8)
    };
    format!("#{}{}{}", f(0.0), f(8.0), f(4.0))
}

/// The built-in color, ignoring the user's overrides.
pub fn default_color_of(kind: &str, value: &str) -> Option<String> {
    if let Some(d) = default_color(kind, value) {
        return Some(d.to_string());
    }
    if kind == "platform" {
        return Some(platform_default(value));
    }
    if matches!(kind, "region" | "language") {
        return Some("#d9dde3".to_string());
    }
    if kind == "source" {
        return Some(format!("hsl({}, 50%, 85%)", hash_n(&format!("{kind}:{value}"), 360)));
    }
    None
}

fn text_on(bg: &str) -> &'static str {
    if let Some(hex) = bg.strip_prefix('#').filter(|h| h.len() == 6) {
        let p = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f64;
        return if 0.299 * p(0) + 0.587 * p(2) + 0.114 * p(4) > 150.0 { "#111" } else { "#fff" };
    }
    if let Some(l) = bg.rsplit(',').next().and_then(|x| x.trim().trim_end_matches(')').trim_end_matches('%').parse::<f64>().ok()) {
        return if l > 60.0 { "#111" } else { "#fff" };
    }
    "#111"
}

// --- flags ---

fn flag_emoji(cc: &str) -> String {
    let cc = cc.to_ascii_uppercase();
    if cc.len() != 2 || !cc.chars().all(|c| c.is_ascii_uppercase()) {
        return String::new();
    }
    cc.chars().filter_map(|c| char::from_u32(0x1F1E6 + (c as u32 - 'A' as u32))).collect()
}

pub fn region_flag(code: &str) -> String {
    match code {
        "WORLD" => return "🌐".into(),
        "ASIA" => return "🌏".into(),
        "OTHER" => return "🏳️".into(),
        "UK" => return flag_emoji("GB"),
        _ => {}
    }
    if code.len() == 2 && code.chars().all(|c| c.is_ascii_uppercase()) {
        return flag_emoji(code);
    }
    let cc = match code.to_ascii_lowercase().as_str() {
        "portugal" => "PT",
        "norway" => "NO",
        "finland" => "FI",
        "denmark" => "DK",
        "turkey" => "TR",
        "greece" => "GR",
        "austria" => "AT",
        "switzerland" => "CH",
        "belgium" => "BE",
        "mexico" | "latin" => "MX",
        "argentina" => "AR",
        "india" => "IN",
        "israel" => "IL",
        "south africa" => "ZA",
        "new zealand" => "NZ",
        "ireland" => "IE",
        "czech" => "CZ",
        "hungary" => "HU",
        "croatia" => "HR",
        "ukraine" => "UA",
        "thailand" => "TH",
        "philippines" => "PH",
        "indonesia" => "ID",
        "malaysia" => "MY",
        "singapore" => "SG",
        "vietnam" => "VN",
        "scandinavia" => "SE",
        _ => "",
    };
    flag_emoji(cc)
}

pub fn lang_flag(code: &str) -> String {
    let cc = match code {
        "en" => "GB",
        "ja" => "JP",
        "fr" => "FR",
        "es" | "ca" => "ES",
        "it" => "IT",
        "de" => "DE",
        "kr" | "ko" => "KR",
        "zh" | "zh-s" => "CN",
        "zh-t" => "TW",
        "pt" => "PT",
        "nl" => "NL",
        "sv" => "SE",
        "da" => "DK",
        "no" => "NO",
        "fi" => "FI",
        "ru" => "RU",
        "pl" => "PL",
        "cs" => "CZ",
        "tr" => "TR",
        "hu" => "HU",
        "el" => "GR",
        _ => "",
    };
    flag_emoji(cc)
}

// --- platform glyphs: a user icon file, else a generic shape by kind ---

const GLYPH_CART: &str = r#"<svg class="ico" viewBox="0 0 16 16"><path d="M3 1h10a1 1 0 0 1 1 1v9l-2 4H4l-2-4V2a1 1 0 0 1 1-1zm2 2v4h6V3z"/></svg>"#;
const GLYPH_DISC: &str = r#"<svg class="ico" viewBox="0 0 16 16"><path d="M8 1a7 7 0 1 1 0 14A7 7 0 0 1 8 1zm0 5a2 2 0 1 0 0 4 2 2 0 0 0 0-4z"/></svg>"#;
const GLYPH_HANDHELD: &str = r#"<svg class="ico" viewBox="0 0 16 16"><path d="M4 1h8a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V2a1 1 0 0 1 1-1zm1 2v6h6V3zm1 8v2h1v-2zm4 0a1 1 0 1 0 0 2 1 1 0 0 0 0-2z"/></svg>"#;
const GLYPH_COMPUTER: &str = r#"<svg class="ico" viewBox="0 0 16 16"><path d="M2 2h12a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1H9v1h3v1H4v-1h3v-1H2a1 1 0 0 1-1-1V3a1 1 0 0 1 1-1zm1 2v6h10V4z"/></svg>"#;

pub fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

fn is_handheld(name: &str) -> bool {
    let n = name.to_lowercase();
    ["game boy", "game gear", "lynx", "wonderswan", "neo geo pocket", "psp", "playstation portable", "vita", "nintendo ds", "dsi", "3ds", "switch", "pocketstation", "n-gage", "pico", "virtual boy", "pokemon mini", "pokémon mini"].iter().any(|k| n.contains(k))
}

pub fn platform_glyph(name: &str) -> String {
    let s = slug(name);
    if let Some(file) = ["svg", "png", "webp", "jpg", "gif", "ico"].iter().map(|e| format!("{s}.{e}")).find(|f| state::has_icon(f)) {
        return format!(r#"<img class="ico" src="{}" alt="" title="{}">"#, esc(&crate::api::media_url(&format!("/icons/{file}"))), esc(name));
    }
    if is_handheld(name) {
        return GLYPH_HANDHELD.to_string();
    }
    if is_mycom_name(name) {
        return GLYPH_COMPUTER.to_string();
    }
    if state::platform_disc_based(name) {
        return GLYPH_DISC.to_string();
    }
    GLYPH_CART.to_string()
}

fn icon_for(kind: &str, value: &str) -> String {
    if let Some(url) = state::ui_icon(kind, value) {
        return format!(r#"<img class="ico" src="{}" alt="">"#, esc(&url));
    }
    if kind == "platform" {
        return platform_glyph(value);
    }
    let flag = match kind {
        "region" => region_flag(value),
        "language" => lang_flag(value),
        _ => String::new(),
    };
    if flag.is_empty() {
        String::new()
    } else {
        format!(r#"<span class="flag">{flag}</span>"#)
    }
}

/// A colored badge for a kind of value (platform / region / language / source / format /
/// category), the console's `cbadge`.
pub fn cbadge(kind: &str, value: &str, label: Option<&str>, title: Option<&str>) -> String {
    if value.is_empty() {
        return String::new();
    }
    let bg = color_of(kind, value);
    let style = bg.as_ref().map(|b| format!(r#"style="background:{b};color:{}""#, text_on(b))).unwrap_or_default();
    let title = esc(title.unwrap_or(value));
    let shown = if kind == "platform" && label.is_none() { state::platform_label(value) } else { label.unwrap_or(value).to_string() };
    let label = esc(&shown);
    if kind == "platform" {
        return format!(
            r#"<span class="pchip" style="color:{}" title="{title}">{}</span><span class="badge" {style} title="{title}">{label}</span>"#,
            bg.as_deref().unwrap_or("#9aa0aa"),
            icon_for(kind, value)
        );
    }
    if matches!(kind, "region" | "language") {
        let icon = icon_for(kind, value);
        let chip = if icon.is_empty() { String::new() } else { format!(r#"<span class="fchip" title="{title}">{icon}</span>"#) };
        return format!(r#"{chip}<span class="badge fb" {style} title="{title}">{label}</span>"#);
    }
    format!(r#"<span class="badge" {style} title="{title}">{}{label}</span>"#, icon_for(kind, value))
}

fn csv(v: &str) -> impl Iterator<Item = &str> {
    v.split(',').map(str::trim).filter(|x| !x.is_empty())
}

pub fn lang_label(code: &str) -> String {
    state::language_label(code)
}

pub fn lang_badges(v: &str) -> String {
    csv(v).map(|c| cbadge("language", c, Some(&lang_label(c)), Some(&state::language_english(c)))).collect()
}

pub fn region_badges(v: &str) -> String {
    csv(v).map(|c| cbadge("region", c, Some(c), Some(&state::region_name(c)))).collect()
}

pub fn platform_badges(v: &str) -> String {
    csv(v).map(|p| cbadge("platform", p, None, None)).collect()
}

pub fn provider_icon(name: &str) -> String {
    provider_icon_titled(name, name)
}

/// [`provider_icon`] with its own tooltip (the icon's title is the one a hover shows, so a
/// wrapper's title never would).
pub fn provider_icon_titled(name: &str, title: &str) -> String {
    let (ab, bg) = match name {
        "igdb" => ("IG", "#9147ff"),
        "mobygames" => ("MG", "#e08a1e"),
        "retroachievements" => ("RA", "#c9a200"),
        "thegamesdb" => ("TG", "#1e90ff"),
        "screenscraper" => ("SS", "#2ea043"),
        "steamgriddb" => ("SG", "#3c6e9e"),
        "emuready" => ("ER", "#e05252"),
        "hasheous" => ("HS", "#6b7280"),
        "playmatch" => ("PM", "#7c5cbf"),
        "xlenore" => ("XL", "#00439c"),
        _ => ("", "#555"),
    };
    let ab = if ab.is_empty() { name.chars().take(2).collect::<String>().to_uppercase() } else { ab.to_string() };
    format!(r#"<span class="pico" style="background:{bg}" title="{}">{ab}</span>"#, esc(title))
}

pub fn provider_icons(v: &str) -> String {
    let mut seen = std::collections::BTreeSet::new();
    csv(v).filter(|p| seen.insert(p.to_string())).map(provider_icon).collect()
}

pub fn source_badges(v: &str) -> String {
    csv(v).map(|x| cbadge("source", x, None, None)).collect()
}

pub fn hint_icon(title: &str) -> String {
    format!(r#"<span class="hint" title="{}">💡</span>"#, esc(title))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert_eq!(esc("<a&\"'>"), "&lt;a&amp;&quot;&#39;&gt;");
        assert_eq!(fmt_size(Some(2048)), "2.0 KiB");
        assert_eq!(fmt_size(Some(12)), "12 B");
        assert_eq!(fmt_date("2026-09-27T11:13:02.5"), "2026-09-27 11:13:02");
        assert_eq!(slug("Sony PlayStation 2"), "sony-playstation-2");
        assert_eq!(region_flag("JP"), "🇯🇵");
        assert_eq!(text_on("#ffffff"), "#111");
    }
}
