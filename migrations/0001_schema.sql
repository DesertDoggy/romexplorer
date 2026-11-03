-- romexplorer database schema. Design: docs/plan/database.md (tables are laid out there in
-- the same low-level -> high-level order used here). MariaDB / InnoDB / utf8mb4 throughout.
-- Schema conventions (deleted/app_platform_id/*_hash columns): see CODING_RULES.md.

-- ---------------------------------------------------------------------------
-- reference
-- ---------------------------------------------------------------------------

CREATE TABLE sources (
    id   TINYINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    -- 'redump' | 'no-intro' | 'tosec' | 'retroachievements' | 'mame-redump' | 'retrobios'
    name VARCHAR(32) NOT NULL UNIQUE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Fixed, app-defined reference data (a compiled-in enum): no `deleted`, only ever changed
-- by an app update or manual repair.
CREATE TABLE platforms (
    id            SMALLINT UNSIGNED PRIMARY KEY,   -- common::models::Platform as u16
    igdb_id       SMALLINT UNSIGNED NULL UNIQUE,   -- cross-reference for querying IGDB; NULL if none
    name          VARCHAR(128) NOT NULL,
    family        VARCHAR(32) NULL,                -- nintendo / sega / sony / ...
    is_disc_based BOOLEAN NOT NULL DEFAULT FALSE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- ---------------------------------------------------------------------------
-- content: deduped, hash-identified payloads
-- ---------------------------------------------------------------------------

CREATE TABLE content (
    id              BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    deleted         BOOLEAN NOT NULL DEFAULT FALSE,
    app_platform_id SMALLINT UNSIGNED NOT NULL DEFAULT 0,
    -- Filename as the reference DAT names it; the on-disk/extracted name depends on the parent.
    file_name       VARCHAR(512) NULL,
    size            BIGINT UNSIGNED NULL,           -- some sources' disk entries carry no size
    headered        BOOLEAN NULL,
    encrypted       BOOLEAN NULL,
    byte_order      ENUM('big','byte_swapped','little') NULL,
    media_type      VARCHAR(32) NULL,               -- common::models::MediaType
    partial         BOOLEAN NOT NULL DEFAULT FALSE, -- a partial file (one bin/raw track of a set)
    source_id       TINYINT UNSIGNED NULL,          -- the DAT source that last confirmed this
    first_seen_at   DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT fk_content_platform FOREIGN KEY (app_platform_id) REFERENCES platforms(id),
    CONSTRAINT fk_content_source   FOREIGN KEY (source_id)       REFERENCES sources(id),
    INDEX idx_content_platform (app_platform_id),
    INDEX idx_content_file_name (file_name(191))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Platform-specific technical data (decryption scheme/key, ...). Our own JSON, 1:1, optional.
CREATE TABLE content_platform_data (
    content_id BIGINT UNSIGNED PRIMARY KEY,
    data       JSON NOT NULL,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
    CONSTRAINT fk_cpd_content FOREIGN KEY (content_id) REFERENCES content(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- What a container's bytes decode into (archive -> members); self-referencing.
CREATE TABLE content_contents (
    parent_content_id BIGINT UNSIGNED NOT NULL,
    child_content_id  BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (parent_content_id, child_content_id),
    CONSTRAINT fk_cc_parent FOREIGN KEY (parent_content_id) REFERENCES content(id),
    CONSTRAINT fk_cc_child  FOREIGN KEY (child_content_id)  REFERENCES content(id),
    INDEX idx_cc_child (child_content_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE hashes (
    id         BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    -- NULL only for rom-level hashes (ra_hash), which attach via rom_rahash instead.
    content_id BIGINT UNSIGNED NULL,
    algorithm  ENUM('crc32','md5','sha1','sha256','xxh3_128','ra_hash') NOT NULL,
    value_hex  VARCHAR(64) NOT NULL,               -- lowercase hex
    UNIQUE KEY uq_hashes_content_algo (content_id, algorithm),
    -- The actual hash -> content lookup path.
    INDEX idx_hashes_algo_value (algorithm, value_hex),
    CONSTRAINT fk_hashes_content FOREIGN KEY (content_id) REFERENCES content(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- ---------------------------------------------------------------------------
-- ownership: what's actually on THIS machine's disk (local_*)
-- ---------------------------------------------------------------------------
-- local_ / remote_ convention (docs/plan/api.md): every machine's own ownership rows live
-- in local_*; a machine that has synced another machine's collection holds a read-only
-- remote_* mirror of these same tables. Catalog/reference tables (content, hashes,
-- release, title, ...) are shared and never prefixed. Scan/hash/convert/archive/copy only
-- ever write local_*.

-- A collection root, or a subdirectory inside one that needs its own dir_type/platform/
-- archival setting different from its nearest configured ancestor (self-referencing --
-- an unconfigured subdirectory simply inherits, no row needed for every directory on
-- disk). The app never scans/writes above a row with is_collection_root = TRUE.
CREATE TABLE local_directories (
    id                  BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    parent_directory_id BIGINT UNSIGNED NULL,            -- NULL: a collection root
    path                VARCHAR(1024) NOT NULL,           -- real, absolute (not root+relative)
    path_hash           CHAR(40) NOT NULL UNIQUE,         -- sha1(path)
    is_collection_root  BOOLEAN NOT NULL DEFAULT FALSE,
    dir_type            ENUM('bios','rom','mixed') NULL,  -- NULL: inherit from nearest configured ancestor
    app_platform_id     SMALLINT UNSIGNED NULL,           -- NULL: inherit/unresolved; set: force, like PlatformContext::Forced
    is_archival         BOOLEAN NOT NULL DEFAULT FALSE,   -- excluded from the main collection list (query-time, not denormalized)
    deleted             BOOLEAN NOT NULL DEFAULT FALSE,
    CONSTRAINT fk_ldir_parent   FOREIGN KEY (parent_directory_id) REFERENCES local_directories(id),
    CONSTRAINT fk_ldir_platform FOREIGN KEY (app_platform_id)     REFERENCES platforms(id),
    INDEX idx_ldir_parent (parent_directory_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE local_files (
    id                   BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    path                 VARCHAR(1024) NOT NULL,
    path_hash            CHAR(40) NOT NULL UNIQUE,  -- sha1(path)
    content_id           BIGINT UNSIGNED NOT NULL,  -- always set: an archive's own bytes are content too
    directory_id         BIGINT UNSIGNED NULL,      -- which configured local_directories row governs this file; NULL if none (e.g. synthetic/virtual_collection rows)
    archive_format       VARCHAR(16) NULL,          -- common::models::ArchiveFormat tag
    size                 BIGINT UNSIGNED NOT NULL,
    deleted              BOOLEAN NOT NULL DEFAULT FALSE,   -- not found on disk at last scan
    last_modified        DATETIME NULL,
    last_integrity_check DATETIME NULL,
    last_full_check      DATETIME NULL,
    CONSTRAINT fk_lfiles_content   FOREIGN KEY (content_id)   REFERENCES content(id),
    CONSTRAINT fk_lfiles_directory FOREIGN KEY (directory_id) REFERENCES local_directories(id),
    INDEX idx_lfiles_content (content_id),
    INDEX idx_lfiles_directory (directory_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- One playable unit: a single ROM file, a cue/gdi+bin set, a directory dump.
CREATE TABLE local_rom (
    id              BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    app_platform_id SMALLINT UNSIGNED NOT NULL DEFAULT 0,  -- as-detected (scan heuristic, then DAT-confirmed)
    disc_number     SMALLINT UNSIGNED NULL,
    main_path       VARCHAR(1024) NOT NULL,
    alt             BOOLEAN NOT NULL DEFAULT FALSE,        -- alternate dump of the same disc ("(Alt)")
    deleted         BOOLEAN NOT NULL DEFAULT FALSE,
    CONSTRAINT fk_lrom_platform FOREIGN KEY (app_platform_id) REFERENCES platforms(id),
    INDEX idx_lrom_platform (app_platform_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE local_rom_files (
    rom_id  BIGINT UNSIGNED NOT NULL,
    file_id BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (rom_id, file_id),
    CONSTRAINT fk_lrf_rom  FOREIGN KEY (rom_id)  REFERENCES local_rom(id),
    CONSTRAINT fk_lrf_file FOREIGN KEY (file_id) REFERENCES local_files(id),
    INDEX idx_lrf_file (file_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- RetroAchievements' hash is per playable unit (can span files), so it attaches here, to a
-- `hashes` row with algorithm='ra_hash' and content_id NULL.
CREATE TABLE local_rom_rahash (
    rom_id  BIGINT UNSIGNED NOT NULL,
    hash_id BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (rom_id, hash_id),
    CONSTRAINT fk_lrr_rom  FOREIGN KEY (rom_id)  REFERENCES local_rom(id),
    CONSTRAINT fk_lrr_hash FOREIGN KEY (hash_id) REFERENCES hashes(id),
    INDEX idx_lrr_hash (hash_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- One owned file-set (complete or not), never merged in storage. No platform column: derived
-- via local_game_roms -> local_rom.
CREATE TABLE local_game (
    id               BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    name             VARCHAR(512) NOT NULL,          -- as scanned; catalog names live on release/title
    playlist_file_id BIGINT UNSIGNED NULL,           -- the .m3u, when one exists
    region           VARCHAR(128) NULL,
    category         VARCHAR(64) NULL,
    cover_media_id   BIGINT UNSIGNED NULL,           -- which media row to show (FK added below)
    deleted          BOOLEAN NOT NULL DEFAULT FALSE,
    CONSTRAINT fk_lgame_playlist FOREIGN KEY (playlist_file_id) REFERENCES local_files(id),
    INDEX idx_lgame_name (name(191))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE local_game_roms (
    game_id BIGINT UNSIGNED NOT NULL,
    rom_id  BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (game_id, rom_id),
    CONSTRAINT fk_lgr_game FOREIGN KEY (game_id) REFERENCES local_game(id),
    CONSTRAINT fk_lgr_rom  FOREIGN KEY (rom_id)  REFERENCES local_rom(id),
    INDEX idx_lgr_rom (rom_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- ---------------------------------------------------------------------------
-- catalog: DAT-level releases and abstract titles
-- ---------------------------------------------------------------------------

-- One shipped SKU as a whole (all discs), not literally one DAT line.
CREATE TABLE `release` (
    id                BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    app_platform_id   SMALLINT UNSIGNED NOT NULL DEFAULT 0,   -- catalog-canonical
    name              VARCHAR(512) NOT NULL,                  -- as the source DAT/provider gives it
    name_hash         CHAR(40) NOT NULL,                      -- sha1(name), for the unique key
    norm_name         VARCHAR(512) NOT NULL,                  -- matching::normalize_title(base title)
    norm_hash         CHAR(40) NOT NULL,                      -- sha1(norm_name)
    region            VARCHAR(128) NULL,                      -- structured <release region> preferred, else parsed
    languages         VARCHAR(128) NULL,                      -- comma-separated codes
    revision          VARCHAR(32) NULL,
    year              SMALLINT UNSIGNED NULL,
    released_on       DATE NULL,
    serial            VARCHAR(64) NULL,
    category          VARCHAR(64) NULL,                       -- DAT <category> (Games/Retail/BIOS/...)
    disc_count        SMALLINT UNSIGNED NOT NULL DEFAULT 1,
    min_players       SMALLINT UNSIGNED NULL,
    max_players       SMALLINT UNSIGNED NULL,
    source_id         TINYINT UNSIGNED NULL,
    is_beta           BOOLEAN NOT NULL DEFAULT FALSE,
    is_prototype      BOOLEAN NOT NULL DEFAULT FALSE,
    is_demo           BOOLEAN NOT NULL DEFAULT FALSE,
    is_unlicensed     BOOLEAN NOT NULL DEFAULT FALSE,
    community_patched BOOLEAN NOT NULL DEFAULT FALSE,
    patch_type        ENUM('gameplay','translation') NULL,
    deleted           BOOLEAN NOT NULL DEFAULT FALSE,
    CONSTRAINT fk_release_platform FOREIGN KEY (app_platform_id) REFERENCES platforms(id),
    CONSTRAINT fk_release_source   FOREIGN KEY (source_id)       REFERENCES sources(id),
    -- Re-importing a refreshed DAT updates in place instead of duplicating.
    UNIQUE KEY uq_release_identity (source_id, app_platform_id, name_hash),
    INDEX idx_release_name (name(191)),
    INDEX idx_release_norm (norm_hash),
    INDEX idx_release_platform (app_platform_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Catalog side of "which content is this release made of" (the DAT's rom/disk lines).
CREATE TABLE release_contents (
    release_id BIGINT UNSIGNED NOT NULL,
    content_id BIGINT UNSIGNED NOT NULL,
    file_name  VARCHAR(512) NOT NULL,
    PRIMARY KEY (release_id, content_id),
    CONSTRAINT fk_rc_release FOREIGN KEY (release_id) REFERENCES `release`(id),
    CONSTRAINT fk_rc_content FOREIGN KEY (content_id) REFERENCES content(id),
    INDEX idx_rc_content (content_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- An owned game matches one release as a whole. Ownership-side (points at local_game), so
-- it carries the local_ prefix even though it also references the shared `release`.
CREATE TABLE local_release_games (
    release_id BIGINT UNSIGNED NOT NULL,
    game_id    BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (release_id, game_id),
    CONSTRAINT fk_lrg_release FOREIGN KEY (release_id) REFERENCES `release`(id),
    CONSTRAINT fk_lrg_game    FOREIGN KEY (game_id)    REFERENCES local_game(id),
    INDEX idx_lrg_game (game_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- A provider's own serial for this release (release.serial stays DAT-sourced, unchanged).
-- One row per provider: never overwritten, never merged with the DAT serial.
CREATE TABLE release_provider_serials (
    release_id BIGINT UNSIGNED NOT NULL,
    provider   VARCHAR(32) NOT NULL,
    serial     VARCHAR(64) NOT NULL,
    PRIMARY KEY (release_id, provider),
    CONSTRAINT fk_rps_release FOREIGN KEY (release_id) REFERENCES `release`(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Per-region age rating boards. Multi-value by nature (a release can carry several).
CREATE TABLE release_ratings (
    release_id BIGINT UNSIGNED NOT NULL,
    board      ENUM('esrb','pegi','cero','usk','acb','grac','oflc') NOT NULL,
    rating     VARCHAR(16) NOT NULL,
    PRIMARY KEY (release_id, board),
    CONSTRAINT fk_rr_release FOREIGN KEY (release_id) REFERENCES `release`(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Abstract, region-independent identity (same gameplay content). No platform column.
CREATE TABLE title (
    id                BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    name              VARCHAR(512) NOT NULL,
    norm_name         VARCHAR(512) NOT NULL,
    norm_hash         CHAR(40) NOT NULL,
    category          ENUM('base_game','port','remake','remaster','dlc','expansion','bundle','mod') NOT NULL DEFAULT 'base_game',
    basegame_title_id BIGINT UNSIGNED NULL,
    deleted           BOOLEAN NOT NULL DEFAULT FALSE,
    CONSTRAINT fk_title_basegame FOREIGN KEY (basegame_title_id) REFERENCES title(id),
    INDEX idx_title_name (name(191)),
    INDEX idx_title_norm (norm_hash),
    INDEX idx_title_basegame (basegame_title_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE title_releases (
    title_id   BIGINT UNSIGNED NOT NULL,
    release_id BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (title_id, release_id),
    CONSTRAINT fk_tr_title   FOREIGN KEY (title_id)   REFERENCES title(id),
    CONSTRAINT fk_tr_release FOREIGN KEY (release_id) REFERENCES `release`(id),
    INDEX idx_tr_release (release_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Localized title names. Keyed generically enough that a later JA-for-US/EU-release
-- pass, or any other language/region/source combination, is just inserted rows.
CREATE TABLE title_locales (
    title_id BIGINT UNSIGNED NOT NULL,
    language VARCHAR(8) NOT NULL,    -- matches common::models::Language's own codes
    region   VARCHAR(16) NOT NULL DEFAULT '',  -- disambiguates same-language variants (e.g. EUR vs ASIA text); '' when irrelevant
    source   VARCHAR(32) NOT NULL,
    name     VARCHAR(512) NOT NULL,
    PRIMARY KEY (title_id, language, region, source),
    CONSTRAINT fk_tl_title FOREIGN KEY (title_id) REFERENCES title(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Self-referencing: a subgenre's parent is its major genre, a major genre's parent is
-- NULL. No fixed tier count -- a future finer tier needs no migration.
CREATE TABLE genre (
    id              SMALLINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    name            VARCHAR(64) NOT NULL UNIQUE,
    parent_genre_id SMALLINT UNSIGNED NULL,
    deleted         BOOLEAN NOT NULL DEFAULT FALSE,
    CONSTRAINT fk_genre_parent FOREIGN KEY (parent_genre_id) REFERENCES genre(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Which canonical genre (at any tier) a source's own raw genre text means. Generalizes
-- ra_genre.rs's hardcoded keyword rules into data; RA becomes this table's first
-- populated source, not a special case.
CREATE TABLE genre_raw_mapping (
    source   VARCHAR(32) NOT NULL,
    raw_text VARCHAR(256) NOT NULL,
    genre_id SMALLINT UNSIGNED NOT NULL,
    PRIMARY KEY (source, raw_text),
    CONSTRAINT fk_grm_genre FOREIGN KEY (genre_id) REFERENCES genre(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE franchise (
    id      BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    name    VARCHAR(256) NOT NULL UNIQUE,
    deleted BOOLEAN NOT NULL DEFAULT FALSE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE company (
    id      BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    name    VARCHAR(256) NOT NULL,
    deleted BOOLEAN NOT NULL DEFAULT FALSE,
    UNIQUE KEY uq_company_name (name(191))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- source in the PK: the same title's genre from two sources are two rows, even when
-- both resolve to the same genre_id (e.g. IGDB "RPG" and No-Intro "RPG" both ->
-- canonical RPG) -- never merged, so nothing is ever overwritten across sources.
CREATE TABLE title_genres (
    title_id BIGINT UNSIGNED NOT NULL,
    genre_id SMALLINT UNSIGNED NOT NULL,
    source   VARCHAR(32) NOT NULL,
    PRIMARY KEY (title_id, genre_id, source),
    CONSTRAINT fk_tg_title FOREIGN KEY (title_id) REFERENCES title(id),
    CONSTRAINT fk_tg_genre FOREIGN KEY (genre_id) REFERENCES genre(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Player perspective (IGDB player_perspectives, MobyGames "Perspective" category):
-- kept as its own metadata, source-tagged; not a genre.
CREATE TABLE perspective (
    id   SMALLINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    name VARCHAR(64) NOT NULL UNIQUE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE title_perspectives (
    title_id       BIGINT UNSIGNED NOT NULL,
    perspective_id SMALLINT UNSIGNED NOT NULL,
    source         VARCHAR(32) NOT NULL,
    PRIMARY KEY (title_id, perspective_id, source),
    CONSTRAINT fk_tp_title       FOREIGN KEY (title_id)       REFERENCES title(id),
    CONSTRAINT fk_tp_perspective FOREIGN KEY (perspective_id) REFERENCES perspective(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE title_franchises (
    title_id     BIGINT UNSIGNED NOT NULL,
    franchise_id BIGINT UNSIGNED NOT NULL,
    source       VARCHAR(32) NOT NULL,
    PRIMARY KEY (title_id, franchise_id, source),
    CONSTRAINT fk_tf_title     FOREIGN KEY (title_id)     REFERENCES title(id),
    CONSTRAINT fk_tf_franchise FOREIGN KEY (franchise_id) REFERENCES franchise(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE title_companies (
    title_id   BIGINT UNSIGNED NOT NULL,
    company_id BIGINT UNSIGNED NOT NULL,
    role       ENUM('developer','publisher','porting_studio') NOT NULL,
    source     VARCHAR(32) NOT NULL,
    PRIMARY KEY (title_id, company_id, role, source),
    CONSTRAINT fk_tc_title   FOREIGN KEY (title_id)   REFERENCES title(id),
    CONSTRAINT fk_tc_company FOREIGN KEY (company_id) REFERENCES company(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- ---------------------------------------------------------------------------
-- collection sets: user-made, ordered lists of releases -- a playlist -- for copying a
-- whole set to another machine, or just for grouping (docs/plan/api.md)
-- ---------------------------------------------------------------------------

CREATE TABLE collection_sets (
    id          BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    name        VARCHAR(256) NOT NULL,
    description TEXT NULL,
    created_at  DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at  DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
    deleted     BOOLEAN NOT NULL DEFAULT FALSE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE collection_set_releases (
    set_id     BIGINT UNSIGNED NOT NULL,
    release_id BIGINT UNSIGNED NOT NULL,
    position   INT UNSIGNED NOT NULL,
    added_at   DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (set_id, release_id),
    CONSTRAINT fk_csr_set     FOREIGN KEY (set_id)     REFERENCES collection_sets(id),
    CONSTRAINT fk_csr_release FOREIGN KEY (release_id) REFERENCES `release`(id),
    INDEX idx_csr_release (release_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- ---------------------------------------------------------------------------
-- media: files on disk, attached to catalog tiers (never owned by game)
-- ---------------------------------------------------------------------------

CREATE TABLE media (
    id           BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    media_type   ENUM('cover','box','box_3d','title_screen','screenshot','video','bgm','fanart','logo','icon') NOT NULL,
    sub_category ENUM('front','back','top','bottom','left','right','spine') NULL,
    path         VARCHAR(1024) NOT NULL,
    source       VARCHAR(32) NULL,                   -- provider it came from
    source_url   VARCHAR(1024) NULL,
    deleted      BOOLEAN NOT NULL DEFAULT FALSE,
    INDEX idx_media_type (media_type, sub_category)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

ALTER TABLE local_game ADD CONSTRAINT fk_lgame_cover FOREIGN KEY (cover_media_id) REFERENCES media(id);

CREATE TABLE title_media (
    title_id BIGINT UNSIGNED NOT NULL,
    media_id BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (title_id, media_id),
    CONSTRAINT fk_tm_title FOREIGN KEY (title_id) REFERENCES title(id),
    CONSTRAINT fk_tm_media FOREIGN KEY (media_id) REFERENCES media(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE release_media (
    release_id BIGINT UNSIGNED NOT NULL,
    media_id   BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (release_id, media_id),
    CONSTRAINT fk_rm_release FOREIGN KEY (release_id) REFERENCES `release`(id),
    CONSTRAINT fk_rm_media   FOREIGN KEY (media_id)   REFERENCES media(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Media attachment rule: images and videos belong to releases (a provider's title-level
-- cover is downloaded once and linked to the title's releases; region-specific art
-- overrides per release), audio belongs to a title (its soundtrack), a franchise or a
-- keyword (arrangement tracks spanning games -- e.g. one keyword covering an original and
-- its remake).
CREATE TABLE franchise_media (
    franchise_id BIGINT UNSIGNED NOT NULL,
    media_id     BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (franchise_id, media_id),
    CONSTRAINT fk_fm_franchise FOREIGN KEY (franchise_id) REFERENCES franchise(id),
    CONSTRAINT fk_fm_media     FOREIGN KEY (media_id)     REFERENCES media(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Keywords: free tags across titles (user-defined, or a provider's keywords); the unit
-- audio that isn't one game's soundtrack attaches to.
CREATE TABLE keyword (
    id   BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    name VARCHAR(191) NOT NULL,
    UNIQUE KEY uq_keyword_name (name)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE title_keywords (
    title_id   BIGINT UNSIGNED NOT NULL,
    keyword_id BIGINT UNSIGNED NOT NULL,
    source     VARCHAR(32) NOT NULL DEFAULT 'user',
    PRIMARY KEY (title_id, keyword_id, source),
    CONSTRAINT fk_tk_title   FOREIGN KEY (title_id)   REFERENCES title(id),
    CONSTRAINT fk_tk_keyword FOREIGN KEY (keyword_id) REFERENCES keyword(id),
    INDEX idx_tk_keyword (keyword_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE keyword_media (
    keyword_id BIGINT UNSIGNED NOT NULL,
    media_id   BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (keyword_id, media_id),
    CONSTRAINT fk_km_keyword FOREIGN KEY (keyword_id) REFERENCES keyword(id),
    CONSTRAINT fk_km_media   FOREIGN KEY (media_id)   REFERENCES media(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- ---------------------------------------------------------------------------
-- provider caches (raw responses live on disk: raw_response_path/raw_format, NULL under
-- raw_response_retention='discard')
-- ---------------------------------------------------------------------------

-- PlayMatch (playmatch.retrorealm.dev): the second hash-lookup service, one row per content
-- like Hasheous; `match_type` is its gameMatchType (SHA256/SHA1/MD5/CRC/FileNameAndSize).
CREATE TABLE playmatch_lookups (
    content_id        BIGINT UNSIGNED PRIMARY KEY,
    raw_response_path VARCHAR(1024) NULL,
    raw_format        VARCHAR(16) NULL DEFAULT 'json',
    hit               BOOLEAN NOT NULL DEFAULT FALSE,
    match_type        VARCHAR(32) NULL,
    fetched_at        DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted           BOOLEAN NOT NULL DEFAULT FALSE,
    CONSTRAINT fk_pl_content FOREIGN KEY (content_id) REFERENCES content(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE hasheous_lookups (
    content_id        BIGINT UNSIGNED PRIMARY KEY,
    raw_response_path VARCHAR(1024) NULL,
    raw_format        VARCHAR(16) NULL DEFAULT 'json',
    hit               BOOLEAN NOT NULL DEFAULT FALSE,
    fetched_at        DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted           BOOLEAN NOT NULL DEFAULT FALSE,
    CONSTRAINT fk_hl_content FOREIGN KEY (content_id) REFERENCES content(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Title-keyed
CREATE TABLE igdb_games (
    provider_id       INT UNSIGNED PRIMARY KEY,
    raw_response_path VARCHAR(1024) NULL,
    raw_format        VARCHAR(16) NULL DEFAULT 'json',
    title             VARCHAR(512) NULL,
    fetched_at        DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted           BOOLEAN NOT NULL DEFAULT FALSE,
    INDEX idx_igdb_title (title(191))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE mobygames_games (
    provider_id       BIGINT UNSIGNED PRIMARY KEY,
    raw_response_path VARCHAR(1024) NULL,
    raw_format        VARCHAR(16) NULL DEFAULT 'json',
    title             VARCHAR(512) NULL,
    fetched_at        DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted           BOOLEAN NOT NULL DEFAULT FALSE,
    INDEX idx_moby_title (title(191))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE emuready_games (
    provider_id       VARCHAR(64) PRIMARY KEY,       -- EmuReady ids are opaque strings
    raw_response_path VARCHAR(1024) NULL,
    raw_format        VARCHAR(16) NULL DEFAULT 'json',
    title             VARCHAR(512) NULL,
    fetched_at        DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted           BOOLEAN NOT NULL DEFAULT FALSE,
    INDEX idx_emuready_title (title(191))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE emuready_reports (
    emuready_game_id  VARCHAR(64) NOT NULL,
    device            VARCHAR(128) NOT NULL,
    emulator          VARCHAR(64) NOT NULL,
    rating            VARCHAR(32) NULL,
    raw_response_path VARCHAR(1024) NULL,
    raw_format        VARCHAR(16) NULL DEFAULT 'json',
    fetched_at        DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (emuready_game_id, device, emulator),
    CONSTRAINT fk_er_game FOREIGN KEY (emuready_game_id) REFERENCES emuready_games(provider_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE steamgriddb_games (
    provider_id       BIGINT UNSIGNED PRIMARY KEY,
    raw_response_path VARCHAR(1024) NULL,
    raw_format        VARCHAR(16) NULL DEFAULT 'json',
    title             VARCHAR(512) NULL,
    fetched_at        DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted           BOOLEAN NOT NULL DEFAULT FALSE,
    INDEX idx_sgdb_title (title(191))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE screenscraper_games (
    provider_id       BIGINT UNSIGNED PRIMARY KEY,   -- the "jeu" id
    raw_response_path VARCHAR(1024) NULL,
    raw_format        VARCHAR(16) NULL DEFAULT 'json',
    title             VARCHAR(512) NULL,
    fetched_at        DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted           BOOLEAN NOT NULL DEFAULT FALSE,
    INDEX idx_ss_title (title(191))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- ScreenScraper's lookup API is hash-first: content -> jeu, so a content row resolves
-- without re-querying.
CREATE TABLE screenscraper_rom_index (
    content_id BIGINT UNSIGNED PRIMARY KEY,
    jeu_id     BIGINT UNSIGNED NOT NULL,
    fetched_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT fk_ssri_content FOREIGN KEY (content_id) REFERENCES content(id),
    CONSTRAINT fk_ssri_jeu     FOREIGN KEY (jeu_id)     REFERENCES screenscraper_games(provider_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Release-keyed
CREATE TABLE thegamesdb_games (
    provider_id       BIGINT UNSIGNED PRIMARY KEY,
    raw_response_path VARCHAR(1024) NULL,
    raw_format        VARCHAR(16) NULL DEFAULT 'json',
    title             VARCHAR(512) NULL,
    fetched_at        DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted           BOOLEAN NOT NULL DEFAULT FALSE,
    INDEX idx_tgdb_title (title(191))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE retroachievements_games (
    provider_id       INT UNSIGNED PRIMARY KEY,      -- RA game id
    console_id        INT UNSIGNED NULL,             -- RA console id
    raw_response_path VARCHAR(1024) NULL,
    raw_format        VARCHAR(16) NULL DEFAULT 'json',
    title             VARCHAR(512) NULL,
    num_achievements  INT UNSIGNED NULL,
    genre_raw         VARCHAR(256) NULL,             -- RA's fine-grained genre text
    fetched_at        DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted           BOOLEAN NOT NULL DEFAULT FALSE,
    INDEX idx_ra_title (title(191)),
    INDEX idx_ra_console (console_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- RA's bulk per-console hash list (API_GetGameList withHashes): md5 -> RA game.
CREATE TABLE retroachievements_hashes (
    md5        CHAR(32) PRIMARY KEY,
    ra_game_id INT UNSIGNED NOT NULL,
    console_id INT UNSIGNED NOT NULL,
    fetched_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_rah_game (ra_game_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE title_provider_links (
    title_id    BIGINT UNSIGNED NOT NULL,
    provider    VARCHAR(32) NOT NULL,
    provider_id VARCHAR(64) NOT NULL,
    -- exact: the provider's own hash list matched; hash: a hash-lookup service's (Hasheous/
    -- PlayMatch) cross-reference for the identified game; name: a name search whose
    -- normalized name equals ours; fuzzy: a name search's best guess; manual: the user.
    confidence  ENUM('exact','hash','name','fuzzy','manual') NOT NULL,
    linked_at   DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted     BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (title_id, provider),
    CONSTRAINT fk_tpl_title FOREIGN KEY (title_id) REFERENCES title(id),
    INDEX idx_tpl_provider (provider, provider_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE release_provider_links (
    release_id  BIGINT UNSIGNED NOT NULL,
    provider    VARCHAR(32) NOT NULL,
    provider_id VARCHAR(64) NOT NULL,
    -- exact: the provider's own hash list matched; hash: a hash-lookup service's (Hasheous/
    -- PlayMatch) cross-reference for the identified game; name: a name search whose
    -- normalized name equals ours; fuzzy: a name search's best guess; manual: the user.
    confidence  ENUM('exact','hash','name','fuzzy','manual') NOT NULL,
    linked_at   DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted     BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (release_id, provider),
    CONSTRAINT fk_rpl_release FOREIGN KEY (release_id) REFERENCES `release`(id),
    INDEX idx_rpl_provider (provider, provider_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- ---------------------------------------------------------------------------
-- app infrastructure: media/cache/temp storage, independent of any collection root
-- ---------------------------------------------------------------------------

-- Permanent media storage plus an optional tiered cache in front of it. tier1_cache/
-- tier2_cache are cardinality-limited to one active row each -- enforced at the Rust
-- layer (MediaDirs::set_tier_cache replaces rather than inserts), not by this table.
-- image/video/audio are genuinely multi-row (several directories per kind allowed).
CREATE TABLE media_dirs (
    id      BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    path    VARCHAR(1024) NOT NULL,
    role    ENUM('tier1_cache','tier2_cache','image','video','audio') NOT NULL,
    deleted BOOLEAN NOT NULL DEFAULT FALSE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Transient scratch space: Single's convert/archive intermediates, and an optional
-- pre-import staging area for provider raw responses. Tier 1 tried first, tier 2 as
-- fallback -- same "at most one row per tier" Rust-level rule as media_dirs' cache tiers.
CREATE TABLE temp_dirs (
    id      BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    path    VARCHAR(1024) NOT NULL,
    tier    TINYINT UNSIGNED NOT NULL,
    deleted BOOLEAN NOT NULL DEFAULT FALSE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- ---------------------------------------------------------------------------
-- operational
-- ---------------------------------------------------------------------------

CREATE TABLE jobs (
    id         BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    job_type   ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize') NOT NULL,
    status     ENUM('pending','in_progress','done','failed') NOT NULL DEFAULT 'pending',
    payload    JSON NOT NULL,
    attempts   SMALLINT UNSIGNED NOT NULL DEFAULT 0,
    -- 0 = background (scans, bulk syncs, hash-match follow-ups); higher runs first and in
    -- its lane's fast slot, e.g. the metadata/art refresh for a page the user just opened.
    priority   SMALLINT UNSIGNED NOT NULL DEFAULT 0,
    last_error TEXT NULL,
    claimed_at DATETIME NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
    INDEX idx_jobs_type_status (job_type, status),
    INDEX idx_jobs_pending (status, priority, id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE provider_quota (
    provider   VARCHAR(32) PRIMARY KEY,
    remaining  INT NULL,
    resets_at  DATETIME NULL,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- The (previously undecided) settings mechanism: one JSON value per key. Keys in use:
-- 'provider_priority' (ordered array of provider names), 'provider_sync_scope' (object:
-- provider -> single|list|full|by_platform|by_keyword), 'raw_response_retention'
-- (keep_all|keep_latest|discard), 'cache_ttl_days'.
CREATE TABLE settings (
    `key`      VARCHAR(64) PRIMARY KEY,
    value      JSON NOT NULL,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Which DAT files have been imported (idempotent re-import, dat_sync staleness).
CREATE TABLE dat_imports (
    id              BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    source_id       TINYINT UNSIGNED NOT NULL,
    app_platform_id SMALLINT UNSIGNED NOT NULL DEFAULT 0,
    file_name       VARCHAR(512) NOT NULL,          -- pack member name or file name
    file_hash       CHAR(40) NOT NULL,              -- sha1(file_name)
    dat_name        VARCHAR(512) NULL,              -- <header><name>
    dat_version     VARCHAR(64) NULL,
    game_count      INT UNSIGNED NOT NULL DEFAULT 0,
    file_count      INT UNSIGNED NOT NULL DEFAULT 0,
    imported_at     DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE KEY uq_dat_imports (source_id, file_hash),
    CONSTRAINT fk_di_source   FOREIGN KEY (source_id)       REFERENCES sources(id),
    CONSTRAINT fk_di_platform FOREIGN KEY (app_platform_id) REFERENCES platforms(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
