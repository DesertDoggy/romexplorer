# Streaming extraction/creation -- work in progress

**Superseded as a live spec by `docs/plan/rom_module.md`, which folds in everything DONE
here.** Kept as the historical session log and source of the benchmark numbers `rom_module.md`
cites -- not maintained as current status going forward.

> **Since then:** the streaming variant is the *default* output of `build.sh`,
> `build_chdman_lib.sh` and `build_dolphin_rvz.sh` (and of `scripts/move_libs_to_app_root.sh`);
> `--nostreaming` builds/deploys the plain one. `--streaming` below is still accepted as a
> no-op, so the log reads correctly either way.

Session handoff. Goal: add byte-level streaming (one decompression pass feeding both a
disk write and a hasher/re-archiver) alongside the existing extract-to-file flow, for all
6 container formats, with both an unpatched and a streaming build of each native library.

## Ground rules agreed

- Keep the current flow (extract out to file) working untouched; **add** streaming methods.
- Prefer implementing in each submodule's own `user/` dir. Only write a vendored patch
  where `user/`-only is impossible, and never edit vendored sources in place.
- Keep the current build script working; the streaming build goes to
  `user/release/with_streaming/<platform>/<arch>/<version>/...`.
- Streams must identify **which content file** each chunk belongs to, so a consumer can
  hash / write / re-archive separate members as separate files.
- Test and benchmark both variants.

## Why streaming at all (measured, this machine, release build, 1.4 GiB ISO, warm cache)

| Step | Time | Rate |
|---|---|---|
| CRC32 | 0.22 s | 6273 MiB/s |
| XXH3-128 | 0.21 s | 6617 MiB/s |
| SHA-1 | 0.78 s | 1792 MiB/s |
| SHA-256 | 0.89 s | 1573 MiB/s |
| MD5 | 2.33 s | 598 MiB/s |
| all 5 at once | 2.81 s | 496 MiB/s |
| extract RVZ(zstd) -> ISO | 0.43 s | 3210 MiB/s |

Note `cargo test` without `--release` is ~20x slower and misleading -- always benchmark
with `--release`. "Warm OS cache" cannot be guaranteed (the kernel may evict at any time),
which is why real streaming beats hash-immediately-after.

## Feasibility per format (researched, with file:line evidence)

| Format | Route | Vendored patch? |
|---|---|---|
| zip / 7z | wrap the `ISequentialOutStream` our own `GetStream` already hands 7-Zip | No -- `user/` only |
| RVZ / WIA / GCZ extract | reimplement `ConvertToPlain`'s copy loop in `user/` | No -- `user/` only |
| RVZ / WIA / GCZ create | `ObservingBlob : DiscIO::BlobReader` wrapper, pass to `ConvertTo*` | No -- `user/` only |
| CHD raw/DVD extract | `chd_file::read_bytes` loop (~15 lines) | No -- `user/` only |
| CHD cd/gd-rom extract | ~300 lines of pregap/split/swap/interleave logic must stay bit-identical | **Yes** -- 1-line hook |

