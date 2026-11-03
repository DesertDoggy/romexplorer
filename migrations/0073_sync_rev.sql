-- Row-level change tracking for the peer sync (`api::sync`, `database::sync_rev`).
--
-- Every synced table gets a `sync_rev` column, filled from this one sequence by triggers
-- whenever a row is added or one of its counted values changes. A client remembers the
-- highest number it has received per table and asks only for rows above it, instead of
-- downloading the whole table again whenever any row in it moved. The columns and triggers
-- themselves are added in Rust (`sync_rev::ensure`), from the live schema, so a column added
-- by a later migration is counted without anyone remembering to update a trigger.
--
-- Signed BIGINT is the sequence's own type on every MariaDB this app supports (unsigned
-- sequences only arrived in 11.5); 2^63 is far beyond anything a collection can use up.
CREATE SEQUENCE IF NOT EXISTS sync_rev_seq START WITH 1 INCREMENT BY 1 MINVALUE 1 CACHE 1000;

-- Hard deletes, so a client can delete the same rows instead of wiping and refilling the
-- table. `pk` is a JSON array of the row's primary-key values as text, in key order.
-- Pruned after a while (`sync_rev::prune`); a client further behind than that is sent the
-- whole table instead.
CREATE TABLE IF NOT EXISTS sync_tombstones (
    rev         BIGINT UNSIGNED NOT NULL PRIMARY KEY,
    table_name  VARCHAR(64) NOT NULL,
    pk          TEXT NOT NULL,
    deleted_at  DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    KEY idx_tombstones_table (table_name, rev),
    KEY idx_tombstones_age (deleted_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- This database's sync identity. `epoch` is random and changes whenever the numbers stop
-- meaning what a client remembers: a fresh database (the sequence starts again at 1) or the
-- catalogue being emptied with TRUNCATE, which leaves no tombstones. Kept out of `settings`
-- on purpose, so a backup restore or a settings import never brings another machine's epoch.
-- `pruned_through` is the highest tombstone number already pruned: a client whose position
-- is below it may have missed deletes, so it is sent the whole table.
CREATE TABLE IF NOT EXISTS sync_state (
    id              TINYINT UNSIGNED NOT NULL PRIMARY KEY,
    epoch           VARCHAR(64) NOT NULL,
    pruned_through  BIGINT UNSIGNED NOT NULL DEFAULT 0
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
INSERT IGNORE INTO sync_state (id, epoch) VALUES (1, REPLACE(UUID(), '-', ''));

-- Where each mirrored table has got to: the source's epoch (see `sync_state`) and the
-- highest change number received. NULL means
-- "unknown", which makes the next pull of that table a full one, as before.
ALTER TABLE remote_sync_state
    ADD COLUMN sync_epoch VARCHAR(64) NULL,
    ADD COLUMN sync_rev   BIGINT UNSIGNED NULL;
