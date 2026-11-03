-- A copy registers what it wrote itself, and a job that re-reads what this machine holds.
--
-- Two corrections to 0025's model:
--
-- 1. A copy used to hand registration to a `file_scan`, which re-derives every file's
--    identity by hashing it with the full algorithm set. That is work with a known answer:
--    the copy already carries each file's `content_id` from the ownership row it came from,
--    and `content` is shared catalogue data, identical on both machines. So a copy writes
--    `local_files` itself and hashing becomes optional -- and when it is on, it is one quick
--    algorithm (`copy_verify_hash`, xxh3_128 by default), read back to confirm the bytes
--    landed rather than to work out what they are.
--
-- 2. That same quick read-back is worth having on its own schedule, over files nobody just
--    copied: `integrity_check` walks `local_files` oldest-checked first, compares one hash
--    against `hashes`, and stamps `last_integrity_check` (a column 0001 already has and
--    nothing has been writing).
ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply','ra_patch_download','title_merge_audit','backup_database','backup_catalog','catalog_sync','restore_backup','sqlite_export','sqlite_import','copy_files','integrity_check') NOT NULL;

-- Oldest-checked first is the whole access pattern of an integrity run, and without this it
-- is a filesort over every file the machine holds.
CREATE INDEX idx_lfiles_integrity ON local_files (deleted, last_integrity_check);
