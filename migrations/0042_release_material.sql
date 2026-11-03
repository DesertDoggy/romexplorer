-- What a release is when it is not software.
--
-- The DATs list more than games: Redump has "Audio CD" (7,545 discs), "DVD-Video" (3,665),
-- "Video CD", "BD-Video" and "Photo CD"; No-Intro has magazine, manual and document scans,
-- game soundtracks, UMD Video and source code; TOSEC has Magazines, Manuals, Books and
-- Catalogs for most machines. All of it was imported as releases of an unknown platform and
-- listed among the titles, a bonus DVD next to the game it came with.
--
--   NULL      software: games, applications, BIOS, firmware, demos
--   audio     music discs and soundtracks
--   video     DVD-Video, BD-Video, Video CD, UMD Video
--   document  manuals, magazines, books, scans
--   image     Photo CD, wallpapers, artwork
--   source    source code
--
-- Set by the import from the DAT's name (`platform_map::material_from_dat_name`). The lists
-- show NULL unless asked for something else.
--
-- No index: both are appended as nullable columns, which is instant on a `release` table of
-- millions of rows, and the lists test the column on rows they have already joined.
--
-- `dat_imports.material` is the same value for the DAT as a whole. NULL there also means
-- "imported before this existed", which is how a DAT sync finds the non-software DATs to
-- import once more (`import::unclassified_members`).
ALTER TABLE `release`
    ADD COLUMN material VARCHAR(16) NULL;

ALTER TABLE dat_imports
    ADD COLUMN material VARCHAR(16) NULL;
