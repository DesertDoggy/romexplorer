# `rom` module

**Status: implemented and tested** (not a plan anymore for most of this file -- the
original 77-line version predates almost everything below). Supersedes `docs/plan/
streaming.md`, whose session-log content is folded in here; that file is kept only as a
historical record of the streaming design work and its benchmark numbers, not as a live
spec -- see its own header.

## Architecture

One decode pass per game, with hash/convert/archive fanned out off shared chunks -- not
separate passes, not separate per-stage threads. Concretely:

```
detect (sync, before streaming starts)
   |
   v
content::produce  -- one decode pass (parallel::parallel_decode, N reader threads)
   |
   v  Arc<[u8]> chunks, per content file
pipeline::ChunkPipeline
   |-- input HashDigest (hashes the file's own bytes)
   |-- optional Convert (StreamConverter, in-place on the same thread)
   |     |-- output HashDigest (hashes the converted bytes)
   |     '-- ChunkSink::File and/or ChunkSink::Pipe
   '-- (no convert) -> ChunkSink directly
                          |
                          v  ChunkSink::Pipe (source::pipe(), bounded)
                       archive::archive_from_sources  -- own thread, consumes the pipe(s)
```

Detection reads container metadata directly (CHD/RVZ via true random access, zip/7z via
an in-memory member read) -- **no extraction to a temp file ever happens for metadata**.
The main pipeline itself never stages ROM bytes in a temp directory either;
`SingleArgs::temp_dir` is Dolphin's own internal scratch space, not something this crate
writes extracted content into. A separate, non-streaming, crate-root-exported API
(`extract()`/`archive()` in `extract.rs`/`archive.rs`) exists for callers who want real
files on disk without the fused pipeline -- `operation::Single` doesn't use it.

Only **archive creation** runs on a separate thread from the producer (`std::thread::scope`
spawn in `Single::run_stream`, or Dolphin pulling randomly and calling back into the
pipeline in `Single::run_dolphin`, for RVZ/GCZ/WIA targets). Extract/hash/convert are
fused onto the producer thread by design -- benchmarked and chosen over a literal
per-stage-thread model (see "Benchmark evidence" below).

## Module map

