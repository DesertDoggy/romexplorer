//! The read side of the collection for list/detail screens: titles, releases, owned games,
//! files -- each list paged, sortable on an allowlisted key and filterable by a substring
//! -- plus hash lookup and media files. Every list joins the catalog against the machine's
//! own `local_*` ownership tables so a row always says whether it's held locally, and
//! the `view` parameter picks local / server / collection / collection-local-first.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::mysql::{MySqlPool, MySqlRow};
use sqlx::Row;

use crate::common::models::{HashAlgorithms, Platform};
use crate::database::lookup;
use crate::database::release_buckets;
use crate::database::settings as db_settings;

use super::{clamp_page, like_pattern, App, Error, Result};

// ---------------------------------------------------------------------------
// shared shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct PlatformInfo {
    pub id: u16,
    pub name: String,
    /// `Platform::japanese_name`, for the `platform_name_language` setting.
    pub name_ja: String,
    pub family: Option<String>,
    /// `console` | `other` | `handheld` | `arcade` | `computer` (`connection::platform_group`).
    pub group: &'static str,
    pub is_disc_based: bool,
}

/// `view` values: `local` (held here), `server` (held on the main server -- the `remote_*`
/// mirror, so empty until `CatalogSync` has run), `missing_on_server` (the main server
/// hasn't got it but this machine or a peer has: the backlog still to copy up -- a copy on
/// a peer is not a backup, so it neither satisfies the main server nor excuses its absence),
/// `collection` (everything, sorted as asked), `collection_local_first` (everything,
/// held-locally rows first; the default), and the RetroAchievements views:
/// `local_ra_local_first` / `local_ra_ra_first` (what is held here plus what RA supports,
/// in that or the other order) and `ra_only` (only what RA supports). These used to be a
/// filter on the RetroAchievements tab, where every row is an RA game already and they
/// could not narrow anything; here they separate RA-supported games from the rest of the
/// catalogue.
pub const VIEWS: [&str; 8] = ["local", "server", "missing_on_server", "collection", "collection_local_first", "local_ra_local_first", "local_ra_ra_first", "ra_only"];

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListQuery {
    /// Substring on the row's name/path.
    pub search: Option<String>,
    /// Which releases count (titles / releases): a comma list of [`KINDS`], any of which may
    /// match -- `commercial,translation` is commercial releases and translations -- and
    /// `guessed` for releases whose language is a guess. The filter's older names are still
    /// read: `all` (the default), `patches` (everything but RetroAchievements subsets) and
    /// `commercial`.
    pub kinds: Option<String>,
    /// What the releases are (titles / releases): absent or `game` for software only -- the
    /// default, so music discs, films and scans are not listed among the games -- one of
    /// `platform_map::MATERIALS` for that kind alone, `multimedia` for audio, video and image
    /// discs together, `etc` for what a community makes around a console (map packs, tools,
    /// trainers, saves, DLC installers), `other` for all of those, `unbundled`
    /// for the DLC, updates, themes and avatars of titles with no game behind them (the ones left to merge by
    /// hand), `all` for everything. A title's or a set's own releases are all listed unless
    /// one is asked for.
    pub material: Option<String>,
    /// Titles only: the titles linked to this IGDB game -- the same game on each platform it
    /// is on, which is how a row of the cross-platform list opens.
    pub igdb: Option<u64>,
    /// Cross-platform list only: just the games that are more than one title.
    #[serde(default)]
    pub multi: bool,
    /// Internal (a release page's own query, never a request's): the DLC and updates of the
    /// title for a game release on this platform -- see [`ReleaseDetail::addons`].
    #[serde(skip)]
    pub addons_for: Option<u16>,
    /// Titles only: a title, its base game and everything sharing that base game (ports,
    /// remakes, DLC, ...). Given a port, its base game is the anchor.
    pub basegame: Option<u64>,
    pub platform: Option<u16>,
    /// Several platforms, comma-separated: any of them (or, with `platform_logic` and on the
    /// cross-platform list only, all of them). Together with `platform`.
    pub platforms: Option<String>,
    /// and | or (default). Cross-platform only: a game on every chosen platform.
    pub platform_logic: Option<String>,
    /// Titles only, comma-separated platform ids: titles with a release on one of them come
    /// first, without leaving the others out (a picker moving releases of that platform).
    pub prefer_platforms: Option<String>,
    /// Titles only, comma-separated release ids: their platforms are preferred too, or with
    /// `near_only` the only ones listed (the picker moving these releases to another title).
    pub near_releases: Option<String>,
    #[serde(default)]
    pub near_only: bool,
    /// DAT source name (releases only).
    pub source: Option<String>,
    /// Metadata provider name a title must be linked to (titles only): igdb, mobygames,
    /// retroachievements, ...
    pub provider: Option<String>,
    /// Narrow releases to one title.
    pub title_id: Option<u64>,
    /// Narrow releases to one collection set (ordered by set position by default).
    pub set_id: Option<u64>,
    /// Narrow files to one configured directory.
    pub directory_id: Option<u64>,
    /// See [`VIEWS`].
    pub view: Option<String>,
    /// Definitive tab: which version leads a row, `definitive` (default) or `base`.
    pub lead: Option<String>,
    /// Definitive tab: only games with a version note of this rank or above (`S`, `A`, `B`).
    pub noted: Option<String>,
    /// Major genre name (one of `ra_genre::MAJOR_GENRES`), or several comma-separated: the
    /// title has a genre that is it or sits under it (`genre_major_logic`: or by default).
    pub genre_major: Option<String>,
    pub genre_major_logic: Option<String>,
    /// Comma-separated raw genre names; `genre_logic` and (default) / or.
    pub genres: Option<String>,
    pub genre_logic: Option<String>,
    pub franchises: Option<String>,
    pub franchise_logic: Option<String>,
    /// Company (developer/publisher/...) names.
    pub companies: Option<String>,
    pub company_logic: Option<String>,
    pub keywords: Option<String>,
    pub keyword_logic: Option<String>,
    /// Keyword *tags*: the user's and the auto-tagger's (`title_keywords.source` user / auto),
    /// not providers' keywords. Comma-separated names; `tag_logic` and (default) / or.
    pub tags: Option<String>,
    pub tag_logic: Option<String>,
    /// user | auto: only tags put there that way (alone: titles carrying any such tag).
    pub tag_source: Option<String>,
    /// One region code / language code a release must carry (titles: any of its releases).
    pub region: Option<String>,
    pub language: Option<String>,
    #[serde(default)]
    pub include_deleted: bool,
    /// Games / roms: complete (every file present) | missing (some file gone).
    pub status: Option<String>,
    /// Display (`api::display`), as each release's value (recorded, else its system's
    /// default) -- titles: any of its game releases. Aspect, widescreen and refresh match
    /// exactly; `display_res` is one resolution in its list (`256x224p`); `display_scan`
    /// `p` / `i`: it has a progressive / interlaced one; `display_state` `field:default` or
    /// `field:confirmed` (`aspect:default`: the 4:3 / 8:7 question still open).
    pub display_aspect: Option<String>,
    pub display_widescreen: Option<String>,
    pub display_refresh: Option<String>,
    pub display_res: Option<String>,
    pub display_scan: Option<String>,
    pub display_state: Option<String>,
    /// Titles / releases: leave out minor versions -- a title a fuller edition covers on its
    /// platforms and regions, a release a fuller release of its title covers
    /// (`database::versions`).
    #[serde(default)]
    pub hide_minor: bool,
    /// Titles: one version of each version set (`database::versions`): the held one, the
    /// user's pick, else a stable random one.
    #[serde(default)]
    pub one_per_group: bool,
    pub sort: Option<String>,
    #[serde(default)]
    pub desc: bool,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct TitleRow {
    pub id: u64,
    pub name: String,
    pub category: String,
    /// What the title is when none of its releases is the game as it was sold
    /// ([`TITLE_KIND_SQL`]): shown in place of `category`, which says "base game" of
    /// every title IGDB has not said otherwise about -- a title of nothing but hacks too.
    pub game_kind: Option<String>,
    pub basegame_title_id: Option<u64>,
    pub release_count: u64,
    pub platforms: Option<String>,
    /// Comma-separated provider names this title (or one of its releases) is linked to.
    pub providers: Option<String>,
    pub local_games: u64,
    /// Held on the main server (`remote_sources.role = 'main'`): 0 until `CatalogSync` runs,
    /// `None` when there is no main server at all (standalone) -- then nothing is "not on
    /// the server", and the UIs show plain local/not-local instead.
    pub remote_games: Option<u64>,
    /// Held on some non-main source (a peer / client share). Informational: a peer never
    /// changes whether a locally-held row counts as missing from the server.
    pub peer_games: u64,
    pub cover_media_id: Option<u64>,
    /// The name to show: the top-priority release's (region / language priority) --
    /// romanized DAT name, or its localized name per `title_display_mode`.
    pub display_name: String,
    #[serde(skip)]
    pub top_release_id: Option<u64>,
    /// Which of the title's rows this is (`release_buckets::row_key_sql`): its bucket --
    /// `retail`, `demo`, `translation`, ... -- or, for a bucket whose releases are each a
    /// row of their own, the bucket and the release (`hack:1234`). A title is listed once per
    /// row; adding a row to a set or copying it takes only that row's releases.
    pub row_key: String,
    /// The row as one number, unique in the list (`release_buckets::row_id`): what the UIs
    /// select rows by. `title_row_ids` in a set or copy request takes it back.
    pub row_id: u64,
    /// The row's bucket (`release_buckets::BUCKETS`): the badge the lists show. Retail has none.
    pub bucket: String,
    /// The languages of the row's releases, for a translation row's badge ("en, zh").
    pub row_languages: Option<String>,
    /// Some release of it is RetroAchievements-supported (`ra_exists!`).
    #[serde(default)]
    pub ra: bool,
    /// Its local files on disk: of all its releases, and of its first release by region
    /// priority (`local_size_sql`).
    pub local_size_all: Option<u64>,
    pub local_size_top: Option<u64>,
    /// The same by the DATs, uncompressed (`catalog_size_sql`).
    pub catalog_size_all: Option<u64>,
    pub catalog_size_top: Option<u64>,
    /// Its first release's display (`api::display::effective_sql`).
    pub display_aspect: Option<String>,
    pub display_widescreen: Option<String>,
    pub display_modes: Option<String>,
    pub display_refresh: Option<String>,
    /// With `one_per_group`: the version set the row stands for, and how many of its
    /// versions are not listed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_group: Option<VersionGroupBadge>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VersionGroupBadge {
    pub key: String,
    pub hidden: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseRow {
    pub id: u64,
    pub name: String,
    pub platform_id: u16,
    pub platform: Option<String>,
    pub region: Option<String>,
    pub languages: Option<String>,
    pub year: Option<u16>,
    pub revision: Option<String>,
    pub serial: Option<String>,
    pub category: Option<String>,
    /// What the release is when it is not the game itself (`platform_map::MATERIALS`: dlc,
    /// update, audio, video, ...). A title page lists each kind in a section of its own.
    pub material: Option<String>,
    /// What the game is when it is not a commercial release (`release.game_kind`: hack, mod,
    /// translation, homebrew, arcade, wiiware): the badge the lists show.
    pub game_kind: Option<String>,
    /// `guessed` when the language of a patched release was inferred and not stated; the
    /// lists mark it. `confirmed` / `manual` otherwise, absent on ordinary releases.
    pub languages_state: Option<String>,
    /// How its link to the game it is made from is known (`release_bases.status`):
    /// `suggested` is marked in the lists until the user confirms or unlinks it.
    pub base_status: Option<String>,
    pub disc_count: u16,
    pub source: Option<String>,
    pub title_id: Option<u64>,
    pub title_name: Option<String>,
    pub local_games: u64,
    /// Held on the main server (`remote_sources.role = 'main'`): 0 until `CatalogSync` runs,
    /// `None` when there is no main server at all (standalone) -- then nothing is "not on
    /// the server", and the UIs show plain local/not-local instead.
    pub remote_games: Option<u64>,
    /// Held on some non-main source (a peer / client share). Informational only.
    pub peer_games: u64,
    pub content_count: u64,
    pub cover_media_id: Option<u64>,
    /// Local files of this release whose content is confirmed by hash: the release's own DAT
    /// lines (the file itself, or an archive member: a CHD's tracks), or a Hasheous hit.
    /// Hasheous alone left every DAT-matched release it doesn't know (most WiiWare, for one)
    /// without the badge, however complete the dump.
    pub local_verified: u64,
    /// Local files of this release no longer found on disk.
    pub local_missing: u64,
    /// Position within the set, when listing a collection set.
    pub set_position: Option<u32>,
    /// What put it in the set: release, title (its best release), game (any platform) or
    /// family (a definitive-version entry: `copy_definitive`).
    pub set_via: Option<String>,
    /// The title or game entry it came from, so adding it to another set keeps its kind.
    pub set_title_id: Option<u64>,
    /// The title entry's row (`retail`, `demo`, `hack:1234`): which of the title's kinds.
    pub set_row_key: Option<String>,
    pub set_igdb_id: Option<String>,
    /// The family entry's title (a definitive-version entry), so another set gets it as one.
    pub set_family_title_id: Option<u64>,
    /// Commercial editions (`name_parse::Edition`): a budget reissue ("PlayStation 2 the
    /// Best"), a special / limited / first-print edition, an aftermarket release.
    pub is_budget: bool,
    pub is_special_edition: bool,
    pub is_aftermarket: bool,
    /// A re-release on later hardware or a service: Virtual Console, a collection, ...
    pub is_rerelease: bool,
    /// What its local copies take on disk, every file once at its real size (`local_size_sql`).
    pub local_size: Option<u64>,
    /// Its size by the DATs: the files uncompressed, one dump (`catalog_size_sql`).
    pub catalog_size: Option<u64>,
    /// Never for a hack, a translation or something made for the game (a trainer, a patch):
    /// those sit under the original's title, whose localized name is the original's, and
    /// would all be listed under that name instead of their own.
    /// The title's localized name for this release: a `title_locales` name for one of the
    /// release's regions, else for its only language, else (an English-region or English-
    /// language release) the title's English name -- ほげほげ伝説V for "Shin Legend of Hogehoge V
    /// (Japan)", "Legend of Hogehoge" for "Legend of Hogehoge (Europe)", "Hoge Chronicle" for the European one.
    pub local_title: Option<String>,
    /// The localized name, else the DAT name without region / language / revision tags --
    /// what the lists show as the name when localized titles are on (`name` under it).
    pub display_name: String,
    /// RetroAchievements-supported (`ra_exists!`).
    #[serde(default)]
    pub ra: bool,
    /// Its display (`api::display::effective_sql`): recorded, else its system's default.
    pub display_aspect: Option<String>,
    pub display_widescreen: Option<String>,
    pub display_modes: Option<String>,
    pub display_refresh: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GameRow {
    pub id: u64,
    /// As scanned (kept as is).
    pub name: String,
    /// As the lists show it: the release's name (else the scanned one), without "(Disc N)"
    /// when the game holds several discs.
    pub display_name: String,
    pub region: Option<String>,
    pub languages: Option<String>,
    pub category: Option<String>,
    pub playlist_file_id: Option<u64>,
    pub release_id: Option<u64>,
    pub release_name: Option<String>,
    pub title_id: Option<u64>,
    pub title_name: Option<String>,
    pub platform: Option<String>,
    /// A release it is a copy of is RetroAchievements-supported.
    #[serde(default)]
    pub ra: bool,
    pub rom_count: u64,
    pub file_count: u64,
    pub cover_media_id: Option<u64>,
    /// Linked to a release from a DAT (not one built from a Hasheous hit).
    pub dat_verified: bool,
    /// Files whose content Hasheous confirmed.
    pub hasheous_verified: u64,
    /// Files no longer found on disk.
    pub missing_files: u64,
    /// The release's DAT files this game must hold (see `coverage`), and how many it does
    /// (directly, or inside an archive: a CHD's tracks).
    pub contents_required: u64,
    pub contents_present: u64,
    /// Every file present and every required DAT file held (None: no release to judge by).
    pub complete: Option<bool>,
    /// Incomplete, but together with the other incomplete local games of its release the
    /// release's dump would be complete (their files belong in one game).
    pub merge_hint: bool,
    /// Missing files whose bytes another live local file holds.
    pub missing_found: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileRow {
    pub id: u64,
    pub path: String,
    pub size: u64,
    pub archive_format: Option<String>,
    pub directory_id: Option<u64>,
    pub content_id: u64,
    pub deleted: bool,
    pub last_modified: Option<NaiveDateTime>,
    pub last_full_check: Option<NaiveDateTime>,
    /// When the bytes last matched the hash the catalogue records for them.
    pub last_integrity_check: Option<NaiveDateTime>,
    /// When they last did *not*: set by a mismatch, cleared by the next pass. Missing,
    /// unreadable and nothing-to-compare-against are not failures.
    pub last_integrity_failure: Option<NaiveDateTime>,
    pub platform_id: Option<u16>,
    pub platform: Option<String>,
    pub hashes: BTreeMap<String, String>,
    /// Archive members recorded for this file's content (`content_contents` children).
    pub member_count: u64,
    /// A missing file whose bytes another live local file holds: that file's id.
    pub found_elsewhere: Option<u64>,
    /// For a `.cue` / `.gdi` / `.m3u`, what the last check made of it. `None` means it has not
    /// been checked, which is not the same as being fine -- the badge says so.
    pub sheet_check: Option<romexplorer_types::sheets::SheetVerdictView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContentRow {
    pub content_id: u64,
    pub file_name: Option<String>,
    pub size: Option<u64>,
    pub platform_id: u16,
    pub platform: Option<String>,
    pub media_type: Option<String>,
    pub partial: bool,
    pub hashes: BTreeMap<String, String>,
    pub member_count: u64,
    /// Listed as an archive's member: its path inside the archive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_path: Option<String>,
    /// In a member or content list: the name DATs give these bytes, when one does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_name: Option<String>,
    /// On a release page: which DAT source's file list (dump) this file is from -- a
    /// release listed by several DATs (redump cue/bin, mame-redump CHD, ...) has one per source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dump_source: Option<String>,
    /// On a release page: every DAT source that lists this file for the release, comma
    /// separated (`dump_source` is only the one whose file list it was first seen in).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dump_sources: Option<String>,
    /// Which build of a patched work this file is (`release_contents.variant`): the versions
    /// of one patch are one release, and any one of them is the release held.
    pub variant: Option<String>,
    /// On a release page: a local file with these bytes exists -- which of a patched work's
    /// versions is the one held.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    /// In a release's superseded files: when a DAT took it off, and which DAT (name and
    /// version).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_at: Option<chrono::NaiveDateTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Tagged {
    pub name: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompanyTag {
    pub name: String,
    pub role: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Locale {
    pub language: String,
    pub region: String,
    pub source: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AltName {
    pub name: String,
    pub source: String,
    /// The provider's note ("Japanese title", "Acronym", "Alternative spelling", ...).
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderLink {
    pub provider: String,
    pub provider_id: String,
    pub confidence: String,
    pub linked_at: NaiveDateTime,
    /// The provider's own page for this entry, when one can be built.
    pub url: Option<String>,
    /// Which tier the link hangs off: `title` or `release` (with that release's id).
    pub via: String,
    pub release_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaRow {
    pub id: u64,
    pub media_type: String,
    pub sub_category: Option<String>,
    pub path: String,
    pub source: Option<String>,
    pub source_url: Option<String>,
    /// Where it comes from when it is not the page's own: `title` (a release showing its
    /// title's) or `crossplatform` (the game's, by provider id). See [`inherit`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TitleDetail {
    pub id: u64,
    pub name: String,
    /// As the lists show it (see `TitleRow::display_name`).
    pub display_name: String,
    /// The best-priority-region release's grid art (see `title_cover_sql`).
    pub cover_media_id: Option<u64>,
    pub norm_name: String,
    pub category: String,
    /// See [`TitleRow::game_kind`].
    pub game_kind: Option<String>,
    pub basegame_title_id: Option<u64>,
    pub basegame_name: Option<String>,
    /// The platforms the base game's title is on: a game is often on several, and the name
    /// alone does not say which one this points at.
    pub basegame_platforms: Option<String>,
    /// The other direction: the titles whose base game this is -- its ports, remakes,
    /// remasters, DLC and mods, as IGDB categorises each.
    pub derived: Vec<FamilyRef>,
    /// The base game and category were set by the user (`title_family::set_basegame`).
    pub basegame_locked: bool,
    /// The other titles with the same base game: a port's fellow ports and remakes.
    pub siblings: Vec<FamilyRef>,
    /// A collection: the games it holds. `bundle_source`: igdb | manual.
    pub bundle_games: Vec<FamilyRef>,
    /// The collections that hold this game.
    pub in_bundles: Vec<FamilyRef>,
    /// The head of the family this title is in (itself for a family of one): the Definitive
    /// tab's family page.
    pub family_root_id: u64,
    pub genres: Vec<Tagged>,
    pub franchises: Vec<Tagged>,
    pub keywords: Vec<Tagged>,
    /// Player perspective (IGDB/MobyGames), stored but not shown in the console yet.
    pub perspectives: Vec<Tagged>,
    pub companies: Vec<CompanyTag>,
    pub locales: Vec<Locale>,
    /// The user set the localized names (source `user`); IGDB re-apply leaves them alone.
    pub locales_locked: bool,
    /// Every alternative name a provider gives the title.
    pub alt_names: Vec<AltName>,
    /// How its top release is meant to be shown (`api::display`).
    pub display: Vec<super::display::DisplayValue>,
    pub provider_links: Vec<ProviderLink>,
    pub media: Vec<MediaRow>,
    /// In rank order: the user's manual order first (`manual_order`), then the default.
    /// The first 500: a game can have thirty thousand hacks, which a page lists a page at
    /// a time from the releases list (`title_id`, `kinds`) instead.
    pub releases: Vec<ReleaseRow>,
    /// How many releases the title has in all.
    pub releases_total: u64,
    /// Made from this game but under a title of their own (a hack IGDB lists as a game):
    /// listed with the title's own patched ROMs.
    pub linked: Vec<super::bases::PatchedRef>,
    pub local_games: u64,
    /// The user dragged this title's releases into an order of their own.
    pub manual_order: bool,
    /// Some release of it is RetroAchievements-supported.
    pub ra: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RomDetail {
    pub id: u64,
    pub disc_number: Option<u16>,
    pub main_path: String,
    pub platform_id: u16,
    pub platform: Option<String>,
    pub alt: bool,
    pub deleted: bool,
    pub files: Vec<FileRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalGameDetail {
    pub id: u64,
    pub name: String,
    pub region: Option<String>,
    pub category: Option<String>,
    pub playlist: Option<FileRow>,
    pub roms: Vec<RomDetail>,
    /// Every file present and every DAT file of the release held (see `GameRow::complete`).
    pub complete: Option<bool>,
    pub missing_files: u64,
    pub contents_required: u64,
    pub contents_present: u64,
    /// The configured directory its files are in.
    pub directory_id: Option<u64>,
    pub directory_path: Option<String>,
    /// Its place in the user's order of this release's copies, if they set one.
    pub manual_position: Option<i64>,
    /// Linked by a serial rather than by its files' hashes (`database::serial_match`): where
    /// the serial came from (header | manifest | file_name), and the serial.
    pub serial_match: Option<String>,
    pub serial_matched: Option<String>,
}

/// A RetroAchievements game a release is supported by.
#[derive(Debug, Clone, Serialize)]
pub struct RaGameRef {
    pub id: String,
    pub title: Option<String>,
    pub achievements: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseDetail {
    #[serde(flatten)]
    pub release: ReleaseRow,
    pub released_on: Option<chrono::NaiveDate>,
    pub min_players: Option<u16>,
    pub max_players: Option<u16>,
    pub is_beta: bool,
    pub is_prototype: bool,
    pub is_demo: bool,
    pub is_unlicensed: bool,
    pub community_patched: bool,
    pub patch_type: Option<String>,
    pub contents: Vec<ContentRow>,
    /// Files a newer DAT changed or no longer lists (`release_contents_superseded`): shown,
    /// not counted. A later import that lists one again puts it back in `contents`.
    pub superseded: Vec<ContentRow>,
    /// How it is meant to be shown: aspect, widescreen, video modes, refresh -- each recorded
    /// by a source or the system's default (`api::display`).
    pub display: Vec<super::display::DisplayValue>,
    pub games: Vec<LocalGameDetail>,
    /// The RetroAchievements games this release is supported by (`ra_exists!`'s rule).
    pub ra_games: Vec<RaGameRef>,
    pub provider_serials: Vec<Tagged>,
    pub ratings: Vec<Tagged>,
    pub provider_links: Vec<ProviderLink>,
    pub media: Vec<MediaRow>,
    /// Sets this release belongs to.
    pub sets: Vec<SetRef>,
    /// The DLC and updates of its title on this release's platform: the ones for its own
    /// region first (most are regional -- 78% of DLC and 93% of updates on a full import),
    /// then the rest, each part in the lists' usual order. None is left out: which region's
    /// update a dump takes is the user's call. Empty for a release that is itself DLC or an
    /// update.
    pub addons: Vec<ReleaseRow>,
    /// What this release is made from (a hack, a translation, a trainer, ...), and how that
    /// is known. `None` for an original, and for a release the user unlinked.
    pub base: Option<super::bases::BaseLink>,
    /// The releases made from this one: its translations and hacks, trainers and patches.
    pub patched: Vec<super::bases::PatchedRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetRef {
    pub id: u64,
    pub name: String,
    pub position: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileDetail {
    #[serde(flatten)]
    pub file: FileRow,
    pub content: Option<ContentRow>,
    pub members: Vec<ContentRow>,
    pub owners: Vec<FileOwner>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileOwner {
    pub rom_id: u64,
    pub main_path: String,
    pub game_id: Option<u64>,
    pub game_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HashMatchRow {
    pub algorithm: String,
    pub content_id: u64,
    pub file_name: Option<String>,
    pub size: Option<u64>,
    pub platform_id: u16,
    pub platform: String,
    pub source: Option<String>,
    pub release_id: Option<u64>,
    pub release_name: Option<String>,
    pub region: Option<String>,
    pub title_id: Option<u64>,
    pub title_name: Option<String>,
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Platforms a list's platform filter treats as one system: the same machine under its
/// regional names (Famicom / NES, Super Famicom / SNES), and PlayStation with the PS1
/// classics repackaged for PSP, which are PlayStation games. Picking any member filters for
/// all of them, and the filter pulldowns offer each group once (`platform_groups` in the
/// vocabulary).
///
/// Only the filter. A release keeps its own platform -- its badge, its DAT, its emulator --
/// and nothing here merges titles. Add-ons and later generations with libraries of their own
/// (Disk System, Sufami Turbo, PC Engine CD, MSX2) stay separate systems.
pub const PLATFORM_FILTER_GROUPS: [&[Platform]; 3] =
    [&[Platform::Nes, Platform::Famicom], &[Platform::Snes, Platform::SuperFamicom], &[Platform::PlayStation, Platform::PspPs1Classic]];

/// Every platform id a filter on `id` matches: its group's, or just itself.
pub fn platform_filter_ids(id: u16) -> Vec<u16> {
    PLATFORM_FILTER_GROUPS.iter().find(|group| group.iter().any(|p| *p as u16 == id)).map_or_else(|| vec![id], |group| group.iter().map(|p| *p as u16).collect())
}

/// ` AND <column> IN (...)` for a list's platform filter. The ids are this module's own
/// integers, so they are written into the SQL rather than bound.
fn platform_clause(column: &str, id: u16) -> String {
    format!(" AND {column} IN ({})", platform_filter_ids(id).iter().map(u16::to_string).collect::<Vec<_>>().join(","))
}

/// The platforms a list is filtered by (`platform` and `platforms`), each with the platforms
/// shown as one with it (NES and Famicom). Empty: no filter.
pub(crate) fn chosen_platforms(q: &ListQuery) -> Vec<Vec<u16>> {
    let mut ids: Vec<u16> = q.platform.into_iter().collect();
    ids.extend(q.platforms.as_deref().unwrap_or("").split(',').filter_map(|p| p.trim().parse::<u16>().ok()));
    ids.dedup();
    let mut out: Vec<Vec<u16>> = Vec::new();
    for id in ids {
        let group = platform_filter_ids(id);
        if !out.contains(&group) {
            out.push(group);
        }
    }
    out
}

/// The platform filter as ` AND <column> IN (...)`: any of the chosen platforms.
fn platforms_clause(column: &str, q: &ListQuery) -> String {
    let all: Vec<String> = chosen_platforms(q).into_iter().flatten().map(|p| p.to_string()).collect();
    if all.is_empty() {
        return String::new();
    }
    format!(" AND {column} IN ({})", all.join(","))
}

pub(crate) enum Bind {
    Str(String),
    U16(u16),
    U64(u64),
}

pub(crate) fn apply<'q>(mut q: sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments>, binds: &'q [Bind]) -> sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments> {
    for b in binds {
        q = match b {
            Bind::Str(s) => q.bind(s),
            Bind::U16(n) => q.bind(*n),
            Bind::U64(n) => q.bind(*n),
        };
    }
    q
}

/// How to page a big grouped list cheaply: `key_select` is the row key (first column) plus
/// every aggregate alias `body`'s HAVING and the ORDER BY refer to -- nothing with a
/// per-row subquery; `key_col` is the key's column (`t.id`); `select_binds` how many of the
/// binds belong to the full `select`'s own placeholders.
struct Lite<'a> {
    key_select: &'a str,
    key_col: &'a str,
    select_binds: usize,
    /// A second key column, by its alias in both selects, for a list whose rows are not one
    /// per `key_col` (the Titles list: a title once per bucket, `row_key`). The page is then
    /// narrowed by `key_col` and the rows not on it are dropped.
    sub_key: Option<&'a str>,
}

/// Runs `SELECT {select} {body}` paged, plus the total. `body` carries FROM/WHERE/GROUP BY/
/// HAVING; `order` is already validated against an allowlist by the caller. Binds are in
/// SQL order: `select`'s own placeholders first, then `body`'s.
///
/// With `lite`, the count and the page's keys come from `key_select` alone, and the full
/// select (cover art, providers, verification counts -- correlated subqueries) runs only
/// for the page's rows. MariaDB otherwise evaluates those for every group before it sorts
/// and limits: about a minute per page on a 500k-release catalog.
async fn paged<T>(pool: &MySqlPool, select: &str, lite: Option<Lite<'_>>, body: &str, order: &str, binds: Vec<Bind>, limit: u32, offset: u32, map: impl Fn(&MySqlRow) -> Result<T>) -> Result<Page<T>> {
    let Some(lite) = lite else {
        let count_sql = format!("SELECT COUNT(*) AS n FROM (SELECT {select} {body}) x");
        let total: i64 = apply(sqlx::query(sqlx::AssertSqlSafe(count_sql)), &binds).fetch_one(pool).await?.try_get("n")?;
        let sql = format!("SELECT {select} {body} ORDER BY {order} LIMIT ? OFFSET ?");
        let rows = apply(sqlx::query(sqlx::AssertSqlSafe(sql)), &binds).bind(limit).bind(offset).fetch_all(pool).await?;
        let items = rows.iter().map(&map).collect::<Result<Vec<_>>>()?;
        return Ok(Page { items, total: total as u64, limit, offset });
    };
    let body_binds = &binds[lite.select_binds.min(binds.len())..];
    // Count and page keys run side by side (two pool connections).
    let count_sql = format!("SELECT COUNT(*) AS n FROM (SELECT {} {body}) x", lite.key_select);
    let keys_sql = format!("SELECT {} {body} ORDER BY {order} LIMIT ? OFFSET ?", lite.key_select);
    let count_query = apply(sqlx::query(sqlx::AssertSqlSafe(count_sql)), body_binds).fetch_one(pool);
    let keys_query = apply(sqlx::query(sqlx::AssertSqlSafe(keys_sql)), body_binds).bind(limit).bind(offset).fetch_all(pool);
    let (count_row, key_rows) = tokio::try_join!(count_query, keys_query)?;
    let total: i64 = count_row.try_get("n")?;
    let keys: Vec<u64> = key_rows.iter().map(|r| r.try_get::<u64, _>(0)).collect::<std::result::Result<_, _>>()?;
    let sub_keys: Option<HashSet<(u64, String)>> = match lite.sub_key {
        Some(col) => Some(key_rows.iter().map(|r| Ok((r.try_get::<u64, _>(0)?, r.try_get::<String, _>(col)?))).collect::<std::result::Result<_, sqlx::Error>>()?),
        None => None,
    };
    let mut keys = keys;
    keys.sort_unstable();
    keys.dedup();
    if keys.is_empty() {
        return Ok(Page { items: Vec::new(), total: total as u64, limit, offset });
    }
    // The page's keys go into the top-level WHERE (before the last GROUP BY; the keys are
    // integers from the query above, safe to inline).
    let at = body.rfind(" GROUP BY ").ok_or_else(|| Error::internal("paged list body has no GROUP BY"))?;
    let ids = keys.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let narrowed = format!("{} AND {} IN ({ids}){}", &body[..at], lite.key_col, &body[at..]);
    let sql = format!("SELECT {select} {narrowed} ORDER BY {order}");
    let rows = apply(sqlx::query(sqlx::AssertSqlSafe(sql)), &binds).fetch_all(pool).await?;
    let on_page = |r: &&MySqlRow| match (&sub_keys, lite.sub_key) {
        (Some(set), Some(col)) => match (r.try_get::<u64, _>(0), r.try_get::<String, _>(col)) {
            (Ok(id), Ok(key)) => set.contains(&(id, key)),
            _ => false,
        },
        _ => true,
    };
    let items = rows.iter().filter(on_page).map(&map).collect::<Result<Vec<_>>>()?;
    Ok(Page { items, total: total as u64, limit, offset })
}

pub(crate) fn order_clause(sort: Option<&str>, desc: bool, allowed: &[(&str, &str)], default: &str) -> Result<String> {
    let dir = if desc { "DESC" } else { "ASC" };
    match sort.filter(|s| !s.is_empty()) {
        None => Ok(format!("{default} {dir}")),
        Some(key) => match allowed.iter().find(|(k, _)| *k == key) {
            Some((_, expr)) => Ok(format!("{expr} {dir}, {default} ASC")),
            None => Err(Error::invalid(format!("sort must be one of {}", allowed.iter().map(|(k, _)| *k).collect::<Vec<_>>().join("|")))),
        },
    }
}

/// The HAVING clause and whether held-locally rows sort first, for a `view`.
fn view_parts(view: Option<&str>) -> Result<(&'static str, bool)> {
    match view.filter(|v| !v.is_empty()).unwrap_or("collection_local_first") {
        "local" => Ok((" HAVING local_games > 0", false)),
        "server" => Ok((" HAVING remote_games > 0", false)),
        "missing_on_server" => Ok((" HAVING remote_games = 0 AND (local_games > 0 OR peer_games > 0)", false)),
        "collection" => Ok(("", false)),
        "collection_local_first" => Ok(("", true)),
        // The RA views narrow in WHERE (`ra_view`), not HAVING.
        "local_ra_local_first" | "ra_only" => Ok(("", true)),
        "local_ra_ra_first" => Ok(("", false)),
        other => Err(Error::invalid(format!("view {other:?} must be one of {}", VIEWS.join("|")))),
    }
}

/// "RetroAchievements supports this release": it is linked to an RA game, or RA's own hash
/// list (imported as the `retroachievements` DAT source) names it. `release` is the SQL
/// expression for the release id.
fn ra_release_sql(release: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM release_provider_links vrl WHERE vrl.release_id = {release} AND vrl.provider = 'retroachievements' AND vrl.deleted = FALSE AND vrl.confidence IN ('exact', 'manual', 'hash')) \
          OR EXISTS (SELECT 1 FROM release_sources vrs JOIN sources vs ON vs.id = vrs.source_id WHERE vrs.release_id = {release} AND vs.name = 'retroachievements'))"
    )
}

/// "Held on this machine", as a predicate on a release id rather than the list's aggregate,
/// so it can sit in WHERE next to [`ra_release_sql`].
fn local_release_sql(release: &str) -> String {
    format!("EXISTS (SELECT 1 FROM local_release_games vl JOIN local_game vg ON vg.id = vl.game_id AND vg.deleted = FALSE WHERE vl.release_id = {release})")
}

/// What an RA view adds to a list: a WHERE fragment (to go before GROUP BY) and, for
/// `local_ra_ra_first`, an ORDER BY prefix. `title_level`: the list's rows are titles
/// (`t.id`), which qualify through any of their releases; otherwise releases (`r.id`).
///
/// In WHERE rather than HAVING on purpose: these views keep a small part of a
/// million-release catalogue, and filtering before the grouping means the rest is never
/// grouped or sorted at all.
fn ra_view(view: Option<&str>, title_level: bool) -> (Option<String>, Option<String>) {
    let of = |pred: &dyn Fn(&str) -> String| {
        if title_level {
            format!("EXISTS (SELECT 1 FROM title_releases vtr WHERE vtr.title_id = t.id AND {})", pred("vtr.release_id"))
        } else {
            pred("r.id")
        }
    };
    let ra = of(&ra_release_sql);
    let local = of(&local_release_sql);
    match view.unwrap_or("") {
        "ra_only" => (Some(format!(" AND {ra}")), None),
        "local_ra_local_first" => (Some(format!(" AND ({ra} OR {local})")), None),
        "local_ra_ra_first" => (Some(format!(" AND ({ra} OR {local})")), Some(format!("{ra} DESC"))),
        _ => (None, None),
    }
}

pub(crate) fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

fn platform_display(id: u16) -> String {
    Platform::from_igdb_id(id).map(|p| p.display_name().to_string()).unwrap_or_else(|| format!("platform {id}"))
}

/// The order among several media rows in one slot: a front (or a type without sides) before
/// a back or spine -- a grid shows the front -- then the `media_priority` order for the row's
/// type (`MediaRules::rank`), then the oldest. A user's own pick (`manual`) is first unless
/// the order places it. `JSON_SEARCH` gives the place in the order as a path (`"$[3]"`),
/// read back as a number; a source not in the order goes last.
/// Whether release `$alias` is RetroAchievements-supported: RA's own hash list matched it (or
/// the user linked it), and its RA game has achievements -- one that is not cached yet counts
/// as having them. A Hasheous cross-reference does not count: it names the game, not that RA
/// accepts this dump. What the RA badge on every list and page means.
macro_rules! ra_exists {
    ($alias:literal) => {
        concat!(
            "EXISTS (SELECT 1 FROM release_provider_links rl_ra LEFT JOIN retroachievements_games g_ra ON g_ra.provider_id = CAST(rl_ra.provider_id AS UNSIGNED) \
             WHERE rl_ra.release_id = ", $alias, ".id AND rl_ra.provider = 'retroachievements' AND rl_ra.deleted = FALSE AND rl_ra.confidence IN ('exact', 'manual') \
             AND COALESCE(g_ra.num_achievements, 1) > 0)"
        )
    };
}

/// The `media_priority` list `m`'s type is ranked by. A box front and a cover are one slot --
/// every provider's front picture, called `box` by some (TheGamesDB, ScreenScraper, IGDB here)
/// and `cover` by others (SteamGridDB) -- so both go by the box list, then the cover list.
macro_rules! media_priority_list {
    () => {
        "(SELECT IF(m.media_type IN ('box', 'cover'), JSON_MERGE_PRESERVE(COALESCE(JSON_EXTRACT(mp.value, '$.box'), '[]'), COALESCE(JSON_EXTRACT(mp.value, '$.cover'), '[]')), JSON_EXTRACT(mp.value, CONCAT('$.', m.media_type))) FROM settings mp WHERE mp.`key` = 'media_priority')"
    };
}

/// Front first; your own pick first unless `media_priority` places it; then art filed under
/// the release's own region (a regional box -- TheGamesDB's Japanese one for a Japanese
/// release) before art for every region (`common/`) -- never for a provider with no regions
/// (`settings::REGIONLESS_SOURCES`), whose file lands in a region folder only because its
/// releases are one group; then the provider order; then oldest.
macro_rules! media_order {
    () => {
        concat!(
            "(m.sub_category IS NULL OR m.sub_category = 'front') DESC, \
         CASE WHEN m.source = 'manual' AND JSON_SEARCH(",
            media_priority_list!(),
            ", 'one', 'manual') IS NULL THEN 0 ELSE 1 END, \
         CASE WHEN m.source <> 'manual' AND (m.path LIKE '%/common/%' OR m.source IN ('steamgriddb', 'retroachievements')) THEN 1 ELSE 0 END, \
         COALESCE(CAST(REPLACE(REPLACE(JSON_UNQUOTE(JSON_SEARCH(",
            media_priority_list!(),
            ", 'one', m.source)), '$[', ''), ']', '') AS SIGNED), 1000), m.id"
        )
    };
}

/// [`media_order`] for queries outside this module (the RetroAchievements grid).
pub(super) const MEDIA_ORDER: &str = media_order!();

/// Which `media.media_type`s list/grid rows carry as `cover_media_id`, for `FIND_IN_SET`.
pub(crate) async fn grid_media_type(app: &App) -> Result<String> {
    Ok(grid_types(&db_settings::get::<String>(&app.pool, db_settings::KEY_GRID_MEDIA_TYPE).await?.unwrap_or_else(|| "box".to_string())))
}

/// The `grid_media_type` setting as the types it takes: a box and a cover are one slot (see
/// `media_priority_list`), so either takes both.
pub fn grid_types(setting: &str) -> String {
    match setting {
        "box" | "cover" => "box,cover".to_string(),
        other => other.to_string(),
    }
}

async fn hashes_for(pool: &MySqlPool, content_ids: &[u64]) -> Result<HashMap<u64, BTreeMap<String, String>>> {
    let mut out: HashMap<u64, BTreeMap<String, String>> = HashMap::new();
    if content_ids.is_empty() {
        return Ok(out);
    }
    let sql = format!("SELECT content_id, algorithm, value_hex FROM hashes WHERE content_id IN ({})", placeholders(content_ids.len()));
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
    for id in content_ids {
        q = q.bind(id);
    }
    for row in q.fetch_all(pool).await? {
        let id: u64 = row.try_get("content_id")?;
        let algorithm: String = row.try_get("algorithm")?;
        let value: String = row.try_get("value_hex")?;
        out.entry(id).or_default().insert(algorithm, value);
    }
    Ok(out)
}

async fn member_counts_for(pool: &MySqlPool, content_ids: &[u64]) -> Result<HashMap<u64, u64>> {
    let mut out = HashMap::new();
    if content_ids.is_empty() {
        return Ok(out);
    }
    let sql = format!("SELECT parent_content_id, COUNT(*) AS n FROM content_contents WHERE parent_content_id IN ({}) GROUP BY parent_content_id", placeholders(content_ids.len()));
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
    for id in content_ids {
        q = q.bind(id);
    }
    for row in q.fetch_all(pool).await? {
        let id: u64 = row.try_get("parent_content_id")?;
        let n: i64 = row.try_get("n")?;
        out.insert(id, n as u64);
    }
    Ok(out)
}

const FILE_SELECT: &str = "f.id, f.path, f.size, f.archive_format, f.directory_id, f.content_id, f.deleted, f.last_modified, f.last_full_check, f.last_integrity_check, f.last_integrity_failure, c.app_platform_id AS platform_id, p.name AS platform";
const FILE_JOINS: &str = "LEFT JOIN content c ON c.id = f.content_id LEFT JOIN platforms p ON p.id = c.app_platform_id";

fn row_to_file(row: &MySqlRow) -> Result<FileRow> {
    Ok(FileRow {
        id: row.try_get("id")?,
        path: row.try_get("path")?,
        size: row.try_get("size")?,
        archive_format: row.try_get("archive_format")?,
        directory_id: row.try_get("directory_id")?,
        content_id: row.try_get("content_id")?,
        deleted: row.try_get("deleted")?,
        last_modified: row.try_get("last_modified")?,
        last_full_check: row.try_get("last_full_check")?,
        last_integrity_check: row.try_get("last_integrity_check")?,
        last_integrity_failure: row.try_get("last_integrity_failure")?,
        platform_id: row.try_get("platform_id")?,
        platform: row.try_get("platform")?,
        hashes: BTreeMap::new(),
        member_count: 0,
        found_elsewhere: None,
        sheet_check: None,
    })
}

async fn decorate_files(pool: &MySqlPool, files: &mut [FileRow]) -> Result<()> {
    let ids: Vec<u64> = files.iter().map(|f| f.content_id).collect();
    let hashes = hashes_for(pool, &ids).await?;
    let members = member_counts_for(pool, &ids).await?;
    for f in files.iter_mut() {
        f.hashes = hashes.get(&f.content_id).cloned().unwrap_or_default();
        f.member_count = members.get(&f.content_id).copied().unwrap_or(0);
    }
    let missing: Vec<(u64, u64)> = files.iter().filter(|f| f.deleted).map(|f| (f.id, f.content_id)).collect();
    if !missing.is_empty() {
        let list = missing.iter().map(|m| m.1.to_string()).collect::<Vec<_>>().join(",");
        let found: HashMap<u64, u64> = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT content_id, MIN(id) AS id FROM local_files WHERE deleted = FALSE AND content_id IN ({list}) GROUP BY content_id")))
            .fetch_all(pool)
            .await?
            .iter()
            .map(|r| Ok((r.try_get("content_id")?, r.try_get("id")?)))
            .collect::<std::result::Result<_, sqlx::Error>>()?;
        for f in files.iter_mut().filter(|f| f.deleted) {
            f.found_elsewhere = found.get(&f.content_id).copied().filter(|id| *id != f.id);
        }
    }
    decorate_sheet_checks(pool, files).await?;
    Ok(())
}

/// Attaches the last verdict to whichever of these files is a sidecar.
///
/// Only the sidecars: the join is over the handful of `.cue`/`.gdi`/`.m3u` rows in a page, not
/// over every file, because nothing else can have a verdict and asking about the rest is a
/// wasted round trip on every file list in the app.
async fn decorate_sheet_checks(pool: &MySqlPool, files: &mut [FileRow]) -> Result<()> {
    let ids: Vec<u64> = files
        .iter()
        .filter(|f| {
            std::path::Path::new(&f.path)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "cue" | "gdi" | "m3u"))
        })
        .map(|f| f.id)
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let list = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT file_id, verdict, hash_known, detail FROM local_sheet_check WHERE file_id IN ({list})"))).fetch_all(pool).await?;
    let mut by_file: HashMap<u64, romexplorer_types::sheets::SheetVerdictView> = HashMap::new();
    for r in &rows {
        let detail: Option<String> = r.try_get("detail")?;
        by_file.insert(
            r.try_get("file_id")?,
            romexplorer_types::sheets::SheetVerdictView {
                verdict: r.try_get("verdict")?,
                hash_known: r.try_get("hash_known")?,
                problems: detail.map(|d| d.lines().map(str::to_string).collect()).unwrap_or_default(),
            },
        );
    }
    for f in files.iter_mut() {
        f.sheet_check = by_file.remove(&f.id);
    }
    Ok(())
}

const CONTENT_SELECT: &str = "c.id AS content_id, c.file_name, c.size, c.app_platform_id AS platform_id, p.name AS platform, c.media_type, c.partial";

/// The name DATs give content `c`'s bytes (No-Intro / Redump before the rest, then the most
/// common), or NULL when no DAT lists them. `content.file_name` is only the name the bytes
/// were first seen under -- a CHD's inner image is "DRAGONQUEST5.bin" there, while Redump
/// knows the same bytes as "Hoge Quest V - Fuga no Hanayome (Japan, Asia).iso".
const CANONICAL_NAME_SQL: &str = "(SELECT rcn.file_name FROM release_contents rcn LEFT JOIN sources sn ON sn.id = rcn.source_id WHERE rcn.content_id = c.id     GROUP BY rcn.file_name ORDER BY MIN(FIELD(COALESCE(sn.name, ''), 'no-intro', 'redump', 'mame-redump', 'tosec') = 0), MIN(NULLIF(FIELD(COALESCE(sn.name, ''), 'no-intro', 'redump', 'mame-redump', 'tosec'), 0)), COUNT(*) DESC, rcn.file_name LIMIT 1)";

fn row_to_content(row: &MySqlRow) -> Result<ContentRow> {
    Ok(ContentRow {
        content_id: row.try_get("content_id")?,
        file_name: row.try_get("file_name")?,
        size: row.try_get("size")?,
        platform_id: row.try_get("platform_id")?,
        platform: row.try_get("platform")?,
        media_type: row.try_get("media_type")?,
        partial: row.try_get("partial")?,
        hashes: BTreeMap::new(),
        member_count: 0,
        member_path: row.try_get("member_path").unwrap_or(None),
        canonical_name: row.try_get("canonical_name").unwrap_or(None),
        dump_source: None,
        dump_sources: None,
        variant: None,
        held: false,
        superseded_at: None,
        superseded_by: None,
    })
}

async fn decorate_contents(pool: &MySqlPool, contents: &mut [ContentRow]) -> Result<()> {
    let ids: Vec<u64> = contents.iter().map(|c| c.content_id).collect();
    let hashes = hashes_for(pool, &ids).await?;
    let members = member_counts_for(pool, &ids).await?;
    for c in contents.iter_mut() {
        c.hashes = hashes.get(&c.content_id).cloned().unwrap_or_default();
        c.member_count = members.get(&c.content_id).copied().unwrap_or(0);
    }
    Ok(())
}

async fn content_row(pool: &MySqlPool, content_id: u64) -> Result<Option<ContentRow>> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT {CONTENT_SELECT} FROM content c LEFT JOIN platforms p ON p.id = c.app_platform_id WHERE c.id = ?"))).bind(content_id).fetch_optional(pool).await?;
    match row {
        Some(row) => {
            let mut c = vec![row_to_content(&row)?];
            decorate_contents(pool, &mut c).await?;
            Ok(c.pop())
        }
        None => Ok(None),
    }
}

async fn members_of(pool: &MySqlPool, content_id: u64) -> Result<Vec<ContentRow>> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT {CONTENT_SELECT}, cc.member_path, {CANONICAL_NAME_SQL} AS canonical_name FROM content_contents cc JOIN content c ON c.id = cc.child_content_id LEFT JOIN platforms p ON p.id = c.app_platform_id WHERE cc.parent_content_id = ? ORDER BY COALESCE(cc.member_path, c.file_name)")))
        .bind(content_id)
        .fetch_all(pool)
        .await?;
    let mut out = rows.iter().map(row_to_content).collect::<Result<Vec<_>>>()?;
    decorate_contents(pool, &mut out).await?;
    Ok(out)
}

async fn media_for(pool: &MySqlPool, junction: &str, key: &str, id: u64) -> Result<Vec<MediaRow>> {
    let sql = format!("SELECT m.id, m.media_type, m.sub_category, m.path, m.source, m.source_url FROM {junction} j JOIN media m ON m.id = j.media_id WHERE j.{key} = ? AND m.deleted = FALSE ORDER BY m.media_type, m.sub_category, {}", media_order!());
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(id).fetch_all(pool).await?;
    rows.iter()
        .map(|r| Ok(MediaRow { id: r.try_get("id")?, media_type: r.try_get("media_type")?, sub_category: r.try_get("sub_category")?, path: r.try_get("path")?, source: r.try_get("source")?, source_url: r.try_get("source_url")?, via: None }))
        .collect()
}

/// What a page shows is its own media, and for each slot it has nothing in, the next level
/// up's: a release takes its title's, a title the game's (`crossplatform`). A slot is a type
/// with its side (`box`/`front`), or a whole list type (`screenshot`, `fanart`, `video`):
/// one screenshot of its own and a release shows only that one. Nothing is inherited
/// sideways -- a release never shows another region's box.
fn inherit(own: &mut Vec<MediaRow>, wider: Vec<MediaRow>, via: &'static str) {
    let slot = |m: &MediaRow| (m.media_type.clone(), m.sub_category.clone());
    let have: std::collections::HashSet<(String, Option<String>)> = own.iter().map(slot).collect();
    own.extend(wider.into_iter().filter(|m| !have.contains(&slot(m))).map(|m| MediaRow { via: m.via.or(Some(via)), ..m }));
}

/// The game's own art for `title_id`, whichever title fetched it: what its providers have
/// under their id for the game and no region (`<source>/<provider id>/common/...`).
async fn cross_media(pool: &MySqlPool, title_id: u64) -> Result<Vec<MediaRow>> {
    let sql = format!(
        "SELECT m.id, m.media_type, m.sub_category, m.path, m.source, m.source_url FROM title_provider_links l \
         JOIN media m ON m.source = l.provider AND m.provider_id = l.provider_id AND m.deleted = FALSE \
         WHERE l.title_id = ? AND l.deleted = FALSE AND m.path LIKE '%/common/%' ORDER BY m.media_type, m.sub_category, {}",
        media_order!()
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(title_id).fetch_all(pool).await?;
    rows.iter()
        .map(|r| Ok(MediaRow { id: r.try_get("id")?, media_type: r.try_get("media_type")?, sub_category: r.try_get("sub_category")?, path: r.try_get("path")?, source: r.try_get("source")?, source_url: r.try_get("source_url")?, via: Some("crossplatform") }))
        .collect()
}

/// A title's media: its own, then the game's for what it lacks ([`inherit`]).
async fn title_media(pool: &MySqlPool, title_id: u64) -> Result<Vec<MediaRow>> {
    let mut list = media_for(pool, "title_media", "title_id", title_id).await?;
    inherit(&mut list, cross_media(pool, title_id).await?, "crossplatform");
    Ok(list)
}

/// A release's media: its own (its region's and its own uploads), then its title's, then
/// the game's ([`inherit`]).
async fn release_media(pool: &MySqlPool, release_id: u64, title_id: Option<u64>) -> Result<Vec<MediaRow>> {
    let mut list = media_for(pool, "release_media", "release_id", release_id).await?;
    if let Some(title_id) = title_id {
        inherit(&mut list, title_media(pool, title_id).await?, "title");
    }
    Ok(list)
}

/// The provider's own page for an entry. IGDB has no id-addressed page, so its cached raw
/// response is consulted for `url`/`slug` and a site search is the fallback.
async fn provider_url(pool: &MySqlPool, provider: &str, provider_id: &str, title: &str) -> Option<String> {
    match provider {
        "retroachievements" => Some(format!("https://retroachievements.org/game/{provider_id}")),
        "mobygames" => Some(format!("https://www.mobygames.com/game/{provider_id}/")),
        "thegamesdb" => Some(format!("https://thegamesdb.net/game.php?id={provider_id}")),
        "screenscraper" => Some(format!("https://www.screenscraper.fr/gameinfos.php?gameid={provider_id}")),
        "steamgriddb" => Some(format!("https://www.steamgriddb.com/game/{provider_id}")),
        "emuready" => Some(format!("https://www.emuready.com/games/{provider_id}")),
        "hasheous" => Some(format!("https://hasheous.org/index.html?page=dataobjectdetail&type=game&id={provider_id}")),
        "igdb" => {
            let raw: Option<String> = sqlx::query("SELECT raw_response_path FROM igdb_games WHERE provider_id = ? AND deleted = FALSE LIMIT 1").bind(provider_id.parse::<i64>().ok()?).fetch_optional(pool).await.ok().flatten().and_then(|r| r.try_get("raw_response_path").ok());
            if let Some(path) = raw {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                        let wanted = provider_id.parse::<i64>().ok();
                        let entries: Vec<&serde_json::Value> = match &json {
                            serde_json::Value::Array(a) => a.iter().collect(),
                            other => vec![other],
                        };
                        for e in entries {
                            if wanted.is_some_and(|w| e.get("id").and_then(|v| v.as_i64()) != Some(w)) {
                                continue;
                            }
                            if let Some(url) = e.get("url").and_then(|v| v.as_str()) {
                                return Some(url.to_string());
                            }
                            if let Some(slug) = e.get("slug").and_then(|v| v.as_str()) {
                                return Some(format!("https://www.igdb.com/games/{slug}"));
                            }
                        }
                    }
                }
            }
            Some(format!("https://www.igdb.com/search?q={}", urlencoding_min(title)))
        }
        _ => None,
    }
}

/// `provider_url` for other modules (manual-match search results).
pub async fn provider_page_url(pool: &MySqlPool, provider: &str, provider_id: &str, title: &str) -> Option<String> {
    provider_url(pool, provider, provider_id, title).await
}

fn urlencoding_min(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

async fn provider_links_for(pool: &MySqlPool, table: &str, key: &str, id: u64, via: &str, title: &str) -> Result<Vec<ProviderLink>> {
    let sql = format!("SELECT provider, provider_id, confidence, linked_at FROM {table} WHERE {key} = ? AND deleted = FALSE ORDER BY provider");
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(id).fetch_all(pool).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let provider: String = r.try_get("provider")?;
        let provider_id: String = r.try_get("provider_id")?;
        let url = provider_url(pool, &provider, &provider_id, title).await;
        out.push(ProviderLink { provider, provider_id, confidence: r.try_get("confidence")?, linked_at: r.try_get("linked_at")?, url, via: via.to_string(), release_id: (via == "release").then_some(id) });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// facet filters (genre / franchise / company / keyword / region / language)
// ---------------------------------------------------------------------------

fn csv_values(v: Option<&str>) -> Vec<String> {
    v.unwrap_or("").split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

fn logic_is_and(v: Option<&str>) -> bool {
    !v.is_some_and(|l| l.eq_ignore_ascii_case("or"))
}

/// The genre source a title is filtered by: of the sources that gave it genres, the first in
/// `genre_order` (`settings::genre_source_priority`). Matching any source's genre made a game
/// IGDB calls a puzzle game turn up under role-playing because MobyGames said so too.
fn genre_source_sql(genre_order: &[String], junction_alias: &str, title_expr: &str) -> String {
    // The order holds known source names only (`genre_source_priority` filters them), so
    // they can go into the statement as text.
    let list = genre_order.iter().map(|s| format!("'{}'", s.replace('\'', ""))).collect::<Vec<_>>().join(", ");
    if list.is_empty() {
        return String::new();
    }
    format!(" AND {junction_alias}.source = (SELECT tgs.source FROM title_genres tgs WHERE tgs.title_id = {title_expr} ORDER BY FIELD(tgs.source, {list}) = 0, FIELD(tgs.source, {list}), tgs.source LIMIT 1)")
}

/// Appends the title-scoped facet conditions of `q` to `body`; `title_expr` is the title
/// id expression in the enclosing query (`t.id` everywhere the lists join `title t`).
/// Genres are a title's from its leading genre source (`genre_source_sql`).
fn push_facet_filters(body: &mut String, binds: &mut Vec<Bind>, q: &ListQuery, title_expr: &str, genre_order: &[String]) {
    // (junction, name table, junction id column)
    let facets: [(&str, &str, &str, Vec<String>, bool); 4] = [
        ("title_genres", "genre", "genre_id", csv_values(q.genres.as_deref()), logic_is_and(q.genre_logic.as_deref())),
        ("title_franchises", "franchise", "franchise_id", csv_values(q.franchises.as_deref()), logic_is_and(q.franchise_logic.as_deref())),
        ("title_companies", "company", "company_id", csv_values(q.companies.as_deref()), logic_is_and(q.company_logic.as_deref())),
        ("title_keywords", "keyword", "keyword_id", csv_values(q.keywords.as_deref()), logic_is_and(q.keyword_logic.as_deref())),
    ];
    for (junction, table, id_col, names, and) in facets {
        if names.is_empty() {
            continue;
        }
        let by_source = if junction == "title_genres" { genre_source_sql(genre_order, "j", title_expr) } else { String::new() };
        if and {
            for name in names {
                body.push_str(&format!(" AND EXISTS (SELECT 1 FROM {junction} j JOIN {table} n ON n.id = j.{id_col} WHERE j.title_id = {title_expr} AND n.name = ?{by_source})"));
                binds.push(Bind::Str(name));
            }
        } else {
            let marks = std::iter::repeat("?").take(names.len()).collect::<Vec<_>>().join(", ");
            body.push_str(&format!(" AND EXISTS (SELECT 1 FROM {junction} j JOIN {table} n ON n.id = j.{id_col} WHERE j.title_id = {title_expr} AND n.name IN ({marks}){by_source})"));
            binds.extend(names.into_iter().map(Bind::Str));
        }
    }
    let tag_sources = match q.tag_source.as_deref().map(str::trim) {
        Some("user") => "'user'",
        Some("auto") => "'auto'",
        _ => "'user', 'auto'",
    };
    let tags = csv_values(q.tags.as_deref());
    if tags.is_empty() {
        if q.tag_source.as_deref().is_some_and(|s| matches!(s.trim(), "user" | "auto")) {
            body.push_str(&format!(" AND EXISTS (SELECT 1 FROM title_keywords jt WHERE jt.title_id = {title_expr} AND jt.source IN ({tag_sources}))"));
        }
    } else if logic_is_and(q.tag_logic.as_deref()) {
        for name in tags {
            body.push_str(&format!(" AND EXISTS (SELECT 1 FROM title_keywords jt JOIN keyword nt ON nt.id = jt.keyword_id WHERE jt.title_id = {title_expr} AND jt.source IN ({tag_sources}) AND nt.name = ?)"));
            binds.push(Bind::Str(name));
        }
    } else {
        let marks = std::iter::repeat("?").take(tags.len()).collect::<Vec<_>>().join(", ");
        body.push_str(&format!(" AND EXISTS (SELECT 1 FROM title_keywords jt JOIN keyword nt ON nt.id = jt.keyword_id WHERE jt.title_id = {title_expr} AND jt.source IN ({tag_sources}) AND nt.name IN ({marks}))"));
        binds.extend(tags.into_iter().map(Bind::Str));
    }
    let majors = csv_values(q.genre_major.as_deref());
    if !majors.is_empty() {
        let by_source = genre_source_sql(genre_order, "tgm", title_expr);
        let exists = |cond: &str| format!(" AND EXISTS (SELECT 1 FROM title_genres tgm JOIN genre gm ON gm.id = tgm.genre_id LEFT JOIN genre pgm ON pgm.id = gm.parent_genre_id WHERE tgm.title_id = {title_expr} AND {cond}{by_source})");
        if q.genre_major_logic.as_deref().is_some_and(|l| l.eq_ignore_ascii_case("and")) {
            for major in majors {
                body.push_str(&exists("(gm.name = ? OR pgm.name = ?)"));
                binds.push(Bind::Str(major.clone()));
                binds.push(Bind::Str(major));
            }
        } else {
            let marks = vec!["?"; majors.len()].join(", ");
            body.push_str(&exists(&format!("(gm.name IN ({marks}) OR pgm.name IN ({marks}))")));
            binds.extend(majors.iter().cloned().map(Bind::Str));
            binds.extend(majors.into_iter().map(Bind::Str));
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FacetValue {
    pub name: String,
    /// Genres only: the major genre this one sits under (itself for a major genre).
    pub major: Option<String>,
    pub titles: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Facets {
    pub genre_majors: Vec<String>,
    pub genres: Vec<FacetValue>,
    pub franchises: Vec<FacetValue>,
    pub companies: Vec<FacetValue>,
    pub keywords: Vec<FacetValue>,
    /// Keyword tags (the user's and the auto-tagger's), with how many titles carry each.
    pub tags: Vec<FacetValue>,
}

/// Every value the facet filters can take, with how many titles carry it.
pub async fn facets(app: &App) -> Result<Facets> {
    let pool = &app.pool;
    let genres = sqlx::query("SELECT g.name, COALESCE(p.name, g.name) AS major, COUNT(DISTINCT tg.title_id) AS titles FROM genre g LEFT JOIN genre p ON p.id = g.parent_genre_id LEFT JOIN title_genres tg ON tg.genre_id = g.id WHERE g.deleted = FALSE GROUP BY g.id, g.name, p.name ORDER BY g.name").fetch_all(pool).await?;
    let simple = |table: &'static str, junction: &'static str, key: &'static str| {
        let sql = format!("SELECT n.name, COUNT(DISTINCT j.title_id) AS titles FROM {table} n LEFT JOIN {junction} j ON j.{key} = n.id GROUP BY n.id, n.name ORDER BY n.name");
        async move { sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await }
    };
    let franchises = simple("franchise", "title_franchises", "franchise_id").await?;
    let companies = simple("company", "title_companies", "company_id").await?;
    let keywords = simple("keyword", "title_keywords", "keyword_id").await?;
    let tags = sqlx::query("SELECT k.name, COUNT(DISTINCT tk.title_id) AS titles FROM keyword k JOIN title_keywords tk ON tk.keyword_id = k.id WHERE tk.source IN ('user', 'auto') GROUP BY k.id, k.name ORDER BY k.name").fetch_all(pool).await?;
    let plain = |rows: &[MySqlRow]| -> Result<Vec<FacetValue>> { rows.iter().map(|r| Ok(FacetValue { name: r.try_get("name")?, major: None, titles: r.try_get::<i64, _>("titles")? as u64 })).collect() };
    Ok(Facets {
        genre_majors: crate::database::providers::ra_genre::MAJOR_GENRES.iter().map(|s| s.to_string()).collect(),
        genres: genres.iter().map(|r| Ok(FacetValue { name: r.try_get("name")?, major: r.try_get("major")?, titles: r.try_get::<i64, _>("titles")? as u64 })).collect::<Result<_>>()?,
        franchises: plain(&franchises)?,
        companies: plain(&companies)?,
        keywords: plain(&keywords)?,
        tags: plain(&tags)?,
    })
}

// ---------------------------------------------------------------------------
// platforms
// ---------------------------------------------------------------------------

pub async fn platforms(app: &App) -> Result<Vec<PlatformInfo>> {
    let rows = sqlx::query("SELECT id, name, family, is_disc_based FROM platforms ORDER BY name").fetch_all(&app.pool).await?;
    // Japanese names are in the Japanese language file with the rest of the translations
    // (`platform.<name>`); a platform it has none for keeps its own name.
    let ja = super::i18n::strings(app, "ja").unwrap_or(serde_json::Value::Null);
    rows.iter()
        .map(|r| {
            let id: u16 = r.try_get("id")?;
            let name: String = r.try_get("name")?;
            let name_ja = ja.get(format!("platform.{name}")).and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map_or_else(|| name.clone(), str::to_string);
            let group = crate::database::connection::platform_group(crate::common::models::Platform::from_igdb_id(id).unwrap_or(crate::common::models::Platform::Unknown));
            Ok(PlatformInfo { id, name, name_ja, family: r.try_get("family")?, group, is_disc_based: r.try_get("is_disc_based")? })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// titles
// ---------------------------------------------------------------------------

/// The order releases of one title come in (docs/plan/gui.md "release rank"):
/// 1. RetroAchievements-compatible releases first -- a release whose content MD5 is in
///    the RA hash list (or is exact-linked to an RA game) plays with achievements, so it
///    outranks the region-preferred one when that one isn't supported;
/// 2. `region_priority` (first region of the release's list);
/// 3. `language_priority` (best language the release has);
/// then the caller's tie-break. `alias` is the `release` table alias. Codes are validated
/// settings values and inlined (this expression also sits inside ORDER BY, where binds
/// would have to be repeated for the count query).
pub struct ReleaseRank {
    regions: Vec<String>,
    languages: Vec<String>,
    /// `release_edition_order` (`settings::edition_priority`): every edition, in order.
    editions: Vec<String>,
    /// `release_unlicensed` is `retail`: unlicensed releases rank and list with retail.
    pub unlicensed_is_retail: bool,
}

/// The `material` filter as a condition on release alias `r` (empty: no filter).
fn material_clause(material: Option<&str>) -> Result<String> {
    Ok(match material.filter(|m| !m.is_empty()).unwrap_or("game") {
        "game" => " AND r.material IS NULL".to_string(),
        "all" => String::new(),
        "other" => " AND r.material IS NOT NULL".to_string(),
        // Collection › Etc. A DLC installer is DLC that only the `etc` DATs list.
        "etc" => format!(
            " AND (r.material IN ({}) OR (r.material = 'dlc' AND EXISTS (SELECT 1 FROM release_sources rse JOIN sources se ON se.id = rse.source_id WHERE rse.release_id = r.id AND se.name = 'etc')))",
            crate::database::platform_map::ETC.iter().map(|m| format!("'{m}'")).collect::<Vec<_>>().join(", ")
        ),
        // Collection › Multimedia: the disc media, of whichever kind.
        "multimedia" => format!(" AND r.material IN ({})", crate::database::platform_map::MULTIMEDIA.iter().map(|m| format!("'{m}'")).collect::<Vec<_>>().join(", ")),
        "unbundled" => format!(
            " AND r.material IN ({}) AND NOT EXISTS (SELECT 1 FROM title_releases trg JOIN `release` rg ON rg.id = trg.release_id WHERE trg.title_id = t.id AND rg.deleted = FALSE AND rg.material IS NULL)",
            crate::database::platform_map::game_extras_sql()
        ),
        // From the fixed list, so it can go into the statement as text.
        m if crate::database::platform_map::MATERIALS.contains(&m) => format!(" AND r.material = '{m}'"),
        other => return Err(Error::invalid(format!("material {other:?} must be game, all, other, unbundled, multimedia, etc, {}", crate::database::platform_map::MATERIALS.join(", ")))),
    })
}

/// A title of the same family, on a title page: its base game's ports and remakes.
#[derive(Debug, Clone, Serialize)]
pub struct FamilyRef {
    pub id: u64,
    pub name: String,
    pub category: String,
    pub platforms: Option<String>,
}

/// The platforms of one title's releases, by name (binds the title id).
pub(crate) const TITLE_PLATFORMS_SQL: &str = "SELECT GROUP_CONCAT(DISTINCT pf.name ORDER BY pf.name SEPARATOR ', ') FROM title_releases trf JOIN `release` rf ON rf.id = trf.release_id AND rf.deleted = FALSE JOIN platforms pf ON pf.id = rf.app_platform_id WHERE trf.title_id = ?";

/// What title `t` is when it holds no release of a game as it was sold: `NULL` when it holds
/// one (or holds no game release at all); the kind of the game that is not a patch when
/// there is one (a homebrew game with hacks of it is homebrew); the kind its releases share;
/// `patched` for a mix of hacks, mods and translations of a game the catalogue does not have.
pub const TITLE_KIND_SQL: &str = "(SELECT CASE WHEN COUNT(*) = 0 OR SUM(rk.game_kind IS NULL) > 0 THEN NULL \
       WHEN SUM(rk.game_kind NOT IN ('hack', 'mod', 'translation')) > 0 THEN MIN(IF(rk.game_kind NOT IN ('hack', 'mod', 'translation'), rk.game_kind, NULL)) \
       WHEN COUNT(DISTINCT rk.game_kind) = 1 THEN MIN(rk.game_kind) ELSE 'patched' END \
     FROM title_releases trk JOIN `release` rk ON rk.id = trk.release_id AND rk.deleted = FALSE AND rk.material IS NULL WHERE trk.title_id = t.id)";

/// A release that sits under a title without being that title's game: a hack, mod or
/// translation of it, or something made for it or shipped with it. Its names, links and
/// metadata are its own (`providers::cache::APART_SQL` is the same rule for linking).
fn stands_apart(r: &ReleaseRow) -> bool {
    r.material.is_some() || r.game_kind.as_deref().is_some_and(|k| crate::database::platform_map::PATCHED_KINDS.contains(&k))
}

/// What a release is, as the `kinds` filter names it: its bucket
/// (`database::release_buckets::BUCKETS`) -- retail, unlicensed, aftermarket, a game kind of
/// its own (homebrew, arcade, wiiware), a translation / hack / mod, `patched` for what is
/// patched in some way nothing names (listed by RetroAchievements alone), a pirate copy, a
/// demo / beta / prototype, or a RetroAchievements subset.
pub const KINDS: [&str; 15] = release_buckets::BUCKETS;

/// The `kinds` filter as a condition on release `alias` (None: no filter). See
/// [`ListQuery::kinds`]. `unlicensed_is_retail`: the `release_unlicensed` setting -- when
/// unlicensed releases are listed with retail, `unlicensed` still picks them out.
fn kinds_clause(kinds: Option<&str>, alias: &str, unlicensed_is_retail: bool) -> Result<Option<String>> {
    let mut wanted: Vec<&'static str> = Vec::new();
    let mut guessed = false;
    for token in kinds.unwrap_or("all").split(',').map(str::trim).filter(|t| !t.is_empty()) {
        match token {
            "all" => return Ok(None),
            "patches" => wanted.extend(KINDS.iter().filter(|k| **k != "subset")),
            "commercial" => wanted.extend(release_buckets::COMMERCIAL),
            "guessed" => guessed = true,
            other => match KINDS.iter().find(|k| **k == other) {
                Some(kind) => wanted.push(kind),
                None => return Err(Error::invalid(format!("kinds {other:?} must be all, patches, commercial, guessed or any of {}", KINDS.join(", ")))),
            },
        }
    }
    wanted.sort_unstable();
    wanted.dedup();
    if wanted.len() == KINDS.len() || (wanted.is_empty() && !guessed) {
        return Ok(None);
    }
    let mut any = Vec::new();
    if !wanted.is_empty() {
        // From the fixed list, so they can go into the statement as text.
        any.push(format!("{} IN ({})", release_buckets::bucket_sql(alias, unlicensed_is_retail), wanted.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(", ")));
        if unlicensed_is_retail && wanted.contains(&"unlicensed") {
            any.push(format!("({} = 'retail' AND {alias}.is_unlicensed)", release_buckets::bucket_sql(alias, true)));
        }
    }
    if guessed {
        any.push(format!("{alias}.languages_state = 'guessed'"));
    }
    Ok(Some(format!("({})", any.join(" OR "))))
}

impl ReleaseRank {
    pub async fn load(app: &App) -> Result<Self> {
        let clean = |codes: Vec<String>| -> Vec<String> { codes.into_iter().filter(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-')).collect() };
        Ok(Self {
            regions: clean(db_settings::region_priority(&app.pool).await?),
            languages: clean(db_settings::language_priority(&app.pool).await?),
            editions: db_settings::edition_priority(&app.pool).await?.into_iter().filter(|k| db_settings::EDITION_KINDS.contains(&k.as_str())).collect(),
            unlicensed_is_retail: release_buckets::unlicensed_joins_retail(&app.pool).await?,
        })
    }

    /// The release's bucket, in rank order (`release_buckets::BUCKETS`): retail first, then
    /// the other full versions, the patched ones, pirate copies, demos and pre-release builds,
    /// RetroAchievements subsets last.
    pub fn kind_sql(&self, alias: &str) -> String {
        release_buckets::rank_sql(alias, self.unlicensed_is_retail)
    }

    /// The release's row on the Titles list (`release_buckets::row_key_sql`).
    pub fn row_key(&self, alias: &str) -> String {
        release_buckets::row_key_sql(alias, self.unlicensed_is_retail)
    }

    pub fn sql(&self, alias: &str) -> String {
        let kind = self.kind_sql(alias);
        let ra = format!(
            "(EXISTS (SELECT 1 FROM release_contents rcx JOIN hashes hx ON hx.content_id = rcx.content_id AND hx.algorithm IN ('md5', 'ra_hash') JOIN retroachievements_hashes rhx ON rhx.md5 = hx.value_hex WHERE rcx.release_id = {alias}.id) \
              OR EXISTS (SELECT 1 FROM release_provider_links rplx WHERE rplx.release_id = {alias}.id AND rplx.provider = 'retroachievements' AND rplx.confidence = 'exact' AND rplx.deleted = FALSE)) DESC"
        );
        let region_list = self.regions.iter().map(|c| format!("'{}'", c.to_ascii_uppercase())).collect::<Vec<_>>().join(", ");
        let region = if region_list.is_empty() { "0".to_string() } else { format!("COALESCE(NULLIF(FIELD(UPPER(SUBSTRING_INDEX({alias}.region, ',', 1)), {region_list}), 0), 999)") };
        let language = if self.languages.is_empty() {
            "0".to_string()
        } else {
            let terms: Vec<String> = self.languages.iter().enumerate().map(|(i, c)| format!("IF(FIND_IN_SET('{}', {alias}.languages), {}, 999)", c.to_ascii_lowercase(), i + 1)).collect();
            if terms.len() == 1 { terms[0].clone() } else { format!("LEAST({})", terms.join(", ")) }
        };
        // The user's manual order of the title's releases beats everything.
        let manual = format!("COALESCE((SELECT trw.sort_weight FROM title_releases trw WHERE trw.release_id = {alias}.id AND trw.sort_weight IS NOT NULL LIMIT 1), 1e300)");
        // Within one region / language: the edition, in the user's order (a name with two
        // edition tags is its stronger one, as the parser keeps it), then the latest version.
        let edition_list = self.editions.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(", ");
        let edition = format!(
            "FIELD(CASE WHEN {alias}.is_aftermarket THEN 'aftermarket' WHEN {alias}.is_special_edition THEN 'special' WHEN {alias}.is_rerelease THEN 'rerelease' WHEN {alias}.is_budget THEN 'budget' ELSE 'original' END, {edition_list}), {alias}.revision_sort DESC"
        );
        // Then the bucket (a full version before a demo, whatever its region), the user's
        // region / language priority, edition / version;
        // RetroAchievements support only breaks ties. Ends in a plain column: list sorting
        // appends ASC / DESC to the rank.
        // Before all of it: the game's own releases, then what was bundled with it (DLC,
        // updates, a soundtrack disc), so none of that is ever a title's top release.
        format!("({alias}.material IS NOT NULL), {manual}, {kind}, {region}, {language}, {edition}, {ra}, {alias}.id")
    }
}

/// A title's grid art: a user-picked (manual) item on the title itself, else the art of
/// its best-ranked release (images belong to releases; manual first within a release),
/// else anything linked to the title. Binds: media type x3.
///
/// `row`: on the Titles list, the art of one of the title's rows (`ReleaseRank::row_key` of
/// the list's release `r`): its own best-ranked release's, and the title's own pick only on
/// its retail row -- a hack row shows the hack.
pub(crate) fn title_cover_sql(rank: &ReleaseRank, row: Option<&str>) -> String {
    let order = rank.sql("r4");
    let media_order = media_order!();
    let (in_row, own_pick) = match row {
        Some(key) => (format!(" AND {} = {key}", rank.row_key("r4")), format!("{key} = 'retail' AND ")),
        None => (String::new(), String::new()),
    };
    format!(
        "COALESCE((SELECT m.id FROM title_media tm JOIN media m ON m.id = tm.media_id WHERE {own_pick}tm.title_id = t.id AND FIND_IN_SET(m.media_type, ?) AND m.source = 'manual' AND m.deleted = FALSE ORDER BY m.id DESC LIMIT 1), \
         (SELECT m.id FROM title_releases tr4 JOIN `release` r4 ON r4.id = tr4.release_id AND r4.deleted = FALSE JOIN release_media rm4 ON rm4.release_id = r4.id JOIN media m ON m.id = rm4.media_id \
          WHERE tr4.title_id = t.id{in_row} AND FIND_IN_SET(m.media_type, ?) AND m.deleted = FALSE ORDER BY {order}, r4.id, {media_order} LIMIT 1), \
         (SELECT m.id FROM title_media tm JOIN media m ON m.id = tm.media_id WHERE tm.title_id = t.id AND FIND_IN_SET(m.media_type, ?) AND m.deleted = FALSE ORDER BY m.id LIMIT 1))"
    )
}

pub(crate) async fn title_cover_binds(app: &App) -> Result<Vec<Bind>> {
    let media_type = grid_media_type(app).await?;
    Ok(vec![Bind::Str(media_type.clone()), Bind::Str(media_type.clone()), Bind::Str(media_type)])
}

/// The title's best-ranked live release (the same order its grid art follows); with `row`,
/// of that row of the Titles list (see [`title_cover_sql`]).
pub(crate) fn top_release_sql(rank: &ReleaseRank, row: Option<&str>) -> String {
    let in_row = row.map(|key| format!(" AND {} = {key}", rank.row_key("r5"))).unwrap_or_default();
    format!("(SELECT r5.id FROM title_releases tr5 JOIN `release` r5 ON r5.id = tr5.release_id AND r5.deleted = FALSE WHERE tr5.title_id = t.id{in_row} ORDER BY {}, r5.id LIMIT 1)", rank.sql("r5"))
}

/// What the local copies of the releases `releases` names (an SQL condition on
/// `lrgz.release_id`) take on disk: every file of every copy, once, at its real size -- a CHD
/// as a CHD. NULL when nothing is held.
fn local_size_sql(releases: &str) -> String {
    format!(
        "(SELECT CAST(SUM(fz.size) AS UNSIGNED) FROM local_files fz WHERE fz.deleted = FALSE AND fz.id IN \
           (SELECT lrfz.file_id FROM local_release_games lrgz JOIN local_game lgz ON lgz.id = lrgz.game_id AND lgz.deleted = FALSE \
            JOIN local_game_roms lgrz ON lgrz.game_id = lgz.id JOIN local_rom_files lrfz ON lrfz.rom_id = lgrz.rom_id WHERE lrgz.release_id {releases}))"
    )
}

/// A release's size as the DATs list it (`release` = an SQL expression for its id): the raw
/// dump, the way it is hashed -- not what a zip / 7z of it takes, and never a compressed disc
/// image: entries listed as a CHD, RVZ, WIA, GCZ, CSO / ZSO / CISO or WBFS (a mame-redump
/// DAT's) are left out. One dump only: a release several DATs list counts the file list of the
/// source whose files add up to most, not both. NULL when no DAT gives a raw size.
pub(crate) fn catalog_size_sql(release: &str, z: &str) -> String {
    let raw = |rc: &str, c: &str| format!("LOWER(COALESCE({rc}.file_name, {c}.file_name, '')) NOT REGEXP '[.](chd|rvz|wia|gcz|cso|zso|ciso|wbfs)$'");
    format!(
        "(SELECT CAST(SUM(c{z}.size) AS UNSIGNED) FROM release_contents rc{z} JOIN content c{z} ON c{z}.id = rc{z}.content_id WHERE rc{z}.release_id = {release} AND {raw_a} \
           AND rc{z}.source_id <=> (SELECT rcs{z}.source_id FROM release_contents rcs{z} JOIN content cs{z} ON cs{z}.id = rcs{z}.content_id \
             WHERE rcs{z}.release_id = {release} AND {raw_b} GROUP BY rcs{z}.source_id ORDER BY SUM(cs{z}.size) DESC LIMIT 1))",
        raw_a = raw(&format!("rc{z}"), &format!("c{z}")),
        raw_b = raw(&format!("rcs{z}"), &format!("cs{z}")),
    )
}

/// A title's four sizes, as `title_select` columns: its local files on disk for all its
/// releases and for its first release (by region priority), and the same two by the DATs.
/// `in_row` narrows "all its releases" to the list row's (see [`title_select`]).
fn title_size_columns(top_sql: &str, in_row: &dyn Fn(&str) -> String) -> [(&'static str, String); 4] {
    [
        ("local_size_all", local_size_sql(&format!("IN (SELECT trz.release_id FROM title_releases trz JOIN `release` rz ON rz.id = trz.release_id WHERE trz.title_id = t.id{})", in_row("rz")))),
        ("local_size_top", local_size_sql(&format!("= {top_sql}"))),
        (
            "catalog_size_all",
            format!("(SELECT CAST(SUM({}) AS UNSIGNED) FROM title_releases trq JOIN `release` rq ON rq.id = trq.release_id AND rq.deleted = FALSE WHERE trq.title_id = t.id{})", catalog_size_sql("rq.id", "q"), in_row("rq")),
        ),
        ("catalog_size_top", catalog_size_sql(top_sql, "w")),
    ]
}

/// The Titles list's columns. A row is one title's releases of one bucket
/// (`release_buckets`), the list's release `r` any of them: `in_row(alias)` is the
/// condition " AND release `alias` is in the same row", so what is counted for "all its
/// releases" is the row's.
fn title_select(cover_sql: &str, top_sql: &str, row_key: &str, in_row: &dyn Fn(&str) -> String) -> String {
    format!(
        "t.id, t.name, t.category, {TITLE_KIND_SQL} AS game_kind, t.basegame_title_id, {top_sql} AS top_release_id, COALESCE({row_key}, 'retail') AS row_key, COUNT(DISTINCT r.id) AS release_count, \
    GROUP_CONCAT(DISTINCT p.name ORDER BY p.name SEPARATOR ', ') AS platforms, COUNT(DISTINCT lg.id) AS local_games, IF(EXISTS (SELECT 1 FROM remote_sources m WHERE m.role = 'main' AND m.enabled = TRUE AND m.deleted = FALSE), COUNT(DISTINCT IF(rsrc.role = 'main', rg.id, NULL)), NULL) AS remote_games, COUNT(DISTINCT IF(rsrc.role = 'peer', rg.source_id, NULL), IF(rsrc.role = 'peer', rg.id, NULL)) AS peer_games, \
    GROUP_CONCAT(DISTINCT r.languages ORDER BY r.languages SEPARATOR ',') AS row_languages, \
    NULLIF(CONCAT_WS(',', \
        (SELECT GROUP_CONCAT(DISTINCT tpl.provider ORDER BY tpl.provider SEPARATOR ',') FROM title_provider_links tpl WHERE tpl.title_id = t.id AND tpl.deleted = FALSE), \
        (SELECT GROUP_CONCAT(DISTINCT rpl.provider ORDER BY rpl.provider SEPARATOR ',') FROM release_provider_links rpl JOIN title_releases tr2 ON tr2.release_id = rpl.release_id WHERE tr2.title_id = t.id AND rpl.deleted = FALSE)), '') AS providers, \
    {cover_sql} AS cover_media_id, \
    EXISTS (SELECT 1 FROM title_releases tra JOIN `release` rr ON rr.id = tra.release_id AND rr.deleted = FALSE WHERE tra.title_id = t.id{ra_row} AND {ra}) AS ra, {sizes}",
        ra = ra_exists!("rr"),
        ra_row = in_row("rr"),
        sizes = title_size_columns(top_sql, in_row).iter().map(|(name, sql)| format!("{sql} AS {name}")).collect::<Vec<_>>().join(", ")
    )
}

fn row_to_title(row: &MySqlRow) -> Result<TitleRow> {
    let row_key = row.try_get::<Option<String>, _>("row_key").ok().flatten().unwrap_or_else(|| "retail".to_string());
    let bucket = row_key.split(':').next().unwrap_or("retail").to_string();
    let release_count: i64 = row.try_get("release_count")?;
    let local_games: i64 = row.try_get("local_games")?;
    let remote_games: Option<i64> = row.try_get("remote_games")?;
    let peer_games: i64 = row.try_get("peer_games")?;
    Ok(TitleRow {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        category: row.try_get("category")?,
        game_kind: row.try_get("game_kind").unwrap_or(None),
        basegame_title_id: row.try_get("basegame_title_id")?,
        release_count: release_count as u64,
        platforms: row.try_get("platforms")?,
        providers: row.try_get("providers")?,
        local_games: local_games as u64,
        remote_games: remote_games.map(|n| n as u64),
        peer_games: peer_games as u64,
        cover_media_id: row.try_get("cover_media_id")?,
        display_name: String::new(),
        top_release_id: row.try_get("top_release_id")?,
        row_id: release_buckets::row_id(row.try_get("id")?, &row_key).unwrap_or(0),
        row_key,
        bucket,
        row_languages: row.try_get::<Option<String>, _>("row_languages").ok().flatten().map(|l| {
            let mut codes: Vec<&str> = l.split(',').map(str::trim).filter(|c| !c.is_empty()).collect();
            codes.sort_unstable();
            codes.dedup();
            codes.join(",")
        }).filter(|l| !l.is_empty()),
        ra: row.try_get::<Option<i64>, _>("ra").ok().flatten().unwrap_or(0) != 0,
        local_size_all: row.try_get("local_size_all").unwrap_or(None),
        local_size_top: row.try_get("local_size_top").unwrap_or(None),
        catalog_size_all: row.try_get("catalog_size_all").unwrap_or(None),
        catalog_size_top: row.try_get("catalog_size_top").unwrap_or(None),
        display_aspect: None,
        display_widescreen: None,
        display_modes: None,
        display_refresh: None,
        version_group: None,
    })
}

/// The page's titles get their first release's display values (one query for the page).
async fn fill_title_display(pool: &MySqlPool, items: &mut [TitleRow]) -> Result<()> {
    let tops: Vec<u64> = items.iter().filter_map(|t| t.top_release_id).collect();
    if tops.is_empty() {
        return Ok(());
    }
    let cols = crate::common::display::FIELDS.iter().map(|f| format!(", {} AS display_{f}", super::display::effective_sql(f, "r"))).collect::<String>();
    let ids = tops.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT r.id{cols} FROM `release` r WHERE r.id IN ({ids})"))).fetch_all(pool).await?;
    let mut by_id: HashMap<u64, [Option<String>; 4]> = HashMap::new();
    for r in &rows {
        by_id.insert(r.try_get("id")?, [r.try_get("display_aspect")?, r.try_get("display_widescreen")?, r.try_get("display_modes")?, r.try_get("display_refresh")?]);
    }
    for t in items.iter_mut() {
        if let Some([a, w, m, f]) = t.top_release_id.and_then(|id| by_id.get(&id)).cloned() {
            (t.display_aspect, t.display_widescreen, t.display_modes, t.display_refresh) = (a, w, m, f);
        }
    }
    Ok(())
}

/// How titles are named (`title_display_mode`): english (the top release's romanized DAT
/// name), local (its localized name for the release's own region / language), or a
/// language priority list ("en" = the romanized name).
enum NameMode {
    English,
    Local,
    Languages(Vec<String>),
}

async fn name_mode(app: &App) -> Result<NameMode> {
    let v: Option<serde_json::Value> = db_settings::get(&app.pool, db_settings::KEY_TITLE_DISPLAY_MODE).await?;
    Ok(match v {
        Some(serde_json::Value::String(m)) if m == "local" => NameMode::Local,
        Some(serde_json::Value::Object(o)) => NameMode::Languages(o.get("languages").and_then(|l| l.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_ascii_lowercase())).collect()).unwrap_or_default()),
        _ => NameMode::English,
    })
}

/// Display names for (title, its top release): see [`TitleRow::display_name`]. Titles
/// without a release keep their own name.
pub(crate) async fn display_names(app: &App, pairs: &[(u64, Option<u64>)]) -> Result<HashMap<u64, String>> {
    let pool = &app.pool;
    let mode = name_mode(app).await?;
    let region_order = db_settings::region_priority(pool).await?;
    let release_ids: Vec<u64> = pairs.iter().filter_map(|(_, r)| *r).collect();
    let title_ids: Vec<u64> = pairs.iter().map(|(t, _)| *t).collect();
    let mut top: HashMap<u64, (String, Vec<String>, Vec<String>)> = HashMap::new();
    if !release_ids.is_empty() {
        let sql = format!("SELECT id, name, region, languages FROM `release` WHERE id IN ({})", placeholders(release_ids.len()));
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        for id in &release_ids {
            q = q.bind(*id);
        }
        for r in q.fetch_all(pool).await? {
            let split = |v: Option<String>, upper: bool| v.unwrap_or_default().split(',').map(|x| if upper { x.trim().to_ascii_uppercase() } else { x.trim().to_ascii_lowercase() }).filter(|x| !x.is_empty()).collect::<Vec<_>>();
            top.insert(r.try_get("id")?, (r.try_get("name")?, split(r.try_get("region")?, true), split(r.try_get("languages")?, false)));
        }
    }
    // (language, region, name) per title, only when a mode needs them.
    let mut locales: HashMap<u64, Vec<(String, String, String)>> = HashMap::new();
    if !matches!(mode, NameMode::English) && !title_ids.is_empty() {
        let sql = format!("SELECT title_id, language, region, name FROM title_locales WHERE title_id IN ({}) ORDER BY (source = 'user') DESC, (source = 'igdb') DESC, source", placeholders(title_ids.len()));
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        for id in &title_ids {
            q = q.bind(*id);
        }
        for r in q.fetch_all(pool).await? {
            locales.entry(r.try_get("title_id")?).or_default().push((r.try_get("language")?, r.try_get("region")?, r.try_get("name")?));
        }
    }
    let mut out = HashMap::with_capacity(pairs.len());
    for (title_id, release_id) in pairs {
        let Some((name, regions, languages)) = release_id.and_then(|r| top.get(&r)) else { continue };
        let romanized = crate::database::title_merge::base_title(name);
        let empty = Vec::new();
        let locs = locales.get(title_id).unwrap_or(&empty);
        // The release's own localized name (the release list's rule): a locale for one of its
        // regions -- the first of them in region priority, then in the release's own order --
        // else for its only language, else the English name for an English release.
        let own_local = || {
            let english = regions.iter().any(|r| matches!(r.as_str(), "US" | "EU" | "UK" | "AU" | "CA" | "WORLD")) || languages.iter().any(|l| l == "en");
            let mut ranked: Vec<&String> = regions.iter().collect();
            ranked.sort_by_key(|r| region_order.iter().position(|o| o == *r).unwrap_or(usize::MAX));
            ranked
                .iter()
                .find_map(|r| locs.iter().find(|(_, region, _)| region == *r))
                .or_else(|| locs.iter().find(|(lang, _, _)| languages.len() == 1 && languages[0] == *lang))
                .or_else(|| locs.iter().find(|(lang, region, _)| english && lang == "en" && region.is_empty()))
                .map(|(_, _, n)| n.clone())
        };
        let chosen = match &mode {
            NameMode::English => romanized,
            NameMode::Local => own_local().unwrap_or(romanized),
            NameMode::Languages(order) => order
                .iter()
                .find_map(|lang| if lang == "en" { Some(romanized.clone()) } else { locs.iter().find(|(l, _, _)| l == lang).map(|(_, _, n)| n.clone()) })
                .unwrap_or(romanized),
        };
        out.insert(*title_id, chosen);
    }
    Ok(out)
}

/// A text as an SQL literal that cannot end early: its UTF-8 bytes in hex.
fn sql_text(s: &str) -> String {
    format!("_utf8mb4 X'{}'", s.bytes().map(|b| format!("{b:02X}")).collect::<String>())
}

/// How well a title's names match a search, best first: the exact name (its own, a localized
/// or an alternative one), then a name starting with it, then the rest. Literals rather than
/// binds: an ORDER BY of the paged lists takes none (the count and the keys share the binds).
fn search_rank_sql(search: &str) -> String {
    let exact = sql_text(search);
    let prefix = sql_text(&like_pattern(search)[1..]);
    let any = |op: &str, v: &str| {
        format!("(t.name {op} {v} OR EXISTS (SELECT 1 FROM title_locales tlr WHERE tlr.title_id = t.id AND tlr.name {op} {v}) OR EXISTS (SELECT 1 FROM title_alt_names tar WHERE tar.title_id = t.id AND tar.name {op} {v}))")
    };
    format!("CASE WHEN {} THEN 0 WHEN {} THEN 1 ELSE 2 END", any("=", &exact), any("LIKE", &prefix))
}

/// The title (`t`) has a live release on one of these platforms (ids, already numbers).
fn on_platforms_sql(platforms: &[String]) -> String {
    format!("EXISTS (SELECT 1 FROM title_releases trp JOIN `release` rp ON rp.id = trp.release_id AND rp.deleted = FALSE WHERE trp.title_id = t.id AND rp.app_platform_id IN ({}))", platforms.join(","))
}

/// A title row's copy counts, as the views' HAVING reads them (`view_parts`).
const TITLE_COUNTS: &str = "COUNT(DISTINCT lg.id) AS local_games, IF(EXISTS (SELECT 1 FROM remote_sources m WHERE m.role = 'main' AND m.enabled = TRUE AND m.deleted = FALSE), COUNT(DISTINCT IF(rsrc.role = 'main', rg.id, NULL)), NULL) AS remote_games, COUNT(DISTINCT IF(rsrc.role = 'peer', rg.source_id, NULL), IF(rsrc.role = 'peer', rg.id, NULL)) AS peer_games";

/// The Titles list's `FROM ... GROUP BY ... HAVING` for a query, as [`title_list_body`] builds
/// it, with what the order needs from the filters.
struct TitleListBody {
    body: String,
    /// Platforms whose titles come first (`prefer_platforms`, `near_releases`).
    preferred: Vec<String>,
    local_first: bool,
    ra_first: Option<String>,
}

/// The ids of every title the Titles list shows for `q`'s filters, in no order and unpaged:
/// what the review queues narrow to (`api::review`).
pub async fn title_ids_matching(app: &App, q: &ListQuery) -> Result<Vec<u64>> {
    let mut binds = Vec::new();
    let list = title_list_body(app, q, &mut binds).await?;
    let sql = format!("SELECT DISTINCT x.id FROM (SELECT t.id, {TITLE_COUNTS} {}) x", list.body);
    let rows = apply(sqlx::query(sqlx::AssertSqlSafe(sql)), &binds).fetch_all(&app.pool).await?;
    Ok(rows.iter().map(|r| r.try_get::<u64, _>("id")).collect::<std::result::Result<_, _>>()?)
}

pub async fn titles(app: &App, q: &ListQuery) -> Result<Page<TitleRow>> {
    let mut binds = title_cover_binds(app).await?;
    let cover_bind_count = binds.len();
    let rank = ReleaseRank::load(app).await?;
    // A row is one bucket of a title's releases: its retail releases, its demos, one hack.
    let row_key = rank.row_key("r");
    let in_row = |alias: &str| format!(" AND {} = {row_key}", rank.row_key(alias));
    let top_sql = top_release_sql(&rank, Some(&row_key));
    let row_rank = format!("MIN({}) AS row_rank, COALESCE({row_key}, 'retail') AS row_key", rank.kind_sql("r"));
    let select = format!("{}, {row_rank}", title_select(&title_cover_sql(&rank, Some(&row_key)), &top_sql, &row_key, &in_row));
    let TitleListBody { body, preferred, local_first, ra_first } = title_list_body(app, q, &mut binds).await?;
    let display_sort = display_sorts(None);
    let mut allowed: Vec<(&str, &str)> = vec![("name", "t.name"), ("category", "t.category"), ("release_count", "release_count"), ("local_games", "local_games"), ("remote_games", "remote_games"), ("peer_games", "peer_games"), ("platforms", "platforms"), ("providers", "providers"), ("local_size_all", "local_size_all"), ("local_size_top", "local_size_top"), ("catalog_size_all", "catalog_size_all"), ("catalog_size_top", "catalog_size_top"), ("id", "t.id")];
    allowed.extend(display_sort.iter().map(|(k, e)| (*k, e.as_str())));
    // A title's rows stay together, retail first.
    let mut order = format!("{}, t.id, row_rank, row_key", order_clause(q.sort.as_deref(), q.desc, &allowed, "t.name")?);
    if local_first {
        order = format!("(COUNT(DISTINCT lg.id) > 0) DESC, {order}");
    }
    if let Some(prefix) = &ra_first {
        order = format!("{prefix}, {order}");
    }
    if let Some(s) = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty() && q.sort.as_deref().is_none_or(str::is_empty)) {
        order = format!("{}, {order}", search_rank_sql(s));
    }
    if !preferred.is_empty() {
        order = format!("{} DESC, {order}", on_platforms_sql(&preferred));
    }
    let (limit, offset) = clamp_page(q.limit, q.offset);
    // Sorting by providers needs its per-row subquery for every row anyway: single pass.
    let key_select = format!(
        "t.id, t.name, t.category, {} AS release_count, {TITLE_COUNTS}, {} AS platforms, {}, {row_rank}",
        if q.sort.as_deref() == Some("release_count") { "COUNT(DISTINCT r.id)" } else { "NULL" },
        if q.sort.as_deref() == Some("platforms") { "GROUP_CONCAT(DISTINCT p.name ORDER BY p.name SEPARATOR ', ')" } else { "NULL" },
        // A size is per-row subqueries: computed for every row only when sorted by it.
        title_size_columns(&top_sql, &in_row).iter().map(|(name, sql)| format!("{} AS {name}", if q.sort.as_deref() == Some(*name) { sql.as_str() } else { "NULL" })).collect::<Vec<_>>().join(", ")
    );
    let lite = (q.sort.as_deref() != Some("providers")).then_some(Lite { key_select: &key_select, key_col: "t.id", select_binds: cover_bind_count, sub_key: Some("row_key") });
    let mut page = paged(&app.pool, &select, lite, &body, &order, binds, limit, offset, row_to_title).await?;
    // A row of its own release (a hack, a pirate copy, a subset) goes by that release's own
    // name, never the title's localized one: those name the game it was made from.
    let (own, shared): (Vec<&TitleRow>, Vec<&TitleRow>) = page.items.iter().partition(|t| release_buckets::PER_RELEASE.contains(&t.bucket.as_str()));
    let mut names: HashMap<(u64, Option<u64>), String> = HashMap::new();
    for (title, release, name) in shared_names(app, &shared).await? {
        names.insert((title, release), name);
    }
    let own_ids: Vec<u64> = own.iter().filter_map(|t| t.top_release_id).collect();
    if !own_ids.is_empty() {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT id, name FROM `release` WHERE id IN ({})", own_ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")))).fetch_all(&app.pool).await?;
        let by_id: HashMap<u64, String> = rows.iter().map(|r| Ok((r.try_get::<u64, _>("id")?, r.try_get::<String, _>("name")?))).collect::<std::result::Result<_, sqlx::Error>>()?;
        for t in &own {
            if let Some(name) = t.top_release_id.and_then(|id| by_id.get(&id)) {
                names.insert((t.id, t.top_release_id), crate::database::title_merge::base_title(name));
            }
        }
    }
    for t in &mut page.items {
        t.display_name = names.get(&(t.id, t.top_release_id)).cloned().unwrap_or_else(|| t.name.clone());
    }
    fill_title_display(&app.pool, &mut page.items).await?;
    if q.one_per_group {
        let v = crate::database::versions::view(&app.pool).await?;
        for t in &mut page.items {
            if let Some(g) = v.group_of.get(&t.id).and_then(|k| v.groups.get(k)) {
                t.version_group = Some(VersionGroupBadge { key: g.key.clone(), hidden: g.variants.len().saturating_sub(1) as u64 });
            }
        }
    }
    Ok(page)
}

/// The display names of rows that go by their title's name ([`display_names`]), one call per
/// layer so a title listed twice (its retail and its demo row) is named after each row's top
/// release. Returns (title, top release, name).
async fn shared_names(app: &App, rows: &[&TitleRow]) -> Result<Vec<(u64, Option<u64>, String)>> {
    let mut left: Vec<(u64, Option<u64>)> = rows.iter().map(|t| (t.id, t.top_release_id)).collect();
    left.sort_unstable();
    left.dedup();
    let mut out = Vec::new();
    while !left.is_empty() {
        let mut seen = HashSet::new();
        let (layer, rest): (Vec<_>, Vec<_>) = left.into_iter().partition(|(t, _)| seen.insert(*t));
        let names = display_names(app, &layer).await?;
        out.extend(layer.iter().filter_map(|(t, r)| names.get(t).map(|n| (*t, *r, n.clone()))));
        left = rest;
    }
    Ok(out)
}

/// The Titles list's body for `q`, its binds pushed onto `binds` after any already there.
async fn title_list_body(app: &App, q: &ListQuery, binds: &mut Vec<Bind>) -> Result<TitleListBody> {
    let mut body = String::from(
        "FROM title t \
         LEFT JOIN title_releases tr ON tr.title_id = t.id \
         LEFT JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE \
         LEFT JOIN platforms p ON p.id = r.app_platform_id \
         LEFT JOIN local_release_games lrg ON lrg.release_id = r.id \
         LEFT JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE \
         LEFT JOIN remote_release_games rrg ON rrg.release_id = r.id \
         LEFT JOIN remote_game rg ON rg.source_id = rrg.source_id AND rg.id = rrg.game_id AND rg.deleted = FALSE \
         LEFT JOIN remote_sources rsrc ON rsrc.id = rg.source_id AND rsrc.enabled = TRUE AND rsrc.deleted = FALSE \
         WHERE t.deleted = FALSE",
    );
    if let Some(set_id) = q.set_id {
        // A set's titles: those of the releases it resolves to.
        body.push_str(" AND EXISTS (SELECT 1 FROM collection_set_releases csr JOIN title_releases trs ON trs.release_id = csr.release_id WHERE csr.set_id = ? AND trs.title_id = t.id)");
        binds.push(Bind::U64(set_id));
    }
    if let Some(s) = q.search.as_deref().filter(|s| !s.trim().is_empty()) {
        // The title's own name, or any of its localized / alternative names (ほげほげ伝説V,
        // "Shin Legend of Hogehoge V", an acronym, ...) -- and the same for the query's
        // other-language versions from the name dictionary.
        let (cond, pats) = crate::database::dictionary::search_condition(
            &app.pool,
            s.trim(),
            &["t.name LIKE ?", "t.norm_name LIKE ?", "EXISTS (SELECT 1 FROM title_locales tls WHERE tls.title_id = t.id AND tls.name LIKE ?)", "EXISTS (SELECT 1 FROM title_alt_names tan WHERE tan.title_id = t.id AND tan.name LIKE ?)"],
            like_pattern(s.trim()),
        )
        .await?;
        body.push_str(&format!(" AND {cond}"));
        binds.extend(pats.into_iter().map(Bind::Str));
    }
    body.push_str(&platforms_clause("r.app_platform_id", q));
    // Preferred platforms: named, and those of the releases being moved. An EXISTS rather
    // than the join's filter, so a title's platform badges stay whole.
    let mut preferred: Vec<String> = q.prefer_platforms.as_deref().unwrap_or("").split(',').filter_map(|p| p.trim().parse::<u16>().ok()).map(|p| p.to_string()).collect();
    let near: Vec<u64> = q.near_releases.as_deref().unwrap_or("").split(',').filter_map(|r| r.trim().parse().ok()).collect();
    if !near.is_empty() {
        let list = near.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        let theirs: Vec<u16> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT DISTINCT app_platform_id FROM `release` WHERE id IN ({list})"))).fetch_all(&app.pool).await?;
        if q.near_only && !theirs.is_empty() {
            body.push_str(&format!(" AND {}", on_platforms_sql(&theirs.iter().map(u16::to_string).collect::<Vec<_>>())));
        }
        preferred.extend(theirs.iter().map(u16::to_string));
    }
    if let Some(provider) = q.provider.as_deref().filter(|p| !p.is_empty()) {
        body.push_str(" AND (EXISTS (SELECT 1 FROM title_provider_links tpl WHERE tpl.title_id = t.id AND tpl.provider = ? AND tpl.deleted = FALSE) OR EXISTS (SELECT 1 FROM release_provider_links rpl JOIN title_releases tr3 ON tr3.release_id = rpl.release_id WHERE tr3.title_id = t.id AND rpl.provider = ? AND rpl.deleted = FALSE))");
        binds.push(Bind::Str(provider.to_string()));
        binds.push(Bind::Str(provider.to_string()));
    }
    push_facet_filters(&mut body, binds, q, "t.id", &db_settings::genre_source_priority(&app.pool).await?);
    if let Some(igdb) = q.igdb {
        body.push_str(" AND EXISTS (SELECT 1 FROM title_provider_links tpx WHERE tpx.title_id = t.id AND tpx.provider = 'igdb' AND tpx.provider_id = ? AND tpx.deleted = FALSE)");
        binds.push(Bind::Str(igdb.to_string()));
    }
    if let Some(id) = q.basegame {
        // The whole family, through every level (`title_family`), and the collections that
        // hold one of its games.
        body.push_str(
            " AND (COALESCE(t.family_root_id, t.id) = (SELECT COALESCE(b1.family_root_id, b1.id) FROM title b1 WHERE b1.id = ?) \
               OR t.id IN (SELECT bg.bundle_title_id FROM title_bundle_games bg JOIN title g ON g.id = bg.game_title_id WHERE COALESCE(g.family_root_id, g.id) = (SELECT COALESCE(b2.family_root_id, b2.id) FROM title b2 WHERE b2.id = ?)))",
        );
        for _ in 0..2 {
            binds.push(Bind::U64(id));
        }
    }
    let unlicensed_is_retail = release_buckets::unlicensed_joins_retail(&app.pool).await?;
    if let Some(kinds) = kinds_clause(q.kinds.as_deref(), "r", unlicensed_is_retail)? {
        body.push_str(&format!(" AND (r.id IS NULL OR {kinds})"));
    }
    // A title is what its releases are: one with only a soundtrack disc behind it is not
    // among the games, and one with no release at all is (`NULL IS NULL`).
    body.push_str(&material_clause(q.material.as_deref())?);
    if let Some(region) = q.region.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        body.push_str(" AND EXISTS (SELECT 1 FROM title_releases trr JOIN `release` rr ON rr.id = trr.release_id WHERE trr.title_id = t.id AND rr.deleted = FALSE AND FIND_IN_SET(?, REPLACE(rr.region, ' ', '')))");
        binds.push(Bind::Str(region.to_ascii_uppercase()));
    }
    if let Some(language) = q.language.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
        body.push_str(" AND EXISTS (SELECT 1 FROM title_releases trl JOIN `release` rl ON rl.id = trl.release_id WHERE trl.title_id = t.id AND rl.deleted = FALSE AND FIND_IN_SET(?, REPLACE(rl.languages, ' ', '')))");
        binds.push(Bind::Str(language.to_ascii_lowercase()));
    }
    let (display_conds, display_binds) = title_display_filters(q, "t.id")?;
    body.push_str(&display_conds);
    binds.extend(display_binds.into_iter().map(Bind::Str));
    if q.hide_minor || q.one_per_group {
        let v = crate::database::versions::view(&app.pool).await?;
        let mut hidden: Vec<u64> = Vec::new();
        if q.hide_minor {
            hidden.extend(v.minor_titles.iter().copied());
        }
        if q.one_per_group {
            hidden.extend(v.group_hidden.iter().copied());
        }
        if !hidden.is_empty() {
            hidden.sort_unstable();
            hidden.dedup();
            body.push_str(&format!(" AND t.id NOT IN ({})", hidden.iter().map(u64::to_string).collect::<Vec<_>>().join(",")));
        }
    }
    let (ra_filter, ra_first) = ra_view(q.view.as_deref(), true);
    if let Some(filter) = &ra_filter {
        body.push_str(filter);
    }
    // One row per title and bucket (`release_buckets`): a title with retail releases and a
    // demo is listed twice.
    body.push_str(&format!(" GROUP BY t.id, {}", release_buckets::row_key_sql("r", unlicensed_is_retail)));
    let (having, local_first) = view_parts(q.view.as_deref())?;
    body.push_str(having);
    Ok(TitleListBody { body, preferred, local_first, ra_first })
}

/// One row per provider page: a title's releases mostly link the same game the title does
/// (every revision and region of it, and now its DLC), and each link listed that page again.
/// The first is kept -- the title's own, else the best-ranked release's -- so a page two
/// releases reach by different ids (a RetroAchievements set per region) is still two rows.
fn dedupe_provider_links(links: &mut Vec<ProviderLink>) {
    let mut seen = std::collections::HashSet::new();
    links.retain(|l| seen.insert((l.provider.clone(), l.provider_id.clone())));
}

/// One title of a [`CrossRow`]: the game on the platform(s) that title covers.
#[derive(Debug, Clone, Serialize)]
pub struct CrossTitle {
    pub id: u64,
    pub name: String,
    pub platforms: Option<String>,
    pub releases: u64,
    pub local_games: u64,
    /// Some release of the title is RetroAchievements-supported.
    pub ra: bool,
}

/// A game as IGDB knows it, whichever platforms it is on here: every title linked to the one
/// IGDB id. The merge pass keeps a PS4 and an Xbox One title apart on purpose -- emulators
/// and RetroAchievements treat them as different sets -- so this is where they are one row.
#[derive(Debug, Clone, Serialize)]
pub struct CrossRow {
    pub igdb_id: String,
    /// IGDB's name for the game, else its first title's.
    pub name: String,
    /// Its leading title, by `platform_priority`: where the row opens when it is the only one.
    pub title_id: u64,
    pub title_count: u64,
    pub titles: Vec<CrossTitle>,
    /// Every platform of every title, by name.
    pub platforms: String,
    pub releases: u64,
    pub local_games: u64,
    /// One of its titles is RetroAchievements-supported.
    pub ra: bool,
    /// IGDB's cover for the game: stored once under the IGDB id, so it is this row's without
    /// going through any of its titles.
    pub cover_media_id: Option<u64>,
}

/// Where a platform stands in `platform_priority`: its place in the list, and after the list
/// for the rest (which then go by name).
fn platform_rank(priority: &[u16], id: u16) -> usize {
    priority.iter().position(|p| *p == id).unwrap_or(priority.len())
}

/// A row's titles and platforms in `platform_priority` order. Each title comes as
/// (id, name, its platforms as (id, name), releases, local copies); the title whose best
/// platform ranks highest leads, and within a title and across the row the platforms follow
/// the same order. Returns the titles and the row's platform names.
fn order_cross_titles(mut titles: Vec<(u64, String, Vec<(u16, String)>, u64, u64)>, priority: &[u16]) -> (Vec<CrossTitle>, String) {
    let key = |p: &(u16, String)| (platform_rank(priority, p.0), p.1.clone());
    for title in &mut titles {
        title.2.sort_by_key(key);
    }
    titles.sort_by_key(|t| (t.2.first().map(key).unwrap_or((usize::MAX, String::new())), t.0));
    let mut all: Vec<(u16, String)> = titles.iter().flat_map(|t| t.2.iter().cloned()).collect();
    all.sort_by_key(key);
    all.dedup();
    let names = |platforms: &[(u16, String)]| platforms.iter().map(|p| p.1.as_str()).collect::<Vec<_>>().join(", ");
    let row = names(&all);
    (titles.into_iter().map(|(id, name, platforms, releases, local_games)| CrossTitle { id, name, platforms: (!platforms.is_empty()).then(|| names(&platforms)), releases, local_games, ra: false }).collect(), row)
}

/// The platforms that have a game release in an IGDB game linked to more than one title:
/// the ones `platform_priority` can make a difference for, which is all its editor offers
/// to add -- not the whole list of systems.
pub async fn crossplatform_platforms(app: &App) -> Result<Vec<u16>> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT r.app_platform_id FROM title_provider_links l JOIN title t ON t.id = l.title_id AND t.deleted = FALSE \
         JOIN title_releases tr ON tr.title_id = t.id JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE AND r.material IS NULL \
         WHERE l.provider = 'igdb' AND l.deleted = FALSE AND r.app_platform_id <> 0 AND l.provider_id IN \
         (SELECT l2.provider_id FROM title_provider_links l2 JOIN title t2 ON t2.id = l2.title_id AND t2.deleted = FALSE WHERE l2.provider = 'igdb' AND l2.deleted = FALSE GROUP BY l2.provider_id HAVING COUNT(DISTINCT l2.title_id) > 1) \
         ORDER BY r.app_platform_id",
    )
    .fetch_all(&app.pool)
    .await?)
}

/// The cross-platform list: one row per IGDB game that has a title with a game release.
/// Titles with no IGDB link are not here (nothing says what they are the same as); the
/// Titles list has them. Filters: `search` (IGDB's name or a title's), `platform` (games
/// with a title on it -- the row still shows all its platforms), `multi`. Sort: name
/// (default) | titles.
///
/// Grouped when asked, over the IGDB links alone: tens of thousands of rows where the
/// titles are a million, so there is nothing precomputed to keep in step with the merges.
pub async fn crossplatform(app: &App, q: &ListQuery) -> Result<Page<CrossRow>> {
    let pool = &app.pool;
    let game_release = "SELECT 1 FROM title_releases trx JOIN `release` rx ON rx.id = trx.release_id WHERE trx.title_id = t.id AND rx.deleted = FALSE AND rx.material IS NULL";
    let mut body = format!(
        "FROM title_provider_links l JOIN title t ON t.id = l.title_id AND t.deleted = FALSE \
         LEFT JOIN igdb_games g ON g.provider_id = CAST(l.provider_id AS UNSIGNED) AND g.deleted = FALSE \
         WHERE l.provider = 'igdb' AND l.deleted = FALSE AND l.provider_id REGEXP '^[0-9]+$' AND EXISTS ({game_release})"
    );
    let mut binds: Vec<String> = Vec::new();
    if let Some(s) = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let (cond, pats) = crate::database::dictionary::search_condition(pool, s, &["t.name LIKE ?", "g.title LIKE ?", "EXISTS (SELECT 1 FROM title_locales tls WHERE tls.title_id = t.id AND tls.name LIKE ?)"], like_pattern(s)).await?;
        body.push_str(&format!(" AND {cond}"));
        binds.extend(pats);
    }
    let (display_conds, display_binds) = title_display_filters(q, "t.id")?;
    body.push_str(&display_conds);
    binds.extend(display_binds);
    body.push_str(" GROUP BY l.provider_id");
    let mut having: Vec<String> = Vec::new();
    // A game on any of the chosen platforms -- or, asked for, on every one of them.
    let chosen = chosen_platforms(q);
    if !chosen.is_empty() {
        let on = |ids: &[u16]| format!("SUM(EXISTS ({game_release} AND rx.app_platform_id IN ({}))) > 0", ids.iter().map(u16::to_string).collect::<Vec<_>>().join(","));
        if q.platform_logic.as_deref().is_some_and(|l| l.eq_ignore_ascii_case("and")) {
            having.extend(chosen.iter().map(|g| on(g)));
        } else {
            having.push(on(&chosen.concat()));
        }
    }
    if q.multi {
        having.push("COUNT(DISTINCT t.id) > 1".to_string());
    }
    if !having.is_empty() {
        body.push_str(&format!(" HAVING {}", having.join(" AND ")));
    }
    let order = order_clause(q.sort.as_deref(), q.desc, &[("name", "name"), ("titles", "title_count")], "name")?;
    let (limit, offset) = clamp_page(q.limit, q.offset);

    let mut count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM (SELECT l.provider_id {body}) x")));
    for b in &binds {
        count = count.bind(b);
    }
    let total = count.fetch_one(pool).await?.max(0) as u64;

    let sql = format!("SELECT l.provider_id AS igdb_id, COALESCE(MAX(g.title), MIN(t.name)) AS name, COUNT(DISTINCT t.id) AS title_count, CAST(MIN(t.id) AS UNSIGNED) AS title_id {body} ORDER BY {order}, igdb_id LIMIT ? OFFSET ?");
    let mut page = sqlx::query(sqlx::AssertSqlSafe(sql));
    for b in &binds {
        page = page.bind(b);
    }
    let mut items: Vec<CrossRow> = page
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| {
            Ok(CrossRow {
                igdb_id: r.try_get("igdb_id")?,
                name: r.try_get("name")?,
                title_id: r.try_get("title_id")?,
                title_count: r.try_get::<i64, _>("title_count")?.max(0) as u64,
                titles: Vec::new(),
                platforms: String::new(),
                releases: 0,
                local_games: 0,
                ra: false,
                cover_media_id: None,
            })
        })
        .collect::<Result<_>>()?;

    // The page's games in full: each title with its platforms, releases and local copies.
    if !items.is_empty() {
        let sql = format!(
            "SELECT l.provider_id AS igdb_id, t.id, t.name, CAST(GROUP_CONCAT(DISTINCT CONCAT(r.app_platform_id, ':', COALESCE(p.name, 'Unknown')) SEPARATOR '|') AS CHAR) AS platforms, COUNT(DISTINCT r.id) AS releases, COUNT(DISTINCT lg.id) AS local_games, MAX({ra}) AS ra \
             FROM title_provider_links l JOIN title t ON t.id = l.title_id AND t.deleted = FALSE JOIN title_releases tr ON tr.title_id = t.id \
             JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE AND r.material IS NULL LEFT JOIN platforms p ON p.id = r.app_platform_id \
             LEFT JOIN local_release_games lrg ON lrg.release_id = r.id LEFT JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE \
             WHERE l.provider = 'igdb' AND l.deleted = FALSE AND l.provider_id IN ({}) GROUP BY l.provider_id, t.id, t.name ORDER BY t.id",
            placeholders(items.len()),
            ra = ra_exists!("r")
        );
        let mut detail = sqlx::query(sqlx::AssertSqlSafe(sql));
        for item in &items {
            detail = detail.bind(&item.igdb_id);
        }
        let mut found: HashMap<String, Vec<(u64, String, Vec<(u16, String)>, u64, u64)>> = HashMap::new();
        let mut ra_titles: HashSet<u64> = HashSet::new();
        for r in detail.fetch_all(pool).await? {
            if r.try_get::<Option<i64>, _>("ra")?.unwrap_or(0) != 0 {
                ra_titles.insert(r.try_get("id")?);
            }
            let platforms = r.try_get::<Option<String>, _>("platforms")?.unwrap_or_default().split('|').filter_map(|p| p.split_once(':')).filter_map(|(id, name)| Some((id.parse().ok()?, name.to_string()))).collect();
            found.entry(r.try_get("igdb_id")?).or_default().push((r.try_get("id")?, r.try_get("name")?, platforms, r.try_get::<i64, _>("releases")?.max(0) as u64, r.try_get::<i64, _>("local_games")?.max(0) as u64));
        }
        let priority = db_settings::platform_priority(pool).await?;
        for item in &mut items {
            let (mut titles, platforms) = order_cross_titles(found.remove(&item.igdb_id).unwrap_or_default(), &priority);
            for t in &mut titles {
                t.ra = ra_titles.contains(&t.id);
            }
            item.ra = titles.iter().any(|t| t.ra);
            item.releases = titles.iter().map(|t| t.releases).sum();
            item.local_games = titles.iter().map(|t| t.local_games).sum();
            // The row opens on its leading title.
            if let Some(first) = titles.first() {
                item.title_id = first.id;
            }
            item.titles = titles;
            item.platforms = platforms;
        }
        // The art the Titles grid shows for the row's titles, the leading title's first. It
        // used to look for IGDB rows of type `cover` alone -- a type IGDB art has not been
        // stored as since box fronts took its place, so the grid showed none.
        let title_ids: Vec<u64> = items.iter().flat_map(|i| i.titles.iter().map(|t| t.id)).collect();
        if !title_ids.is_empty() {
            let sql = format!("SELECT t.id, {} AS cover_media_id FROM title t WHERE t.id IN ({})", title_cover_sql(&ReleaseRank::load(app).await?, None), placeholders(title_ids.len()));
            let cover_binds = title_cover_binds(app).await?;
            let mut covers = apply(sqlx::query(sqlx::AssertSqlSafe(sql)), &cover_binds);
            for id in &title_ids {
                covers = covers.bind(id);
            }
            let mut by_title: HashMap<u64, u64> = HashMap::new();
            for r in covers.fetch_all(pool).await? {
                if let Some(media) = r.try_get::<Option<u64>, _>("cover_media_id")? {
                    by_title.insert(r.try_get("id")?, media);
                }
            }
            for item in &mut items {
                item.cover_media_id = item.titles.iter().find_map(|t| by_title.get(&t.id).copied());
            }
        }
    }
    Ok(Page { items, total, limit, offset })
}

/// Titles of a family for a title page: `cond` on title `d` (ids interpolated by the caller).
async fn family_refs(pool: &MySqlPool, cond: &str) -> Result<Vec<FamilyRef>> {
    let sql = format!("SELECT d.id, d.name, d.category, ({}) AS platforms FROM title d WHERE d.deleted = FALSE AND {cond} ORDER BY d.category, d.name LIMIT 200", TITLE_PLATFORMS_SQL.replace("= ?", "= d.id"));
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| Ok(FamilyRef { id: r.try_get("id")?, name: r.try_get("name")?, category: r.try_get("category")?, platforms: r.try_get("platforms")? }))
        .collect()
}

pub async fn title(app: &App, id: u64) -> Result<TitleDetail> {
    let pool = &app.pool;
    let row = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT t.id, t.name, t.norm_name, t.category, {TITLE_KIND_SQL} AS game_kind, t.basegame_title_id, b.name AS basegame_name, t.locales_locked, t.basegame_locked, t.family_root_id FROM title t LEFT JOIN title b ON b.id = t.basegame_title_id WHERE t.id = ? AND t.deleted = FALSE")))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| Error::not_found(format!("no title {id}")))?;
    let name: String = row.try_get("name")?;
    let genres = sqlx::query("SELECT g.name, tg.source FROM title_genres tg JOIN genre g ON g.id = tg.genre_id WHERE tg.title_id = ? ORDER BY g.name, tg.source").bind(id).fetch_all(pool).await?;
    let franchises = sqlx::query("SELECT f.name, tf.source FROM title_franchises tf JOIN franchise f ON f.id = tf.franchise_id WHERE tf.title_id = ? ORDER BY f.name, tf.source").bind(id).fetch_all(pool).await?;
    let companies = sqlx::query("SELECT c.name, tc.role, tc.source FROM title_companies tc JOIN company c ON c.id = tc.company_id WHERE tc.title_id = ? ORDER BY tc.role, c.name").bind(id).fetch_all(pool).await?;
    let locales = sqlx::query("SELECT language, region, source, name FROM title_locales WHERE title_id = ? ORDER BY language, region, (source = 'user') DESC, source").bind(id).fetch_all(pool).await?;
    let keywords = sqlx::query("SELECT k.name, tk.source FROM title_keywords tk JOIN keyword k ON k.id = tk.keyword_id WHERE tk.title_id = ? ORDER BY k.name, tk.source").bind(id).fetch_all(pool).await?;
    let perspectives = sqlx::query("SELECT p.name, tp.source FROM title_perspectives tp JOIN perspective p ON p.id = tp.perspective_id WHERE tp.title_id = ? ORDER BY p.name, tp.source").bind(id).fetch_all(pool).await?;
    let tagged = |rows: &[MySqlRow]| -> Result<Vec<Tagged>> { rows.iter().map(|r| Ok(Tagged { name: r.try_get("name")?, source: r.try_get("source")? })).collect() };
    let releases = releases(app, &ListQuery { title_id: Some(id), limit: Some(500), view: Some("collection".into()), ..Default::default() }).await?;
    let local_games = releases.items.iter().map(|r| r.local_games).sum();
    let mut provider_links = provider_links_for(pool, "title_provider_links", "title_id", id, "title", &name).await?;
    // Through the game's own releases only: a hack under this title has its own page on
    // RetroAchievements and TheGamesDB, and a game with a thousand hacks is not on a
    // thousand pages.
    for r in releases.items.iter().filter(|r| !stands_apart(r)) {
        provider_links.extend(provider_links_for(pool, "release_provider_links", "release_id", r.id, "release", &name).await?);
    }
    dedupe_provider_links(&mut provider_links);
    let cover_binds = title_cover_binds(app).await?;
    let cover_sql = format!("SELECT {} AS cover_media_id FROM title t WHERE t.id = ?", title_cover_sql(&ReleaseRank::load(app).await?, None));
    let cover_media_id: Option<u64> = apply(sqlx::query(sqlx::AssertSqlSafe(cover_sql)), &cover_binds).bind(id).fetch_one(pool).await?.try_get("cover_media_id")?;
    // Releases come in rank order here: the first is the title's top release.
    let display_name = display_names(app, &[(id, releases.items.first().map(|r| r.id))]).await?.remove(&id).unwrap_or_else(|| name.clone());
    Ok(TitleDetail {
        id: row.try_get("id")?,
        display_name,
        name,
        cover_media_id,
        norm_name: row.try_get("norm_name")?,
        category: row.try_get("category")?,
        game_kind: row.try_get("game_kind").unwrap_or(None),
        basegame_title_id: row.try_get("basegame_title_id")?,
        basegame_name: row.try_get("basegame_name")?,
        basegame_platforms: match row.try_get::<Option<u64>, _>("basegame_title_id")? {
            Some(base) => sqlx::query_scalar(TITLE_PLATFORMS_SQL).bind(base).fetch_one(pool).await?,
            None => None,
        },
        derived: sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT d.id, d.name, d.category, ({}) AS platforms FROM title d WHERE d.basegame_title_id = ? AND d.deleted = FALSE AND d.id <> ? ORDER BY d.category, d.name LIMIT 200",
            TITLE_PLATFORMS_SQL.replace("= ?", "= d.id")
        )))
        .bind(id)
        .bind(id)
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| Ok(FamilyRef { id: r.try_get("id")?, name: r.try_get("name")?, category: r.try_get("category")?, platforms: r.try_get("platforms")? }))
        .collect::<Result<_>>()?,
        genres: tagged(&genres)?,
        franchises: tagged(&franchises)?,
        keywords: tagged(&keywords)?,
        perspectives: tagged(&perspectives)?,
        companies: companies.iter().map(|r| Ok(CompanyTag { name: r.try_get("name")?, role: r.try_get("role")?, source: r.try_get("source")? })).collect::<Result<_>>()?,
        locales: locales.iter().map(|r| Ok(Locale { language: r.try_get("language")?, region: r.try_get("region")?, source: r.try_get("source")?, name: r.try_get("name")? })).collect::<Result<_>>()?,
        locales_locked: row.try_get("locales_locked")?,
        basegame_locked: row.try_get("basegame_locked")?,
        siblings: family_refs(pool, &format!("d.basegame_title_id = (SELECT b.basegame_title_id FROM title b WHERE b.id = {id}) AND d.id <> {id}")).await?,
        bundle_games: family_refs(pool, &format!("d.id IN (SELECT game_title_id FROM title_bundle_games WHERE bundle_title_id = {id})")).await?,
        in_bundles: family_refs(pool, &format!("d.id IN (SELECT bundle_title_id FROM title_bundle_games WHERE game_title_id = {id})")).await?,
        family_root_id: row.try_get::<Option<u64>, _>("family_root_id")?.unwrap_or(id),
        alt_names: sqlx::query("SELECT name, source, comment FROM title_alt_names WHERE title_id = ? ORDER BY comment IS NULL, comment, name").bind(id).fetch_all(pool).await?.iter().map(|r| Ok(AltName { name: r.try_get("name")?, source: r.try_get("source")?, comment: r.try_get("comment")? })).collect::<Result<_>>()?,
        display: match releases.items.iter().find(|r| r.material.is_none()) {
            Some(top) => super::display::values(app, top.id, top.platform_id, top.region.as_deref()).await?,
            None => Vec::new(),
        },
        provider_links,
        media: title_media(pool, id).await?,
        releases_total: releases.total,
        releases: releases.items,
        linked: super::bases::made_from_title_elsewhere(pool, id).await?,
        local_games,
        manual_order: sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM title_releases WHERE title_id = ? AND sort_weight IS NOT NULL").bind(id).fetch_one(pool).await? > 0,
        ra: sqlx::query_scalar::<_, i64>(concat!("SELECT COUNT(*) FROM title_releases tra JOIN `release` rr ON rr.id = tra.release_id AND rr.deleted = FALSE WHERE tra.title_id = ? AND ", ra_exists!("rr")))
            .bind(id)
            .fetch_one(pool)
            .await?
            > 0,
    })
}

// ---------------------------------------------------------------------------
// releases
// ---------------------------------------------------------------------------

// `local_title`: a release with an IGDB game of its own other than its title's (a title
// combined from several games) takes no name from the title's game.
//
// `local_verified` asks with IN, not EXISTS: an EXISTS naming both this release and the file
// of the enclosing subquery is rewritten by MariaDB 13.2 into a two-column IN, and preparing
// that crashed the server now and then (Item_row::fix_fields under Item_in_optimizer).
const RELEASE_SELECT_BASE: &str = concat!("r.id, r.name, r.app_platform_id AS platform_id, MAX(p.name) AS platform, r.region, r.languages, r.year, r.revision, r.serial, r.category, r.material, r.game_kind, r.languages_state, (SELECT rbs.status FROM release_bases rbs WHERE rbs.release_id = r.id) AS base_status, r.disc_count, \
    r.is_budget, r.is_special_edition, r.is_aftermarket, r.is_rerelease, \
    (SELECT GROUP_CONCAT(s2.name ORDER BY s2.name SEPARATOR ',') FROM release_sources rs2 JOIN sources s2 ON s2.id = rs2.source_id WHERE rs2.release_id = r.id) AS source, MAX(t.id) AS title_id, MAX(t.name) AS title_name, COUNT(DISTINCT lg.id) AS local_games, IF(EXISTS (SELECT 1 FROM remote_sources m WHERE m.role = 'main' AND m.enabled = TRUE AND m.deleted = FALSE), COUNT(DISTINCT IF(rsrc.role = 'main', rg.id, NULL)), NULL) AS remote_games, COUNT(DISTINCT IF(rsrc.role = 'peer', rg.source_id, NULL), IF(rsrc.role = 'peer', rg.id, NULL)) AS peer_games, (SELECT COUNT(DISTINCT rc.content_id) FROM release_contents rc WHERE rc.release_id = r.id) AS content_count, \
    (SELECT m.id FROM release_media rm JOIN media m ON m.id = rm.media_id WHERE rm.release_id = r.id AND FIND_IN_SET(m.media_type, ?) AND m.deleted = FALSE ORDER BY ", media_order!(), " LIMIT 1) AS cover_media_id, \
    (SELECT COUNT(DISTINCT f5.content_id) FROM local_release_games lrg5 JOIN local_game_roms lgr5 ON lgr5.game_id = lrg5.game_id JOIN local_rom_files lrf5 ON lrf5.rom_id = lgr5.rom_id JOIN local_files f5 ON f5.id = lrf5.file_id WHERE lrg5.release_id = r.id \
       AND (f5.content_id IN (SELECT rc5.content_id FROM release_contents rc5 WHERE rc5.release_id = r.id) \
         OR f5.content_id IN (SELECT cc5.parent_content_id FROM content_contents cc5 JOIN release_contents rc5m ON rc5m.content_id = cc5.child_content_id WHERE rc5m.release_id = r.id) \
         OR EXISTS (SELECT 1 FROM hasheous_lookups hl WHERE hl.content_id = f5.content_id AND hl.hit = TRUE AND hl.deleted = FALSE))) AS local_verified, \
    (SELECT tl.name FROM title_releases trl JOIN title_locales tl ON tl.title_id = trl.title_id WHERE trl.release_id = r.id \
       AND (r.game_kind IS NULL OR r.game_kind NOT IN ('hack', 'mod', 'translation')) AND (r.material IS NULL OR r.material NOT IN ('patch', 'trainer', 'save', 'mappack', 'tool')) \
       AND COALESCE((SELECT rlo.provider_id FROM release_provider_links rlo WHERE rlo.release_id = r.id AND rlo.provider = 'igdb' AND rlo.deleted = FALSE), '') \
         IN ('', COALESCE((SELECT tlo.provider_id FROM title_provider_links tlo WHERE tlo.title_id = trl.title_id AND tlo.provider = 'igdb' AND tlo.deleted = FALSE), \
                          (SELECT rlo2.provider_id FROM release_provider_links rlo2 WHERE rlo2.release_id = r.id AND rlo2.provider = 'igdb' AND rlo2.deleted = FALSE))) \
       AND ((tl.region <> '' AND FIND_IN_SET(tl.region, REPLACE(COALESCE(r.region, ''), ' ', ''))) OR tl.language = REPLACE(COALESCE(r.languages, ''), ' ', '') \
            OR (tl.language = 'en' AND tl.region = '' AND (REPLACE(COALESCE(r.region, ''), ' ', '') REGEXP '(^|,)(US|EU|UK|AU|CA|WORLD)(,|$)' OR FIND_IN_SET('en', REPLACE(COALESCE(r.languages, ''), ' ', ''))))) \
       ORDER BY (tl.region <> '' AND FIND_IN_SET(tl.region, REPLACE(COALESCE(r.region, ''), ' ', ''))) DESC, /*REGION_ORDER*/FIND_IN_SET(tl.region, REPLACE(COALESCE(r.region, ''), ' ', '')), (tl.language = REPLACE(COALESCE(r.languages, ''), ' ', '')) DESC, \
         (tl.source = 'user') DESC, (tl.source = 'igdb') DESC, tl.source LIMIT 1) AS local_title, \
    (SELECT COUNT(DISTINCT f6.id) FROM local_release_games lrg6 JOIN local_game_roms lgr6 ON lgr6.game_id = lrg6.game_id JOIN local_rom_files lrf6 ON lrf6.rom_id = lgr6.rom_id JOIN local_files f6 ON f6.id = lrf6.file_id WHERE lrg6.release_id = r.id AND f6.deleted = TRUE) AS local_missing");
const RELEASE_BODY: &str = "FROM `release` r \
    LEFT JOIN platforms p ON p.id = r.app_platform_id \
    LEFT JOIN sources s ON s.id = r.source_id \
    LEFT JOIN title_releases tr ON tr.release_id = r.id \
    LEFT JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE \
    LEFT JOIN local_release_games lrg ON lrg.release_id = r.id \
    LEFT JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE \
    LEFT JOIN remote_release_games rrg ON rrg.release_id = r.id \
    LEFT JOIN remote_game rg ON rg.source_id = rrg.source_id AND rg.id = rrg.game_id AND rg.deleted = FALSE \
    LEFT JOIN remote_sources rsrc ON rsrc.id = rg.source_id AND rsrc.enabled = TRUE AND rsrc.deleted = FALSE";
/// Grouped by the primary key alone: every other `r.` column depends on it, and a
/// multi-column GROUP BY makes MariaDB sort all 1.3M rows by those columns (5x slower).
const RELEASE_GROUP: &str = " GROUP BY r.id";

/// [`RELEASE_SELECT_BASE`] with the localized name of a release with several regions taken
/// by `region_priority`: a release for Japan and Korea showed whichever locale was stored
/// first, the Korean name as often as not.
async fn release_select(app: &App) -> Result<String> {
    let order = db_settings::region_priority(&app.pool).await?;
    // Region codes from the setting go into the statement as text: letters and digits only.
    let list = order.iter().filter(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_alphanumeric())).map(|c| format!("'{c}'")).collect::<Vec<_>>().join(", ");
    let rank = if list.is_empty() { String::new() } else { format!("FIELD(tl.region, {list}) = 0, FIELD(tl.region, {list}), ") };
    let display = crate::common::display::FIELDS.iter().map(|f| format!(", {} AS display_{f}", super::display::effective_sql(f, "r"))).collect::<String>();
    Ok(format!("{}, {} AS ra, {} AS local_size, {} AS catalog_size{display}", RELEASE_SELECT_BASE.replace("/*REGION_ORDER*/", &rank), ra_exists!("r"), local_size_sql("= r.id"), catalog_size_sql("r.id", "z")))
}

