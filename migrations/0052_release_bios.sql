-- BIOS, firmware and other system files are not games.
--
-- They were marked only by a free-text `release.category` ('BIOS', 'Firmware': retrobios,
-- TOSEC, Redump's BIOS DATs) or a "[BIOS]" name (No-Intro). `material` stayed NULL -- the
-- value for a game -- so the import gave every one a title and the Titles list showed them
-- among the games. From here on they are `material = 'bios'` (`platform_map::bios_from_release`),
-- listed under Collection > BIOS / firmware with their platform, and have no title.
--
-- The same rule as the import, for what is already here; then the titles left holding only
-- such releases go, and every link from one to a title (as 0046 did for disc media).
UPDATE `release` r
   SET r.material = 'bios'
 WHERE r.material IS NULL
   AND (r.category IN ('BIOS', 'Firmware')
        OR r.name LIKE '[BIOS]%'
        OR EXISTS (SELECT 1 FROM release_sources rs JOIN sources s ON s.id = rs.source_id WHERE rs.release_id = r.id AND s.name = 'retrobios'));

UPDATE title t
   SET t.deleted = TRUE
 WHERE t.deleted = FALSE
   AND EXISTS (SELECT 1 FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE tr.title_id = t.id AND r.material = 'bios')
   AND NOT EXISTS (SELECT 1 FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE tr.title_id = t.id AND (r.material IS NULL OR r.material <> 'bios'));

DELETE tr
  FROM title_releases tr
  JOIN `release` r ON r.id = tr.release_id
 WHERE r.material = 'bios';

-- What a scan identified a file as, when it is a system file (`rom::detect::systemfile`):
-- bios | firmware | nand | sdcard | font | kanji | dsp | other, and the platform it belongs
-- to. A NAND or SD-card dump is in no DAT, so this is the only place it is known for one.
ALTER TABLE local_files
    ADD COLUMN system_type VARCHAR(16) NULL,
    ADD COLUMN system_platform SMALLINT UNSIGNED NULL;
