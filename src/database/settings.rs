//! The `settings` table: one JSON value per key. This is the storage the plan left open for
//! provider priority, per-provider sync scope, raw-response retention and cache TTL.

use serde::{de::DeserializeOwned, Serialize};
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;
use super::providers::Provider;

pub const KEY_PROVIDER_PRIORITY: &str = "provider_priority";
pub const KEY_SYNC_SCOPE: &str = "provider_sync_scope";
pub const KEY_RAW_RETENTION: &str = "raw_response_retention";
pub const KEY_CACHE_TTL_DAYS: &str = "cache_ttl_days";
/// `"english"` | `"local"` | `{"languages": ["ja", "en", ...]}` (docs/plan/database.md).
pub const KEY_TITLE_DISPLAY_MODE: &str = "title_display_mode";
/// `{"genre": [...sources...], "serial": [...], "franchise": [...], "release_name": [...]}`.
pub const KEY_METADATA_SOURCE_PRIORITY: &str = "metadata_source_priority";

/// What "app detection" is called in a source list: the serial the scanner read off the
/// file itself, which is not any DAT's or provider's.
pub const SOURCE_DETECTED: &str = "detected";

/// Which sources can supply each `metadata_source_priority` field at all, in the default
/// order. The editors used to offer every DAT source and every provider under every field,
/// most of which could never have a value there. Each list is what this code base actually
/// stores from that source, checked against the importers rather than against what the
/// source's own site offers:
///
/// - `genre`: `providers::metadata` writes `title_genres` for IGDB, RetroAchievements and
///   MobyGames and for nothing else. No DAT carries a genre.
/// - `serial`: the scanner's own reading, then the DATs that carry one -- Redump (a
///   `<serial>` element per game) and No-Intro (a `serial` attribute on the rom). MAME
///   Redump and TOSEC DATs have none, and no provider's serial is fetched.
/// - `franchise`: IGDB only (`set_title_franchises`). DATs have no such notion.
/// - `release_name`: the DAT sources that define releases, then IGDB for localized names.
///
/// Hasheous and PlayMatch are in none of them: they answer "which provider entry is this
/// hash", and have no genre, serial, franchise or name of their own to prefer.
pub const METADATA_FIELD_SOURCES: [(&str, &[&str]); 4] = [
    ("genre", &["igdb", "retroachievements", "mobygames"]),
    ("serial", &[SOURCE_DETECTED, "redump", "no-intro"]),
    ("franchise", &["igdb"]),
    ("release_name", &["redump", "no-intro", "mame-redump", "tosec", "lost-level-archive", "hacks", "homebrew", "etc", "retroachievements", "igdb"]),
];

/// `metadata_source_priority`'s `genre` order, else the default one: whose genres a title is
/// filtered by when several sources gave it some (`api::catalog`'s genre filters).
pub async fn genre_source_priority(pool: &MySqlPool) -> Result<Vec<String>, DatabaseError> {
    field_source_priority(pool, "genre").await
}

/// `metadata_source_priority`'s `release_name` order, else the default one: which DAT's
/// release a scanned file is linked to when several cover it equally (`api::scan`'s hash
/// match), so that link agrees with the name shown.
pub async fn release_source_priority(pool: &MySqlPool) -> Result<Vec<String>, DatabaseError> {
    field_source_priority(pool, "release_name").await
}

/// One field's stored order (known sources only), with the sources it leaves out appended
/// in their default order.
async fn field_source_priority(pool: &MySqlPool, field: &str) -> Result<Vec<String>, DatabaseError> {
    let allowed = metadata_field_sources(field).unwrap_or_default();
    let stored = get::<serde_json::Value>(pool, KEY_METADATA_SOURCE_PRIORITY).await?;
    let mut order: Vec<String> = stored
        .as_ref()
        .and_then(|v| v.get(field))
        .and_then(|v| v.as_array())
        .map(|list| list.iter().filter_map(|s| s.as_str()).filter(|s| allowed.contains(s)).map(str::to_string).collect())
        .unwrap_or_default();
    for source in allowed {
        if !order.iter().any(|s| s == source) {
            order.push(source.to_string());
        }
    }
    Ok(order)
}

/// The sources a field accepts, or `None` for a field this setting does not have.
pub fn metadata_field_sources(field: &str) -> Option<&'static [&'static str]> {
    METADATA_FIELD_SOURCES.iter().find(|(name, _)| *name == field).map(|(_, sources)| *sources)
}

/// `{"category": {"base_game": "#RRGGBB", ...}, "platform": {...}, "region": {...},
/// "language": {...}, "source": {...}, "archive_format": {...}}` -- badge background colors
/// per value; anything not set falls back to the console's built-in defaults.
pub const KEY_UI_COLORS: &str = "ui_colors";
/// `"code"` (en, ja) | `"english"` (English, Japanese) | `"native"` (English, 日本語).
pub const KEY_LANGUAGE_DISPLAY: &str = "language_display";
/// `en` | `ja`: the language of the UI's own text (menus, buttons, headings). The text
/// itself lives in `assets/i18n/<lang>.json`.
pub const KEY_UI_LANGUAGE: &str = "ui_language";
/// `en` | `ja`: the language platform names are shown in, independent of the UI language
/// (a Japanese collector may want English menus with Japanese platform names, or the
/// reverse).
pub const KEY_PLATFORM_NAME_LANGUAGE: &str = "platform_name_language";
/// Which `media.media_type` the grid views show: box (default) | cover | title_screen | ...
pub const KEY_GRID_MEDIA_TYPE: &str = "grid_media_type";
/// `{"percent": 10, "min_bytes": 1073741824}` -- free space a temp dir must keep beyond
/// what an operation needs before it's chosen.
pub const KEY_TEMP_DIR_MARGIN: &str = "temp_dir_margin";
/// `{"platform": {"<name>": "<url>"}, "region": {"JP": "<url>"}, "language": {...},
/// "source": {...}}` -- icon/favicon URLs shown before badge text; unset values fall back
/// to the console's built-in emoji flags and family glyphs.
pub const KEY_UI_ICONS: &str = "ui_icons";
pub const KEY_REGION_PRIORITY: &str = "region_priority";
pub const KEY_LANGUAGE_PRIORITY: &str = "language_priority";
/// Which release names a (merged) title: one ordered list of region codes (upper case: US,
/// EU) and language codes (lower case: en, ja); the first entry a release matches ranks it,
/// unlisted come last. Separate from the display order above.
pub const KEY_CANONICAL_NAME_PRIORITY: &str = "canonical_name_priority";
/// How many Hasheous per-platform dump zips download at once (default 4).
pub const KEY_HASHEOUS_PARALLEL: &str = "hasheous_parallel_downloads";
/// Background PlayMatch gap-fill batch size: N contents per batch, 0 = unlimited (still 100
/// per request at the client's rate limit), -1 = no background requests (default 100).
pub const KEY_PLAYMATCH_BATCH: &str = "playmatch_batch_size";
/// Size caps of the media cache tiers, in MiB (0 = uncapped).
pub const KEY_MEDIA_CACHE_TIER1_MB: &str = "media_cache_tier1_max_mb";
pub const KEY_MEDIA_CACHE_TIER2_MB: &str = "media_cache_tier2_max_mb";
/// Grid thumbnails: longest edge in px, lossy quality (1-100), format avif|jpeg|webp|jxl.
pub const KEY_MEDIA_THUMB_PX: &str = "media_thumb_px";
pub const KEY_MEDIA_THUMB_QUALITY: &str = "media_thumb_quality";
pub const KEY_MEDIA_THUMB_FORMAT: &str = "media_thumb_format";
/// Which media a provider sync may download: `{"igdb": {"box": false, "screenshot": true}}`.
/// Unlisted, box and cover are on and everything else is off ([`MEDIA_DEFAULT_ON`]).
pub const KEY_MEDIA_DOWNLOAD: &str = "media_download";
/// How many items of a list type (screenshots, fan art) are taken per game from one provider.
pub const KEY_MEDIA_LIST_CAP: &str = "media_list_cap";
/// For the media types a game has one of (box, cover, logo, ...), which provider's is used:
/// `{"box": ["screenscraper", "igdb"]}`. Unlisted providers come after the listed ones.
pub const KEY_MEDIA_PRIORITY: &str = "media_priority";
/// Which media types the UI shows on title / release pages: `{"video": false}`. Unlisted
/// types are shown.
pub const KEY_MEDIA_SHOW: &str = "media_show";
/// Client mode: which media types this machine keeps its own copy of (`{"video": true}`).
/// A type that is off is streamed from the main server when a page shows it. Unlisted types
/// are kept, except video, which is streamed.
pub const KEY_MEDIA_CLIENT_KEEP: &str = "media_client_keep";
/// What a track the browser cannot play (DSD, ALAC, AIFF) is converted to for playing:
/// `aac` (default, 256 kbps) | `flac`.
pub const KEY_AUDIO_DSD_CACHE_FORMAT: &str = "audio_dsd_cache_format";
/// Size cap of the converted-audio cache, in MiB (0 = uncapped; default 4096).
pub const KEY_AUDIO_CACHE_MAX_MB: &str = "audio_cache_max_mb";
/// Client mode: the format a kept copy of the main server's audio is fetched in:
/// `none` (default: as it is) | `aac` | `xhe_aac`.
pub const KEY_AUDIO_CLIENT_FORMAT: &str = "audio_client_format";
/// Client mode: tracks above this bitrate (kbps) are converted, the rest copied as they are.
pub const KEY_AUDIO_CLIENT_BITRATE: &str = "audio_client_bitrate_kbps";
/// What a title with no audio of its own plays: `none` (default: silence) | `random` |
/// `generic_bgm` (the files carrying that tag).
pub const KEY_AUDIO_FALLBACK: &str = "audio_fallback";
/// Where a title page's music comes from: `tagged` (audio files tagged with the title) |
/// `rom` (the music inside the title's ROMs) | `both` (default).
pub const KEY_AUDIO_TITLE_SOURCES: &str = "audio_title_sources";
/// Which ROM sounds a title page plays: `bgm` (default) | `bgm_jingle` | `all`.
pub const KEY_AUDIO_ROM_KINDS: &str = "audio_rom_kinds";
/// A title with no ROM of its own plays ROM music from: `related` (default: another version
/// in its family, such as the original of a remake) | `random` | `none`.
pub const KEY_AUDIO_ROM_NOT_OWNED: &str = "audio_rom_not_owned";
/// Whether a title's music keeps playing on other screens until another title page opens
/// (default true).
pub const KEY_AUDIO_KEEP_PLAYING: &str = "audio_keep_playing";
/// Whether a title page plays the sound of the video files in its ROMs (title movies);
/// default false. A title's own choice (`rom_audio_title`) goes over it.
pub const KEY_AUDIO_ROM_VIDEOS: &str = "audio_rom_videos";
/// The lengths, in seconds, a ROM sound is sorted by (`rom::rom_audio::classify`).
pub const KEY_AUDIO_ROM_LOOP_MIN_S: &str = "audio_rom_loop_min_s";
pub const KEY_AUDIO_ROM_BGM_MIN_S: &str = "audio_rom_bgm_min_s";
pub const KEY_AUDIO_ROM_JINGLE_MIN_S: &str = "audio_rom_jingle_min_s";
/// Folders of music rip sets, matched to the ROMs by content when ROM audio is indexed.
pub const KEY_AUDIO_RIP_FOLDERS: &str = "audio_rip_folders";
/// This installation's own identity, created on first use and never changed.
///
/// Needed the moment data moves between machines: a SQLite cache or a backup carries it, so
/// an import can tell "this is my own data coming back" from "this is someone else's". See
/// [`instance_id`].
pub const KEY_INSTANCE_ID: &str = "instance_id";
/// `standalone` | `client` -- whether this installation builds its own catalogue.
///
/// Chosen once, at first run, because the two are not interchangeable in practice. A
/// **standalone** builds the shared catalogue itself: it imports DATs and queries providers,
/// and the row ids it assigns are its own. A **client** never does: it takes the catalogue
/// wholesale from its main server, so its ids are the server's, which is the only thing that
/// makes mirroring another machine's ownership meaningful ([`KEY_CATALOG_FROM`]).
///
/// Mixing the two is what produces the failure this exists to prevent: a machine that imports
/// its own DATs *and* syncs from a server ends up with two numbering schemes for the same
/// games, and ownership mirrored from a peer silently attaches to the wrong releases. Asking
/// once is much cheaper than detecting that afterwards.
pub const KEY_INSTANCE_MODE: &str = "instance_mode";

