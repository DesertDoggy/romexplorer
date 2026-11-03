-- A third backup kind: the DAT-derived half of the catalogue on its own.
--
-- The catalogue mixes what a DAT import built (hashes and names) with what the providers
-- added (other people's databases, under API terms that restrict passing them on). Exporting
-- only the first half is what makes a catalogue movable between a user's own machines, and
-- it needs its own job type so it gets its own history and keep-count rather than pruning
-- against the full catalogue's.
ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply','ra_patch_download','title_merge_audit','backup_database','backup_catalog','backup_dat_seed','catalog_sync','restore_backup','sqlite_export','sqlite_import','copy_files','integrity_check','sheet_create','sheet_verify','m3u_verify','media_sync') NOT NULL;
