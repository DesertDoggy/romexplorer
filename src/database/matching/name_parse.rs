//! DAT/file name -> base title + tags. Ported from playmatch `matching/name_parse.rs` (MIT);
//! this project's additions on top of upstream:
//! - 2-letter codes with a casing rule ("(JP)"/"(US)" uppercase = region, "(en)"/"(ja)"
//!   lowercase = language) so TOSEC-style names classify, not just No-Intro's spelled-out
//!   regions;
//! - `serial` (a catalog/product code token, e.g. Redump's "serial,version" DATs put the
//!   disc serial in the name) and `alt` ("(Alt)" / "(Alt N)": an alternate dump of the
//!   same disc);
//! - `Variant::Sample`/`Demo` etc. map onto `release.is_*` flags at import;
//! - `edition`: budget reissues, special / limited editions and aftermarket releases, from
//!   whole-group tags only (DATs tag an edition only when a dump needs telling apart; an
//!   untagged reprint is just a revision), and [`revision_sort`] to order `(v1.10)` / `(Rev A)`.

use lazy_static::lazy_static;
use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedName {
    pub base: String,
    pub regions: Vec<RegionTag>,
    pub year: Option<u16>,
    pub revision: Option<String>,
    /// The strongest of [`Self::variants`] ([`Variant::strength`]): what the release mainly is.
    pub variant: Option<Variant>,
    /// Every variant tag the name carries, in the order written: `(Demo) (Unl)` is both.
    pub variants: Vec<Variant>,
    pub disc: Option<u8>,
    pub disc_total: Option<u8>,
    pub languages: Vec<Lang>,
    pub serial: Option<String>,
    pub alt: bool,
    /// Budget reissue, special / limited edition or aftermarket release (commercial all).
    pub edition: Option<Edition>,
    /// Parenthesised/bracketed groups nothing above claimed (publisher names in TOSEC,
    /// dump flags like "[b1]", "Track 1", ...).
    pub residual: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegionTag {
    World,
    Usa,
    Europe,
    Japan,
    Asia,
    Australia,
    Brazil,
    Korea,
    China,
    Taiwan,
    HongKong,
    Canada,
    Uk,
    Germany,
    France,
    Italy,
    Spain,
    Netherlands,
    Sweden,
    Russia,
    Poland,
}

impl RegionTag {
    /// The short code stored in `release.region` (matches `common::models::Region::code`
    /// where both exist).
    pub fn code(self) -> &'static str {
        match self {
            Self::World => "WORLD",
            Self::Usa => "US",
            Self::Europe => "EU",
            Self::Japan => "JP",
            Self::Asia => "ASIA",
            Self::Australia => "AU",
            Self::Brazil => "BR",
            Self::Korea => "KR",
            Self::China => "CN",
            Self::Taiwan => "TW",
            Self::HongKong => "HK",
            Self::Canada => "CA",
            Self::Uk => "UK",
            Self::Germany => "DE",
            Self::France => "FR",
            Self::Italy => "IT",
            Self::Spain => "ES",
            Self::Netherlands => "NL",
            Self::Sweden => "SE",
            Self::Russia => "RU",
            Self::Poland => "PL",
        }
    }

    /// ScreenScraper region codes.
    pub fn ss_codes(&self) -> &'static [&'static str] {
        match self {
            Self::World => &["wor"],
            Self::Usa => &["us"],
            Self::Europe => &["eu"],
            Self::Japan => &["jp"],
            Self::Asia => &["asi", "cn", "ko"],
            Self::Australia => &["au"],
            Self::Brazil => &["br"],
            Self::Korea => &["ko"],
            Self::China => &["cn"],
            Self::Taiwan => &["tw"],
            Self::HongKong => &["cn"],
            Self::Canada => &["ca"],
            Self::Uk => &["uk"],
            Self::Germany => &["de"],
            Self::France => &["fr"],
            Self::Italy => &["it"],
            Self::Spain => &["sp"],
            Self::Netherlands => &["nl"],
            Self::Sweden => &["se"],
            Self::Russia => &["ru"],
            Self::Poland => &["pl"],
        }
    }

    pub fn all_codes(&self) -> Vec<&'static str> {
        let mut v = vec![self.code()];
        v.extend_from_slice(self.ss_codes());
        v
    }
}

impl From<RegionTag> for crate::common::models::Region {
    fn from(tag: RegionTag) -> Self {
        use crate::common::models::Region;
        match tag {
            RegionTag::World => Region::World,
            RegionTag::Usa => Region::Usa,
            RegionTag::Europe => Region::Europe,
            RegionTag::Japan => Region::Japan,
            RegionTag::Asia => Region::Asia,
            RegionTag::Australia => Region::Australia,
            RegionTag::Brazil => Region::Brazil,
            RegionTag::Korea => Region::Korea,
            RegionTag::China => Region::China,
            RegionTag::Taiwan => Region::Taiwan,
            RegionTag::HongKong => Region::HongKong,
            RegionTag::Canada => Region::Canada,
            RegionTag::Uk => Region::Uk,
            RegionTag::Germany => Region::Germany,
            RegionTag::France => Region::France,
            RegionTag::Italy => Region::Italy,
            RegionTag::Spain => Region::Spain,
            RegionTag::Netherlands => Region::Netherlands,
            RegionTag::Sweden => Region::Sweden,
            RegionTag::Russia => Region::Russia,
            RegionTag::Poland => Region::Poland,
        }
    }
}

