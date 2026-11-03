-- A name dictionary, and automatic tags apart from the user's own.
--
-- `name_dictionary`: one game or series under its names in different scripts -- "Hoge
-- Quest" and ホゲクエスト -- each name one row of a group. A search in one script also tries
-- the others (titles, releases, games, the cross-platform list, audio), and audio is tagged
-- with the title it belongs to whichever language its album is named in. `norm` is the
-- name folded for matching (`database::dictionary::norm`: NFKC, lowercase, hiragana as
-- katakana, roman numerals as digits, separators dropped), compared byte for byte: under a
-- case- or accent-insensitive collation two different names could collide on the key.
--
-- `source`: catalog (a title's own name beside its localized names), series (pairs of
-- numbered titles with the number taken off), wikidata (downloaded on request), user (added
-- by hand). A rebuild replaces the first three; `name_dictionary_blocks` keeps the
-- automatic names the user removed from coming back.
CREATE TABLE name_dictionary (
    id        BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
    group_key VARCHAR(64) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL,
    term      VARCHAR(255) NOT NULL,
    norm      VARCHAR(255) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL,
    cjk       BOOLEAN NOT NULL,
    source    VARCHAR(16) NOT NULL,
    UNIQUE KEY uq_nd_group_norm (group_key, norm),
    INDEX idx_nd_norm (norm),
    INDEX idx_nd_source (source)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE name_dictionary_blocks (
    group_key VARCHAR(64) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL,
    norm      VARCHAR(255) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL,
    PRIMARY KEY (group_key, norm)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Who put a tag on an audio file: the user (`manual`) or the auto-tagger (`auto`). Title
-- keyword tags already say so in `title_keywords.source` ('user' / 'auto').
ALTER TABLE audio_file_tags ADD COLUMN source VARCHAR(8) NOT NULL DEFAULT 'manual';

ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync','ra_genre_fetch','title_dedupe','igdb_reapply','ra_patch_download','title_merge_audit','backup_database','backup_catalog','backup_dat_seed','catalog_sync','restore_backup','sqlite_export','sqlite_import','copy_files','integrity_check','sheet_create','sheet_verify','m3u_verify','media_sync','audio_scan','audio_sync','media_backfill','ra_badges','dictionary_build','audio_autotag') NOT NULL;