/// The `instance_id` of the main server this machine's catalogue came from.
///
/// Catalogue row ids (`release.id`, `content.id`) are per-machine AUTO_INCREMENT values, and
/// the `remote_*` mirror copies them verbatim -- so a mirrored row only means anything if both
/// machines number their catalogue the same way. They do when both synced from the same main
/// server, and they do not when each imported the same DATs itself. This records which, so
/// [`super::super::api::sync`] can refuse the second case instead of silently attaching a
/// peer's games to whatever release happens to hold that id here.
pub const KEY_CATALOG_FROM: &str = "catalog_from";
/// After a catalogue sync from the main server brought new art, queue a `media_sync` job that
/// fetches it (bool, default true).
pub const KEY_MEDIA_SYNC_AFTER_PULL: &str = "media_sync_after_pull";
/// While the main server is offline, fetch art from the provider's own URL instead (bool,
/// default true). Off keeps a client from ever calling a provider for art. While the server
/// is up it is always the server that fetches: it is the household's art cache.
pub const KEY_MEDIA_PROVIDER_FALLBACK: &str = "media_provider_fallback";
/// Searches also try the other script's names from the name dictionary (`dictionary`).
pub const KEY_SEARCH_TRANSLATIONS: &str = "search_translations";
/// A provider download whose file is already at its place in the media folder (left by an
/// earlier install whose database is gone) takes that file instead of downloading again
/// (bool, default true). Unset also means the first-run question was never answered.
pub const KEY_MEDIA_REUSE_EXISTING: &str = "media_reuse_existing";
/// Edit › Merge review's order: `regional` (default) | `english`.
pub const KEY_MERGE_REVIEW_ORDER: &str = "merge_review_order";
/// The Gemini model Merge review's lookups use; unset picks the newest Flash the key lists.
pub const KEY_GEMINI_MODEL: &str = "gemini_model";
/// Where backups are written. Absent or empty means `<data root>/backups`.
pub const KEY_BACKUP_DIR: &str = "backup_dir";
/// How many backups of each kind to keep; 0 keeps every one. Default 5.
pub const KEY_BACKUP_KEEP: &str = "backup_keep";
/// Whether a database backup also runs a catalogue backup. Off by default: the catalogue is
/// rebuilt by re-importing the same DATs, so it is the large, derivable part, and it is worth
/// backing up far less often than the small one. The two are separate jobs writing separate
/// files either way -- this only decides whether asking for one gets you both. Media *files*
/// are never in either; backing up a collection's art is its own, much larger job.
pub const KEY_BACKUP_CATALOG: &str = "backup_catalog";
/// Which hashes a file scan computes: an object of `algorithm -> bool` over the six
/// `hashes.algorithm` values. Defaults are `sha256` off and everything else on -- sha256
/// costs a full extra pass and nothing reads it that sha1 doesn't already satisfy
/// (`import.rs` dedups sha256 > sha1 > md5, so sha1 alone identifies content), while
/// xxh3_128 is the cheapest of the set and was previously not computed at all.
///
/// `ra_hash` is honoured only where a RetroAchievements console id can be resolved; see
/// [`scan_hashes`]. It is never computed for archive members.
pub const KEY_SCAN_HASHES: &str = "scan_hashes";
/// How many files a scan / a copy works on at once, per size tier (`api::parallel`):
/// `{"mode": "auto"|"custom", "small": n, "medium": n, "large": n, "drive": "ssd"|"hdd"|"network"}`.
/// Auto by default; `drive` absent detects the disk.
pub const KEY_SCAN_PARALLEL: &str = "scan_parallel_files";
pub const KEY_COPY_PARALLEL: &str = "copy_parallel_files";
/// Which detection heuristics a scan may use: `{"ext": bool, "path": bool}`. `ext` guesses
/// a platform from the file extension, `path` from the directory names above it. Both on by
/// default. Turning `path` off matters when a collection is filed under names that do not
/// describe the content.
pub const KEY_SCAN_HEURISTICS: &str = "scan_heuristics";
/// Whether a scan reads the title out of a PS3 or PSP package's encrypted PARAM.SFO
/// (`rom::platforms::sony::Pkg::decrypt_sfo`). Off by default.
pub const KEY_SCAN_PKG_TITLES: &str = "scan_pkg_titles";
/// Lenient matching (`database::serial_match`): which formats a hash match may link by serial
/// when no DAT hashes them, and which serials it may use. `{<format>: bool, "manifest": bool,
/// "file_name": bool}` with the formats of [`SERIAL_MATCH_FORMATS`]; `manifest` defaults on,
/// `file_name` off.
pub const KEY_SERIAL_MATCH: &str = "serial_match";
/// The formats [`KEY_SERIAL_MATCH`] lists, with their defaults. On by default where no DAT
/// hashes the format; off where one does (No-Intro hashes PSN packages, Redump discs), so a
/// changed dump is not quietly linked to the release it no longer is.
///
/// - `cia`, `xiso`, `wua`, `zar`, `vpk`, `pkg`, `psv`: by extension (`psv` also covers a Vita
///   game-card `.img`).
/// - `nsp`: a Switch `.nsp` or `.nsz`, by the ticket inside or the title id in its name.
/// - `vita_folder`: a Vita app folder, by its `sce_sys/param.sfo`.
/// - `ps_disc`: a PS1, PS2 or PS3 disc image, by the serial on the disc, and a PS3 game
///   folder by its `PS3_GAME/PARAM.SFO`.
pub const SERIAL_MATCH_FORMATS: [(&str, bool); 10] = [
    ("cia", true),
    ("xiso", true),
    ("wua", true),
    ("zar", true),
    ("vpk", true),
    ("vita_folder", true),
    ("nsp", true),
    ("pkg", false),
    ("psv", false),
    ("ps_disc", false),
];
/// RetroAchievements tab: whether "in progress" includes beaten games by default (true).
pub const KEY_RA_INCLUDE_BEATEN: &str = "ra_include_beaten_default";
/// RetroAchievements tab: which games by default -- local | server | local_first (default) |
/// server_first.
pub const KEY_RA_SOURCE: &str = "ra_source_default";
/// Default view per list: `{"titles": "grid", "releases": "grid", "games": "grid", "ra":
/// "badges"}`; collection lists list|grid (default grid), RetroAchievements lists
/// list|grid|icon|title|screenshot|badges (default badges).
pub const KEY_VIEW_DEFAULTS: &str = "view_defaults";
/// RetroAchievements tab: which base filter the lists start with -- console (default) | genre.
pub const KEY_RA_BASE_FILTER: &str = "ra_base_filter_default";
/// Collection Titles / Releases: which releases are listed by default -- all (default:
/// subsets and patches too) | patches (commercial + patched) | commercial.
pub const KEY_RELEASE_KINDS: &str = "release_kinds_default";
/// Platform ids in preference order: which platform's title leads a row of the cross-platform
/// list, and the order its platforms are shown in. Unset: [`DEFAULT_PLATFORM_PRIORITY`].
pub const KEY_PLATFORM_PRIORITY: &str = "platform_priority";
/// One list for every era, because a row only compares the platforms its game is on: the
/// PlayStation and the Saturn never meet the PS4 and the Xbox One. Generation by generation,
/// newest first, and within one PlayStation, then Xbox or Sega, then Nintendo, home consoles
/// before handhelds; PC last -- which is the one thing a single list cannot vary by era.
pub const DEFAULT_PLATFORM_PRIORITY: [crate::common::models::Platform; 25] = {
    use crate::common::models::Platform::*;
    [
        // current
        PlayStation5, XboxSeries, NintendoSwitch2,
        // PS4's
        PlayStation4, XboxOne, NintendoSwitch, PlayStationVita, WiiU, Nintendo3ds,
        // PS3's
        PlayStation3, Xbox360, Wii, PlayStationPortable, NintendoDs,
        // PS2's
        PlayStation2, Dreamcast, Xbox, GameCube,
        // PlayStation's
        PlayStation, SegaSaturn, Nintendo64,
        PcWindows, PcDos, Mac, Linux,
    ]
};

pub fn default_platform_priority() -> Vec<u16> {
    // The Definitive tab's place for PC versions, right after the PS5.
    let mut list: Vec<u16> = DEFAULT_PLATFORM_PRIORITY.iter().map(|p| *p as u16).collect();
    let at = list.iter().position(|p| *p == crate::common::models::Platform::PlayStation5 as u16).map_or(0, |i| i + 1);
    list.insert(at, crate::database::title_family::PC_DEFINITIVE);
    list
}

/// `platform_priority` as stored, else the default. A stored list is taken as it is --
/// unlike the region list, nothing is appended to it, so a platform taken out stays out
/// (and then ranks after the listed ones, by name).
pub async fn platform_priority(pool: &MySqlPool) -> Result<Vec<u16>, DatabaseError> {
    Ok(get::<Vec<u16>>(pool, KEY_PLATFORM_PRIORITY).await?.unwrap_or_else(default_platform_priority))
}

/// The Collection sub-tab the Collection tab opens on: one of [`COLLECTION_TABS`] (default
/// `titles`).
pub const KEY_COLLECTION_DEFAULT_TAB: &str = "collection_default_tab";
/// The Collection tab's sub-tabs, in their order on screen.
/// The Collection sub-tabs turned off: a list of [`COLLECTION_TABS`] names. Unset or empty:
/// all shown. Titles and Releases are always shown and never listed.
pub const KEY_COLLECTION_HIDDEN_TABS: &str = "collection_hidden_tabs";
/// The sub-tabs that can't be turned off.
pub const COLLECTION_FIXED_TABS: [&str; 2] = ["titles", "releases"];
pub const COLLECTION_TABS: [&str; 16] = ["crossplatform", "definitive", "titles", "releases", "games", "roms", "files", "contents", "sets", "audio", "multimedia", "documents", "source", "etc", "patches", "bios"];
/// Within one region / language, which edition comes first: an ordered list of
/// [`EDITION_KINDS`] (see [`edition_priority`]), each edition latest version first. Older
/// values, budget_first and versions_first, are still read.
pub const KEY_RELEASE_EDITION_ORDER: &str = "release_edition_order";
/// The editions a release can be (`name_parse::Edition`, `original` for none), in the default
/// order: a budget reissue is usually the latest data of the original release; a re-release
/// (Virtual Console, a collection, ...) is the game ported to later hardware.
pub const EDITION_KINDS: [&str; 5] = ["budget", "original", "rerelease", "special", "aftermarket"];
/// Launching emulators from a client on another machine (REST from a non-loopback
/// address). Off by default: an emulator starts on the machine running the app.
pub const KEY_ALLOW_REMOTE_LAUNCH: &str = "allow_remote_launch";

// Copying ROMs out of the collection (`api::copy`). The per-platform folder names and
// output formats are rows in `platform_output`, not keys here: ~82 platforms in one JSON
// document would be rewritten wholesale on every single-field edit, and `set` has no
// compare-and-swap.
/// How a copy names its destination subdirectories: dat | display | slug | es | custom.
pub const KEY_COPY_FOLDER_SCHEME: &str = "copy_folder_scheme";
/// Which platforms a copy folds into another's folder: group key -> on/off.
pub const KEY_COPY_PLATFORM_COMBINE: &str = "copy_platform_combine";
/// The archive settings a copy converts with (`romexplorer_types::copy::OutputSettings`).
pub const KEY_COPY_OUTPUT_DEFAULTS: &str = "copy_output_defaults";
/// Whether the copy dialog opens its format section expanded and editable.
pub const KEY_COPY_DECIDE_ON_COPY: &str = "copy_decide_on_copy";
/// The destinations the copy dialog offers again, most recent first.
pub const KEY_COPY_RECENT_DESTINATIONS: &str = "copy_recent_destinations";
/// The algorithm a verified copy makes sure the catalogue ends up holding. Which hash it
/// *compares* against is not a setting: it is whichever recorded one is cheapest on this CPU
/// (`rom::hash::fallback_order`), since only a hash the catalogue already knew can settle
/// whether the bytes are right.
pub const KEY_COPY_POPULATE_HASH: &str = "copy_populate_hash";
/// Whether a copy verifies unless told otherwise.
pub const KEY_COPY_VERIFY_DEFAULT: &str = "copy_verify_default";
/// Which dump a copy takes when one copy holds a headered and a headerless one.
pub const KEY_COPY_HEADER_PREFERENCE: &str = "copy_header_preference";
/// Which file a copy takes when one copy holds a Wii U game as a `.wua` and as a `.wux`.
pub const KEY_COPY_WIIU_PREFERENCE: &str = "copy_wiiu_preference";
/// Which format a copy takes when one copy holds a game in several (`Game.fds` + `Game.qd`).
pub const KEY_COPY_FORMAT_ORDER: &str = "copy_format_order";
/// Whether a copy prefers a decrypted dump to an encrypted one.
pub const KEY_COPY_PREFER_DECRYPTED: &str = "copy_prefer_decrypted";
/// Whether choosing a lossy output (wua, zar, xiso) asks first.
pub const KEY_COPY_LOSSY_WARNING: &str = "copy_lossy_warning";
/// The algorithm `integrity_check` makes sure every file it passes ends up holding. Separate
/// from [`KEY_COPY_POPULATE_HASH`] on purpose: a copy fills in one file it just wrote, while
/// a sweep fills in a whole collection, and someone may want those to be different hashes.
pub const KEY_INTEGRITY_POPULATE_HASH: &str = "integrity_populate_hash";