impl From<Lang> for crate::common::models::Language {
    fn from(lang: Lang) -> Self {
        use crate::common::models::Language;
        match lang {
            Lang::En => Language::En,
            Lang::Fr => Language::Fr,
            Lang::De => Language::De,
            Lang::Es => Language::Es,
            Lang::It => Language::It,
            Lang::Ja => Language::Ja,
            Lang::Pt => Language::Pt,
            Lang::Nl => Language::Nl,
            Lang::Sv => Language::Sv,
            Lang::Da => Language::Da,
            Lang::No => Language::No,
            Lang::Fi => Language::Fi,
            Lang::Ko => Language::Kr,
            Lang::Zh => Language::Zh,
            Lang::Ru => Language::Ru,
            Lang::Pl => Language::Pl,
            Lang::Cs => Language::Cs,
            Lang::Tr => Language::Tr,
            Lang::Hu => Language::Hu,
            Lang::El => Language::El,
            Lang::Ca => Language::Ca,
        }
    }
}

/// A commercial release's edition, from its tag. Ordered by strength: a name carrying two
/// edition tags keeps the stronger (aftermarket > special > re-release > budget).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Edition {
    /// Budget reissue: "(PlayStation 2 the Best)", "(Greatest Hits)", "(Platinum)", ...
    Budget,
    /// The game again on later hardware or a service: "(Virtual Console)", a collection it
    /// came on, a re-issuer's cartridge. Usually a port of the original data, not the
    /// original release.
    Rerelease,
    /// First-print / limited / commemorative edition: "(Shokai Genteiban)", "(Limited Edition)".
    Special,
    /// Released after the platform's commercial life: "(Aftermarket)", "(Homebrew)".
    Aftermarket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Variant {
    Proto,
    Beta,
    Sample,
    Demo,
    Unlicensed,
    Pirate,
    Hack,
    Translation,
}

impl Variant {
    /// Which tag names the release when it carries several: a patch over everything (a
    /// hack of a demo is a hack), then a pirate copy, then the pre-release kinds, and
    /// "unlicensed" last -- it says who sold it, not what it is.
    pub fn strength(self) -> u8 {
        match self {
            Self::Translation | Self::Hack => 6,
            Self::Pirate => 5,
            Self::Proto => 4,
            Self::Beta => 3,
            Self::Demo => 2,
            Self::Sample => 1,
            Self::Unlicensed => 0,
        }
    }
}

impl ParsedName {
    /// Records a variant tag: kept in [`Self::variants`], and [`Self::variant`] becomes the
    /// strongest seen (the first of equal strength).
    pub fn add_variant(&mut self, v: Variant) {
        if !self.variants.contains(&v) {
            self.variants.push(v);
        }
        if self.variant.is_none_or(|cur| v.strength() > cur.strength()) {
            self.variant = Some(v);
        }
    }

    pub fn has_variant(&self, v: Variant) -> bool {
        self.variants.contains(&v)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    En,
    Fr,
    De,
    Es,
    It,
    Ja,
    Pt,
    Nl,
    Sv,
    Da,
    No,
    Fi,
    Ko,
    Zh,
    Ru,
    Pl,
    Cs,
    Tr,
    Hu,
    El,
    Ca,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Fr => "fr",
            Self::De => "de",
            Self::Es => "es",
            Self::It => "it",
            Self::Ja => "ja",
            Self::Pt => "pt",
            Self::Nl => "nl",
            Self::Sv => "sv",
            Self::Da => "da",
            Self::No => "no",
            Self::Fi => "fi",
            Self::Ko => "ko",
            Self::Zh => "zh",
            Self::Ru => "ru",
            Self::Pl => "pl",
            Self::Cs => "cs",
            Self::Tr => "tr",
            Self::Hu => "hu",
            Self::El => "el",
            Self::Ca => "ca",
        }
    }
}

pub fn parse_name(input: &str) -> ParsedName {
    let mut parsed = ParsedName::default();
    let mut base = String::with_capacity(input.len());

    let mut idx = 0;
    while idx < input.len() {
        let rest = &input[idx..];
        let next_char = rest.chars().next().expect("non-empty rest");
        match next_char {
            '(' => {
                if let Some(end) = find_matching(input, idx, '(', ')') {
                    let content = &input[idx + '('.len_utf8()..end];
                    classify_into(content, &mut parsed);
                    idx = end + ')'.len_utf8();
                    continue;
                }
                base.push(next_char);
                idx += next_char.len_utf8();
            }
            '[' => {
                if let Some(end) = find_matching(input, idx, '[', ']') {
                    let content = &input[idx + '['.len_utf8()..end];
                    let trimmed = content.trim();
                    if !trimmed.is_empty() {
                        // TOSEC dump flags: [tr en] = translation, [h] = hack, [p] = pirate.
                        let lower = trimmed.to_ascii_lowercase();
                        if lower.starts_with("tr ") || lower == "tr" {
                            parsed.add_variant(Variant::Translation);
                        } else if lower == "h" || lower.starts_with("h ") {
                            parsed.add_variant(Variant::Hack);
                        } else if lower == "p" || lower.starts_with("p ") {
                            parsed.add_variant(Variant::Pirate);
                        }
                        parsed.residual.push(trimmed.to_string());
                    }
                    idx = end + ']'.len_utf8();
                    continue;
                }
                base.push(next_char);
                idx += next_char.len_utf8();
            }
            _ => {
                base.push(next_char);
                idx += next_char.len_utf8();
            }
        }
    }

    parsed.base = base.split_whitespace().collect::<Vec<_>>().join(" ");
    parsed
}

