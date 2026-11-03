-- How far a long job got (`jobs::Progress::checkpoint`): a job started again after a restart
-- goes on from there instead of checking everything it already did. Claiming a job leaves it
-- alone; each job type decides what it holds.
ALTER TABLE jobs ADD COLUMN checkpoint VARCHAR(512) NULL;
