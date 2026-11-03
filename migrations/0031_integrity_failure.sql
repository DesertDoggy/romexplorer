-- What the last integrity check made of a file, not only when it last passed.
--
-- `last_integrity_check` is stamped on a pass alone (0026): a mismatch is left unstamped so the
-- file stays at the front of the queue. That makes the queue right and the file page mute --
-- a file that failed and one never looked at are both just "no date". A failure here means
-- exactly one thing: the bytes on disk no longer hash to what the catalogue recorded for
-- them. A file that is gone, unreadable, or has no recorded hash to compare against is not a
-- failure and is not written here.
--
-- A later pass clears it, so the two columns never disagree about the current state.
ALTER TABLE local_files ADD COLUMN last_integrity_failure DATETIME NULL AFTER last_integrity_check;
-- The mirror of another machine's `local_files` carries the same columns (catalog sync
-- projects every source column).
ALTER TABLE remote_files ADD COLUMN last_integrity_failure DATETIME NULL AFTER last_integrity_check;