/// `name` without its region, language and revision groups -- `(Japan)`, `(USA, Europe)`,
/// `(En,Fr,De)`, `(Rev 1)`, `(v1.02)` -- everything else kept as written (`(Disc 1)`,
/// `(Demo)`, `[b]`, a TOSEC year, ...). Uses the same group classifiers as [`parse_name`].
pub fn strip_region_lang_revision(name: &str) -> String {
    let is_stripped = |content: &str| -> bool {
        let trimmed = content.trim();
        if trimmed.is_empty() || classify_variant(trimmed).is_some() {
            return false;
        }
        if classify_revision(trimmed).is_some() {
            return true;
        }
        let tokens: Vec<&str> = trimmed.split(',').map(str::trim).filter(|t| !t.is_empty()).collect();
        !tokens.is_empty() && tokens.iter().all(|t| classify_region(t).is_some() || classify_lang(t).is_some())
    };
    let mut out = String::with_capacity(name.len());
    let mut idx = 0;
    while idx < name.len() {
        let next_char = name[idx..].chars().next().expect("non-empty rest");
        if next_char == '(' {
            if let Some(end) = find_matching(name, idx, '(', ')') {
                let content = &name[idx + 1..end];
                if !is_stripped(content) {
                    out.push_str(&name[idx..end + 1]);
                }
                idx = end + 1;
                continue;
            }
        }
        out.push(next_char);
        idx += next_char.len_utf8();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn find_matching(s: &str, start: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0i32;
    for (offset, ch) in s[start..].char_indices() {
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                return Some(start + offset);
            }
        }
    }
    None
}

fn classify_into(content: &str, parsed: &mut ParsedName) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return;
    }

    if let Some(v) = classify_variant(trimmed) {
        parsed.add_variant(v);
        return;
    }
    if let Some(r) = classify_revision(trimmed) {
        parsed.revision = Some(r);
        return;
    }
    if let Some(e) = classify_edition(trimmed) {
        parsed.edition = Some(parsed.edition.map_or(e, |prev| prev.max(e)));
        return;
    }
    if let Some((d, total)) = classify_disc(trimmed) {
        parsed.disc = Some(d);
        if total.is_some() {
            parsed.disc_total = total;
        }
        return;
    }
    if classify_alt(trimmed) {
        parsed.alt = true;
        return;
    }
    if let Some(y) = classify_year(trimmed) {
        parsed.year = Some(y);
        return;
    }

    let tokens: Vec<&str> = trimmed.split(',').map(str::trim).filter(|t| !t.is_empty()).collect();

    let regions: Option<Vec<_>> = tokens.iter().map(|t| classify_region(t)).collect();
    if let Some(rs) = regions {
        for r in rs {
            if !parsed.regions.contains(&r) {
                parsed.regions.push(r);
            }
        }
        return;
    }

    let langs: Option<Vec<_>> = tokens.iter().map(|t| classify_lang(t)).collect();
    if let Some(ls) = langs {
        for l in ls {
            if !parsed.languages.contains(&l) {
                parsed.languages.push(l);
            }
        }
        return;
    }

    // Mixed region/language groups ("En,Fr,De" is handled above; TOSEC writes "(JP)(en)"
    // as separate groups, so a mixed group is rare) -- take what classifies, keep the rest.
    if tokens.len() > 1 {
        let mut leftover = Vec::new();
        for t in &tokens {
            if let Some(r) = classify_region(t) {
                if !parsed.regions.contains(&r) {
                    parsed.regions.push(r);
                }
            } else if let Some(l) = classify_lang(t) {
                if !parsed.languages.contains(&l) {
                    parsed.languages.push(l);
                }
            } else {
                leftover.push(*t);
            }
        }
        if leftover.len() < tokens.len() {
            if !leftover.is_empty() {
                parsed.residual.push(leftover.join(", "));
            }
            return;
        }
    }

    if parsed.serial.is_none() && classify_serial(trimmed) {
        parsed.serial = Some(trimmed.to_string());
        return;
    }

    parsed.residual.push(trimmed.to_string());
}

