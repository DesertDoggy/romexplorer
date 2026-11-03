-- DLC and updates among the releases already imported.
--
-- `release.material` (0042) also says `dlc` and `update`: an add-on to a game or a newer
-- version of it, which the DATs list entry by entry inside ordinary game DATs -- No-Intro's
-- "(DLC)", "(Addon)", "(Update)", "(Title Update)" and "(Patch)" tags, Redump's "Add-Ons"
-- and "Updates" categories. About 80,000 of 1.18 million releases on a full import.
--
-- Unlike the kinds 0042 describes, these can be told from the release's own name, so the
-- rows already there are stamped here rather than by the next DAT sync. This is
-- `platform_map::addon_from_release` as SQL, as it stood when this was written; new imports
-- go through the Rust.
--
-- Not a console's system software ("[BIOS] PS3 System Software Update ... (Update)") and not
-- a hack. DLC first: "(DLC) ... (Update)" is DLC.
UPDATE `release`
   SET material = 'dlc'
 WHERE material IS NULL
   AND name NOT LIKE '[BIOS]%'
   AND COALESCE(category, '') <> 'Hack'
   AND name REGEXP '[(](DLC[^)]*|Addon|Add-On[^)]*|Expansion[^)]*)[)]';

UPDATE `release`
   SET material = 'update'
 WHERE material IS NULL
   AND name NOT LIKE '[BIOS]%'
   AND COALESCE(category, '') <> 'Hack'
   AND name REGEXP '[(](Update|Update [^)]*|Title Update|Patch|Patch [^)]*)[)]';

UPDATE `release` SET material = 'dlc' WHERE material IS NULL AND category = 'Add-Ons' AND name NOT LIKE '[BIOS]%';
UPDATE `release` SET material = 'update' WHERE material IS NULL AND category = 'Updates' AND name NOT LIKE '[BIOS]%';
