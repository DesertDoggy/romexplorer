-- A server's directory order, mirrored. A client copies from the server's files, so the
-- copy it takes of a release -- and the file it takes when one copy holds two dumps of one
-- file, a headered and a headerless cartridge -- follows the server's Config > Directories
-- order, as a copy on the server itself does.
ALTER TABLE remote_directories ADD COLUMN priority INT NOT NULL DEFAULT 0;

-- The rows already mirrored have no priority yet: the next pull takes the table again.
DELETE FROM remote_sync_state WHERE table_name = 'local_directories';
