-- The day an IGDB game first came out (`first_release_date`), next to its year: a rating
-- on Collection > Definitive is marked when a version of the family came out after it was
-- made. `release_read`: the day was read out of the cached raw file (every game already
-- indexed is read once more by the next families pass, for this alone).
ALTER TABLE igdb_games
    ADD COLUMN first_release DATE NULL AFTER first_year,
    ADD COLUMN release_read BOOLEAN NOT NULL DEFAULT FALSE AFTER relations_read;
