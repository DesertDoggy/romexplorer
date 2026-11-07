-- Alternative titles the user removed from a title. A provider's names are written again
-- whenever its game is re-applied, so a removed one is remembered here and skipped
-- (`metadata::set_igdb_locales`) rather than coming back on the next metadata update.
CREATE TABLE title_alt_names_hidden (
    title_id  BIGINT UNSIGNED NOT NULL,
    name_hash CHAR(40) NOT NULL,                 -- sha1(name), as in title_alt_names
    PRIMARY KEY (title_id, name_hash),
    CONSTRAINT fk_tanh_title FOREIGN KEY (title_id) REFERENCES title(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- What IGDB wrote onto a title whose IGDB link was removed by hand stayed behind (its
-- alternative names above all), with no way to remove it. Unlinking now takes it off
-- (`api::links::unlink_title`); this clears what earlier unlinks left. A title linked to
-- IGDB through its own releases (combined by hand) keeps its rows.
DELETE x FROM title_alt_names x
 WHERE x.source = 'igdb'
   AND NOT EXISTS (SELECT 1 FROM title_provider_links l WHERE l.title_id = x.title_id AND l.provider = 'igdb' AND l.deleted = FALSE)
   AND NOT EXISTS (SELECT 1 FROM title_releases tr JOIN release_provider_links rl ON rl.release_id = tr.release_id AND rl.provider = 'igdb' AND rl.deleted = FALSE WHERE tr.title_id = x.title_id);
DELETE x FROM title_locales x
 WHERE x.source = 'igdb'
   AND NOT EXISTS (SELECT 1 FROM title_provider_links l WHERE l.title_id = x.title_id AND l.provider = 'igdb' AND l.deleted = FALSE)
   AND NOT EXISTS (SELECT 1 FROM title_releases tr JOIN release_provider_links rl ON rl.release_id = tr.release_id AND rl.provider = 'igdb' AND rl.deleted = FALSE WHERE tr.title_id = x.title_id);
DELETE x FROM title_genres x
 WHERE x.source = 'igdb'
   AND NOT EXISTS (SELECT 1 FROM title_provider_links l WHERE l.title_id = x.title_id AND l.provider = 'igdb' AND l.deleted = FALSE)
   AND NOT EXISTS (SELECT 1 FROM title_releases tr JOIN release_provider_links rl ON rl.release_id = tr.release_id AND rl.provider = 'igdb' AND rl.deleted = FALSE WHERE tr.title_id = x.title_id);
DELETE x FROM title_perspectives x
 WHERE x.source = 'igdb'
   AND NOT EXISTS (SELECT 1 FROM title_provider_links l WHERE l.title_id = x.title_id AND l.provider = 'igdb' AND l.deleted = FALSE)
   AND NOT EXISTS (SELECT 1 FROM title_releases tr JOIN release_provider_links rl ON rl.release_id = tr.release_id AND rl.provider = 'igdb' AND rl.deleted = FALSE WHERE tr.title_id = x.title_id);
DELETE x FROM title_franchises x
 WHERE x.source = 'igdb'
   AND NOT EXISTS (SELECT 1 FROM title_provider_links l WHERE l.title_id = x.title_id AND l.provider = 'igdb' AND l.deleted = FALSE)
   AND NOT EXISTS (SELECT 1 FROM title_releases tr JOIN release_provider_links rl ON rl.release_id = tr.release_id AND rl.provider = 'igdb' AND rl.deleted = FALSE WHERE tr.title_id = x.title_id);
DELETE x FROM title_companies x
 WHERE x.source = 'igdb'
   AND NOT EXISTS (SELECT 1 FROM title_provider_links l WHERE l.title_id = x.title_id AND l.provider = 'igdb' AND l.deleted = FALSE)
   AND NOT EXISTS (SELECT 1 FROM title_releases tr JOIN release_provider_links rl ON rl.release_id = tr.release_id AND rl.provider = 'igdb' AND rl.deleted = FALSE WHERE tr.title_id = x.title_id);
DELETE x FROM title_keywords x
 WHERE x.source = 'igdb'
   AND NOT EXISTS (SELECT 1 FROM title_provider_links l WHERE l.title_id = x.title_id AND l.provider = 'igdb' AND l.deleted = FALSE)
   AND NOT EXISTS (SELECT 1 FROM title_releases tr JOIN release_provider_links rl ON rl.release_id = tr.release_id AND rl.provider = 'igdb' AND rl.deleted = FALSE WHERE tr.title_id = x.title_id);