fn row_to_release(row: &MySqlRow) -> Result<ReleaseRow> {
    let mut r = row_to_release_raw(row)?;
    r.display_name = r.local_title.clone().unwrap_or_else(|| crate::database::matching::name_parse::strip_region_lang_revision(&r.name));
    Ok(r)
}

fn row_to_release_raw(row: &MySqlRow) -> Result<ReleaseRow> {
    let local_games: i64 = row.try_get("local_games")?;
    let remote_games: Option<i64> = row.try_get("remote_games")?;
    let peer_games: i64 = row.try_get("peer_games")?;
    let content_count: i64 = row.try_get("content_count")?;
    Ok(ReleaseRow {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        platform_id: row.try_get("platform_id")?,
        platform: row.try_get("platform")?,
        region: row.try_get("region")?,
        languages: row.try_get("languages")?,
        year: row.try_get("year")?,
        revision: row.try_get("revision")?,
        serial: row.try_get("serial")?,
        category: row.try_get("category")?,
        material: row.try_get("material")?,
        game_kind: row.try_get("game_kind").unwrap_or(None),
        languages_state: row.try_get("languages_state").unwrap_or(None),
        base_status: row.try_get("base_status").unwrap_or(None),
        disc_count: row.try_get("disc_count")?,
        source: row.try_get("source")?,
        title_id: row.try_get("title_id")?,
        title_name: row.try_get("title_name")?,
        local_games: local_games as u64,
        remote_games: remote_games.map(|n| n as u64),
        peer_games: peer_games as u64,
        content_count: content_count as u64,
        cover_media_id: row.try_get("cover_media_id")?,
        local_verified: row.try_get::<i64, _>("local_verified").unwrap_or(0) as u64,
        local_missing: row.try_get::<i64, _>("local_missing").unwrap_or(0) as u64,
        set_position: row.try_get("set_position")?,
        set_via: row.try_get("set_via").unwrap_or(None),
        set_title_id: row.try_get("set_title_id").unwrap_or(None),
        set_row_key: row.try_get::<Option<String>, _>("set_row_key").unwrap_or(None).filter(|k| !k.is_empty()),
        set_igdb_id: row.try_get("set_igdb_id").unwrap_or(None),
        set_family_title_id: row.try_get("set_family_title_id").unwrap_or(None),
        is_budget: row.try_get("is_budget").unwrap_or(false),
        is_special_edition: row.try_get("is_special_edition").unwrap_or(false),
        is_aftermarket: row.try_get("is_aftermarket").unwrap_or(false),
        is_rerelease: row.try_get("is_rerelease").unwrap_or(false),
        local_size: row.try_get("local_size").unwrap_or(None),
        catalog_size: row.try_get("catalog_size").unwrap_or(None),
        local_title: row.try_get("local_title").unwrap_or(None),
        display_name: String::new(),
        ra: row.try_get::<Option<i64>, _>("ra").ok().flatten().unwrap_or(0) != 0,
        display_aspect: row.try_get("display_aspect").unwrap_or(None),
        display_widescreen: row.try_get("display_widescreen").unwrap_or(None),
        display_modes: row.try_get("display_modes").unwrap_or(None),
        display_refresh: row.try_get("display_refresh").unwrap_or(None),
    })
}

