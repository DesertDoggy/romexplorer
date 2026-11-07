# romexplorer 0.2.0 (alpha)

Alpha: the database schema may change between releases with no upgrade path. Back up before
replacing a build.

## Changes

**AI in Merge review**
- "Ask Google" is now an Ask menu: Google AI Mode, Claude, ChatGPT and Perplexity open in the
  browser with the question filled in.
- The answer in the app can come from Gemini, Claude, OpenAI or Perplexity, each with its own
  API key and model.
- Answers are saved. Ask again fetches a new one; pinned answers are not fetched again.
- Saved answers are in the database backup.

**Config › AI**
- A new tab for the AI settings.
- Shows how many answers are saved and their approximate cost this month and in all. Answers
  can be exported and imported as JSON.

**AI on the Definitive tab**
- Each game's page can ask the AI which version has the most content and the best graphics, sound
  and quality of life. The answer comes with a rank like the reviewed notes.
- An answer from a browser link can be pasted in, or saved from the clipboard in one click.
- The first question shows a cost warning. Config › AI links to each provider's spending limits.
- "Batch rate with AI" asks about several games as one job: the selected games on the Definitive
  tab, or a set's selected rows or the whole set. It shows the number of questions and an
  approximate cost first, skips games already answered, and can be scheduled for a set.
- A rank made before a newer version of the game came out is marked "New version since rating",
  and the filter can skip such ranks.

**Titles: minor versions and version sets**
- "Hide minor versions" leaves out a game where a fuller edition covers it on the same platform
  and region (International, Director's Cut, Final Mix and the like; an Easy Type loses to the
  regular game). On Titles, Releases and a title's releases.
- "One of each version group" lists one version of games sold side by side (Pokémon, Yo-kai Watch
  and others): the one you hold, else your pick, else a random one, with 🎲 to show another.
  Versions of a set no longer share a family on the Definitive tab.
- A title page's new Versions card shows both, and lets you correct them.

**Config › Advanced**
- A new tab for operations that can damage the database or lose files. DB optimize moved here
  from Jobs and Data › DB.

**ROM audio**
- Title pages can play the music inside your ROMs (supported platforms and media types only).
- NES and Famicom music plays straight from the ROM, through the game's own sound code: no
  original audio files needed. Famicom sound chips (VRC6, Namco 163, Sunsoft 5B, MMC5) included.
- Game Boy, Game Boy Color, Master System, Game Gear, Mega Drive and PC Engine music plays straight from the
  ROM the same way, the Japanese Master System's FM sound included.
- Settings adjust which tracks play.
- Original audio formats (NSF, SPC, GBS, VGM, HES, KSS, AY, SAP and Game Gear SGC,
  loose or zipped) are supported.
- An original audio library: list the folders where you keep these files in Settings › Audio, and Index ROM
  audio matches each set to your ROMs by their content, so one set plays for every region of
  the game whatever its name.
- The player on title pages has a "don't play this track" button. Hidden tracks are listed under
  the playlist, where a click plays one and Restore brings it back.
- ROM music decoded for playing is cached in memory (Settings › Audio, 256 MiB by default).
  When that is full, tracks move to the audio cache on disk, which uses the tier 1 cache
  folder when one is set.
- Normalise in the player has been tuned.
- The player can crossfade (2000 ms by default, 0 for off).
- Silence at the end of a track is cut: ROM tracks are trimmed, and for audio files the audio
  scan measures where the sound ends (run it again to measure files scanned before).
- Music on title pages is on by default, and the first start asks whether you want it, right
  after the language. A title with no audio of its own plays random tracks by default.
- PSF and PSF2 files use a PS2 BIOS from your collection and are skipped without one.

**Merge review and combined titles**
- Releases on a card can be dragged to the title they belong to, or onto "+" for a new title
  named after the first one. Tapping a release and then a title works too.
- Cards already taken or turned down can be shown again with the new state filter. They are
  hidden by default.
- Splits are now listed under Edit › Combined titles and can be undone there. Splits made
  from Merge review before this update are added to the list.

## Bug fixes

- Pirate and unlicensed games were merged into retail games' titles.
- The "another game inside" card left pirate and unlicensed games from other regions in the
  title.
- The "another game inside" card treated a release with a number in front of its name
  ("0743 - ...") as another game.
- Jobs like the merge audit showed little of their progress. They now show it in detail.
- The Windows arm64 and Linux arm64 builds could not convert Wii U, Xbox and Xbox 360 images,
  read PBP files or play ROM music.

# romexplorer 0.1.8 (alpha)

Alpha: the database schema may change between releases with no upgrade path. Back up before
replacing a build.

## Changes

**Media**
- After starting over with a new database, the art already in the media folder is used
  instead of downloading it again.

**Jobs**
- The jobs list's detail columns were adjusted.
- A waiting job has a Run next button, also for a selection: it starts as soon as the job
  running in its lane is done. Other lanes are not affected.

**Hash match**
- A hash match with only Hasheous on also fetches IGDB data and art for what it matched, as
  with PlayMatch.
- Settings › serial_match sets per platform whether a serial in the file name counts. A
  platform left as it is follows the file name box.

**Merge review**
- The Gemini answer and Ask Google come back in the UI language.
- When the lookup fails, the message says why: a certificate or a timeout, the model, and
  the quota that ran out. A key with no free quota for the model says so.
- If the model list can't be read, Gemini is still asked; on a quota error the next Flash
  model is tried once.

**Definitive and Crossplat**
- Only titles with an IGDB id are listed. Wii U titles named by their title id no longer show
  in Definitive.

## Bug fixes

**Collection**
- The Definitive tab's search found only the leading title's own name. It now finds a family
  by any version's name, including localized and alternative names.

# romexplorer 0.1.7 (alpha)

Alpha: the database schema may change between 0.1.x releases with no upgrade path. Back up
before replacing a build.

## Changes

**Wii U**
- A copy can turn a .wux or .wud and its key into Cemu's .wua. A scan matches that .wua to
  the same DAT entry as the disc it came from.
- When a release is held both as .wua and as .wux, Config › Copy sets which one a copy takes
  (.wua unless changed).

**Xbox and Xbox 360**
- A copy can turn an Xbox 360 image into Xenia's .zar, and an Xbox image into an XISO for
  xemu. A scan matches a .zar to the same DAT entry as the disc it came from.

