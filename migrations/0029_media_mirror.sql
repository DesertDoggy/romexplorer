-- The main server's art, mirrored so a client fetches it from the server before it asks a
-- provider (`api::sync` carries these three tables; `api::media_sync` acts on them).
--
-- `media` is a per-machine table: paths are relative to this machine's roots and ids are its
-- own AUTO_INCREMENT values, so a client cannot take the server's rows into its own `media`
-- the way it takes `release` or `title`. They go into a `remote_` mirror keyed by source
-- instead, exactly like ownership -- and because the catalogue *is* shared, the junctions'
-- `title_id` / `release_id` mean the same rows here as on the server. `path` and `root_role`
-- describe the server's disk and are kept only as information; what a client uses is the
-- server's media id (to ask it for the bytes), and the row's `source` / `provider_id` /
-- `source_url` (to record what it fetched as if it had fetched it itself, and to fall back to
-- the provider when the server no longer has the file).
--
-- ENUM columns arrive as text, so they are text here: the server's enum may gain a value
-- before this machine is updated, and a mirror that refuses a row it does not understand is
-- worse than one that carries it.
CREATE TABLE remote_media (
    source_id    SMALLINT UNSIGNED NOT NULL,
    id           BIGINT UNSIGNED NOT NULL,
    media_type   VARCHAR(32) NOT NULL,
    sub_category VARCHAR(16) NULL,
    path         VARCHAR(1024) NOT NULL,
    root_role    VARCHAR(16) NULL,
    content_sha1 CHAR(40) NULL,
    bytes        BIGINT UNSIGNED NULL,
    source       VARCHAR(32) NULL,
    provider_id  VARCHAR(64) NULL,
    source_url   VARCHAR(1024) NULL,
    deleted      BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (source_id, id),
    INDEX idx_rmedia_url (source_id, source_url(191)),
    INDEX idx_rmedia_sha1 (source_id, content_sha1),
    CONSTRAINT fk_rmedia_source FOREIGN KEY (source_id) REFERENCES remote_sources(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Hard-deleted on the server like the ownership junctions, so replaced wholesale per source
-- on every pull. No foreign keys onto `title` / `release`: a row may arrive before its
-- catalogue row in the same pull, and `sync::sweep_unresolved` drops what never resolves.
CREATE TABLE remote_title_media (
    source_id SMALLINT UNSIGNED NOT NULL,
    title_id  BIGINT UNSIGNED NOT NULL,
    media_id  BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (source_id, title_id, media_id),
    INDEX idx_rtm_media (source_id, media_id),
    CONSTRAINT fk_rtm_source FOREIGN KEY (source_id) REFERENCES remote_sources(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE remote_release_media (
    source_id  SMALLINT UNSIGNED NOT NULL,
    release_id BIGINT UNSIGNED NOT NULL,
    media_id   BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (source_id, release_id, media_id),
    INDEX idx_rrm_media (source_id, media_id),
    CONSTRAINT fk_rrm_source FOREIGN KEY (source_id) REFERENCES remote_sources(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- `media_sync`: fetch the art the main server has and this machine lacks (`api::media_sync`).
ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply','ra_patch_download','title_merge_audit','backup_database','backup_catalog','catalog_sync','restore_backup','sqlite_export','sqlite_import','copy_files','integrity_check','sheet_create','sheet_verify','m3u_verify','media_sync') NOT NULL;
