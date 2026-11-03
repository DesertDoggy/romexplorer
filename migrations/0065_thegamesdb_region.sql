-- TheGamesDB lists each regional release as a game of its own (a Japanese box, a European
-- box), and says which in `region_id` / `country_id`. Kept per cached game so a release links
-- to the entry of its own region, and its art is that region's box. 0 = TheGamesDB has none;
-- NULL = not read yet (the next dump sync fills them from the local dump file).
ALTER TABLE thegamesdb_games
    ADD COLUMN region_id  INT UNSIGNED NULL AFTER app_platform_id,
    ADD COLUMN country_id INT UNSIGNED NULL AFTER region_id;
