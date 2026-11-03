-- When RetroAchievements last changed a game's achievement set (API_GetGameList's
-- DateModified), and which of those dates the account's fetched achievements reflect -- so an
-- edited set (a reworded achievement, new points, new badge art, achievements added or
-- removed) is fetched again, not only a game with a new unlock.
ALTER TABLE retroachievements_games ADD COLUMN set_modified_at DATETIME NULL AFTER num_achievements;
ALTER TABLE ra_user_game_detail ADD COLUMN set_modified_at DATETIME NULL AFTER fetched_at;
