-- TheGamesDB lists a game once per platform (the same name appears for every port), so a
-- local search needs each cached game's platform to be usable: TheGamesDB's own platform
-- id and name, and the app platform it maps to (NULL when unmapped). Filled by the dump
-- import and the by-platform sync; the next dump diff run backfills existing rows from the
-- local dump file without downloading it again.
ALTER TABLE thegamesdb_games
    ADD COLUMN remote_platform_id INT UNSIGNED NULL AFTER title,
    ADD COLUMN platform_name      VARCHAR(128) NULL AFTER remote_platform_id,
    ADD COLUMN app_platform_id    SMALLINT UNSIGNED NULL AFTER platform_name,
    ADD INDEX idx_tgdb_app_platform (app_platform_id);
