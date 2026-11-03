-- Which worker runs a job.
--
-- A running job could only be cancelled by the worker whose task it was. When that task was
-- gone -- the process killed, or the database down at the moment the job ended, so its
-- outcome was never written -- the row stayed `in_progress` with nothing behind it: cancel
-- only ever set `cancel_requested`, and two hours later it was put back to `pending` and ran
-- again. Each worker now writes its id on the jobs it claims and holds a named lock
-- (`romexplorer_job_worker:<id>`) for as long as it lives; MariaDB frees the lock when the
-- process dies. A job whose worker holds no lock has no task behind it.
ALTER TABLE jobs ADD COLUMN worker_id VARCHAR(48) NULL AFTER claimed_at;
