//! Streaming benchmarks. Release only -- debug builds are ~20x slower and misleading:
//!
//! ```text
//! cargo test --release --test bench_streaming -- --ignored --nocapture
//! ```
//!
//! Needs the streaming builds of all three native libraries and the samples root (see
//! `support/inputs.rs`). Results go to `tests/results/benchmark_streaming_{formats,chd_metadata}.md`. Timings are
//! warm-cache (every input is read once before measuring).
//!
//! 1. `formats`: per container -- CHD (CD), CHD (DVD), RVZ (zstd), zip (deflate), 7z (zstd):
//!    extract sequential (streaming decode), extract random (reader, 1 thread and one reader
//!    per logical CPU), archive from a sequential source, from a random source, and from a
//!    file path (the existing API, as baseline).
//! 2. `chd_metadata`: per platform (PC Engine CD, PSP, PS1, PS2 CD, PS2 DVD), the platform's
//!    own metadata method over a CHD, timed separately for confirming the disc signature and
//!    for extracting title/serial, three ways: extract the disc with chdman first (its
//!    single-threaded decode) then parse the file; sequential streaming decode that stops as
//!    soon as the parser has what it needs; random access (sector reads, no decode beyond
//!    what's asked for).
//! 3. `scaling`: CHD/RVZ extract and RVZ archive across 1..20 threads (`rom::parallel`), plus
//!    the CHD metadata random-access path across the same thread counts.
//! 4. `hash_strategy_*`: hashing while extracting (one pass) vs extracting then hashing (two
//!    passes, to tmpfs or to disk) -- `hash_strategy_rvz` (2T/4T/20T), `hash_strategy_chd_dvd`
//!    (fixed 16T), `hash_strategy_chd_cd` (full 1..20T sweep, #[ignore]d like the others but
//!    deliberately not run alongside the rest -- see its own doc comment).

#[path = "support/mod.rs"]
mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use romexplorer::common::models::Platform;
use romexplorer::rom::chdman_wrapper::{self, ChdInfo, ChdReader, ChdTrack, ChdmanArgs, ChdmanCommand, CD_MAX_SECTOR_BYTES, CD_TRACK_AUDIO, CD_TRACK_RAW_DONTCARE};
use romexplorer::rom::detect::{disc_dispatch, disc_fs};
use romexplorer::rom::dolphin_wrapper::{self, DiscCompression, DiscFormat, DiscReader, DolphinConvertArgs};
use romexplorer::rom::native_lib::{self, LibVariant};
use romexplorer::rom::sevenzip_wrapper::{
    self, create_archive, create_archive_from_sources, ArchiveFormat, CompressionMethod, CreateArchiveArgs, CreateEntry, EntryReader,
    SevenZipArchive,
};
use romexplorer::rom::source::{ReadAt, Source};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn root() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/output/bench_streaming");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Scratch space for the scaling benchmark: a tmpfs (RAM) when there is one, so file output
/// measures the writer, not the SSD's sustained write speed.
fn ram_scratch() -> (PathBuf, bool) {
    let shm = Path::new("/dev/shm");
    if shm.is_dir() {
        let dir = shm.join("romexplorer_bench_scaling");
        if std::fs::create_dir_all(&dir).is_ok() {
            return (dir, true);
        }
    }
    (root().join("scaling"), false)
}

fn streaming_builds_present() -> bool {
    sevenzip_wrapper::find_library(LibVariant::Streaming).is_some()
        && chdman_wrapper::find_library(LibVariant::Streaming).is_some()
        && dolphin_wrapper::find_library(LibVariant::Streaming).is_some()
}

fn warm(path: &Path) {
    let _ = std::io::copy(&mut std::fs::File::open(path).unwrap(), &mut std::io::sink());
}

fn mib_s(bytes: u64, d: Duration) -> f64 {
    bytes as f64 / 1048576.0 / d.as_secs_f64()
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

/// A CHD sample with the platform its directory maps to (`disc` records).
struct ChdSample {
    path: PathBuf,
    platform: Platform,
    info: ChdInfo,
}

fn chd_samples() -> Vec<ChdSample> {
    let root = support::inputs::samples_root();
    let mut out = Vec::new();
    for row in support::inputs::rows("disc") {
        let [dir, platform] = row else { continue };
        let Some(platform) = support::inputs::platform(platform) else { continue };
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("chd")) {
                continue;
            }
            let Ok(reader) = ChdReader::open(&path, None, 0) else { continue };
            let Ok(info) = reader.info() else { continue };
            out.push(ChdSample { path, platform, info });
        }
    }
    out.sort_by_key(|s| std::fs::metadata(&s.path).map(|m| m.len()).unwrap_or(0));
    out
}

fn sample_label(path: &Path) -> String {
    path.strip_prefix(support::inputs::samples_root()).unwrap_or(path).display().to_string()
}

fn chd_args(input: &Path, output: &Path, split: bool) -> ChdmanArgs {
    ChdmanArgs {
        input: Some(input.to_path_buf()),
        output: Some(output.to_path_buf()),
        output_split_bin: split,
        output_force: true,
        ..Default::default()
    }
}

/// First data track of a CD CHD (index, info).
fn first_data_track(reader: &ChdReader) -> (u32, ChdTrack) {
    let info = reader.info().unwrap();
    (0..info.num_tracks)
        .map(|i| (i, reader.track(i).unwrap()))
        .find(|(_, t)| t.track_type != CD_TRACK_AUDIO)
        .expect("CD has a data track")
}

