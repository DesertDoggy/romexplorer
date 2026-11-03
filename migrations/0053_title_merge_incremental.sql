-- The title merge pass looks at what changed since the last one, not the whole catalogue.
--
-- Every pass used to go over every title: each DLC-only title against every game's name,
-- every group of titles sharing an IGDB game (those on different platforms stay apart on
-- purpose, so they come back every time), every merged title's canonical name, every
-- suggestion. On a large catalogue that was most of an hour each time, to change nothing.
--
-- `pass_marks` remembers where the last completed pass started: the time (links carry
-- `linked_at`, merges `merged_at`) and the highest title and release ids (titles and
-- releases carry no time of their own, but ids only grow). The next pass collects the titles
-- touched since into `title_merge_dirty` -- an ordinary work table, emptied before and
-- after, since a TEMPORARY one needs a privilege the app's database user may not have --
-- and only looks at those (and what they could merge with). Neither table is backed up: a
-- restored catalogue has no mark, so its first pass is a full one.
CREATE TABLE pass_marks (
    name           VARCHAR(32) PRIMARY KEY,
    started_at     DATETIME NOT NULL,
    max_title_id   BIGINT UNSIGNED NOT NULL DEFAULT 0,
    max_release_id BIGINT UNSIGNED NOT NULL DEFAULT 0
) ENGINE=InnoDB;

CREATE TABLE title_merge_dirty (
    title_id BIGINT UNSIGNED PRIMARY KEY
) ENGINE=InnoDB;
