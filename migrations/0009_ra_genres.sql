-- RetroAchievements genres for the RetroAchievements tab's genre filter. RA's bulk game
-- list carries no genre, so it is fetched one game at a time (API_GetGame) by the
-- ra_genre_fetch job -- local games automatically after an account sync, the whole list on
-- request (RAChecker's approach). genre_major = the 19 major genres the raw text folds
-- onto (providers::ra_genre), comma-separated; genre_checked_at marks a game as asked
-- even when RA has no genre for it.
ALTER TABLE retroachievements_games
    ADD COLUMN genre_major      VARCHAR(255) NULL AFTER genre_raw,
    ADD COLUMN genre_checked_at DATETIME NULL AFTER genre_major;

ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch') NOT NULL;