/// The display filters of `q` on the release aliased `alias` (` AND ...` each).
fn display_filters(q: &ListQuery, alias: &str) -> Result<(String, Vec<String>)> {
    super::display::filter_sql(alias, q.display_aspect.as_deref(), q.display_widescreen.as_deref(), q.display_refresh.as_deref(), q.display_res.as_deref(), q.display_scan.as_deref(), q.display_state.as_deref())
}

/// The same for a title: some game release of its passes them; empty when none is asked.
pub(crate) fn title_display_filters(q: &ListQuery, title_expr: &str) -> Result<(String, Vec<String>)> {
    let (conds, binds) = display_filters(q, "rdf")?;
    if conds.is_empty() {
        return Ok((conds, binds));
    }
    Ok((format!(" AND EXISTS (SELECT 1 FROM title_releases trdf JOIN `release` rdf ON rdf.id = trdf.release_id WHERE trdf.title_id = {title_expr} AND rdf.deleted = FALSE AND rdf.material IS NULL{conds})"), binds))
}

/// Sort keys by display field: a release's value; a title's lowest over its game releases.
fn display_sorts(release_alias: Option<&str>) -> Vec<(&'static str, String)> {
    super::display::FIELDS_SORT
        .iter()
        .map(|(key, field)| {
            let expr = match release_alias {
                Some(a) => super::display::sort_sql(field, a),
                None => format!("(SELECT MIN({}) FROM title_releases trds JOIN `release` rds ON rds.id = trds.release_id WHERE trds.title_id = t.id AND rds.deleted = FALSE AND rds.material IS NULL)", super::display::sort_sql(field, "rds")),
            };
            (*key, expr)
        })
        .collect()
}

