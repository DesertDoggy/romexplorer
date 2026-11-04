-- Music and sound found inside this machine's ROMs (`api::rom_audio`), played from the ROM
-- itself like a CD player: nothing is extracted, only where each track is and what it is.
--
-- `rom_audio_index` says a ROM was looked at (and when), so a quick run skips it, even when
-- it held nothing. `rom_audio_tracks` is one row per track: a CD audio track (`cd_track`),
-- or a file on the disc (`inner_path`, with `data_track` the data track it sits on, and
-- `subsong` for a bank that holds many streams). Whether a track is BGM, a jingle or a sound
-- effect is worked out when a playlist is made, from the loop, the length and the
-- thresholds in Settings > Audio, so changing them needs no re-index; `kind_manual` is a
-- user's own choice, kept through a re-index.
CREATE TABLE rom_audio_index (
    rom_id     BIGINT UNSIGNED NOT NULL PRIMARY KEY,
    indexed_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    tracks     INT UNSIGNED NOT NULL DEFAULT 0,
    error      VARCHAR(1024) NULL,
    CONSTRAINT fk_rai_rom FOREIGN KEY (rom_id) REFERENCES local_rom(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE rom_audio_tracks (
    id           BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    rom_id       BIGINT UNSIGNED NOT NULL,
    location     ENUM('cdda','file') NOT NULL,
    cd_track     SMALLINT UNSIGNED NULL,
    data_track   SMALLINT UNSIGNED NOT NULL DEFAULT 0,
    inner_path   VARCHAR(1024) NULL,
    subsong      INT UNSIGNED NOT NULL DEFAULT 0,
    name         VARCHAR(512) NOT NULL,
    kind_manual  ENUM('bgm','jingle','se') NULL,
    sample_rate  INT UNSIGNED NOT NULL,
    channels     TINYINT UNSIGNED NOT NULL,
    duration_ms  INT UNSIGNED NOT NULL,
    looping      BOOLEAN NOT NULL DEFAULT FALSE,
    loop_start   BIGINT UNSIGNED NOT NULL DEFAULT 0,
    loop_end     BIGINT UNSIGNED NOT NULL DEFAULT 0,
    codec        VARCHAR(128) NOT NULL DEFAULT '',
    format       VARCHAR(128) NOT NULL DEFAULT '',
    bytes        BIGINT UNSIGNED NOT NULL DEFAULT 0,
    INDEX idx_rat_rom (rom_id),
    CONSTRAINT fk_rat_rom FOREIGN KEY (rom_id) REFERENCES local_rom(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply','ra_patch_download','title_merge_audit','backup_database','backup_catalog','backup_dat_seed','catalog_sync','restore_backup','sqlite_export','sqlite_import','copy_files','integrity_check','sheet_create','sheet_verify','m3u_verify','media_sync','audio_scan','audio_sync','media_backfill','ra_badges','dictionary_build','audio_autotag','display_data','artwork_export','rom_audio_index') NOT NULL;