**Serial matching**
- Hash match can link a .cia, .xiso, .wua or .zar by the serial in its header, since no DAT
  has hashes for them. Such a link is marked "serial match". Settings › serial_match picks
  the formats, and whether the serial in a copy's manifest or in the file name counts too.
- Serial matching also covers .vpk, Vita app folders and Switch .nsp (on by default), and
  .pkg, .psv and PS1/PS2/PS3 discs (off by default, since DATs hash them).

**PlayStation**
- Scans read .cso files and PS1 EBOOT.PBP files, and match them by the ISO, or the cue and
  tracks, inside.
- A copy can compress a PS2 or PSP ISO into a .cso.

**Vita and digital downloads**
- Scans recognize Vita .pkg, .vpk, .psv, game-card images and app folders, and read their
  title id. A PS3 or PSP game folder with subfolders is one game.
- Packages show whether they hold a game, an update or an add-on. Settings › scan_pkg_titles
  also reads the title inside PS3 and PSP .pkg files (off by default).
- Switch .nsp, .nsz, .xci and .xcz are recognized. A scan hashes each NCA in an .nsp, so it
  matches No-Intro's digital DATs.

**RetroAchievements**
- Scans work out the RetroAchievements hash for systems where it isn't the file's MD5 (NES,
  SNES, N64, PC Engine, DS and disc systems), and hash match links on it. Zipped cartridges
  and discs in CHD, RVZ, CSO or PBP are read in memory, with nothing extracted.

**CHD**
- CHD headers and track lists are read by a built-in reader, so a scan no longer waits
  behind a running CHD conversion. By default it is checked against chdman, and any
  difference is listed in the scan's result to report. Settings › chd_reader can switch to
  chdman only, or to the built-in reader alone (not recommended yet).

**DAT import**
- When a newer DAT changes or drops a file, the old one stays on the release page as
  superseded instead of being deleted, and a DAT that lists it again puts it back. A DAT cut
  short, or one that would take off more than 2% of its files at once, takes nothing off.

**Titles**
- Collection › Titles lists a title once per kind of release: retail, demos, betas,
  prototypes, unlicensed and aftermarket each get a row, and every hack, mod and pirate copy
  its own. Translations share one row, badged with their languages.
- Adding a row to a set or copying it takes only that kind, so a set never fills a missing
  retail release with a demo. The title page still shows every release.
- Within a title, every full version comes before demos, betas and prototypes.
- A new setting, release_unlicensed, lists unlicensed releases with retail instead of on
  their own row.

**Definitive versions**
- A new Collection › Definitive tab lists each game once, with its ports, remakes, remasters,
  editions and the collections that hold it. Each row leads with the newest version, or with
  the base game when toggled, and a two-cover grid shows both covers.
- The tab picks from the versions in your catalogue. Versions no DAT lists, such as PC, PS5
  or Switch 2 releases, show up like games you don't hold once their platform's IGDB data is
  synced. A new setting, definitive_fetch_versions, can instead fetch the other versions of
  each game held on disk (off by default).
- Platform priority gains a "PC (Definitive tab)" entry for recent PC versions. Drag a game's
  versions into your own order on its page.
- A title page can set its base game and what it is of it (port, remaster, edition, ...),
  and lists the game's other versions and collections.
- Some games carry a note when an older version has content the latest lacks, with a filter
  by its rank.
