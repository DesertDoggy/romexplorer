-- A title whose localized names the user set by hand: IGDB re-apply (one title or all of
-- them) leaves its names alone. The user's names are the title_locales rows with source
-- 'user'; unlocking drops them and takes IGDB's again.
ALTER TABLE title ADD COLUMN locales_locked BOOLEAN NOT NULL DEFAULT FALSE;