pub async fn releases(app: &App, q: &ListQuery) -> Result<Page<ReleaseRow>> {
    let base = release_select(app).await?;
    let select = if q.set_id.is_some() {
        format!("{base}, MAX(csr.position) AS set_position, MAX(CASE WHEN cse.title_id IS NOT NULL THEN 'title' WHEN cse.igdb_id IS NOT NULL THEN 'game' WHEN cse.family_title_id IS NOT NULL THEN 'family' WHEN cse.id IS NOT NULL THEN 'release' END) AS set_via, MAX(cse.title_id) AS set_title_id, MAX(cse.row_key) AS set_row_key, MAX(cse.igdb_id) AS set_igdb_id, MAX(cse.family_title_id) AS set_family_title_id")
    } else {
        format!("{base}, NULL AS set_position")
    };
    let mut body = String::from(RELEASE_BODY);
    let mut binds = vec![Bind::Str(grid_media_type(app).await?)];
    if let Some(set_id) = q.set_id {
        body.push_str(" JOIN collection_set_releases csr ON csr.release_id = r.id AND csr.set_id = ? LEFT JOIN collection_set_entries cse ON cse.id = csr.entry_id");
        binds.push(Bind::U64(set_id));
    }
    body.push_str(" WHERE r.deleted = FALSE");
    if let Some(s) = q.search.as_deref().filter(|s| !s.trim().is_empty()) {
        // The release's own name / serial, or a localized or alternative name of its title
        // (ほげほげ伝説 finds "Shin Legend of Hogehoge (Japan)").
        let (cond, pats) = crate::database::dictionary::search_condition(
            &app.pool,
            s.trim(),
            &[
                "r.name LIKE ?",
                "r.norm_name LIKE ?",
                "r.serial LIKE ?",
                "EXISTS (SELECT 1 FROM title_releases trs JOIN title_locales tls ON tls.title_id = trs.title_id WHERE trs.release_id = r.id AND tls.name LIKE ?)",
                "EXISTS (SELECT 1 FROM title_releases tra JOIN title_alt_names tan ON tan.title_id = tra.title_id WHERE tra.release_id = r.id AND tan.name LIKE ?)",
            ],
            like_pattern(s.trim()),
        )
        .await?;
        body.push_str(&format!(" AND {cond}"));
        binds.extend(pats.into_iter().map(Bind::Str));
    }
    body.push_str(&platforms_clause("r.app_platform_id", q));
    if let Some(source) = q.source.as_deref().filter(|s| !s.is_empty()) {
        body.push_str(" AND EXISTS (SELECT 1 FROM release_sources rsf JOIN sources sf ON sf.id = rsf.source_id WHERE rsf.release_id = r.id AND sf.name = ?)");
        binds.push(Bind::Str(source.to_string()));
    }
    if let Some(title_id) = q.title_id {
        body.push_str(" AND tr.title_id = ?");
        binds.push(Bind::U64(title_id));
    }
    push_facet_filters(&mut body, &mut binds, q, "t.id", &db_settings::genre_source_priority(&app.pool).await?);
    if let Some(kinds) = kinds_clause(q.kinds.as_deref(), "r", release_buckets::unlicensed_joins_retail(&app.pool).await?)? {
        body.push_str(&format!(" AND {kinds}"));
    }
    if q.material.is_some() || (q.title_id.is_none() && q.set_id.is_none()) {
        body.push_str(&material_clause(q.material.as_deref())?);
    }
    if let Some(platform) = q.addons_for {
        // The add-on's DAT may name no platform (3DS DLC comes that way): 0 goes with any.
        body.push_str(&format!(" AND r.material IN ({}) AND r.app_platform_id IN (0, {platform})", crate::database::platform_map::game_extras_sql()));
    }
    if let Some(region) = q.region.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        body.push_str(" AND FIND_IN_SET(?, REPLACE(r.region, ' ', ''))");
        binds.push(Bind::Str(region.to_ascii_uppercase()));
    }
    if let Some(language) = q.language.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
        body.push_str(" AND FIND_IN_SET(?, REPLACE(r.languages, ' ', ''))");
        binds.push(Bind::Str(language.to_ascii_lowercase()));
    }
    let (display_conds, display_binds) = display_filters(q, "r")?;
    body.push_str(&display_conds);
    binds.extend(display_binds.into_iter().map(Bind::Str));
    let (ra_filter, ra_first) = ra_view(q.view.as_deref(), false);
    if let Some(filter) = &ra_filter {
        body.push_str(filter);
    }
    body.push_str(RELEASE_GROUP);
    let (having, local_first) = view_parts(q.view.as_deref())?;
    body.push_str(having);
    // Within a title (and on request anywhere) releases come in rank order: RA-compatible,
    // then region priority, then language priority.
    let ranking = ReleaseRank::load(app).await?;
    let rank = ranking.sql("r");
    let sort = match q.sort.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => Some(s.to_string()),
        None if q.title_id.is_some() && q.set_id.is_none() => Some("rank".to_string()),
        None => None,
    };
    let display_sort = display_sorts(Some("r"));
    let mut allowed: Vec<(&str, &str)> = vec![("name", "r.name"), ("platform", "platform"), ("region", "r.region"), ("languages", "r.languages"), ("year", "r.year"), ("serial", "r.serial"), ("source", "source"), ("category", "r.category"), ("disc_count", "r.disc_count"), ("local_games", "local_games"), ("remote_games", "remote_games"), ("peer_games", "peer_games"), ("content_count", "content_count"), ("title", "title_name"), ("position", "set_position"), ("local_size", "local_size"), ("catalog_size", "catalog_size"), ("rank", rank.as_str()), ("id", "r.id")];
    allowed.extend(display_sort.iter().map(|(k, e)| (*k, e.as_str())));
    let mut order = order_clause(sort.as_deref(), q.desc, &allowed, "r.name")?;
    // The default order groups the releases by bucket (see ReleaseRank::kind_sql).
    if sort.is_none() && q.set_id.is_none() {
        order = format!("{}, {order}", ranking.kind_sql("r"));
    }
    if local_first && q.set_id.is_none() {
        order = format!("(COUNT(DISTINCT lg.id) > 0) DESC, {order}");
    }
    if let Some(prefix) = ra_first.as_ref().filter(|_| q.set_id.is_none()) {
        order = format!("{prefix}, {order}");
    }
    let (limit, offset) = clamp_page(q.limit, q.offset);
    // Only what this sort needs: content_count is a per-row subquery, so it's computed for
    // every row only when the list is sorted by it.
    let key_select = format!(
        "r.id, r.name, r.app_platform_id, r.region, r.languages, r.year, r.revision, r.serial, r.category, r.disc_count, MAX(p.name) AS platform, MAX(s.name) AS source, \
         MAX(t.name) AS title_name, COUNT(DISTINCT lg.id) AS local_games, IF(EXISTS (SELECT 1 FROM remote_sources m WHERE m.role = 'main' AND m.enabled = TRUE AND m.deleted = FALSE), COUNT(DISTINCT IF(rsrc.role = 'main', rg.id, NULL)), NULL) AS remote_games, COUNT(DISTINCT IF(rsrc.role = 'peer', rg.source_id, NULL), IF(rsrc.role = 'peer', rg.id, NULL)) AS peer_games, {} AS content_count, {} AS set_position, {} AS local_size, {} AS catalog_size",
        if q.sort.as_deref() == Some("content_count") { "(SELECT COUNT(DISTINCT rc.content_id) FROM release_contents rc WHERE rc.release_id = r.id)" } else { "NULL" },
        if q.set_id.is_some() { "MAX(csr.position)" } else { "NULL" },
        if q.sort.as_deref() == Some("local_size") { local_size_sql("= r.id") } else { "NULL".to_string() },
        if q.sort.as_deref() == Some("catalog_size") { catalog_size_sql("r.id", "z") } else { "NULL".to_string() }
    );
    let lite = Some(Lite { key_select: &key_select, key_col: "r.id", select_binds: 1, sub_key: None });
    paged(&app.pool, &select, lite, &body, &order, binds, limit, offset, row_to_release).await
}

