-- The path of a member inside its archive ("Disc/Track 02.bin"), recorded on the archive →
-- member link: the same bytes can sit at different paths in different archives.
ALTER TABLE content_contents
    ADD COLUMN member_path VARCHAR(512) NULL;
