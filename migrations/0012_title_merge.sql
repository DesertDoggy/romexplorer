-- Title merging (database::title_merge): titles split only by regional naming ("Suikoden V" /
-- "Gensou Suikoden V", "Dark Cloud 2" / "Dark Chronicle") are merged -- automatically once
-- they share an IGDB id at exact/hash/manual confidence on a common platform, or by the user.
-- A merged title's normalized name(s) stay here as aliases of the surviving title, so a DAT
-- re-import finds the merged title instead of splitting it again.
CREATE TABLE title_aliases (
    norm_hash CHAR(40) NOT NULL,
    title_id  BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (norm_hash, title_id),
    INDEX idx_ta_title (title_id),
    CONSTRAINT fk_ta_title FOREIGN KEY (title_id) REFERENCES title(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- title_dedupe: one pass of the automatic IGDB-id merge over the whole catalog.
-- igdb_reapply: re-applies cached IGDB games' metadata to their titles (localized names).
ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply') NOT NULL;

-- Every merge, with why (igdb:<id> automatic, or manual) and what the merged title was
-- called -- the audit trail (and what an undo would need).
CREATE TABLE title_merge_log (
    id              BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    survivor_id     BIGINT UNSIGNED NOT NULL,
    merged_title_id BIGINT UNSIGNED NOT NULL,
    merged_name     VARCHAR(512) NOT NULL,
    merged_norm_hash CHAR(40) NOT NULL,
    reason          VARCHAR(64) NOT NULL,
    merged_at       DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_tml_survivor (survivor_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- The backlog: titles sharing an IGDB game that the automatic pass did not merge -- the
-- link is only a name-search guess, or the titles are on different platforms (ports) --
-- for the user to accept or dismiss. One row per IGDB game; title_ids is the sorted list.
CREATE TABLE title_merge_suggestions (
    igdb_id    VARCHAR(64) NOT NULL PRIMARY KEY,
    title_ids  VARCHAR(1024) NOT NULL,
    reason     VARCHAR(32) NOT NULL,             -- name_link | cross_platform
    status     ENUM('pending','accepted','dismissed') NOT NULL DEFAULT 'pending',
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_tms_status (status)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