async fn files_for_rom(pool: &MySqlPool, rom_id: u64) -> Result<Vec<FileRow>> {
    let sql = format!("SELECT {FILE_SELECT} FROM local_rom_files lrf JOIN local_files f ON f.id = lrf.file_id {FILE_JOINS} WHERE lrf.rom_id = ? ORDER BY f.path");
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(rom_id).fetch_all(pool).await?;
    rows.iter().map(row_to_file).collect()
}

async fn file_by_id(pool: &MySqlPool, id: u64) -> Result<Option<FileRow>> {
    let sql = format!("SELECT {FILE_SELECT} FROM local_files f {FILE_JOINS} WHERE f.id = ?");
    let row = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(id).fetch_optional(pool).await?;
    row.as_ref().map(row_to_file).transpose()
}

/// The order of one release's local copies: the user's, where they set one (those first, by
/// position); then complete before incomplete, then the priority of the directory they are in
/// (`directories::priority_keys`), then id. `complete` is what each copy is known to be (None:
/// not judged -- then it ranks with the complete ones, as the copy job's own check decides).
pub fn order_local_games(games: &mut [(u64, Option<bool>, Option<u64>, Option<i64>)], keys: &HashMap<u64, Vec<i64>>) {
    games.sort_by(|a, b| {
        let rank = |g: &(u64, Option<bool>, Option<u64>, Option<i64>)| (g.3.is_none(), g.3.unwrap_or(0), g.1 == Some(false), g.2.and_then(|d| keys.get(&d)).cloned().unwrap_or_else(|| vec![i64::MAX]), g.0);
        rank(a).cmp(&rank(b))
    });
}

