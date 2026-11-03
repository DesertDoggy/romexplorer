-- A set entry for a game's family (Collection > Definitive): `family_title_id` is the title
-- heading the family when it was added (`api::sets::resolve` looks its head up again, so a
-- family that gained a new head keeps its entry). It resolves to one release of the family,
-- as `copy_definitive` says: the best version held, or the definitive version only.
ALTER TABLE collection_set_entries ADD COLUMN family_title_id BIGINT UNSIGNED NULL AFTER igdb_id,
    ADD UNIQUE KEY uq_cse_family (set_id, family_title_id);
