-- One release per platform + name, whichever DATs list it (database::release_merge). Before
-- this, a release was keyed by its DAT source too, so e.g. a disc listed by redump (cue/bin),
-- mame-redump (CHD) and the RetroAchievements hash list was three releases.
--
-- release_sources: which DAT sources list a release. release_contents.source_id: which
-- source's file list a file belongs to -- a release's "dumps" (cue/bin set, CHD, ...); a
-- file two sources share keeps the first. release.source_id stays as the first source.
-- The merge itself and the unique-key change run in Rust after the migrations
-- (release_merge::merge_duplicate_releases), so they can be resumed and tested.
CREATE TABLE release_sources (
    release_id BIGINT UNSIGNED NOT NULL,
    source_id  TINYINT UNSIGNED NOT NULL,
    PRIMARY KEY (release_id, source_id),
    INDEX idx_rs_source (source_id),
    CONSTRAINT fk_rs_release FOREIGN KEY (release_id) REFERENCES `release`(id),
    CONSTRAINT fk_rs_source  FOREIGN KEY (source_id)  REFERENCES sources(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

INSERT IGNORE INTO release_sources (release_id, source_id)
SELECT id, source_id FROM `release` WHERE source_id IS NOT NULL;

ALTER TABLE release_contents
    ADD COLUMN source_id TINYINT UNSIGNED NULL AFTER file_name,
    ADD INDEX idx_rc_source (release_id, source_id);

UPDATE release_contents rc JOIN `release` r ON r.id = rc.release_id SET rc.source_id = r.source_id;
