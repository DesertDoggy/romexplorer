# romexplorer 0.2.2 (alpha)

Alpha: the database schema may change between releases with no upgrade path. Back up before
replacing a build.

## Changes

**Scan**
- "Quick scan (changed)" was added: it also reads files whose size or date changed.
- Scan hashes are set separately for regular files, archive files and what is inside archives. Archive files use sha1 and xxh3_128 by default.

**Copy**
- A copy started again checks the files already at the destination several at a time.

**Lists**
- Card grids with many entries open and scroll faster.

**Progress**
- Every job shows what it is doing, the total and how far it is, including folder walks, checks and database steps.
- Jobs made of several steps list their steps.
- Jobs have a "details" button that shows the steps and the files being read.
- Running jobs show the time taken and the time left.
- Large downloads and file checks show how much has been read.
- Clearing the catalogue and importing a sets file run as jobs.
- The desktop app shows what it is doing while it starts.

**Audio**
- Several tracks can be converted for playback at once.

**ROM audio**
- The ROM music index can read several ROMs at once (4 by default, set under Audio settings).
- The ROM music index reads only the best-ranked copy of each title, and other copies only when it has no music.
- On games with several discs, music files an earlier disc already has are not read again.
- Cartridge games whose music the app already knows play it as soon as the title page opens, without waiting for the index.
- "Clear ROM audio index" was added (all ROMs or one platform).

## Bug fixes

- The audio scan's progress stopped while it measured loudness.
- The ROM music index's progress moved only every 500 ROMs.

# romexplorer 0.2.1 (alpha)

Alpha: the database schema may change between releases with no upgrade path. Back up before
replacing a build.

## Changes

**3DS**
- 3DS ROMs can be converted to other formats.
- No keys are bundled. Put aes_keys.txt (and seeddb.bin when a title needs it) beside the files,
  beside romexplorer, or in its assets folder.
- 3DS ROMs can be serial matched.

**ROM audio**
- Opening a title page now indexes that game's ROM music first.
- The ROM music index now reads the smallest ROMs first and skips unsupported ROMs.
- PS1 games with their own sound engine are supported.

## Bug fixes

- The ROM music index looked stuck while one ROM took long to read.
- The ROM music index skipped ROMs that were identified only by a DAT match.
- A scan left Japanese Super Famicom ROMs whose internal title is in katakana without a platform.
- A title page listed the same music once for every copy of the game it found.
- ROM music indexing never finished on some disc images.
- When a copy converted SNES dumps, a file whose size and internal header disagreed about a
  copier header was converted anyway.

**Scan**
- On Linux, a scan missed cue, gdi and m3u entries written in another case or with
  backslashes, and took two files whose names differ only in case for one ROM.
- Scanning a folder ignored the type and platform set on folders inside it.
- Rescans read folder games again, split tracks and lost serial matches. Grouping discs on
  large folders is faster.
- Files that are not ROMs (text, images) now become games only once identified.
- One of crc32, md5 and sha1 must now stay on in the scan hashes. With only xxh3_128, DAT
  matches were lost.

**Copy**
- Overwriting deleted the old file before the new one was written, a copy rewrote playlists
  set to be kept, and two files whose names differ only in case were written to one.

**Title merging**
- Merging joined versions of a version set, or retail titles with unlicensed ones. "Accept
  all" brought back decoupled merges, and IGDB links removed by the audit came back.
- A Hasheous match on a merged game's other name made a duplicate title.

**Server and jobs**
- Fetching art for a client could reach the server's own network. A size limit was added too.
- Downloading the "database" and "all" SQLite caches now needs an admin.
- A full sync cut short left a table half filled, and the next backup started while a
  cancelled one was still writing.
- Copies to another drive and syncs from another server waited for unrelated jobs, and a job
  could start twice after a database restart.

**Other**
- ROM music crashed on a PSF2 set.
- Parts of the web console and the settings screen stayed in English.
- MariaDB older than 10.6 now shows an error at connect.

# romexplorer 0.2.0 (alpha)

## Changes

**Merge review AI**
- "Ask Google" is now an Ask menu. It opens Google's AI Mode, Claude, ChatGPT or Perplexity in
  the browser with the question filled in.
- In-app answers can come from Gemini, Claude, OpenAI or Perplexity, each with its own API key
  and model.
- Answers are now saved. "Ask again" fetches a new one; pinned answers are never refetched.
- Saved answers are included in database backups.