/// Which cue/gdi dialect this installation writes when it rebuilds a sheet.
///
/// Redump's by default, and not arbitrarily: every DAT this app reads is written against it,
/// so it is the one dialect a rebuild can be *checked* against. The others exist because a
/// collection organised around chdman's output wants its generated sheets to match the rest.
pub const KEY_SHEET_STYLE: &str = "sheet_style";
pub const KEY_CHD_READER: &str = "chd_reader";
/// `{"mode": "all" | "scanned" | "scanned_strict" | "manual", "platforms": [ids]}` -- which
/// platforms a DAT sync downloads and imports. Unset: all.
pub const KEY_DAT_SYNC_PLATFORMS: &str = "dat_sync_platforms";
pub const DAT_SYNC_PLATFORM_MODES: [&str; 4] = ["all", "scanned", "scanned_strict", "manual"];

/// Every key this project defines, with a one-line description for a settings UI.
pub const KNOWN_KEYS: [(&str, &str); 102] = [
    (crate::api::sets::KEY_COPY_DEFINITIVE, "What a set's definitive-version entry (added from Collection > Definitive) copies: best_held (default; the first version in the family's order you hold, its best release) | definitive_only (the definitive version when you hold it, else nothing of that game)."),
    (crate::database::title_family::KEY_FETCH_VERSIONS, "Collection > Definitive: where a family's other versions (older and newer, the base game too) come from. off (default): the catalogue's titles, and unowned versions only on platforms whose IGDB metadata is synced | per_title: for games held on disk, the whole family is fetched from IGDB by id, on any platform (versions on platforms you don't collect appear) | whole_platform (deprecated): per_title, and each platform such a version is on gets an IGDB platform sync."),
    (KEY_COLLECTION_HIDDEN_TABS, "Which Collection sub-tabs are turned off: a list of crossplatform | definitive | games | roms | files | contents | sets | audio | multimedia | documents | source | etc | patches | bios. Empty (default): all shown. Titles and Releases are always shown."),
    (crate::database::title_family::KEY_PC_MAX_AGE, "Collection > Definitive: a PC version older than the newest version by fewer than this many years takes the \"PC (Definitive tab)\" place in platform_priority; an older one ranks by its own year. Default 10; 0: PC versions never move up."),
    (crate::database::title_family::KEY_TWO_COVER_LAYOUT, "Collection > Definitive, two-cover grid: how the definitive version's and the base game's covers share a card: top_bottom (default) | diagonal | left_right."),
    (KEY_PLATFORM_PRIORITY, "Ordered list of platform ids: which platform's title leads a row of the Collection cross-platform list, and the order its platforms are shown in. 65001 is \"PC (Definitive tab)\": where PC versions rank on the Definitive tab (definitive_pc_max_age). Unlisted platforms come after, by name. Unset: by generation, newest first (PS5, Xbox Series, Switch 2; PS4, Xbox One, Switch, Vita, Wii U; PS3, Xbox 360; PS2, Dreamcast; PlayStation, Saturn), PC last."),
    (KEY_COLLECTION_DEFAULT_TAB, "Which sub-tab the Collection tab opens on: titles (default) | crossplatform | definitive | releases | games | roms | files | contents | sets | audio | multimedia | documents | source | etc | patches | bios."),
    (KEY_UI_LANGUAGE, "The language of the UI's own text: \"en\" (default) | \"ja\". Platform names have their own setting."),
    (KEY_PLATFORM_NAME_LANGUAGE, "The language platform names are shown in: \"en\" (default) | \"ja\"."),
    (KEY_SHEET_STYLE, "How a disc's sheet is written and read: redump | redump_catalog | chdman | mame. It decides the cue/gdi rebuilt from a CHD wherever one is -- a scan's member hashes, a copy's conversion and its check, an extraction -- and whether a GD-ROM is laid out as a cue (Redump) or a gdi (chdman, MAME). Redump by default (CRLF line endings), because it is the one every DAT is written against and so the only one a rebuild can be checked against. A loose cue on disk is always hashed as it is."),
    (KEY_DAT_SYNC_PLATFORMS, "Which platforms a DAT sync downloads and imports: {\"mode\": all (default) | scanned (platforms of scanned files, as the scan placed them) | scanned_strict (only platforms a hash match confirmed; not recommended) | manual, \"platforms\": [platform ids, for manual]}. Redump, MAME Redump and the GitHub sources skip the other platforms' files; No-Intro and TOSEC still download their one pack and import only these platforms' DATs. A DAT whose platform isn't known is always imported."),
    (KEY_CHD_READER, "Who reads a CHD's header and track list: checked (default; the built-in reader, compared with chdman whenever chdman is free, chdman's answer used where they differ) | chdman (chdman only; waits behind a running CHD conversion) | rust (the built-in reader only; fastest, not recommended yet)."),
    (KEY_INSTANCE_MODE, "standalone (builds its own catalogue from DATs and providers) or client (takes the catalogue from its main server and never builds one). Chosen at first run; changing it later means replacing the catalogue."),
    (KEY_CATALOG_FROM, "The instance this machine's catalogue was synced from. Ownership can only be mirrored between machines whose catalogues share a lineage, because row ids are copied verbatim."),
    (KEY_MEDIA_SYNC_AFTER_PULL, "After a catalogue sync from the main server brings new art, queue a media_sync job that fetches it from the server (true | false; default true). The server is asked first either way; this only decides whether it happens on its own."),
    (KEY_MERGE_REVIEW_ORDER, "Which suggestions Edit › Merge review shows first: \"regional\" (default) -- International and Director's Cut releases, Japanese releases filed under another game, names that translate each other, bundles holding single games -- or \"english\": titles sharing an IGDB game first."),
    (KEY_GEMINI_MODEL, "Older name of ai_model_gemini, still read when that is unset."),
    (crate::api::ai::KEY_PROVIDER, "Which AI answers in the app (Merge review's \"What differs\"): gemini (default) | claude | openai | perplexity. Each needs its API key under Config › AI and is paid per use."),
    ("ai_model_gemini", "The Gemini model asked. Unset: the newest Flash model the key can use."),
    ("ai_model_claude", "The Claude model asked (with web search). Unset: claude-opus-5-5."),
    ("ai_model_openai", "The OpenAI model asked (with web search). Unset: gpt-5."),
    ("ai_model_perplexity", "The Perplexity model asked. Unset: sonar."),
    (crate::api::ai::KEY_ASK_FIRST, "Which link of the Ask menu comes first: google (default; Google AI Mode) | claude | chatgpt | perplexity. The links open in the browser with your own sign-in, at no cost to this app."),
    (crate::api::ai::KEY_CACHE_DAYS, "Saved AI answers older than this many days are asked again (pinned ones never). Unset or 0 (default): kept until you ask again or clear them."),
    (crate::api::ai::KEY_PRICES, "Prices for the approximate AI cost, in US dollars per million tokens: {\"model-prefix\": {\"in\": 1.25, \"out\": 10}}. Laid over the built-in table; the longest matching prefix wins."),
    (KEY_SEARCH_TRANSLATIONS, "Searches also look for a name's other-language versions from the name dictionary (Edit › Name dictionary): a search in Japanese finds the English name and the other way round (true | false; default true)."),
    (KEY_MEDIA_PROVIDER_FALLBACK, "While the main server is offline, fetch art from the provider's own URL (true | false; default true). While the server is up, it does every provider fetch itself and this machine takes the result from it; off means this machine never calls a provider for art at all."),
    (KEY_MEDIA_REUSE_EXISTING, "When art a provider sync would download is already at its place in the media folder -- left there by an earlier install, say after starting over with a new database -- record that file instead of downloading it again (true | false; default true). Checked per provider and per picture, so what is missing still downloads. Your own uploads are not found this way."),
    (KEY_MEDIA_DOWNLOAD, "Which media a provider sync downloads: {\"igdb\": {\"box\": false, \"screenshot\": true}, ...}. Unlisted, box and cover are on and everything else is off. Files you add yourself (upload or URL) are never blocked."),
    (KEY_MEDIA_LIST_CAP, "How many screenshots / fan art images are taken per game from one provider (1-50, default 6)."),
    ("media_export_priority", "Export artwork: per slot (box, title, snap, logo), whose picture an emulator gets, best first -- \"manual\" is your own. Unset: box manual, thegamesdb, screenscraper, mobygames, retroachievements, igdb, steamgriddb; title manual, thegamesdb, screenscraper, retroachievements; snap the same plus igdb; logo manual, steamgriddb, thegamesdb, screenscraper."),
    ("media_export_prefer_region", "Export artwork: a picture filed under the release's own region beats one made for every region even from a provider ranked higher, as on a release's page (default true; false: the order decides)."),
    ("media_export_cache_max_mb", "Export artwork: size cap of the cache of converted pictures, MiB (default 1024; 0 = no cap). The least recently used go first."),
    (KEY_MEDIA_PRIORITY, "Per media type, the order of sources, {\"box\": [\"manual\", \"screenscraper\", \"igdb\"]}; \"manual\" is your own uploads, first unless placed. For a type a game has one of (box, box_3d, cover, logo, icon, title_screen) the first that has one is shown, and a lower provider's is not downloaded once a higher source has it. Screenshots, fan art and videos come from every provider; the order only decides which are shown first."),
    (KEY_MEDIA_SHOW, "Which media types title and release pages show: {\"video\": false, \"bgm\": true, ...}. Unlisted types are shown. bgm is the audio player."),
    (KEY_MEDIA_CLIENT_KEEP, "Client mode only: which media types this machine keeps its own copy of, {\"video\": false, ...}. Images are stored as AVIF when that is smaller. A type that is off is streamed from the main server instead. Video and bgm (audio) are off unless listed: a downloaded video stays in its original format, so streaming is the better choice; kept audio is fetched by the audio_sync job in the format audio_client_format names."),
    (KEY_AUDIO_DSD_CACHE_FORMAT, "What audio the browser cannot play (DSD, ALAC, AIFF) is converted to when it is played from this machine: aac (default; 256 kbps, small) | flac (lossless, larger). Converted once and cached. Without the AAC encoder library the lossless form is used either way."),
    (KEY_AUDIO_CACHE_MAX_MB, "Size cap of the converted-audio cache in MiB (default 4096; 0 = no cap). The least recently played conversions are removed first and made again when next needed."),
    (KEY_AUDIO_CLIENT_FORMAT, "Client mode: the format this machine's kept copies of the main server's audio are fetched in: none (as they are) | aac (the default on Linux) | xhe_aac (the default elsewhere). AAC is recommended when a Linux client is involved; xHE-AAC saves the most storage and plays on Windows 11, macOS, iOS and Android."),
    (KEY_AUDIO_CLIENT_BITRATE, "Client mode: the bitrate (kbps, 48-320, default 128) a kept copy is converted to. A track already at or below it is copied as it is."),
    (KEY_AUDIO_FALLBACK, "What a title with no audio linked to it plays: none (default; silent) | random (tracks picked at random from the audio linked to other titles) | generic_bgm (the audio files tagged generic_bgm)."),
    (KEY_AUDIO_TITLE_SOURCES, "Where a title page's music comes from: tagged (audio files tagged with the title) | rom (the music inside the title's ROMs, played from the ROM) | both (default)."),
    (KEY_AUDIO_ROM_KINDS, "Which sounds from a ROM a title page plays: bgm (default; music only) | bgm_jingle (music and short jingles) | all (sound effects too)."),
    (KEY_AUDIO_ROM_NOT_OWNED, "ROM music for a title with no ROM here: related (default; another version of the game, such as the original of a remake) | random (another title's) | none."),
    (KEY_AUDIO_KEEP_PLAYING, "Keep a title's music playing on other screens until another title page opens (true | false; default true)."),
    (KEY_AUDIO_ROM_VIDEOS, "Play the sound of the video files in a title's ROMs, such as its title movie (true | false; default false). Each title page can turn it on or off for that title."),
    (KEY_AUDIO_ROM_LOOP_MIN_S, "A ROM sound that loops is music from this many seconds (default 3)."),
    (KEY_AUDIO_ROM_BGM_MIN_S, "A ROM sound that does not loop is music from this many seconds (default 60)."),
    (KEY_AUDIO_ROM_JINGLE_MIN_S, "A ROM sound that does not loop is a jingle from this many seconds (default 8); shorter is a sound effect."),
    (KEY_AUDIO_RIP_FOLDERS, "Folders of music rip sets (NSF, SPC, GBS, HES, KSS, SGC and the like, loose or zipped; one set per zip or folder). Index ROM audio matches each set to your ROMs by content, in every region. Default: none."),
    (KEY_INSTANCE_ID, "This installation's own id, generated once. Stamped into exports so an import can tell your own data from another machine's."),
    (KEY_BACKUP_DIR, "Where backup zips are written. Empty = <data root>/backups."),
    (KEY_BACKUP_KEEP, "How many backups of each kind to keep (0 = all). Counted per kind, so a database backup never prunes a catalogue one."),
    (KEY_BACKUP_CATALOG, "Run a catalogue backup alongside each database backup. Off by default -- the catalogue is rebuilt by re-importing DATs, so it is the large derivable part and is worth backing up far less often. Media files are never backed up."),
    (KEY_SCAN_HASHES, "Which hashes a file scan computes: object of crc32|xxh3_128|md5|sha1|sha256|ra_hash -> true/false. Default: everything except sha256. ra_hash needs a resolvable RetroAchievements console id and is skipped when there is none."),
    (KEY_SCAN_PARALLEL, "How many files a scan hashes at the same time, by size: small up to 16 MiB, medium 16 MiB to 1 GiB, large 1 GiB and up. Auto picks from the disk, memory and cores (a spinning disk gets one big read at a time). Custom sets any size yourself, 1-64; a size left empty stays on auto. Only the reading and hashing run in parallel -- results are recorded one file at a time, in order. Drive says what the disk is (SSD / NVMe, HDD, network share) when detection gets it wrong; auto's numbers follow it. A ZFS pool counts as SSD when all its data disks are."),
    (KEY_COPY_PARALLEL, "How many files a copy works on at the same time, by the same sizes as scan_parallel_files. Auto takes the slower of the source and destination disks, and fewer when the copy converts (CHD / RVZ / zip / 7z compress on several threads of their own)."),
    (KEY_SERIAL_MATCH, "Lenient matching: formats no DAT hashes can be matched by their serial instead, and the link is marked as a serial match. {\"cia\", \"xiso\", \"wua\", \"zar\", \"vpk\", \"vita_folder\" (Vita app folders), \"nsp\" (Switch .nsp/.nsz, by the ticket or the [title id] in the name): true/false (default true), \"pkg\" (PS3, PSP and Vita packages), \"psv\" (Vita .psv and game-card images), \"ps_disc\" (PS1, PS2 and PS3 discs, PS3 game folders): true/false (default false, since DATs hash these), \"manifest\": use the serial in the romexplorer.json a copy here wrote (default true), \"file_name\": also use a serial spelled in the file name (default false), \"file_name_platforms\": {\"<platform id>\": true/false} to choose that per platform (a platform not listed follows file_name)}."),
    (KEY_SCAN_PKG_TITLES, "Read the title and version of PS3 and PSP .pkg files from inside the package (true | false; default false). Their PARAM.SFO is encrypted with keys every package tool carries; Vita packages need no key and are always read."),
    (KEY_SCAN_HEURISTICS, "Which detection heuristics a scan may use: {\"ext\": true, \"path\": true}. ext guesses the platform from the file extension, path from the directory names above it."),
    (KEY_PROVIDER_PRIORITY, "Ordered list of provider names: which provider's data wins and the query order."),
    (KEY_SYNC_SCOPE, "Object provider -> single|list|full|by_platform|by_keyword."),
    (KEY_RAW_RETENTION, "keep_all | keep_latest | discard -- what happens to raw provider response files."),
    (KEY_CACHE_TTL_DAYS, "Days before a cached provider response is considered stale."),
    (KEY_TITLE_DISPLAY_MODE, "Display localized title as title: \"english\" (off: romanized names) | \"local\" (on: each title / release under its localized name) | {\"languages\": [\"ja\", \"en\"]} (first language with a name). Used by the Titles and Releases lists, grids and pages."),
    (KEY_METADATA_SOURCE_PRIORITY, "Object field -> ordered source list (genre, serial, franchise, release_name) for display on overlap. Each field takes only the sources that can supply it: genre igdb/retroachievements/mobygames; serial detected/redump/no-intro; franchise igdb; release_name the DAT sources and igdb."),
    (KEY_UI_COLORS, "Badge background colors: {\"category\": {\"base_game\": \"#RRGGBB\"}, \"platform\": {}, \"region\": {}, \"language\": {}, \"source\": {}, \"archive_format\": {}}. Unset values use built-in defaults."),
    (KEY_LANGUAGE_DISPLAY, "\"code\" (en, ja) | \"english\" (English, Japanese) | \"native\" (English, 日本語) -- how languages are shown."),
    (KEY_GRID_MEDIA_TYPE, "Which media type the grid views show: box (default), cover, title_screen, screenshot, logo, ..."),
    (KEY_TEMP_DIR_MARGIN, "{\"percent\": 10, \"min_bytes\": 1073741824} -- free space a temp dir must keep beyond what an operation needs."),
    (KEY_REGION_PRIORITY, "Ordered list of region codes (US, EU, JP, ...): which release of a title comes first (its art/name represents the title); unlisted regions come last."),
    (KEY_LANGUAGE_PRIORITY, "Ordered list of language codes (en, ja, ...): tie-break after region for which release of a title comes first."),
    (KEY_CANONICAL_NAME_PRIORITY, "Which release's name becomes a title's canonical (normalized) name: ordered region codes (upper case) and language codes (lower case), default [\"USA\", \"EUR\", \"WORLD\", \"ASIA\", \"en\", \"JPN\"]; unlisted come last. Display uses title_display_mode. The same order decides the name used when an export converts names to the release name: for a title with several releases, the release this list ranks first supplies it."),
    (KEY_UI_ICONS, "Icon/favicon URLs before badge text: {\"platform\": {\"<name>\": \"<url>\"}, \"region\": {\"JP\": \"<url>\"}, \"language\": {}, \"source\": {}}. Unset values use built-in emoji flags / family glyphs."),
    (KEY_HASHEOUS_PARALLEL, "How many Hasheous dump zips download at once (default 4)."),
    (KEY_PLAYMATCH_BATCH, "Background PlayMatch gap fill: contents per batch (default 100); 0 = unlimited (still 100 per request, rate-limited); -1 = off."),
    (KEY_MEDIA_CACHE_TIER1_MB, "Media cache tier 1 size cap in MiB (0 = uncapped); least recently used full-size copies are evicted first, thumbnails last."),
    (KEY_MEDIA_CACHE_TIER2_MB, "Media cache tier 2 size cap in MiB (0 = uncapped)."),
    (KEY_MEDIA_THUMB_PX, "Grid thumbnail longest edge in pixels (default 320)."),
    (KEY_MEDIA_THUMB_QUALITY, "Grid thumbnail lossy quality 1-100 (default 60)."),
    (KEY_MEDIA_THUMB_FORMAT, "Grid thumbnail format: avif (default) | jpeg | webp | jxl (webp/jxl fall back to jpeg until implemented)."),
    (KEY_RA_INCLUDE_BEATEN, "RetroAchievements tab: \"in progress\" includes beaten games by default (true | false; default true)."),
    (KEY_RA_SOURCE, "RetroAchievements tab default games: all (every RA game, held ones first; the default) | local (only the ones held here)."),
    (KEY_RA_BASE_FILTER, "RetroAchievements tab: which base filter the lists start with: console (default) | genre."),
    (KEY_RELEASE_KINDS, "Collection Titles / Releases default filter: all (default -- subsets and patches too) | patches (commercial + patched) | commercial."),
    (super::release_buckets::KEY_UNLICENSED, "Unlicensed releases with no other tag: separate (default -- a Titles row and kind of their own) | retail (listed and ranked with the title's retail releases)."),
    (KEY_ALLOW_REMOTE_LAUNCH, "Let clients on other machines start emulators on this one (true | false; default false). The desktop app and a browser on this machine always can."),
    (KEY_RELEASE_EDITION_ORDER, "Release order within one region / language, by edition: an ordered list of budget (budget reissue), original, rerelease (Virtual Console, collections), special (special / limited edition) and aftermarket; each edition latest version first. Default [\"budget\", \"original\", \"rerelease\", \"special\", \"aftermarket\"]."),
    (KEY_COPY_FOLDER_SCHEME, "How a copy names its destination subdirectories: dat (\"Nintendo - Super Nintendo Entertainment System\") | display (\"Super Nintendo Entertainment System\") | slug (\"snes\", the IGDB slug RomM matches folders against) | es (\"snes\", the EmulationStation / RetroArch short name; the default) | custom (whatever you set per platform under Config > Copy)."),
    (KEY_COPY_PLATFORM_COMBINE, "Which platforms this app splits but a folder layout shouldn't: {\"famicom\": true, \"fds\": false, ...}. A group that's on puts the child's games in the parent's folder. Unlisted groups use their built-in default."),
    (KEY_COPY_OUTPUT_DEFAULTS, "Archive settings a copy converts with: {\"zip\": {\"codec\": \"zstd\", \"level\": 5, \"re_archive\": false}, \"seven_zip\": {...}, \"chd\": {\"preset\": \"chdman_default\"}, \"rvz\": {\"codec\": \"zstd\", \"block_size\": \"128k\", \"level\": 5}}. re_archive re-encodes a file already in the target format instead of copying its bytes; off means a byte-for-byte copy."),
    (KEY_COPY_DECIDE_ON_COPY, "Open the copy dialog's format section expanded and editable, so every copy chooses its own conversion settings (true | false; default false = use the settings above)."),
    (KEY_COPY_RECENT_DESTINATIONS, "The destination folders the copy dialog offers again, most recent first. Written by the app; clearing it just forgets them."),
    (KEY_COPY_POPULATE_HASH, "Which hash a verified copy compares what it wrote against: xxh3_128 (default) | crc32 | md5 | sha1 | sha256. Used whenever the catalogue records it for that content; where it does not, the copy compares against the cheapest recorded hash on this CPU and records this one off the same read once the bytes have matched. Separate from integrity_populate_hash."),
    (KEY_COPY_VERIFY_DEFAULT, "Whether a copy reads its output back and checks it (true | false; default false). Off is the fast path: it halves the reads, which over a mounted share is the whole cost. The copy dialog can turn it on per copy either way."),
    (KEY_COPY_HEADER_PREFERENCE, "Which file a copy takes when one copy of a release holds a headered and a headerless dump of one cartridge: priority (default; the one in the directory first under Config > Directories) | headered | headerless. Headerless is what the DAT names .unh. The set's size counts the same file."),
    (KEY_COPY_WIIU_PREFERENCE, "Which file a copy takes when one copy of a release holds a Wii U game both as Cemu's .wua and as a disc image (.wux / .wud): wua (default; the disc's .key is left behind too) | wux (.wux before .wud, with its .key) | priority (the one in the directory first under Config > Directories)."),
    (KEY_COPY_FORMAT_ORDER, "Which format a copy takes when one copy holds the same game in several (the same name, different extensions): a list of format families, each first-preferred, e.g. [[\"fds\", \"qd\"], [\"z64\", \"v64\", \"n64\"], [\"sfc\", \"smc\", \"swc\", \"fig\"]]. Files are alternatives only when all their extensions are in one family, so a disc's cue and bin both go. The set's size counts the same file."),
    (KEY_COPY_PREFER_DECRYPTED, "Between copies of one release, take a decrypted dump before an encrypted one -- told by \"Encrypted\" in its folder or file name (true | false; default true). A copy put first by hand still comes first."),
    (KEY_COPY_LOSSY_WARNING, "Ask before choosing an output that keeps a disc's files but not the disc -- wua (Wii U), zar (Xbox 360), xiso (Xbox) -- since it can't be turned back into the original image (true | false; default true). The question's \"don't ask again\" turns it off."),
    (KEY_INTEGRITY_POPULATE_HASH, "Which hash the integrity check compares files against: xxh3_128 (default) | crc32 | md5 | sha1 | sha256. Used whenever the catalogue records it for the file's content; where it does not, the check compares against the cheapest recorded hash on this CPU (crc32, then xxh3_128, then sha1/sha256 where the CPU has hardware SHA and md5 where it does not) and records this one off the same read once the file has matched, so later checks use it."),
    (KEY_VIEW_DEFAULTS, "Default view per list: {\"titles\": \"grid\", \"releases\": \"grid\", \"games\": \"grid\", \"ra\": \"badges\"}; collection lists list|grid (default grid), RetroAchievements list|grid|icon|title|screenshot|badges (default badges)."),
];

