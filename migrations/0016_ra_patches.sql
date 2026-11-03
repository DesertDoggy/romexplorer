-- RetroAchievements compatibility patches (RA game page → "download patch").
--
-- `ra_game_hashes` caches RetroAchievements' per-hash list for a game (`API_GetGameHashes`:
-- the ROM name RA knows the hash by, its labels, and the compatibility patch's URL when the
-- hash is a patched ROM). `ra_patches` records a downloaded patch archive, kept as is under
-- the directory of type `patch` (`<patch dir>/<Platform>/<archive>`), with the names needed
-- to apply it later: the patched release's canonical name and the stem the archive's
-- patch / sheet files share. `ra_patch_files` lists the archive's members (a small DAT of
-- the patch), each hashed into `content` / `hashes` and linked archive → member in
-- `content_contents`, so a renamed copy of the same archive is recognized.
CREATE TABLE ra_game_hashes (
    md5        CHAR(32) PRIMARY KEY,
    ra_game_id INT UNSIGNED NOT NULL,
    name       VARCHAR(512) NULL,
    labels     VARCHAR(255) NULL,
    patch_url  VARCHAR(1024) NULL,
    fetched_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_rgh_game (ra_game_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE ra_patches (
    id               BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    ra_game_id       INT UNSIGNED NOT NULL,
    -- RA's hash of the patched ROM this patch produces.
    md5              CHAR(32) NOT NULL,
    release_id       BIGINT UNSIGNED NULL,
    patch_url        VARCHAR(1024) NOT NULL,
    -- Relative to the patch directory, '/' separators on every OS.
    rel_path         VARCHAR(1024) NOT NULL,
    archive_format   VARCHAR(16) NULL,
    size             BIGINT UNSIGNED NOT NULL,
    content_id       BIGINT UNSIGNED NULL,
    -- The patched release's canonical name (our release, else RA's name for the hash).
    patched_name     VARCHAR(512) NOT NULL,
    -- The stem the archive's patch / sheet files share ("Game (Japan) [T-En]" for
    -- ".xdelta" + ".cue"); NULL when there are none.
    patch_rom_name   VARCHAR(512) NULL,
    names_consistent BOOLEAN NOT NULL DEFAULT TRUE,
    downloaded_at    DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE KEY uq_rap_md5 (md5),
    INDEX idx_rap_game (ra_game_id),
    INDEX idx_rap_content (content_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE ra_patch_files (
    patch_id    BIGINT UNSIGNED NOT NULL,
    member_path VARCHAR(512) NOT NULL,
    kind        ENUM('patch','sheet','readme','other') NOT NULL,
    ext         VARCHAR(16) NULL,
    size        BIGINT UNSIGNED NULL,
    content_id  BIGINT UNSIGNED NULL,
    PRIMARY KEY (patch_id, member_path(255)),
    CONSTRAINT fk_rpf_patch FOREIGN KEY (patch_id) REFERENCES ra_patches(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- A directory type for patches, next to bios / rom / mixed.
ALTER TABLE local_directories
    MODIFY COLUMN dir_type ENUM('bios','rom','mixed','patch') NULL;

ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply','ra_patch_download') NOT NULL;
