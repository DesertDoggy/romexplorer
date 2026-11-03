-- Live per-file progress for jobs that work on several files at once (a scan: which files
-- are being read, how far each is), and the job that downloads the media a configuration
-- asks for but the catalogue does not have yet.
ALTER TABLE jobs ADD COLUMN progress_detail JSON NULL AFTER progress_message;

ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply','ra_patch_download','title_merge_audit','backup_database','backup_catalog','backup_dat_seed','catalog_sync','restore_backup','sqlite_export','sqlite_import','copy_files','integrity_check','sheet_create','sheet_verify','m3u_verify','media_sync','audio_scan','audio_sync','media_backfill') NOT NULL;