### zip / 7z (DONE)
`CPP/` is already patched at HEAD (commit "ad streaming and stop for hashing use") with
`IArchiveExtractCallbackData2`, and `user/tests/check-source-hooks.sh` asserts it stays.
That hook is **7z-only** (zip's handler has none), so it was deliberately NOT used.
Instead `CArchiveExtractCallback::GetStream` (ours) wraps the stream it already creates --
works identically for zip and 7z, touches no vendored code.

### RVZ / WIA / GCZ
- `DiscIO::ConvertToPlain` (`Source/Core/DiscIO/FileBlob.cpp:41-112`) is a plain copy loop:
  chunk = `GetBlockSize()` doubled up to >= `0x80000`, progress every `max(1, n/100)`
  chunks, short final chunk, deletes partial output on failure. Reimplement in `user/`.
- `BlobReader` (`Source/Core/DiscIO/Blob.h:61-105`) is fully public/virtual and
  subclassable from outside; all three `ConvertTo*` take `BlobReader*`, not a path.
  `ScrubbedBlob` is the forwarding-wrapper template to copy (9 overrides).
- WIA/RVZ creation re-reads the first 0x80 bytes out of band (`WIABlob.cpp:1814`) -- gate
  the hasher on `offset == expected_cursor` to drop it.
- **Output-side streaming is impossible for GCZ/WIA/RVZ even with a patch**: both
  back-patch their headers after all data is written. Only ISO output is linear.
- Dolphin has **no** patch workflow, and the chdman scratch-copy trick does not transfer
  (34 nested submodules; `git archive` would drop all of `Externals/`).

### CHD
- `chdman_lib.cpp` calls chdman's real `main()` renamed via `-Dmain=chdman_cli_entry`
  (`Makefile.chdman_lib:419-421`).
- `chd.cpp` and `cdrom.cpp` are already compiled/linked, and `-I$(U)` is on every TU, so a
  `user/`-side loop against `chd_file` / `cdrom_file` compiles with no build change.
  Error type is `std::error_condition`; `cdrom_file(chd)` throws `nullptr` -- catch `...`.
- `do_extract_raw` write site: `chdman.cpp:2617-2636` (chunk = `(32MiB/hunk_bytes)*hunk_bytes`).
- `do_extract_cd` write site: `chdman.cpp:2976-2985`. In scope there: `tracknum`,
  `trackbin_name`, `outputoffs`, `bufferoffs`, `trk`, `toc.tracks[trk]` -- everything a
  per-track callback needs. Companion hooks: file switch `chdman.cpp:2891-2897`,
  per-track descriptor `chdman.cpp:2917`.
- Exact-fidelity traps for a hand-rolled cd loop: `actualframes = frames - padframes +
  splitframes`; `phys=true` on reads; data+subcode interleaved per frame; audio byte-swap
  for CUEBIN/GDI when `version > 4`; `output_frame_size` drops subcode for cue/gdi.
- **The build uses `git archive` of a pinned MAME commit** (0.286, from
  `patches/APPLY_PATCHES.md`), so editing `external/mame/` in place does nothing.
  Patches are applied from `patches/0*.patch` (`build_chdman_lib.sh:252-269`).
  Streaming patches live in `user/patches/streaming/` (outside chdman-simd's own `patches/`).
  Existing patches 01-05 do not touch the extract functions -- a new hook patch is
  conflict-free, but patch 01 shifts later line numbers ~+121.

## What is DONE and verified

All in `deps/7-Zip-zstd/`:

1. `sevenzip_lib.h` -- `SevenZipDataCb(entry_index, offset_in_entry, data, size, user_data)`
   (returns 0 to stop) and `sevenzip_extract_entry_stream(...)`, both behind
   `#ifdef SEVENZIP_WITH_STREAMING`.
2. `sevenzip_lib.cpp` --
   - `CTapOutStream`: pass-through `ISequentialOutStream` forwarding to an optional inner
     stream and emitting each chunk. Honors the partial-write contract (writes inner
     first, emits only accepted bytes). `StopFlag` points at the owning callback's bool
     (the tap dies before the result is read -- do not store the flag in the tap).
   - `CArchiveExtractCallback`: added `OnData` / `DataUserData` / `StreamOnly` /
     `DataStopped`; `GetStream` now uses its `index` param and wraps (or replaces) the
     output stream. Init-list order matters (`-Wreorder` is an error).
   - `ExtractOne` gained defaulted streaming params; `sevenzip_extract_entry_stream` added.
3. `sevenzip_lib_streaming.def` -- the 9 existing exports plus `sevenzip_extract_entry_stream`.
   (Adding it to the default `.def` would break that build's link.)
4. `scripts/build.sh` -- `--streaming` flag: sets `CXXFLAGS_EXTRA=-DSEVENZIP_WITH_STREAMING`,
   `DEF_FILE=...sevenzip_lib_streaming.def`, and redirects `out_dir` to
   `release/with_streaming/...`. Default path/flags/exports unchanged.
5. `tests/stream_extract_test.c` -- dlopen-based functional test (skips with status 77
   against a non-streaming build).

Verified on Linux x64, version `v26.02-v1.5.7-R2-55-g3adff3e5`:
- default build: 9 `sevenzip_*` exports, no stream symbol, 3827200 bytes
- streaming build: 10 exports incl. `sevenzip_extract_entry_stream`, 3831296 bytes
- `stream_extract_test` vs default -> `SKIP` (rc 77), as designed
- `stream_extract_test` vs streaming, on a zip and on a solid 7z -> **all checks passed**: chunks
  carry the right entry index, offsets contiguous from 0, total == entry size, the file
  written in the same pass is byte-identical to the streamed bytes, `out_path=NULL`
  streams without writing anything, and returning 0 stops with -6.

Rebuild/verify:
```
cd deps/7-Zip-zstd
bash scripts/build.sh --nostreaming  # plain -> release/linux/x64/<ver>/7z.so
bash scripts/build.sh                # default -> release/with_streaming/linux/x64/<ver>/7z.so
gcc -O2 -Wall -Wextra -o /tmp/stream_test tests/stream_extract_test.c -ldl
/tmp/stream_test release/with_streaming/linux/x64/<ver>/7z.so <archive> 1   # 0=7z, 1=zip
```
Rust: `cargo test --test test_archiver sevenzip_stream_extract` -- created zip/7z plus every
zip/7z fixture archive (drop a solid 7z among them to cover solid skip-through;
`create_archive` always writes non-solid 7z, see `sevenzip_lib.cpp`'s forced `s=off`).

### Capability detection (DONE, Rust side)
- `native_lib::LibVariant { Standard, Streaming }` -> `user/release` vs
  `user/release/with_streaming`. Each wrapper has `find_library(variant)` and a
  `STREAMING_SYMBOL` const. Existing wrapper calls still use `Standard` only.
- `romexplorer::detect_capabilities(variant) -> NativeCapabilities` (per lib: `path`,
  `streaming`). Streaming = the library **exports** `STREAMING_SYMBOL` (dlsym), not merely
  lives in `with_streaming/`. Plain runtime code, so a main app can call it from its own
  `build.rs` (romexplorer as build-dependency) to choose build options.
- Symbol contract the streaming builds must export:
  `sevenzip_extract_entry_stream` (done), `chdman_extract_stream`,
  `dolphinrvz_extract_stream` (names reserved, not implemented yet).
- Test: `cargo test --test test_archiver native_capabilities`.

### chdman (DONE)
All in `deps/chdman-simd/` (chdman-simd's own `patches/` untouched):
- `patches/streaming/06_extract_stream_hook.patch` -- applied after chdman-simd's 01-05;
  calls `chdman_stream::emit()` at the two write sites (`do_extract_raw`, `do_extract_cd`)
  and skips the disk write when stream-only. No stream active == unpatched behaviour.
- `chdman_stream_hook.h` / `chdman_stream.cpp` -- `chdman_extract_stream` (per output file:
  `file_index`, `file_name`, `offset_in_file`; cue/gdi/toc sheet delivered last; -6 on stop)
  and the `chdman_reader_*` random-access API (no patch needed: `chd_file`/`cdrom_file`
  directly, stdio file handle so no Windows sequential-scan hint, optional LRU hunk cache).
- `Makefile.chdman_lib`: `STREAMING=1` adds `chdman_stream.cpp` + `-DCHDMAN_WITH_STREAMING`.
  Also fixed for both variants: `-DXML_DEV_URANDOM` (non-Windows; a fresh obj dir failed
  expat's entropy check -- only a stale `xmlparse.o` had hidden it) and
  `-Wl,--exclude-libs,ALL` on Linux (the statically linked libstdc++ was re-exporting ~2800
  `std::` symbols, and two libchdman copies in one process -- e.g. default + streaming --
  segfaulted in iostream code; now 1263 exports, and both load side by side).
- `build_chdman_lib.sh --streaming` -- own scratch src/obj under `_build/with_streaming/`
  (mandatory: no flag dependency tracking), installs to `release/with_streaming/...`.
- Exports: default 2 (`chdman_run`, `chdman_free_log`), streaming 11.
- `tests/chd_stream_test.c` -- vs the default build's plain extract and optionally the
  official `chdman` (`mame-tools`): per-file streamed == written == reference, stream-only
  leaves nothing, stop -> -6, random reads (single thread, LRU, 4 threads). Passed on CD
  (single/multi-track, audio-first), GD-ROM, and DVD samples up to 2.4 GiB; default build
  output identical to official chdman 0.285 on all of them.
- `tests/chd_reader_bench.c` -- random hunk throughput vs thread count. No internal ceiling:
  scales until logical CPUs, flat/declining past it (oversubscription hurts on DVD).
  Memory per reader ~0.6 MiB (CD) / ~15 MiB (DVD).
- Rust (`chdman_wrapper`): `run_stream`, `ChdReader` (`Send`, one per thread),
  `recommended_reader_threads()` = `available_parallelism()` -- **no hardcoded thread cap**.
  `ChdmanError::{Cancelled, ReadFailed}`. Tests: `chdman_stream_extract`,
  `chdman_reader_parallel_random_access` (samples picked by probing CHD contents).

### dolphin (DONE)
All in `deps/dolphin/` (no Dolphin source touched):
- `dolphinrvz_stream.cpp` -- `dolphinrvz_extract_stream` (ConvertToPlain's exact loop + data
  tap; optional output file), `dolphinrvz_convert_stream` (convert + `ObservingBlob` on the
  input: every input byte delivered once, in order -- re-reads dropped, skipped ranges read
  and delivered in between, tail filled after conversion; needed because WIA/RVZ creation
  reads Wii partition data through a second reader it opens by path), `dolphinrvz_reader_*`
  random access (one reader per thread).
- `dolphinrvz_internal.h` + `dolphinrvz.cpp` refactor: `Convert(options, wrap, cancel)`;
  `dolphinrvz_convert` = `Convert(options, nullptr, nullptr)`.
- `dolphinrvz_target.cmake`: second target `dolphinrvz_streaming` in the SAME build tree
  (links the same static libs -> no Dolphin rebuild; output `with_streaming/libdolphinrvz.so`).
  `build_dolphin_rvz.sh --streaming` builds/installs it; default target/exports unchanged.
- Exports: default 3, streaming 9. Two copies load side by side (dynamic libstdc++; the
  32 GNU_UNIQUE exports are libstdc++ template statics, harmless).
- **Upstream race fixed for both variants**: `FileSystemGCWii::FindFileInfo(u64)` lazily fills
  a `mutable std::map` unlocked; RVZ packing calls it from every compressor thread -> rare
  crash in `_Rb_tree_insert_and_rebalance` (seen once, core dump, default lib WIA->RVZ).
  `dolphinrvz.cpp` now does ConvertToWIAOrRVZ's steps itself and warms each filesystem's
  cache on the calling thread first. Not reproducible on demand (0/12 either way); outputs
  byte-identical across 12 runs.
- `tests/dolphin_stream_test.c` -- vs default build + official `dolphin-tool`; passed for
  GC RVZ/WIA/GCZ and a Wii RVZ (4.7 GB plain), incl. RVZ output of `convert_stream`
  byte-identical to plain convert. WIA/GCZ samples were created once with `dolphin-tool`.
- Rust: `dolphin_wrapper::{extract_stream, convert_stream, DiscReader}`;
  `native_lib::recommended_reader_threads()` shared by all wrappers (no cap). Tests
  `dolphin_stream_extract`, `dolphin_reader_parallel_random_access` (per container format).

## Access matrix (DONE)
Every library, both directions, both access kinds (streaming builds only; default builds
unchanged):

| | extract sequential | extract random | archive from sequential source | archive from random source |
|---|---|---|---|---|
| zip/7z | `extract_stream` | `EntryReader::read_at` (emulated, O(offset)) | `create_archive_from_sources` | same (read in order) |
| CHD | `run_stream` | `ChdReader` | `create_from_sources` | same (read in order / in place) |
| GCZ/WIA/RVZ | `extract_stream` | `DiscReader` | `convert_from_source` (spooled) | same (read in place) |

- Rust `rom::source`: `Source { name, size, Access::{Sequential(Read), Random(ReadAt)} }`,
  `Source::hashed` (in-order hashing tap -> `HashReport`), `pipe()` (push -> pull bridge for any
  `*_stream` callback), `ReadAt` for File / `EntryReader` / `ChdReader` / `DiscReader`.
- 7z: `sevenzip_create_archive_from_sources` (`CSourceInStream` in `GetStream`),
  `sevenzip_entry_reader_*` (extract on a worker thread into an 8 MiB bounded pipe; own
  archive handle per reader). Streaming `.def` updated.
- chdman: `chdman_create_from_sources` + `user/patches/streaming/07_osd_file_open_hook.patch`
  (posix + win `osd_file::open`): registered sources are virtual files (read_at direct;
  sequential through a 32 MiB history window). Sheets (cue/gdi/toc) are parsed with stdio, so
  the sheet source is written to a private temp dir, and track sources are anchored there.
  createcd from .iso/.nrg (stdio too) isn't supported from sources -- use createdvd.
- dolphin: `dolphinrvz_convert_from_source` (`SourceBlob` for read_at, shared lock across
  copies; sequential spooled to a temp file). `Convert` takes an `OpenInput`.
- Tests: `sevenzip_entry_reader_and_sources`, `chdman_create_from_sources`,
  `dolphin_convert_from_source`, `stream_pipe_rvz_to_7z` (RVZ -> pipe -> 7z, hashed).
- Detection: `disc_fs::{read_iso_file_bytes_with, read_iso_volume_id_with,
  read_iso_system_id_with}` and `disc_dispatch::disc_metadata_with_reader` run the per-platform
  ISO methods over any reader (e.g. CHD sector reads); path-based behaviour unchanged.
- Benchmarks: `tests/src/bench_streaming.rs` (`cargo test --release --test bench_streaming --
  --ignored --nocapture`) -> `tests/results/benchmark_streaming_{formats,chd_metadata}.md`.

## Defaults, CHD metadata, parallel decode (DONE)
- `source::default_access(container, operation)`: zip/7z sequential; CHD metadata/extract and
  RVZ extract/archive random (one reader/thread per logical CPU); CHD archive sequential. Basis:
  `tests/results/benchmark_streaming_scaling.md`.
- `detect::chd_disc`: CHD metadata by random access (`ChdDisc` = first data track as its .bin,
  or the DVD image), `chd_disc_signature` / `chd_disc_metadata` / `chd_disc_metadata_batch`
  (parallel, default all CPUs). Wired into `platforms::deep_try_disc_platform` for `.chd` when
  the streaming chdman is present (then validated strictly, like a readable image).
  Test `chd_detection_random_access`.
- `disc_fs`: PVD search probes LBA 16/166 in every layout before the 0..512 scan (PS1 raw
  images: ~1000 wasted sector reads before).
- `rom::parallel::parallel_decode`: N readers over any `ReadAt`, delivery in order / to file /
  unordered. Test `parallel_decode_matches_extract`.
- Dolphin compressor threads: `dolphinrvz_set_compress_threads` (overrides
  `std::thread::hardware_concurrency` inside the streaming lib; Linux/macOS only) +
  `DolphinConvertArgs::compress_threads`. Streaming lib now exports only `dolphinrvz_*`
  (version script) -- required for the override to bind locally.
- Known cost: opening a large DVD CHD decodes its whole hunk map (~16 ms for a 2.4 GB image);
  detection opens once per candidate platform -- caching the open disc per path would help.

## Hash strategy: extract-while-hashing is now the DEFAULT (DONE)
`tests/results/benchmark_streaming_hash_strategy_{rvz,chd_dvd,chd_cd}.md`: hashing while
extracting (one `parallel_decode` pass, in order, writing + hashing each block as it
arrives) vs extracting to a file first and hashing it in a separate pass, tmpfs vs SSD.
- **Fused wins every case measured (RVZ, CHD DVD/raw)**: 1.3x (CHD DVD/raw, tmpfs) up to 23x
  (RVZ, SSD) faster than extract-then-hash. Extract-then-hash is not a legitimate
  alternative on this evidence -- it exists in the codebase only as `parallel_decode` +
  `Delivery::ToFile` with no hashing, for the rare case a hash genuinely isn't wanted.
- Promoted out of the benchmark into production: `rom::parallel::extract_and_hash_to_file`
  (writes + hashes) / `hash_while_decoding` (hash only, nothing written), both documented
  as the default pattern. `HashDigest` is caller-supplied (any `HashFlags`, not just CRC32).
- CHD DVD/raw at 16T: fused is ~identical on tmpfs vs SSD (1193 vs 1157 ms), but
  extract-then-hash falls off a cliff on real SSD (1600 ms tmpfs -> 5109 ms SSD) because its
  extract phase uses unordered/scattered writes (fastest for pure decode-to-tmpfs per
  `scaling.md`) while the fused pass writes strictly in order. Not fully isolated whether
  that's "one pass vs two" or "ordered vs scattered writes" -- doesn't change the
  recommendation either way, since fused is both faster and simpler.
- RVZ: confirms `hash::HashDigest`'s real overhead vs a raw `crc32fast::Hasher` -- ~35%
  slower (9767 vs ~14859 MiB/s at 4T) from the per-chunk `Arc` copy + channel dispatch to
  its background hasher thread. Worth it regardless (still crushes any two-pass approach),
  but relevant if CRC-while-extracting throughput is ever the bottleneck elsewhere.

## Per-container decode thread presets: `rom::decode_threads::DecodeThreads` (DONE)
Decode-side thread count only -- hashing threads are separate/background regardless (see
above), but every recommendation here takes the `HashFlags` that will be hashed alongside
the decode and leaves room for them (`hash::HashDigest::worker_thread_count`, 1-4 depending
on which algorithms, kept side by side with `HashDigest::new`'s own thread-spawning logic so
the two can't drift). `ChdKind::{Cd, DvdRaw}` + `DecodeThreads { chd_cd, chd_dvd_raw, rvz:
Option<usize> }`; `None` = recommended default, `Some(n)` overrides and bypasses the formula
entirely (an explicit pin is taken at face value, not hash-adjusted).

Recommended defaults, from the hash-strategy benchmarks:
- **CHD (both CD and DVD/raw): `physical cores - hash worker threads`, minimum 1.** One
  shared formula for both kinds (`ChdKind` stays in the API in case a future benchmark
  finds a real reason to split them, but nothing so far does). `physical`, not logical --
  this machine is 20 logical / 14 physical, hybrid P/E, so `logical/2` would be wrong too
  (confirmed: `physical_core_count()` correctly returns 14, not `20/2=10`). `scaling.md`
  and `hash_strategy_chd_cd`'s full 1..20T sweep both showed CHD decode (CD and DVD/raw
  alike) still improving at the highest thread count tried (20), unlike RVZ's early
  ceiling -- physical count leaves room for the hash threads without giving up decode
  throughput CHD can still use. `hash_strategy_chd_cd` also found extract-while-hashing and
  extract-then-hash within ~1% of each other in total for CD (111.3s vs 112.4s summed
  across the whole sweep) -- CD decode (CD-specific codecs, e.g. FLAC for audio tracks, are
  heavier per hunk than DVD/raw's) dominates the total time enough that the second hash
  pass barely registers either way, unlike RVZ/DVD's large, consistent fused win. Default
  stays extract-while-hashing regardless (never worse, and still the simpler code path).
- **RVZ: 4, unless that plus the hash threads would exceed physical cores** (same formula,
  capped at 4: `hash_strategy_rvz` found 4 optimal on its own already-hashing benchmark and
  *worse* at 20T -- a single hasher thread can't drain more than ~4 decode producers, so
  unlike CHD, more headroom from a bigger machine doesn't raise this number; it only comes
  down on a machine too small to fit 4 decode + up to 4 hash threads, roughly <8 physical
  cores for the heaviest hash sets).
- Added `num_cpus` dependency for `physical_core_count()` (std has no physical-core API).

## Per-game operation flow: `rom::operation::Single` (DONE, untested against samples)

One pass per game: detect -> extract -> hash -> convert -> archive, every stage switchable,
progress per stage + a cost-weighted blended total, per-stage `Ok/Err/Skipped/Off`.
- `rom::content`: `InputSet::resolve` (plain / directory / cue+gdi set / container),
  `ContentPlan::build` (content files listed without decoding), `produce` (one decode pass
  into per-file `pipeline::ChunkPipeline`s: input digest -> optional converter -> output
  digest -> sinks: file on disk and/or a pipe into `archive::archive_from_sources` on a
  consumer thread). RVZ/GCZ/WIA creation instead pulls a random source (Dolphin) and its
  in-order input callback feeds the pipeline.
- `rom::chd_cd`: CD/GD-ROM CHD as `chdman extractcd` lays it out (datasize bytes per
  frame, audio byte-swap, padframes dropped, GD-ROM-as-cue Redump TOC rewrite with
  cross-track `splitframes`) + `detect::sheet` Rust cue/gdi/toc writer mirroring
  `output_track_metadata` -- of **chdman-simd's patched chdman** (the one this crate ships:
  `deps/chdman-simd/_build/.../src/tools/chdman.cpp`), which differs from the
  MAME submodule's: Redump rewrite for GD-ROM cue, no gdi pregap LBA shift, no `REM SESSION`.
  `FileData::hash` uses it too now (its earlier split over the raw logical stream was
  wrong: subcode, audio order). `CueSheetOptions` (style Redump/RedumpCatalog/Chdman/Mame,
  line ending Native/Lf/Crlf, single-track `(Track 1)` suffix, CATALOG line) is the knob set
  -- chdman writes platform-native newlines (CRLF on Windows), Redump sheets are CRLF.
- Disc signatures cross-checked against CHDlite: added Mega CD's other hardware ids
  (`SEGABOOTDISC`/`SEGADISC`/`SEGADATADISC`), 3DO (`Platform::ThreeDo`, Opera sync bytes),
  PC-FX (`PC-FX:Hu_CD-ROM`), Neo Geo CD `IPL.TXT` via the ISO9660 walk (CHD/RVZ readers
  too), PS3 sector-1 `PlayStation3` fallback, PC Engine CD PVD system-id fallback. Not
  added: DVD-Video (not a game), FM Towns (no known signature).
- `detect::{resolve_candidates, metadata_cycle}` (`CycleOutcome`: platform/remaining
  candidates/metadata/trail; `platforms::rejects_on_miss` decides drops),
  `detect::rvz_disc` (RVZ/WIA/GCZ detection by random access, like `chd_disc`).
- `convert`: `Convert` trait + `StreamConverter`; NES (iNES, DB-backed, buffered), SNES
  copier headers (SMC/SWC exact; FIG/UFO/GD7 best-effort), N64 byte order, FDS header.
  FDS<->QD re-layout and NDS/3DS crypt are NOT implemented (placeholders).
- `ArchiveArgs::validate`: targets mutually exclusive except chd + one of rvz/gcz/wia.
- Tests: `tests/src/test_operation.rs` (sample-backed: CHD content vs chdman extractcd
  byte-for-byte, zip->7z, CHD CD repack, RVZ repack, cue set->CHD, metadata-only parity;
  synthetic: args exclusivity, unknown platform, SNES convert, output collisions, cycle
  rejection). Written, NOT yet run.

## NEXT STEPS (in order)

1. Run `cargo test --lib`, `cargo test --test test_operation`, then the `test_detect` /
   `test_archiver` regressions (the detection refactor rewired nothing in
   `read_metadata_for_input`, but `deep_try_disc_platform` now also reads RVZ by random access).
2. Open questions: Redump cue dialect (currently == Chdman), NES unheadered extension
   (`.unh`), FDS<->QD, SNES FIG/UFO/GD7 header bytes, whether a platform whose container
   target is disabled should fall back to zip.
3. `Multiple` (batch over `Single`s, using `recommended_parallel_*`; built, then removed in
   0.1.3 as unused -- the app's jobs batch through `api::parallel`); the `LazyArchive`
   design.

## Watch out

- chdman and dolphin each hold a process-wide call lock in their create/extract calls;
  `ChdReader`/`DiscReader` reads don't. `Single` only ever pairs readers with creators.
- A pipe-fed `create_from_sources` (createcd) assumes chdman consumes sources in sheet
  order; if it probes tracks out of order the producer/consumer would stall -- the repack
  test is the check.
- `cargo test` accepts only ONE name filter; passing two is an error.
- The user controls git; never run git write commands.
