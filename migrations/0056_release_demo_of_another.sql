-- A demo named after another game is a demo.
--
-- Only a bare "(Demo)" or "(Taikenban)" was recognised. A bonus disc filed under the game it
-- came with names the game it is a demo of -- "Hoge Quest (Japan) (Disc 2) (Fuga Saga
-- Taikenban)" -- and was taken for part of that game. Its hash then linked the whole title to
-- the other game's IGDB entry, and the merge pass joined the two games into one title. The
-- import now recognises such a tag (`name_parse::classify_variant`); this marks the releases
-- already imported. A demo under a title that also holds a full game takes its provider links
-- itself (`providers::cache::APART_SQL`).
UPDATE `release`
SET is_demo = TRUE
WHERE is_demo = FALSE
  AND name REGEXP '\\([^()]* (Demo|Taikenban|Trial|Otameshiban)\\)';
