-- A game linked to a release by a serial rather than by its files' hashes (lenient matching,
-- `database::serial_match`): which kind of serial it was, and the serial. NULL for every
-- link a hash made, or a person.
ALTER TABLE local_release_games
    ADD COLUMN serial_match ENUM('header', 'manifest', 'file_name') NULL,
    ADD COLUMN serial_matched VARCHAR(64) NULL;
