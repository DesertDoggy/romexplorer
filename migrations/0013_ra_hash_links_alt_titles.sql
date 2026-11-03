-- RetroAchievements links by RA hash: RA's hash list (retroachievements_hashes) was only
-- matched against plain MD5s, but a disc's RA hash is RA's own disc hash -- the value the
-- RA DATs carry as `ra_hash`. Every release whose file has an RA-listed ra_hash is RA-
-- supported: an exact link (a weaker existing link is replaced; a manual one is kept).
INSERT INTO release_provider_links (release_id, provider, provider_id, confidence, linked_at, deleted)
SELECT DISTINCT rc.release_id, 'retroachievements', CAST(rh.ra_game_id AS CHAR), 'exact', NOW(), FALSE
FROM retroachievements_hashes rh
JOIN hashes h ON h.algorithm = 'ra_hash' AND h.value_hex = rh.md5
JOIN release_contents rc ON rc.content_id = h.content_id
ON DUPLICATE KEY UPDATE
    provider_id = IF(release_provider_links.deleted OR release_provider_links.confidence <> 'manual', VALUES(provider_id), release_provider_links.provider_id),
    confidence = IF(release_provider_links.deleted OR release_provider_links.confidence <> 'manual', 'exact', release_provider_links.confidence),
    deleted = FALSE;

-- Every alternative name a provider gives a title (IGDB's alternative_names: "Japanese
-- title", "Acronym", "Alternative spelling", ...), shown on the title page and searched by
-- the title search. Names that stand for a language also go to title_locales.
CREATE TABLE title_alt_names (
    title_id BIGINT UNSIGNED NOT NULL,
    name     VARCHAR(512) NOT NULL,
    name_hash CHAR(40) NOT NULL,                 -- sha1(name): the key (names are long)
    source   VARCHAR(32) NOT NULL,
    comment  VARCHAR(128) NULL,
    PRIMARY KEY (title_id, name_hash),
    INDEX idx_tan_name (name(191)),
    CONSTRAINT fk_tan_title FOREIGN KEY (title_id) REFERENCES title(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE INDEX idx_tl_name ON title_locales (name(191));
