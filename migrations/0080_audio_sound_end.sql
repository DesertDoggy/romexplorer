-- Where a tagged audio file's sound ends: the last sample above -60 dBFS, in ms. Measured
-- with the loudness (`api::audio::measure_loudness`); the player moves to the next track
-- there instead of playing out the file's silent end. NULL until measured.
ALTER TABLE audio_files
    ADD COLUMN sound_end_ms INT UNSIGNED NULL AFTER loudness_lufs;