**Config › AI**
- An AI settings tab was added.
- It shows the number of saved answers and the estimated cost this month and in total. Answers
  can be exported and imported as JSON.

**Definitive tab AI**
- Each game's page can ask an AI how its versions differ, to help pick the definitive one.
  Answers get the same ranks as the notes.
- A cost notice is shown before the first question. Config › AI links to each provider's
  usage limit page.
- "Evaluate with AI" asks about several games in one job. It shows the number of questions and
  the estimated cost first, and skips games already answered.
- Games with a new version released after their evaluation are marked "New version since
  evaluation" and can be filtered out.

**Titles: lesser versions and version sets**
- "Hide lesser versions" was added.
- "One from each version set" was added.
- A "Versions" card was added to title pages.

**Config › Advanced**
- A tab was added that gathers actions that can damage the database or lose files. Optimise
  database moved here from Jobs & Data › DB.

**ROM audio**
- Title pages can play the music inside ROMs (CD audio tracks of disc games, and Nintendo DS
  and 3DS sound archives).
- The criteria for which tracks play can be adjusted in settings.
- A "Don't play this track" button was added to the title page player.
- ROM music decoded for playback is cached in memory (Settings › Audio, 256 MiB by default).
- The player's normalisation was tuned.
- The player can crossfade (2000 ms by default, 0 turns it off).
- Silence at the end of tracks is now cut.
- Title page music is now on by default. On first start, after the language, the app asks
  whether to play music. Titles without their own audio play a random track by default.

**Merge review and merged titles**
- A release on a card can be dragged onto the right title. Dropping it on "+" makes a new
  title.
- Accepted and rejected cards can be shown with the state filter. They are hidden by default.
- Splits are also recorded in Edit › Merged titles and can be undone there. Splits made in
  merge review before this update are listed too.

**Combining releases**
- Discs of one game listed as separate releases can be selected and joined with "Combine into
  one release".

**Scan**
- "Rescan this file" was added to file pages. It reads the file again together with the ROM's
  other files and matches the game to its release right away.
- File pages now show the release and title they link to.

## Bug fixes

- Names with a hyphen inside a word did not match IGDB's spelling, so some games found no IGDB
  match.
- SNES and NES IGDB syncs left out games released only in Japan.
- Unlinking IGDB left IGDB's alternative names on the title, with no way to delete them. They
  now go with the link, and each one can be deleted with ✕.
- IGDB links removed by hand were linked again by "Refresh metadata". They now stay unlinked
  until linked again by hand.
- RetroAchievements subsets (challenges, speedruns and so on) joined the game's title and
  could pull different versions into one title.
- Undoing a merge made in merge review did not bring the suggestion back.
- Suggestions for titles already merged by hand stayed in merge review.
- Pirate and unlicensed games were merged into retail titles.
- "Different games mixed" cards left pirate and unlicensed games of other regions in the
  title.
- "Different games mixed" cards took releases with a number before their name ("0743 - …")
  for different games.
- Jobs such as the merge audit now show detailed progress.
- On Windows arm64 and Linux arm64, Wii U, Xbox and Xbox 360 images could not be converted,
  PBP files could not be read and ROM music did not play.
- Files listed in a DAT stayed linked to no release and no title.

# romexplorer 0.1.8 (alpha)

## Changes

**Media**
- Starting over with a new database now reuses the art already in the media folder.

**Jobs**
- The details in the job list were adjusted.
- A "Run next" button was added to queued jobs.

**Hash matching**
- Hash matching with only Hasheous on now also fetches IGDB data and art.
- serial_match can now choose per platform whether to use serials in file names.

**Merge review**
- Gemini's answers and "Ask Google" now use the UI language.
- A failed request now shows why.
- Gemini is now asked even when its model list cannot be read.

**Definitive and cross-platform**
- Only titles with an IGDB id are now shown.

## Bug fixes

**Collection**
- Search in the Definitive tab found only the leading title's name.

# romexplorer 0.1.7 (alpha)

## Changes

**Wii U**
- A copy can make a Cemu .wua from a .wux / .wud and its key.
- Config › Copy can choose whether to copy the .wua or the .wux.

**Xbox / Xbox 360**
- A copy can make a Xenia .zar and a XISO for xemu.

**Serial matching**
- Serial matching for .cia, .xiso, .wua and .zar, which have no hashes in the DATs.
- Serial matching for .vpk, Vita app folders and Switch .nsp.

