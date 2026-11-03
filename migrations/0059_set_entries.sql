-- What the user put in a set: a release, a title, or a game (an IGDB id, any platform).
-- `collection_set_releases` becomes the resolved list derived from these (`api::sets::resolve`):
-- a title gives its best release held first, a game the same across its platforms.
CREATE TABLE collection_set_entries (
    id         BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    set_id     BIGINT UNSIGNED NOT NULL,
    release_id BIGINT UNSIGNED NULL,
    title_id   BIGINT UNSIGNED NULL,
    igdb_id    VARCHAR(64) NULL,
    position   INT UNSIGNED NOT NULL,
    added_at   DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE KEY uq_cse_release (set_id, release_id),
    UNIQUE KEY uq_cse_title (set_id, title_id),
    UNIQUE KEY uq_cse_game (set_id, igdb_id),
    INDEX idx_cse_order (set_id, position),
    CONSTRAINT fk_cse_set FOREIGN KEY (set_id) REFERENCES collection_sets(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

ALTER TABLE collection_set_releases ADD COLUMN entry_id BIGINT UNSIGNED NULL;

INSERT INTO collection_set_entries (set_id, release_id, position, added_at)
    SELECT set_id, release_id, position, added_at FROM collection_set_releases;
UPDATE collection_set_releases csr JOIN collection_set_entries e ON e.set_id = csr.set_id AND e.release_id = csr.release_id SET csr.entry_id = e.id;
