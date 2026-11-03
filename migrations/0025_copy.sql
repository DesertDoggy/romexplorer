-- Copying ROMs out of a collection: the mount that makes another machine's paths reachable,
-- the per-platform destination layout, and the job that does the copying.
--
-- The model (docs/plan/api.md, "Multi-machine model"): the server's collection root is
-- mounted on the client over SMB or whatever the user has, and copying is plain OS file
-- copy -- bytes never travel through REST. What the client needs in order to do that is a
-- way to turn a `remote_files.path`, which is absolute *as the source machine writes it*,
-- into a path on this machine.

-- `jobs.job_type` is an ENUM, so a new kind of work is a schema change rather than a Rust
-- constant. The full list is restated because MODIFY replaces the definition.
ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply','ra_patch_download','title_merge_audit','backup_database','backup_catalog','catalog_sync','restore_backup','sqlite_export','sqlite_import','copy_files') NOT NULL;

-- Where a source machine's paths appear on this one.
--
-- Keyed on a path PREFIX rather than on a remote_directories row, because the mount is
-- often the parent of several collection roots and no row exists for that parent. The
-- longest matching prefix wins -- the same nearest-configured-ancestor rule
-- `database::directories::resolve` already applies to local_directories.
--
-- `remote_sep` is the separator the SOURCE machine writes, which is not this machine's:
-- a Windows server's `D:\Roms\...` has to become `/mnt/roms/...` on a Linux client, and
-- the comparison is case-insensitive for `\` and case-sensitive for `/`.
CREATE TABLE remote_mounts (
    id                  SMALLINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    source_id           SMALLINT UNSIGNED NOT NULL,
    remote_prefix       VARCHAR(1024) NOT NULL,       -- as the source writes it, no trailing separator
    remote_prefix_hash  CHAR(40) NOT NULL,            -- sha1(lower(remote_prefix)) -- CODING_RULES
    -- Which collection root the user picked, when they picked one instead of typing a
    -- parent. Provenance for the UI only: resolution never reads it, which is why it may
    -- be NULL and why its absence is not an error.
    remote_directory_id BIGINT UNSIGNED NULL,
    remote_sep          ENUM('/','\\') NOT NULL DEFAULT '/',
    local_path          VARCHAR(1024) NOT NULL,       -- absolute on THIS machine
    local_path_hash     CHAR(40) NOT NULL,
    enabled             BOOLEAN NOT NULL DEFAULT TRUE,
    deleted             BOOLEAN NOT NULL DEFAULT FALSE,
    last_verified_at    DATETIME NULL,
    created_at          DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE KEY uq_rmount (source_id, remote_prefix_hash),
    CONSTRAINT fk_rmount_source FOREIGN KEY (source_id) REFERENCES remote_sources(id),
    CONSTRAINT fk_rmount_dir FOREIGN KEY (source_id, remote_directory_id) REFERENCES remote_directories(source_id, id),
    INDEX idx_rmount_source (source_id, enabled, deleted)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Per-platform overrides for a copy's destination layout: what the subdirectory is called
-- and what the files are converted to on the way. Sparse -- only platforms the user has
-- actually changed get a row; everything else follows the compiled-in tables in
-- `common::platform_names` and the chosen scheme.
--
-- A table rather than a `settings` JSON blob for ~82 platforms: `settings::set` has no
-- compare-and-swap, so one document rewritten wholesale means two open config tabs clobber
-- each other; the FK enforces CODING_RULES' "app_platform_id is always platforms.id", which
-- a blob's keys cannot; and it joins against `platforms` to render the tab in one query.
-- The folder name and the output format share a row because they are the same row of the
-- same table in the UI.
CREATE TABLE platform_output (
    app_platform_id SMALLINT UNSIGNED PRIMARY KEY,
    folder_name     VARCHAR(255) NULL,   -- custom subdirectory; NULL = whatever the scheme says
    output_format   VARCHAR(16)  NULL,   -- same | raw | zip | 7z | chd | rvz | gcz | wia; NULL = same
    deleted         BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at      DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
    CONSTRAINT fk_pout_platform FOREIGN KEY (app_platform_id) REFERENCES platforms(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- The IGDB platform slug RomM matches its library folder names against, which is one of the
-- four naming schemes a copy can lay a destination out with.
--
-- A column rather than only the compiled-in seed in `common::platform_names` because IGDB
-- may rename a slug: `database::icon_import::import_romm_icons` already resolves the live
-- value per platform while fetching icons, and now records it here. NULL means "not resolved
-- yet"; the compiled seed covers that case, so an install with no IGDB credentials still
-- gets sensible folder names.
ALTER TABLE platforms ADD COLUMN romm_slug VARCHAR(64) NULL;