fn classify_variant(content: &str) -> Option<Variant> {
    let lower = content.to_ascii_lowercase();
    match lower.as_str() {
        "proto" | "prototype" => Some(Variant::Proto),
        "beta" => Some(Variant::Beta),
        "sample" => Some(Variant::Sample),
        // Taikenban / Otameshi: Japanese trial discs.
        "demo" | "kiosk" | "kiosk demo" | "kiosk disc" | "preview" | "trial" | "taikenban" | "taikenban disc" | "hatsu taikenban" | "tentou taikenban" | "otameshi disc" | "otameshiban" => Some(Variant::Demo),
        "unl" | "unlicensed" => Some(Variant::Unlicensed),
        "pirate" => Some(Variant::Pirate),
        "hack" => Some(Variant::Hack),
        // A demo named after the game it is a demo of -- a bonus disc filed under another
        // game: "(Fuga Saga Taikenban)", "(Fuga Saga Demo)".
        _ if [" demo", " taikenban", " trial", " otameshiban"].iter().any(|s| lower.ends_with(s)) => Some(Variant::Demo),
        _ => {
            if lower.starts_with("beta ") || lower.starts_with("proto ") || lower.starts_with("demo ") || lower.starts_with("sample ") {
                match lower.split(' ').next() {
                    Some("beta") => Some(Variant::Beta),
                    Some("proto") => Some(Variant::Proto),
                    Some("demo") => Some(Variant::Demo),
                    Some("sample") => Some(Variant::Sample),
                    _ => None,
                }
            } else {
                None
            }
        }
    }
}

/// An edition tag: the whole group must match (a substring would hit game names -- "Example
/// Arcade's Greatest Hits" -- and TOSEC publishers -- "Silicon Limited", "Brutal Deluxe").
fn classify_edition(content: &str) -> Option<Edition> {
    lazy_static! {
        static ref RE_BUDGET: Regex = Regex::new(
            r"(?i)^(?:(?:PlayStation(?: \d| Vita| Portable)?|PSP|PS one|Koei|Sega|Capcom|Konami|Square Enix|Namco|Bandai|Bandai Namco|Hudson|Taito|Spike|Atlus|Falcom|EA|Nippon Ichi|Kadokawa|Marvelous|Tecmo|Koei Tecmo) the Best(?: Reprint)?|Greatest Hits|Platinum|Player'?s Choice|Nintendo Selects|Essentials|Satakore|SuperLite 1500(?: Series)?|Best Price|Budget(?: - .+)?|Reprint|Mega Hits!?|EA Best Hits|(?:[\w.]+ )?Best Collection|Best Selection)$"
        )
        .unwrap();
        // One item of a re-release tag; a tag may list several ("Virtual Console, Steam").
        // Taken from the tags the DATs use: the services and later machines a game came back
        // on (a bare "NES" / "GBA" is the game inside a Wii / Wii U / 3DS release), the
        // re-issuers, and the collection discs. Not the original digital releases (WiiWare,
        // eShop, PSN, a Wii channel), a dumper's own collection in TOSEC ("Someone's
        // Collection"), a compilation that came out as one ("Compilation - ..."), nor the
        // publishers and budget labels TOSEC names "... Classics".
        static ref RE_RERELEASE_ITEM: Regex = Regex::new(
            r"(?i)^(?:(?:.+ )?Re-?release|(?:Nintendo )?Switch(?: Online(?: \+ Expansion Pack)?)?|Wii U|Wii|3DS|NES|SNES|GBA|DS|GameCube|PS Vita|.*Classic Mini|.+ Mini(?: \d)?|.+ Mini Mix|GameCube Edition|Animal Crossing|e-Reader|Evercade|Retro-Bit(?: Generations)?|Piko Interactive|Ratalaika Games|Limited Run Games|iam8bit|Steam|GOG|Sega Channel|Sega Ages(?: .+)?|Arcade Archives|Capcom Town|.+ Arcade Garage|(?:PS2|PS one|PSone|QUByte|Midway|Sega 3D|Rockstar) Classics|Namco Museum(?: .+)?|Collection of .+|(?:[^']+ )?Collection(?: \d+)?|.+ Anthology(?: - .+)?|The)$"
        )
        .unwrap();
        static ref RE_SPECIAL: Regex = Regex::new(
            r"(?i)^(?:.*\bGentei ?-?ban|.*\bGenteiban|Shokai (?:Seisan|Tokuten|Gentei ?)?-?ban|Tokubetsu ?-?ban|.*\bHakkou Kinen|.*\bKinen ?-?ban|.*\bKinen|Doukon ?-?ban|(?:Limited|Collector'?s|Special|Premium|Deluxe|Launch|Anniversary) (?:Edition|Box|Package|Pack|Set)|Premium Box|Premium Package|Premium Pack|First Print(?: Edition)?)$"
        )
        .unwrap();
    }
    let lower = content.to_ascii_lowercase();
    if lower == "aftermarket" || lower == "homebrew" {
        return Some(Edition::Aftermarket);
    }
    if RE_BUDGET.is_match(content) {
        return Some(Edition::Budget);
    }
    if RE_SPECIAL.is_match(content) {
        return Some(Edition::Special);
    }
    // Virtual Console however the tag words it ("Wii and Wii U Virtual Console", "Virtual
    // Console, CIA", "Virtual Wii"); otherwise every item of the tag must be a re-release.
    if lower.contains("virtual console") || lower.contains("virtual wii") || content.split(',').map(str::trim).all(|item| RE_RERELEASE_ITEM.is_match(item) && !item.to_ascii_lowercase().starts_with("compilation")) {
        return Some(Edition::Rerelease);
    }
    None
}

