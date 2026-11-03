-- A re-release of a game on later hardware or a service: Virtual Console,
-- a collection disc (`name_parse::Edition::Rerelease`). Its own edition, ranked
-- by `release_edition_order` apart from budget reissues. Filled for existing releases by
-- `release_editions::backfill_rereleases`.
ALTER TABLE `release` ADD COLUMN is_rerelease BOOLEAN NOT NULL DEFAULT FALSE AFTER is_aftermarket;