/// Media types a game has one of, so a provider order decides whose is used. Everything else
/// (screenshots, fan art, videos) is a list and comes from every provider.
/// `media.source` of what a user added (upload or URL), and its name in `media_priority`.
pub const MANUAL_SOURCE: &str = "manual";

/// Providers whose art is never for a region: SteamGridDB's covers are community art for the
/// game, RetroAchievements' box is one per achievement set. Their files still land in a region
/// folder when every release they are linked to is in one group, so the folder alone would
/// rank them as region art above another provider's common box. `catalog::media_order` lists
/// the same names in SQL.
pub const REGIONLESS_SOURCES: [&str; 2] = ["steamgriddb", "retroachievements"];

/// Whether art from `source` filed at `path` is region art: in a region folder and from a
/// provider that has regions, or a user's pick wherever it is filed.
pub fn is_region_art(source: Option<&str>, path: &str) -> bool {
    match source {
        None => true,
        Some(s) if s == MANUAL_SOURCE => true,
        Some(s) if REGIONLESS_SOURCES.contains(&s) => false,
        Some(_) => !path.replace('\\', "/").contains("/common/"),
    }
}

pub const SINGLE_MEDIA_TYPES: [&str; 6] = ["box", "box_3d", "cover", "logo", "icon", "title_screen"];

/// What each provider's sync can download today. The settings screens draw their checkboxes
/// from this, so a type appears there when the code that fetches it exists and not before.
/// (IGDB's videos are YouTube links, not files, so they are not here.)
pub const MEDIA_CAPABILITIES: [(Provider, &[&str]); 5] = [
    (Provider::Igdb, &["box", "screenshot", "fanart"]),
    (Provider::SteamGridDb, &["cover", "fanart", "logo", "icon"]),
    (Provider::MobyGames, &["box"]),
    (Provider::RetroAchievements, &["box", "title_screen", "screenshot"]),
    // Only ever downloaded by hand ("download art"), for its small allowance.
    (Provider::TheGamesDb, &["box", "title_screen", "screenshot", "logo", "fanart"]),
];

