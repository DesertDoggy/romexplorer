-- Release editions and version order, and the user's manual release order per title.
--
-- Budget reissues ("(PlayStation 2 the Best)"), special / limited editions ("(Shokai
-- Genteiban)") and aftermarket releases are commercial releases with their own flag, from
-- whole-group name tags (`name_parse::Edition`). `revision_sort` orders "(v1.10)" /
-- "(Rev A)" numerically (0 = the initial release). Existing rows are filled by a one-time
-- data migration in Rust (`release_editions::backfill`), recorded in `data_migrations`.
ALTER TABLE `release`
    ADD COLUMN is_special_edition BOOLEAN NOT NULL DEFAULT FALSE AFTER patch_type,
    ADD COLUMN is_budget          BOOLEAN NOT NULL DEFAULT FALSE AFTER is_special_edition,
    ADD COLUMN is_aftermarket     BOOLEAN NOT NULL DEFAULT FALSE AFTER is_budget,
    ADD COLUMN revision_sort      INT UNSIGNED NOT NULL DEFAULT 0 AFTER revision;

-- The user's order of a title's releases (dragged on the title page): lower weight first,
-- NULL = the default order. On the link row, so it moves with the release when titles are
-- merged or decoupled.
ALTER TABLE title_releases
    ADD COLUMN sort_weight DOUBLE NULL,
    ADD COLUMN sort_set_at DATETIME NULL,
    ADD INDEX idx_tr_weight (sort_weight);

-- One-time data migrations done in Rust after the SQL ones.
CREATE TABLE data_migrations (
    name    VARCHAR(64) PRIMARY KEY,
    done_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
