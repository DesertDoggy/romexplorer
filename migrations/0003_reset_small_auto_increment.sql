-- `sources` (TINYINT id), `genre` and `perspective` (SMALLINT ids) were filled with
-- INSERT IGNORE / upserts on every call, and InnoDB consumes an AUTO_INCREMENT value even
-- for an ignored duplicate -- so a well-used database could run `sources` out of ids (255)
-- and then silently fail to add a new source (every Redump import failed "no rows").
-- The code now selects first (`database::lookup_or_insert_name`); this puts each counter
-- back to MAX(id)+1 (MariaDB treats a value below the current maximum that way), recovering
-- a database that already hit the limit. No row data changes.
ALTER TABLE sources AUTO_INCREMENT = 1;
ALTER TABLE genre AUTO_INCREMENT = 1;
ALTER TABLE perspective AUTO_INCREMENT = 1;
