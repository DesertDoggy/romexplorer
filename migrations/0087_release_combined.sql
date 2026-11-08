-- Releases the user combined into one (`api::release_combine`): the discs of one game that a
-- DAT lists as separate entries -- a bundle's discs, a bonus disc named apart from the game
-- -- each a release of its own, so a copy of the game took one disc. A combined release keeps
-- its own row, files and local copies (a DAT import still finds it by name); it is listed
-- under the release it was combined into, and a copy of that release takes it too.
CREATE TABLE release_combined (
    release_id  BIGINT UNSIGNED NOT NULL PRIMARY KEY,  -- the release combined into another
    lead_id     BIGINT UNSIGNED NOT NULL,              -- the one it is listed and copied with
    combined_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT fk_rcomb_release FOREIGN KEY (release_id) REFERENCES `release`(id),
    CONSTRAINT fk_rcomb_lead    FOREIGN KEY (lead_id)    REFERENCES `release`(id),
    INDEX idx_rcomb_lead (lead_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