/// A revision / version tag as a number that sorts: `v2.01` > `v1.10` > `v1.02` > `v1.00`,
/// `Rev B` = `Rev 2` > `Rev A` = `Rev 1`; 0 = none (the initial release).
pub fn revision_sort(revision: Option<&str>) -> u32 {
    let Some(rev) = revision.map(str::trim).filter(|r| !r.is_empty()) else { return 0 };
    let lower = rev.to_ascii_lowercase();
    let body = lower.strip_prefix("rev").or_else(|| lower.strip_prefix("version")).or_else(|| lower.strip_prefix('v')).unwrap_or(&lower).trim();
    let is_rev = lower.starts_with("rev");
    if is_rev && body.len() == 1 && body.as_bytes()[0].is_ascii_alphabetic() {
        return u32::from(body.as_bytes()[0] - b'a' + 1);
    }
    let parts: Vec<u32> = body.split('.').map(|p| p.chars().take_while(char::is_ascii_digit).collect::<String>().parse::<u32>().unwrap_or(0).min(999)).collect();
    if is_rev && parts.len() == 1 {
        return parts[0];
    }
    let part = |i: usize| parts.get(i).copied().unwrap_or(0);
    part(0).min(4000) * 1_000_000 + part(1) * 1_000 + part(2)
}

fn classify_revision(content: &str) -> Option<String> {
    lazy_static! {
        static ref RE_REV: Regex = Regex::new(r"^(?i)Rev\s+\S+$").unwrap();
        static ref RE_VER: Regex = Regex::new(r"(?i)^v\d+(\.\d+)*[a-z]?$").unwrap();
        static ref RE_VERSION: Regex = Regex::new(r"(?i)^version\s+\S+$").unwrap();
    }
    if RE_REV.is_match(content) || RE_VER.is_match(content) || RE_VERSION.is_match(content) {
        Some(content.to_string())
    } else {
        None
    }
}

fn classify_disc(content: &str) -> Option<(u8, Option<u8>)> {
    lazy_static! {
        static ref RE_DISC: Regex = Regex::new(r"(?i)^(?:Disc|Disk|CD|Cart|Tape)\s+(\d+)(?:\s+of\s+(\d+))?").unwrap();
        static ref RE_SIDE: Regex = Regex::new(r"(?i)^Side\s+([A-Z])$").unwrap();
    }
    if let Some(c) = RE_DISC.captures(content) {
        if let Some(n) = c.get(1).and_then(|m| m.as_str().parse::<u8>().ok()) {
            let total = c.get(2).and_then(|m| m.as_str().parse::<u8>().ok());
            return Some((n, total));
        }
    }
    if let Some(c) = RE_SIDE.captures(content) {
        if let Some(ch) = c.get(1).and_then(|m| m.as_str().chars().next()) {
            return Some(((ch.to_ascii_uppercase() as u8) - b'A' + 1, None));
        }
    }
    None
}

fn classify_alt(content: &str) -> bool {
    lazy_static! {
        static ref RE_ALT: Regex = Regex::new(r"(?i)^Alt(?:\s+\d+)?$").unwrap();
    }
    RE_ALT.is_match(content)
}

fn classify_year(content: &str) -> Option<u16> {
    // "1998" and TOSEC's "199x"/"19xx" (unknown digit -> year unknown).
    let parsed = content.parse::<u16>().ok()?;
    (1970..=2100).contains(&parsed).then_some(parsed)
}

fn classify_region(token: &str) -> Option<RegionTag> {
    let lower = token.to_ascii_lowercase();
    let two_letter_upper = token.len() == 2 && token.chars().all(|c| c.is_ascii_uppercase());
    match lower.as_str() {
        "usa" | "u" | "united states" | "north america" => Some(RegionTag::Usa),
        "europe" | "e" | "eur" => Some(RegionTag::Europe),
        "japan" | "j" | "jpn" => Some(RegionTag::Japan),
        "world" | "w" | "wor" => Some(RegionTag::World),
        "asia" | "asi" => Some(RegionTag::Asia),
        "australia" | "aus" => Some(RegionTag::Australia),
        "brazil" | "bra" => Some(RegionTag::Brazil),
        "korea" | "kor" | "south korea" => Some(RegionTag::Korea),
        "china" | "chn" => Some(RegionTag::China),
        "taiwan" | "twn" => Some(RegionTag::Taiwan),
        "hong kong" | "hkg" => Some(RegionTag::HongKong),
        "canada" | "can" => Some(RegionTag::Canada),
        "united kingdom" | "gbr" => Some(RegionTag::Uk),
        "germany" | "ger" | "deu" => Some(RegionTag::Germany),
        "france" | "fra" => Some(RegionTag::France),
        "italy" | "ita" => Some(RegionTag::Italy),
        "spain" | "esp" | "spa" => Some(RegionTag::Spain),
        "netherlands" | "nld" | "holland" => Some(RegionTag::Netherlands),
        "sweden" | "swe" => Some(RegionTag::Sweden),
        "russia" | "rus" => Some(RegionTag::Russia),
        "poland" | "pol" => Some(RegionTag::Poland),
        // 2-letter codes: region only when written uppercase ("(JP)"), which is how TOSEC
        // (and No-Intro's short form "(J)") distinguish them from lowercase language tags.
        "jp" | "us" | "eu" | "de" | "fr" | "it" | "es" | "kr" | "cn" | "tw" | "hk" | "br" | "au" | "nl" | "ru" | "pl" | "se" | "gb" | "uk" | "ca" if two_letter_upper => Some(match lower.as_str() {
            "jp" => RegionTag::Japan,
            "us" => RegionTag::Usa,
            "eu" => RegionTag::Europe,
            "de" => RegionTag::Germany,
            "fr" => RegionTag::France,
            "it" => RegionTag::Italy,
            "es" => RegionTag::Spain,
            "kr" => RegionTag::Korea,
            "cn" => RegionTag::China,
            "tw" => RegionTag::Taiwan,
            "hk" => RegionTag::HongKong,
            "br" => RegionTag::Brazil,
            "au" => RegionTag::Australia,
            "nl" => RegionTag::Netherlands,
            "ru" => RegionTag::Russia,
            "pl" => RegionTag::Poland,
            "se" => RegionTag::Sweden,
            "gb" | "uk" => RegionTag::Uk,
            "ca" => RegionTag::Canada,
            _ => unreachable!(),
        }),
        _ => None,
    }
}

