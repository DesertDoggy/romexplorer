# Coding rules

## The app and the web console offer the same things

There are two UIs over one API: the app (`romexplorer-gui`, desktop and its web build at
`/app/`) and the server's web console (`src/server/console.html`). They must be identical in
what they offer -- every screen, setting, dialog, menu entry, job form and button exists in
both, with the same labels and in the same place. The one difference: the app also has a
**Server console** tab that opens the web console, for reaching the server.

Two more exceptions, both app-only on purpose (`docs/plan/handheld.md`): **Big Picture
mode** (`screens/bigpicture.rs`), a full-screen controller view of screens the app already
has, and the planned phone app **romcompanion**, a deliberate subset. Neither needs a console
counterpart -- but a setting, dialog or job form they rely on still lives in both UIs.

So a change to either UI is done in both, in the same change, and checked in both. A setting
whose form exists in one and points to "Settings (JSON)" in the other is a gap, not a
shortcut.

## Compression: use the fast paths this repo already ships

Never reach for a pure-Rust deflate by default. This project vendors two native libraries
specifically to avoid it, and they are already built and deployed by
`scripts/move_libs_to_app_root.sh`:

| use | reach for | not |
|---|---|---|
| **writing** zip / 7z | `rom::sevenzip_wrapper` (`external/7-Zip-zstd`, built against zlib-ng) | the `zip` crate's own codecs |
| ROM content out of an archive | `sevenzip_wrapper::extract_stream` — chunks into hashing/detection | reading it all into memory first |
| a whole non-ROM member (a DAT, a config) | `sevenzip_wrapper::read_entry` | `extract_stream` — it is for ROM content, and needs the streaming build |
| raw DEFLATE streams (CHD `zlib`/`cdzl`) | `rom::zlib_ng` | `flate2` / `miniz_oxide` |
| anything we choose the format of | **zstd** | deflate |

`read_entry` is `extract_to_buffer` under a name that says what it is for; both are safe at
any size since the use-after-free in `sevenzip_lib.cpp` was fixed (it used to free the buffer
before copying it out, which only looked like it worked below glibc's 128 KB mmap threshold).
`tests/src/test_archiver.rs::extract_to_buffer_survives_members_above_the_mmap_threshold` is
the regression guard.

**The bundled 7-Zip now decodes Deflate through zlib-ng** (`deps/7-Zip-zstd/zlibng_deflate.cpp` replaces
7-Zip's own registration for method 0x40108), which took its archive reading from 205 to
269 MB/s. `deps/7-Zip-zstd/scripts/build.sh` resolves zlib-ng from `deps/zlib-ng`'s release tree
and links it dynamically with an `$ORIGIN` rpath; `move_libs_to_app_root.sh` deploys the
library under its SONAME as well as its plain name, because the loader looks for the former
and the Rust `dlopen` uses the latter.

**Even so: measure before assuming the native path wins.** For *whole-member decompression*
the pure-Rust `zip` crate is still ahead — 385 MB/s against 269 on No-Intro's 49.6 MB IBM PC
datfile — because 7-Zip's per-entry COM and callback machinery costs more than the codec saves.
`dat_parse.rs` therefore stays on the `zip` crate and says why. Raw inflate alone tells the
opposite story (zlib-ng 667 MB/s, flate2 407), which is exactly why the whole-path number is
the one that decides.

Measured on this repo's own 135 MB catalogue export (NDJSON, the shape most of our bulk data
takes), so these are not general-purpose rules of thumb:

| codec | size | throughput |
|---|---|---|
| deflate ‑6 (`flate2`/miniz_oxide, the accidental default) | 19.02 MB | 75 MB/s |
| deflate ‑9 | 18.99 MB | 40 MB/s |
| brotli q5 | 16.88 MB | 58 MB/s |
| **zstd ‑3** | **18.11 MB** | **362 MB/s** |
| zstd ‑19 | 13.87 MB | 1 MB/s |

zstd ‑3 is smaller *and* ~5x faster than the deflate we would otherwise get for free. Higher
levels are a trap: zstd ‑19 and brotli q11 gain a quarter for a hundred times the time, and
brotli q9 is both bigger than q5 and five times slower.

### Where this already applies

- **Backups** (`api::backup`) write zstd ‑3 into the zip. 13s → 6s on the catalogue.
- **REST responses** (`server::compression`) are zstd/gzip by content negotiation. A sync
  export page: 177,932 bytes plain, 42,424 zstd. Per *response*, not per stream — measured at
  0.8% **smaller** than one stream over a whole table, because each page gets entropy tables
  fitted to its own rows, and paging keeps the cursor, retry granularity and progress bar.

### Never compress what is already compressed

`server::compression` runs over every route. tower-http's `DefaultPredicate` skips only tiny
bodies, gRPC, `image/*` and SSE — **not** zip, video, audio, PDF or `application/octet-stream`,
all of which this server does serve. Any new route returning an already-compressed body needs
its content type added to the predicate in `src/server/mod.rs`, or it is compressed twice.
Getting this wrong is invisible: the response is still correct, just needlessly expensive.

### The one place deflate is required

Reading upstream DAT packs. Every No-Intro / TOSEC / redump zip is deflate, so the `zip`
crate keeps its `deflate` feature and `dat_parse.rs` can open them. That is a *read*
requirement — never a reason to write deflate.

### When adding compression anywhere new

Measure on real data from this repo before choosing, and put the numbers in the comment. The
defaults are usually wrong in a way that is invisible until something takes minutes.

## Release notes: short, user-facing, Japanese written as Japanese

`docs/release_notes.md` and `docs/release_notes.ja.md`. The existing entries are the model;
match their length. When in doubt, shorter.

- One or two sentences per item: what changed for the user. No internals (type or function
  names like `Single`/`Multiple`), no reasons or "because" chains, no test details.
- Merge related changes into one item, grouped under an area heading (Scan, Config screens,
  Archive settings, ...).
- A bug goes under **Bug fixes** / **バグ修正**, written as what was wrong ("A scan read a
  zip, 7z, CHD or RVZ twice."), with the fix in one short clause only when it isn't obvious.
- The Japanese is written as Japanese, not translated from the English sentence by
  sentence: ます調, short sentences, natural terms (照合, 記録, 反映), no nested conditional
  clauses copied from English structure. It may say less than the English if that reads
  better.

## Builds and test runs stay inside the repo

On the user's PC, build and test only inside this repo's directory: Norton quarantines
anything built elsewhere. A separate tree (second worktree, scratch output) goes in a
`.`-prefixed subdirectory of the repo, listed in `.gitignore` (see CODING_RULES.md, Tests).
