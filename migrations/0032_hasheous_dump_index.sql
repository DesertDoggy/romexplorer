-- Hasheous' published hash map, indexed by hash.
--
-- The per-platform dumps (`Dumps/platforms/<name>.zip`) were only ever *read through*: a
-- bulk sync matched each zip against the content the catalogue held at that moment and kept
-- the matches (`hasheous_lookups`). Everything else in the zip was dropped, so a file
-- scanned afterwards -- the usual order on a new install -- had no answer in the database
-- and `hash_match` asked Hasheous for it live: on the test collection 6,826 requests at
-- about 0.67 s each, for answers already downloaded.
--
-- These two tables keep what the zips say, the way `retroachievements_hashes` keeps RA's
-- list: a hash is looked up here first, and only a miss goes to the network.
--
-- Only games Hasheous has mapped to at least one provider are kept; an entry with no ids
-- answers nothing a lookup is made for.
CREATE TABLE hasheous_dump_games (
    id        BIGINT UNSIGNED NOT NULL,               -- Hasheous' own game id
    name      VARCHAR(512) NOT NULL,
    platform  VARCHAR(255) NULL,
    -- [["igdb","385"],["thegamesdb","2854"], ...]: provider name and the id it is mapped to
    mapped    TEXT NOT NULL,
    -- the zip it came from, so re-reading one zip replaces exactly its own rows
    dump_name VARCHAR(255) NOT NULL,
    PRIMARY KEY (id),
    INDEX idx_hdg_dump (dump_name)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- `value_hex` has the charset and collation of `hashes.value_hex`, so the join between the
-- two uses both indexes without a conversion.
CREATE TABLE hasheous_dump_roms (
    algorithm ENUM('md5','sha1') NOT NULL,
    value_hex VARCHAR(64) NOT NULL,                   -- lowercase hex
    game_id   BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (algorithm, value_hex, game_id),
    INDEX idx_hdr_game (game_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
