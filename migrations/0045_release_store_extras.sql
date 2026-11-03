-- The console stores' extras among the releases already imported.
--
-- No-Intro lists what the stores sold beside the games: PS3, Vita and PSP themes and avatars
-- (about 5,500 entries on a full import; the PS3 ones have DATs of their own, "(PSN)
-- (Avatars)" and "(PSN) (Themes)", the rest are in "(PSN) (Content)" with the games),
-- soundtracks, videos, manuals and the unlock keys of trials. Each says so in a tag of its
-- own name, and each was a title.
--
--   theme     themes and avatars -- a new `release.material` value: a game's own extra, like
--             its DLC, bundled with its title and listed in a section of its own
--   audio     (Soundtrack)
--   video     (Video), (E3 Video)
--   document  (Manual)
--   dlc       (Unlock Key): nothing without its trial
--
-- This is `platform_map::tagged_material` as SQL, as it stood when this was written; new
-- imports go through the Rust. Whole tags only, so Satellaview's "(SoundLink Magazine)"
-- broadcasts stay software. A release 0043 took for DLC or an update by another tag is
-- what this says it is: "... (Theme) (Update)" is a theme.
UPDATE `release`
   SET material = 'theme'
 WHERE (material IS NULL OR material IN ('dlc', 'update'))
   AND name NOT LIKE '[BIOS]%'
   AND name REGEXP '[(](Theme|Dynamic Theme|Static Theme|Avatar|Avatar Item)[)]';

UPDATE `release` SET material = 'audio' WHERE material IS NULL AND name NOT LIKE '[BIOS]%' AND name REGEXP '[(]Soundtrack[)]';
UPDATE `release` SET material = 'video' WHERE material IS NULL AND name NOT LIKE '[BIOS]%' AND name REGEXP '[(](Video|E3 Video)[)]';
UPDATE `release` SET material = 'document' WHERE material IS NULL AND name NOT LIKE '[BIOS]%' AND name REGEXP '[(]Manual[)]';
UPDATE `release` SET material = 'dlc' WHERE material IS NULL AND name NOT LIKE '[BIOS]%' AND name REGEXP '[(]Unlock Key[)]';
