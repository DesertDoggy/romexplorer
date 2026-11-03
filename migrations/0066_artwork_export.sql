-- Exporting art into an emulator's own folders (RetroArch thumbnails, PCSX2 / DuckStation
-- covers). Each emulator row says where and in which layout and formats
-- (`database::emulators::EmulatorArtwork`, as JSON); NULL = no export.
ALTER TABLE emulators ADD COLUMN artwork JSON NULL AFTER enabled;

-- Every file an export wrote, so a re-run rewrites only what changed and never overwrites a
-- file it did not write (one the user or another tool put there). Keyed by a hash of the
-- path: a 1024-character path is past InnoDB's key length.
CREATE TABLE media_exports (
    emulator_id BIGINT UNSIGNED NOT NULL,
    path_sha1   CHAR(40) NOT NULL,
    path        VARCHAR(1024) NOT NULL,
    media_id    BIGINT UNSIGNED NULL,
    source_sha1 CHAR(40) NULL,
    written_at  DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
    PRIMARY KEY (emulator_id, path_sha1),
    CONSTRAINT fk_mexp_emulator FOREIGN KEY (emulator_id) REFERENCES emulators(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply','ra_patch_download','title_merge_audit','backup_database','backup_catalog','backup_dat_seed','catalog_sync','restore_backup','sqlite_export','sqlite_import','copy_files','integrity_check','sheet_create','sheet_verify','m3u_verify','media_sync','audio_scan','audio_sync','media_backfill','ra_badges','dictionary_build','audio_autotag','display_data','artwork_export') NOT NULL;
