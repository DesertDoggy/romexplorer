-- Which copy of a release comes first.
--
-- A release held more than once -- the same dump in two collections, a verified set and a
-- working copy -- listed its copies in the order they were scanned. They now go complete
-- first, then by the priority of the directory they are in, and the user can put one release's
-- copies in an order of their own. The first one is also the copy a copy job takes.
--
-- `priority`: a directory's place among its siblings (the directories with the same parent;
-- the roots among the roots), lower first. Set to their path order to begin with. Mirrored
-- directories (`remote_directories`) have none: the order is this machine's own.
ALTER TABLE local_directories ADD COLUMN priority INT NOT NULL DEFAULT 0;

UPDATE local_directories d
JOIN (SELECT id, ROW_NUMBER() OVER (PARTITION BY COALESCE(parent_directory_id, 0) ORDER BY path) AS n FROM local_directories) o ON o.id = d.id
SET d.priority = o.n;

-- One release's copies in the user's order (`position`, lower first); copies not in it come
-- after, in the default order.
CREATE TABLE local_game_order (
    release_id BIGINT UNSIGNED NOT NULL,
    game_id    BIGINT UNSIGNED NOT NULL,
    position   INT NOT NULL,
    PRIMARY KEY (release_id, game_id),
    INDEX idx_lgo_game (game_id)
) ENGINE=InnoDB;