/// Each local copy's directory (of its first file) and manual position for `release_id`.
pub async fn local_game_places(pool: &MySqlPool, release_id: u64) -> Result<HashMap<u64, (Option<u64>, Option<i64>)>> {
    let rows = sqlx::query(
        "SELECT lrg.game_id, (SELECT MIN(f.directory_id) FROM local_game_roms lgr JOIN local_rom_files lrf ON lrf.rom_id = lgr.rom_id JOIN local_files f ON f.id = lrf.file_id WHERE lgr.game_id = lrg.game_id) AS directory_id, \
           (SELECT o.position FROM local_game_order o WHERE o.release_id = lrg.release_id AND o.game_id = lrg.game_id) AS position \
         FROM local_release_games lrg WHERE lrg.release_id = ?",
    )
    .bind(release_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(|r| Ok((r.try_get("game_id")?, (r.try_get("directory_id")?, r.try_get::<Option<i32>, _>("position")?.map(i64::from))))).collect()
}

async fn local_games_for_release(pool: &MySqlPool, release_id: u64) -> Result<Vec<LocalGameDetail>> {
    let games = sqlx::query("SELECT lg.id, lg.name, lg.region, lg.category, lg.playlist_file_id, CAST(lrg.serial_match AS CHAR) AS serial_match, lrg.serial_matched FROM local_release_games lrg JOIN local_game lg ON lg.id = lrg.game_id WHERE lrg.release_id = ? AND lg.deleted = FALSE ORDER BY lg.id")
        .bind(release_id)
        .fetch_all(pool)
        .await?;
    let mut out = Vec::new();
    for g in &games {
        let game_id: u64 = g.try_get("id")?;
        let playlist_file_id: Option<u64> = g.try_get("playlist_file_id")?;
        let rom_rows = sqlx::query("SELECT lr.id, lr.disc_number, lr.main_path, lr.app_platform_id, p.name AS platform, lr.alt, lr.deleted FROM local_game_roms lgr JOIN local_rom lr ON lr.id = lgr.rom_id LEFT JOIN platforms p ON p.id = lr.app_platform_id WHERE lgr.game_id = ? ORDER BY lr.disc_number, lr.id")
            .bind(game_id)
            .fetch_all(pool)
            .await?;
        let mut roms = Vec::new();
        let mut all_files: Vec<FileRow> = Vec::new();
        for r in &rom_rows {
            let rom_id: u64 = r.try_get("id")?;
            let files = files_for_rom(pool, rom_id).await?;
            all_files.extend(files.iter().cloned());
            roms.push(RomDetail { id: rom_id, disc_number: r.try_get("disc_number")?, main_path: r.try_get("main_path")?, platform_id: r.try_get("app_platform_id")?, platform: r.try_get("platform")?, alt: r.try_get("alt")?, deleted: r.try_get("deleted")?, files });
        }
        let mut playlist = match playlist_file_id {
            Some(id) => file_by_id(pool, id).await?,
            None => None,
        };
        if let Some(p) = &playlist {
            all_files.push(p.clone());
        }
        decorate_files(pool, &mut all_files).await?;
        let by_id: HashMap<u64, FileRow> = all_files.into_iter().map(|f| (f.id, f)).collect();
        for rom in &mut roms {
            for f in &mut rom.files {
                if let Some(d) = by_id.get(&f.id) {
                    *f = d.clone();
                }
            }
        }
        if let Some(p) = &mut playlist {
            if let Some(d) = by_id.get(&p.id) {
                *p = d.clone();
            }
        }
        let missing_files = roms.iter().flat_map(|r| r.files.iter()).filter(|f| f.deleted).count() as u64;
        out.push(LocalGameDetail {
            id: game_id,
            name: g.try_get("name")?,
            region: g.try_get("region")?,
            category: g.try_get("category")?,
            playlist,
            roms,
            complete: None,
            missing_files,
            contents_required: 0,
            contents_present: 0,
            directory_id: None,
            directory_path: None,
            manual_position: None,
            serial_match: g.try_get("serial_match")?,
            serial_matched: g.try_get("serial_matched")?,
        });
    }
    // Complete or not, the way the Games list judges it.
    let mut rows: Vec<GameRow> = out.iter().map(|g| GameRow { id: g.id, release_id: Some(release_id), missing_files: g.missing_files, ..Default::default() }).collect();
    decorate_games(pool, &mut rows).await?;
    let places = local_game_places(pool, release_id).await?;
    let dirs: HashMap<u64, String> = sqlx::query("SELECT id, path FROM local_directories").fetch_all(pool).await?.iter().map(|r| Ok((r.try_get("id")?, r.try_get("path")?))).collect::<Result<_>>()?;
    for g in &mut out {
        if let Some(row) = rows.iter().find(|r| r.id == g.id) {
            g.complete = row.complete;
            g.contents_required = row.contents_required;
            g.contents_present = row.contents_present;
        }
        if let Some((dir, position)) = places.get(&g.id) {
            g.directory_id = *dir;
            g.directory_path = dir.and_then(|d| dirs.get(&d).cloned());
            g.manual_position = *position;
        }
    }
    let keys = crate::database::directories::priority_keys(pool).await?;
    let mut order: Vec<(u64, Option<bool>, Option<u64>, Option<i64>)> = out.iter().map(|g| (g.id, g.complete, g.directory_id, g.manual_position)).collect();
    order_local_games(&mut order, &keys);
    out.sort_by_key(|g| order.iter().position(|o| o.0 == g.id));
    Ok(out)
}

#[derive(Debug, Clone, Deserialize)]
pub struct LocalGameMove {
    pub game_id: u64,
    /// Put it after this copy; None: first.
    pub after_game_id: Option<u64>,
}

/// Puts one local copy of a release where the user dropped it: the release's copies get an
/// order of their own (the current one, with this copy moved).
pub async fn move_local_game(app: &App, release_id: u64, req: &LocalGameMove) -> Result<()> {
    let pool = &app.pool;
    let mut ids: Vec<u64> = local_games_for_release(pool, release_id).await?.into_iter().map(|g| g.id).collect();
    let Some(at) = ids.iter().position(|g| *g == req.game_id) else { return Err(Error::not_found(format!("game {} is not a copy of release {release_id}", req.game_id))) };
    let moving = ids.remove(at);
    let to = match req.after_game_id {
        None => 0,
        Some(after) => ids.iter().position(|g| *g == after).map(|p| p + 1).ok_or_else(|| Error::not_found(format!("game {after} is not a copy of release {release_id}")))?,
    };
    ids.insert(to, moving);
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM local_game_order WHERE release_id = ?").bind(release_id).execute(&mut *tx).await?;
    for (n, id) in ids.iter().enumerate() {
        sqlx::query("INSERT INTO local_game_order (release_id, game_id, position) VALUES (?, ?, ?)").bind(release_id).bind(id).bind(n as i64 + 1).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Back to the default order of a release's copies.
pub async fn reset_local_game_order(app: &App, release_id: u64) -> Result<u64> {
    Ok(sqlx::query("DELETE FROM local_game_order WHERE release_id = ?").bind(release_id).execute(&app.pool).await?.rows_affected())
}

pub async fn release(app: &App, id: u64) -> Result<ReleaseDetail> {
    let pool = &app.pool;
    let base = release_select(app).await?;
    let sql = format!("SELECT {base}, NULL AS set_position, MAX(r.released_on) AS released_on, MAX(r.min_players) AS min_players, MAX(r.max_players) AS max_players, MAX(r.is_beta) AS is_beta, MAX(r.is_prototype) AS is_prototype, MAX(r.is_demo) AS is_demo, MAX(r.is_unlicensed) AS is_unlicensed, MAX(r.community_patched) AS community_patched, MAX(r.patch_type) AS patch_type {RELEASE_BODY} WHERE r.deleted = FALSE AND r.id = ?{RELEASE_GROUP}");
    let row = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(grid_media_type(app).await?).bind(id).fetch_optional(pool).await?.ok_or_else(|| Error::not_found(format!("no release {id}")))?;
    let release = row_to_release(&row)?;
    let content_rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT {CONTENT_SELECT}, rc.file_name AS release_file_name, rc.variant AS variant, EXISTS (SELECT 1 FROM local_files lfh WHERE lfh.content_id = c.id AND lfh.deleted = FALSE) AS held, ds.name AS dump_source, (SELECT GROUP_CONCAT(DISTINCT s9.name ORDER BY s9.name SEPARATOR ',') FROM release_content_sources rcs JOIN sources s9 ON s9.id = rcs.source_id WHERE rcs.release_id = rc.release_id AND rcs.content_id = rc.content_id) AS dump_sources FROM release_contents rc JOIN content c ON c.id = rc.content_id LEFT JOIN platforms p ON p.id = c.app_platform_id LEFT JOIN sources ds ON ds.id = rc.source_id WHERE rc.release_id = ? ORDER BY ds.name, rc.file_name")))
        .bind(id)
        .fetch_all(pool)
        .await?;
    let mut contents = Vec::with_capacity(content_rows.len());
    for r in &content_rows {
        let mut c = row_to_content(r)?;
        if c.file_name.is_none() {
            c.file_name = r.try_get("release_file_name")?;
        }
        c.dump_source = r.try_get("dump_source")?;
        c.variant = r.try_get("variant").unwrap_or(None);
        c.held = r.try_get::<i64, _>("held").unwrap_or(0) != 0;
        // A catalogue restored from before 0033 has no rows there yet: the one known source
        // stands in.
        c.dump_sources = r.try_get::<Option<String>, _>("dump_sources")?.or_else(|| c.dump_source.clone());
        contents.push(c);
    }
    decorate_contents(pool, &mut contents).await?;
    let superseded_rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {CONTENT_SELECT}, x.file_name AS release_file_name, x.variant AS variant, x.superseded_at, CONCAT_WS(' ', x.dat_name, x.dat_version) AS superseded_by, ds.name AS dump_source, \
         EXISTS (SELECT 1 FROM local_files lfh WHERE lfh.content_id = c.id AND lfh.deleted = FALSE) AS held \
         FROM release_contents_superseded x JOIN content c ON c.id = x.content_id LEFT JOIN platforms p ON p.id = c.app_platform_id LEFT JOIN sources ds ON ds.id = x.source_id \
         WHERE x.release_id = ? AND NOT EXISTS (SELECT 1 FROM release_contents rc WHERE rc.release_id = x.release_id AND rc.content_id = x.content_id) ORDER BY ds.name, x.file_name"
    )))
    .bind(id)
    .fetch_all(pool)
    .await?;
    let mut superseded = Vec::with_capacity(superseded_rows.len());
    for r in &superseded_rows {
        let mut c = row_to_content(r)?;
        // The name the DAT gave it, not the one the bytes were first seen under.
        if let Some(name) = r.try_get::<Option<String>, _>("release_file_name")? {
            c.file_name = Some(name);
        }
        c.dump_source = r.try_get("dump_source")?;
        c.variant = r.try_get("variant").unwrap_or(None);
        c.held = r.try_get::<i64, _>("held").unwrap_or(0) != 0;
        c.superseded_at = r.try_get("superseded_at")?;
        c.superseded_by = r.try_get::<Option<String>, _>("superseded_by")?.filter(|s| !s.is_empty());
        superseded.push(c);
    }
    decorate_contents(pool, &mut superseded).await?;
    let serials = sqlx::query("SELECT provider AS source, serial AS name FROM release_provider_serials WHERE release_id = ? ORDER BY provider").bind(id).fetch_all(pool).await?;
    let ratings = sqlx::query("SELECT board AS source, rating AS name FROM release_ratings WHERE release_id = ? ORDER BY board").bind(id).fetch_all(pool).await?;
    let set_rows = sqlx::query("SELECT s.id, s.name, csr.position FROM collection_set_releases csr JOIN collection_sets s ON s.id = csr.set_id WHERE csr.release_id = ? AND s.deleted = FALSE ORDER BY s.name").bind(id).fetch_all(pool).await?;
    let tagged = |rows: &[MySqlRow]| -> Result<Vec<Tagged>> { rows.iter().map(|r| Ok(Tagged { name: r.try_get("name")?, source: r.try_get("source")? })).collect() };
    let flag = |key: &str| -> Result<bool> {
        let v: Option<i64> = row.try_get(key)?;
        Ok(v.unwrap_or(0) != 0)
    };
    let name = release.name.clone();
    let addons = match release.title_id {
        Some(title_id) if release.material.is_none() => {
            let codes = |v: Option<&str>| -> Vec<String> { v.unwrap_or("").split(',').map(|r| r.trim().to_ascii_uppercase()).filter(|r| !r.is_empty()).collect() };
            let own = codes(release.region.as_deref());
            let mut list = releases(app, &ListQuery { title_id: Some(title_id), limit: Some(500), view: Some("collection".into()), addons_for: Some(release.platform_id), ..Default::default() }).await?.items;
            // Stable: within "this region's" and "the others" the ranked order stays.
            list.sort_by_key(|a| {
                let theirs = codes(a.region.as_deref());
                !(theirs.is_empty() || own.is_empty() || theirs.iter().any(|r| r == "WORLD" || own.contains(r)) || own.iter().any(|r| r == "WORLD"))
            });
            list
        }
        _ => Vec::new(),
    };
    let display = super::display::values(app, id, release.platform_id, release.region.as_deref()).await?;
    Ok(ReleaseDetail {
        display,
        addons,
        base: super::bases::of_release(pool, id).await?.filter(|b| b.status != "rejected"),
        patched: super::bases::made_from_release(pool, id).await?,
        released_on: row.try_get("released_on")?,
        min_players: row.try_get("min_players")?,
        max_players: row.try_get("max_players")?,
        is_beta: flag("is_beta")?,
        is_prototype: flag("is_prototype")?,
        is_demo: flag("is_demo")?,
        is_unlicensed: flag("is_unlicensed")?,
        community_patched: flag("community_patched")?,
        patch_type: row.try_get("patch_type")?,
        contents,
        superseded,
        games: local_games_for_release(pool, id).await?,
        ra_games: sqlx::query(
            "SELECT rl.provider_id, g.title, g.num_achievements FROM release_provider_links rl LEFT JOIN retroachievements_games g ON g.provider_id = CAST(rl.provider_id AS UNSIGNED) \
             WHERE rl.release_id = ? AND rl.provider = 'retroachievements' AND rl.deleted = FALSE AND rl.confidence IN ('exact', 'manual') AND COALESCE(g.num_achievements, 1) > 0 ORDER BY rl.provider_id",
        )
        .bind(id)
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| Ok(RaGameRef { id: r.try_get("provider_id")?, title: r.try_get("title")?, achievements: r.try_get::<Option<u32>, _>("num_achievements")? }))
        .collect::<Result<_>>()?,
        provider_serials: tagged(&serials)?,
        ratings: tagged(&ratings)?,
        provider_links: {
            // Title-tier providers (IGDB, ScreenScraper, ...) carry per-region names and
            // box art too, so a release page links to them through its title.
            let mut links = provider_links_for(pool, "release_provider_links", "release_id", id, "release", &name).await?;
            // Not for what was made from the game: its title is the original's, and the
            // original's pages are not the hack's.
            // Nor for a provider the release has a game of its own at (a title combined from
            // several IGDB games): that one is its page.
            if let Some(title_id) = release.title_id.filter(|_| !stands_apart(&release)) {
                let own: Vec<String> = links.iter().map(|l| l.provider.clone()).collect();
                links.extend(provider_links_for(pool, "title_provider_links", "title_id", title_id, "title", &name).await?.into_iter().filter(|l| !own.contains(&l.provider)));
            }
            links
        },
        media: release_media(pool, id, release.title_id).await?,
        sets: set_rows.iter().map(|r| Ok(SetRef { id: r.try_get("id")?, name: r.try_get("name")?, position: r.try_get("position")? })).collect::<Result<_>>()?,
        release,
    })
}

