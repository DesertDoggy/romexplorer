-- The merge review suggestion a merge or split came from. Undoing it in Combined titles puts
-- that suggestion back in the review as pending (`title_merge::reopen_suggestion`).
ALTER TABLE title_merge_log
    ADD COLUMN suggestion_key VARCHAR(64) NULL;
