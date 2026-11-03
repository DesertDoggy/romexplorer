-- Saved AI answers (`api::ai`): one per purpose, subject, language and question, so a card
-- opened again shows what was paid for instead of asking again.
--
-- `subject_key` is made of stable ids (IGDB ids, the platform, the variant), never row ids,
-- so the answers survive a rebuilt catalogue and can move to another install (export /
-- import, and the DATABASE backup). `prompt_hash` is the SHA-1 of `prompt`, the full text
-- sent (question and language line): a changed question is a new answer. `cost_usd` is
-- approximate (tokens x the price table). A pinned answer is never replaced or expired.
CREATE TABLE ai_answers (
    id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    purpose VARCHAR(64) NOT NULL,
    subject_key VARCHAR(255) NOT NULL,
    lang VARCHAR(8) NOT NULL,
    provider VARCHAR(16) NOT NULL,
    model VARCHAR(96) NOT NULL,
    prompt_hash CHAR(40) NOT NULL,
    prompt TEXT NOT NULL,
    answer MEDIUMTEXT NOT NULL,
    sources JSON NULL,
    tokens_in INT UNSIGNED NOT NULL DEFAULT 0,
    tokens_out INT UNSIGNED NOT NULL DEFAULT 0,
    cost_usd DECIMAL(12, 6) NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    pinned BOOLEAN NOT NULL DEFAULT FALSE,
    UNIQUE KEY uq_ai_answers (purpose, subject_key, lang, prompt_hash),
    INDEX idx_ai_answers_subject (purpose, subject_key)
);
