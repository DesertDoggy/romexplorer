-- How sure each merge was, so merges can be filtered by it and undone together.
--
--   manual  the user merged the titles, accepted a suggestion, or linked every one by hand
--   hash    a hash put them under one game (Hasheous / PlayMatch / RetroAchievements)
--   name    at least one title is linked to the game only because its name is one of the
--           game's names
--
-- Every automatic merge made before this needed a hash-grade link, so those are `hash`.
ALTER TABLE title_merge_log
    ADD COLUMN grade VARCHAR(8) NOT NULL DEFAULT 'hash',
    ADD INDEX idx_tml_grade (grade);

UPDATE title_merge_log SET grade = 'manual' WHERE reason = 'manual' OR reason LIKE 'suggestion:%';