**Core orchestration**
| File | Role |
|---|---|
| `mod.rs` | module declarations only |
| `scan.rs` | Directory scanning (layered; see `docs/plan`): `classify_directory` (layer 1 -- one directory's files into ROM candidates / system files / `.m3u` playlist groups / unknowns; `.cue`/`.gdi` tracks never listed apart), `bundle_at` + `walk`/`scan` (layer 2 -- depth-first, single-level directory-as-one-game resolution from a leaf: a declared `RomShape` like `dir(raw)`, or a platform's own bundle signature; never climbs to collapse several bundles like `game/{disk1/, disk2/}` into one -- that linking is left to the database/import layer). `PlatformLayout` (`Forced`/`SeparatedBySubdir`/`Unseparated`) resolves platform context per subtree. `skip_unknown` (default false) controls whether unaccepted files still become `CandidateKind::Unknown` candidates (hashed/detected, never converted or archived to CHD/RVZ/GCZ/WIA -- `unknown_archive_args`) or are only reported. Orchestrator (layer 3: grouping candidates by platform/archive/size and running one `Multiple` per group) not yet written |
| `operation.rs` | `Single`/`SingleArgs` -- the per-game 5-stage orchestrator (see below); also the batch-sizing helper `recommended_parallel_files`, which `api::parallel` uses to size scans and copies. (`Multiple`, a generic batch runner over `Single`s, was removed in 0.1.3: nothing used it -- the app's jobs schedule files in `api::parallel` and chdlite has its own `batch`.) |
| `content.rs` | the real streaming engine: `InputSet::resolve`, `ContentPlan::build` (lists content files without decoding), `produce()` (dispatches to per-container-kind `produce_*`, feeds `pipeline::ChunkPipeline`s) |
| `pipeline.rs` | `ChunkPipeline` -- one chunk in, fanned out to input hash / optional convert / output hash / sinks |
| `parallel.rs` | `parallel_decode()` (N reader threads over any `source::ReadAt`), `extract_and_hash_to_file`/`hash_while_decoding` (the default fused pattern) |
| `decode_threads.rs` | per-container decode-thread-count presets (`DecodeThreads`, benchmark-derived) |
| `source.rs` | `ReadAt`/`Source`/`Access`, `pipe()` (bounded push->pull bridge), the native-wrapper C-callback trampolines |
| `models.rs` | shared arg/result types: `MetadataArgs`, `ConvertArgs`+flags, `ArchiveArgs`+per-format args, container metadata structs (Chd/Rvz/Zip/SevenZip), `RomMetadata`, `RomReason`, `normalize_metadata`/`metadata_score` |

**Detection** (`detect/`)
| File | Role |
|---|---|
| `mod.rs` | the 4 detection modes, `read_metadata_for_input` (single-call), `resolve_candidates`+`metadata_cycle` (split, used by `operation::Single`), extension/path heuristic tables |
| `reason.rs` | `RomReason` (which mode resolved it), `DetectionStepKind`/`Evidence` (real audit-trail values, not descriptions) |
| `disc_dispatch.rs` | per-platform disc title/serial parsers (PS boot serial, PARAM.SFO, XBE/XEX, Sega hardware-id scan, 3DO/PC-FX/PCE-CD signatures, GC/Wii magic) |
| `disc_fs.rs` | shared ISO9660/XDVDFS filesystem readers, generic over any byte-range reader |
| `chd_disc.rs` / `rvz_disc.rs` | disc metadata read by random access straight out of a CHD/RVZ, no extraction |
| `descriptor.rs` | archive format sniffing, zip/7z member read via `sevenzip_wrapper`, cue/gdi-preferred-over-cue selection |
| `sheet.rs` | cue/gdi parsing and writing (byte-identical to chdman's own sheet output) |

**Platform-specific metadata** (`platforms/`)
| File | Role |
|---|---|
| `mod.rs` | dispatch (`deep_try_platform`, `deep_try_disc_platform`/`deep_try_generic_platform`, `rejects_on_miss`) |
| `helpers.rs` | pure classification predicates (`is_chd_platform`, `is_rvz_platform`, family checks) |
| `mycom.rs` | MSX family, PC-8801, PC-98, FM-7, FM Towns |
| `nintendo.rs` | NES/FDS/GB/GBC/GBA/DS/DSi/Sufami Turbo/N64/N64DD/3DS/SFC/WiiU/GC-Wii boot header |
| `other.rs` | PCE/TG16/SuperGrafx, NeoGeo Pocket(Color), NeoGeo CD id inference, WonderSwan(Color), Virtual Boy |
| `sega.rs` | Genesis/Pico, 32X, Master System/Game Gear, SG-1000 (Sega's disc platforms route through `detect::disc_dispatch` instead) |
| `sony.rs` | PKG header (PS3/PSP/Vita, told apart by content type), PARAM.SFO, Vita `.vpk`/`.psv`/game-card images (exFAT), PS3/PSP SFO decryption behind `scan_pkg_titles`, PocketStation (PS1/2/3/PSP discs are parsed in `detect::disc_dispatch`) |
| `switch.rs` | Switch NSP/NSZ (PFS0) and XCI/XCZ, key-free: ticket title id, per-NCA hashing for No-Intro's digital DATs |
| `download.rs` | digital-download/loose-file platforms: DSi/DSiWare, 3DS, PS3, PSP install folders/packages |

**Archive / extract / convert / hash / native wrappers**
| File | Role |
|---|---|
| `archive.rs` | `archive()` (file-to-file, crate-root export) and `archive_from_sources()` (streaming, used by `operation::Single`) |
| `extract.rs` | `extract()` (file-to-file, crate-root export, not used internally); real GD-ROM/CD naming logic (`CdOutputFormat::{CueBin,Gdi}`) |
| `convert/mod.rs` | `Convert` trait, `StreamConverter`, `converter_for()` -- NES/FDS/SNES/N64 registered, nothing else (see "Not implemented") |
| `convert/nes.rs` | `fcdb.bin` hash-database-backed NES header add/strip |
| `chd_cd.rs` | chdman-simd's CD/GD-ROM extract layout logic reimplemented in native Rust (frame segments, audio byte-swap, Redump GD-ROM TOC rewrite) |
| `chd_info.rs` | parses chdman's own `info` text output into `ChdMetadata` |
| `chdman_wrapper.rs` / `dolphin_wrapper.rs` / `sevenzip_wrapper.rs` | FFI wrappers around the three native libraries (file-to-file + streaming + random-access reader entry points each) |
| `zlib_ng.rs` | dlopen binding to zlib-ng, for CHD's zlib/cdzl codecs |
| `native_lib.rs` | `LibVariant`, `recommended_reader_threads()`/`physical_core_count()` (no hardcoded cap), `detect_capabilities()` |

## Detection: the 4 modes

Ported from `reference/old_archiver.rs`, minus its `strict_metadata_mode`/`quick_metadata_mode`
(nothing needs either). **In cycle-priority order:**

1. **Forced** -- caller asserted the platform; skips path/extension heuristics and never
   tries any other candidate. **Still runs real signature detection for that one platform**
   -- `metadata_for_forced_platform` calls `platforms::deep_try_platform` with the same
   dispatcher the cycle itself uses per-candidate, not a lesser/skipped path. For the 12
   platforms with real signature validation (`disc_signature_validated`: Dreamcast, Saturn,
   SegaCD, GameCube, Wii, TurboGrafxCD, PS1/2/3, PSP, 3DO, PC-FX), a readable file whose
   content doesn't match the forced platform comes back with empty title/serial (a real,
   detectable mismatch -- just not auto-corrected or hard-errored, since the platform is
   asserted as ground truth). Note: the CHD/RVZ *container* metadata the archive/convert
   stages actually consume (track layout, hunk size, disc validity) isn't sourced from this
   detection step at all -- `content::ContentPlan::build()` reads that separately and
   unconditionally (`chd_info::read_chd_metadata` / Dolphin's `DiscReader::info()`), every
   run regardless of detection mode, and fails independently if the container itself isn't
   valid.
2. **Path heuristic** -- parent directory name names the platform.
3. **Extension heuristic** -- file extension names the platform (possibly narrowed to a
   single candidate by the path heuristic when the extension alone is ambiguous).
4. **Deep cycle** -- reads the file, trying each narrowed candidate's own header/descriptor
   parser (`platforms::deep_try_platform`) until one produces real title/serial evidence.

Extension/path narrowing happens entirely *before* the cycle; the cycle itself only
performs per-candidate metadata extraction, it never re-derives candidates.

**Platform hint** (`MetadataArgs::preferred_platform`, `detect::hinted_metadata_cycle`) --
not a fifth mode but a soft version of mode 1: a platform (e.g. the platform folder a scan
found the file under) is tried first on its own, and real title/serial evidence settles it
(`RomReason::Hint`); anything else falls back to exactly the unhinted detection, without
re-trying the hint. Only tried when the file's extension admits it -- the same rule the
path heuristic narrows by (`narrow_extension_candidates_by_path`) -- so a stray `.gba` in a
PlayStation folder is never tried as PlayStation. A wrong hint costs one parser attempt,
never a wrong answer. Ignored when a platform is forced.

The extension table is `detect::EXTENSION_TABLE` (extension -> candidate platforms, in
cycle order), read forward by `infer_platforms_from_extension_heuristics` and in reverse by
`extensions_for_platform`.

### `operation::Single`'s detection flow, confirmed against the code

`Single::detect()` ([operation.rs:801-831](../../src/rom/operation.rs)):

```rust
fn detect(&self) -> (RomMetadata, Vec<Platform>, StageResult) {
    let flags = HeuristicFlags::new(meta_args.extension_heuristic, meta_args.path_heuristic);
    if let Some(forced) = meta_args.forced_platform {
        // mode 1: short-circuits inside read_metadata_for_input, no heuristics, no cycle
        return (detect::read_metadata_for_input(&self.input, Some(forced), flags.ext, flags.path), vec![forced], StageResult::Ok);
    }
    // modes 2/3: extension first, narrowed by path
    let candidates = detect::resolve_candidates(&self.input, None, flags);
    // mode 4: the deep cycle over exactly those candidates (or every platform if empty)
    let outcome = detect::metadata_cycle(&self.input, candidates, flags);
    // fallback: if the cycle landed on Unknown, use the pure-heuristic answer, or the
    // single remaining (rejection-narrowed) candidate if the heuristics gave nothing
    ...
}
```

**Confirmed: force -> (if not forced) heuristics (extension, then path) -> signature/deep
cycle detection**, exactly as designed. `operation::Single` deliberately calls the split
`resolve_candidates`/`metadata_cycle` primitives rather than the single-call
`read_metadata_for_input` -- both paths run identical logic (`detect/mod.rs`'s own comment:
*"Candidate narrowing and the detection cycle as separate, public steps (for
`operation::Single`, which runs them explicitly: forced? -> extension -> path -> cycle).
`read_metadata_for_input` above is the same flow in one call, unchanged."*). A 4th step not
in the earlier plan but present and intentional: if the cycle can't settle on one platform,
`Single::detect` falls back first to the pure-heuristic answer, then to the single
remaining candidate if the cycle's rejections (`platforms::rejects_on_miss`) narrowed the
list to exactly one, even without positive title/serial evidence for it.

Acceptance rule for what counts as "the cycle found something" is deliberately *not*
`metadata_score(&meta) > 0` (which is what old code tested) -- that score counts `platform`/
`media_type`, which the cycle fills in from whichever candidate it's trying regardless of
whether it read anything, so every candidate would score >0 and the cycle would always
"succeed" on the first platform tried. Only a `title` or `serial_product_id` actually read
from the file counts as evidence (`has_content_evidence`).

Audio-container extensions (`.wav`/`.wv`/`.flac`/`.t77`/`.cas`) get special handling
throughout: the container format says nothing about which machine's tape it holds, so a
mere title match isn't decisive -- the cycle keeps looking for a serial (platform-specific
evidence) across every candidate before falling back to the best generic title match with
`platform` left `Unknown`.

## The 5 operations

| Operation | File(s) | Threading | Streaming | Formats |
|---|---|---|---|---|
| **Detect** | `detect/**`, `platforms/**` | single-threaded per input (`chd_disc::chd_disc_metadata_batch` supports parallel batching but nothing calls it yet) | yes -- CHD/RVZ via true random access, zip/7z via in-memory member read, no extraction | zip, 7z, chd (CD/GD-ROM + DVD/raw), rvz/wia/gcz, cue+bin, gdi, plain files, directories |
| **Extract** | `content.rs` (fused, used by `operation::Single`) and `extract.rs` (standalone file-to-file, not used internally) | `parallel::parallel_decode` (N reader threads); zip/7z sequential (native library constraint) | yes for the `content.rs` path (in-process, chunks pushed straight into `pipeline::ChunkPipeline`); `extract.rs` is deliberately non-streaming | zip, 7z, chd (CD/GD-ROM -> cue+bin or gdi, DVD/raw -> iso/raw), rvz/wia/gcz -> iso, plain files/dirs need none |
| **Hash** | `hash.rs` (`HashDigest`), fed by `pipeline::ChunkPipeline` | one worker thread per algorithm (1-4; CRC32+XXH3 combine into one thread when a heavier hash also runs); RAHASH on its own thread (needs sibling files by path, not a byte-stream algorithm) | yes -- chunk-by-chunk off the decode/convert stream, never a second pass | same as Extract; RAHASH additionally needs the real cue/bin track files on disk |
| **Convert** | `convert/mod.rs`, `convert/nes.rs`, inside `ChunkPipeline`'s converter slot | same thread as the rest of the pipeline (not its own thread) | streaming for FDS/SNES/N64 (chunk-by-chunk with alignment carry-over); **buffered** for NES (needs the full-file hash for its DB lookup before any output) | NES headered<->unheadered, FDS `FDS\x1A` header add/strip, SNES sfc<->smc/swc (fig/ufo/gd7 refuse with an explicit error, not silently wrong), N64 z64<->v64<->n64 |
| **Archive** | `content`+`pipeline` -> `archive::archive_from_sources` (streaming, used by `operation::Single`) and `archive::archive()` (file-to-file, standalone) | the native libraries' own internal compression threads (`num_processors`/`compress_threads`/`max_workers`); chdman and dolphin each hold a process-wide call lock on create/extract (readers don't) | yes for the streaming path (`Source`s -- files, pipes, or another container's decoded bytes -- feed the native libraries' streaming entry points directly); two caveats: WIA/RVZ/GCZ output can't be fully streaming even patched (headers are back-patched after all data is written), and CHD `createcd` from sources needs a private temp dir for the sheet itself (chdman parses sheets with stdio) | rvz/gcz/wia (GameCube/Wii only), chd (disc platforms only), zip/7z (any platform) |

`SingleArgs{metadata, stages: StageSwitches, hash_flags, convert, archive, output_dir,
temp_dir, cue_sheet, cd_sheet, decode_threads, max_workers, cost_model}` drives all five;
`Stage::ALL = [Detect, Extract, Hash, Convert, Archive]` in fixed order, each with its own
`StageResult{Off,Skipped,Ok,Err}` and `SkipReason`; a failed stage cascades `Err` to its
dependents (`StageResults::fail()`). `CostModel` gives a blended `total_fraction` progress
value across byte-equivalent per-stage/per-container weights.

## Not implemented (flagged explicitly, not silently missing)

- **Scan orchestrator (layer 3 of `scan.rs`)** -- designed, not yet written. Layers 1
  (`classify_directory`) and 2 (`bundle_at`, `walk`/`scan`, `PlatformLayout`,
  `skip_unknown`) exist, with global dedup already in place (`walk`'s accumulator skips a
  path already seen anywhere in the tree). Still open: marker-anchored roots for deep
  digital dumps where the leaf `is_leaf`/`bundle_at` would resolve isn't the real title
  root (PS3's `PS3_GAME/USRDIR/...`) -- flagged, not designed. The orchestrator itself
  groups candidates by (platform context, archive or not, size class, whether the
  candidate's immediate parent is the walk root -- see `docs/plan`'s root-boundary rule)
  and runs one batch of `Single`s per group, sized with `recommended_parallel_files`
  from each input's size and storage class.
- **NDS/NDSi/3DS/New3DS crypt** -- `ConvertArgs` has the flag bits
  (`nds_crypt`/`ndsi_crypt`/`o3ds_crypt`/`n3ds_crypt`) and its own doc comment already calls
  them placeholders; `convert::converter_for` has no match arm for any of the four platforms.
- **FDD<->QD re-layout** -- does not exist. What exists (`FdsConverter`) is an `FDS\x1A`
  16-byte container-header add/strip, a different and much smaller thing than a floppy-image
  re-layout. An early note calling this "fdd<->qd" was inaccurate and is corrected here.
- **SNES FIG/UFO/GD7 headers** -- shape-building code exists for their header bytes, but
  `SnesConverter::shape()` explicitly refuses them (`ConvertError::Failed`, not a silent
  wrong output); only SFC/SMC/SWC actually convert.
- **`.raw`/`.bin` filename matching against a reference database** (Redump names `.raw` as
  `.bin`) -- no such logic exists in `rom/` or `database/`. Likely belongs in the database
  module's import/matching layer, not here -- scope this out of `rom/` rather than treating
  it as a gap in this module.
- **`LazyArchive`** -- design mentioned in the earlier session log, not started.

## Benchmark evidence (from `docs/plan/streaming.md`, condensed)

Machine-measured (release build), the basis for the defaults above:

- **Fused extract-while-hash beats extract-then-hash in every case measured**: 1.3x (CHD
  DVD/raw, tmpfs) up to 23x (RVZ, SSD) faster. This is why `content::produce` always hashes
  off the same pass rather than as a second one.
- **CHD decode threads**: `physical_core_count() - hash_worker_threads`, minimum 1 (same
  formula for CD and DVD/raw) -- decode throughput was still improving at 20 threads on the
  benchmark machine (14 physical / 20 logical, hybrid P/E), unlike RVZ.
- **RVZ decode threads**: 4, capped so it plus hash threads never exceeds physical cores --
  a single hasher thread can't drain more than ~4 decode producers; more only helps up to
  that point.
- `physical_core_count()` (not `logical/2`) matters concretely on hybrid P/E CPUs -- verified
  correct (14, not `20/2=10`) on the benchmark machine.
- `HashDigest`'s per-chunk `Arc` + channel dispatch costs ~35% throughput vs. a raw hasher
  (9767 vs ~14859 MiB/s at 4 threads on RVZ) -- acceptable given the fused-pass win still
  dwarfs any two-pass alternative.

## Testing

`tests/src/test_operation.rs` (sample-backed, `tests/results/operation_*.md`): CHD content
vs. `chdman extractcd` byte-for-byte, zip->7z, CHD CD repack, RVZ repack, cue set->CHD,
metadata-only parity; plus synthetic cases (args exclusivity, unknown platform, SNES
convert, output collisions, cycle rejection). `tests/src/test_detect.rs`,
`tests/src/test_archiver.rs`, `tests/src/test_hasher.rs`, `tests/src/test_converter.rs`
cover their respective layers; `tests/src/bench_streaming.rs` is the benchmark suite behind
the numbers above (`cargo test --release --test bench_streaming -- --ignored --nocapture`).