/// Art sources no provider link stands for: their art is found by the release's own serial
/// (`providers::xlenore`). Offered in the media settings beside the providers.
pub const SERIAL_ART_SOURCES: [(&str, &[&str]); 1] = [(super::providers::xlenore::NAME, &super::providers::xlenore::TYPES)];

/// A name `media_download`, `media_priority` and `media_export_priority` accept: your own
/// uploads, a provider, or a serial art source.
pub fn is_media_source(name: &str) -> bool {
    name == MANUAL_SOURCE || Provider::from_name(name).is_some() || SERIAL_ART_SOURCES.iter().any(|(n, _)| *n == name)
}

/// The types a sync downloads unless told otherwise: what it always fetched. Everything else
/// has to be ticked -- screenshots and fan art for a whole catalogue are tens of gigabytes
/// and several provider requests per game, which is not something to start by surprise.
pub const MEDIA_DEFAULT_ON: [&str; 2] = ["box", "cover"];

/// The four media settings, read once per job or request.
#[derive(Debug, Clone, Default)]
pub struct MediaRules {
    download: serde_json::Map<String, serde_json::Value>,
    priority: serde_json::Map<String, serde_json::Value>,
    show: serde_json::Map<String, serde_json::Value>,
    keep: serde_json::Map<String, serde_json::Value>,
}

impl MediaRules {
    pub async fn load(pool: &MySqlPool) -> Result<Self, DatabaseError> {
        let rows = sqlx::query("SELECT `key`, value FROM settings WHERE `key` IN ('media_download', 'media_priority', 'media_show', 'media_client_keep')").fetch_all(pool).await?;
        let mut rules = Self::default();
        for row in &rows {
            let key: String = row.try_get("key")?;
            let serde_json::Value::Object(map) = row.try_get::<serde_json::Value, _>("value")? else { continue };
            match key.as_str() {
                KEY_MEDIA_DOWNLOAD => rules.download = map,
                KEY_MEDIA_PRIORITY => rules.priority = map,
                KEY_MEDIA_SHOW => rules.show = map,
                _ => rules.keep = map,
            }
        }
        Ok(rules)
    }

    /// Whether a sync may download `media_type` from `provider`.
    pub fn downloads(&self, provider: &str, media_type: &str) -> bool {
        self.download.get(provider).and_then(|types| types.get(media_type)).and_then(serde_json::Value::as_bool).unwrap_or_else(|| MEDIA_DEFAULT_ON.contains(&media_type))
    }

    /// The order for `media_type`, your own uploads ([`MANUAL_SOURCE`]) included: first
    /// unless the stored order places them.
    fn order(&self, media_type: &str) -> Vec<&str> {
        let mut order: Vec<&str> = self.priority.get(media_type).and_then(serde_json::Value::as_array).map(|list| list.iter().filter_map(serde_json::Value::as_str).collect()).unwrap_or_default();
        if !order.contains(&MANUAL_SOURCE) {
            order.insert(0, MANUAL_SOURCE);
        }
        order
    }

    /// The sources ranked above `provider` for a single-item `media_type` -- providers, and
    /// [`MANUAL_SOURCE`] where your uploads rank higher. Empty for list types, which every
    /// provider supplies whatever is already there.
    pub fn outranking(&self, provider: &str, media_type: &str) -> Vec<&str> {
        if !SINGLE_MEDIA_TYPES.contains(&media_type) {
            return Vec::new();
        }
        let order = self.order(media_type);
        match order.iter().position(|name| *name == provider) {
            Some(at) => order[..at].to_vec(),
            // Unlisted providers come after every listed one.
            None => order,
        }
    }

    /// Where `source` stands for `media_type` -- what a page shows first: its place in the
    /// order, after it when unlisted. Your uploads are first unless the order places them.
    pub fn rank(&self, source: &str, media_type: &str) -> usize {
        self.order(media_type).iter().position(|name| *name == source).unwrap_or(usize::MAX)
    }

    /// Where `source` stands for the front picture -- a box front and a cover are one slot --
    /// by the box order, then the cover order (as `catalog::media_priority_list`).
    pub fn front_rank(&self, source: &str) -> usize {
        let boxes = self.order("box");
        boxes.iter().position(|name| *name == source).or_else(|| self.order("cover").iter().position(|name| *name == source).map(|at| boxes.len() + at)).unwrap_or(usize::MAX)
    }

    /// Whether the stored order for `media_type` places your uploads; otherwise they come
    /// before everything.
    pub fn places_manual(&self, media_type: &str) -> bool {
        self.priority.get(media_type).and_then(serde_json::Value::as_array).is_some_and(|list| list.iter().any(|v| v.as_str() == Some(MANUAL_SOURCE)))
    }

    pub fn shows(&self, media_type: &str) -> bool {
        self.show.get(media_type).and_then(serde_json::Value::as_bool).unwrap_or(true)
    }

    /// Client mode: whether this machine keeps its own copy of `media_type`.
    pub fn keeps(&self, media_type: &str) -> bool {
        // Video and audio are streamed unless ticked: a kept video is its full size, and
        // keeping audio means fetching the server's whole tagged library.
        self.keep.get(media_type).and_then(serde_json::Value::as_bool).unwrap_or(!matches!(media_type, "video" | "bgm"))
    }
}

pub async fn media_list_cap(pool: &MySqlPool) -> Result<usize, DatabaseError> {
    Ok(get::<u64>(pool, KEY_MEDIA_LIST_CAP).await?.unwrap_or(6).clamp(1, 50) as usize)
}

pub async fn media_sync_after_pull(pool: &MySqlPool) -> Result<bool, DatabaseError> {
    Ok(get::<bool>(pool, KEY_MEDIA_SYNC_AFTER_PULL).await?.unwrap_or(true))
}

pub async fn search_translations(pool: &MySqlPool) -> Result<bool, DatabaseError> {
    Ok(get::<bool>(pool, KEY_SEARCH_TRANSLATIONS).await?.unwrap_or(true))
}

pub async fn media_reuse_existing(pool: &MySqlPool) -> Result<bool, DatabaseError> {
    Ok(get::<bool>(pool, KEY_MEDIA_REUSE_EXISTING).await?.unwrap_or(true))
}

pub async fn media_provider_fallback(pool: &MySqlPool) -> Result<bool, DatabaseError> {
    Ok(get::<bool>(pool, KEY_MEDIA_PROVIDER_FALLBACK).await?.unwrap_or(true))
}

pub async fn hasheous_parallel_downloads(pool: &MySqlPool) -> Result<usize, DatabaseError> {
    Ok(get::<u64>(pool, KEY_HASHEOUS_PARALLEL).await?.map_or(4, |n| n.clamp(1, 32) as usize))
}

/// `None` = background gap fill off (-1); `Some(0)` = unlimited; `Some(n)` = n per batch.
pub async fn playmatch_batch_size(pool: &MySqlPool) -> Result<Option<u64>, DatabaseError> {
    Ok(playmatch_batch_limit(get::<i64>(pool, KEY_PLAYMATCH_BATCH).await?))
}

/// `playmatch_batch_size`'s meaning of a stored value (unset = 100).
pub fn playmatch_batch_limit(stored: Option<i64>) -> Option<u64> {
    match stored.unwrap_or(100) {
        n if n < 0 => None,
        n => Some(n as u64),
    }
}

/// The hash set a file scan computes. Absent or partial settings fall back to the defaults
/// per algorithm, so adding an algorithm later does not silently disable it.
///
/// `ra_hash` is returned as a separate flag rather than folded into [`HashFlags`]: it is the
/// only one that needs a RetroAchievements console id, and `hash::hash` rejects the whole
/// call when that is missing. The scan asks for it only for files whose platform resolved.
/// The sections the settings form is laid out in: (id, heading), in display order.
pub const SETTING_GROUPS: [(&str, &str); 12] = [
    ("display", "Display & language"),
    ("lists", "Lists & ordering"),
    ("scan", "Scanning & files"),
    ("copy", "Copy & conversion"),
    ("metadata", "Metadata & providers"),
    ("ai", "AI"),
    ("media", "Media & art"),
    ("audio", "Audio"),
    ("ra", "RetroAchievements"),
    ("backup", "Backup"),
    ("system", "This installation"),
    ("other", "Other"),
];

