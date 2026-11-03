-- What a patched release was made from.
--
-- A hack, a translation, a trainer, a save or a patch file is of some game: the title, and
-- where it can be told the very release ("Game (Japan)" for "Game (Japan) [T-En by X]").
-- Nothing recorded that. A patched release sat under a title because the import guessed the
-- title from a name, with no way to tell a certain link from a guess, to correct one, or to
-- get from the original's page to what was made from it.
--
-- One row per such release that has a link, or had one taken away:
--
--   hash       one of its files is, byte for byte, a file of that release: the original
--              shipped beside the patch. The surest there is; that file is then no longer
--              listed as one of the patched release's own (it is not a version of it)
--   exact      the name without its patch tags is a release of the same platform
--   igdb       its own title is IGDB's child of the base title (title.basegame_title_id)
--   suggested  put under a game by name alone: shown there marked as a suggestion until
--              the user confirms it
--   manual     confirmed or set by the user; nothing automatic touches it again
--   rejected   unlinked by the user; kept so the same pairing is not suggested again
--
-- base_title_id is the game; base_release_id the release, when one is known. The resolver
-- (`database::release_bases::resolve`, a step of the title merge job) fills the first four
-- and never touches `manual` or `rejected`.
CREATE TABLE release_bases (
    release_id      BIGINT UNSIGNED PRIMARY KEY,
    base_title_id   BIGINT UNSIGNED NULL,
    base_release_id BIGINT UNSIGNED NULL,
    status          ENUM('hash','exact','igdb','suggested','manual','rejected') NOT NULL,
    updated_at      DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
    CONSTRAINT fk_rb_release FOREIGN KEY (release_id) REFERENCES `release`(id) ON DELETE CASCADE,
    CONSTRAINT fk_rb_base    FOREIGN KEY (base_release_id) REFERENCES `release`(id) ON DELETE SET NULL,
    INDEX idx_rb_title (base_title_id),
    INDEX idx_rb_base (base_release_id),
    INDEX idx_rb_status (status)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
