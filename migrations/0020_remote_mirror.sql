-- The remote_* mirror: other machines' ownership tables, imported as-is by CatalogSync.
--
-- Mechanical rule (docs/plan/api.md): prefix + the exact source table name, ownership
-- tables only. One machine's local_* IS another machine's remote_* once it syncs -- same
-- shape, different role, not two schemas. The shared catalog/reference tables (content,
-- hashes, `release`, title, platforms, sources, genre/franchise/company, the provider
-- caches) are never mirrored: a remote_game row references the very same release_id a
-- local_game row would, once sync has upserted whatever reference rows this machine
-- didn't already have.
--
-- SEVERAL sources, one set of tables. A machine syncs from its main server and may also
-- sync from peers (any client that enables sharing serves the same export). That is a
-- `source_id` column, NOT a table per source: per-source table names would mean DDL at
-- sync time, an unbounded number of tables, and every catalog query becoming dynamic SQL
-- or a UNION over a set unknown at compile time -- destroying the one property this
-- mirror exists for, "do I have it / does the server" as a single join.
--
-- Four deliberate differences from the local_* originals:
--
-- 1. No AUTO_INCREMENT, and composite (source_id, id) primary keys. These ids are the
--    *source machine's* ids, copied verbatim so the junction rows line up without a
--    translation table -- which means two sources WILL collide on id. Nothing here ever
--    generates an id of its own.
-- 2. Internal references carry source_id too (remote_files.directory_id ->
--    remote_directories, remote_game.playlist_file_id -> remote_files), so a row can
--    never point into a different source's rows, and a wholesale per-source replace
--    can't dangle into local_* either.
-- 3. UNIQUE path_hash becomes UNIQUE (source_id, path_hash): a re-sync stays an
--    idempotent upsert per source, while two machines may legitimately hold the same path.
-- 4. remote_game.cover_media_id carries no foreign key. media rows describe files this
--    machine downloaded; the source machine's media ids mean nothing locally. The column
--    is kept for copy fidelity (and so a later "fetch that art too" has something to go
--    on), but it is deliberately unenforced and may point at nothing.
--
-- Constraint and index names are the local_* ones with l -> r, matching the `l...`
-- prefixing 0001_schema.sql already adopted so the two sets can coexist.
--
-- File manipulation (scan, hash, convert, archive, copy) NEVER targets these tables --
-- remote_* is a read-only cache, written only by sync.