**PlayStation**
- .cso and PS1 EBOOT.PBP can be scanned.
- A copy can compress PS2 / PSP ISOs to .cso.

**Vita and digital releases**
- Vita games are recognised.
- Packages show their kind (game, update or add-on).
- Switch .nsp, .nsz, .xci and .xcz are recognised.

**RetroAchievements**
- Scans now compute RetroAchievements hashes.

**CHD**
- CHDs are now read with the built-in reader.

**DAT import**
- Files a new DAT drops are now kept as "Replaced files".

**Titles**
- Collection › Titles now shows titles split by release kind.
- Full versions now sort before demos, betas and prototypes.
- release_unlicensed was added.

**Definitive**
- A "Definitive" tab was added to Collection.
- "PC (Definitive tab)" was added to the platform priority.
- Title pages can set a title's relation to its base game.
- Games whose older versions have content the newest lacks get a note.
- Games can be added to a set as definitive. copy_definitive was added.
- A kind column was added to set lists.
- collection_hidden_tabs was added.

**Display language**
- The display language can be picked on first start.
- French, Spanish, Italian, Portuguese, German, Korean and Polish were added.

**DAT sync**
- dat_sync_platforms was added.
- Metadata fetch and Jobs & Data › DAT sync got a platform list.
- DATs that are not games can be picked too.
- "Import downloaded" can pick platforms.

**Catalogue sync**
- Update syncs now fetch only the rows that changed.
- A "Full sync" button was added.

**Scan**
- Hash matching after a scan now uses the sources checked on the scan screen.
- Title merge steps are shown on the scan screen and in the job list.
- A cue or gdi with no tracks beside it is now a warning instead of an error.
- A "Results" button was added to finished jobs.

**Copy**
- Choosing wua, zar or xiso as output now asks for confirmation.
- Files already at the destination are now skipped.

**Media**
- PS1 / PS2 covers from xlenore are supported.

## Bug fixes

**Security**
- The web build at /app/ could read server files without signing in.
- Art fetching could be pointed at the local network.

**Jobs**
- On Linux, jobs failed with "Too many open files".
- On Linux, ZFS pools were taken for HDDs.
- The title merge progress bar went backwards.

**Scan**
- Scans stopped on broken CHD, RVZ, CSO and PBP files.
- Hashing large files ran out of memory, and links to a parent folder were scanned again and
  again.
- Some cue, gdi and m3u files could not be read.
- With only CRC32 kept, rescans lost the folder's DAT matches.
- RetroAchievements hashes were read one file at a time.
- The file name under the progress bar barely updated.
- PSP and PS1 Classics .pkg files were taken for Vita games.
- PSP game and DLC .pbp files were not hashed.

**Titles**
- Hacks, mods, patched versions and demos sorted in the wrong order.

**Copy**
- "Rename" overwrote other files at the destination.
- Files of the same name were lost, and cue/bin zips were split up.
- Cancelled and resumed copies recorded the wrong files.
- RetroAchievements and TheGamesDB ids linked to releases on every platform.
- zip and 7z were copied as they were even with raw output picked.

**Backup and sync**
- A cancelled backup was left empty.
- Moved releases and removed keywords stayed on synced copies.
- An empty key file left by a crash stopped the server from starting.

**Hash matching**
- Matches in several DATs did not follow the release name priority.

**DAT sync**
- Redump and No-Intro DAT syncs failed.
- The Hasheous full download failed on large zips.
- dat_sync_platforms in the web console showed no checkboxes.
- Running several DAT syncs at once failed.

**App and web console**
- Moving set items broke their order, and descriptions were lost.
- Job progress stopped or disappeared too soon.
- Closing a "+ Queue" dialog still queued the next job.
- Some settings were missing from the web console.
- Loading a settings file did not apply until saved.

# romexplorer 0.1.6 (alpha)

## Changes

**Display**
- Display spec data was added to releases.
- A "Collect display data" job was added.
- Resolutions are shown per platform and region.
- NES and SNES screenshots can be compared at 4:3 and 8:7.
- Lists can be sorted and filtered by aspect ratio, resolution, refresh rate and widescreen.
- Dreamcast VGA support is read from the disc.

**Titles**
- Edit › Merge review was added.
- Games released abroad under another colour, name or number are recognised.
- Edit › Aspect ratio review was added.