- A game on the Definitive tab can be added to a set as its definitive version. The setting
  copy_definitive picks what it copies: the best version you hold, or the definitive version
  only (nothing when you don't hold it).
- A set's list has a type column, D (definitive), C (cross-platform), T (title) and R
  (release), and its grid can show the same letter on each cover.
- A new setting, collection_hidden_tabs, turns off Collection sub-tabs you don't use. Titles
  and Releases always stay.

**Display language**
- The first start asks which language to show the UI in, and a language menu at the top
  right of the app and the web console switches it any time.
- The UI is also in French, Spanish, Italian, Portuguese, German, Korean and Polish.

**DAT sync**
- A new setting, dat_sync_platforms, limits DAT sync to the platforms your scans found or
  ones you tick. Redump and MAME Redump skip the other platforms' downloads; No-Intro and
  TOSEC import only those platforms from their packs.
- The platform checkboxes of a metadata request start from those platforms, show each name
  above its box, and have check all, clear all and buttons for a maker or kind of machine.
- The DAT sync forms in Jobs and Data › DATs show the same platform checkboxes, ticked from
  the setting, so one run can leave platforms out.
- DATs that aren't a platform's games (audio CDs, video discs, manuals and scans, source
  code, avatars, themes and wallpapers) and DATs of an unknown platform have checkboxes of their own. Outside "all", only
  what is ticked is imported.
- "Import downloaded" asks whether to import the whole zip or pick platforms first.

**Catalogue sync**
- A client's update sync downloads only the rows that changed on the main server since its
  last sync, instead of every table that had any change. Scan and integrity check times on
  their own don't count as a change.
- Sources has a "full sync" button that downloads everything again, for a client whose copy
  has gone wrong.

**Scan**
- The hash match that follows a scan, quick scan or rehash uses the DAT, Hasheous and
  PlayMatch boxes ticked on the Scan screen, and the boxes are remembered.
- The title merge pass shows each of its seven steps on the Scan screen's Running now: done
  (with what it did and how long it took), running with its own bar, or still to come. The
  Jobs list shows the steps as marks, and a running scan can be cancelled from the Scan screen
  in the app too.

**Copy**
- Choosing wua, zar or xiso as an output asks first, as the disc image can't be got back from
  them. "Don't ask again" turns the question off; Config › Copy turns it back on.
- A file already at the destination is skipped by default. Pressing Copy lists them and asks
  whether to skip, rename or overwrite, for each file or for all.

**Media**
- PS1 and PS2 covers can come from xlenore's psx-covers and ps2-covers, looked up by each
  release's serial, so budget reissues and limited editions get their own cover. Box fronts
  download by default; 3D boxes when ticked in media_download. media_priority sets where it
  ranks against IGDB and the others.

**Scan**
- A cue or gdi with none of its tracks beside it (kept elsewhere as a CHD) is a warning, not
  an error, and no longer fails the scan.
- A finished job has a "result" button in Jobs and Scan, listing a scan's errors and warnings.

## Bug fixes

**Security**
- The web build at /app/ let anyone on the network read any file the server could read,
  without signing in.
- Art downloads could be sent to machines on the local network.

**Jobs**
- On Linux, several jobs at once could fail with "Too many open files". The server now raises
  its open-files limit at start.
- On Linux, a ZFS pool was taken for a hard disk, so scans and copies on NVMe or SSD pools
  read one large file at a time. scan_parallel_files and copy_parallel_files can also set
  the drive type by hand.
- The title merge pass's progress bar jumped back during its fourth step.

**Scan**
- A corrupt CHD, RVZ, CSO or PBP could hang a scan, and a cancelled scan kept reading.
- A large file could fill memory while it was hashed, and a folder link pointing back up was
  scanned again and again.
- Cues in Shift-JIS or with a BOM, gdi files with lined-up columns and m3u entries like
  `./Disc 1.chd` were not read.
- With only CRC32 kept, a rescan dropped the folder's DAT matches. An unreadable file was
  marked deleted, and a hash a DAT corrected was ignored.
- A scan with RetroAchievements hashes on read N64, SNES, NES, PC Engine, DS and disc
  images one at a time, whatever the parallel setting said.
- During a scan of many small files, the files being read under the progress bar barely
  updated. The progress line was also cut short when a file name was long.
- A PSP or PS1 classic .pkg was taken for a Vita game.
- A PSP game's or DLC's .pbp was left unhashed.

**Titles**
- Hacks, mods and patched releases were mixed in with a title's retail releases, and a demo
  could rank ahead of another region's retail release. Names with several tags kept only one.

**Copy**
- Rename, the default, could overwrite a different file already at the destination, and
  converting into the source folder could delete the source.
- Files with the same name in different folders of one game were lost, and zipping a cue and
  bin made one zip per file.
- A cancelled copy recorded nothing it had copied, and a resumed copy trusted any file of the
  right size.
- Linking a RetroAchievements or TheGamesDB id to a title linked every platform's release.
- The raw format (unpack) copied a zip or 7z as it was. It now unpacks it into its files.

**Backups and sync**
- A cancelled backup left an empty one that could push a good backup out, and a failed
  mobile cache export stayed listed.
- A release moved to another title, or a removed keyword, stayed on synced clients.
- An empty key file left by a crash stopped the server from starting.

**Hash match**
- A file matching the same game in several DATs (TOSEC and No-Intro, say) was linked to
  whichever DAT was imported first. It now follows the release name priority.

**DAT sync**
- Redump DAT sync failed after Redump changed its download links, and No-Intro DAT sync
  failed after its site added a terms page to accept and renamed its download buttons. When No-Intro's site is down for
  maintenance, throttling requests or has blocked your address, the sync now says so. A
  slow No-Intro page is waited for and asked again, and any other page is quoted in the error.
- A Hasheous full download failed on its largest zips (Commodore 64, DOS, Amiga) unless
  the connection was fast enough to finish each in two minutes.
- In the web console's Settings, dat_sync_platforms showed only the filter box, without the
  platform checkboxes. The Nintendo and Sony buttons also missed Super Famicom, Pokémon mini,
  PocketStation and a few others.
- Several DAT syncs running at once could fail with a database error, and one DAT that
  failed stopped the rest of a TOSEC or No-Intro pack.

**App and web console**
- Moving a set entry up or down on a filtered or paged list reshuffled the whole set, and
  cancelling the description prompt when renaming a set cleared it.
- Job progress in dialogs stopped after one failed check, and the console hid a job's result
  as soon as it ended.
- Closing a dialog opened from "+ queue" left the next job you started queued. A slow
  console screen could draw over the one you had moved to, and big lists in the app
  redrew slowly.
- The web console had no source mounts, no username and password sign-in for sources, no
  artwork row in the copy dialog and no TLS certificate row.
- Settings brought in by a configuration import, such as the UI language, showed only after
  pressing Save in Settings.

# romexplorer 0.1.6 (alpha)

Alpha: the database schema may change between 0.1.x releases with no upgrade path. Back up
before replacing a build.

## Changes

**Display**
- Releases record how display settings.
- A "collect display data" job reads data from file or database.
- Resolutions are listed exactly, with progressive or interlaced (256x224p, 512x448i), per
  system and region, including handheld screens and TV-out.
- NES and SNES pages show a screenshot at 4:3 and 8:7 side by side to pick from; PC Engine,
  Mega Drive, Saturn and Neo Geo compare against square pixels.
- Lists can be sorted and filtered by aspect, resolution, refresh rate and widescreen.
- The job reads the VGA flag of Dreamcast discs.
- With no screenshot of a game yet, the aspect picker shows its box as a sample only, with a
  warning that a box cannot tell the aspect.

**Titles**
- Edit › Merge review goes through merge suggestions one at a time, regional ones first: an
  International or Director's Cut beside its Japanese original and the foreign versions, a
  Japanese release filed under another game, names that translate each other, and a bundle
  holding single games, each moved into its game's title with one click. For an International or Director's Cut it shows what differs, from Gemini
  when a key is set (Data › Providers), and has an Ask Google button.
- Merge review knows games sold abroad under another colour, name or number, and Japanese
  releases never sold abroad, which it suggests giving a title of their own.
- Edit › Aspect review goes through the 4:3 / 8:7 question one title at a time, titles with
  nothing chosen first. Both reviews filter like the Titles list, a set included.

**Collection**
- Filters with several choices (platform, genre, kinds, ...) have "clear all", and a chosen
  entry comes off with a right-click.
- A set's page has "add to set": the selected rows go to another set, a title or game entry
  staying a title or game.
- Sets can be ticked in the Sets list to copy, download art for or export several at once,
  from the bar above or by right-click. Selected titles and releases, also inside a set, can
  be exported too.
- Sets can be saved to a JSON file and read back, all or the ticked ones. Each entry is found
  again by name, content or IGDB game, so sets survive a server started clean.
- With four sets or more, the right-click "Add to set" lists the three last added to, and
  folds every set under "all sets".
- Moving releases to another title lists the best matches first (the exact name, then names
  starting with it), a page at a time, and can show only titles on the releases' platform.

**Jobs**
- The Jobs tab has a box per kind of job: DAT sync, media download, RetroAchievements data,
  metadata request, integrity check, cue sheet and playlist check, display data and
  database maintenance.

**Copy**
- A copy cut off by closing the app no longer starts again by itself on the next launch. It
  shows as cancelled, and Jobs › retry starts it again.
- Once a copy is running, its window offers "continue in background" or "abort and close".

**Media**
- Download art… fetches one provider's art, of the types you tick, for the titles, releases
  or sets you pick.
- Jobs › download missing media can be limited to one provider and chosen types.
- TheGamesDB art comes per regional release, so a Japanese release gets the Japanese box.
  It is fetched only through Download art…, for its small allowance, which links what you
  pick from TheGamesDB's free game list first: no platform sync needed.
- Releases link to TheGamesDB's entry for their own region, not any entry of the same name.
  A TheGamesDB sync relinks the ones already linked.
- A release shows its own region's box before art made for every region.
- IGDB's regional covers are downloaded too, so a Japanese release gets IGDB's Japanese box.
- A release's media list and the RetroAchievements grid put the release's own region art
  before art for every region, as its cover already did.
- A box front and a cover count as the same picture: the grid, a release's picture, its media
  list and an export take whichever comes first in the provider order. SteamGridDB's covers
  never count as a region's art.

**Emulators**
- An emulator can have an artwork folder: RetroArch's thumbnails, or a PCSX2 / DuckStation
  covers folder. Pictures in other formats are converted.
- Export artwork… writes the art for titles, releases or sets into any folder, in RetroArch's
  layout or as covers named by serial or by file name; an emulator's artwork folder fills it in. The copy window can
  export it for what it copies. Files romexplorer did not write are never replaced.
- Settings › Media has the provider order for exported art. A release's own region art comes
  first, as on its page.
- An emulator can be set not to open .m3u playlists; a multi-disc game then starts from its
  first disc.

**Big Picture mode**
- A full-screen view for handhelds and TVs, from the header's Big Picture button: big covers,
  played with a controller, touch or a mouse.
- Covers sit in one column in portrait and one row in landscape, with the selected game's
  details beside them, or in a grid. Filters sit behind one Filter button.
- A cross-platform game, a title or a release opens on a page with its other releases and
  patched releases; any of them can be started.
- Start uses the platform's default emulator. Open with… picks another one, starts a single
  disc, or makes another emulator the default.
- It can be set to open when the app starts.

## Bug fixes

**Collection**
- On a set's page, right-click › Add to set added only one of the selected rows.
- Adding RetroAchievements games to a set added every region of each game you hold. Sets
  made before need making again.

**Titles**
- Merging a suggestion of two titles with different IGDB games lost one of the games.
- Hash match (all) on a folder holding only a game's DLC or updates took the title's IGDB
  link away for good, so its releases got no IGDB art. Such titles are looked up on IGDB
  again once after updating, and opening their page looks them up too.

**Scan**
- A DSi, Wii or Wii U digital (CDN) game folder was scanned as separate files, so a copy took
  only one and the title showed a file missing. Scan the folder again to fix ones already
  scanned.
- A game's discs stayed separate games when the scanned folder also held the same game in
  another subfolder or format, so each disc showed as incomplete. Discs that sit together are
  now one game; the next scan or hash match puts existing ones right.

**Copy**
- A copy ran small, medium and large files side by side, so 1 / 1 / 1 copied three files at
  once. It now does one size at a time, smallest first, as a scan does.
- Converting the discs of one game at once could fail.
- A copy of a game whose discs were scanned as separate games, as most PS2 games are, took
  only one disc.

**Media**
- A client didn't take a regional box from the server when the release already had that
  provider's common one, so it kept showing the common box.
- Download art… finished as "done" with nothing downloaded when the provider had no key or
  none of the picked releases was linked to it. It now says which.
- On a client, Export artwork… failed for some art with "avif can't be turned into jpg", and
  left out art it only streams, such as screenshots. A client now always exports the main
  server's original picture, fetched when needed rather than stored.


# romexplorer 0.1.5 (alpha)

Alpha: the database schema may change between 0.1.x releases with no upgrade path. Back up
before replacing a build.

## Changes

**Titles**
- Searches also find a name in the other language: a Japanese search finds the English title
  and the other way round, on Titles, Releases, Games, Cross-platform and Audio. The names come
  from the catalogue and Wikidata; see and edit them under Edit › Name dictionary.
- Titles and Releases filter by your keyword tags, apart from IGDB keywords. Adding or removing
  a tag searches the existing tags as you type, as on Audio.
- Manual match has "change…" beside each link, so a link found by hash match can be replaced.
- "Re-apply IGDB metadata (all)" on Titles re-applies every IGDB-linked title, also those that
  already have localized names.
- Titles that IGDB lists as different games (a game and its collection) can be combined and
  keep both: each release keeps its own IGDB page and art, and the title takes its game and art
  from the release first in region priority.
- A release page has "split into its own title" and "move to another title…", for a release
  listed under the wrong game; with several releases selected, they split into one new title
  together or move at once. A split-off title is not combined back automatically.
- A title's localized names can be edited ("edit…" on the title page). Your names are kept
  when IGDB metadata is re-applied, for one title or all, until you unlock them.

**Collection**
- Audio has "auto-tag by title": albums get the tag of the title they are named after,
  translated names included. Automatic tags are kept apart from yours and can be filtered and
  removed on their own.
- Sets show how many titles, releases and platforms they have, their size on disk and their
  catalogue size, on the set's page and in the list of sets. With a main server, sets also
  show their size on the server: what a copy of the set will take.
- A copy takes one copy of each release and one file per name, from the folder first in
  directory priority; a client follows the server's directory order. Under Config › Copy you
  can prefer headered or headerless NES dumps instead.
- A game held in several formats in one copy (.fds and .qd, .z64 and .v64, .sfc and .smc, ...)
  is copied once, in the format first under Config › Copy. Decrypted dumps come before
  encrypted ones by default.
- Games kept in a folder of their own (DSi, Wii and 3DS digital titles, PSP and Vita DLC,
  computer games in a folder per disk) are copied with that folder.
- A set can hold titles, and games from Cross-platform on any platform, as well as releases.
  They use the best release you have whenever the set is opened or copied.
- Releases, titles and sets have a size on disk (the files you hold) and a catalogue size (the
  raw dump the DATs list, without CHD or RVZ), both sortable. A selection shows its total.
- Virtual Console and other re-releases are an edition of their
  own. Which edition comes first is now an ordered list under Config › Settings, with the
  original before re-releases by default.
- Lists and grids show bold LOCAL and RA badges. RA is only on releases RetroAchievements
  supports.
- Title and release pages show the releases or local copies right after the metadata, then
  RetroAchievements, then the rest.
- A release's local copies list complete ones first, then by directory priority (▲▼ under
  Config › Directories). ▲▼ on a copy sets your own order, and copy jobs take the first one.
