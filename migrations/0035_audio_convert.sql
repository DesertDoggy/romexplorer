-- Audio that is converted before it is played or copied, and copies a client keeps.
--
-- `support` replaces the yes/no of `playable` with the three cases there really are:
--   direct       the browser plays the file itself;
--   converted    decodable here but not by a browser (DSD, ALAC, AIFF): served from a
--                conversion made on first use (`api::audio_convert`);
--   unsupported  nothing here decodes it (WMA, APE, WavPack...): listed, never played.
-- `playable` stays, now meaning "the player can use it" = not unsupported, so everything
-- that filtered on it keeps working and DSD / ALAC / AIFF join the playlists.
ALTER TABLE audio_files
    ADD COLUMN support ENUM('direct','converted','unsupported') NOT NULL DEFAULT 'direct' AFTER playable,
    -- Integrated loudness (LUFS), measured at scan time, for the player's "normalise".
    ADD COLUMN loudness_lufs FLOAT NULL AFTER duration_ms,
    -- Which track of which main server a client's kept copy came from (`audio_sync`), so it
    -- is fetched once and the playlist lists it once.
    ADD COLUMN remote_source_id SMALLINT UNSIGNED NULL,
    ADD COLUMN remote_id BIGINT UNSIGNED NULL,
    ADD UNIQUE KEY uq_audio_remote (remote_source_id, remote_id);

UPDATE audio_files SET support = 'converted', playable = TRUE
 WHERE format IN ('dsf', 'dff', 'aiff', 'aif') OR (playable = FALSE AND format IN ('m4a', 'm4b', 'mp4', 'alac'));
UPDATE audio_files SET support = 'unsupported' WHERE playable = FALSE;

ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply','ra_patch_download','title_merge_audit','backup_database','backup_catalog','backup_dat_seed','catalog_sync','restore_backup','sqlite_export','sqlite_import','copy_files','integrity_check','sheet_create','sheet_verify','m3u_verify','media_sync','audio_scan','audio_sync') NOT NULL;