fn classify_lang(token: &str) -> Option<Lang> {
    let base = token.split('-').next().unwrap_or(token).trim();
    // Uppercase 2-letter tokens are regions (see classify_region); No-Intro writes language
    // codes capitalised ("En,Fr") and TOSEC lowercase ("en"), both accepted here.
    if base.len() == 2 && base.chars().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    let lower = base.to_ascii_lowercase();
    match lower.as_str() {
        "en" => Some(Lang::En),
        "fr" => Some(Lang::Fr),
        "de" => Some(Lang::De),
        "es" => Some(Lang::Es),
        "it" => Some(Lang::It),
        "ja" => Some(Lang::Ja),
        "pt" => Some(Lang::Pt),
        "nl" => Some(Lang::Nl),
        "sv" => Some(Lang::Sv),
        "da" => Some(Lang::Da),
        "no" => Some(Lang::No),
        "fi" => Some(Lang::Fi),
        "ko" => Some(Lang::Ko),
        "zh" => Some(Lang::Zh),
        "ru" => Some(Lang::Ru),
        "pl" => Some(Lang::Pl),
        "cs" => Some(Lang::Cs),
        "tr" => Some(Lang::Tr),
        "hu" => Some(Lang::Hu),
        "el" => Some(Lang::El),
        "ca" => Some(Lang::Ca),
        _ => None,
    }
}

