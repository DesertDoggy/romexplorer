# RomM as a reference (rommapp/romm, master, researched 2026-09-25)

Notes on how RomM identifies ROMs and what its UI looks like, kept as a reference for
this project's hash matching (`docs/plan/api.md`) and GUI (`docs/plan/gui.md`). RomM is
Python/FastAPI + Vue 3/Vuetify; nothing here is copied, it's what to compare against.

## Identification: Hasheous + PlayMatch

### Hasheous (`backend/handler/metadata/hasheous_handler.py`)
- `POST {HASHEOUS_API_URL}/Lookup/ByHash?returnAllSources=true&returnFields=Signatures, Metadata, Attributes`
  with headers `Content-Type: application/json-patch+json`, `User-Agent: RomM/<ver>`,
  `X-Client-API-Key: <app key>`. The body is a **list**: one `{"mD5","shA1","crc"}` per
  top-level file of the game (empty hashes dropped; CHD sends only the CHD sha1; an archive
  sends its largest member's hashes). No sha256, no file name or size.
- Read from the response: `id`, `name`; `metadata[]` with `source` in IGDB/TheGamesDB/
  RetroAchievements -> `immutableId` (ints only; IGDB slugs ignored); `attributes[]` with
  `attributeName == "Logo"` -> cover URL; `signatures{TOSEC, MAMEArcade, MAMEMess,
  NoIntros, Redump, MAMERedump, WHDLoad, RetroAchievements, FBNeo, PureDOSDAT}` -> per-DAT
  "verified" flags, and `signatures[src][i].rom.country/language` -> regions/languages.
  Publisher and year are not used.
- 404 -> conclusive miss (stale ids cleared on a `hashes` scan); other errors -> inconclusive
  (rescans retry). No rate limiter, no retry, no caching.
- **Metadata proxy** (IGDB without credentials): `GET /MetadataProxy/IGDB/Game?Id=<igdb_id>&expandColumns=age_ratings, alternative_names, collections, cover, dlcs, expanded_games, franchise, franchises, game_modes, genres, involved_companies, platforms, ports, remakes, screenshots, similar_games, videos`
  (expanded lists come back keyed by id). `GET /MetadataProxy/RA/Game?Id=` exists but its
  body is unused. Static `HASHEOUS_PLATFORM_LIST` maps platform slugs to hasheous/igdb/
  tgdb/ra platform ids.

### PlayMatch (`playmatch_handler.py`, the hosted service, not the vendored server code)
- `GET https://playmatch.retrorealm.dev/api/v2/identify/ids?fileName=&fileSize=&md5=&sha1=&crc=`
  for **one** file (largest top-level, path tie-break so disc 1 wins). 4 rps limiter, one
  retry on 429 after 2 s.
- `gameMatchType` in `{SHA256, SHA1, MD5, CRC, FileNameAndSize, NoMatch}` -- only `NoMatch`
  is special; every other type is accepted equally. `externalMetadata[].providerName/
  providerId` consumed for IGDB, MobyGames, ScreenScraper, SteamGridDB, LaunchBox (the
  service also returns RA, TGDB, Hasheous, HLTB, Libretro, Flashpoint, Gamelist ids).
- `POST /suggestion/external/game` `{md5, sha1, sha256, fileName, fileSize, mappings:[{provider, providerId}]}`
  fire-and-forget when a user manually matches a game.

### Scan flow (`scan_handler.scan_rom`)
1. Hash stage: Hasheous and PlayMatch run concurrently on the same file hashes.
2. Provider stage (one `gather`, failures -> empty + inconclusive): IGDB by id (Hasheous id
   wins over PlayMatch id), else filename search (`(igdb-N)` tag, serial/titledb/MAME/ScummVM
   normalisers, fuzzy prefix check); RA by Hasheous id, else `(ra-N)` tag, else a local
   per-platform hash index built from `API_GetGameList` (30-day refresh); Moby/SS/LaunchBox/
   SGDB by PlayMatch ids; Hasheous itself fetches IGDB + RA through its proxy.
3. Merge by configured source priority (highest writes last); region/language/tags from
   hash sources fill empty slots.
- Rate limits: 4 rps evenly spaced for PlayMatch/IGDB/RA (429 -> sleep 2 s, one retry);
  IGDB Twitch token cached in Redis. No response caching for Hasheous/PlayMatch, no batching
  beyond the multi-file Hasheous POST.

### What this suggests for romexplorer (not done)
- Send all of a game's file hashes in one Hasheous request (vendored client is single-hash).
- Use Hasheous's MetadataProxy as the IGDB path when `igdb_id/igdb_key` are unset.
- A hosted-PlayMatch `/identify/ids` client as a second hash source (more provider ids
  than Hasheous: Moby/SS/SGDB/LaunchBox), and `/suggestion/external/game` on manual match.
- Read `signatures[].rom.country/language` and the `Logo` attribute from Hasheous hits.

## UI (`frontend/src`, Vue 3 + Vuetify 3 + Pinia; a newer `v2/` UI and a gamepad
## "console mode" under `/console` exist alongside)

### Navigation / routes (`plugins/router.ts`)
- Desktop: permanent left rail (60/90 px, collapsible): Home, Search, Platforms, Collections,
  Scan, Console mode; bottom: Random, Upload, User (avatar -> settings drawer). Platforms and
  Collections open secondary drawers (filter field + list with platform icon, name, rom
  count; collections grouped as collections / smart / virtual). Mobile: top bar + bottom
  navigation with the same five entries.
- Routes: `/` home (stats, recently added, continue playing, platforms, collections),
  `/search`, `/platform/:id`, `/collection/:id` (+ `/virtual/`, `/smart/`), `/rom/:id`
  (+ `/ejs`, `/ruffle` in-browser play), settings group `/scan`, `/upload`, `/user/:id`,
  `/user-interface`, `/library-management`, `/metadata-sources`, `/client-api-tokens`,
  `/administration`, `/server-stats`, `/logs`, `/activity`.
- Gallery filter/sort state mirrored into the query string; details tab in `?tab=`.
  Global dialogs (manual match, edit, refresh metadata, cover search, collections, delete,
  notes, saves/states) are mounted once in the main layout and opened via an event bus.

### Game grid / list
- Three views cycled by one button: big cards, small cards, virtual-scrolled table. Cover
  aspect 2:3 by default; boxart style setting (cover / 3D box 3:4 / physical 1:1 / miximage).
- Card: cover image (small cover as placeholder, skeleton while loading, generated SVG when
  missing -- a distinct question-mark version for unidentified games). Overlays: top-left
  chips of region flag emoji, language flag emoji and play-status emoji (each togglable, max
  3 then a "+" chip); bottom row: platform icon left, right chips for missing-from-filesystem
  (`mdi-folder-question`), Hasheous-verified check, sibling versions count, favorite,
  has-notes. Hover: title bar slides in, zoom button, action bar (play, download, favorite,
  admin menu: manual match / edit / refresh metadata / collections / delete). Optional 3D
  tilt, hover video preview, load animations. Title shows the filename without tags when
  unmatched. Tags (beta/demo/hack/rev) are not on the card -- details page + filter facet.
- Filter drawer (500 px floating): tri-state buttons matched/unmatched, favorites,
  duplicates, playable, missing, verified, has-RA; multi-select facets each with an
  any/all/none logic toggle: genre, franchise, collection, company, age rating, region,
  language, tag, player count, metadata provider, status. Facet values come from the server
  response (`filter_values`) together with `total`, an A-Z `char_index` for a jump bar and a
  rom-id index. Sort persisted in localStorage. Infinite scroll (limit/offset) with a manual
  "load more" fallback; multi-select mode with a floating bulk-action overlay.

### Game details
- Blurred cover as page backdrop; left column: big cover card (cover zoom, SteamGridDB cover
  search), action bar (download incl. selected files, copy link, play, QR code, admin menu),
  related games (from IGDB, linking to the local rom when owned). Right column: title,
  platform icon + name (link to platform gallery), provider rating chips with logos (IGDB,
  Moby, SS, LaunchBox, RA, Hasheous), version switcher for siblings; tabs:
  - details: file list with checkboxes (size, SHA-1 / CHD SHA-1, MD5, CRC), tags chips; user
    collections chips; regions/languages/genres/franchises/collections/companies/player count
    as chips linking to the filtered gallery; age-rating icons; markdown summary; "Sources"
    links to igdb.com / screenscraper.fr / mobygames.com / LaunchBox with the cover's origin.
  - media: carousel (YouTube, local video, screenshots), PDF viewer for manuals, soundtrack
    player. - save data: saves/states upload/download. - personal: backlogged / now playing /
    hidden switches, star rating + difficulty, completion slider, status select, RA progress,
    markdown notes (public/private). - time to beat, additional content (DLC/mods/patches).
- Manual match dialog: text field + search-by ID/Name, provider toggle chips (only enabled
  sources), results as cards with provider logos, then per-provider cover choice, "rename
  file from source name" switch, confirm writes the provider ids. Edit dialog: name, sort
  key, filename, summary, cover upload/URL, manual + soundtrack upload, per-provider id
  fields, raw metadata panel, "Unmatch". Refresh-metadata dialog: sources + scan type for
  one rom.

### Scan
- Three selects: platforms (multi, icon + name + rom count, "not identified" chip; empty =
  all), metadata sources (multi, provider logo, disabled with tooltip when the key is missing
  or the source needs hashes while hashing is off; remembered in localStorage), scan type
  (new platforms / quick / unmatched / update / hashes / complete, each with a subtitle).
  Buttons: scan, abort, manage library.
- Transport: socket.io (`scan`, `scan:scanning_platform`, `scan:scanning_rom`,
  `scan:update_stats`, `scan:done`, `scan:done_ko`, `scan:stop`); the scan survives a page
  refresh and every connected user sees it (a header indicator).
- Progress: auto-scrolling accordion, one panel per platform (icon, name, rom count, new
  firmware, "not identified" chips) expanding to rows with per-rom chips "identifying…",
  "not identified", verified, and one logo per provider that matched; a sticky bottom chip
  with platforms/roms/firmware scanned, new, identified. The same task shows in the admin
  tasks panel.

### Settings / admin / look
- Metadata sources page: a card per provider (logo, key status icon, connectivity icon); keys
  come from env vars and are read-only in the UI; v2 has a drag-orderable metadata/artwork
  priority list.
- Library management: folder-name -> platform mappings table (auto/binding/version),
  excluded platforms/files/extensions, missing games cleanup. Administration: users (roles
  admin/editor/viewer, per-scope permissions, invite links), API tokens, scheduled tasks with
  live progress. User interface: UI version, language, theme (dark/light/auto), which home
  sections show, platform grouping, sibling grouping, show titles/action bar/status/regions/
  languages, 3D effect, animations, boxart style.
- Theme: dark by default; dark palette primary `#8B74E8`, secondary `#9E8CD6`, accent
  `#E1A38D`, background `#0D1117`, surface `#161B22`, top layer `#1C2330`; light primary
  `#371f69` on `#F2F4F8`. Material Design Icons, self-hosted Roboto, translucent chips over
  covers, layered rounded surfaces.

### Assets
- Platform icons: `frontend/assets/platforms/<slug>.svg|.ico` (~300 slugs plus a
  "systematic" set, with an ATTRIBUTIONS file); fallback chain fs_slug.svg -> fs_slug.ico ->
  slug.svg -> slug.ico -> default.ico. Console mode has themed system art.
- Region/language: Unicode flag emoji from code maps (same approach as this console), no
  flag images. Status: emoji.
- Provider logos: `frontend/assets/scrappers/<provider>.png` with one registry (key -> name,
  title, logo, tint) used by badges and the "metadata provider" filter.
- Missing/unmatched/collection/favorite covers: generated SVG blobs as object URLs.
