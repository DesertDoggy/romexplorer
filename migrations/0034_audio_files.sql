-- An audio library: the files under the configured audio folders, with what their tags say.
--
-- Audio so far existed only as `media` rows of type `bgm`, attached one upload at a time.
-- Nothing looked at the audio folder itself. `audio_scan` walks it (every subfolder: music
-- lives in artist/album directories) and records each file here with its title, album,
-- artist, disc and track numbers and year, so the list can be searched and filtered and a
-- title can play everything tagged for it.
--
-- `playable`: whether the app's own browser can play the format (WAV, MP3, AAC, FLAC, Ogg,
-- Opus). ALAC, DSD, WMA, APE and the like are listed all the same -- they are part of the
-- library -- and skipped by the player until something converts them.
CREATE TABLE audio_files (
    id            BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    path          VARCHAR(1024) NOT NULL,
    path_hash     CHAR(40) NOT NULL,                 -- sha1 of the path: the unique key
    file_name     VARCHAR(512) NOT NULL,
    size          BIGINT UNSIGNED NOT NULL,
    last_modified DATETIME NULL,
    format        VARCHAR(16) NOT NULL,              -- the extension, lowercase
    playable      BOOLEAN NOT NULL DEFAULT TRUE,
    duration_ms   INT UNSIGNED NULL,
    title         VARCHAR(512) NULL,
    album         VARCHAR(512) NULL,
    artist        VARCHAR(512) NULL,
    album_artist  VARCHAR(512) NULL,
    disc_number   SMALLINT UNSIGNED NULL,
    track_number  SMALLINT UNSIGNED NULL,
    year          SMALLINT UNSIGNED NULL,
    scanned_at    DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted       BOOLEAN NOT NULL DEFAULT FALSE,
    UNIQUE KEY uq_audio_path (path_hash),
    INDEX idx_audio_title (title(191)),
    INDEX idx_audio_album (album(191)),
    INDEX idx_audio_artist (artist(191)),
    INDEX idx_audio_year (year)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- The keyword tags on a file, by *name*. Not by `keyword.id`: `keyword` is a catalogue
-- table, emptied and refilled when a catalogue is replaced from a main server, and an id
-- kept here would then point at nothing or at something else. A title plays the files that
-- share a keyword name with it.
CREATE TABLE audio_file_tags (
    audio_file_id BIGINT UNSIGNED NOT NULL,
    name          VARCHAR(191) NOT NULL,
    PRIMARY KEY (audio_file_id, name),
    INDEX idx_aft_name (name),
    CONSTRAINT fk_aft_file FOREIGN KEY (audio_file_id) REFERENCES audio_files(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply','ra_patch_download','title_merge_audit','backup_database','backup_catalog','backup_dat_seed','catalog_sync','restore_backup','sqlite_export','sqlite_import','copy_files','integrity_check','sheet_create','sheet_verify','m3u_verify','media_sync','audio_scan') NOT NULL;
