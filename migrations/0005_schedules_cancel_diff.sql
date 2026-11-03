-- Job cancellation: a pending job goes straight to 'cancelled'; a running one gets
-- cancel_requested and the worker aborts it on its next tick (database::jobs::cancel).
ALTER TABLE jobs
    MODIFY COLUMN status ENUM('pending','in_progress','done','failed','cancelled') NOT NULL DEFAULT 'pending',
    ADD COLUMN cancel_requested BOOLEAN NOT NULL DEFAULT FALSE AFTER status;

-- The playmatch gap filler is a job of its own (api::playmatch_fill).
ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill') NOT NULL;

-- Recurring syncs (database::schedules): a job payload plus when to enqueue it -- on a
-- fixed interval, when a cheap change check says the provider's data changed, or both.
CREATE TABLE schedules (
    id                BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    name              VARCHAR(128) NOT NULL,
    target            JSON NOT NULL,                  -- the job payload (incl. job_type)
    mode              ENUM('diff','redownload') NOT NULL DEFAULT 'diff',
    trigger_kind      ENUM('fixed','check','both') NOT NULL DEFAULT 'fixed',
    fixed_every_n     INT UNSIGNED NULL,
    fixed_unit        ENUM('minutes','hours','days','weeks','months') NULL,
    check_every_n     INT UNSIGNED NULL,
    check_unit        ENUM('minutes','hours','days','weeks','months') NULL,
    enabled           BOOLEAN NOT NULL DEFAULT TRUE,
    next_fixed_at     DATETIME NULL,
    next_check_at     DATETIME NULL,
    last_run_at       DATETIME NULL,
    last_job_id       BIGINT UNSIGNED NULL,
    last_check_at     DATETIME NULL,
    last_check_result VARCHAR(255) NULL,
    created_at        DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted           BOOLEAN NOT NULL DEFAULT FALSE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Downloaded dump files and their provider-side identity, so a diff sync fetches a file
-- again only when it changed (Hasheous per-platform zips by md5, the TheGamesDB dump by
-- ETag/Last-Modified).
CREATE TABLE provider_dump_files (
    provider      VARCHAR(32) NOT NULL,
    name          VARCHAR(255) NOT NULL,
    md5           VARCHAR(64) NULL,
    size          BIGINT UNSIGNED NULL,
    last_modified VARCHAR(64) NULL,
    etag          VARCHAR(255) NULL,
    entries       INT UNSIGNED NULL,              -- games in the file (coverage totals)
    -- MAX(content.id) when this file was last matched against the catalog: an unchanged
    -- file is re-processed (from the local copy) only when newer content exists.
    processed_content_max BIGINT UNSIGNED NULL,
    fetched_at    DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (provider, name)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- IGDB's own last-change time per cached game: a diff platform sync asks only for games
-- updated after the last complete sync.
ALTER TABLE igdb_games ADD COLUMN updated_at DATETIME NULL AFTER title;