-- Where a mirror's rows came from. `role` decides how ownership is presented: the ONE
-- main server is what "not backed up yet" is measured against, peers are informational.
-- At most one main, enforced here rather than in code: main_role is NULL for every peer
-- and for disabled/soft-deleted rows, and NULLs don't collide in a UNIQUE index.
CREATE TABLE remote_sources (
    id            SMALLINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    name          VARCHAR(128) NOT NULL,
    base_url      VARCHAR(512) NOT NULL,          -- http://host:port -- no trailing /api/v1
    -- The same bearer token romexplorer_server's own middleware checks, because a peer is
    -- just another romexplorer_server: a client that shares its collection serves the very
    -- same authenticated API. Kept as an opaque string deliberately -- if this later
    -- becomes "sign in with a user and password, keep what you get back", only how the
    -- value is obtained changes, not this column or anything reading it.
    token         VARCHAR(255) NULL,
    role          ENUM('main','peer') NOT NULL DEFAULT 'peer',
    enabled       BOOLEAN NOT NULL DEFAULT TRUE,
    deleted       BOOLEAN NOT NULL DEFAULT FALSE,
    last_sync_at  DATETIME NULL,
    created_at    DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    main_role     TINYINT UNSIGNED AS (IF(role = 'main' AND NOT deleted, 1, NULL)) VIRTUAL,
    UNIQUE KEY uq_rsrc_main (main_role),
    UNIQUE KEY uq_rsrc_base (base_url)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE remote_directories (
    source_id           SMALLINT UNSIGNED NOT NULL,
    id                  BIGINT UNSIGNED NOT NULL,
    parent_directory_id BIGINT UNSIGNED NULL,
    path                VARCHAR(1024) NOT NULL,
    path_hash           CHAR(40) NOT NULL,
    is_collection_root  BOOLEAN NOT NULL DEFAULT FALSE,
    dir_type            ENUM('bios','rom','mixed','patch') NULL,
    app_platform_id     SMALLINT UNSIGNED NULL,
    is_archival         BOOLEAN NOT NULL DEFAULT FALSE,
    deleted             BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (source_id, id),
    UNIQUE KEY uq_rdir_path (source_id, path_hash),
    CONSTRAINT fk_rdir_source   FOREIGN KEY (source_id) REFERENCES remote_sources(id),
    CONSTRAINT fk_rdir_parent   FOREIGN KEY (source_id, parent_directory_id) REFERENCES remote_directories(source_id, id),
    CONSTRAINT fk_rdir_platform FOREIGN KEY (app_platform_id) REFERENCES platforms(id),
    INDEX idx_rdir_parent (source_id, parent_directory_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE remote_files (
    source_id            SMALLINT UNSIGNED NOT NULL,
    id                   BIGINT UNSIGNED NOT NULL,
    path                 VARCHAR(1024) NOT NULL,
    path_hash            CHAR(40) NOT NULL,
    content_id           BIGINT UNSIGNED NOT NULL,
    directory_id         BIGINT UNSIGNED NULL,
    archive_format       VARCHAR(16) NULL,
    size                 BIGINT UNSIGNED NOT NULL,
    deleted              BOOLEAN NOT NULL DEFAULT FALSE,
    last_modified        DATETIME NULL,
    last_integrity_check DATETIME NULL,
    last_full_check      DATETIME NULL,
    PRIMARY KEY (source_id, id),
    UNIQUE KEY uq_rfiles_path (source_id, path_hash),
    CONSTRAINT fk_rfiles_source    FOREIGN KEY (source_id)  REFERENCES remote_sources(id),
    CONSTRAINT fk_rfiles_content   FOREIGN KEY (content_id) REFERENCES content(id),
    CONSTRAINT fk_rfiles_directory FOREIGN KEY (source_id, directory_id) REFERENCES remote_directories(source_id, id),
    INDEX idx_rfiles_content (content_id),
    INDEX idx_rfiles_directory (source_id, directory_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE remote_rom (
    source_id       SMALLINT UNSIGNED NOT NULL,
    id              BIGINT UNSIGNED NOT NULL,
    app_platform_id SMALLINT UNSIGNED NOT NULL DEFAULT 0,
    disc_number     SMALLINT UNSIGNED NULL,
    main_path       VARCHAR(1024) NOT NULL,
    alt             BOOLEAN NOT NULL DEFAULT FALSE,
    deleted         BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (source_id, id),
    CONSTRAINT fk_rrom_source   FOREIGN KEY (source_id)       REFERENCES remote_sources(id),
    CONSTRAINT fk_rrom_platform FOREIGN KEY (app_platform_id) REFERENCES platforms(id),
    INDEX idx_rrom_platform (app_platform_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE remote_rom_files (
    source_id SMALLINT UNSIGNED NOT NULL,
    rom_id    BIGINT UNSIGNED NOT NULL,
    file_id   BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (source_id, rom_id, file_id),
    CONSTRAINT fk_rrf_rom  FOREIGN KEY (source_id, rom_id)  REFERENCES remote_rom(source_id, id),
    CONSTRAINT fk_rrf_file FOREIGN KEY (source_id, file_id) REFERENCES remote_files(source_id, id),
    INDEX idx_rrf_file (source_id, file_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE remote_rom_rahash (
    source_id SMALLINT UNSIGNED NOT NULL,
    rom_id    BIGINT UNSIGNED NOT NULL,
    hash_id   BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (source_id, rom_id, hash_id),
    CONSTRAINT fk_rrr_rom  FOREIGN KEY (source_id, rom_id) REFERENCES remote_rom(source_id, id),
    CONSTRAINT fk_rrr_hash FOREIGN KEY (hash_id)           REFERENCES hashes(id),
    INDEX idx_rrr_hash (hash_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE remote_game (
    source_id        SMALLINT UNSIGNED NOT NULL,
    id               BIGINT UNSIGNED NOT NULL,
    name             VARCHAR(512) NOT NULL,
    playlist_file_id BIGINT UNSIGNED NULL,
    region           VARCHAR(128) NULL,
    category         VARCHAR(64) NULL,
    cover_media_id   BIGINT UNSIGNED NULL,   -- the source machine's media id; no FK (see header)
    deleted          BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (source_id, id),
    CONSTRAINT fk_rgame_source   FOREIGN KEY (source_id) REFERENCES remote_sources(id),
    CONSTRAINT fk_rgame_playlist FOREIGN KEY (source_id, playlist_file_id) REFERENCES remote_files(source_id, id),
    INDEX idx_rgame_playlist (source_id, playlist_file_id),
    INDEX idx_rgame_name (name(191))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE remote_game_roms (
    source_id SMALLINT UNSIGNED NOT NULL,
    game_id   BIGINT UNSIGNED NOT NULL,
    rom_id    BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (source_id, game_id, rom_id),
    CONSTRAINT fk_rgr_game FOREIGN KEY (source_id, game_id) REFERENCES remote_game(source_id, id),
    CONSTRAINT fk_rgr_rom  FOREIGN KEY (source_id, rom_id)  REFERENCES remote_rom(source_id, id),
    INDEX idx_rgr_rom (source_id, rom_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- The join that makes "do I have this, does the server have it, both, neither" one query:
-- release_id is the SHARED release, so this and local_release_games point at the same rows.
CREATE TABLE remote_release_games (
    source_id  SMALLINT UNSIGNED NOT NULL,
    release_id BIGINT UNSIGNED NOT NULL,
    game_id    BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (source_id, release_id, game_id),
    CONSTRAINT fk_rrg_release FOREIGN KEY (release_id)         REFERENCES `release`(id),
    CONSTRAINT fk_rrg_game    FOREIGN KEY (source_id, game_id) REFERENCES remote_game(source_id, id),
    INDEX idx_rrg_game (source_id, game_id),
    INDEX idx_rrg_release (release_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- What this machine last pulled from each source, per mirrored table: the source's content
-- hash and how big the result was. CatalogSync asks a source for its manifest, compares the
-- hash per table, and re-downloads only the tables that moved -- the same "fetch again only
-- when it changed" bookkeeping provider_dump_files (0005) does for provider dumps, pointed
-- at a peer instead of a provider. `table_name` is a local_* name as the SOURCE calls it.
CREATE TABLE remote_sync_state (
    source_id     SMALLINT UNSIGNED NOT NULL,
    table_name    VARCHAR(64) NOT NULL,
    source_hash   CHAR(64) NOT NULL,
    row_count     BIGINT UNSIGNED NOT NULL DEFAULT 0,
    generated_at  DATETIME NULL,              -- when the SOURCE computed that hash
    synced_at     DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (source_id, table_name),
    CONSTRAINT fk_rss_source FOREIGN KEY (source_id) REFERENCES remote_sources(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