/// What the settings form shows behind a setting's "!", paragraph by paragraph, for a key
/// whose [`KNOWN_KEYS`] description is written for someone editing the JSON (value shapes,
/// defaults) rather than for someone choosing on the form. Each UI shows these in the one "!"
/// a setting has, instead of the description, and keeps only short hints next to the editor.
pub const EXPLAIN: &[(&str, &[&str])] = &[
    (crate::database::title_family::KEY_PC_MAX_AGE, &[
        "Collection > Definitive puts a game's newest version first, and between versions of the same year the platform priority decides.",
        "A PC version is often a port of an older version: it takes the \"PC (Definitive tab)\" place in the platform priority only while it is less than this many years older than the newest version, so a recent PC port can lead but a decades-old one does not.",
        "0: PC versions keep their own year and PC's own place in the list.",
    ]),
    (KEY_TITLE_DISPLAY_MODE, &[
        "How titles and releases are named in the Titles and Releases lists, grids and their pages. Take a game released as Hoge Adventure in the USA, ホゲの冒険 in Japan and L'aventure d'Hoge in France:",
        "Off (romanized): every title and release under its romanized name, the DAT name: Hoge Adventure for the title, and the Japanese release as Hoge no Bouken.",
        "On (localized): each release under its name in its own language, where IGDB has one: the Japanese release as ホゲの冒険, the French one as L'aventure d'Hoge, the US one as Hoge Adventure. A title takes the name of its first release (region_priority).",
        "Custom: the first language in your list that has a name. With ja, en: ホゲの冒険 wherever a Japanese name exists, Hoge Adventure otherwise (en = romanized).",
        "Display only: sorting, matching and merging use the canonical name (canonical_name_priority).",
    ]),
    (KEY_REGION_PRIORITY, &[
        "Which release of a title comes first, and so whose box art and name represent the title: the first region in the list wins; regions not listed come last.",
        "Before region, releases rank by kind: retail first, then unlicensed, aftermarket, translations, hacks and pirate copies; demos, betas and prototypes come after every full version, RetroAchievements subsets last. Within a region, a release that works with RetroAchievements comes first; language_priority breaks the remaining ties.",
    ]),
    (KEY_CANONICAL_NAME_PRIORITY, &[
        "Which release names a title in the database: its canonical (normalized) name, used to sort, match and merge titles. It is not what is shown; title_display_mode decides that.",
        "Commercial releases first, then the first entry a release matches: a region (upper case: USA, EUR, JPN) or a language it has (lower case: en, ja). Anything not listed comes last. Default: USA, EUR, WORLD, ASIA, en, JPN.",
        "The same order picks the release whose name an export uses when it converts names to the release name.",
        "Recommended: leave this as it is. It is for the database's sorting and matching, not for display. After a change, run a title dedupe to rename merged titles.",
    ]),
    (KEY_RELEASE_KINDS, &[
        "Which releases Collection › Titles and Releases list by default; the page can switch it.",
        "All (default): everything, so nothing you own seems missing. Commercial + patches: leaves out RetroAchievements subsets. Commercial: official releases only.",
        "Patches are user-created patches of a commercial release: ROM hacks, fan translations and the like. Subsets are RetroAchievements achievement subsets: extra achievement sets for a game (bonus, challenge and so on), listed as releases of their own.",
    ]),
    (super::release_buckets::KEY_UNLICENSED, &[
        "Whether an unlicensed release (Unl) is a kind of its own or counts as retail.",
        "Separate (default): unlicensed releases get their own row in Collection › Titles and their own kind in the filter, and rank after retail.",
        "With retail: an unlicensed release with no other tag is listed and ranked with the title's retail releases. An unlicensed demo or pirate copy stays a demo or pirate copy.",
    ]),
    (KEY_RELEASE_EDITION_ORDER, &[
        "The order of releases within one region / language. Your manual order on a title page beats it.",
        "Which edition of a game comes first. Default: a budget reissue (\"PlayStation 2 the Best\", \"Greatest Hits\"), usually the latest data, then the original release, then a re-release (Virtual Console, a collection), then special editions, then aftermarket.",
        "Within one edition the latest version comes first.",
    ]),
    (KEY_SHEET_STYLE, &[
        "How a disc's sheet is written and read: redump (default), redump_catalog, chdman or mame. It decides the cue/gdi rebuilt from a CHD wherever one is -- the members a scan records, a copy's conversion and its check, an extraction -- and whether a GD-ROM is a cue (Redump) or a gdi (chdman, MAME).",
        "Redump (CRLF line endings) is the one every DAT is written against, so it is the only one a rebuilt sheet can be checked against. A loose cue on disk is always hashed as it is; changing this only changes sheets rebuilt from a CHD, and a rescan replaces the old sheet's link.",
    ]),
    (KEY_DAT_SYNC_PLATFORMS, &[
        "Which platforms a DAT sync downloads and imports. All (default): every platform. On file scan: the platforms of your scanned files, as the scan placed them (folder setting, file hints, extension). On file scan, strict: only platforms a hash match has confirmed against a DAT -- not recommended, as a platform whose DAT isn't imported yet can never be confirmed. Manual: the platforms ticked here.",
        "Redump, MAME Redump and the GitHub sources skip the other platforms' files. No-Intro and TOSEC only offer one pack for every platform, so it is still downloaded, and only these platforms' DATs are imported from it. Only what is ticked is fetched: DATs that aren't a platform's games (audio CDs, video discs, manuals and scans, source code) and DATs of an unknown platform have checkboxes of their own, all ticked in All and unticked otherwise until you tick them. The platforms of the provider download's \"bulk: checked platforms\" start ticked from this setting too.",
    ]),
    (KEY_CHD_READER, &[
        "Who reads a CHD's header and track list when a scan, copy or extraction needs it. chdman runs one thing at a time, so asking it waits behind any CHD conversion that is running; the built-in reader reads the same fields from the file directly.",
        "Checked (default): the built-in reader, compared with chdman whenever chdman is free. Where they differ, chdman's answer is used and the difference is listed in the scan's result, for you to report. chdman: chdman only, as before. Built-in only: no comparison, so a mistake in the built-in reader would go unnoticed. Not recommended yet.",
    ]),
    (KEY_SERIAL_MATCH, &[
        "Lenient matching, for formats no DAT has a hash for: a .cia, an .xiso, a .wua or a .zar is matched by the serial in its header (CTR-P-XXXX, MS-2022, WUP-P-XXXX) against the serials the DATs list. Such a match is marked \"serial match\": it says which game the file is, not that its contents are a verified dump.",
        "Manifest: a .zar this app wrote records the serial of the disc it came from, and it is used too. File name: a serial written in the file name is tried as well, after the header's. It is off by default because a name can be wrong.",
        "When one serial belongs to several releases, the region in the file name decides, then the source order under Metadata source priority. A game still tied is left unmatched.",
    ]),
    (KEY_SCAN_HASHES, &[
        "Which hashes a file scan computes. Default: everything except sha256.",
        "Content is identified by hash, so at least one of crc32, xxh3_128, md5, sha1 and sha256 must stay on. ra_hash (RetroAchievements) is skipped where the platform has no RetroAchievements console id.",
    ]),
    (KEY_COPY_RECENT_DESTINATIONS, &[
        "The destination folders the copy dialog offers again, most recent first. Written by the app as you copy; remove the ones you no longer want, or reset to forget them all.",
    ]),
    (KEY_COPY_FOLDER_SCHEME, &[
        "How a copy names its destination subdirectories: es (snes, the EmulationStation / RetroArch short name; default), slug (snes, the IGDB slug RomM matches folders against), display (Super Nintendo Entertainment System), dat (Nintendo - Super Nintendo Entertainment System) or custom (the names set per platform under Config › Copy).",
    ]),
    (KEY_COPY_PLATFORM_COMBINE, &[
        "Platforms this app splits but a folder layout should not: a group that is on puts the child's games in the parent's folder (Famicom games in the NES folder, for example). \"(default)\" leaves the built-in choice.",
    ]),
    (KEY_COPY_POPULATE_HASH, &[
        "The hash a verified copy compares what it wrote against. Default: xxh3_128, far stronger than crc32 at nearly its speed. Set separately from the integrity check's hash.",
        "Used whenever the catalogue records it for that content. Where it does not, the copy compares against the cheapest recorded hash on this CPU and records this one off the same read once the bytes have matched, so later checks use it. Only a hash the catalogue already knew can settle whether the bytes are right.",
    ]),
    (KEY_MEDIA_DOWNLOAD, &[
        "What a provider sync downloads, per provider and media type. Untick a type and that provider's sync leaves it alone; what is already downloaded stays.",
        "Box and cover are on unless unticked. Everything else is off until ticked: for a whole catalogue, screenshots and fan art are many gigabytes and extra provider requests for every game.",
        "Files you add yourself (upload or URL) are never blocked.",
    ]),
    (KEY_MEDIA_PRIORITY, &[
        "Per media type, whose comes first: the providers and your own uploads (\"manual\"). For media a game has one of (box, 3D box, cover, logo, icon, title screen) the first that has one is used, and a lower provider's is not downloaded once a higher source has it.",
        "Your uploads are first unless you place them lower, for example below a provider whose art you prefer; that provider's is then downloaded and shown even where you added your own.",
        "Screenshots, fan art and videos come from every provider; here the order only decides which are shown first.",
    ]),
    (KEY_MEDIA_CLIENT_KEEP, &[
        "Client mode only: which media types this machine keeps its own copy of, fetched from the main server; images are stored as AVIF when that is smaller. Not ticked: nothing is stored, and a page streams it from the main server when it shows it.",
        "Video and bgm (audio) are off unless ticked. A video is kept in its original format, so streaming is usually better; kept audio is fetched in the format audio_client_format names.",
    ]),
    (KEY_MEDIA_THUMB_FORMAT, &[
        "The format grid thumbnails are made in. Default: AVIF. WebP and JPEG XL are placeholders that fall back to JPEG for now; a client may still ask for another format per request.",
    ]),
    (KEY_MEDIA_CACHE_TIER1_MB, &[
        "Size cap of the tier 1 media cache, in MiB; 0 = no cap. The least recently used full-size copies are evicted first, thumbnails last.",
    ]),
    (KEY_METADATA_SOURCE_PRIORITY, &[
        "Per field: which source's value is shown when several sources have one. Drag to order; sources not listed fall back to whatever exists.",
        "Each field offers only the sources that can supply it: genre from IGDB, RetroAchievements and MobyGames; serial detected, Redump or No-Intro; franchise IGDB; release name the DAT sources and IGDB.",
        "The release name order also decides which DAT's release a hash match links a file to when several list it equally (a TOSEC and a No-Intro release of the same game, say). Run Hash match all to apply a changed order to files already linked.",
        "The genre order also decides the genre filters of Titles, Releases and Games: a title is filtered by the genres of the first source in this order that gave it any. The RetroAchievements tab always uses RetroAchievements' own genre.",
    ]),
    (KEY_UI_ICONS, &[
        "An icon / favicon URL shown before a value (platform, region, language, source).",
        "Without one, platforms use assets/icons/<slug>.png or .svg, or the built-in silhouette; regions and languages an emoji flag.",
    ]),
    (KEY_UI_LANGUAGE, &[
        "The language of the UI's own text: menus, buttons, headings. Platform names have their own setting (platform_name_language).",
    ]),
    (KEY_AUDIO_DSD_CACHE_FORMAT, &[
        "What audio the browser cannot play (DSD, ALAC, AIFF) is converted to when it is played from this machine: AAC (default; 256 kbps, small) or FLAC (lossless, several times larger).",
        "Converted once, on first play, and cached; audio_cache_max_mb caps the cache. Without the AAC encoder library the lossless form is used either way.",
    ]),
    (KEY_AUDIO_CACHE_MAX_MB, &[
        "Size cap of the converted-audio cache, in MiB (default 4096; 0 = no cap).",
        "The least recently played conversions are removed first and made again when next needed.",
    ]),
    (KEY_AUDIO_CLIENT_FORMAT, &[
        "Client mode: the format this machine fetches its kept copies of the main server's audio in (bgm ticked under media_client_keep). The server does the converting.",
        "AAC plays everywhere, Linux included, and is the default on Linux. xHE-AAC saves the most storage and plays on Windows 11, macOS, iOS and Android, and is the default there. No conversion keeps the original files.",
    ]),
    (KEY_AUDIO_CLIENT_BITRATE, &[
        "Client mode: the bitrate a kept copy is converted to (48-320 kbps, default 128).",
        "A track already at or below it is copied as it is; DSD, ALAC and AIFF are always converted.",
    ]),
    (KEY_AUDIO_TITLE_SOURCES, &[
        "Where a title page's music comes from: the audio files tagged with the title (soundtrack CDs, concerts), the music inside the title's ROMs, or both (default).",
        "ROM music is played from the ROM itself, like a CD player: run Index ROM audio on the Audio tab first. A track the browser cannot play is converted on first play and kept in the converted-audio cache (audio_cache_max_mb).",
    ]),
    (KEY_AUDIO_ROM_KINDS, &[
        "Which sounds from a ROM a title page plays: music only (default), music and jingles, or everything including sound effects.",
        "Sounds are sorted by whether they loop and how long they are (the three lengths below).",
    ]),
    (KEY_AUDIO_ROM_NOT_OWNED, &[
        "ROM music for a title with no ROM on this machine: another version of the game you do have (the original of a remake, a port), music from another title picked at random, or none.",
        "When this finds nothing, audio_fallback decides.",
    ]),
    (KEY_AUDIO_ROM_LOOP_MIN_S, &[
        "Seconds. A ROM sound that loops and is at least this long is music (default 3).",
    ]),
    (KEY_AUDIO_ROM_BGM_MIN_S, &[
        "Seconds. A ROM sound that does not loop is music from this length (default 60), a jingle from audio_rom_jingle_min_s (default 8), and a sound effect below that.",
    ]),
    (KEY_AUDIO_FALLBACK, &[
        "What a title with no audio linked to it plays: nothing (default), tracks picked at random from the audio linked to other titles, or the audio files tagged generic_bgm.",
        "generic_bgm is a tag of its own: it is put on audio files and taken off them like any keyword tag, but a title cannot carry it and it cannot be renamed or deleted.",
    ]),
    (KEY_ALLOW_REMOTE_LAUNCH, &[
        "Emulators start on the machine running the app. The desktop app and a browser on this machine can always launch; this lets clients on other machines start them too.",
        "Emulator launching is not tested yet.",
    ]),
    (KEY_RA_SOURCE, &[
        "RetroAchievements tab: which games the lists show by default. All: every game RetroAchievements has achievements for (as cached by its bulk sync), the ones held here first. Local: only the ones held here.",
    ]),
    (KEY_PLAYMATCH_BATCH, &[
        "Background PlayMatch gap filler: at most this many contents per batch (default 100); 0 = unlimited (still 100 per request, rate-limited); -1 = off.",
        "It runs only while no other sync job is queued, and only for providers with usable credentials.",
    ]),
];

/// The paragraphs behind `key`'s "!", when it has its own ([`EXPLAIN`]).
pub fn explain(key: &str) -> &'static [&'static str] {
    EXPLAIN.iter().find(|(k, _)| *k == key).map_or(&[], |(_, p)| *p)
}

/// The value a key behaves as when it is not set, for a key whose default depends on the
/// build -- so a form can show it selected. `None` for the rest (their defaults are fixed and
/// the forms know them).
pub fn platform_default(key: &str) -> Option<serde_json::Value> {
    match key {
        KEY_AUDIO_CLIENT_FORMAT => Some(serde_json::Value::from(default_audio_client_format())),
        KEY_PLATFORM_PRIORITY => Some(serde_json::json!(default_platform_priority())),
        _ => None,
    }
}

/// `audio_client_format` when unset: AAC on Linux, where xHE-AAC has no system decoder to play
/// it with; xHE-AAC elsewhere (Windows 11, macOS, iOS, Android all play it), where it is the
/// smallest.
pub fn default_audio_client_format() -> &'static str {
    if cfg!(target_os = "linux") {
        "aac"
    } else {
        "xhe_aac"
    }
}

/// Which [`SETTING_GROUPS`] section a key belongs in. Unknown keys go to "other".
pub fn group_of(key: &str) -> &'static str {
    match key {
        KEY_UI_LANGUAGE | KEY_PLATFORM_NAME_LANGUAGE | "title_display_mode" | "language_display" | "ui_colors" | "ui_icons" | "grid_media_type" => "display",
        "release_kinds_default" | "release_unlicensed" | KEY_COLLECTION_DEFAULT_TAB | KEY_COLLECTION_HIDDEN_TABS | "definitive_fetch_versions" | KEY_PLATFORM_PRIORITY | "definitive_pc_max_age" | "definitive_two_cover_layout" | "release_edition_order" | "view_defaults" | "region_priority" | "language_priority" | "canonical_name_priority" => "lists",
        KEY_SCAN_HASHES | KEY_SCAN_HEURISTICS | KEY_SCAN_PKG_TITLES | KEY_SERIAL_MATCH | KEY_SCAN_PARALLEL | KEY_SHEET_STYLE | KEY_CHD_READER | "integrity_populate_hash" => "scan",
        k if k.starts_with("copy_") => "copy",
        KEY_GEMINI_MODEL => "ai",
        k if k.starts_with("ai_") => "ai",
        KEY_DAT_SYNC_PLATFORMS | KEY_MERGE_REVIEW_ORDER | "provider_priority" | "provider_sync_scope" | "raw_response_retention" | "cache_ttl_days" | "metadata_source_priority" | "hasheous_parallel_downloads" | "playmatch_batch_size" => "metadata",
        k if k.starts_with("media_") => "media",
        k if k.starts_with("audio_") => "audio",
        k if k.starts_with("ra_") => "ra",
        k if k.starts_with("backup_") => "backup",
        "instance_mode" | "catalog_from" | "instance_id" | "allow_remote_launch" | "temp_dir_margin" => "system",
        _ => "other",
    }
}

