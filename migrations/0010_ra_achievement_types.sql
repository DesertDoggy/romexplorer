-- Achievement types (progression / win_condition / missable) were read from the wrong JSON
-- key ("type"; RA sends "Type"), so every cached game has them empty. Forget which games
-- were fetched; each is fetched again (with its types) the next time it is shown.
DELETE FROM ra_user_game_detail;