- Platform, genre, franchise, company and keyword filters are searchable tick boxes, matching
  any (OR) or all (AND). Platforms match any, and on Cross-platform also all.

**Scan**
- Commodore 64, Amiga, ZX Spectrum, Apple II, Amstrad CPC, BBC Micro, Intellivision,
  ColecoVision, Vectrex and about twenty more home computers and consoles are platforms of
  their own. DATs imported before, when these had no platform, move onto them at the next
  DAT refresh.
- Walking the folders is faster, most of all over a network share: each folder is read
  once, and file types come from the folder listing.
- Hash match shows a count for every step instead of standing still between them, and writes
  disc numbers in bulk.
- A "download missing media" job started again (after a restart, or retried) goes on where it
  stopped instead of checking every game it already did.

**Disc sheets and playlists**
- Checking sheets and playlists can also fix them: with "also fix" ticked, one that works but
  doesn't match the DAT is rewritten to match it.
- Sheets and playlists the create job writes are recorded right away, by a quick scan of their
  folders, so a playlist is used to launch its game at once.

## Bug fixes

**Jobs**
- A job cancelled just before the server stopped ran again after the restart. Clearing
  selected jobs now also removes ones that never started.

**Titles**
- A title could be linked to another game through a file many discs share (a stock audio
  track), and merged with that game. Such titles are split and relinked once after updating.
