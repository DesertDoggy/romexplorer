-- Live progress for long-running jobs (provider bulk syncs, DAT downloads), written by the
-- worker as it goes (throttled, see database::jobs::Progress) and shown by the console.
-- progress_total NULL = the amount of work isn't known up front (then only done/message).
ALTER TABLE jobs
    ADD COLUMN progress_done    BIGINT UNSIGNED NULL AFTER last_error,
    ADD COLUMN progress_total   BIGINT UNSIGNED NULL AFTER progress_done,
    ADD COLUMN progress_message VARCHAR(255) NULL AFTER progress_total;