/// A stored `*_parallel_files` setting; unset or unreadable is auto.
pub async fn parallel_files(pool: &MySqlPool, key: &str) -> Result<crate::api::parallel::ParallelSetting, DatabaseError> {
    Ok(get::<serde_json::Value>(pool, key).await?.and_then(|v| crate::api::parallel::ParallelSetting::parse(&v).ok()).unwrap_or_default())
}

pub async fn scan_hashes(pool: &MySqlPool) -> Result<(crate::rom::hash::HashFlags, bool), DatabaseError> {
    use crate::rom::hash::HashFlags;
    let v = get::<serde_json::Value>(pool, KEY_SCAN_HASHES).await?.unwrap_or(serde_json::Value::Null);
    let on = |key: &str, default: bool| v.get(key).and_then(|b| b.as_bool()).unwrap_or(default);
    let mut flags = HashFlags::empty();
    for (key, bit, default) in [
        ("crc32", HashFlags::CRC32, true),
        ("xxh3_128", HashFlags::XXH3_128, true),
        ("md5", HashFlags::MD5, true),
        ("sha1", HashFlags::SHA1, true),
        // Off by default: a whole extra pass over every byte, and `import.rs` already
        // identifies content by sha1 when there is no sha256.
        ("sha256", HashFlags::SHA256, false),
    ] {
        if on(key, default) {
            flags.insert(bit);
        }
    }
    Ok((flags, on("ra_hash", true)))
}

/// The `chd_reader` setting, put into effect for this process (`chd_info::read_chd_metadata`
/// reads it). Called where CHDs are about to be read: a scan, a copy, a settings change.
pub async fn apply_chd_reader(pool: &MySqlPool) -> Result<crate::rom::chd_info::ChdReaderMode, DatabaseError> {
    use crate::rom::chd_info::{set_reader_mode, ChdReaderMode};
    let stored: Option<String> = get(pool, KEY_CHD_READER).await?;
    let mode = stored.as_deref().and_then(ChdReaderMode::from_name).unwrap_or(ChdReaderMode::Checked);
    set_reader_mode(mode);
    Ok(mode)
}

/// Turns [`KEY_SCAN_PKG_TITLES`] on or off for this process's detection.
pub async fn apply_scan_pkg_titles(pool: &MySqlPool) -> Result<bool, DatabaseError> {
    let on = get::<bool>(pool, KEY_SCAN_PKG_TITLES).await?.unwrap_or(false);
    crate::rom::platforms::sony::set_read_package_titles(on);
    Ok(on)
}

/// The platforms [`KEY_SERIAL_MATCH`]'s `file_name_platforms` can set one by one (IGDB ids):
/// those a serial-matched format is on -- 3DS and New 3DS, Wii U, Xbox and Xbox 360, Vita,
/// PS1 / PS2 / PS3, PSP and its PS1 classics, Switch. Both settings forms list these.
pub const SERIAL_FILE_NAME_PLATFORMS: [u16; 12] = [37, 137, 41, 11, 12, 46, 7, 8, 9, 38, 30009, 130];

/// [`KEY_SERIAL_MATCH`] as stored, with its defaults filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialMatch {
    /// The formats in [`SERIAL_MATCH_FORMATS`] that are on.
    pub formats: Vec<&'static str>,
    pub manifest: bool,
    /// Use a serial in the file name: the default for a platform not in
    /// `file_name_platforms` (the one setting from before it was per platform).
    pub file_name: bool,
    /// Per platform (IGDB id): use a serial in the file name or not.
    pub file_name_platforms: std::collections::BTreeMap<u16, bool>,
}

impl SerialMatch {
    pub fn from_value(v: &serde_json::Value) -> Self {
        let on = |key: &str, default: bool| v.get(key).and_then(|b| b.as_bool()).unwrap_or(default);
        let file_name_platforms = v.get("file_name_platforms").and_then(|m| m.as_object()).into_iter().flatten().filter_map(|(id, on)| Some((id.parse().ok()?, on.as_bool()?))).collect();
        Self { formats: SERIAL_MATCH_FORMATS.iter().filter(|(f, d)| on(f, *d)).map(|(f, _)| *f).collect(), manifest: on("manifest", true), file_name: on("file_name", false), file_name_platforms }
    }

    /// Whether a serial in the file name counts for a file of `platform`: its own choice,
    /// else the default (`file_name`).
    pub fn file_name_for(&self, platform: crate::common::models::Platform) -> bool {
        self.file_name_platforms.get(&platform.igdb_id()).copied().unwrap_or(self.file_name)
    }
}

pub async fn serial_match(pool: &MySqlPool) -> Result<SerialMatch, DatabaseError> {
    let v = get::<serde_json::Value>(pool, KEY_SERIAL_MATCH).await?.unwrap_or(serde_json::Value::Null);
    Ok(SerialMatch::from_value(&v))
}

/// `(ext, path)` -- which detection heuristics a scan may use. Both default on.
pub async fn scan_heuristics(pool: &MySqlPool) -> Result<(bool, bool), DatabaseError> {
    let v = get::<serde_json::Value>(pool, KEY_SCAN_HEURISTICS).await?.unwrap_or(serde_json::Value::Null);
    let on = |key: &str| v.get(key).and_then(|b| b.as_bool()).unwrap_or(true);
    Ok((on("ext"), on("path")))
}

pub async fn media_cache_caps(pool: &MySqlPool) -> Result<(u64, u64), DatabaseError> {
    let mb = |v: Option<u64>| v.unwrap_or(0) * 1024 * 1024;
    Ok((mb(get::<u64>(pool, KEY_MEDIA_CACHE_TIER1_MB).await?), mb(get::<u64>(pool, KEY_MEDIA_CACHE_TIER2_MB).await?)))
}

/// (longest edge px, quality, format name)
pub async fn media_thumb(pool: &MySqlPool) -> Result<(u32, u8, String), DatabaseError> {
    Ok((
        get::<u64>(pool, KEY_MEDIA_THUMB_PX).await?.map_or(320, |n| n.clamp(32, 2048) as u32),
        get::<u64>(pool, KEY_MEDIA_THUMB_QUALITY).await?.map_or(60, |n| n.clamp(1, 100) as u8),
        get::<String>(pool, KEY_MEDIA_THUMB_FORMAT).await?.unwrap_or_else(|| "avif".to_string()),
    ))
}

/// Every stored row: (key, value, updated_at).
pub async fn list_all(pool: &MySqlPool) -> Result<Vec<(String, serde_json::Value, chrono::NaiveDateTime)>, DatabaseError> {
    let rows = sqlx::query("SELECT `key`, value, updated_at FROM settings ORDER BY `key`").fetch_all(pool).await?;
    rows.iter().map(|row| Ok((row.try_get("key")?, row.try_get("value")?, row.try_get("updated_at")?))).collect()
}

pub async fn delete(pool: &MySqlPool, key: &str) -> Result<bool, DatabaseError> {
    let result = sqlx::query("DELETE FROM settings WHERE `key` = ?").bind(key).execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

pub async fn get<T: DeserializeOwned>(pool: &MySqlPool, key: &str) -> Result<Option<T>, DatabaseError> {
    let row = sqlx::query("SELECT value FROM settings WHERE `key` = ?").bind(key).fetch_optional(pool).await?;
    match row {
        Some(row) => {
            let value: serde_json::Value = row.try_get("value")?;
            Ok(Some(serde_json::from_value(value)?))
        }
        None => Ok(None),
    }
}

/// This machine's id, generated and stored the first time it is asked for.
///
/// Deliberately opaque and meaningless -- it identifies an *installation*, not a user or a
/// host, so it carries nothing about the machine when it travels inside an exported file.
pub async fn instance_id(pool: &MySqlPool) -> Result<String, DatabaseError> {
    if let Some(existing) = get::<String>(pool, KEY_INSTANCE_ID).await? {
        if !existing.is_empty() {
            return Ok(existing);
        }
    }
    let id: String = {
        use rand::Rng;
        let mut rng = rand::rng();
        (0..32).map(|_| char::from_digit(rng.random_range(0..16), 16).unwrap_or('0')).collect()
    };
    // `INSERT IGNORE`-style: two processes asking at once must agree, so whoever lost the
    // race reads the winner's value back rather than overwriting it.
    sqlx::query("INSERT IGNORE INTO settings (`key`, value) VALUES (?, ?)").bind(KEY_INSTANCE_ID).bind(serde_json::to_value(&id)?).execute(pool).await?;
    Ok(get::<String>(pool, KEY_INSTANCE_ID).await?.unwrap_or(id))
}

/// The three `settings` rows that describe *this* installation rather than configure it, and
/// so must survive an import of another machine's `settings` table intact.
///
/// `settings` is worth importing (it is configuration), but a backup or SQLite cache carries
/// the identity of the machine that made it. Taking that would make this installation claim
/// to be the other one -- and `instance_id` is what decides "is this my own data coming
/// back", `instance_mode` whether this machine may build a catalogue at all, and
/// `catalog_from` which server's numbering its catalogue uses. Importing a client's settings
/// onto a standalone would silently make it a client with a locally built catalogue still in
/// place; importing a standalone's `catalog_from` onto a client would forge a lineage and let
/// peer ownership attach to unrelated rows. Cheaper and more reliable to capture the three
/// before the write and put them back after than to filter rows out of a generic table copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub instance_id: String,
    /// Raw stored value, not parsed: "never chosen" (absent) is a state the UI shows.
    pub instance_mode: Option<String>,
    pub catalog_from: Option<String>,
}

impl Identity {
    pub async fn capture(pool: &MySqlPool) -> Result<Self, DatabaseError> {
        Ok(Self {
            instance_id: instance_id(pool).await?,
            instance_mode: get::<String>(pool, KEY_INSTANCE_MODE).await?,
            catalog_from: get::<String>(pool, KEY_CATALOG_FROM).await?,
        })
    }

    /// Puts the captured rows back, on the connection the import used. A value that was
    /// absent before is deleted, not left as whatever the import wrote -- "unset" is
    /// meaningful for both optional keys.
    pub async fn restore(&self, conn: &mut sqlx::MySqlConnection) -> Result<(), DatabaseError> {
        const UPSERT: &str = "INSERT INTO settings (`key`, value) VALUES (?, ?) ON DUPLICATE KEY UPDATE value = VALUES(value)";
        sqlx::query(UPSERT).bind(KEY_INSTANCE_ID).bind(serde_json::Value::String(self.instance_id.clone())).execute(&mut *conn).await?;
        for (key, value) in [(KEY_INSTANCE_MODE, &self.instance_mode), (KEY_CATALOG_FROM, &self.catalog_from)] {
            match value {
                Some(v) => {
                    sqlx::query(UPSERT).bind(key).bind(serde_json::Value::String(v.clone())).execute(&mut *conn).await?;
                }
                None => {
                    sqlx::query("DELETE FROM settings WHERE `key` = ?").bind(key).execute(&mut *conn).await?;
                }
            }
        }
        Ok(())
    }
}

/// What this installation is (see [`KEY_INSTANCE_MODE`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceMode {
    /// Builds its own catalogue: imports DATs, queries providers, assigns its own row ids.
    Standalone,
    /// Takes the catalogue from its main server and never builds one of its own.
    Client,
}

impl InstanceMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::Client => "client",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "standalone" => Some(Self::Standalone),
            "client" => Some(Self::Client),
            _ => None,
        }
    }
}

/// This installation's mode, defaulting to `standalone`.
///
/// The default is deliberately the permissive one: an installation that was never asked
/// (every database created before this setting existed) keeps working exactly as it did, and
/// is only ever restricted by an explicit choice.
pub async fn instance_mode(pool: &MySqlPool) -> Result<InstanceMode, DatabaseError> {
    Ok(get::<String>(pool, KEY_INSTANCE_MODE).await?.and_then(|v| InstanceMode::parse(&v)).unwrap_or(InstanceMode::Standalone))
}

pub async fn set<T: Serialize>(pool: &MySqlPool, key: &str, value: &T) -> Result<(), DatabaseError> {
    let json = serde_json::to_value(value)?;
    sqlx::query("INSERT INTO settings (`key`, value) VALUES (?, ?) ON DUPLICATE KEY UPDATE value = VALUES(value)").bind(key).bind(json).execute(pool).await?;
    Ok(())
}

/// What the raw-response store does with a provider's response bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawRetention {
    KeepAll,
    KeepLatest,
    Discard,
}

/// Scope of a provider fetch (see docs/plan/database.md "Provider sync strategy").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncScope {
    Single,
    List,
    Full,
    ByPlatform,
    ByKeyword,
}

