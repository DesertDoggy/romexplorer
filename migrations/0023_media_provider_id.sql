-- Where a piece of art came from, as the provider's own id for the thing it depicts.
--
-- `source` already records which provider supplied it. What was missing is a handle that
-- survives the catalogue being renumbered: `title_media.title_id` and `release_media.release_id`
-- are local row ids, so after a catalogue replacement they point at whatever now holds those
-- ids -- art silently attached to a different game.
--
-- (source, provider_id) is machine-independent in exactly the way `title_provider_links
-- (provider, provider_id)` already is, so the junction can be rebuilt by asking the freshly
-- synced catalogue "which title does this provider call 1234?" rather than by trusting an id
-- whose meaning has changed.
--
-- NULL for manual uploads and for every row written before this migration: those cannot be
-- re-linked and are reported as such rather than guessed at.
ALTER TABLE media
    ADD COLUMN provider_id VARCHAR(64) NULL AFTER source,
    ADD INDEX idx_media_provider (source, provider_id);

-- Which junction each media row was in before a catalogue replacement dropped them.
--
-- Provider art is attached to *releases* (`providers::media::attach_media` resolves a title to
-- its releases), while manual picks and some title-level art sit on `title_media`. Rebuilding
-- from (source, provider_id) alone cannot tell those apart, and guessing would silently move
-- release art up to the title. Two columns and one row per link removes the guess.
--
-- Written by `api::sources::clear_catalog` and consumed by `api::sync::relink_media`; empty at
-- every other moment.
CREATE TABLE media_relink (
    media_id BIGINT UNSIGNED NOT NULL,
    kind     ENUM('title','release') NOT NULL,
    PRIMARY KEY (media_id, kind),
    CONSTRAINT fk_mrl_media FOREIGN KEY (media_id) REFERENCES media(id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