// ---------------------------------------------------------------------------
// owned games
// ---------------------------------------------------------------------------

const GAME_SELECT: &str = concat!("lg.id, lg.name, COALESCE(lg.region, MAX(r.region)) AS region, MAX(r.languages) AS languages, lg.category, lg.playlist_file_id, MIN(r.id) AS release_id, MIN(r.name) AS release_name, MAX(t.id) AS title_id, MAX(t.name) AS title_name, \
    MAX(p.name) AS platform, COUNT(DISTINCT lr.id) AS rom_count, COUNT(DISTINCT lrf.file_id) AS file_count, \
    COALESCE(MAX(lg.cover_media_id), (SELECT m.id FROM local_release_games lrg2 JOIN release_media rm ON rm.release_id = lrg2.release_id JOIN media m ON m.id = rm.media_id WHERE lrg2.game_id = lg.id AND FIND_IN_SET(m.media_type, ?) AND m.deleted = FALSE ORDER BY ", media_order!(), " LIMIT 1)) AS cover_media_id, \
    MAX(CASE WHEN EXISTS (SELECT 1 FROM release_sources rsv JOIN sources sv ON sv.id = rsv.source_id WHERE rsv.release_id = r.id AND sv.name <> 'hasheous') THEN 1 ELSE 0 END) AS dat_verified, \
    (SELECT COUNT(DISTINCT hl.content_id) FROM local_game_roms lgr7 JOIN local_rom_files lrf7 ON lrf7.rom_id = lgr7.rom_id JOIN local_files f7 ON f7.id = lrf7.file_id JOIN hasheous_lookups hl ON hl.content_id = f7.content_id AND hl.hit = TRUE AND hl.deleted = FALSE WHERE lgr7.game_id = lg.id) AS hasheous_verified, \
    COUNT(DISTINCT CASE WHEN f.deleted THEN f.id END) AS missing_files, \
    (SELECT COUNT(DISTINCT f3.id) FROM local_game_roms lgr3 JOIN local_rom_files lrf3 ON lrf3.rom_id = lgr3.rom_id JOIN local_files f3 ON f3.id = lrf3.file_id AND f3.deleted = TRUE WHERE lgr3.game_id = lg.id \
       AND EXISTS (SELECT 1 FROM local_files o WHERE o.content_id = f3.content_id AND o.deleted = FALSE AND o.id <> f3.id)) AS missing_found, \
    MAX(", ra_exists!("r"), ") AS ra");

fn row_to_game(row: &MySqlRow) -> Result<GameRow> {
    let rom_count: i64 = row.try_get("rom_count")?;
    let file_count: i64 = row.try_get("file_count")?;
    Ok(GameRow {
        id: row.try_get("id")?,
        display_name: {
            let name: String = row.try_get("name")?;
            let shown: String = row.try_get::<Option<String>, _>("release_name")?.unwrap_or(name);
            if rom_count > 1 { crate::api::scan::disc_set_key(&shown).0 } else { shown }
        },
        name: row.try_get("name")?,
        region: row.try_get("region")?,
        languages: row.try_get("languages")?,
        category: row.try_get("category")?,
        playlist_file_id: row.try_get("playlist_file_id")?,
        release_id: row.try_get("release_id")?,
        release_name: row.try_get("release_name")?,
        title_id: row.try_get("title_id")?,
        title_name: row.try_get("title_name")?,
        platform: row.try_get("platform")?,
        ra: row.try_get::<Option<i64>, _>("ra").ok().flatten().unwrap_or(0) != 0,
        rom_count: rom_count as u64,
        file_count: file_count as u64,
        cover_media_id: row.try_get("cover_media_id")?,
        dat_verified: row.try_get::<i64, _>("dat_verified").unwrap_or(0) != 0,
        hasheous_verified: row.try_get::<i64, _>("hasheous_verified").unwrap_or(0) as u64,
        missing_files: row.try_get::<i64, _>("missing_files").unwrap_or(0) as u64,
        contents_required: 0,
        contents_present: 0,
        complete: None,
        merge_hint: false,
        missing_found: row.try_get::<i64, _>("missing_found").unwrap_or(0) as u64,
    })
}

pub async fn games(app: &App, q: &ListQuery) -> Result<Page<GameRow>> {
    let mut body = String::from(
        "FROM local_game lg \
         LEFT JOIN local_release_games lrg ON lrg.game_id = lg.id \
         LEFT JOIN `release` r ON r.id = lrg.release_id \
         LEFT JOIN sources s ON s.id = r.source_id \
         LEFT JOIN title_releases tr ON tr.release_id = r.id \
         LEFT JOIN title t ON t.id = tr.title_id \
         LEFT JOIN local_game_roms lgr ON lgr.game_id = lg.id \
         LEFT JOIN local_rom lr ON lr.id = lgr.rom_id \
         LEFT JOIN platforms p ON p.id = lr.app_platform_id \
         LEFT JOIN local_rom_files lrf ON lrf.rom_id = lr.id \
         LEFT JOIN local_files f ON f.id = lrf.file_id \
         WHERE 1 = 1",
    );
    let mut binds = vec![Bind::Str(grid_media_type(app).await?)];
    if !q.include_deleted {
        body.push_str(" AND lg.deleted = FALSE");
    }
    if let Some(s) = q.search.as_deref().filter(|s| !s.trim().is_empty()) {
        let (cond, pats) = crate::database::dictionary::search_condition(&app.pool, s.trim(), &["lg.name LIKE ?", "r.name LIKE ?"], like_pattern(s.trim())).await?;
        body.push_str(&format!(" AND {cond}"));
        binds.extend(pats.into_iter().map(Bind::Str));
    }
    body.push_str(&platforms_clause("lr.app_platform_id", q));
    push_facet_filters(&mut body, &mut binds, q, "t.id", &db_settings::genre_source_priority(&app.pool).await?);
    if let Some(region) = q.region.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        body.push_str(" AND FIND_IN_SET(?, REPLACE(COALESCE(lg.region, r.region), ' ', ''))");
        binds.push(Bind::Str(region.to_ascii_uppercase()));
    }
    if let Some(language) = q.language.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
        body.push_str(" AND FIND_IN_SET(?, REPLACE(r.languages, ' ', ''))");
        binds.push(Bind::Str(language.to_ascii_lowercase()));
    }
    let (display_conds, display_binds) = display_filters(q, "r")?;
    body.push_str(&display_conds);
    binds.extend(display_binds.into_iter().map(Bind::Str));
    body.push_str(" GROUP BY lg.id, lg.name, lg.region, lg.category, lg.playlist_file_id");
    body.push_str(status_having(q.status.as_deref())?);
    let order = order_clause(q.sort.as_deref(), q.desc, &[("name", "lg.name"), ("region", "region"), ("languages", "languages"), ("platform", "platform"), ("release", "release_name"), ("title", "title_name"), ("rom_count", "rom_count"), ("file_count", "file_count"), ("id", "lg.id")], "lg.name")?;
    let (limit, offset) = clamp_page(q.limit, q.offset);
    let mut page = paged(&app.pool, GAME_SELECT, None, &body, &order, binds, limit, offset, row_to_game).await?;
    decorate_games(&app.pool, &mut page.items).await?;
    Ok(page)
}
// ---------------------------------------------------------------------------
// roms
// ---------------------------------------------------------------------------

/// A local ROM: one playable unit as scanned (a cartridge file, a cue with its tracks, a
/// CHD), its files and whether any is missing.
#[derive(Debug, Clone, Serialize)]
pub struct RomRow {
    pub id: u64,
    pub main_path: String,
    pub platform: Option<String>,
    pub disc_number: Option<u16>,
    pub alt: bool,
    pub deleted: bool,
    pub game_id: Option<u64>,
    pub game_name: Option<String>,
    pub release_id: Option<u64>,
    pub release_name: Option<String>,
    pub file_count: u64,
    /// Files no longer found on disk.
    pub missing_files: u64,
    /// Archive members recorded for its files (CHD tracks, zip contents).
    pub member_count: u64,
    /// Every file present.
    pub complete: bool,
    /// Missing files whose bytes another live local file holds (moved / renamed / copied).
    pub missing_found: u64,
}

const ROM_SELECT: &str = "lr.id, lr.main_path, MAX(p.name) AS platform, lr.disc_number, lr.alt, lr.deleted, MAX(lg.id) AS game_id, MAX(lg.name) AS game_name, MAX(r.id) AS release_id, MAX(r.name) AS release_name, \
    COUNT(DISTINCT f.id) AS file_count, COUNT(DISTINCT CASE WHEN f.deleted THEN f.id END) AS missing_files, \
    (SELECT COUNT(*) FROM local_rom_files lrf2 JOIN local_files f2 ON f2.id = lrf2.file_id JOIN content_contents cc ON cc.parent_content_id = f2.content_id WHERE lrf2.rom_id = lr.id) AS member_count, \
    (SELECT COUNT(DISTINCT f3.id) FROM local_rom_files lrf3 JOIN local_files f3 ON f3.id = lrf3.file_id AND f3.deleted = TRUE WHERE lrf3.rom_id = lr.id \
       AND EXISTS (SELECT 1 FROM local_files o WHERE o.content_id = f3.content_id AND o.deleted = FALSE AND o.id <> f3.id)) AS missing_found";

fn row_to_rom(row: &MySqlRow) -> Result<RomRow> {
    let missing = row.try_get::<i64, _>("missing_files").unwrap_or(0) as u64;
    Ok(RomRow {
        id: row.try_get("id")?,
        main_path: row.try_get("main_path")?,
        platform: row.try_get("platform")?,
        disc_number: row.try_get("disc_number")?,
        alt: row.try_get("alt")?,
        deleted: row.try_get("deleted")?,
        game_id: row.try_get("game_id")?,
        game_name: row.try_get("game_name")?,
        release_id: row.try_get("release_id")?,
        release_name: row.try_get("release_name")?,
        file_count: row.try_get::<i64, _>("file_count").unwrap_or(0) as u64,
        missing_files: missing,
        member_count: row.try_get::<i64, _>("member_count").unwrap_or(0) as u64,
        complete: missing == 0,
        missing_found: row.try_get::<i64, _>("missing_found").unwrap_or(0) as u64,
    })
}

/// `status`: complete (every file present) | missing (some file gone) | none.
fn status_having(status: Option<&str>) -> Result<&'static str> {
    match status.filter(|s| !s.is_empty()) {
        None => Ok(""),
        Some("complete") => Ok(" HAVING missing_files = 0"),
        Some("missing") => Ok(" HAVING missing_files > 0"),
        Some(other) => Err(Error::invalid(format!("status {other:?} must be complete or missing"))),
    }
}

pub async fn roms(app: &App, q: &ListQuery) -> Result<Page<RomRow>> {
    let mut body = String::from(
        "FROM local_rom lr LEFT JOIN platforms p ON p.id = lr.app_platform_id LEFT JOIN local_game_roms lgr ON lgr.rom_id = lr.id LEFT JOIN local_game lg ON lg.id = lgr.game_id \
         LEFT JOIN local_release_games lrg ON lrg.game_id = lg.id LEFT JOIN `release` r ON r.id = lrg.release_id \
         LEFT JOIN local_rom_files lrf ON lrf.rom_id = lr.id LEFT JOIN local_files f ON f.id = lrf.file_id WHERE 1 = 1",
    );
    let mut binds = Vec::new();
    if !q.include_deleted {
        body.push_str(" AND lr.deleted = FALSE");
    }
    if let Some(s) = q.search.as_deref().filter(|s| !s.trim().is_empty()) {
        body.push_str(" AND (lr.main_path LIKE ? OR r.name LIKE ? OR lg.name LIKE ?)");
        let pat = like_pattern(s.trim());
        binds.extend([Bind::Str(pat.clone()), Bind::Str(pat.clone()), Bind::Str(pat)]);
    }
    body.push_str(&platforms_clause("lr.app_platform_id", q));
    body.push_str(" GROUP BY lr.id, lr.main_path, lr.disc_number, lr.alt, lr.deleted");
    body.push_str(status_having(q.status.as_deref())?);
    let order = order_clause(q.sort.as_deref(), q.desc, &[("path", "lr.main_path"), ("platform", "platform"), ("game", "game_name"), ("release", "release_name"), ("file_count", "file_count"), ("missing", "missing_files"), ("id", "lr.id")], "lr.main_path")?;
    let (limit, offset) = clamp_page(q.limit, q.offset);
    paged(&app.pool, ROM_SELECT, None, &body, &order, binds, limit, offset, row_to_rom).await
}

/// A single rom: its files (with their archive members / hashes), its game and releases.
#[derive(Debug, Clone, Serialize)]
pub struct RomPage {
    pub rom: RomDetail,
    pub game_id: Option<u64>,
    pub game_name: Option<String>,
    /// The other roms (discs) of its game.
    pub siblings: Vec<RomSibling>,
    pub releases: Vec<RomRelease>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RomSibling {
    pub id: u64,
    pub disc_number: Option<u16>,
    pub main_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RomRelease {
    pub id: u64,
    pub name: String,
}

pub async fn rom(app: &App, id: u64) -> Result<RomPage> {
    let pool = &app.pool;
    let r = sqlx::query("SELECT lr.id, lr.disc_number, lr.main_path, lr.app_platform_id, p.name AS platform, lr.alt, lr.deleted FROM local_rom lr LEFT JOIN platforms p ON p.id = lr.app_platform_id WHERE lr.id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| Error::not_found(format!("no rom {id}")))?;
    let mut files = files_for_rom(pool, id).await?;
    decorate_files(pool, &mut files).await?;
    let rom = RomDetail { id, disc_number: r.try_get("disc_number")?, main_path: r.try_get("main_path")?, platform_id: r.try_get("app_platform_id")?, platform: r.try_get("platform")?, alt: r.try_get("alt")?, deleted: r.try_get("deleted")?, files };
    let game = sqlx::query("SELECT lg.id, lg.name FROM local_game_roms lgr JOIN local_game lg ON lg.id = lgr.game_id AND lg.deleted = FALSE WHERE lgr.rom_id = ? ORDER BY lg.id LIMIT 1").bind(id).fetch_optional(pool).await?;
    let game_id: Option<u64> = game.as_ref().map(|g| g.try_get("id")).transpose()?;
    let game_name: Option<String> = game.as_ref().map(|g| g.try_get("name")).transpose()?;
    let (siblings, releases) = match game_id {
        Some(g) => (
            sqlx::query("SELECT lr.id, lr.disc_number, lr.main_path FROM local_game_roms lgr JOIN local_rom lr ON lr.id = lgr.rom_id AND lr.deleted = FALSE WHERE lgr.game_id = ? AND lr.id <> ? ORDER BY lr.disc_number, lr.id")
                .bind(g)
                .bind(id)
                .fetch_all(pool)
                .await?
                .iter()
                .map(|x| Ok(RomSibling { id: x.try_get("id")?, disc_number: x.try_get("disc_number")?, main_path: x.try_get("main_path")? }))
                .collect::<Result<Vec<_>>>()?,
            sqlx::query("SELECT r.id, r.name FROM local_release_games lrg JOIN `release` r ON r.id = lrg.release_id WHERE lrg.game_id = ? ORDER BY r.name")
                .bind(g)
                .fetch_all(pool)
                .await?
                .iter()
                .map(|x| Ok(RomRelease { id: x.try_get("id")?, name: x.try_get("name")? }))
                .collect::<Result<Vec<_>>>()?,
        ),
        None => (Vec::new(), Vec::new()),
    };
    // A multi-disc game shows without "(Disc N)" (its stored name keeps it).
    let game_name = game_name.map(|n| if siblings.is_empty() { n } else { crate::api::scan::disc_set_key(&n).0 });
    Ok(RomPage { rom, game_id, game_name, siblings, releases })
}

// ---------------------------------------------------------------------------
// contents
// ---------------------------------------------------------------------------

/// A content row (a file's bytes, wherever and however named) with where it occurs.
#[derive(Debug, Clone, Serialize)]
pub struct ContentListRow {
    #[serde(flatten)]
    pub content: ContentRow,
    /// Live local files with exactly these bytes.
    pub local_files: i64,
    /// Archives (content rows) it was found inside.
    pub parents: i64,
    /// DAT releases listing it.
    pub releases: i64,
}

const CONTENTS_SELECT: &str = "c.id AS content_id, c.file_name, c.size, c.app_platform_id AS platform_id, p.name AS platform, c.media_type, c.partial, \
    (SELECT COUNT(*) FROM local_files f WHERE f.content_id = c.id AND f.deleted = FALSE) AS local_files, \
    (SELECT COUNT(*) FROM content_contents cc WHERE cc.child_content_id = c.id) AS parents, \
    (SELECT COUNT(*) FROM release_contents rc WHERE rc.content_id = c.id) AS releases";

/// `view`: local (default: held by a live file, directly or inside an archive) | all.
pub async fn contents(app: &App, q: &ListQuery) -> Result<Page<ContentListRow>> {
    let mut body = String::from("FROM content c LEFT JOIN platforms p ON p.id = c.app_platform_id WHERE 1 = 1");
    let mut binds = Vec::new();
    match q.view.as_deref().filter(|v| !v.is_empty()).unwrap_or("local") {
        "local" => body.push_str(
            " AND (EXISTS (SELECT 1 FROM local_files lf WHERE lf.content_id = c.id AND lf.deleted = FALSE) \
             OR EXISTS (SELECT 1 FROM content_contents lcc JOIN local_files lf2 ON lf2.content_id = lcc.parent_content_id AND lf2.deleted = FALSE WHERE lcc.child_content_id = c.id))",
        ),
        "all" => {}
        other => return Err(Error::invalid(format!("view {other:?} must be local or all"))),
    }
    if let Some(s) = q.search.as_deref().filter(|s| !s.trim().is_empty()) {
        let s = s.trim();
        if s.len() >= 8 && s.chars().all(|c| c.is_ascii_hexdigit()) {
            body.push_str(" AND c.id IN (SELECT h.content_id FROM hashes h WHERE h.value_hex = ?)");
            binds.push(Bind::Str(s.to_ascii_lowercase()));
        } else {
            // The first-seen name, or any name a DAT gives the same bytes.
            body.push_str(" AND (c.file_name LIKE ? OR EXISTS (SELECT 1 FROM release_contents rcs WHERE rcs.content_id = c.id AND rcs.file_name LIKE ?))");
            binds.push(Bind::Str(like_pattern(s)));
            binds.push(Bind::Str(like_pattern(s)));
        }
    }
    body.push_str(&platforms_clause("c.app_platform_id", q));
    let order = order_clause(q.sort.as_deref(), q.desc, &[("name", "c.file_name"), ("size", "c.size"), ("platform", "platform"), ("id", "c.id")], "c.file_name")?;
    let (limit, offset) = clamp_page(q.limit, q.offset);
    let mut page = paged(&app.pool, CONTENTS_SELECT, None, &body, &order, binds, limit, offset, |row| {
        Ok(ContentListRow { content: row_to_content(row)?, local_files: row.try_get("local_files")?, parents: row.try_get("parents")?, releases: row.try_get("releases")? })
    })
    .await?;
    let mut rows: Vec<ContentRow> = page.items.iter().map(|r| r.content.clone()).collect();
    decorate_contents(&app.pool, &mut rows).await?;
    // DAT names for the page's rows only: in the paged select the subquery would also run
    // for every row the count walks (all 3M contents in the `all` view).
    if !rows.is_empty() {
        let ids = rows.iter().map(|r| r.content_id.to_string()).collect::<Vec<_>>().join(",");
        let names: HashMap<u64, Option<String>> = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT c.id, {CANONICAL_NAME_SQL} AS canonical_name FROM content c WHERE c.id IN ({ids})")))
            .fetch_all(&app.pool)
            .await?
            .iter()
            .map(|r| Ok((r.try_get("id")?, r.try_get("canonical_name")?)))
            .collect::<Result<_>>()?;
        for row in &mut rows {
            row.canonical_name = names.get(&row.content_id).cloned().flatten();
        }
    }
    for (item, decorated) in page.items.iter_mut().zip(rows) {
        item.content = decorated;
    }
    Ok(page)
}

#[derive(Debug, Clone, Serialize)]
pub struct ContentFile {
    pub id: u64,
    pub path: String,
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContentRelease {
    pub release_id: u64,
    pub name: String,
    pub platform: Option<String>,
    /// The file name the DAT gives it.
    pub file_name: String,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContentDetail {
    pub content: ContentRow,
    /// The name DATs give these bytes (the most common one; Redump / No-Intro before
    /// TOSEC and the rest), else the name it was first seen under.
    pub canonical_name: Option<String>,
    /// Local files with these bytes.
    pub files: Vec<ContentFile>,
    /// Archives it was found inside, with their local files.
    pub parents: Vec<ContentRow>,
    pub parent_files: Vec<ContentFile>,
    /// Its own members, when it is an archive.
    pub members: Vec<ContentRow>,
    pub releases: Vec<ContentRelease>,
}

pub async fn content(app: &App, id: u64) -> Result<ContentDetail> {
    let pool = &app.pool;
    let content = content_row(pool, id).await?.ok_or_else(|| Error::not_found(format!("no content {id}")))?;
    let file = |r: &MySqlRow| -> Result<ContentFile> { Ok(ContentFile { id: r.try_get("id")?, path: r.try_get("path")?, deleted: r.try_get("deleted")? }) };
    let files = sqlx::query("SELECT id, path, deleted FROM local_files WHERE content_id = ? ORDER BY deleted, path").bind(id).fetch_all(pool).await?.iter().map(file).collect::<Result<Vec<_>>>()?;
    let parent_rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT {CONTENT_SELECT}, cc.member_path FROM content_contents cc JOIN content c ON c.id = cc.parent_content_id LEFT JOIN platforms p ON p.id = c.app_platform_id WHERE cc.child_content_id = ? ORDER BY c.file_name")))
        .bind(id)
        .fetch_all(pool)
        .await?;
    let mut parents = parent_rows.iter().map(row_to_content).collect::<Result<Vec<_>>>()?;
    decorate_contents(pool, &mut parents).await?;
    let parent_files = sqlx::query("SELECT f.id, f.path, f.deleted FROM content_contents cc JOIN local_files f ON f.content_id = cc.parent_content_id WHERE cc.child_content_id = ? ORDER BY f.deleted, f.path")
        .bind(id)
        .fetch_all(pool)
        .await?
        .iter()
        .map(file)
        .collect::<Result<Vec<_>>>()?;
    let members = members_of(pool, id).await?;
    let releases = sqlx::query(
        "SELECT r.id, r.name, p.name AS platform, rc.file_name, s.name AS source FROM release_contents rc JOIN `release` r ON r.id = rc.release_id AND r.deleted = FALSE \
         LEFT JOIN platforms p ON p.id = r.app_platform_id LEFT JOIN sources s ON s.id = rc.source_id WHERE rc.content_id = ? ORDER BY r.name LIMIT 200",
    )
    .bind(id)
    .fetch_all(pool)
    .await?
    .iter()
    .map(|r| Ok(ContentRelease { release_id: r.try_get("id")?, name: r.try_get("name")?, platform: r.try_get("platform")?, file_name: r.try_get("file_name")?, source: r.try_get("source")? }))
    .collect::<Result<Vec<_>>>()?;
    let canonical_name: Option<String> = sqlx::query_scalar(
        "SELECT rc.file_name FROM release_contents rc LEFT JOIN sources s ON s.id = rc.source_id WHERE rc.content_id = ? \
         GROUP BY rc.file_name ORDER BY MIN(FIELD(COALESCE(s.name, ''), 'no-intro', 'redump', 'mame-redump', 'tosec') = 0), MIN(NULLIF(FIELD(COALESCE(s.name, ''), 'no-intro', 'redump', 'mame-redump', 'tosec'), 0)), COUNT(*) DESC, rc.file_name LIMIT 1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .or_else(|| content.file_name.clone());
    Ok(ContentDetail { content, canonical_name, files, parents, parent_files, members, releases })
}

// ---------------------------------------------------------------------------
// game completeness
// ---------------------------------------------------------------------------

/// Disc containers: a DAT line for one of these is the whole dump on its own.
const CONTAINER_EXTS: [&str; 8] = ["chd", "rvz", "wia", "gcz", "cso", "zso", "pbp", "xiso"];
/// Disc sheets a container carries no copy of (CHD / RVZ hold the tracks, not the cue).
const SHEET_EXTS: [&str; 4] = ["cue", "gdi", "ccd", "m3u"];

fn ext_of(name: &str) -> String {
    std::path::Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// One dump's coverage (one DAT source's lines): (required, present). A container line
/// present (a CHD a mame-redump DAT lists) covers it by itself; otherwise every
/// non-container line is required, except the disc sheet when the game is held as a
/// container (its tracks are matched from inside it).
pub fn coverage(lines: &[(String, bool)], game_has_container: bool) -> (u64, u64) {
    if lines.iter().any(|(name, present)| *present && CONTAINER_EXTS.contains(&ext_of(name).as_str())) {
        return (1, 1);
    }
    let required: Vec<&(String, bool)> = lines
        .iter()
        .filter(|(name, _)| {
            let ext = ext_of(name);
            !CONTAINER_EXTS.contains(&ext.as_str()) && !(game_has_container && SHEET_EXTS.contains(&ext.as_str()))
        })
        .collect();
    (required.len() as u64, required.iter().filter(|(_, p)| *p).count() as u64)
}

/// A release several DATs list holds one dump per source (Redump's cue / bins,
/// mame-redump's CHDs, a No-Intro digital package): the game is judged by the dump it holds
/// best -- complete when any one source's dump is. Lines are (dump, file name, present).
///
/// What a dump is belongs to the caller: the source alone in the tests here, and
/// [`Dump`] -- source and variant -- for a release's real lines, so that each version of a
/// patched work is a dump too and holding one of them is holding the release.
pub fn best_coverage<K: Clone + Eq + std::hash::Hash>(lines: &[(K, String, bool)], game_has_container: bool) -> (u64, u64) {
    let mut by_source: HashMap<K, Vec<(String, bool)>> = HashMap::new();
    for (source, name, present) in lines {
        by_source.entry(source.clone()).or_default().push((name.clone(), *present));
    }
    by_source
        .values()
        .map(|l| coverage(l, game_has_container))
        .filter(|(required, _)| *required > 0)
        .max_by(|a, b| {
            let full = |x: &(u64, u64)| x.1 >= x.0;
            full(a).cmp(&full(b)).then((a.1 * b.0).cmp(&(b.1 * a.0))).then(a.1.cmp(&b.1))
        })
        .unwrap_or((0, 0))
}

/// One dump of a release: the DAT source that lists it, and for a patched work which build
/// (`release_contents.variant`; `None` everywhere else).
pub type Dump = (Option<u8>, Option<String>);

/// Sources whose "release" lines are alternative dumps of one game rather than the parts of
/// one dump. Hasheous files a game once with every regional dump under it (10-Yard Fight:
/// Japan, Japan Rev 1, USA/Europe, Retro-Bit -- four lines), so holding any one of them is
/// holding the game; requiring all four reported every single-cartridge NES game as 1/4.
const ALTERNATIVE_SOURCES: &[&str] = &["hasheous"];

/// Lines that are alternatives of each other folded into one, present when any is:
/// every line of an [`ALTERNATIVE_SOURCES`] source, and within any source a headered /
/// headerless pair (`x.nes` + `x.unh`, as No-Intro lists its NES BIOSes), which is one
/// file in two spellings.
pub fn collapse_alternatives<K: Clone + Eq + std::hash::Hash>(lines: &[(K, String, bool)], alternative_sources: &HashSet<K>) -> Vec<(K, String, bool)> {
    let mut out: Vec<(K, String, bool)> = Vec::with_capacity(lines.len());
    // Key: what a line is an alternative of. The whole source, or the file's stem.
    let mut index: HashMap<(K, String), usize> = HashMap::new();
    for (source, name, present) in lines {
        let ext = ext_of(name);
        let key = if alternative_sources.contains(source) {
            String::new()
        } else if matches!(ext.as_str(), "nes" | "unh") {
            name[..name.len() - ext.len() - 1].to_ascii_lowercase()
        } else {
            name.clone()
        };
        match index.get(&(source.clone(), key.clone())) {
            Some(&i) => {
                out[i].2 |= *present;
                if *present {
                    out[i].1 = name.clone();
                }
            }
            None => {
                index.insert((source.clone(), key), out.len());
                out.push((source.clone(), name.clone(), *present));
            }
        }
    }
    out
}

/// Fills `contents_required` / `contents_present` / `complete` for a page of games.
async fn decorate_games(pool: &MySqlPool, games: &mut [GameRow]) -> Result<()> {
    let ids: Vec<u64> = games.iter().filter(|g| g.release_id.is_some()).map(|g| g.id).collect();
    if ids.is_empty() {
        for g in games.iter_mut() {
            g.complete = Some(g.missing_files == 0).filter(|_| g.release_id.is_some());
        }
        return Ok(());
    }
    let release_list = games.iter().filter_map(|g| g.release_id).map(|r| r.to_string()).collect::<Vec<_>>().join(",");
    // Every live game of those releases (not just this page's): for the merge hint.
    let all_games: Vec<(u64, u64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT lrg.game_id, lrg.release_id FROM local_release_games lrg JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE WHERE lrg.release_id IN ({release_list})"
    )))
    .fetch_all(pool)
    .await?;
    let mut game_ids: Vec<u64> = ids.clone();
    game_ids.extend(all_games.iter().map(|g| g.0));
    game_ids.sort_unstable();
    game_ids.dedup();
    let list = game_ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT lrg.game_id, lrg.release_id, rc.source_id, rc.variant, src.name AS source_name, rc.file_name, EXISTS (SELECT 1 FROM local_game_roms lgr JOIN local_rom_files lrf ON lrf.rom_id = lgr.rom_id JOIN local_files f ON f.id = lrf.file_id AND f.deleted = FALSE \
           LEFT JOIN content_contents cc ON cc.parent_content_id = f.content_id WHERE lgr.game_id = lrg.game_id AND (f.content_id = rc.content_id OR cc.child_content_id = rc.content_id)) AS present \
         FROM local_release_games lrg JOIN release_contents rc ON rc.release_id = lrg.release_id LEFT JOIN sources src ON src.id = rc.source_id WHERE lrg.game_id IN ({list})"
    )))
    .fetch_all(pool)
    .await?;
    let mut lines: HashMap<u64, Vec<(Dump, String, bool)>> = HashMap::new();
    // release -> (dump, file) -> held by any of its games
    let mut pooled: HashMap<u64, HashMap<(Dump, String), bool>> = HashMap::new();
    let mut alternative_sources: HashSet<Dump> = HashSet::new();
    for r in rows {
        let (game, release): (u64, u64) = (r.try_get("game_id")?, r.try_get("release_id")?);
        let (name, present): (String, bool) = (r.try_get("file_name")?, r.try_get::<i64, _>("present")? != 0);
        let dump: Dump = (r.try_get("source_id")?, r.try_get("variant")?);
        if r.try_get::<Option<String>, _>("source_name")?.as_deref().is_some_and(|s| ALTERNATIVE_SOURCES.contains(&s)) {
            alternative_sources.insert(dump.clone());
        }
        *pooled.entry(release).or_default().entry((dump.clone(), name.clone())).or_default() |= present;
        lines.entry(game).or_default().push((dump, name, present));
    }
    let games_of_release: HashMap<u64, usize> = all_games.iter().fold(HashMap::new(), |mut m, (_, r)| {
        *m.entry(*r).or_default() += 1;
        m
    });
    let containers: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT lgr.game_id FROM local_game_roms lgr JOIN local_rom_files lrf ON lrf.rom_id = lgr.rom_id JOIN local_files f ON f.id = lrf.file_id \
         WHERE lgr.game_id IN ({list}) AND f.archive_format IN ('chd', 'rvz', 'wia', 'gcz', 'cso', 'pbp', 'xiso')"
    )))
    .fetch_all(pool)
    .await?;
    for g in games.iter_mut() {
        let Some(l) = lines.get(&g.id) else {
            g.complete = Some(g.missing_files == 0).filter(|_| g.release_id.is_some());
            continue;
        };
        let l = collapse_alternatives(l, &alternative_sources);
        let (required, present) = best_coverage(&l, containers.contains(&g.id));
        g.contents_required = required;
        g.contents_present = present;
        g.complete = Some(g.missing_files == 0 && required > 0 && present >= required);
        if g.complete == Some(false) {
            if let Some(release) = g.release_id.filter(|r| games_of_release.get(r).copied().unwrap_or(0) > 1) {
                let together: Vec<(Dump, String, bool)> = pooled.get(&release).into_iter().flatten().map(|((s, n), p)| (s.clone(), n.clone(), *p)).collect();
                let together = collapse_alternatives(&together, &alternative_sources);
                let (req, pres) = best_coverage(&together, containers.contains(&g.id));
                g.merge_hint = req > 0 && pres >= req;
            }
        }
    }
    Ok(())
}


