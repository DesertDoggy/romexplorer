-- How a release is meant to be shown: its display aspect (4:3, 8:7, 16:9, 3:4...), widescreen
-- support (none / option / native / patch), video modes (240p ... 1080p) and refresh (50 / 60).
-- Per release, not per title: a PAL release is 576i at 50 Hz where its NTSC one is 480i at 60,
-- and one region's release can add 16:9 another lacks.
--
-- One row per field per source -- the user, the game's own files (a PS3 game's PARAM.SFO),
-- MAME's machine list, PCSX2's widescreen patches -- and the highest-ranked one counts
-- (`common::display::SOURCE_RANK`). A field with no row shows the system's default, which is
-- computed, never stored.
CREATE TABLE release_display (
    release_id BIGINT UNSIGNED NOT NULL,
    field      ENUM('aspect','widescreen','modes','refresh') NOT NULL,
    value      VARCHAR(64) NOT NULL,
    source     VARCHAR(16) NOT NULL,
    noted_at   DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (release_id, field, source),
    INDEX idx_rdisp_value (field, value(16)),
    CONSTRAINT fk_rdisp_release FOREIGN KEY (release_id) REFERENCES `release`(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- The job that collects it from game files, MAME and PCSX2's patches.
ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply','ra_patch_download','title_merge_audit','backup_database','backup_catalog','backup_dat_seed','catalog_sync','restore_backup','sqlite_export','sqlite_import','copy_files','integrity_check','sheet_create','sheet_verify','m3u_verify','media_sync','audio_scan','audio_sync','media_backfill','ra_badges','dictionary_build','audio_autotag','display_data') NOT NULL;