/// A product/catalog code token: letters then digits with an optional separator
/// ("SLUS-01234", "ULUS12345", "T-1234G", "DL-DOL-ABCD-USA", "HDR-0001"). Never used for
/// matching, only recorded on the release.
fn classify_serial(token: &str) -> bool {
    lazy_static! {
        static ref RE_SERIAL: Regex = Regex::new(r"^[A-Z]{1,6}[A-Z0-9]*[- ]?[0-9]{2,6}[A-Z0-9-]*$").unwrap();
        static ref RE_SERIAL_DASHED: Regex = Regex::new(r"^[A-Z0-9]{2,6}(-[A-Z0-9]{2,6}){1,4}$").unwrap();
    }
    if token.len() > 24 || token.contains(' ') && token.split(' ').count() > 2 {
        return false;
    }
    (RE_SERIAL.is_match(token) || RE_SERIAL_DASHED.is_match(token)) && token.chars().any(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_only_region_language_revision() {
        assert_eq!(strip_region_lang_revision("Shin Legend of Hogehoge V (Japan)"), "Shin Legend of Hogehoge V");
        assert_eq!(strip_region_lang_revision("Legend of Hogehoge V (Europe) (En,Fr,De,Es,It)"), "Legend of Hogehoge V");
        assert_eq!(strip_region_lang_revision("Super Sample World (USA, Europe) (Rev 1)"), "Super Sample World");
        assert_eq!(strip_region_lang_revision("Hoge Quest VII (USA) (Disc 2)"), "Hoge Quest VII (Disc 2)");
        assert_eq!(strip_region_lang_revision("Hogehoge Adventure (Japan) (Demo)"), "Hogehoge Adventure (Demo)");
        assert_eq!(strip_region_lang_revision("Game (Japan) (v1.02)"), "Game");
        assert_eq!(strip_region_lang_revision("Example I (1983)(Some Publisher)(US)[a]"), "Example I (1983)(Some Publisher)[a]");
    }

    #[test]
    fn no_intro_shape() {
        let p = parse_name("Example Title (USA, Europe) (En,Fr,De,Es,It) (Rev A)");
        assert_eq!(p.base, "Example Title");
        assert_eq!(p.regions, vec![RegionTag::Usa, RegionTag::Europe]);
        assert_eq!(p.languages, vec![Lang::En, Lang::Fr, Lang::De, Lang::Es, Lang::It]);
        assert_eq!(p.revision.as_deref(), Some("Rev A"));
    }

    #[test]
    fn short_region_and_variants() {
        assert_eq!(parse_name("Example (J)").regions, vec![RegionTag::Japan]);
        assert_eq!(parse_name("Example (Proto)").variant, Some(Variant::Proto));
        assert_eq!(parse_name("Example (Beta 2)").variant, Some(Variant::Beta));
        assert_eq!(parse_name("Example (Unl)").variant, Some(Variant::Unlicensed));
        assert_eq!(parse_name("Example (Demo)").variant, Some(Variant::Demo));
        assert_eq!(parse_name("Example Gear 2 - Sons of Sample (Japan) (Hatsu Taikenban)").variant, Some(Variant::Demo));
        assert_eq!(parse_name("Example Bandit (Japan) (Otameshi Disc)").variant, Some(Variant::Demo));
        assert_eq!(parse_name("Example Quest (Japan) (Disc 2) (Example Saga 2 Taikenban)").variant, Some(Variant::Demo), "a bonus demo of another game");
        assert_eq!(parse_name("Example Quest (USA) (Example Saga Demo)").variant, Some(Variant::Demo));
        assert_eq!(parse_name("Example Quest (USA) (Kiosk)").variant, Some(Variant::Demo));
        assert_eq!(parse_name("Example Quest (Japan) (Taikenban Disc)").variant, Some(Variant::Demo));
    }

    #[test]
    fn every_variant_tag_is_kept() {
        let p = parse_name("Example (USA) (Demo) (Unl)");
        assert_eq!((p.variant, p.variants.clone()), (Some(Variant::Demo), vec![Variant::Demo, Variant::Unlicensed]), "Unl after Demo used to replace it");
        assert_eq!(parse_name("Example (Japan) (Proto) (Unl)").variant, Some(Variant::Proto));
        assert_eq!(parse_name("Example (Japan) (Unl) (Beta)").variant, Some(Variant::Beta));
        let hack = parse_name("Example (USA) (Hack) (Unl)");
        assert_eq!(hack.variant, Some(Variant::Hack));
        assert!(hack.has_variant(Variant::Unlicensed));
        assert_eq!(parse_name("Example (USA) (Demo) [h]").variant, Some(Variant::Hack), "a hack of a demo is a hack");
        assert_eq!(parse_name("Example (USA) (Beta) (Proto)").variants, vec![Variant::Beta, Variant::Proto]);
        let aftermarket = parse_name("Example (World) (Aftermarket) (Unl)");
        assert_eq!((aftermarket.edition, aftermarket.variant), (Some(Edition::Aftermarket), Some(Variant::Unlicensed)));
    }

    #[test]
    fn disc_alt_serial_year() {
        let p = parse_name("Example (USA) (Disc 2 of 3) (Alt) (SLUS-01234) (v1.1)");
        assert_eq!(p.disc, Some(2));
        assert_eq!(p.disc_total, Some(3));
        assert!(p.alt);
        assert_eq!(p.serial.as_deref(), Some("SLUS-01234"));
        assert_eq!(p.revision.as_deref(), Some("v1.1"));
        assert_eq!(parse_name("Example (1994)").year, Some(1994));
        assert_eq!(parse_name("Example (3000)").year, None);
    }

    #[test]
    fn tosec_shape_two_letter_codes() {
        let p = parse_name("Example (1996)(Some Publisher)(JP)(en)[tr en]");
        assert_eq!(p.base, "Example");
        assert_eq!(p.year, Some(1996));
        assert_eq!(p.regions, vec![RegionTag::Japan]);
        assert_eq!(p.languages, vec![Lang::En]);
        assert_eq!(p.variant, Some(Variant::Translation));
        assert!(p.residual.iter().any(|r| r == "Some Publisher"));
    }

    #[test]
    fn de_is_language_when_lowercase_region_when_uppercase() {
        assert_eq!(parse_name("Example (de)").languages, vec![Lang::De]);
        assert_eq!(parse_name("Example (DE)").regions, vec![RegionTag::Germany]);
        assert_eq!(parse_name("Example (De)").languages, vec![Lang::De]);
    }

    #[test]
    fn converts_to_app_enums_and_round_trips_codes() {
        use crate::common::models::{Language, Region};
        for tag in [RegionTag::World, RegionTag::Usa, RegionTag::Netherlands, RegionTag::Poland, RegionTag::Uk] {
            let region: Region = tag.into();
            assert_eq!(region.code(), tag.code());
            assert_eq!(Region::from_code(region.code()), region);
        }
        for lang in [Lang::En, Lang::Ko, Lang::Zh, Lang::Sv, Lang::Ca] {
            let language: Language = lang.into();
            assert_eq!(Language::from_code(language.code()), language);
        }
        assert_eq!(Language::from(Lang::Ko), Language::Kr);
        assert_eq!(Language::from_code("ko"), Language::Kr);
    }

    #[test]
    fn nested_and_unmatched() {
        let p = parse_name("Example (Foo (Bar))");
        assert_eq!(p.base, "Example");
        assert_eq!(p.residual, vec!["Foo (Bar)"]);
        assert_eq!(parse_name("Example (USA").base, "Example (USA");
        assert_eq!(parse_name("Example [b1]").residual, vec!["b1"]);
    }

    #[test]
    fn editions_from_whole_tags() {
        let e = |n: &str| parse_name(n).edition;
        assert_eq!(e("Shin Legend of Hogehoge III (Japan) (v1.02)"), None);
        assert_eq!(e("Shin Legend of Hogehoge III (Japan) (v2.01)"), None);
        assert_eq!(e("Shin Legend of Hogehoge III (Japan) (Shokai Genteiban)"), Some(Edition::Special));
        assert_eq!(e("Shin Legend of Hogehoge III (Japan) (Shin Legend of Hogehoge-sai Hakkou Kinen)"), Some(Edition::Special));
        assert_eq!(e("Example Gear 2 - Sons of Sample (Japan) (Premium Package)"), Some(Edition::Special));
        assert_eq!(e("Sample Game 2 (Japan) (PlayStation 2 the Best)"), Some(Edition::Budget));
        assert_eq!(e("Sample Chronicle V (Japan) (Koei the Best)"), Some(Edition::Budget));
        assert_eq!(e("Exampleden (Asia) (PlayStation Vita the Best)"), Some(Edition::Budget));
        assert_eq!(e("Example Saga - Sample Trails (Japan) (v1.02) (PlayStation 3 the Best) (Update)"), Some(Edition::Budget));
        assert_eq!(e("Some Game (World) (Aftermarket) (Unl)"), Some(Edition::Aftermarket));
        assert_eq!(e("Hoge Quest (USA) (Virtual Console)"), Some(Edition::Rerelease));
        assert_eq!(e("Hoge Quest (Japan) (Wii U Virtual Console)"), Some(Edition::Rerelease));
        assert_eq!(e("Hoge Quest (USA) (Virtual Console, Steam)"), Some(Edition::Rerelease));
        assert_eq!(e("Hoge Quest (Europe) (Evercade, Retro-Bit)"), Some(Edition::Rerelease));
        assert_eq!(e("Fuga Saga (Japan) (Mega Drive Mini)"), Some(Edition::Rerelease));
        assert_eq!(e("Fuga Saga (USA) (Hoge Anniversary Collection)"), Some(Edition::Rerelease));
        // A budget tag that ends in "Collection" stays budget; a re-release beats budget.
        assert_eq!(e("Fuga Saga (Japan) (Best Collection)"), Some(Edition::Budget));
        assert_eq!(e("Fuga Saga (USA) (Greatest Hits) (Virtual Console)"), Some(Edition::Rerelease));
        assert_eq!(e("Virtual Console Hoge (USA)"), None);
        assert_eq!(e("Hoge Quest (Japan) (Wii and Wii U Virtual Console)"), Some(Edition::Rerelease));
        assert_eq!(e("Hoge Quest (Japan) (Virtual Console, CIA)"), Some(Edition::Rerelease));
        assert_eq!(e("Hoge Quest (USA) (Rerelease)"), Some(Edition::Rerelease));
        assert_eq!(e("Hoge Quest (USA) (NES)"), Some(Edition::Rerelease));
        assert_eq!(e("Hoge Quest (Japan) (PS2 Classics)"), Some(Edition::Rerelease));
        assert_eq!(e("Hoge Quest (USA) (Evercade)"), Some(Edition::Rerelease));
        assert_eq!(e("Hoge Quest (USA) (Steam, GOG)"), Some(Edition::Rerelease));
        assert_eq!(e("Hoge Quest (USA) (Hoge Collection, The)"), Some(Edition::Rerelease));
        assert_eq!(e("Hoge Quest (Japan) (Mega Drive Mini 2, Genesis Mini 2)"), Some(Edition::Rerelease));
        // Original digital releases, a dumper's collection and a budget line are not.
        assert_eq!(e("Hoge Quest (USA) (WiiWare)"), None);
        assert_eq!(e("Hoge Quest (Japan) (eShop)"), None);
        assert_eq!(e("Hoge Quest (1990)(Someone's Collection)"), None);
        assert_eq!(e("Hoge Quest (Japan) (Hoge Best Collection)"), Some(Edition::Budget));
        assert_eq!(e("Hoge Quest (1988)(Compilation - The Hoge Collection)"), None);
        assert_eq!(e("Hoge Quest (1988)(Fuga Classics)"), None);
        // Names and publishers that only contain edition words stay untagged.
        assert_eq!(e("Example Arcade's Greatest Hits (USA)"), None);
        assert_eq!(e("Example Band Greatest Hits (198x)(Moses, George)"), None);
        assert_eq!(e("Some Tape (1986)(Silicon Limited)"), None);
        assert_eq!(e("Some Disk (1990)(Brutal Deluxe)"), None);
        // The edition stays part of the name when region / language / revision are stripped.
        assert_eq!(strip_region_lang_revision("Shin Legend of Hogehoge III (Japan) (Shokai Genteiban)"), "Shin Legend of Hogehoge III (Shokai Genteiban)");
    }

    #[test]
    fn revisions_sort_as_numbers() {
        let r = |x: &str| revision_sort(Some(x));
        assert_eq!(revision_sort(None), 0);
        assert!(r("v1.00") < r("v1.02") && r("v1.02") < r("v1.10") && r("v1.10") < r("v2.01"));
        assert!(r("Rev A") < r("Rev B") && r("Rev 1") == r("Rev A") && r("Rev 2") == r("Rev B"));
        assert!(r("v1.9") < r("v1.10"));
    }
}
