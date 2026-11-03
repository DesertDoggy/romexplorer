-- Redump gives every serial a disc was sold under in one field, and 64 characters held about
-- five of them: longer lists were first rejected (failing the whole DAT), then cut. 1024 holds
-- every list in the current Redump sets with room to spare. The column is not indexed, so the
-- width costs nothing until it is used.
ALTER TABLE `release` MODIFY COLUMN serial VARCHAR(1024) NULL;