// ---------------------------------------------------------------------------
// files
// ---------------------------------------------------------------------------

pub async fn files(app: &App, q: &ListQuery) -> Result<Page<FileRow>> {
    let mut body = format!("FROM local_files f {FILE_JOINS} WHERE 1 = 1");
    let mut binds = Vec::new();
    if !q.include_deleted {
        body.push_str(" AND f.deleted = FALSE");
    }
    if let Some(s) = q.search.as_deref().filter(|s| !s.trim().is_empty()) {
        body.push_str(" AND f.path LIKE ?");
        binds.push(Bind::Str(like_pattern(s.trim())));
    }
    body.push_str(&platforms_clause("c.app_platform_id", q));
    if let Some(dir) = q.directory_id {
        body.push_str(" AND f.directory_id = ?");
        binds.push(Bind::U64(dir));
    }
    let order = order_clause(q.sort.as_deref(), q.desc, &[("path", "f.path"), ("size", "f.size"), ("archive_format", "f.archive_format"), ("platform", "platform"), ("last_modified", "f.last_modified"), ("id", "f.id")], "f.path")?;
    let (limit, offset) = clamp_page(q.limit, q.offset);
    let mut page = paged(&app.pool, FILE_SELECT, None, &body, &order, binds, limit, offset, row_to_file).await?;
    decorate_files(&app.pool, &mut page.items).await?;
    Ok(page)
}

pub async fn file(app: &App, id: u64) -> Result<FileDetail> {
    let pool = &app.pool;
    let mut file = file_by_id(pool, id).await?.ok_or_else(|| Error::not_found(format!("no file {id}")))?;
    let mut one = vec![file];
    decorate_files(pool, &mut one).await?;
    file = one.pop().expect("one file");
    let content = content_row(pool, file.content_id).await?;
    let members = members_of(pool, file.content_id).await?;
    let owner_rows = sqlx::query("SELECT lr.id AS rom_id, lr.main_path, lg.id AS game_id, lg.name AS game_name FROM local_rom_files lrf JOIN local_rom lr ON lr.id = lrf.rom_id LEFT JOIN local_game_roms lgr ON lgr.rom_id = lr.id LEFT JOIN local_game lg ON lg.id = lgr.game_id WHERE lrf.file_id = ? ORDER BY lr.id")
        .bind(id)
        .fetch_all(pool)
        .await?;
    let owners = owner_rows.iter().map(|r| Ok(FileOwner { rom_id: r.try_get("rom_id")?, main_path: r.try_get("main_path")?, game_id: r.try_get("game_id")?, game_name: r.try_get("game_name")? })).collect::<Result<_>>()?;
    Ok(FileDetail { file, content, members, owners })
}

// ---------------------------------------------------------------------------
// media files
// ---------------------------------------------------------------------------

/// The file (full size, through the cache tiers) and MIME type of a `media` row, for
/// streaming to a client -- see `api::media::file` for thumbnails.
pub async fn media_file(app: &App, id: u64) -> Result<(PathBuf, &'static str)> {
    super::media::file(app, id, false, None).await
}

// ---------------------------------------------------------------------------
// hash lookup
// ---------------------------------------------------------------------------

fn algorithm_from_name(name: &str) -> Option<HashAlgorithms> {
    match name.to_ascii_lowercase().replace('-', "_").as_str() {
        "crc32" | "crc" => Some(HashAlgorithms::Crc32),
        "md5" => Some(HashAlgorithms::Md5),
        "sha1" => Some(HashAlgorithms::Sha1),
        "sha256" => Some(HashAlgorithms::Sha256),
        "ra_hash" | "rahash" | "ra" => Some(HashAlgorithms::Rahash),
        "xxh3_128" | "xxh3" => Some(HashAlgorithms::Xxhash3_128),
        _ => None,
    }
}

/// Looks `value` up as `algorithm`, or -- when none is given -- by the hex length (8 crc32,
/// 32 md5, 40 sha1, 64 sha256; a 32-char value is also tried as an RA hash).
pub async fn lookup_hash(app: &App, algorithm: Option<&str>, value: &str) -> Result<Vec<HashMatchRow>> {
    let value = value.trim();
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::invalid("value must be a hex hash"));
    }
    let algorithms: Vec<HashAlgorithms> = match algorithm.filter(|a| !a.is_empty()) {
        Some(a) => vec![algorithm_from_name(a).ok_or_else(|| Error::invalid(format!("unknown algorithm {a:?}")))?],
        None => match value.len() {
            8 => vec![HashAlgorithms::Crc32],
            32 => vec![HashAlgorithms::Md5, HashAlgorithms::Rahash, HashAlgorithms::Xxhash3_128],
            40 => vec![HashAlgorithms::Sha1],
            64 => vec![HashAlgorithms::Sha256],
            n => return Err(Error::invalid(format!("{n} hex chars is not a crc32/md5/sha1/sha256 length"))),
        },
    };
    let mut out = Vec::new();
    for algorithm in algorithms {
        for m in lookup::lookup_by_hash(&app.pool, algorithm, value).await? {
            out.push(HashMatchRow {
                algorithm: lookup::algorithm_column(algorithm).to_string(),
                content_id: m.content_id,
                file_name: m.file_name,
                size: m.size,
                platform_id: m.platform.igdb_id(),
                platform: platform_display(m.platform.igdb_id()),
                source: m.source,
                release_id: m.release_id,
                release_name: m.release_name,
                region: m.region,
                title_id: m.title_id,
                title_name: m.title_name,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{best_coverage, collapse_alternatives, coverage, dedupe_provider_links, inherit, kinds_clause, order_cross_titles, order_local_games, platform_clause, KINDS, platform_filter_ids, MediaRow, Platform, ProviderLink, PLATFORM_FILTER_GROUPS};
    use std::collections::HashSet;

    /// A release's local copies: complete before incomplete (not judged counts as complete),
    /// then by the priority of their directory, then id; the user's order before all of it.
    #[test]
    fn media_order_names_every_regionless_source() {
        for source in crate::database::settings::REGIONLESS_SOURCES {
            assert!(super::MEDIA_ORDER.contains(&format!("'{source}'")), "{source} is missing from media_order's region rule");
        }
    }

    #[test]
    fn local_copies_go_complete_then_by_directory_priority_unless_ordered() {
        let keys = std::collections::HashMap::from([(10, vec![1, 2]), (11, vec![1, 1]), (12, vec![2])]);
        let mut games = vec![(1, Some(false), Some(11), None), (2, Some(true), Some(10), None), (3, None, Some(11), None), (4, Some(true), Some(12), None), (5, Some(true), None, None)];
        order_local_games(&mut games, &keys);
        assert_eq!(games.iter().map(|g| g.0).collect::<Vec<_>>(), vec![3, 2, 4, 5, 1]);
        games[4].3 = Some(0);
        games[2].3 = Some(1);
        order_local_games(&mut games, &keys);
        assert_eq!(games.iter().map(|g| g.0).collect::<Vec<_>>(), vec![1, 4, 3, 2, 5]);
    }

    /// A cross-platform row leads with the platform the setting ranks first; platforms the
    /// setting does not name follow, by name.
    #[test]
    fn a_cross_platform_row_follows_the_platform_priority() {
        let p = |id: u16, name: &str| (id, name.to_string());
        let titles = || vec![(1, "Game".to_string(), vec![p(49, "Xbox One")], 1, 0), (2, "Game".to_string(), vec![p(6, "PC (Windows)"), p(48, "PlayStation 4")], 2, 1), (3, "Game".to_string(), vec![p(999, "Amiga")], 1, 0)];
        let (ordered, row) = order_cross_titles(titles(), &[48, 49, 6]);
        assert_eq!(ordered.iter().map(|t| t.id).collect::<Vec<_>>(), vec![2, 1, 3]);
        assert_eq!(ordered[0].platforms.as_deref(), Some("PlayStation 4, PC (Windows)"));
        assert_eq!(row, "PlayStation 4, Xbox One, PC (Windows), Amiga");
        let (ordered, row) = order_cross_titles(titles(), &[49]);
        assert_eq!((ordered.iter().map(|t| t.id).collect::<Vec<_>>(), row.as_str()), (vec![1, 3, 2], "Xbox One, Amiga, PC (Windows), PlayStation 4"), "the rest by name");
        assert_eq!(order_cross_titles(Vec::new(), &[48]).1, "");
    }

    /// The title page lists a provider page once, however many of its releases link it.
    #[test]
    fn a_provider_page_is_listed_once() {
        let link = |provider: &str, id: &str, via: &str, release: Option<u64>| ProviderLink { provider: provider.into(), provider_id: id.into(), confidence: "hash".into(), linked_at: Default::default(), url: None, via: via.into(), release_id: release };
        let mut links = vec![link("igdb", "1234", "title", None), link("igdb", "1234", "release", Some(1)), link("retroachievements", "10", "release", Some(1)), link("retroachievements", "10", "release", Some(2)), link("retroachievements", "11", "release", Some(3))];
        dedupe_provider_links(&mut links);
        let kept: Vec<(&str, &str, &str, Option<u64>)> = links.iter().map(|l| (l.provider.as_str(), l.provider_id.as_str(), l.via.as_str(), l.release_id)).collect();
        assert_eq!(kept, vec![("igdb", "1234", "title", None), ("retroachievements", "10", "release", Some(1)), ("retroachievements", "11", "release", Some(3))], "the title's own link wins; another region's RA set is another page");
    }

    /// A page shows its own media, and the level above's only for slots it has nothing in.
    #[test]
    fn media_is_inherited_slot_by_slot() {
        let row = |id: u64, media_type: &str, sub: Option<&str>| MediaRow { id, media_type: media_type.into(), sub_category: sub.map(str::to_string), path: String::new(), source: None, source_url: None, via: None };
        let mut release = vec![row(1, "box", Some("front")), row(2, "screenshot", None)];
        let title = vec![row(10, "box", Some("front")), row(11, "box", Some("back")), row(12, "screenshot", None), row(13, "screenshot", None), MediaRow { via: Some("crossplatform"), ..row(14, "cover", None) }];
        inherit(&mut release, title, "title");
        let shown: Vec<(u64, Option<&str>)> = release.iter().map(|m| (m.id, m.via)).collect();
        assert_eq!(shown, vec![(1, None), (2, None), (11, Some("title")), (14, Some("crossplatform"))], "its own front and its own screenshot; the title's back; the game's cover, still marked as the game's");
    }

    #[test]
    fn a_platform_filter_matches_its_whole_group() {
        // Either regional name picks both; the group's first platform is the one a pulldown offers.
        assert_eq!(platform_filter_ids(Platform::Famicom as u16), vec![Platform::Nes as u16, Platform::Famicom as u16]);
        assert_eq!(platform_filter_ids(Platform::Snes as u16), vec![Platform::Snes as u16, Platform::SuperFamicom as u16]);
        assert_eq!(platform_filter_ids(Platform::PspPs1Classic as u16), vec![Platform::PlayStation as u16, Platform::PspPs1Classic as u16]);
        // Add-ons and everything else stand alone.
        assert_eq!(platform_filter_ids(Platform::FamicomDiskSystem as u16), vec![Platform::FamicomDiskSystem as u16]);
        assert_eq!(platform_filter_ids(Platform::TurboGrafxCd as u16), vec![Platform::TurboGrafxCd as u16]);
        assert_eq!(platform_clause("r.app_platform_id", Platform::Nes as u16), " AND r.app_platform_id IN (18,99)");
        // No platform sits in two groups, or a filter on it would be ambiguous.
        let mut seen = HashSet::new();
        assert!(PLATFORM_FILTER_GROUPS.iter().flat_map(|g| g.iter()).all(|p| seen.insert(*p as u16)));
    }

    fn l(name: &str, present: bool) -> (String, bool) {
        (name.to_string(), present)
    }

    /// 10-Yard Fight under Hasheous: four regional dumps as four lines of one entry, and
    /// a No-Intro BIOS listed headered and headerless. Holding one of each is holding it.
    #[test]
    fn alternative_lines_count_as_one() {
        let hasheous: HashSet<Option<u8>> = [Some(7)].into_iter().collect();
        let lines = vec![
            (Some(7), "10-Yard Fight (Japan) (Rev 1).nes".to_string(), true),
            (Some(7), "10-Yard Fight (Japan).nes".to_string(), false),
            (Some(7), "10-Yard Fight (USA, Europe).nes".to_string(), false),
            (Some(7), "10-Yard Fight (USA) (Retro-Bit Generations).unh".to_string(), false),
            (Some(1), "[BIOS] Sharp Famicom Titler (Japan).unh".to_string(), false),
            (Some(1), "[BIOS] Sharp Famicom Titler (Japan).nes".to_string(), true),
            (Some(1), "manual.pdf".to_string(), false),
        ];
        let folded = collapse_alternatives(&lines, &hasheous);
        assert_eq!(best_coverage(&folded, false), (1, 1), "the Hasheous entry is one game, held");
        let no_intro: Vec<_> = folded.iter().filter(|l| l.0 == Some(1)).cloned().collect();
        assert_eq!(no_intro.len(), 2, "the .nes/.unh pair folded, the manual did not: {no_intro:?}");
        assert_eq!(coverage(&no_intro.iter().map(|(_, n, p)| (n.clone(), *p)).collect::<Vec<_>>(), false), (2, 1));
        // Nothing held is still nothing held.
        let none: Vec<_> = lines.iter().map(|(s, n, _)| (*s, n.clone(), false)).collect();
        let (required, present) = best_coverage(&collapse_alternatives(&none, &hasheous), false);
        assert!(required > 0 && present == 0, "({required}, {present})");
    }

    #[test]
    fn the_kinds_filter_is_any_of_what_is_ticked() {
        assert_eq!(kinds_clause(None, "r", false).unwrap(), None);
        assert_eq!(kinds_clause(Some("all"), "r", false).unwrap(), None);
        assert_eq!(kinds_clause(Some(&KINDS.join(",")), "r", false).unwrap(), None, "every kind is no filter");
        let retail = kinds_clause(Some("retail"), "r", false).unwrap().unwrap();
        assert!(retail.ends_with("IN ('retail'))"), "{retail}");
        let some = kinds_clause(Some("translation, retail,translation"), "r", false).unwrap().unwrap();
        assert!(some.ends_with("IN ('retail', 'translation'))"), "{some}");
        // The older names: everything but the subsets, and everything sold as it was.
        let patches = kinds_clause(Some("patches"), "r", false).unwrap().unwrap();
        assert!(patches.contains("'homebrew'") && patches.contains("'patched'") && !patches.contains("'subset')"), "{patches}");
        let commercial = kinds_clause(Some("commercial"), "r", false).unwrap().unwrap();
        let listed = commercial.rsplit(" IN (").next().unwrap();
        assert!(listed.contains("'demo'") && listed.contains("'retail'") && !listed.contains("'hack'"), "{commercial}");
        // A guessed language is one more thing a release may be.
        assert_eq!(kinds_clause(Some("guessed"), "x", false).unwrap().as_deref(), Some("(x.languages_state = 'guessed')"));
        let both = kinds_clause(Some("hack,guessed"), "r", false).unwrap().unwrap();
        assert!(both.contains("IN ('hack') OR r.languages_state = 'guessed'"), "{both}");
        // Listed with retail, unlicensed releases still answer to their own kind.
        assert!(kinds_clause(Some("unlicensed"), "r", true).unwrap().unwrap().contains("= 'retail' AND r.is_unlicensed)"));
        assert!(kinds_clause(Some("romhack"), "r", false).is_err());
    }

    /// Three versions of one patch, the middle one held: the release is complete, where
    /// the same three lines as parts of one dump would be one of three.
    #[test]
    fn any_one_version_of_a_patched_work_is_the_release() {
        let v = |variant: &str, name: &str, present: bool| ((Some(7u8), Some(variant.to_string())), name.to_string(), present);
        let versions = [v("v1.0", "Game [T-En by X v1.0].gba", false), v("v1.02", "Game [T-En by X v1.02].gba", true), v("v1.1", "Game [T-En by X v1.1].gba", false)];
        assert_eq!(best_coverage(&versions, false), (1, 1));
        let as_one_dump: Vec<_> = versions.iter().map(|(_, n, p)| (Some(7u8), n.clone(), *p)).collect();
        assert_eq!(best_coverage(&as_one_dump, false), (3, 1));
        // A version that is itself several files needs all of them.
        let set = [v("v2", "hack.cue", true), v("v2", "hack (Track 1).bin", false), v("v1", "old.cue", false), v("v1", "old (Track 1).bin", false)];
        assert_eq!(best_coverage(&set, false), (2, 1));
    }

    #[test]
    fn coverage_by_dump_kind() {
        // Redump cue/bin held as a CHD: the tracks count, the cue doesn't.
        let redump = [l("Game (USA).cue", false), l("Game (USA) (Track 1).bin", true), l("Game (USA) (Track 2).bin", true), l("Game (USA).chd", false)];
        assert_eq!(coverage(&redump, true), (2, 2));
        // Held as plain cue/bin: the cue is required.
        assert_eq!(coverage(&redump, false), (3, 2));
        // The CHD itself listed by mame-redump and held: complete on its own.
        assert_eq!(coverage(&[l("Game (USA).chd", true), l("Game (USA).cue", false)], true), (1, 1));
        // Cartridge: every file.
        assert_eq!(coverage(&[l("Game (USA).sfc", true)], false), (1, 1));
    }

    #[test]
    fn best_dump_decides() {
        let s = |src: u8, name: &str, present: bool| (Some(src), name.to_string(), present);
        // HQ7 (USA) as CHDs: Redump's bins held via the CHDs' tracks; mame-redump's CHD
        // lines and No-Intro's PSN package not.
        let lines = [
            s(1, "HQ7 (Disc 1).chd", false),
            s(2, "HQ7 (Disc 1).cue", false),
            s(2, "HQ7 (Disc 1).bin", true),
            s(2, "HQ7 (Disc 2).cue", false),
            s(2, "HQ7 (Disc 2).bin", true),
            s(3, "x.pkg", false),
            s(3, "x.rap", false),
            s(4, "HQ7.m3u", false),
        ];
        assert_eq!(best_coverage(&lines, true), (2, 2));
        assert_eq!(best_coverage(&lines[5..7], false), (2, 0));
    }
}
