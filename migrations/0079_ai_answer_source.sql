-- Where a saved AI answer came from (`api::ai`): asked through a provider's API by the app
-- (`api`, `provider` = gemini / claude / openai / perplexity), or pasted in by the user from
-- one of the Ask menu's browser links (`browser`, `provider` = google / claude / chatgpt /
-- perplexity, `pasted_at` when). One question can now hold an answer per source: the unique
-- key takes the source and the provider, and `ai_source_priority` picks which one is shown.
ALTER TABLE ai_answers
    ADD COLUMN source ENUM('api', 'browser') NOT NULL DEFAULT 'api' AFTER provider,
    ADD COLUMN pasted_at TIMESTAMP NULL,
    DROP INDEX uq_ai_answers,
    ADD UNIQUE KEY uq_ai_answers (purpose, subject_key, lang, prompt_hash, source, provider);
