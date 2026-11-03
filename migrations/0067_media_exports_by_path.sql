-- Exporting art no longer needs an emulator: it can go into any folder. What an export wrote is
-- then a file path, whoever asked for it -- two settings pointing at one folder share it -- so
-- the record is keyed by the path alone, and the emulator (when there was one) is a note.
DELETE e1 FROM media_exports e1 JOIN media_exports e2 ON e1.path_sha1 = e2.path_sha1 AND e1.written_at < e2.written_at;
DELETE e1 FROM media_exports e1 JOIN media_exports e2 ON e1.path_sha1 = e2.path_sha1 AND e1.written_at = e2.written_at AND e1.emulator_id < e2.emulator_id;
ALTER TABLE media_exports DROP FOREIGN KEY fk_mexp_emulator;
ALTER TABLE media_exports
    DROP PRIMARY KEY,
    MODIFY COLUMN emulator_id BIGINT UNSIGNED NULL,
    ADD PRIMARY KEY (path_sha1),
    ADD INDEX idx_mexp_emulator (emulator_id);