/// Runtime provider priority (which provider's data wins, query order), user-configurable;
/// defaults to the implementation order.
pub async fn provider_priority(pool: &MySqlPool) -> Result<Vec<Provider>, DatabaseError> {
    let stored: Option<Vec<String>> = get(pool, KEY_PROVIDER_PRIORITY).await?;
    let mut order: Vec<Provider> = stored.map(|names| names.iter().filter_map(|n| Provider::from_name(n)).collect()).unwrap_or_default();
    for p in Provider::implementation_order() {
        if !order.contains(&p) {
            order.push(p);
        }
    }
    Ok(order)
}

pub async fn raw_retention(pool: &MySqlPool) -> Result<RawRetention, DatabaseError> {
    Ok(get(pool, KEY_RAW_RETENTION).await?.unwrap_or(RawRetention::KeepLatest))
}

pub async fn sync_scope(pool: &MySqlPool, provider: Provider) -> Result<SyncScope, DatabaseError> {
    let map: Option<std::collections::HashMap<String, SyncScope>> = get(pool, KEY_SYNC_SCOPE).await?;
    Ok(map.and_then(|m| m.get(provider.name()).copied()).unwrap_or(provider.default_scope()))
}

/// The `dat_sync_platforms` setting: its mode, and the platform ids it stands for -- `None`
/// for every platform (mode `all`, or the setting unset or unreadable).
pub async fn dat_sync_platforms(pool: &MySqlPool) -> Result<(String, Option<Vec<u16>>), DatabaseError> {
    let v = get::<serde_json::Value>(pool, KEY_DAT_SYNC_PLATFORMS).await?.unwrap_or(serde_json::Value::Null);
    let mode = v.get("mode").and_then(|m| m.as_str()).filter(|m| DAT_SYNC_PLATFORM_MODES.contains(m)).unwrap_or("all").to_string();
    let ids: Option<Vec<u16>> = match mode.as_str() {
        "manual" => Some(v.get("platforms").and_then(|p| p.as_array()).map(|a| a.iter().filter_map(|x| x.as_u64()).filter_map(|x| u16::try_from(x).ok()).collect()).unwrap_or_default()),
        // As the scan placed them: the folder's platform, the file's own hint, its extension.
        "scanned" => Some(sqlx::query_scalar::<_, u16>("SELECT DISTINCT app_platform_id FROM local_rom WHERE deleted = FALSE AND app_platform_id <> 0").fetch_all(pool).await?),
        // Only what a hash match tied to a DAT release.
        "scanned_strict" => Some(
            sqlx::query_scalar::<_, u16>("SELECT DISTINCT r.app_platform_id FROM local_release_games lrg JOIN `release` r ON r.id = lrg.release_id AND r.deleted = FALSE WHERE r.app_platform_id <> 0")
                .fetch_all(pool)
                .await?,
        ),
        _ => None,
    };
    Ok((mode, ids))
}

/// `dat_sync_platforms.extras`: the DATs without a platform a DAT sync fetches, by
/// `dat_download::DAT_EXTRAS` key, in every mode. Unset: every one in mode `all` (`None`),
/// none in the others -- a platform list is a whitelist, so a DAT no platform is known
/// for (Palm OS, say) is not fetched unless its box is ticked.
pub async fn dat_sync_extras(pool: &MySqlPool) -> Result<Option<Vec<String>>, DatabaseError> {
    let v = get::<serde_json::Value>(pool, KEY_DAT_SYNC_PLATFORMS).await?.unwrap_or(serde_json::Value::Null);
    let all = v.get("mode").and_then(|m| m.as_str()).filter(|m| DAT_SYNC_PLATFORM_MODES.contains(m)).unwrap_or("all") == "all";
    Ok(match v.get("extras").and_then(|e| e.as_array()) {
        Some(a) => Some(a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()),
        None if all => None,
        None => Some(Vec::new()),
    })
}

pub async fn cache_ttl_days(pool: &MySqlPool) -> Result<u32, DatabaseError> {
    Ok(get(pool, KEY_CACHE_TTL_DAYS).await?.unwrap_or(90))
}

pub const DEFAULT_REGION_PRIORITY: [&str; 6] = ["US", "WORLD", "EU", "JP", "ASIA", "UK"];
pub const DEFAULT_LANGUAGE_PRIORITY: [&str; 2] = ["en", "ja"];
pub const DEFAULT_CANONICAL_NAME_PRIORITY: [&str; 6] = ["USA", "EUR", "WORLD", "ASIA", "en", "JPN"];

/// A canonical-name priority entry's region code as stored on releases: the setting spells
/// USA / EUR / JPN; the release codes US / EU / JP (and every other code) pass through.
pub fn canonical_region_code(entry: &str) -> String {
    match entry.to_ascii_uppercase().as_str() {
        "USA" => "US".to_string(),
        "EUR" => "EU".to_string(),
        "JPN" => "JP".to_string(),
        other => other.to_string(),
    }
}

/// The canonical-name order (`KEY_CANONICAL_NAME_PRIORITY`): regions upper case, languages
/// lower case, as stored; unset or empty = the default.
pub async fn canonical_name_priority(pool: &MySqlPool) -> Result<Vec<String>, DatabaseError> {
    let stored: Vec<String> = get::<Vec<String>>(pool, KEY_CANONICAL_NAME_PRIORITY).await?.unwrap_or_default().into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    Ok(if stored.is_empty() { DEFAULT_CANONICAL_NAME_PRIORITY.iter().map(|s| s.to_string()).collect() } else { stored })
}

/// Language codes in preference order (after region) for which release of a title comes
/// first. Unlisted languages come after the list.
pub async fn language_priority(pool: &MySqlPool) -> Result<Vec<String>, DatabaseError> {
    let stored: Option<Vec<String>> = get(pool, KEY_LANGUAGE_PRIORITY).await?;
    let mut order: Vec<String> = stored.unwrap_or_default().into_iter().map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect();
    for code in DEFAULT_LANGUAGE_PRIORITY {
        if !order.iter().any(|c| c == code) {
            order.push(code.to_string());
        }
    }
    Ok(order)
}

/// The edition order (`KEY_RELEASE_EDITION_ORDER`): every one of [`EDITION_KINDS`], the stored
/// list first and the rest after it in the default order. The two older values are what they
/// meant: budget_first the default, versions_first the original release before its budget
/// reissue.
pub async fn edition_priority(pool: &MySqlPool) -> Result<Vec<String>, DatabaseError> {
    let stored: Option<serde_json::Value> = get(pool, KEY_RELEASE_EDITION_ORDER).await?;
    let mut order: Vec<String> = match &stored {
        Some(serde_json::Value::String(s)) if s == "versions_first" => vec!["original".to_string(), "budget".to_string()],
        Some(serde_json::Value::Array(list)) => list.iter().filter_map(|v| v.as_str()).filter(|k| EDITION_KINDS.contains(k)).map(str::to_string).collect(),
        _ => Vec::new(),
    };
    order.dedup();
    for kind in EDITION_KINDS {
        if !order.iter().any(|k| k == kind) {
            order.push(kind.to_string());
        }
    }
    Ok(order)
}

/// Region codes in preference order: which release's art represents a title, and which
/// localized name/box wins when a title has several. Unlisted regions come after the list.
pub async fn region_priority(pool: &MySqlPool) -> Result<Vec<String>, DatabaseError> {
    let stored: Option<Vec<String>> = get(pool, KEY_REGION_PRIORITY).await?;
    let mut order: Vec<String> = stored.unwrap_or_default().into_iter().map(|s| s.trim().to_ascii_uppercase()).filter(|s| !s.is_empty()).collect();
    for code in DEFAULT_REGION_PRIORITY {
        if !order.iter().any(|c| c == code) {
            order.push(code.to_string());
        }
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(download: serde_json::Value, priority: serde_json::Value, show: serde_json::Value, keep: serde_json::Value) -> MediaRules {
        let map = |v: serde_json::Value| v.as_object().cloned().unwrap_or_default();
        MediaRules { download: map(download), priority: map(priority), show: map(show), keep: map(keep) }
    }

    #[test]
    fn media_rules_default_to_everything_but_kept_video() {
        let r = MediaRules::default();
        assert!(r.downloads("igdb", "box") && r.downloads("steamgriddb", "cover"));
        assert!(!r.downloads("igdb", "screenshot"), "list types are opt-in");
        assert!(r.shows("video"));
        assert!(r.keeps("box"));
        assert!(!r.keeps("video"), "a client streams video unless told to keep it");
        assert!(!r.keeps("bgm"), "and audio");
        assert_eq!(r.outranking("igdb", "box"), vec![MANUAL_SOURCE], "your uploads first when nothing is set");
    }

    #[test]
    fn media_rules_gate_and_rank() {
        let r = rules(
            serde_json::json!({"igdb": {"box": false, "screenshot": true}}),
            serde_json::json!({"box": ["screenscraper", "igdb"], "screenshot": ["igdb"]}),
            serde_json::json!({"video": false}),
            serde_json::json!({"video": true, "box": false}),
        );
        assert!(!r.downloads("igdb", "box"));
        assert!(r.downloads("igdb", "screenshot"));
        assert!(r.downloads("mobygames", "box"));
        // Uploads not placed in the order come first.
        assert_eq!(r.outranking("igdb", "box"), vec![MANUAL_SOURCE, "screenscraper"]);
        assert_eq!(r.outranking("screenscraper", "box"), vec![MANUAL_SOURCE]);
        // An unlisted provider is below every listed one.
        assert_eq!(r.outranking("mobygames", "box"), vec![MANUAL_SOURCE, "screenscraper", "igdb"]);
        // Screenshots come from everyone, whatever an order says.
        assert!(r.outranking("mobygames", "screenshot").is_empty());
        assert!(r.rank("screenscraper", "box") < r.rank("igdb", "box"));
        assert_eq!(r.rank("mobygames", "box"), usize::MAX);
        assert_eq!(r.rank(MANUAL_SOURCE, "box"), 0);
        // Placed lower, uploads give way to the providers above them.
        let placed = rules(serde_json::json!({}), serde_json::json!({"box": ["screenscraper", "manual", "igdb"]}), serde_json::json!({}), serde_json::json!({}));
        assert!(placed.outranking("screenscraper", "box").is_empty());
        assert_eq!(placed.outranking("igdb", "box"), vec!["screenscraper", MANUAL_SOURCE]);
        assert!(placed.rank("screenscraper", "box") < placed.rank(MANUAL_SOURCE, "box"));
        assert!(!r.shows("video") && r.shows("box"));
        assert!(r.keeps("video") && !r.keeps("box"));
    }

    #[test]
    fn playmatch_batch_semantics() {
        assert_eq!(playmatch_batch_limit(None), Some(100), "unset = PlayMatch's bulk maximum");
        assert_eq!(playmatch_batch_limit(Some(250)), Some(250));
        assert_eq!(playmatch_batch_limit(Some(0)), Some(0), "0 = unlimited (still 100 per request)");
        assert_eq!(playmatch_batch_limit(Some(-1)), None, "-1 = off");
    }
}

#[cfg(test)]
mod scan_setting_tests {
    use super::*;
    use crate::rom::hash::HashFlags;

    /// The mapping `scan_hashes` applies, without a database. The defaults matter: sha256 is
    /// the one that costs a whole extra pass and buys nothing `sha1` doesn't already give
    /// (`import.rs` dedups sha256 > sha1 > md5), and xxh3_128 is the cheapest of the set and
    /// was previously never computed at all.
    fn flags_from(json: serde_json::Value) -> (HashFlags, bool) {
        let on = |key: &str, default: bool| json.get(key).and_then(|b| b.as_bool()).unwrap_or(default);
        let mut flags = HashFlags::empty();
        for (key, bit, default) in [
            ("crc32", HashFlags::CRC32, true),
            ("xxh3_128", HashFlags::XXH3_128, true),
            ("md5", HashFlags::MD5, true),
            ("sha1", HashFlags::SHA1, true),
            ("sha256", HashFlags::SHA256, false),
        ] {
            if on(key, default) {
                flags.insert(bit);
            }
        }
        (flags, on("ra_hash", true))
    }

    #[test]
    fn defaults_are_everything_but_sha256() {
        let (flags, ra) = flags_from(serde_json::Value::Null);
        assert!(flags.contains(HashFlags::CRC32 | HashFlags::XXH3_128 | HashFlags::MD5 | HashFlags::SHA1));
        assert!(!flags.contains(HashFlags::SHA256), "sha256 is a whole extra pass for nothing sha1 doesn't cover");
        assert!(ra, "ra_hash is wanted by default, even though it is skipped where no console id resolves");
    }

    #[test]
    fn a_partial_setting_leaves_the_rest_at_their_defaults() {
        // Adding an algorithm later must not silently disable it for anyone who saved a
        // setting before it existed.
        let (flags, _) = flags_from(serde_json::json!({ "sha256": true }));
        assert!(flags.contains(HashFlags::SHA256));
        assert!(flags.contains(HashFlags::CRC32 | HashFlags::MD5 | HashFlags::SHA1 | HashFlags::XXH3_128));
    }

    #[test]
    fn each_algorithm_can_be_turned_off() {
        let (flags, ra) = flags_from(serde_json::json!({
            "crc32": false, "xxh3_128": false, "md5": false, "sha1": true, "sha256": false, "ra_hash": false
        }));
        assert_eq!(flags, HashFlags::SHA1);
        assert!(!ra);
    }
}
