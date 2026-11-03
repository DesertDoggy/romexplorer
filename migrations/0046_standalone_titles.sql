-- Disc media, documents and source code have no title.
--
-- A title is a game. An audio CD, a DVD-Video, a magazine scan or a source archive
-- (`release.material` audio / video / image / document / source) is a thing in its own
-- right, listed under Collection > Multimedia, Documents and Source code by its own name.
-- The import gave each a title all the same -- about 70,000 titles that existed only to
-- hold one disc or one PDF -- and hid those from the Titles list.
--
-- From here on such a release has a title only when someone links it to a game's
-- (`api::titles::link_release`): the soundtrack disc of a game, its strategy guide. The
-- import no longer makes one, and the ones it made are undone here.
--
-- First the titles that are nothing else (while the links that say so are still there),
-- then every such link -- also the ones onto a game's title, which only a shared name made:
-- "no title until linked" is the same rule for all of them.
UPDATE title t
   SET t.deleted = TRUE
 WHERE t.deleted = FALSE
   AND EXISTS (SELECT 1 FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE tr.title_id = t.id AND r.material IN ('audio', 'video', 'image', 'document', 'source'))
   AND NOT EXISTS (SELECT 1 FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE tr.title_id = t.id AND (r.material IS NULL OR r.material NOT IN ('audio', 'video', 'image', 'document', 'source')));

DELETE tr
  FROM title_releases tr
  JOIN `release` r ON r.id = tr.release_id
 WHERE r.material IN ('audio', 'video', 'image', 'document', 'source');
