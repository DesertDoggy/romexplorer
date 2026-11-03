-- Families: a game, its official versions and the collections that hold it, for the
-- Collection > Definitive tab (`database::title_family`, `api::definitive`).
--
-- category  gains `edition` (a Complete / GOTY edition: IGDB's version_parent) and
--           `standalone` (a standalone expansion). Both fit title_merge_archive's
--           VARCHAR(16).
-- basegame_locked  the base game and category were set by the user: no IGDB pass changes
--           them again until the user resets them.
-- family_root_id   the title that heads the family this title is in (the top of its
--           base-game chain; titles of one IGDB game are one family). NULL: a family of
--           its own. Rebuilt by the title merge pass and after every manual edit.
ALTER TABLE title
    MODIFY category ENUM('base_game','port','remake','remaster','edition','dlc','expansion','standalone','bundle','mod') NOT NULL DEFAULT 'base_game',
    ADD COLUMN basegame_locked BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN family_root_id BIGINT UNSIGNED NULL,
    ADD INDEX idx_title_family (family_root_id);

-- What the family pass needs of a cached IGDB game, read out of its raw file once
-- (`relations_read`) instead of on every pass.
ALTER TABLE igdb_games
    ADD COLUMN game_type VARCHAR(16) NULL,
    ADD COLUMN first_year SMALLINT UNSIGNED NULL,
    ADD COLUMN platforms VARCHAR(512) NULL,
    ADD COLUMN relations_read BOOLEAN NOT NULL DEFAULT FALSE,
    ADD INDEX idx_igdb_relations_read (relations_read);

-- IGDB's links between games, from each game's own record:
--   parent    its parent_game / version_parent (a port, remake, edition, DLC to its game)
--   port / remake / remaster   the game's lists of its versions
--   bundle    a collection that holds the game
CREATE TABLE igdb_game_relations (
    game_id    INT UNSIGNED NOT NULL,
    related_id INT UNSIGNED NOT NULL,
    relation   ENUM('parent','port','remake','remaster','bundle') NOT NULL,
    PRIMARY KEY (game_id, related_id, relation),
    INDEX idx_igr_related (related_id, relation)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- The games a collection title holds: from IGDB, or set by the user on the title page.
CREATE TABLE title_bundle_games (
    bundle_title_id BIGINT UNSIGNED NOT NULL,
    game_title_id   BIGINT UNSIGNED NOT NULL,
    source          ENUM('igdb','manual') NOT NULL,
    PRIMARY KEY (bundle_title_id, game_title_id),
    INDEX idx_tbg_game (game_title_id),
    CONSTRAINT fk_tbg_bundle FOREIGN KEY (bundle_title_id) REFERENCES title(id) ON DELETE CASCADE,
    CONSTRAINT fk_tbg_game FOREIGN KEY (game_title_id) REFERENCES title(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- The user's order of a family's versions on the Definitive tab (dragged on the family
-- page): lower weight first; versions without a row follow in the default order.
CREATE TABLE title_family_order (
    root_title_id BIGINT UNSIGNED NOT NULL,
    title_id      BIGINT UNSIGNED NOT NULL,
    sort_weight   DOUBLE NOT NULL,
    PRIMARY KEY (root_title_id, title_id),
    INDEX idx_tfo_title (title_id),
    CONSTRAINT fk_tfo_root FOREIGN KEY (root_title_id) REFERENCES title(id) ON DELETE CASCADE,
    CONSTRAINT fk_tfo_title FOREIGN KEY (title_id) REFERENCES title(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Each family's definitive version, as `database::title_family::rank` puts them (the user's
-- order, the newest version, the platform priority): kept so the Definitive tab can sort and
-- page by it. Worked out again after the family pass, an edit, a drag, or a change to the
-- settings it depends on. A family of one has no row (it is its own definitive version).
CREATE TABLE title_families (
    root_title_id       BIGINT UNSIGNED PRIMARY KEY,
    definitive_title_id BIGINT UNSIGNED NOT NULL,
    members             SMALLINT UNSIGNED NOT NULL,
    INDEX idx_tf_definitive (definitive_title_id),
    CONSTRAINT fk_tf_root FOREIGN KEY (root_title_id) REFERENCES title(id) ON DELETE CASCADE,
    CONSTRAINT fk_tf_definitive FOREIGN KEY (definitive_title_id) REFERENCES title(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
