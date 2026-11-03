-- A schedule's "run only when idle": a due run waits until no job is pending or running
-- instead of joining the queue behind (or beside) work the user started by hand. On for
-- every existing schedule too: a scheduled sync has no deadline, and the reason this exists
-- is scheduled work competing with an import the user was watching.
ALTER TABLE schedules ADD COLUMN when_idle BOOLEAN NOT NULL DEFAULT TRUE AFTER enabled;
