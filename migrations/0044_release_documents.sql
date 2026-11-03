-- Reading matter that 0042's rule did not catch.
--
-- 0042 told a document by its DAT's name, from a list of words. That missed TOSEC's
-- "Game Guides" (969 releases), and everything whose DAT is named for a machine and not for
-- what it holds: manuals filed under a console's "Games" (3,324 for the unknown platforms
-- alone, 283 for Xbox 360, 202 for Saturn), No-Intro's printables and SDK documents. 40,928
-- releases of a full import have nothing but PDFs and the like, and all of them were still
-- among the games.
--
-- So a release is a document when its DAT's category says so, or when every file it lists
-- is one that is only ever read. This is `platform_map::document_from_release` as SQL, as it
-- stood when this was written; new imports go through the Rust. Unlike 0042's kinds it can
-- be told from the rows themselves, so nothing waits for a DAT sync.
--
-- A release already stamped keeps its kind, except that an "update" which is nothing but a
-- PDF is a document: 0043 went by the name alone.
UPDATE `release`
   SET material = 'document'
 WHERE material IS NULL
   AND category IN ('Manuals', 'Magazines', 'Game Guides', 'Books', 'Catalogs', 'Docs', 'Newsletters', 'Comics', 'Artbooks');

UPDATE `release` r
   SET r.material = 'document'
 WHERE (r.material IS NULL OR r.material IN ('dlc', 'update'))
   AND EXISTS (SELECT 1 FROM release_contents rc WHERE rc.release_id = r.id)
   AND NOT EXISTS (SELECT 1 FROM release_contents rc WHERE rc.release_id = r.id AND LOWER(rc.file_name) NOT REGEXP '[.](pdf|cbz|cbr|djvu|epub)$');