**Collection**
- "Clear all" was added to multi-select filters.
- "Add to set" was added to set pages.
- Several sets can be copied, downloaded and exported at once.
- Sets can be exported and imported as JSON.
- The right-click "Add to set" shows recently used sets.
- Search results when moving a release are sorted better.

**Jobs**
- The jobs screen is grouped by kind.

**Copy**
- Interrupted copies no longer resume on their own.
- "Continue in background" and "Stop and close" were added to the copy window.

**Media**
- "Download art…" was added.
- "Download missing media" can pick providers and kinds.
- TheGamesDB art and links per region are supported.
- IGDB regional covers are supported.
- Images of the release's own region are now shown first.
- Box fronts and covers are now treated as one image.

**Emulators**
- Art folders can be set (RetroArch, PCSX2, DuckStation).
- "Export art…" was added.
- Export settings were added.
- Emulators that do not open .m3u can be set.

**Big Picture mode**
- A full-screen view for handhelds and TVs was added.
- "Open with another emulator…" was added.
- A setting to start the app in Big Picture was added.

## Bug fixes

**Collection**
- Right-click › Add to set added only one row.
- Adding a RetroAchievements game to a set added releases of every region.

**Titles**
- Accepting a merge suggestion lost one of the IGDB games.
- Hash matching DLC or update folders unlinked IGDB.

**Scan**
- DSi, Wii and Wii U digital folders were recorded as separate files.
- Discs of one game in different subfolders became separate games.

**Copy**
- Three files were copied at once even with 1 / 1 / 1.
- Converting several discs at once could fail.
- Multi-disc games copied only one disc.

**Media**
- Clients did not fetch regional covers.
- "Download art…" finished without doing anything.
- Exporting art on a client failed.

# romexplorer 0.1.5 (alpha)

## Changes

**Titles**
- Search now also matches names in other languages. Edit › Name dictionary was added.
- Titles and releases can be filtered by keyword tags.
- "Change…" was added to each manual match link.
- "Reapply IGDB metadata (all)" was added.
- Titles that are different games on IGDB can be merged.
- "Make separate title" and "Move to another title…" were added.
- Localised title names can be edited.

**Collection**
- "Auto-tag by title" was added to Audio.
- Sets show title, release and platform counts and sizes.
- Copies now take only the highest-priority file per release.
- The preferred format can be picked when a game has several.
- Per-game folders are now copied whole.
- Titles and cross-platform games can be added to sets.
- On-disk size and catalogue size were added.
- Re-releases are now treated as separate versions.
- "Local" and "RA" badges were added.
- The order of sections on title and release pages changed.
- The order of local copies can be changed.
- Filters are now searchable checkboxes.

**Scan**
- About 30 home computers and consoles are supported.
- Folder walking is faster.
- Hash matching shows its progress as counts.
- "Download missing media" now resumes where it stopped.

**Sheets and playlists**
- Sheet and playlist checks can also fix them.
- Sheets and playlists written by a job are now recorded right away.

## Bug fixes

**Jobs**
- Cancelled jobs ran again after a restart.

**Titles**
- Files shared by many discs merged titles into the wrong game.
- Space-separated searches did not match hyphenated names.
- The same game on two platforms was merged into one title.
- Multi-region releases ignored the region priority.
- The base game of a remake or port could be another port.
- Bonus discs merged two games into one title.
- Images of a wrongly linked game stayed after fixing the link.

**Sheets and playlists**
- Running playlist creation again failed.
- Multi-disc games were skipped.

**Scan**
- Multi-disc games matched by DAT stayed "Unknown".
- Some DATs were imported with no platform.

**Copy**
- Games with both headered and headerless files had name clashes.
- Copying CDN DSi titles made file names clash.
- Playlists of multi-disc games had the wrong file names.

**Collection**
- Super Famicom and SNES (Famicom and NES) stayed separate titles.
- WonderSwan Color and Neo Geo Pocket Color games were filed under the wrong platform.
- Picking a folder in the copy dialog crashed the app.
- Removing releases from a set did not update its counts and size.

# romexplorer 0.1.4 (alpha)

## Changes

**Hacks, translations and homebrew**
- Lost Level Archive was added as a DAT source.
- `romexplorer_dbtool make-dats` was added.
- Translated ROMs are now recorded as translations.
- Versions of a hack or translation are grouped into one release.
- "Patched ROMs" was added to title pages.
- Patched releases link to their original game.
- Release filters are now checkboxes.
- "Other" and "Patches" tabs were added to Collection.
- DAT import shows its progress in entries.

