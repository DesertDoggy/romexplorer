-- Indexes and two small columns the database audit found missing. Nothing here rewrites a
-- large table: indexes are built beside the rows, and a nullable column added last is
-- instant in MariaDB (10.3+).

-- `title_merge::live_title` follows a merged title to its survivor once per hop, and the
-- merge log is read by merged title in decoupling and the review: without this each was a
-- scan of the whole log.
ALTER TABLE title_merge_log ADD INDEX idx_tml_merged (merged_title_id);

-- A scan looks every ROM up by its path (`api::scan::upsert_rom`), so a scan was quadratic
-- in the size of the library. VARCHAR(1024) in utf8mb4 is too long for a full key; the first
-- 255 characters narrow a lookup to a row or two. The lookups compare the full path byte for
-- byte after that (`api::scan::MAIN_PATH_EQ`), as `local_files.path_hash` already does: the column's
-- collation is case-insensitive, and on Linux `A.gb` and `a.gb` are two files.
ALTER TABLE local_rom ADD INDEX idx_lrom_main_path (main_path(255));

-- The IGDB (or other) game a merge audit found wrongly linked to a title. A pass that
-- matches the title's files to that game again does not link it back; a manual link does
-- (`providers::cache` LINK_UPSERT_*).
ALTER TABLE title_provider_links ADD COLUMN rejected_id VARCHAR(64) NULL;

-- A merge suggestion's title ids, sorted, comma-separated. 1024 characters held about a
-- hundred ids and the list was cut there, in the middle of an id.
ALTER TABLE title_merge_suggestions MODIFY COLUMN title_ids TEXT NOT NULL;
