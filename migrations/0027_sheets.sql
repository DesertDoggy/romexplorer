-- Rebuilding and checking the index files a dump needs: .cue, .gdi, .m3u.
--
-- A cue/bin set is the one dump shape whose index is a separate file, so losing that file
-- loses the dump: the tracks are still on disk, but nothing says what they are or what order
-- they go in. Three new job types rebuild those indexes and check the ones already there.
--
-- `sheet_create` and the two verify jobs are separate types rather than modes of one, because
-- they answer different questions of different things -- one reconstructs disc geometry from
-- track bytes, one re-reads a sheet and compares it, one checks that a playlist's lines still
-- resolve -- and a schedule wants the checks without ever wanting the writer.
ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply','ra_patch_download','title_merge_audit','backup_database','backup_catalog','catalog_sync','restore_backup','sqlite_export','sqlite_import','copy_files','integrity_check','sheet_create','sheet_verify','m3u_verify') NOT NULL;

-- The last verdict on one sidecar file.
--
-- Two columns rather than one, because the question has two independent answers and collapsing
-- them loses the distinction the whole feature turns on: `verdict` says whether the sheet
-- *works* (it parses, it names files that are there, their sizes divide into whole sectors),
-- and `hash_known` says whether the catalogue had anything to compare it against. A sheet that
-- works but is not byte-identical to Redump's is `structure_ok` with `hash_known` set -- worth
-- showing differently from one nobody can check, and worth never calling broken, because it
-- plays.
--
-- No `deleted` column, and not an oversight: this is not an entity but the cached result of a
-- check, one row per file, replaced wholesale every time the check runs. There is nothing to
-- soft-delete -- a stale verdict is overwritten, and a verdict whose file is gone is removed
-- with it by the FK. Its lifetime is the file's.
CREATE TABLE local_sheet_check (
    file_id    BIGINT UNSIGNED PRIMARY KEY,
    verdict    ENUM('verified','structure_ok','broken') NOT NULL,
    -- Whether a DAT records this sheet at all. Only meaningful for `structure_ok`: it separates
    -- "nothing to compare against" from "compared, and it differs".
    hash_known BOOLEAN NOT NULL DEFAULT FALSE,
    -- The problems, one per line, for `broken`. Each is one thing a person could go and fix.
    detail     TEXT NULL,
    checked_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
    CONSTRAINT fk_lsc_file FOREIGN KEY (file_id) REFERENCES local_files(id),
    -- "Show me what is broken" and "check what was checked longest ago" are the two ways this
    -- table is ever read.
    INDEX idx_lsc_verdict (verdict, checked_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
