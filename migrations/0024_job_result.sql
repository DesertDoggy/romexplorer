-- The job's outcome in full, for the UI. progress_message is a 255-character one-liner, and
-- the structured summaries a restore, a sync or an import produces (rows per table, tables
-- skipped, ownership declined, art left unresolved after a catalogue replacement) were only
-- ever written to the log. Written once, with the final progress, by database::jobs::Progress.
ALTER TABLE jobs ADD COLUMN result JSON NULL AFTER progress_message;
