-- Media storage layout (database::media_store): new files live under a configured media
-- root of their role (media_dirs image/video/audio, else the app's assets/media = 'app')
-- at <Platform>/<Title>/<Region>/<type>_<sub>.<ext>, and `path` is relative to that root.
-- Rows written before this migration keep root_role NULL and an absolute path until the
-- media_relayout job moves them. content_sha1 dedups identical bytes across providers.
ALTER TABLE media
    ADD COLUMN root_role    ENUM('app','image','video','audio') NULL AFTER path,
    ADD COLUMN content_sha1 CHAR(40) NULL AFTER root_role,
    ADD COLUMN bytes        BIGINT UNSIGNED NULL AFTER content_sha1,
    ADD INDEX idx_media_sha1 (content_sha1),
    ADD INDEX idx_media_source_url (source_url(191));

-- Size-capped LRU cache copies in the tier 1 / tier 2 cache dirs: full-size copies and
-- grid thumbnails ('thumb:avif', 'thumb:jpeg', ...). tier 0 = the app's own thumbnail dir,
-- used when no cache tier is configured (never evicted). rel_path is relative to the tier.
CREATE TABLE media_cache (
    media_id    BIGINT UNSIGNED NOT NULL,
    variant     VARCHAR(16) NOT NULL,
    tier        TINYINT UNSIGNED NOT NULL,
    rel_path    VARCHAR(255) NOT NULL,
    bytes       BIGINT UNSIGNED NOT NULL,
    last_access DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (media_id, variant, tier),
    INDEX idx_mc_lru (tier, last_access),
    CONSTRAINT fk_mc_media FOREIGN KEY (media_id) REFERENCES media(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Moves pre-layout (hashed-name, absolute-path) app-downloaded media into the layout.
ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout') NOT NULL;
