-- The parent game IGDB names for a title, kept so the link to it does not depend on order.
--
-- A port, remake, DLC or mod has a parent game on IGDB. `title.basegame_title_id` points at
-- the local title linked to that parent -- but it was looked up once, when the port's own
-- IGDB data was applied, and the parent's id was then thrown away. A port fetched before
-- its base game was in the catalogue kept no base game for good, however the base game
-- arrived later.
--
-- basegame_igdb_id  NULL  not known yet (no IGDB data applied since this column exists;
--                         filled from the cached IGDB game by the title merge pass)
--                   0     IGDB names no parent
--                   else  the parent game's IGDB id
--
-- `providers::metadata::link_basegames` (a step of the title merge pass, and of applying
-- IGDB data) resolves it to `basegame_title_id` whenever a title linked to that id exists.
ALTER TABLE title
    ADD COLUMN basegame_igdb_id INT UNSIGNED NULL,
    ADD INDEX idx_title_basegame_igdb (basegame_igdb_id);
