# Plan: achievement badges on RetroArch screenshots

Status: plan only, nothing built yet.

## Why

RetroArch can take a screenshot when an achievement unlocks (`cheevos_auto_screenshot`), but
the picture it saves is the bare game frame: the unlock pop-up with the badge is an overlay
and is not captured. The app already holds both halves, the screenshot on disk and the
unlock (badge, title, time) from RetroAchievements, so it can put them back together.

## What exists already

- **Unlocks, per achievement:** `ra_user_achievements` (`badge_name`, `title`, `description`,
  `points`, `date_earned`, `date_earned_hardcore`), refreshed by the `ra_badges` job and by
  opening a game on the RetroAchievements tab.
- **Badge images, one file per badge:** RetroAchievements serves each badge on its own,
  `https://media.retroachievements.org/Badge/<BadgeName>.png` (64x64, colour) and
  `<BadgeName>_lock.png` (greyed). The `ra_badges` job downloads both into
  `<provider cache>/retroachievements/media/Badge/`. `GET /api/v1/ra/badges/{name}[?locked=true]`
  serves one. Badge images need no API allowance.
- **Image composition:** `api::ra::compose_mosaic` already draws badges with the `image`
  crate, so overlaying one badge on a screenshot is the same code path.

## Design

1. **Find the screenshots.** A configured folder (RetroArch's `screenshot_directory`, with
   or without "sort screenshots by content directory"). RetroArch names them
   `<content name>-YYMMDD-HHMMSS.png` in local time.
2. **Match a screenshot to an unlock.**
   - Content: the screenshot's content name, matched to a local game, then to its release,
     then to its RA game id (`release_provider_links`).
   - Time: the screenshot's timestamp against `date_earned` / `date_earned_hardcore`
     (RA returns UTC; convert with the machine's zone). Allow a few seconds of tolerance.
     The nearest unlock wins; ties go to the one RetroArch would have shown last.
3. **Compose.** Draw the pop-up RetroArch would have drawn: badge, title, points, hardcore
   or softcore marker, in a corner chosen by a setting. Keep the original untouched and
   write the composed copy beside it (`…-badge.png`), or into the release's media as a
   `screenshot` of source `manual`, so it joins the media strip and `media_priority`.
4. **Run it.**
   - As a step at the end of `ra_badges`: new unlocks, then the matching screenshots.
   - As a button on a game's RetroAchievements page: "add badges to this game's screenshots".

## Open questions

- Should the composed image be media (shown in the strip, synced to clients) or files only?
- Should screenshots taken without an unlock nearby be imported as plain screenshots too?
- The pop-up style: copy RetroArch's own look, or a simpler badge plus caption?
