-- Where a pending job stands in its lane ("Run next", `database::jobs::run_next`).
--
-- The worker starts the head of each lane group, and the head used to be the lowest id: the
-- order jobs were added in, and nothing could change it. A job's place is now its own number,
-- the id to begin with (set right after the insert), and "Run next" gives a waiting job a
-- number below the rest of its lane so it starts as soon as the job running there is done.
-- The id itself never changes.
ALTER TABLE jobs ADD COLUMN queue_order BIGINT NULL;
UPDATE jobs SET queue_order = id;
CREATE INDEX idx_jobs_queue_order ON jobs (status, queue_order);