**Titles**
- Ports, remakes and remasters link to their base game.

**Jobs**
- "+ Queue" was added.
- Hacks, homebrew and etc. were left out of syncing all DAT sources.
- Provider sync and title merge progress was improved.
- Finished jobs can be removed from the list.
- Automatic title merging now handles only what changed.

**Scan**
- Scans now read one folder at a time, smallest files first.
- System files such as BIOS, firmware and NAND are now recorded.

**Collection**
- Paging and "Select all on this page" were added above lists and grids.
- RetroAchievements games can be added to sets.
- Filters were added to set pages.
- A "BIOS / Firmware" tab was added.

**Settings**
- Settings export and import were added.

## Bug fixes

**Hacks and translations**
- Hash matching a hack linked the original game to the hack's IGDB data.
- Web pages of games and their hacks were mixed up.
- Hacks and translations showed the original game's name.
- Restoring an old backup turned hacks into retail releases.
- Title merging failed with "Access denied".

**Server**
- The server became unusable when its built-in database stopped.

**Jobs**
- Jobs left running could not be cancelled.
- EmuReady syncs failed.
- IGDB sync showed the wrong status.
- Jobs that partly failed started over from the beginning.
- An interrupted full scan started over from the beginning.

**Collection**
- Genre filters matched genres of other sources.
- Japan-only games linked to another IGDB game.
- Changing an IGDB link left the old game's data.
- "Reapply IGDB metadata" did nothing.

**Media**
- Cross-platform grids showed no covers.
- Settings › Storage had the wrong default selection.
- Art whose file was lost was not downloaded again.
- Saving to a media folder the server could not write to failed silently.

# romexplorer 0.1.3 (alpha)

## Changes

**Scan**
- A Scan tab was added.
- The platform is now recorded only when the file's signature confirms it.
- `sheet_style` picks the format of rebuilt cue/gdi sheets.
- Cue sheets with missing tracks are now scanned.
- Hash matching was split into "New" and "All".
- DAT hash matching and title merging now run after a scan.
- Title merging now runs alongside scans.

**Media**
- Where media is stored changed.
- A "Download missing media" job was added.
- Media you added yourself was added to `media_priority`.
- `audio_fallback` was added.

**RetroAchievements**
- A "Download / update RA badges" job was added.
- The achievement filters were split up and adjusted.

**Settings screens**
- Feature descriptions moved to the ! buttons.
- Settings and Settings (JSON) are split into sub-tabs per section.
- Per-platform lists are laid out in columns.
- `collection_default_tab` was added.
- A Cross-platform tab was added.
- The audio_client_format default now depends on the build.
- Translations moved to `assets/i18n/`.
- Settings › Emulators now says launching is untested.
- Provider settings can also be opened from the jobs screen and the settings tab.

**Backups**
- Backup zips can be uploaded.
- "Create DAT seed" was added.

**DAT files** (Data › DAT)
- DAT files can be uploaded and downloaded.
- "Import downloaded" was added.
- Hasheous matching and title merging now run after a DAT import.

**Archive settings**
- Codecs are now a list of only what can be written.
- RVZ, GCZ and WIA output can be set apart.
- CHD custom presets can be set apart for CD and DVD.

**Database cache** (Settings › Server)
- Cache settings were added.
- The cache is now saved on exit and loaded back on start.

**Lists**
- Music CDs, video discs, manuals and the like were left out of title and release lists.
- DLC, add-ons, updates and patches were split out of release lists.
- Column widths can be changed.

**Providers**
- Bulk fetch can pick several platforms.

## Bug fixes

**Hash matching**
- CHD, zip and 7z did not match on Hasheous and PlayMatch.
- Regional-title games got IGDB ids by fuzzy search.

**Scan and copy**
- Full scans failed on newer MariaDB.
- Files showed as "missing" during full scans.
- Scans and copies handled one file at a time.
- Scans read zip, 7z, CHD and RVZ twice.
- Converted copies were not recognised by later scans.
- Files whose content changed kept their old data.
- Making a CHD stopped when tracks were listed before the cue.
- CHD compression used only the first codec.
- RVZ "Remove junk data" had no effect.
- WIA conversion with the default codec failed.
- Codecs 7-Zip can only read are now refused when saving.
- Some CHD preset descriptions were wrong.
- Integrity checks and verified copies did not use the configured hash.

