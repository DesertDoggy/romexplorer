-- Backups as job types, so they run in the worker with progress and retry like every other
-- long operation, and so `schedules` can run them on a timer (api::schedules::SCHEDULABLE).
--
-- Two of them rather than one with a flag, because they have genuinely different shapes: the
-- database backup is small and worth running nightly, the catalogue backup is large and worth
-- running rarely. Separate types means separate schedules, separate keep-counts and separate
-- files -- and either can be restored without the other.
--
-- `jobs.job_type` is an ENUM, so a new kind of work is a schema change rather than a Rust
-- constant. The full list is restated because MODIFY replaces the definition.
ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply','ra_patch_download','title_merge_audit','backup_database','backup_catalog') NOT NULL;
