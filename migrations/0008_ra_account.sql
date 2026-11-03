-- The RetroAchievements account from api.env (ra_username): its profile and every game it
-- has played with its award level (API_GetUserProfile / API_GetUserCompletionProgress),
-- refreshed by the ra_account_sync job. Keyed by user so a changed account doesn't mix.
CREATE TABLE ra_user_profile (
    ra_user               VARCHAR(64) PRIMARY KEY,
    user_pic              VARCHAR(255) NULL,
    member_since          VARCHAR(32) NULL,
    rich_presence         VARCHAR(512) NULL,
    last_game_id          INT UNSIGNED NULL,
    total_points          BIGINT UNSIGNED NULL,     -- hardcore
    total_softcore_points BIGINT UNSIGNED NULL,
    total_true_points     BIGINT UNSIGNED NULL,     -- RetroPoints
    motto                 VARCHAR(255) NULL,
    fetched_at            DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE ra_user_progress (
    ra_user              VARCHAR(64) NOT NULL,
    game_id              INT UNSIGNED NOT NULL,
    title                VARCHAR(512) NULL,
    console_id           INT UNSIGNED NULL,
    console_name         VARCHAR(128) NULL,
    image_icon           VARCHAR(255) NULL,
    max_possible         INT UNSIGNED NULL,
    num_awarded          INT UNSIGNED NULL,       -- softcore + hardcore
    num_awarded_hardcore INT UNSIGNED NULL,
    most_recent_awarded  DATETIME NULL,
    -- mastered (hardcore complete) / completed (softcore complete) / beaten-hardcore /
    -- beaten-softcore / NULL
    highest_award_kind   VARCHAR(20) NULL,
    highest_award_date   DATETIME NULL,
    fetched_at           DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (ra_user, game_id),
    INDEX idx_rup_award (ra_user, highest_award_kind),
    INDEX idx_rup_recent (ra_user, most_recent_awarded)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- RA's per-game images (icon from the game list; title screen, in-game screenshot and box
-- art from API_GetGame, fetched lazily the first time a grid asks for them).
ALTER TABLE retroachievements_games
    ADD COLUMN console_name VARCHAR(128) NULL AFTER console_id,
    ADD COLUMN image_icon   VARCHAR(255) NULL,
    ADD COLUMN image_title  VARCHAR(255) NULL,
    ADD COLUMN image_ingame VARCHAR(255) NULL,
    ADD COLUMN image_boxart VARCHAR(255) NULL;

ALTER TABLE jobs
    MODIFY COLUMN job_type ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize','playmatch_fill','media_relayout','ra_account_sync') NOT NULL;

-- One game's achievements with the account's unlocks (API_GetGameInfoAndUserProgress),
-- fetched lazily when a badge mosaic or game page is shown; re-fetched when the account's
-- progress shows a newer unlock than fetched_at (or after 30 days).
CREATE TABLE ra_user_game_detail (
    ra_user    VARCHAR(64) NOT NULL,
    game_id    INT UNSIGNED NOT NULL,
    fetched_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (ra_user, game_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE ra_user_achievements (
    ra_user              VARCHAR(64) NOT NULL,
    game_id              INT UNSIGNED NOT NULL,
    achievement_id       INT UNSIGNED NOT NULL,
    title                VARCHAR(255) NULL,
    description          VARCHAR(1024) NULL,
    points               INT UNSIGNED NULL,
    true_ratio           INT UNSIGNED NULL,
    badge_name           VARCHAR(32) NULL,
    display_order        INT NULL,
    kind                 VARCHAR(32) NULL,        -- progression / win_condition / missable / NULL
    date_earned          DATETIME NULL,
    date_earned_hardcore DATETIME NULL,
    PRIMARY KEY (ra_user, game_id, achievement_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