**Collection**
- Releases matched by DAT hash got no "✓ verified" badge.

**Media**
- Images of other releases were not fetched.
- Box backs and screenshots were not fetched as expected.
- Images were saved in the wrong place.
- Each provider now gets its own folder.

**Desktop app**
- Reordering releases did not work.
- Saved settings did not show until the screen was reopened.
- Fast typing in filter boxes dropped letters.
- Dropdowns and text boxes were white on white.

**Lists**
- Resizing columns gave the wrong widths.

**Title merging**
- Unrelated games were grouped under one IGDB game.
- Titles linked by name were not merged.
- Title merge progress was not shown.
- "Merged titles" now shows why titles were merged.

**Server**
- Shutdown did not finish while connections were open.
- The Linux service restarted after a shutdown.
- On Windows, the database did not shut down cleanly.

**Status**
- Status › Providers did not show why a provider was unavailable.

**Settings**
- Saving removed `db_max_connections`.

**Providers**
- Bulk fetch for all platforms ended at 0 with no DATs imported.

# romexplorer 0.1.2 (alpha)

## Changes

**Scheduled jobs**
- A "Scheduled jobs" tab was added (Settings › Schedules was removed).
- "Run only when idle" was added.
- Quick scans can be scheduled.

**Scan**
- The number of files found is shown while folders are walked.

**Collection**
- Platform filters now treat Famicom / NES and the like as one system.

**Settings**
- Metadata source priority now lists only sources that can provide each field.
- The canonical name priority's description was added.

## Bug fixes

**Upgrade**
- The server could not start after an upgrade.

**Console**
- The status screen was slow.

**DAT**
- No-Intro / TOSEC sync progress looked stuck at 50%.
- Redump serials were lost.

# romexplorer 0.1.1 (alpha)

## Changes

**Providers and settings**
- A credentials form was added to Data › Providers.
- Settings › Settings is now a form with every setting.
- Settings › Sources was renamed Settings › Sharing.

**Language**
- Japanese is supported.

**Media**
- Audio files can be scanned, stored and played.
- Images and videos are shown.

## Bug fixes

**Collection**
- Games linked through Hasheous showed as incomplete.
- "Not on server" showed in standalone mode.
- Titles and releases did not show their local games.
- Downloaded covers did not show in the grid.
- CHD contents were shown wrong.
- Hash matching skipped files in subfolders.

**App**
- Back / forward buttons were added to the desktop app.
- An "About" tab was added.
- List columns can be widened.
- Notifications stayed on screen.
- Backup progress went past 100%.
- "Sync all sources" showed the wrong progress.
- Settings › Settings sometimes would not open.

# romexplorer 0.1.0 (alpha)

## Packages

| platform | server | desktop | archive |
|---|---|---|---|
| windows/x64 | ✓ | ✓ | zip |
| windows/arm64 | ✓ | ✓ | zip |
| macos/arm64 | ✓ | ✓ (+ `.app`, ad-hoc signed) | zip |
| linux/x64 | ✓ | — | tar.gz |
| linux/arm64 | ✓ | — | tar.gz |

`server` bundles MariaDB and the web GUI; `desktop` is the GUI app with its own MariaDB.

## Features

**ROM handling**
- Platform detection from file content.
- CHD extract and create.
- RVZ extract and repack; RVZ, WIA and GCZ from ISO.
- zip and 7z.
- CRC32, MD5, SHA-1, SHA-256, XXH3 and RetroAchievements hashes.
- CHD and RVZ repack, and cue to CHD.
- NES and SNES header add/strip.
- Cue/GDI sheet checks and fixes.
- Copy to a device.

**Catalogue**
- Bundled MariaDB.
- Redump, MAME-Redump, RetroAchievements, RetroBIOS, No-Intro and TOSEC DATs.
- Hash and name lookup.
- Release and title merging.
- Hasheous and EmuReady metadata.
- IGDB, TheGamesDB, SteamGridDB, MobyGames, ScreenScraper and RetroAchievements metadata and
  media.
- File scan and bulk provider sync jobs.

**Server and apps**
- REST API.
- TLS.
- Response compression.
- Web GUI at `/app/`.
- Desktop app.
- Service installer, `install.sh` / `install.ps1`.
- Backups.
