-- A title entry names one row of the title (`api::catalog::TitleRow::row_key`): its retail
-- releases, its demos, one hack. It resolves within that row only, never to another kind of
-- release. '' is an entry made before rows (or by a set file that names only the title):
-- `api::sets::resolve` gives it the row of the title's best release, once.
ALTER TABLE collection_set_entries ADD COLUMN row_key VARCHAR(40) NOT NULL DEFAULT '' AFTER title_id,
    DROP INDEX uq_cse_title,
    ADD UNIQUE KEY uq_cse_title (set_id, title_id, row_key);