- A search typed with a space missed names a DAT writes with a hyphen ("Hoge Quest" against
  "Hoge-Quest"), and the other way round.
- The same game on two platforms (PS2 and PSP, for example) could end up as one title. Such
  titles are split again once after updating; Cross-platform is where they are one row.
- A release for two regions, such as (Japan, Korea), showed either region's localized name,
  ignoring region priority.
- A remake or port could name a port of its base game as the base game instead of the
  original (the WonderSwan Color version rather than the Super Famicom one, for example).
- A bonus disc with a demo of another game linked the title it came with to that game, and the
  merge pass joined the two games into one title. The demo now keeps its link to itself, and
  titles merged this way are split again by a merge audit that runs once after updating.
- Art from a game a title was wrongly linked to stayed after the link was changed or removed.
  It is now taken off, and the right game's art is downloaded, reusing files already here.

**Disc sheets and playlists**
- Creating playlists again reported every playlist written by an earlier run as an error. One
  that is already there is now skipped.
- A multi-disc game was skipped when a copy of one of its discs sat in another folder (the same
  disk in two machines' TOSEC sets).

**Scan**
- Multi-disc games matched by a DAT stayed under Unknown.
- Some TOSEC and No-Intro DATs (MSX, TOSEC's NES set, "Unofficial" and "Non-Redump" sets)
  were imported with no platform.

**Copy**
- Previewing a copy of a set reported name collisions for games held both headered and
  headerless.
- DSi titles from the CDN lost their folder in a copy, so their files collided.
- A copy that converted a multi-disc game to CHD left the playlist naming the old files
  for every disc but the first, and a plain copy could point a playlist line at a disc's .bin
  instead of its .cue.

**Collection**
- A game stayed two titles when one DAT filed it under the Super Famicom (or Famicom) and
  another under the SNES (or NES).
- WonderSwan Color and Neo Geo Pocket Color games from the RetroAchievements list were filed
  under the WonderSwan and the Neo Geo Pocket.
- The desktop app crashed when a folder was picked with "in page" (or after the folder dialog
  failed) in the copy dialog. The folder browser now opens over the dialog.
- A set's page kept its old counts and sizes after releases were removed from it. Set sizes
  are now shown in GiB, so small changes show.


# romexplorer 0.1.4 (alpha)

Alpha: the database schema may change between 0.1.x releases with no upgrade path. Back up
before replacing a build.

## Changes

**Hacks, translations and homebrew**
- New DAT sources: Lost Level Archive, downloaded like the others.
- `romexplorer_dbtool make-dats <folder> --out <folder>` turns a folder of community DATs into
  DATs for those sources: hacks, mods and translations; homebrew; map packs, DLC installers and
  tools; trainers; saves; source code.
- A translation is listed as a translation, with the original's region and the language it was
  translated into. A language the name does not state is marked as a guess, and the release
  page has confirm, change and unlink for it.
- Every version of one hack or translation is one release. The release page lists the versions
  and marks the one you hold; holding any one makes the release complete.
- A title page lists hacks and translations under Patched ROMs, apart from the game's own
  releases, with sections of their own for patches, trainers, saves and map packs.
- A patched release is linked to the game and the release it was made from, and the original's
  page lists what was made from it. A link made from a name alone is marked suggested: confirm,
  change or unlink it on the release page, or many at once under "patch links" on Titles.
- The releases filter on Titles and Releases is a pop-up of tick boxes (commercial,
  translations, hacks, mods, homebrew, arcade ports, WiiWare, RetroAchievements subsets,
  guessed language): tick what to show, and anything matching a ticked box is listed.
- New Collection tabs: Etc (map packs, tools, trainers, saves) and Patches (RetroAchievements
  patches with their download button, patch files from DATs, and your own files in a patch
  directory). `collection_default_tab` can open on either.
- A DAT import shows how many entries are done, also inside one large DAT.

**Titles**
- A port, remake or remaster is linked to its base game whichever of the two was added first,
  and the base game's page lists them. Both rows show platforms.

**Jobs**
- Every job button has "+ queue" beside it. A queued job starts when nothing else is running
  or waiting, so a list of them runs one after another instead of all at once.
- A DAT sync of all sources leaves out hacks, homebrew and etc. Sync or import those from
  their own rows.
- A provider sync shows games done of the total, and the title merge pass shows its step.
- Finished jobs can be cleared from the list: all of them, by status, or the selected rows.
- The title merge pass looks only at what changed since the last one. "Full merge pass" on
  Titles still goes over everything.

**Scan**
- A scan finishes one folder at a time, and in it small files, then medium, then large, each
  at its own limit. Before, all three sizes ran at once.
- BIOS, firmware, NAND and other system files a scan identifies are recorded too.

**Collection**
- Lists and grids have the pager above them as well as below, with a "select all on this page"
  box beside each.
- Games on the RetroAchievements tab can be added to a set, with the checkboxes or a right-click.
- A set's page has the Releases tab's filters (platform, region, language, genre, ...), and the
  selected releases can be removed from the set at once.
- New BIOS / firmware tab: BIOS, firmware, DSP ROMs and NAND dumps with their platform, from
  DATs and from your scans. They are no longer listed among the titles.

**Config**
- Config › Backup exports the configuration as one JSON file and imports it: settings,
  scheduled jobs, emulators, copy folder names and formats, directories and storage folders.
  Provider credentials and accounts are not in it.

## Bug fixes

**Hacks and translations**
- A hash match on a hack linked the original game's title to the hack's own IGDB entry, so the
  game was shown as a mod with the hack's makers. The link now goes on the hack's release, and
  the title merge pass puts affected titles right.
- A game's page listed the web pages of every hack under it, and a hack's page the game's.
  Each page now lists its own.
- A hack or translation was listed under the original's localized name instead of its own.
- Hacks in a catalogue restored from an older backup counted as commercial releases. The title
  merge pass now marks them again.
- The title merge pass failed with "Access denied" when the database user could not create
  temporary tables.

**Server**
- When the server's own database stopped unexpectedly, the server stayed unusable (signing
  in failed and the UI fell back to English) until it was restarted. It now starts the
  database again by itself and logs why it stopped.

**Jobs**
- A job left running after the server or its database stopped could not be cancelled, and
  started again by itself two hours later. Such a job is now cancelled at once, or queued
  again if no cancel was asked for.
- An EmuReady sync failed on every platform EmuReady does not have. Only its 19 platforms are
  offered now, and a sync of every platform skips the rest.
- An IGDB sync said "screenshots and fan art" while both were switched off and nothing was
  being downloaded.

- A scan, DAT sync or media relayout that finished with a few failed items was run again from
  the start, up to three times. It now ends once, as failed, with what went wrong; a scan is
  still followed by its hash match.
- A full scan interrupted by a server restart started over from the first file. It now goes on
  from where it was.

**Collection**
- The RetroAchievements tab's genre filter also matched genres of the linked catalogue title.
  It now uses RetroAchievements' own genre, the one the list shows.
- The genre filters of Titles, Releases and Games matched a genre from any source. A title is
  now filtered by the first source in the genre priority (Config › Settings) that gave it one.
- A Japan-only Super Famicom or Famicom game could be linked to another IGDB game sharing a
  word with it. Such links are no longer made, and the merge audit removes the ones already
  there.
- A title whose IGDB link moved to another game kept the first game's localized names, genres
  and companies beside the new ones.
- "Re-apply IGDB metadata" did nothing for titles that already had localized names ("0 titles").
  It is now also on the selection and the right-click menu in Titles, and re-applies those titles.

**Media**
- The Cross-platform grid showed no cover art.
- Config › Storage started on a cache tier, so an image dir was easily added as a cache. It
  starts on image now and shows where new downloads go.
- Art whose file had gone (a restored database, an emptied folder) counted as held and was
  never downloaded again. A sync puts it back, and so does a view served from a cache copy.
- Covers silently failed to save into a media dir the server could not write to. Such a dir
  is now refused when added and marked "not writable" in Config › Storage, and the job says
  how many covers failed and why.


# romexplorer 0.1.3 (alpha)

Alpha: the database schema may change between 0.1.x releases with no upgrade path. Back up
before replacing a build.

## Changes

**Scan**
- A new Scan tab shows only scans and hash matches, with each file in progress and how far it
  has got.
- A platform is recorded only when the file's own signature confirms it, with the folder's
  platform as the first hint. Anything unconfirmed stays unknown until a DAT hash match.
- `sheet_style` (Redump by default) decides how a CHD's cue/gdi is rebuilt everywhere, and
  whether a GD-ROM is a cue (Redump) or a gdi (chdman, MAME).
- A cue sheet whose track file is missing is still scanned, and the missing track is listed in
  the scan's errors.
- Hash match comes in two kinds: new (games with no link yet) and all, which removes the links it
  will re-make and matches every game again. Manual links stay.
- A finished scan queues a DAT hash match of the same directory, and a hash match queues the
  title merge pass when any title is linked to IGDB.
- The title merge pass runs alongside a scan instead of waiting for it.

**Media**
- Media files are stored by what they show: a provider's art under
  `<provider>/<its id for the game>/<region or common>/`, your own uploads under
  `release/<id>/` or `title/<id>/`. A game on several platforms has one folder. A release
  page shows its title's media, and a title page the game's, for whatever it has none of.
- "Download missing media" (Jobs, or on a schedule) fetches the media types turned on in
  Settings that linked games don't have yet, for owned games or the whole catalogue.
- Your own uploads can be placed in `media_priority` for every image and video type, below a
  provider whose art you prefer. Unplaced, they stay first.
- `audio_fallback` sets what a title with no audio plays: nothing, random tracks from other
  titles' audio, or the audio files tagged `generic_bgm`. That tag goes on audio files only.

**RetroAchievements**
- "RA badges download/update" (Jobs, or on a schedule) picks up new unlocks and downloads every
  badge of the account's or your owned games, so their badges show the current state.
  Achievements RetroAchievements has edited since they were fetched are fetched again.
- A game's achievements can be filtered by achieved / unachieved and, separately, missable only.

**Config screens**
- Long explanations are behind a round "!" button: one per setting, tab, card and list.
- Settings and Settings (JSON) have one tab per section and one row per setting. The filter box
  searches every section.
- Per-platform lists (color overrides, Config › Copy's platform table, combine groups) are laid
  out in columns.
- `collection_default_tab` chooses the sub-tab the Collection tab opens on.
- A new Cross-platform tab, left of Titles, lists each IGDB game once whichever platforms it
  is on, with its titles side by side. `platform_priority` sets which platform leads a row.
- audio_client_format defaults to AAC on Linux and xHE-AAC elsewhere (it was "no conversion").
- Every translation is in `assets/i18n/`; a copy edited in the install takes effect without a
  rebuild.
- Config › Emulators says that emulator launching is not tested yet.
- The provider request form is also on Jobs, and provider credentials also under Config ›
  Provider credentials.

**Backup**
- Upload a backup zip on the Backup screen: it is checked, stored and listed. Restoring it is a
  separate step.
- "make DAT seed" writes the DAT releases and titles with their IGDB and RetroAchievements
  ids, and nothing else, as a starting catalogue for another install. Restoring one queues
  the IGDB metadata for its games.

**DAT files** (Data › DATs)
- The synced DAT files can be downloaded (one, one source's, or all as a single zip) and
  uploaded back.
- "import downloaded" imports what is already in the DAT folder without the network.
- A DAT import links its new entries from the downloaded Hasheous dumps by hash, with no files
  on disk, then runs the title merge pass.

**Archive settings** (Config › Copy, the copy dialog, and Settings › `copy_output_defaults`)
- Codecs are pulldowns listing only the ones that can be written.
- RVZ, GCZ and WIA have settings of their own. Saved settings keep their RVZ values; GCZ starts
  at 128k blocks and WIA at lzma, level 5.
- CHD custom preset: separate codecs and hunk size for CD and for DVD images. Saved values
  become the CD ones.

**Database cache** (Config › Server)
- Set as a share of memory (default 25%, no longer capped at 8 GB) or a fixed size, from the
  next start.
- The whole cache is saved at shutdown and reloaded at start, and the catalogue is read into it
  in the background (can be switched off).

**Lists**
- Music discs, films, manuals, magazine scans and source code that the DATs list (Redump's
  Audio CD and DVD-Video, No-Intro's scans, TOSEC's Magazines, ...) are no longer among the
  titles and releases. They have Collection tabs of their own -- Multimedia, Documents and
  Source code -- and no title until one is linked to a game's. Existing catalogues are sorted
  out by the next DAT sync.
- DLC, add-ons, updates, patches, themes and avatars are no longer listed as releases of their
  own. A title page
  shows them in sections below the game's releases, in the same order; a release page shows
  those for its platform, its own region first. The title merge pass moves DLC under its game
  by name. What it cannot place is under material "DLC, updates & themes not bundled with a
  game", to combine by hand (Edit › Combined titles undoes either).
- Columns can be resized in both the web console and the desktop app.

**Providers**
- A bulk request can tick several platforms at once; each becomes its own one-platform job.

## Bug fixes

**Hash match**
- A CHD, zip or 7z was looked up on Hasheous and PlayMatch by the archive's own hash, which
  they never know, so its IGDB and other ids came from the name search. It is now looked up by
  the track or ROM inside.
- A game whose DAT name is a regional title got its IGDB id as
  a fuzzy match. IGDB's regional and alternative names now count as a name match.

**Scan and copy**
- A full scan failed at once with a "mismatched types" database error on newer MariaDB.
- A full scan showed every file in the directory as missing until it reached it again, and left
  them missing if it was interrupted (a server restart, for one). Files are now marked missing
  only when the scan finishes and they are gone from disk.
- Scans and copies handled one file at a time. They now run several at once, set by
  `scan_parallel_files` and `copy_parallel_files`: auto, or a number per file size.
- A scan read a zip, 7z, CHD or RVZ twice. Scan, rehash, copy and conversion now share one
  engine that reads each file once.
- A converted copy was recorded under the hash of its contents (a CD CHD failed with "decoded,
  but produced no whole-file hash"), so a later scan did not recognise it. It is now recorded
  under the hash of the file written.
- A file replaced by other bytes under the same name kept its old integrity check, platform and
  release. A scan now records the new bytes and drops the release they no longer match.
- Creating a CHD from a folder or zip whose track files sort before the cue could hang.
- A CHD was written with only the first codec of its list (`cdzs, cdfl` reached chdman as
  `cdzs`).
- A copy's RVZ "remove junk data" setting was never passed to Dolphin.
- A WIA conversion with the default codec wrote nothing and left a raw disc image named `.wia`.
  WIA has its own codec now, and an empty conversion is reported as an error.
- Codecs 7-Zip can read but not write (deflate64, brotli, lz4, ...) are refused when saved
  instead of failing file by file.
- The CHD presets were described wrongly: both are zstd, and mobile differs only for
  PlayStation 2 (zlib).
- The integrity check and verified copies compared against the cheapest recorded hash (usually
  crc32) instead of the chosen one (`integrity_populate_hash`, `copy_populate_hash`).

**Collection**
- A release showed "✓ verified" only when Hasheous knew the file, so many releases matched
  by their DAT hash (most WiiWare, for one) had no badge at all.

**Media**
- Opening a title fetched no art for its other releases once the first release had some.
- A box back counted as the box front already being there, and raising `media_list_cap` never
  fetched the extra screenshots or fan art.
- Art downloaded before an image dir was set stayed in the app's own folder, and
  RetroAchievements images were always kept in the provider cache. Every provider download now
  goes to the image dir (videos to the video dir), and "media relayout" moves what is already
  stored.
- Each provider's art, and your own uploads, now have their own folder:
  `<Platform>/<Title>/<Region>/<provider or user>/box_front.jpg`, with screenshots, fan art and
  videos under `<Platform>/<Title>/_title/<provider>/screenshots/` and so on. An upload whose
  file name is taken asks whether to overwrite it, save it under a new name, or cancel.

**Desktop app**
- A title's manual release order did not show after ▲▼, release cards could not be dragged, and
  on Windows no drag-to-reorder worked.
- After saving a setting, its editor showed the old value until the screen was reopened.
- Typing quickly in a filter box could drop characters.
- Pulldowns and text boxes outside a toolbar showed pale text on a white box.

**Lists**
- A resized column could not be made narrower than its text, and long values stayed cut off
  when it was widened.

**Title merging**
- Unrelated games (on other platforms, or DLC) were linked to one IGDB game and suggested for
  merging. A PlayMatch guess by file name and size, or a file many games share, no longer links.
  "Audit merges" under Combined titles removes links made this way before.
- Titles linked to the same IGDB game by name were only suggested for merging, so nothing merged
  until files were scanned. They now merge on the same platform; a fuzzy search guess still only
  suggests.
- The merge pass showed no progress until it finished.
- Combined titles shows each merge's grade (manual, hash or name), filters by it, and can
  decouple every merge of one grade.

**Server**
- A shutdown waited without limit for open connections, so the server could seem not to stop.
  It now waits two seconds.
- Installed as a Linux service, the console's shutdown button stopped the server and systemd
  started it again two seconds later. Re-run the installer to update the service.
- On Windows, Ctrl+C or closing the console window killed the database before it was shut
  down, and the next start had to recover it.

**Status**
- Status › Providers: hovering an icon shows why that provider is unavailable again.

**Settings**
- Saving the ports or the database cache dropped `db_max_connections` from
  `data/database.env`, resetting a raised connection limit.

**Providers**
- A bulk sync over every catalog platform finished at once with 0 games when the platforms
  had scanned files but no imported DAT. It now covers the collection's platforms too, and
  shows each platform's pages as they come in.


# romexplorer 0.1.2 (alpha)

Alpha: the database schema may change between 0.1.x releases with no upgrade path. Back up
before replacing a build.

## Changes

**Scheduled jobs**
- Schedules have their own tab, Scheduled jobs, next to Jobs; Config › Schedules is gone.
- Each schedule has "run only when idle" (on by default, and on for existing schedules): a due
  run waits until no other job is queued or running.
- A file quick scan of a chosen directory can be scheduled.

**Scan**
- A file scan shows how many files it has found while it walks the directory, before hashing
  starts.

**Collection**
- The platform filter lists Famicom / NES, Super Famicom / SNES and PlayStation / PS1 classics
  for PSP as one system each, and matches all of a system's releases. Badges and everything
  else keep each release's own platform.

**Settings**
- Metadata source priority lists only the sources that can supply each field: genre from
  IGDB, RetroAchievements and MobyGames; serial from the scanner's own detection, Redump and
  No-Intro; franchise from IGDB; release name from the DAT sources and IGDB. Hasheous and
  PlayMatch are hash-lookup services with no metadata of their own, and are no longer offered.
- Canonical name priority explains that the same order names exports that convert to the
  release name.

## Bug fixes

**Upgrades**
- An upgrade that changed the database schema left a server installed with the default,
  restricted database account unable to start (`Access denied for user 'rx_app'`). The
  server now applies such changes with the root password setup stored, then carries on as
  the restricted account.

**Console**
- The Status page, which is where the console opens, counted every table one after another on
  each load: seconds per table on a large catalogue. The counts now run four at a time and are
  reused for 30 seconds.

**DATs**
- A No-Intro or TOSEC sync showed 50% for the whole import and looked hung: each is one pack
  of many DATs, and progress counted files. The job now reports each DAT in the pack.
- Redump serials were dropped on import (Redump gives one `<serial>` per game, not per rom).
  Re-import the Redump DATs to fill them in. A disc sold under several serials keeps all of
  them: the serial field is now 1024 characters wide.


# romexplorer 0.1.1 (alpha)

Alpha: the database schema may change between 0.1.x releases with no upgrade path. Back up
before replacing a build.


## Changes

**Providers and settings**
- Data › Providers has a credentials form
- Config › Settings is a form for every setting (radios, pulldowns, checkboxes, orderable
  lists); Config › Settings (JSON) shows the stored values as they are and takes new keys.
- Config › Sources is now Config › Share.

**Language**
- The UI's text ships in English and Japanese. Two
  settings: `ui_language` (menus, headings, help text, buttons, messages) and
  `platform_name_language` (platform names, with Japanese names for every platform) --
  independent, so English menus with Japanese platform names is a choice. A missing entry
  falls back to the English, and a file on disk wins over the compiled-in copy, so a
  translation can be corrected in place. 

**Media**
- Added audio file scan/database and playback.
- Images and Videos show in UI

## Bug fixes
**Collection**
- Games: a game linked through Hasheous was reported incomplete (1/4) because Hasheous files
  every regional dump under one entry; those lines are alternatives and count as one. A
  No-Intro BIOS listed headered and headerless (`.nes` + `.unh`) counts as one file too.
- "not on server" is no longer shown in standalone mode, where there is no server to be
  missing from.
- Titles and releases did not list the local games that belong to them.
- Covers that had been downloaded were missing from the grid views.
- A CHD's members were shown under the wrong names, the ISO inside was missing from the
  contents list, and the platform stayed "unknown".
- Hash match skipped files in subfolders of the scanned directory.

**Apps**
- Desktop: back / forward buttons.
- About tab, with the version and these notes.
- List columns: a column can be stretched past the card (the list then scrolls sideways)
  and no further than its widest content, so a badge column stops at its badge and a path
  column reaches its longest path.
- Notifications stayed on screen instead of fading.
- Backup progress ran past its total and past 100%.
- "Sync all sources" showed each source's progress instead of the whole run's.
- Config › Settings could fail to open with "Failed to execute 'appendChild'".

# romexplorer 0.1.0 (alpha)

Alpha: the database schema may change between 0.1.x releases with no upgrade path. Back up
before replacing a build.

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

Tested on Windows x64 and macOS arm64 unless a line says otherwise. Linux and the arm64
Windows/Linux packages are built and pass `romexplorer_dbtool check`, nothing more.

**ROM handling**
- Platform detection from file content: cartridges, disc images, directory games, PKG.
- CHD extract and create (CD, GD-ROM, DVD; zstd, lzma, zlib, flac, huff).
- CHD streaming extract and parallel random-access reads.
- RVZ extract and repack, all five codecs; create RVZ, WIA and GCZ from ISO.
- zip and 7z through the bundled 7-Zip, Deflate decoded by zlib-ng (zip/7z sample round-trips not tested on macOS).
- Hashing: CRC32, MD5, SHA-1, SHA-256, XXH3, and RetroAchievements hashes.
- One-pass extract → hash → repack: CHD repack, RVZ repack, cue set to CHD.
- NES and SNES header add/strip.
- Cue/GDI sheet checks and fixes.
- Copy to a device, with FAT-safe names and playlists.

**Catalogue**
- Bundled MariaDB: bootstrap, migrations, least-privilege app user.
- DAT download and import: Redump, MAME-Redump, RetroAchievements, RetroBIOS.
- DAT download and import: No-Intro, TOSEC (not tested on macOS).
- Hash-first, name-second lookup.
- Release and title merging, with audit and decouple.
- Metadata lookups: Hasheous, EmuReady.
- Metadata lookups and media: IGDB, TheGamesDB, SteamGridDB, MobyGames, ScreenScraper, RetroAchievements (not tested on macOS).
- Background jobs: file scans, bulk provider sync (bulk sync not tested on macOS).

**Server and apps**
- REST API with token and password auth.
- TLS with a self-signed certificate and client-side pinning.
- zstd/gzip response compression.
- Web GUI served at `/app/`.
- Desktop app with an in-process database (macOS: launches and starts its database; not exercised further).
- Service installer, `install.sh` / `install.ps1`, with first-run setup (not tested on macOS).
- Backups written as zstd zip.
