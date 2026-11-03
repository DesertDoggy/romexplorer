-- Merge suggestions of more kinds than "these titles share an IGDB game": regional variants
-- (International, Director's Cut), names that are translations of each other, a bundle beside
-- its game, and a Japanese release IGDB has no page for filed under another game. `kind`
-- says which (`igdb` for the ones before), `priority` orders the review queue, and `detail`
-- holds the choices a review offers, each as the titles the releases end up in.
ALTER TABLE title_merge_suggestions
    ADD COLUMN kind VARCHAR(32) NOT NULL DEFAULT 'igdb' AFTER reason,
    ADD COLUMN priority SMALLINT NOT NULL DEFAULT 0 AFTER kind,
    ADD COLUMN detail JSON NULL AFTER priority,
    ADD INDEX idx_tms_queue (status, priority, updated_at);
