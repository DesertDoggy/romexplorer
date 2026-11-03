-- The merge audit job: decouples title merges and removes IGDB links whose names don't
-- match the IGDB game (`merge_audit::audit`).
ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply','ra_patch_download','title_merge_audit') NOT NULL;
