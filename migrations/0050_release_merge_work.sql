-- The work table of the pass that folds the versions of one patched work into one release
-- (`database::release_merge::fold_patched_works`): which release goes into which.
--
-- It was a TEMPORARY table, which needs the CREATE TEMPORARY TABLES privilege. A database
-- user created by hand for the app often has every privilege on the app's tables and not
-- that one, and the title merge job then failed with
-- "1044: Access denied for user ... to database" before it did anything. An ordinary table
-- needs nothing the app does not already have. One pass at a time uses it (the job runs
-- alone in its lane), and it is empty between passes; it is not part of any backup.
CREATE TABLE release_merge (
    old_id BIGINT UNSIGNED PRIMARY KEY,
    new_id BIGINT UNSIGNED NOT NULL,
    INDEX idx_release_merge_new (new_id)
) ENGINE=InnoDB;