/// Random-access view of one CD track as its extracted .bin (data_size bytes per sector),
/// served by sector reads straight out of the CHD.
fn cd_track_reader(reader: &mut ChdReader, track: ChdTrack) -> impl FnMut(u64, usize) -> Result<Vec<u8>, String> + '_ {
    let data_size = track.data_size as u64;
    let mut sector = vec![0u8; CD_MAX_SECTOR_BYTES];
    move |offset, len| {
        let mut out = Vec::with_capacity(len);
        let (mut pos, end) = (offset, offset + len as u64);
        while pos < end {
            let lba = pos / data_size;
            if lba >= track.frames as u64 {
                return Err("read past track".into());
            }
            let within = (pos % data_size) as usize;
            reader
                .read_sector(track.physical_start + lba as u32, &mut sector, CD_TRACK_RAW_DONTCARE, true)
                .map_err(|e| e.to_string())?;
            let take = (data_size as usize - within).min((end - pos) as usize);
            out.extend_from_slice(&sector[within..within + take]);
            pos += take as u64;
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// 1. Formats
// ---------------------------------------------------------------------------

struct Row {
    format: String,
    sample: String,
    plain_bytes: u64,
    cells: Vec<(String, String)>,
}

/// K random `len`-byte reads over `size` bytes, 1 thread and `threads` threads (one reader
/// each, from `open`). Returns (1-thread MiB/s, reads/s, N-thread MiB/s, reads/s).
fn random_reads<R: ReadAt>(open: impl Fn() -> R + Sync, size: u64, len: usize, reads: usize, threads: usize) -> (f64, f64, f64, f64) {
    let run = |seed: u64| {
        let mut reader = open();
        let mut buf = vec![0u8; len];
        let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ seed;
        for _ in 0..reads {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let offset = state % (size - len as u64);
            reader.read_at(offset, &mut buf).unwrap();
        }
    };
    let t = Instant::now();
    run(1);
    let one = t.elapsed();
    let t = Instant::now();
    std::thread::scope(|s| {
        for i in 0..threads {
            let run = &run;
            s.spawn(move || run(i as u64 + 2));
        }
    });
    let many = t.elapsed();
    let bytes_one = (reads * len) as u64;
    let bytes_many = bytes_one * threads as u64;
    (
        mib_s(bytes_one, one),
        reads as f64 / one.as_secs_f64(),
        mib_s(bytes_many, many),
        (reads * threads) as f64 / many.as_secs_f64(),
    )
}

/// Runs every mode `rounds` times, interleaved (a b c a b c ...), so thermal/turbo drift
/// affects all modes alike; returns each mode's median.
fn interleaved(rounds: usize, modes: &mut [&mut dyn FnMut()]) -> Vec<Duration> {
    let mut times = vec![Vec::new(); modes.len()];
    for _ in 0..rounds {
        for (i, mode) in modes.iter_mut().enumerate() {
            let t = Instant::now();
            mode();
            times[i].push(t.elapsed());
        }
    }
    times.into_iter().map(median).collect()
}

const ARCHIVE_ROUNDS: usize = 3;

#[test]
#[ignore]
fn formats() {
    if !streaming_builds_present() {
        eprintln!("streaming builds missing -- skipping");
        return;
    }
    let dir = root().join("formats");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let threads = native_lib::recommended_reader_threads();
    let samples = chd_samples();
    let mut rows: Vec<Row> = Vec::new();
    let read_len = 64 * 1024;

    // --- CHD CD (PC Engine CD) and CHD DVD (PSP) ---
    for (label, platform, want_cd) in [("CHD (CD)", Platform::TurboGrafxCd, true), ("CHD (DVD)", Platform::PlayStationPortable, false)] {
        let Some(sample) = samples.iter().find(|s| s.platform == platform && s.info.is_cd == want_cd) else {
            eprintln!("{label}: no sample");
            continue;
        };
        warm(&sample.path);
        let mut cells = Vec::new();
        let (extract_cmd, create_cmd, sheet) =
            if want_cd { (ChdmanCommand::ExtractCd, ChdmanCommand::CreateCd, "disc.cue") } else { (ChdmanCommand::ExtractDvd, ChdmanCommand::CreateDvd, "disc.iso") };

        // plain files (for the archive side), and the plain size
        let plain_dir = dir.join(format!("{}_plain", if want_cd { "cd" } else { "dvd" }));
        std::fs::create_dir_all(&plain_dir).unwrap();
        chdman_wrapper::run(extract_cmd, &chd_args(&sample.path, &plain_dir.join(sheet), want_cd), |_, _| {}).unwrap();
        let plain_files: Vec<PathBuf> = std::fs::read_dir(&plain_dir).unwrap().map(|e| e.unwrap().path()).collect();
        let plain_bytes: u64 = plain_files.iter().map(|p| std::fs::metadata(p).unwrap().len()).sum();
        plain_files.iter().for_each(|p| warm(p));

        // extract sequential: streaming decode, nothing written
        let scratch = dir.join("stream_only");
        std::fs::create_dir_all(&scratch).unwrap();
        let t = Instant::now();
        let mut total = 0u64;
        chdman_wrapper::run_stream(extract_cmd, &chd_args(&sample.path, &scratch.join(sheet), want_cd), false, |_, _, _, c| {
            total += c.len() as u64;
            true
        }, |_, _| {})
        .unwrap();
        let d = t.elapsed();
        cells.push(("extract seq".into(), format!("{:.0} MiB/s", mib_s(total, d))));

        // extract random: logical bytes via ChdReader (64 KiB reads)
        let logical = sample.info.logical_bytes;
        let path = sample.path.clone();
        let (m1, r1, mn, rn) = random_reads(|| ChdReader::open(&path, None, 4).unwrap(), logical, read_len, 2000, threads);
        cells.push(("extract random 1T".into(), format!("{m1:.0} MiB/s ({r1:.0} reads/s)")));
        cells.push((format!("extract random {threads}T"), format!("{mn:.0} MiB/s ({rn:.0} reads/s)")));

        // archive: sequential sources, random sources, file path
        let sources = |random: bool| -> Vec<Source> {
            plain_files
                .iter()
                .map(|p| {
                    let name = p.file_name().unwrap().to_string_lossy().into_owned();
                    if random {
                        Source::file(p, name).unwrap()
                    } else {
                        Source::sequential(name, std::fs::metadata(p).unwrap().len(), std::fs::File::open(p).unwrap())
                    }
                })
                .collect()
        };
        let out = dir.join("out.chd");
        let from_sources = |random: bool| {
            let mut srcs = sources(random);
            let args = ChdmanArgs { input: Some(sheet.into()), output: Some(out.clone()), output_force: true, ..Default::default() };
            chdman_wrapper::create_from_sources(create_cmd, &args, &mut srcs, |_, _| {}).unwrap();
        };
        let times = interleaved(
            ARCHIVE_ROUNDS,
            &mut [
                &mut || from_sources(false),
                &mut || from_sources(true),
                &mut || {
                    chdman_wrapper::run(create_cmd, &chd_args(&plain_dir.join(sheet), &out, false), |_, _| {}).unwrap();
                },
            ],
        );
        for (name, d) in ["archive seq src", "archive random src", "archive from file"].iter().zip(times) {
            cells.push((name.to_string(), format!("{:.0} MiB/s", mib_s(plain_bytes, d))));
        }

        rows.push(Row { format: label.into(), sample: sample_label(&sample.path), plain_bytes, cells });
    }

    // --- RVZ (zstd): a GameCube disc re-encoded as zstd level 5, 128 KiB blocks ---
    let gc_dir = support::inputs::rows("disc")
        .find(|r| r.len() >= 2 && support::inputs::platform(&r[1]) == Some(Platform::GameCube))
        .map(|r| support::inputs::samples_root().join(&r[0]));
    let gc_source = gc_dir.and_then(|d| {
        let mut rvz: Vec<PathBuf> = std::fs::read_dir(d).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "rvz")).collect();
        rvz.sort_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0));
        rvz.into_iter().next()
    });
    if let Some(gc) = gc_source {
        let user_dir = dir.join("dolphin_user");
        let iso = dir.join("gc.iso");
        dolphin_wrapper::convert(&DolphinConvertArgs::new(&gc, &iso, DiscFormat::Iso).with_user_dir(&user_dir), |_, _| true).unwrap();
        let rvz_args = |input: &Path, out: &Path| {
            DolphinConvertArgs::new(input, out, DiscFormat::Rvz).with_user_dir(&user_dir).with_block_size(131072).with_compression(DiscCompression::Zstd, 5)
        };
        let rvz = dir.join("zstd.rvz");
        dolphin_wrapper::convert(&rvz_args(&iso, &rvz), |_, _| true).unwrap();
        warm(&iso);
        warm(&rvz);
        let plain_bytes = std::fs::metadata(&iso).unwrap().len();
        let mut cells = Vec::new();

        let t = Instant::now();
        let mut total = 0u64;
        dolphin_wrapper::extract_stream(&rvz, None, Some(&user_dir), |_, _, _, c| {
            total += c.len() as u64;
            true
        }, |_, _| true)
        .unwrap();
        cells.push(("extract seq".into(), format!("{:.0} MiB/s", mib_s(total, t.elapsed()))));

        let (m1, r1, mn, rn) = random_reads(|| DiscReader::open(&rvz, Some(&user_dir)).unwrap(), plain_bytes, read_len, 2000, threads);
        cells.push(("extract random 1T".into(), format!("{m1:.0} MiB/s ({r1:.0} reads/s)")));
        cells.push((format!("extract random {threads}T"), format!("{mn:.0} MiB/s ({rn:.0} reads/s)")));

        let out = dir.join("out.rvz");
        let from_source = |random: bool| {
            let mut src = if random {
                Source::file(&iso, "disc.iso").unwrap()
            } else {
                Source::sequential("disc.iso", plain_bytes, std::fs::File::open(&iso).unwrap())
            };
            dolphin_wrapper::convert_from_source(&rvz_args(Path::new("disc.iso"), &out), &mut src, |_, _, _, _| true, |_, _| true).unwrap();
        };
        let times = interleaved(
            ARCHIVE_ROUNDS,
            &mut [&mut || from_source(false), &mut || from_source(true), &mut || {
                dolphin_wrapper::convert(&rvz_args(&iso, &out), |_, _| true).unwrap();
            }],
        );
        for ((name, note), d) in [("archive seq src", " (spooled)"), ("archive random src", ""), ("archive from file", "")].iter().zip(times) {
            cells.push((name.to_string(), format!("{:.0} MiB/s{note}", mib_s(plain_bytes, d))));
        }
        rows.push(Row { format: "RVZ (zstd 5, 128K)".into(), sample: format!("re-encoded from {}", sample_label(&gc)), plain_bytes, cells });
    }

    // --- zip (deflate) and 7z (zstd): one member, the DVD image extracted above ---
    let payload = dir.join("dvd_plain/disc.iso");
    if payload.exists() {
        let plain_bytes = std::fs::metadata(&payload).unwrap().len();
        for (label, format, method, ext) in
            [("zip (deflate 5)", ArchiveFormat::Zip, CompressionMethod::Deflate, "zip"), ("7z (zstd 5)", ArchiveFormat::SevenZ, CompressionMethod::Zstd, "7z")]
        {
            let archive_path = dir.join(format!("payload.{ext}"));
            let mut cells = Vec::new();
            let entries = vec![CreateEntry { input_path: payload.clone(), archive_name: "disc.iso".into() }];
            let t = Instant::now();
            create_archive(&archive_path, &CreateArchiveArgs { format, method, level: 5, entries }, |_, _| true).unwrap();
            let from_file = t.elapsed();
            warm(&archive_path);

            let archive = SevenZipArchive::open_variant(&archive_path, format, LibVariant::Streaming, |_, _| true).unwrap();
            let t = Instant::now();
            let mut total = 0u64;
            archive
                .extract_stream(0, None, |_, _, c| {
                    total += c.len() as u64;
                    true
                }, |_, _| true)
                .unwrap();
            cells.push(("extract seq".into(), format!("{:.0} MiB/s", mib_s(total, t.elapsed()))));

            // Emulated random access: each read decodes from the start (or the last position).
            let reads = 8;
            let (m1, r1, _, _) = random_reads(|| EntryReader::open(&archive_path, format, 0).unwrap(), plain_bytes, read_len, reads, 1);
            cells.push(("extract random 1T".into(), format!("{m1:.2} MiB/s ({:.0} ms/read, emulated)", 1000.0 / r1)));

            let _ = from_file;
            let out = dir.join(format!("out.{ext}"));
            let from_sources = |random: bool| {
                let mut srcs = vec![if random {
                    Source::file(&payload, "disc.iso").unwrap()
                } else {
                    Source::sequential("disc.iso", plain_bytes, std::fs::File::open(&payload).unwrap())
                }];
                let _ = std::fs::remove_file(&out);
                create_archive_from_sources(&out, format, method, 5, &mut srcs, |_, _| true).unwrap();
            };
            let times = interleaved(
                ARCHIVE_ROUNDS,
                &mut [&mut || from_sources(false), &mut || from_sources(true), &mut || {
                    let _ = std::fs::remove_file(&out);
                    let entries = vec![CreateEntry { input_path: payload.clone(), archive_name: "disc.iso".into() }];
                    create_archive(&out, &CreateArchiveArgs { format, method, level: 5, entries }, |_, _| true).unwrap();
                }],
            );
            for (name, d) in ["archive seq src", "archive random src", "archive from file"].iter().zip(times) {
                cells.push((name.to_string(), format!("{:.0} MiB/s", mib_s(plain_bytes, d))));
            }
            rows.push(Row { format: label.into(), sample: "the CHD (DVD) image above, as one member".into(), plain_bytes, cells });
        }
    }

    // --- report ---
    let mut md = String::from("# Streaming benchmark: formats\n\n");
    md += &format!(
        "Release build, warm cache, {threads} logical CPUs. MiB/s are of plain (decoded) bytes. Random reads are 64 KiB at uniformly random offsets (2000 per thread; zip/7z: 8, emulated by decoding from the start). Archive columns: median of {ARCHIVE_ROUNDS} rounds, the three modes interleaved each round.\n\n"
    );
    let columns: Vec<String> = rows.first().map(|r| r.cells.iter().map(|(k, _)| k.clone()).collect()).unwrap_or_default();
    let mut all_columns = columns.clone();
    for r in &rows {
        for (k, _) in &r.cells {
            if !all_columns.contains(k) {
                all_columns.push(k.clone());
            }
        }
    }
    md += &format!("| format | sample | plain size | {} |\n", all_columns.join(" | "));
    md += &format!("|---|---|---|{}\n", "---|".repeat(all_columns.len()));
    for r in &rows {
        let cells: Vec<String> =
            all_columns.iter().map(|c| r.cells.iter().find(|(k, _)| k == c).map(|(_, v)| v.clone()).unwrap_or_else(|| "--".into())).collect();
        md += &format!("| {} | {} | {:.0} MiB | {} |\n", r.format, r.sample, r.plain_bytes as f64 / 1048576.0, cells.join(" | "));
    }
    write_report("formats", &md);
    println!("{md}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 2. CHD metadata per platform
// ---------------------------------------------------------------------------

/// Bytes of one output file of a chdman streaming extract, growing as it decodes, so a
/// parser can read from it while decoding continues. Stops the decode when dropped.
struct StreamedFile {
    shared: Arc<(Mutex<(Vec<u8>, bool)>, Condvar)>,
    stop: Arc<AtomicBool>,
    /// Every byte chdman decoded so far (all output files, not just the watched one).
    decoded: Arc<std::sync::atomic::AtomicU64>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl StreamedFile {
    fn start(command: ChdmanCommand, chd: &Path, sheet_out: PathBuf, split: bool, file_index: u32) -> Self {
        let shared = Arc::new((Mutex::new((Vec::new(), false)), Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let decoded = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (s, st, chd, dec) = (shared.clone(), stop.clone(), chd.to_path_buf(), decoded.clone());
        let worker = std::thread::spawn(move || {
            let _ = chdman_wrapper::run_stream(command, &chd_args(&chd, &sheet_out, split), false, |index, _, _, chunk| {
                if st.load(Ordering::Relaxed) || index > file_index {
                    return false;
                }
                dec.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                if index == file_index {
                    let (lock, cv) = &*s;
                    lock.lock().unwrap().0.extend_from_slice(chunk);
                    cv.notify_all();
                }
                true
            }, |_, _| {});
            let (lock, cv) = &*s;
            lock.lock().unwrap().1 = true;
            cv.notify_all();
        });
        Self { shared, stop, decoded, worker: Some(worker) }
    }

    /// Blocks until `offset + len` bytes have been decoded (or the file ended).
    fn read(&self, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        let (lock, cv) = &*self.shared;
        let end = offset as usize + len;
        let guard = cv.wait_while(lock.lock().unwrap(), |(data, done)| data.len() < end && !*done).unwrap();
        guard.0.get(offset as usize..end).map(|s| s.to_vec()).ok_or_else(|| "past end of streamed file".into())
    }

    /// Bytes decoded so far (after at least `min` are available, or the file ended).
    fn prefix(&self, min: usize) -> Vec<u8> {
        let (lock, cv) = &*self.shared;
        let guard = cv.wait_while(lock.lock().unwrap(), |(data, done)| data.len() < min && !*done).unwrap();
        guard.0[..min.min(guard.0.len())].to_vec()
    }
}

impl Drop for StreamedFile {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// PC Engine CD: the platform's method scans a prefix of the first data track (up to 1 MiB)
/// for its signature. Readers here grow the prefix (64 KiB doubling) until it's found.
fn pce_prefix(read_prefix: &mut dyn FnMut(usize) -> Vec<u8>) -> Option<Vec<u8>> {
    let mut len = 64 * 1024;
    loop {
        let bytes = read_prefix(len);
        if disc_dispatch::pce_cd_signature_offset(&bytes).is_some() {
            return Some(bytes);
        }
        if len >= 1 << 20 || bytes.len() < len {
            return None;
        }
        len *= 2;
    }
}

fn signature_ok(platform: Platform, system_id: Option<String>) -> bool {
    let id = system_id.unwrap_or_default();
    match platform {
        Platform::PlayStationPortable => id.starts_with("PSP GAME"),
        _ => id.starts_with("PLAYSTATION"),
    }
}

type Found = (Option<String>, Option<String>);

/// One measurement: `(signature confirmed, title/serial)` with a fresh reader each.
fn measure(reps: usize, mut f: impl FnMut() -> (bool, Found)) -> (Duration, bool, Found) {
    let mut times = Vec::new();
    let mut last = (false, (None, None));
    for _ in 0..reps {
        let t = Instant::now();
        last = f();
        times.push(t.elapsed());
    }
    (median(times), last.0, last.1)
}

#[test]
#[ignore]
fn chd_metadata() {
    if !streaming_builds_present() {
        eprintln!("streaming builds missing -- skipping");
        return;
    }
    let dir = root().join("metadata");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let samples = chd_samples();
    let cases = [
        ("PC Engine CD", Platform::TurboGrafxCd, true),
        ("PSP", Platform::PlayStationPortable, false),
        ("PS1", Platform::PlayStation, true),
        ("PS2 CD", Platform::PlayStation2, true),
        ("PS2 DVD", Platform::PlayStation2, false),
    ];
    let reps = 5;
    let mut md = String::from("# Streaming benchmark: CHD metadata per platform\n\n");
    md += "Release build, warm cache, median of 5. Each cell: time to confirm the disc signature / time to extract title+serial (each timed from scratch, including opening). ";
    md += "Signature: PC Engine CD = its signature string in the first data track; PS1/PS2 = ISO9660 system id \"PLAYSTATION\"; PSP = \"PSP GAME\". ";
    md += "Title/serial: the platform's own method (PCE title+serial bytes; PS1 SYSTEM.CNF BOOT; PS2 volume id + SYSTEM.CNF BOOT2; PSP PARAM.SFO).\n\n";
    md += "| platform | sample | CHD size | extract with chdman (1 thread) then parse | sequential stream (stops when found) | random access (sector reads) | title | serial |\n";
    md += "|---|---|---|---|---|---|---|---|\n";

    for (label, platform, want_cd) in cases {
        let Some(sample) = samples.iter().find(|s| s.platform == platform && s.info.is_cd == want_cd) else {
            md += &format!("| {label} | (no sample) | | | | | | |\n");
            continue;
        };
        warm(&sample.path);
        let chd = sample.path.clone();
        let (track_index, track) = if want_cd { first_data_track(&ChdReader::open(&chd, None, 0).unwrap()) } else { (0, dummy_track()) };
        let (command, sheet) = if want_cd { (ChdmanCommand::ExtractCd, "disc.cue") } else { (ChdmanCommand::ExtractDvd, "disc.iso") };

        // --- extract (chdman, 1 thread) then parse the file ---
        let ex_dir = dir.join("extract");
        let data_file = |ex_dir: &Path| -> PathBuf {
            if !want_cd {
                return ex_dir.join(sheet);
            }
            let mut files: Vec<PathBuf> = std::fs::read_dir(ex_dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "bin")).collect();
            files.sort();
            files[track_index as usize].clone()
        };
        let extract_then = |which: u8| -> (bool, Found) {
            let _ = std::fs::remove_dir_all(&ex_dir);
            std::fs::create_dir_all(&ex_dir).unwrap();
            chdman_wrapper::run(command, &chd_args(&chd, &ex_dir.join(sheet), want_cd), |_, _| {}).unwrap();
            let file = data_file(&ex_dir);
            let mut reader = |o: u64, l: usize| disc_fs::read_exact_at(&file, o, l);
            parse(platform, which, &mut reader, &mut |n| {
                let mut bytes = Vec::with_capacity(n);
                let _ = std::io::Read::read_to_end(&mut std::io::Read::take(std::fs::File::open(&file).unwrap(), n as u64), &mut bytes);
                bytes
            })
        };
        let (ex_sig, _, _) = measure(reps.min(3), || extract_then(0));
        let (ex_meta, _, _) = measure(reps.min(3), || extract_then(1));

        // --- sequential stream, stopping as soon as the parser is satisfied ---
        let stream_dir = dir.join("stream");
        std::fs::create_dir_all(&stream_dir).unwrap();
        let stream_then = |which: u8| -> (bool, Found) {
            let streamed = StreamedFile::start(command, &chd, stream_dir.join(sheet), want_cd, if want_cd { track_index } else { 0 });
            let mut reader = |o: u64, l: usize| streamed.read(o, l);
            let result = parse(platform, which, &mut reader, &mut |n| streamed.prefix(n));
            drop(streamed);
            result
        };
        let (st_sig, _, _) = measure(reps, || stream_then(0));
        let (st_meta, _, _) = measure(reps, || stream_then(1));

        // --- random access ---
        let random_then = |which: u8| -> (bool, Found) {
            let mut chd_reader = ChdReader::open(&chd, None, 8).unwrap();
            if want_cd {
                let mut reader = cd_track_reader(&mut chd_reader, track);
                let mut prefix_reader = |n: usize| {
                    let mut r2 = ChdReader::open(&chd, None, 8).unwrap();
                    let mut rd = cd_track_reader(&mut r2, track);
                    let max = track.frames as u64 * track.data_size as u64;
                    rd(0, (n as u64).min(max) as usize).unwrap_or_default()
                };
                parse(platform, which, &mut reader, &mut prefix_reader)
            } else {
                let size = sample.info.logical_bytes;
                let mut reader = |o: u64, l: usize| {
                    if o + l as u64 > size {
                        return Err("past end".to_string());
                    }
                    let mut buf = vec![0u8; l];
                    chd_reader.read_bytes(o, &mut buf).map_err(|e| e.to_string()).map(|_| buf)
                };
                parse(platform, which, &mut reader, &mut |_| Vec::new())
            }
        };
        let (rd_sig, sig_ok, _) = measure(reps, || random_then(0));
        let (rd_meta, _, found) = measure(reps, || random_then(1));

        let cell = |s: Duration, m: Duration| format!("{:.2} ms / {:.2} ms", ms(s), ms(m));
        md += &format!(
            "| {label} | {} | {:.0} MiB | {} | {} | {} | {} | {} |\n",
            sample_label(&sample.path),
            std::fs::metadata(&sample.path).unwrap().len() as f64 / 1048576.0,
            cell(ex_sig, ex_meta),
            cell(st_sig, st_meta),
            cell(rd_sig, rd_meta),
            found.0.as_deref().unwrap_or("--"),
            found.1.as_deref().unwrap_or("--"),
        );
        if !sig_ok {
            md += &format!("|   | signature NOT confirmed for {label} | | | | | | |\n");
        }
    }
    write_report("chd_metadata", &md);
    println!("{md}");
    let _ = std::fs::remove_dir_all(&dir);
}

fn dummy_track() -> ChdTrack {
    ChdTrack {
        track_type: 0,
        sub_type: 0,
        data_size: 2048,
        sub_size: 0,
        frames: 0,
        pregap: 0,
        postgap: 0,
        session: 0,
        logical_start: 0,
        physical_start: 0,
        chd_frame_start: 0,
    }
}

/// which = 0: confirm the signature only; 1: extract title/serial (the full method).
fn parse<R: FnMut(u64, usize) -> Result<Vec<u8>, String>>(
    platform: Platform,
    which: u8,
    reader: &mut R,
    read_prefix: &mut dyn FnMut(usize) -> Vec<u8>,
) -> (bool, Found) {
    if platform == Platform::TurboGrafxCd {
        let Some(bytes) = pce_prefix(read_prefix) else { return (false, (None, None)) };
        if which == 0 {
            return (true, (None, None));
        }
        let (title, serial, _) = disc_dispatch::disc_metadata_from_bytes(&bytes, platform, false);
        return (true, (title, serial));
    }
    if which == 0 {
        let id = disc_fs::read_iso_system_id_with(reader);
        return (signature_ok(platform, id), (None, None));
    }
    match disc_dispatch::disc_metadata_with_reader(reader, platform) {
        Some((title, serial, _)) => (true, (title, serial)),
        None => (false, (None, None)),
    }
}

fn write_report(name: &str, md: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/results");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("benchmark_streaming_{name}.md")), md).unwrap();
}

// ---------------------------------------------------------------------------
// 3. Thread scaling: CHD/RVZ extract, RVZ archive, CHD metadata
// ---------------------------------------------------------------------------

const THREADS: [usize; 9] = [1, 2, 3, 4, 6, 8, 12, 16, 20];

/// Parallel decode (`rom::parallel`) of a whole container, `threads` readers:
/// in-order into a sequentially written file, or random writes straight to offsets.
fn decode_to_file<R: ReadAt, O: Fn() -> std::io::Result<R> + Sync>(open: O, size: u64, block: usize, threads: usize, out: &Path, in_order: bool) {
    use romexplorer::rom::parallel::{parallel_decode, Delivery};
    use std::io::Write;
    if in_order {
        let mut file = std::io::BufWriter::with_capacity(4 << 20, std::fs::File::create(out).unwrap());
        let mut on_block = |_: u64, bytes: &[u8]| file.write_all(bytes).is_ok();
        assert!(parallel_decode(open, size, block, threads, Delivery::InOrder(&mut on_block)).unwrap());
        file.flush().unwrap();
    } else {
        parallel_decode(open, size, block, threads, Delivery::ToFile(out)).unwrap();
    }
}

/// Parallel decode without touching any file: in order with CRC32 on every byte (hashing while
/// extracting), or unordered (pure decode).
fn decode_only<R: ReadAt, O: Fn() -> std::io::Result<R> + Sync>(open: O, size: u64, block: usize, threads: usize, in_order: bool) {
    use romexplorer::rom::parallel::{parallel_decode, Delivery};
    if in_order {
        let mut crc = crc32fast::Hasher::new();
        let mut on_block = |_: u64, bytes: &[u8]| {
            crc.update(bytes);
            true
        };
        assert!(parallel_decode(open, size, block, threads, Delivery::InOrder(&mut on_block)).unwrap());
        std::hint::black_box(crc.finalize());
    } else {
        let sink = |_: u64, bytes: &[u8]| {
            std::hint::black_box(bytes.len());
            true
        };
        assert!(parallel_decode(open, size, block, threads, Delivery::Unordered(&sink)).unwrap());
    }
}

#[test]
#[ignore]
fn scaling() {
    if !streaming_builds_present() {
        eprintln!("streaming builds missing -- skipping");
        return;
    }
    let (dir, in_ram) = ram_scratch();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cpus = native_lib::recommended_reader_threads();
    let samples = chd_samples();
    let mut md = String::from("# Streaming benchmark: thread scaling (CHD / RVZ)\n\n");
    md += &format!(
        "Release build, warm cache, {cpus} logical CPUs (6 P-cores with HT + 8 E-cores). Thread counts above the CPU count are not run. \
         MiB/s are of decoded bytes. Parallel decode (`rom::parallel`, one reader per thread): `seq` = delivered strictly in order, `random` = in completion order. \
         `decode` rows write nothing (seq also CRC32s every byte, i.e. hashing while extracting); `-> file` rows write the full image \
         (seq front to back, random at each block's offset) to {}. \
         `native` = the library's own single-threaded path (chdman / Dolphin), decode-only and to file. Inputs are read from the page cache.\n\n",
        if in_ram { "a tmpfs (RAM disk), so the disk's write speed is out of the picture" } else { "the output directory on disk" }
    );
    let threads_list: Vec<usize> = THREADS.iter().copied().filter(|&t| t <= cpus).collect();
    let out = dir.join("out.bin");

    // --- extract: CHD (CD), CHD (DVD), RVZ ---
    md += "## Extract\n\nCHD output here is the CHD's logical data (for a CD: raw 2448-byte frames incl. subcode, not cue/bin), i.e. the full decode work.\n\n";
    md += &format!("| container | sample | size | native 1T | mode | {} |\n", threads_list.iter().map(|t| format!("{t}T")).collect::<Vec<_>>().join(" | "));
    md += &format!("|---|---|---|---|---|{}\n", "---|".repeat(threads_list.len()));
    let mut extract_cases: Vec<(String, String, u64, usize, Box<dyn Fn() -> Box<dyn ReadAt> + Sync>, Box<dyn Fn(bool)>)> = Vec::new();
    for (label, platform, want_cd) in [("CHD (CD)", Platform::TurboGrafxCd, true), ("CHD (DVD)", Platform::PlayStation2, false)] {
        let Some(sample) = samples.iter().find(|s| s.platform == platform && s.info.is_cd == want_cd) else { continue };
        warm(&sample.path);
        let block = ((4 << 20) / sample.info.hunk_bytes as usize).max(1) * sample.info.hunk_bytes as usize;
        let (path, native_dir) = (sample.path.clone(), dir.join("native"));
        let (cmd, sheet) = if want_cd { (ChdmanCommand::ExtractCd, "disc.cue") } else { (ChdmanCommand::ExtractDvd, "disc.iso") };
        let p2 = path.clone();
        extract_cases.push((
            label.into(),
            sample_label(&sample.path),
            sample.info.logical_bytes,
            block,
            Box::new(move || Box::new(ChdReader::open(&path, None, 0).unwrap()) as Box<dyn ReadAt>),
            Box::new(move |to_file: bool| {
                std::fs::create_dir_all(&native_dir).unwrap();
                chdman_wrapper::run_stream(cmd, &chd_args(&p2, &native_dir.join(sheet), false), to_file, |_, _, _, _| true, |_, _| {}).unwrap();
            }),
        ));
    }
    let gc_dir = support::inputs::rows("disc")
        .find(|r| r.len() >= 2 && support::inputs::platform(&r[1]) == Some(Platform::GameCube))
        .map(|r| support::inputs::samples_root().join(&r[0]));
    let gc = gc_dir.and_then(|d| {
        let mut rvz: Vec<PathBuf> = std::fs::read_dir(d).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "rvz")).collect();
        rvz.sort_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0));
        rvz.into_iter().next()
    });
    let user_dir = dir.join("dolphin_user");
    let gc_iso = dir.join("gc.iso");
    let zstd_rvz = dir.join("zstd.rvz");
    let rvz_args = |input: &Path, out: &Path| {
        DolphinConvertArgs::new(input, out, DiscFormat::Rvz).with_user_dir(&user_dir).with_block_size(131072).with_compression(DiscCompression::Zstd, 5)
    };
    if let Some(gc) = &gc {
        dolphin_wrapper::convert(&DolphinConvertArgs::new(gc, &gc_iso, DiscFormat::Iso).with_user_dir(&user_dir), |_, _| true).unwrap();
        dolphin_wrapper::convert(&rvz_args(&gc_iso, &zstd_rvz), |_, _| true).unwrap();
        warm(&zstd_rvz);
        let size = std::fs::metadata(&gc_iso).unwrap().len();
        let (r1, r2, u1, u2, o2) = (zstd_rvz.clone(), zstd_rvz.clone(), user_dir.clone(), user_dir.clone(), dir.join("native.iso"));
        extract_cases.push((
            "RVZ (zstd 5, 128K)".into(),
            format!("re-encoded from {}", sample_label(gc)),
            size,
            2 << 20,
            Box::new(move || Box::new(DiscReader::open(&r1, Some(&u1)).unwrap()) as Box<dyn ReadAt>),
            Box::new(move |to_file: bool| {
                let out = to_file.then_some(o2.as_path());
                dolphin_wrapper::extract_stream(&r2, out, Some(&u2), |_, _, _, _| true, |_, _| true).unwrap();
            }),
        ));
    }
    for (label, sample, size, block, open, native) in &extract_cases {
        let native_times = interleaved(2, &mut [&mut || native(false), &mut || native(true)]);
        let mut cells = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for &t in &threads_list {
            let times = interleaved(
                2,
                &mut [
                    &mut || decode_only(|| Ok(open()), *size, *block, t, true),
                    &mut || decode_only(|| Ok(open()), *size, *block, t, false),
                    &mut || decode_to_file(|| Ok(open()), *size, *block, t, &out, true),
                    &mut || decode_to_file(|| Ok(open()), *size, *block, t, &out, false),
                ],
            );
            for (cell, d) in cells.iter_mut().zip(times) {
                cell.push(format!("{:.0}", mib_s(*size, d)));
            }
        }
        let native_cells = [
            format!("{:.0} MiB/s decode", mib_s(*size, native_times[0])),
            String::new(),
            format!("{:.0} MiB/s -> file", mib_s(*size, native_times[1])),
            String::new(),
        ];
        let modes = ["seq decode+CRC", "random decode", "seq -> file", "random -> file"];
        for (i, mode) in modes.iter().enumerate() {
            let head = if i == 0 { format!("| {label} | {sample} | {:.0} MiB |", *size as f64 / 1048576.0) } else { "| | | |".to_string() };
            md += &format!("{head} {} | {mode} | {} |\n", native_cells[i], cells[i].join(" | "));
        }
        println!("extract {label} done");
    }
    let _ = std::fs::remove_dir_all(dir.join("native"));
    let _ = std::fs::remove_file(dir.join("native.iso"));

    // --- RVZ archive: compressor threads ---
    if gc.is_some() {
        let size = std::fs::metadata(&gc_iso).unwrap().len();
        warm(&gc_iso);
        md += "\n## RVZ archive (ISO -> RVZ zstd 5, 128 KiB blocks)\n\n";
        md += "Threads = Dolphin compressor threads (`compress_threads`). `random` = random-access source (read in place); `seq` = sequential source (spooled to a temp file first).\n\n";
        md += &format!("| source | {} |\n|---|{}\n", threads_list.iter().map(|t| format!("{t}T")).collect::<Vec<_>>().join(" | "), "---|".repeat(threads_list.len()));
        let mut rows = [Vec::new(), Vec::new()];
        let rvz_out = dir.join("out.rvz");
        for &t in &threads_list {
            let run = |random: bool| {
                let mut src = if random {
                    Source::file(&gc_iso, "disc.iso").unwrap()
                } else {
                    Source::sequential("disc.iso", size, std::fs::File::open(&gc_iso).unwrap())
                };
                let args = rvz_args(Path::new("disc.iso"), &rvz_out).with_compress_threads(t as u32);
                dolphin_wrapper::convert_from_source(&args, &mut src, |_, _, _, _| true, |_, _| true).unwrap();
            };
            let times = interleaved(3, &mut [&mut || run(true), &mut || run(false)]);
            rows[0].push(format!("{:.0} MiB/s", mib_s(size, times[0])));
            rows[1].push(format!("{:.0} MiB/s", mib_s(size, times[1])));
        }
        md += &format!("| random | {} |\n| seq (spooled) | {} |\n", rows[0].join(" | "), rows[1].join(" | "));
        println!("rvz archive done");
    }

    // --- CHD metadata ---
    md += "\n## CHD metadata\n\n";
    md += "Per platform, the platform's own method via `detect::chd_disc` (random access), plus the sequential stream (chdman decode that stops once the parser has what it needs; one chdman call at a time, so 1T only). \
           Random NT: N threads each run 20 extractions concurrently (fresh open each, which includes decoding the CHD's hunk map). Cells: median latency / throughput (extractions per second) / disc bytes the parser read per extraction / speed (those bytes per second, all threads; for the sequential stream: everything chdman decoded until the parser was satisfied). \
           `sig` = signature confirmed; `meta` = title+serial extracted.\n\n";
    let cases = [
        ("PC Engine CD", Platform::TurboGrafxCd, true),
        ("PSP", Platform::PlayStationPortable, false),
        ("PS1", Platform::PlayStation, true),
        ("PS2 CD", Platform::PlayStation2, true),
        ("PS2 DVD", Platform::PlayStation2, false),
    ];
    let meta_dir = dir.join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    for (label, platform, want_cd) in cases {
        let Some(sample) = samples.iter().find(|s| s.platform == platform && s.info.is_cd == want_cd) else { continue };
        warm(&sample.path);
        let chd = sample.path.clone();
        md += &format!("### {label} ({}, {:.0} MiB)\n\n", sample_label(&chd), std::fs::metadata(&chd).unwrap().len() as f64 / 1048576.0);
        md += "| access | threads | sig latency | sig ops/s | sig read | sig speed | meta latency | meta ops/s | meta read | meta speed |\n|---|---|---|---|---|---|---|---|---|---|\n";

        // sequential stream, 1T
        let (track_index, _) = if want_cd { first_data_track(&ChdReader::open(&chd, None, 0).unwrap()) } else { (0, dummy_track()) };
        let (command, sheet) = if want_cd { (ChdmanCommand::ExtractCd, "disc.cue") } else { (ChdmanCommand::ExtractDvd, "disc.iso") };
        let stream_once = |which: u8| -> (Duration, u64) {
            let t = Instant::now();
            let streamed = StreamedFile::start(command, &chd, meta_dir.join(sheet), want_cd, if want_cd { track_index } else { 0 });
            let mut reader = |o: u64, l: usize| streamed.read(o, l);
            let (ok, _) = parse(platform, which, &mut reader, &mut |n| streamed.prefix(n));
            let elapsed = t.elapsed();
            let bytes = streamed.decoded.load(Ordering::Relaxed);
            drop(streamed);
            assert!(ok, "{label}: sequential {}", if which == 0 { "signature" } else { "metadata" });
            (elapsed, bytes)
        };
        let seq: Vec<(Duration, u64)> = (0..2).flat_map(|which| {
            let runs: Vec<(Duration, u64)> = (0..3).map(|_| stream_once(which)).collect();
            let d = median(runs.iter().map(|r| r.0).collect());
            let bytes = runs[0].1;
            [(d, bytes)]
        }).collect();
        let size_str = |bytes: f64| if bytes >= 1048576.0 { format!("{:.1} MiB", bytes / 1048576.0) } else { format!("{:.1} KiB", bytes / 1024.0) };
        let speed_str = |bytes: u64, d: Duration| {
            let v = mib_s(bytes, d);
            if v >= 10.0 { format!("{v:.0} MiB/s") } else { format!("{v:.2} MiB/s") }
        };
        let cell = |d: Duration, bytes: u64, ops: f64| format!("{:.2} ms | {ops:.1} | {} | {}", ms(d), size_str(bytes as f64), speed_str(bytes, d));
        md += &format!("| seq stream | 1 | {} | {} |\n", cell(seq[0].0, seq[0].1, 1.0 / seq[0].0.as_secs_f64()), cell(seq[1].0, seq[1].1, 1.0 / seq[1].0.as_secs_f64()));

        // random, N threads
        use romexplorer::rom::detect::chd_disc::{disc_metadata, disc_signature, ChdDisc};
        for &t in &threads_list {
            let per_thread = 20;
            let run = |which: u8| -> (Duration, f64, u64, Duration) {
                let lat = Mutex::new(Vec::new());
                let bytes = std::sync::atomic::AtomicU64::new(0);
                let wall = Instant::now();
                std::thread::scope(|s| {
                    for _ in 0..t {
                        s.spawn(|| {
                            for _ in 0..per_thread {
                                let start = Instant::now();
                                let mut disc = ChdDisc::open(&chd).unwrap();
                                let ok = if which == 0 { disc_signature(&mut disc, platform) == Some(true) } else { disc_metadata(&mut disc, platform).is_some() };
                                assert!(ok);
                                lat.lock().unwrap().push(start.elapsed());
                                bytes.fetch_add(disc.bytes_read(), Ordering::Relaxed);
                            }
                        });
                    }
                });
                let wall = wall.elapsed();
                let n = (t * per_thread) as f64;
                (median(lat.into_inner().unwrap()), n / wall.as_secs_f64(), bytes.into_inner(), wall)
            };
            let (sl, sops, sbytes, swall) = run(0);
            let (ml, mops, mbytes, mwall) = run(1);
            let n = (t * per_thread) as f64;
            md += &format!(
                "| random | {t} | {:.2} ms | {sops:.0} | {} | {} | {:.2} ms | {mops:.0} | {} | {} |\n",
                ms(sl),
                size_str(sbytes as f64 / n),
                speed_str(sbytes, swall),
                ms(ml),
                size_str(mbytes as f64 / n),
                speed_str(mbytes, mwall)
            );
        }
        md += "\n";
        println!("metadata {label} done");
    }

    write_report("scaling", &md);
    println!("{md}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 4. Hash strategy: hash while extracting (one pass) vs extract-then-hash (two passes,
//    to tmpfs or to disk)
// ---------------------------------------------------------------------------

/// Arithmetic mean -- `hash_strategy_chd_cd`'s sweep is asked for the average of 3 runs,
/// unlike every other benchmark here, which takes the median.
fn mean(v: &[Duration]) -> Duration {
    v.iter().sum::<Duration>() / v.len() as u32
}

fn fmt_speed(d: Duration, bytes: u64) -> String {
    format!("{:.0} ms ({:.0} MiB/s)", ms(d), mib_s(bytes, d))
}

/// tmpfs (RAM) scratch dir under `/dev/shm` if usable, and the normal on-disk output
/// directory (an SSD on this machine) -- both exist together for the whole test, unlike
/// `ram_scratch` (used by `scaling`), which picks one or the other.
fn hash_bench_scratch(name: &str) -> (Option<PathBuf>, PathBuf) {
    let disk = root().join(name);
    let _ = std::fs::remove_dir_all(&disk);
    std::fs::create_dir_all(&disk).unwrap();
    let shm = Path::new("/dev/shm");
    let tmpfs = if shm.is_dir() {
        let dir = shm.join(format!("romexplorer_bench_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok().map(|_| dir)
    } else {
        None
    };
    (tmpfs, disk)
}

/// Pure decode + CRC32, delivered in order, nothing written to disk. Thin wrapper around
/// the production `rom::parallel::hash_while_decoding`, so this benchmark exercises the
/// same code path real callers get.
fn hash_while_extracting<R: ReadAt, O: Fn() -> std::io::Result<R> + Sync>(open: O, size: u64, block: usize, threads: usize) -> (Duration, String) {
    use romexplorer::rom::hash::{HashDigest, HashFlags};
    use romexplorer::rom::parallel::hash_while_decoding;
    let digest = HashDigest::new(HashFlags::CRC32);
    let t = Instant::now();
    assert!(hash_while_decoding(open, size, block, threads, &digest).unwrap());
    (t.elapsed(), digest.finish().crc32.expect("crc32"))
}

/// Decode in order, writing each block to `out` AND updating a CRC32 digest in the same
/// pass, timed as a single span. Thin wrapper around the production, DEFAULT
/// `rom::parallel::extract_and_hash_to_file`.
fn decode_and_hash_to_file<R: ReadAt, O: Fn() -> std::io::Result<R> + Sync>(open: O, size: u64, block: usize, threads: usize, out: &Path) -> (Duration, String) {
    use romexplorer::rom::hash::{HashDigest, HashFlags};
    use romexplorer::rom::parallel::extract_and_hash_to_file;
    let digest = HashDigest::new(HashFlags::CRC32);
    let t = Instant::now();
    assert!(extract_and_hash_to_file(open, size, block, threads, out, &digest).unwrap());
    (t.elapsed(), digest.finish().crc32.expect("crc32"))
}

/// Extracts to `out` (parallel decode, unordered/fastest writes), then hashes the written
/// file in a separate pass (`hash::hash_file_other` -- the call real "verify what I just
/// extracted" code would make). Extract and hash timed separately.
fn extract_then_hash<R: ReadAt, O: Fn() -> std::io::Result<R> + Sync>(
    open: O, size: u64, block: usize, threads: usize, out: &Path,
) -> (Duration, Duration, String) {
    use romexplorer::rom::hash::{hash_file_other, HashFlags};
    let t = Instant::now();
    decode_to_file(open, size, block, threads, out, false);
    let extract = t.elapsed();
    let t2 = Instant::now();
    let report = hash_file_other(out, HashFlags::CRC32, None, |_, _| {}).unwrap();
    (extract, t2.elapsed(), report.crc32.expect("crc32"))
}

/// Same as [`extract_then_hash`], but extract+hash are timed as one combined span (used by
/// `hash_strategy_chd_cd`, where each of the 32 combinations is meant to be one number).
fn extract_then_hash_combined<R: ReadAt, O: Fn() -> std::io::Result<R> + Sync>(
    open: O, size: u64, block: usize, threads: usize, out: &Path,
) -> (Duration, String) {
    use romexplorer::rom::hash::{hash_file_other, HashFlags};
    let t = Instant::now();
    decode_to_file(open, size, block, threads, out, false);
    let report = hash_file_other(out, HashFlags::CRC32, None, |_, _| {}).unwrap();
    (t.elapsed(), report.crc32.expect("crc32"))
}

/// RVZ, 2T/4T/20T x {hash while extracting, extract to tmpfs then hash, extract to SSD
/// then hash} = 9 configurations. Every column computes one CRC32 over the whole plain
/// image via the same `HashDigest` primitive -- checks whether splitting extract/hash into
/// two passes (either scratch target) ever beats the integrated single pass, and what RVZ's
/// own best decode-thread count is.
#[test]
#[ignore]
fn hash_strategy_rvz() {
    if !streaming_builds_present() {
        eprintln!("streaming builds missing -- skipping");
        return;
    }
    let (tmpfs, disk) = hash_bench_scratch("hash_strategy_rvz");
    let user_dir = disk.join("dolphin_user");
    let cpus = native_lib::recommended_reader_threads();

    let gc_dir = support::inputs::rows("disc")
        .find(|r| r.len() >= 2 && support::inputs::platform(&r[1]) == Some(Platform::GameCube))
        .map(|r| support::inputs::samples_root().join(&r[0]));
    let Some(gc) = gc_dir.and_then(|d| {
        let mut rvz: Vec<PathBuf> = std::fs::read_dir(d).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "rvz")).collect();
        rvz.sort_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0));
        rvz.into_iter().next()
    }) else {
        eprintln!("no RVZ sample -- skipping");
        return;
    };
    let gc_iso = disk.join("gc.iso");
    dolphin_wrapper::convert(&DolphinConvertArgs::new(&gc, &gc_iso, DiscFormat::Iso).with_user_dir(&user_dir), |_, _| true).unwrap();
    let zstd_rvz = disk.join("zstd.rvz");
    dolphin_wrapper::convert(
        &DolphinConvertArgs::new(&gc_iso, &zstd_rvz, DiscFormat::Rvz).with_user_dir(&user_dir).with_block_size(131072).with_compression(DiscCompression::Zstd, 5),
        |_, _| true,
    )
    .unwrap();
    warm(&zstd_rvz);
    let size = std::fs::metadata(&gc_iso).unwrap().len();
    let block = 2usize << 20;
    let open = || DiscReader::open(&zstd_rvz, Some(&user_dir)).map_err(|e| std::io::Error::other(e.to_string()));

    const ROUNDS: usize = 3;
    let mut md = String::from("# Hash strategy: RVZ\n\n");
    md += &format!(
        "Release build, warm cache, {cpus} logical CPUs, median of {ROUNDS} rounds (3 thread counts x 3 hashing strategies = 9 configurations; \
         each round runs all columns for a given thread count back to back, in the order shown, so thermal/turbo drift affects them alike). \
         Every column computes exactly one CRC32 over the whole plain ({:.0} MiB) image using the same `HashDigest` primitive (the one `hash::hash_file_other` uses) -- \
         only how the bytes reach it differs. `hash while extracting` never touches disk (pure parallel decode, in order, hashed as it arrives). \
         `extract to X then hash` extracts first (parallel decode, fastest/unordered writes) and then runs a normal, separate `hash::hash_file_other` pass over the written file; \
         total = extract + hash medians (each measured on its own, not as one combined span). {}\n\n",
        size as f64 / 1048576.0,
        if tmpfs.is_some() { "tmpfs is a RAM disk (`/dev/shm`); ssd is this machine's normal on-disk output directory." } else { "tmpfs unavailable on this machine (no `/dev/shm`) -- ssd column only." }
    );
    md += "| threads | hash while extracting | tmpfs extract | tmpfs hash | tmpfs total | ssd extract | ssd hash | ssd total |\n";
    md += "|---|---|---|---|---|---|---|---|\n";

    for &t in &[2usize, 4, 20] {
        let threads = t.min(cpus);
        let mut hw = Vec::new();
        let (mut tf_ex, mut tf_h) = (Vec::new(), Vec::new());
        let (mut ss_ex, mut ss_h) = (Vec::new(), Vec::new());
        let mut crcs: Vec<String> = Vec::new();
        let ssd_out = disk.join("out.iso");
        for _ in 0..ROUNDS {
            let (d, crc) = hash_while_extracting(|| open(), size, block, threads);
            hw.push(d);
            crcs.push(crc);
            if let Some(tmpfs_dir) = &tmpfs {
                let out = tmpfs_dir.join("out.iso");
                let (e, h, crc) = extract_then_hash(|| open(), size, block, threads, &out);
                tf_ex.push(e);
                tf_h.push(h);
                crcs.push(crc);
            }
            let (e, h, crc) = extract_then_hash(|| open(), size, block, threads, &ssd_out);
            ss_ex.push(e);
            ss_h.push(h);
            crcs.push(crc);
        }
        assert!(crcs.windows(2).all(|w| w[0] == w[1]), "{threads}T: CRC32 disagreement between hash strategies: {crcs:?}");

        let hw_m = median(hw);
        let tmpfs_cell = if tmpfs.is_some() {
            let (e, h) = (median(tf_ex), median(tf_h));
            format!("{} | {} | {}", fmt_speed(e, size), fmt_speed(h, size), fmt_speed(e + h, size))
        } else {
            "-- | -- | --".to_string()
        };
        let (se, sh) = (median(ss_ex), median(ss_h));
        md += &format!("| {threads} | {} | {tmpfs_cell} | {} | {} | {} |\n", fmt_speed(hw_m, size), fmt_speed(se, size), fmt_speed(sh, size), fmt_speed(se + sh, size));
        println!("rvz hash strategy {threads}T done");
    }
    write_report("hash_strategy_rvz", &md);
    println!("{md}");
    let _ = std::fs::remove_dir_all(&disk);
    if let Some(d) = &tmpfs {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// CHD (DVD/raw, the largest sample available), fixed 16 threads (`scaling.md` showed more
/// threads keep helping CHD, but 16 is a reasonable per-file cap) x {extract then hash,
/// extract while hashing} x {tmpfs, ssd} = 6 measurements (extract-then-hash is phase-split
/// into its extract and hash times, 2 per target; extract-while-hashing is one fused pass
/// that also writes the file, 1 per target).
#[test]
#[ignore]
fn hash_strategy_chd_dvd() {
    if !streaming_builds_present() {
        eprintln!("streaming builds missing -- skipping");
        return;
    }
    let (tmpfs, disk) = hash_bench_scratch("hash_strategy_chd_dvd");
    let cpus = native_lib::recommended_reader_threads();
    let threads = 16usize.min(cpus);

    let samples = chd_samples();
    let Some(sample) = samples.iter().filter(|s| !s.info.is_cd).max_by_key(|s| s.info.logical_bytes) else {
        eprintln!("no DVD/raw CHD sample -- skipping");
        return;
    };
    warm(&sample.path);
    let chd = sample.path.clone();
    let size = sample.info.logical_bytes;
    let block = ((4 << 20) / sample.info.hunk_bytes as usize).max(1) * sample.info.hunk_bytes as usize;
    let open = || ChdReader::open(&chd, None, 0).map_err(|e| std::io::Error::other(e.to_string()));

    const ROUNDS: usize = 3;
    let mut md = String::from("# Hash strategy: CHD (DVD/raw)\n\n");
    md += &format!(
        "Release build, warm cache, {threads} threads (fixed), median of {ROUNDS} rounds. Sample: {} ({:.0} MiB compressed, {:.0} MiB decoded). \
         `extract then hash`: parallel decode to a file (fastest/unordered writes), then a separate `hash::hash_file_other` CRC32 pass over it -- extract and hash timed separately. \
         `extract while hashing`: one parallel decode pass, in order, that both writes the file AND updates the CRC32 digest as each block arrives -- one number, and the file is produced too. \
         Every mode computes the same single CRC32 over the whole plain image, same `HashDigest` primitive throughout. {}\n\n",
        sample_label(&chd),
        std::fs::metadata(&chd).unwrap().len() as f64 / 1048576.0,
        size as f64 / 1048576.0,
        if tmpfs.is_some() { "tmpfs is a RAM disk (`/dev/shm`); ssd is the normal on-disk output directory." } else { "tmpfs unavailable on this machine -- ssd only." }
    );
    md += "| target | extract then hash: extract | extract then hash: hash | extract then hash: total | extract while hashing |\n";
    md += "|---|---|---|---|---|\n";

    for (target_name, target_dir) in [("tmpfs", tmpfs.clone()), ("ssd", Some(disk.clone()))] {
        let Some(target_dir) = target_dir else {
            md += &format!("| {target_name} | -- | -- | -- | -- |\n");
            continue;
        };
        let out_a = target_dir.join("extract_then_hash.bin");
        let out_b = target_dir.join("extract_while_hash.bin");
        let (mut ex, mut h, mut combined) = (Vec::new(), Vec::new(), Vec::new());
        let mut crcs = Vec::new();
        for _ in 0..ROUNDS {
            let (e, ht, crc) = extract_then_hash(|| open(), size, block, threads, &out_a);
            ex.push(e);
            h.push(ht);
            crcs.push(crc);
            let (d, crc) = decode_and_hash_to_file(|| open(), size, block, threads, &out_b);
            combined.push(d);
            crcs.push(crc);
        }
        assert!(crcs.windows(2).all(|w| w[0] == w[1]), "{target_name}: CRC32 disagreement: {crcs:?}");
        let (em, hm, cm) = (median(ex), median(h), median(combined));
        md += &format!("| {target_name} | {} | {} | {} | {} |\n", fmt_speed(em, size), fmt_speed(hm, size), fmt_speed(em + hm, size), fmt_speed(cm, size));
        println!("chd dvd hash strategy {target_name} done");
        let _ = std::fs::remove_file(&out_a);
        let _ = std::fs::remove_file(&out_b);
    }
    write_report("hash_strategy_chd_dvd", &md);
    println!("{md}");
    let _ = std::fs::remove_dir_all(&disk);
    if let Some(d) = &tmpfs {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// CHD (CD, the largest non-GD-ROM sample available): 1T/2T/4T/6T/8T/12T/14T/20T (14 = this
/// machine's physical core count, 6P+8E; 20 = logical, P-cores with HT) x {extract then
/// hash, extract while hashing} x {tmpfs, ssd} = 32 combinations, each averaged (mean, not
/// median) over 3 runs -- 96 extract+hash passes total. Each combination is ONE combined
/// extract+hash number (not phase-split, unlike `hash_strategy_chd_dvd`), to keep this
/// already-long sweep from doubling.
///
/// Deliberately NOT run alongside `hash_strategy_rvz`/`hash_strategy_chd_dvd` -- run it on
/// its own when there's time:
/// `cargo test --release --test bench_streaming hash_strategy_chd_cd -- --ignored --nocapture`
#[test]
#[ignore]
fn hash_strategy_chd_cd() {
    if !streaming_builds_present() {
        eprintln!("streaming builds missing -- skipping");
        return;
    }
    let (tmpfs, disk) = hash_bench_scratch("hash_strategy_chd_cd");
    let cpus = native_lib::recommended_reader_threads();
    let threads_list: Vec<usize> = [1usize, 2, 4, 6, 8, 12, 14, 20].into_iter().filter(|&t| t <= cpus).collect();

    let samples = chd_samples();
    let Some(sample) = samples.iter().filter(|s| s.info.is_cd && !s.info.is_gdrom).max_by_key(|s| s.info.logical_bytes) else {
        eprintln!("no CD CHD sample -- skipping");
        return;
    };
    warm(&sample.path);
    let chd = sample.path.clone();
    let size = sample.info.logical_bytes;
    let block = ((4 << 20) / sample.info.hunk_bytes as usize).max(1) * sample.info.hunk_bytes as usize;
    let open = || ChdReader::open(&chd, None, 0).map_err(|e| std::io::Error::other(e.to_string()));

    const ROUNDS: usize = 3;
    let mut md = String::from("# Hash strategy: CHD (CD)\n\n");
    md += &format!(
        "Release build, warm cache, mean of {ROUNDS} rounds (not median -- this sweep is long enough that outliers should average out rather than be discarded). \
         Sample: {} ({:.0} MiB compressed, {:.0} MiB decoded logical data -- raw frames incl. subcode, not cue/bin). \
         14T = this machine's physical core count (6P+8E); 20T = logical (6P w/ HT + 8E). \
         Each cell is ONE combined extract+hash time (not phase-split, unlike the DVD test), computing a single CRC32 with the same `HashDigest` primitive throughout. {}\n\n",
        sample_label(&chd),
        std::fs::metadata(&chd).unwrap().len() as f64 / 1048576.0,
        size as f64 / 1048576.0,
        if tmpfs.is_some() { "tmpfs is a RAM disk (`/dev/shm`); ssd is the normal on-disk output directory." } else { "tmpfs unavailable on this machine -- ssd columns only." }
    );
    md += "| threads | extract-then-hash tmpfs | extract-then-hash ssd | extract-while-hash tmpfs | extract-while-hash ssd |\n";
    md += "|---|---|---|---|---|\n";

    for &threads in &threads_list {
        let mut cells: [Vec<Duration>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        let mut crcs = Vec::new();
        for _ in 0..ROUNDS {
            if let Some(tmpfs_dir) = &tmpfs {
                let out = tmpfs_dir.join("a.bin");
                let (d, crc) = extract_then_hash_combined(|| open(), size, block, threads, &out);
                cells[0].push(d);
                crcs.push(crc);
            }
            {
                let out = disk.join("a.bin");
                let (d, crc) = extract_then_hash_combined(|| open(), size, block, threads, &out);
                cells[1].push(d);
                crcs.push(crc);
            }
            if let Some(tmpfs_dir) = &tmpfs {
                let out = tmpfs_dir.join("b.bin");
                let (d, crc) = decode_and_hash_to_file(|| open(), size, block, threads, &out);
                cells[2].push(d);
                crcs.push(crc);
            }
            {
                let out = disk.join("b.bin");
                let (d, crc) = decode_and_hash_to_file(|| open(), size, block, threads, &out);
                cells[3].push(d);
                crcs.push(crc);
            }
        }
        assert!(crcs.windows(2).all(|w| w[0] == w[1]), "{threads}T: CRC32 disagreement: {crcs:?}");
        let fmt = |v: &[Duration]| if v.is_empty() { "--".to_string() } else { fmt_speed(mean(v), size) };
        md += &format!("| {threads} | {} | {} | {} | {} |\n", fmt(&cells[0]), fmt(&cells[1]), fmt(&cells[2]), fmt(&cells[3]));
        println!("chd cd hash strategy {threads}T done");
    }
    write_report("hash_strategy_chd_cd", &md);
    println!("{md}");
    let _ = std::fs::remove_dir_all(&disk);
    if let Some(d) = &tmpfs {
        let _ = std::fs::remove_dir_all(d);
    }
}
