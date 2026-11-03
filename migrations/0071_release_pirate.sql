-- A pirate copy (`(Pirate)`, TOSEC `[p]`): still `is_unlicensed`, as before, and now told
-- apart from the unlicensed games sold as such, so the lists can show it on a row of its own.
-- Filled for existing releases by `release_editions::backfill_variants`, which also
-- re-reads the names whose second tag (`(Demo) (Unl)`) the parser used to drop.
ALTER TABLE `release` ADD COLUMN is_pirate BOOLEAN NOT NULL DEFAULT FALSE AFTER is_unlicensed;
