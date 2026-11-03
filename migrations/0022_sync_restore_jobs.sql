-- The jobs that move a database between machines: pulling a peer's ownership tables into the
-- remote_* mirror, restoring a backup, and building/reading the SQLite cache a phone uses.
--
-- All four are long-running and belong in the worker for the same reasons the backups do --
-- progress, retry, history, and a schedule for the one that repeats (catalog_sync).
--
-- `jobs.job_type` is an ENUM, so a new kind of work is a schema change rather than a Rust
-- constant. The full list is restated because MODIFY replaces the definition.
ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply','ra_patch_download','title_merge_audit','backup_database','backup_catalog','catalog_sync','restore_backup','sqlite_export','sqlite_import') NOT NULL;
