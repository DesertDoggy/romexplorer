-- A song record (a sequenced track's engine, its data's place in the ROM, its runs) can be
-- longer than 1024 characters; cut there, it no longer plays. It is stored whole.
ALTER TABLE rom_audio_tracks
    MODIFY COLUMN inner_path TEXT NULL;
