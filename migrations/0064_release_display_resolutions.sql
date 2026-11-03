-- Display `modes` become exact resolutions: WIDTHxHEIGHT and p or i, with the output they are
-- on for a handheld (`screen:960x544p`), e.g. a SNES game's `256x224p` instead of "240p".
-- A list of them is longer than 64 characters.
ALTER TABLE release_display MODIFY COLUMN value VARCHAR(512) NOT NULL;

-- The rows written before, as resolutions: PARAM.SFO's 480 / 576 / 720 / 1080 (1080 is either
-- scan, the console's setting picks it), and the line classes a user could pick.
UPDATE release_display
SET value = TRIM(BOTH ',' FROM
    REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
        CONCAT(',', value, ','),
        ',240p,', ',320x240p,'),
        ',288p,', ',320x288p,'),
        ',480i,', ',640x480i,'),
        ',480p,', ',720x480p,'),
        ',576i,', ',640x576i,'),
        ',576p,', ',720x576p,'),
        ',720p,', ',1280x720p,'),
        ',1080,', ',1920x1080i,1920x1080p,'),
        ',1080i,', ',1920x1080i,'),
        ',1080p,', ',1920x1080p,'))
WHERE field = 'modes';
