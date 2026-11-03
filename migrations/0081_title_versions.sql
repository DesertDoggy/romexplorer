-- Titles › "hide minor versions" and "one of each version group" (`database::versions`).
--
-- The user's own corrections to what is worked out automatically, one row per pair:
--   minor      title_id is a minor version of other_title_id (hidden behind it)
--   not_minor  the two are not minor / major versions of each other, whatever IGDB or the
--              names say
--   group      title_id and other_title_id are equal versions of one game (a version group)
--   ungroup    title_id is in no version group (other_title_id NULL)
CREATE TABLE title_version_relations (
    id             BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    title_id       BIGINT UNSIGNED NOT NULL,
    other_title_id BIGINT UNSIGNED NULL,
    kind           ENUM('minor','not_minor','group','ungroup') NOT NULL,
    created_at     DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE KEY uq_tvr (title_id, other_title_id, kind),
    INDEX idx_tvr_other (other_title_id),
    CONSTRAINT fk_tvr_title FOREIGN KEY (title_id) REFERENCES title(id) ON DELETE CASCADE,
    CONSTRAINT fk_tvr_other FOREIGN KEY (other_title_id) REFERENCES title(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Which version of a version group is shown: the user's pick (`title_id`), and the seed of
-- the stable random pick (the 🎲 button adds one).
CREATE TABLE version_group_picks (
    group_key  VARCHAR(64) NOT NULL PRIMARY KEY,
    title_id   BIGINT UNSIGNED NULL,
    seed       INT UNSIGNED NOT NULL DEFAULT 0,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
