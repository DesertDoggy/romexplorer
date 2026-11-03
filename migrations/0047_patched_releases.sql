-- Patched ROMs, community material, and the versions of one patch.
--
-- release.game_kind        what a game is when it is not a commercial release:
--                            hack | mod | translation   made from an existing game
--                            homebrew | arcade | wiiware
--                          NULL for everything else. `community_patched` and `patch_type`
--                          stay as they are; this is what the lists filter and badge by, and
--                          a translation is `translation`, not `hack`.
--
-- release.languages_state  how far `languages` is to be believed on such a release:
--                            confirmed  the name states it ("[T-En ...]", "Spanish Translation")
--                            guessed    inferred from a folder or a loose word
--                            manual     the user confirmed, changed or cleared it; an import
--                                       never touches it again
--                          NULL on ordinary releases, whose languages are the DAT's own.
--
-- release.base_hint        the name of the game a patched release was made from, as its DAT
--                          gives it (a folder, a `cloneof`): what the link to the original
--                          is suggested from when the name alone does not settle it.
--
-- release_contents.variant which build of a patched work a file is ("v1.02"). The versions
--                          of one patch are one release; each is a dump of it, and holding
--                          any one is holding the release (`catalog::best_coverage` groups
--                          by source and variant). NULL everywhere else.
--
-- `release.material` also gains `mappack`, `trainer`, `save`, `patch` and `tool`.
ALTER TABLE `release`
    ADD COLUMN game_kind VARCHAR(16) NULL,
    ADD COLUMN languages_state VARCHAR(12) NULL,
    ADD COLUMN base_hint VARCHAR(512) NULL;

ALTER TABLE release_contents
    ADD COLUMN variant VARCHAR(64) NULL;

-- What is already known to be patched.
UPDATE `release` SET game_kind = 'translation' WHERE game_kind IS NULL AND patch_type = 'translation';
UPDATE `release` SET game_kind = 'hack' WHERE game_kind IS NULL AND (community_patched = TRUE OR patch_type = 'gameplay');
-- RetroAchievements' lists say so in a category the import used to keep and not act on.
UPDATE `release` SET game_kind = 'hack', community_patched = TRUE, patch_type = COALESCE(patch_type, 'gameplay') WHERE game_kind IS NULL AND material IS NULL AND category = 'Hack';
UPDATE `release` SET game_kind = 'homebrew' WHERE game_kind IS NULL AND material IS NULL AND category = 'Homebrew';
