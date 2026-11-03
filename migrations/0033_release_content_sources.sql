-- Every DAT that lists a file on a release, not only the first.
--
-- `release_contents.source_id` (0011) holds one source per file: whichever DAT listed it
-- first. A headered NES rom that RetroAchievements' list was imported with before No-Intro's
-- therefore reads "retroachievements" alone -- 1,123 files on the test catalogue -- although
-- No-Intro lists the very same bytes for the very same release. That column stays as it is
-- (it still says which dump a file belongs to, which is what groups a release's file lists);
-- this table is the full answer to "who lists this file".
--
-- Seeded from what is known, the one source each row already names. The rest arrives as
-- DATs are imported: an import records itself here for every line, whether or not the row
-- in `release_contents` was already there.
CREATE TABLE release_content_sources (
    release_id BIGINT UNSIGNED NOT NULL,
    content_id BIGINT UNSIGNED NOT NULL,
    source_id  TINYINT UNSIGNED NOT NULL,
    PRIMARY KEY (release_id, content_id, source_id),
    INDEX idx_rcs_source (source_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

INSERT IGNORE INTO release_content_sources (release_id, content_id, source_id)
SELECT release_id, content_id, source_id FROM release_contents WHERE source_id IS NOT NULL;
