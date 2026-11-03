-- Files a DAT no longer lists for a release it still lists: a bad dump replaced by a good one
-- under the same name, or a track dropped. They used to be deleted off the release, which
-- also lost them for good when the new DAT was the broken one. Now they move here: they no
-- longer count toward the release (every query of `release_contents` stays as it was), the
-- release page still shows them, and a later import that lists them again moves them back.
CREATE TABLE release_contents_superseded (
    release_id    BIGINT UNSIGNED NOT NULL,
    content_id    BIGINT UNSIGNED NOT NULL,
    file_name     VARCHAR(512) NOT NULL,
    source_id     TINYINT UNSIGNED NULL,
    variant       VARCHAR(64) NULL,
    -- The DAT whose import took it off, by name and version.
    dat_name      VARCHAR(512) NULL,
    dat_version   VARCHAR(64) NULL,
    superseded_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (release_id, content_id),
    INDEX idx_rcsup_content (content_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Which DAT of its source last listed the file (a hash of the DAT's name, its date and
-- counts left out). A source can have two DATs listing one release with different files --
-- No-Intro's encrypted and decrypted PlayStation 3 sets -- and one must not take the other's
-- files off. NULL until the next import of that DAT; such a row is never superseded.
ALTER TABLE release_content_sources ADD COLUMN dat_key CHAR(40) NULL;
