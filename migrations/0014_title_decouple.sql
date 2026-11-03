-- Decoupling merged titles (Edit > Combined titles).
--
-- A merge archives the merged title as it was -- its row, and its rows in every title-keyed
-- table (releases, locales, alternative names, genres, companies, media, keywords, provider
-- links) as JSON -- so decoupling restores it exactly, under its old id. Merges from before
-- this have no archive and fall back to matching release names.
CREATE TABLE title_merge_archive (
    log_id            BIGINT UNSIGNED PRIMARY KEY,
    title_id          BIGINT UNSIGNED NOT NULL,
    name              VARCHAR(512) NOT NULL,
    norm_name         VARCHAR(512) NOT NULL,
    norm_hash         CHAR(40) NOT NULL,
    category          VARCHAR(16) NOT NULL,
    basegame_title_id BIGINT UNSIGNED NULL,
    -- {"title_releases": [{"release_id": 1}, ...], "title_provider_links": [...], ...}
    rows_json         LONGTEXT NOT NULL,
    INDEX idx_tma_title (title_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- A decoupled merge keeps its log row: when it was undone and which title came back.
ALTER TABLE title_merge_log
    ADD COLUMN undone_at DATETIME NULL,
    ADD COLUMN restored_title_id BIGINT UNSIGNED NULL;

-- Pairs of titles the user split apart: the automatic merge pass never joins them again
-- (a manual merge of the pair lifts the block). Stored with title_a < title_b.
CREATE TABLE title_merge_blocks (
    title_a    BIGINT UNSIGNED NOT NULL,
    title_b    BIGINT UNSIGNED NOT NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (title_a, title_b),
    INDEX idx_tmb_b (title_b)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
