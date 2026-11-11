//! Runs a PlayStation disc in the Beetle PSX libretro core, headless, to find out where the
//! game keeps its music: for discs whose sequences the player finds no trace of in the
//! files (packed or compressed archives).
//!
//! ```text
//! psx_probe <disc .cue/.chd/...> --core <beetle core library> --bios <BIOS folder> --out <folder>
//!           [--seconds N] [--every S] [--formats F] [--press KEY@S,...] [--mash]
//!           [--opt key=value]... [--dump] [--max-mb N] [--boot S] [--spu]
//! ```
//!
//! The core runs for `--seconds` (default 180) of console time. With `--mash`, Start and
//! Cross are pressed in turn every 1.5 s from 8 s on to get past logos and menus (it can
//! hang a game's opening, so it is off by default); `--press` adds presses of its own (`START@20,CROSS@22.5`, keys: CROSS CIRCLE SQUARE TRIANGLE START
//! SELECT UP DOWN LEFT RIGHT L1 R1 L2 R2).
//!
//! Every `--every` seconds (default 5) main RAM is searched for the sound formats' magic
//! strings and scanned by VGMTrans in `--formats` (default: every PlayStation format), and
//! what is new is printed with the loudness of the sound since the last look. Magic hits
//! seen in the first `--boot` seconds (default 10) are the BIOS's own and left out. At the end
//! the bytes of each find are searched for in every file of the disc: found means the music
//! is stored as is (file and offset printed); not found means the game unpacks it.
//!
//! What it writes to `--out`: the sound as `sound.wav`, the memory cards the core saves, and
//! with `--dump` each RAM snapshot as `ram_<seconds>.bin` (VGMTrans and `disc_music` read those)
//! and, with `--spu` too, the sound chip's RAM (its samples) as `spu_<seconds>.bin`, taken from a
//! save state (taking one may upset a game, so it is not done by default).
//! The BIOS is read from its folder as the core finds it there; nothing is copied.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{c_char, c_uint, c_void, CStr, CString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use libloading::Library;
use romexplorer::rom::{disc_files, disc_seq, rom_audio, vgmtrans};

#[repr(C)]
struct GameInfo {
    path: *const c_char,
    data: *const c_void,
    size: usize,
    meta: *const c_char,
}

#[repr(C)]
#[derive(Default)]
struct AvInfo {
    base_width: c_uint,
    base_height: c_uint,
    max_width: c_uint,
    max_height: c_uint,
    aspect: f32,
    fps: f64,
    sample_rate: f64,
}

#[repr(C)]
struct Variable {
    key: *const c_char,
    value: *const c_char,
}

type EnvFn = extern "C" fn(c_uint, *mut c_void) -> bool;
type VideoFn = extern "C" fn(*const c_void, c_uint, c_uint, usize);
type SampleFn = extern "C" fn(i16, i16);
type BatchFn = extern "C" fn(*const i16, usize) -> usize;
type PollFn = extern "C" fn();
type StateFn = extern "C" fn(c_uint, c_uint, c_uint, c_uint) -> i16;

const ENV_GET_CAN_DUPE: c_uint = 3;
const ENV_GET_SYSTEM_DIRECTORY: c_uint = 9;
const ENV_SET_PIXEL_FORMAT: c_uint = 10;
const ENV_GET_VARIABLE: c_uint = 15;
const ENV_GET_VARIABLE_UPDATE: c_uint = 17;
const ENV_GET_SAVE_DIRECTORY: c_uint = 31;
const MEMORY_SYSTEM_RAM: c_uint = 2;
const DEVICE_JOYPAD: c_uint = 1;

/// What the callbacks share with `main`; the core calls them on this thread from `retro_run`.
struct Shared {
    bios_dir: CString,
    save_dir: CString,
    options: HashMap<String, CString>,
    /// RetroPad buttons held this frame (bit = button id).
    held: u32,
    sound: Vec<i16>,
}

static SHARED: Mutex<Option<Shared>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Shared) -> R) -> R {
    let mut guard = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.as_mut().expect("set up before the core runs"))
}

extern "C" fn environment(cmd: c_uint, data: *mut c_void) -> bool {
    match cmd & 0xffff {
        ENV_GET_CAN_DUPE => {
            unsafe { *(data as *mut bool) = true };
            true
        }
        ENV_SET_PIXEL_FORMAT => true,
        ENV_GET_SYSTEM_DIRECTORY => with(|s| {
            unsafe { *(data as *mut *const c_char) = s.bios_dir.as_ptr() };
            true
        }),
        ENV_GET_SAVE_DIRECTORY => with(|s| {
            unsafe { *(data as *mut *const c_char) = s.save_dir.as_ptr() };
            true
        }),
        ENV_GET_VARIABLE => {
            let var = unsafe { &mut *(data as *mut Variable) };
            if var.key.is_null() {
                return false;
            }
            let key = unsafe { CStr::from_ptr(var.key) }.to_string_lossy().into_owned();
            with(|s| match s.options.get(&key) {
                Some(v) => {
                    var.value = v.as_ptr();
                    true
                }
                None => false,
            })
        }
        ENV_GET_VARIABLE_UPDATE => {
            unsafe { *(data as *mut bool) = false };
            true
        }
        _ => false,
    }
}

extern "C" fn video(_: *const c_void, _: c_uint, _: c_uint, _: usize) {}
extern "C" fn sample(l: i16, r: i16) {
    with(|s| s.sound.extend_from_slice(&[l, r]));
}
extern "C" fn batch(data: *const i16, frames: usize) -> usize {
    let samples = unsafe { std::slice::from_raw_parts(data, frames * 2) };
    with(|s| s.sound.extend_from_slice(samples));
    frames
}
extern "C" fn poll() {}
extern "C" fn state(port: c_uint, device: c_uint, _index: c_uint, id: c_uint) -> i16 {
    if port != 0 || device != DEVICE_JOYPAD || id >= 16 {
        return 0;
    }
    with(|s| ((s.held >> id) & 1) as i16)
}

fn button(name: &str) -> Option<u32> {
    Some(match name.to_ascii_uppercase().as_str() {
        "CROSS" => 0,
        "SQUARE" => 1,
        "SELECT" => 2,
        "START" => 3,
        "UP" => 4,
        "DOWN" => 5,
        "LEFT" => 6,
        "RIGHT" => 7,
        "CIRCLE" => 8,
        "TRIANGLE" => 9,
        "L1" => 10,
        "R1" => 11,
        "L2" => 12,
        "R2" => 13,
        _ => return None,
    })
}

struct Core {
    _lib: Library,
    run: unsafe extern "C" fn(),
    memory_data: unsafe extern "C" fn(c_uint) -> *mut c_void,
    memory_size: unsafe extern "C" fn(c_uint) -> usize,
    unload: unsafe extern "C" fn(),
    serialize_size: unsafe extern "C" fn() -> usize,
    serialize: unsafe extern "C" fn(*mut c_void, usize) -> bool,
    deinit: unsafe extern "C" fn(),
}

fn load_core(path: &Path, disc: &Path) -> Result<(Core, AvInfo), String> {
    unsafe {
        let lib = Library::new(path).map_err(|e| format!("{}: {e}", path.display()))?;
        macro_rules! sym {
            ($name:literal, $t:ty) => {
                *lib.get::<$t>(concat!($name, "\0").as_bytes()).map_err(|e| format!("{}: {e}", $name))?
            };
        }
        let set_env = sym!("retro_set_environment", unsafe extern "C" fn(EnvFn));
        let set_video = sym!("retro_set_video_refresh", unsafe extern "C" fn(VideoFn));
        let set_sample = sym!("retro_set_audio_sample", unsafe extern "C" fn(SampleFn));
        let set_batch = sym!("retro_set_audio_sample_batch", unsafe extern "C" fn(BatchFn));
        let set_poll = sym!("retro_set_input_poll", unsafe extern "C" fn(PollFn));
        let set_state = sym!("retro_set_input_state", unsafe extern "C" fn(StateFn));
        let init = sym!("retro_init", unsafe extern "C" fn());
        let load_game = sym!("retro_load_game", unsafe extern "C" fn(*const GameInfo) -> bool);
        let av_info = sym!("retro_get_system_av_info", unsafe extern "C" fn(*mut AvInfo));
        let set_port = sym!("retro_set_controller_port_device", unsafe extern "C" fn(c_uint, c_uint));
        let core = Core {
            run: sym!("retro_run", unsafe extern "C" fn()),
            memory_data: sym!("retro_get_memory_data", unsafe extern "C" fn(c_uint) -> *mut c_void),
            memory_size: sym!("retro_get_memory_size", unsafe extern "C" fn(c_uint) -> usize),
            unload: sym!("retro_unload_game", unsafe extern "C" fn()),
            serialize_size: sym!("retro_serialize_size", unsafe extern "C" fn() -> usize),
            serialize: sym!("retro_serialize", unsafe extern "C" fn(*mut c_void, usize) -> bool),
            deinit: sym!("retro_deinit", unsafe extern "C" fn()),
            _lib: lib,
        };
        set_env(environment);
        init();
        set_video(video);
        set_sample(sample);
        set_batch(batch);
        set_poll(poll);
        set_state(state);
        let path = CString::new(disc.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        let info = GameInfo { path: path.as_ptr(), data: std::ptr::null(), size: 0, meta: std::ptr::null() };
        if !load_game(&info) {
            return Err("the core did not load the disc (BIOS missing from the folder?)".into());
        }
        // One standard pad on port 1 and nothing on the others, as a frontend sets them.
        for port in 0..8 {
            set_port(port, if port == 0 { DEVICE_JOYPAD } else { 0 });
        }
        let mut av = AvInfo::default();
        av_info(&mut av);
        Ok((core, av))
    }
}

/// The sound formats' magic strings, as they sit in memory.
const MAGIC: &[&str] = &["pQES", "pBAV", "AKAO", "smds", "wds ", "SLZ", "KDT1", "SEDBSSCF", "VAGp", "SShd", "SSsq", "IECSsreV"];

/// A run of RAM worth looking for on the disc.
struct Find {
    what: String,
    at: String,
    ram_offset: usize,
    needle: Vec<u8>,
}

const NEEDLE: usize = 48;

fn interesting(needle: &[u8]) -> bool {
    let distinct: HashSet<u8> = needle.iter().copied().collect();
    distinct.len() >= 6
}

fn rms(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / samples.len() as f64).sqrt()
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |args: &mut Vec<String>, f: &str| -> bool {
        let had = args.iter().any(|a| a == f);
        args.retain(|a| a != f);
        had
    };
    // Pressing buttons on a timer hung a game in its opening: only with --mash.
    let no_mash = !flag(&mut args, "--mash");
    flag(&mut args, "--no-mash");
    let dump = flag(&mut args, "--dump");
    let spu = flag(&mut args, "--spu");
    let take = |args: &mut Vec<String>, f: &str| -> Option<String> {
        let i = args.iter().position(|a| a == f)?;
        let v = args.get(i + 1).cloned();
        args.drain(i..(i + 2).min(args.len()));
        v
    };
    let core_path = take(&mut args, "--core").map(PathBuf::from);
    let bios = take(&mut args, "--bios").map(PathBuf::from);
    let out = take(&mut args, "--out").map(PathBuf::from);
    let seconds: f64 = take(&mut args, "--seconds").and_then(|s| s.parse().ok()).unwrap_or(180.0);
    let every: f64 = take(&mut args, "--every").and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let formats = take(&mut args, "--formats").unwrap_or_else(|| format!("{},{}", vgmtrans::PS1, vgmtrans::PS1_CUSTOM));
    let boot_seconds: f64 = take(&mut args, "--boot").and_then(|s| s.parse().ok()).unwrap_or(10.0);
    let max_bytes: u64 = take(&mut args, "--max-mb").and_then(|s| s.parse::<u64>().ok()).unwrap_or(256) << 20;
    let mut presses: Vec<(f64, u32)> = Vec::new();
    if let Some(list) = take(&mut args, "--press") {
        for p in list.split(',') {
            let Some((key, at)) = p.split_once('@') else { continue };
            match (button(key), at.parse::<f64>()) {
                (Some(b), Ok(t)) => presses.push((t, b)),
                _ => eprintln!("ignored press {p}"),
            }
        }
    }
    let mut options: HashMap<String, CString> = HashMap::new();
    // The dynarec runs minutes of console time in seconds; timing accuracy doesn't matter here.
    options.insert("beetle_psx_cpu_dynarec".into(), CString::new("execute").unwrap());
    while let Some(o) = take(&mut args, "--opt") {
        if let Some((k, v)) = o.split_once('=') {
            options.insert(k.to_string(), CString::new(v).unwrap_or_default());
        }
    }
    let (Some(core_path), Some(bios), Some(out), Some(disc)) = (core_path, bios, out, args.first().map(PathBuf::from)) else {
        eprintln!("usage: psx_probe <disc> --core <library> --bios <folder> --out <folder> [--seconds N] [--every S] [--formats F] [--press KEY@S,...] [--mash] [--opt key=value] [--dump] [--max-mb N]");
        std::process::exit(2);
    };
    let saves = out.join("saves");
    std::fs::create_dir_all(&saves).expect("create the --out folder");
    let cstr = |p: &Path| CString::new(p.to_string_lossy().as_bytes()).expect("path without NUL");
    *SHARED.lock().unwrap() = Some(Shared { bios_dir: cstr(&bios), save_dir: cstr(&saves), options, held: 0, sound: Vec::new() });

    let started = Instant::now();
    let (core, av) = match load_core(&core_path, &disc) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    // Held as plain integers: on the PC the floating-point times came out as garbage once the
    // core ran (its code changing the FPU state, it seems), so time is counted in frames.
    let fps = if (20.0..=100.0).contains(&av.fps) { av.fps } else { 59.94 };
    let rate = if (8_000.0..=96_000.0).contains(&av.sample_rate) { av.sample_rate } else { 44_100.0 };
    let fps_milli = (fps * 1000.0).round() as u64;
    let rate_hz = rate.round() as u32;
    // Seconds as tenths, from a frame count, in integers only.
    let tenths = move |frames: u64| frames * 10_000 / fps_milli;
    println!("{}: {fps:.2} fps, {rate:.0} Hz", disc.display());

    let frames = (seconds.max(0.0) as u64) * fps_milli / 1000;
    let every_frames = ((every.max(0.1) * 1000.0) as u64 * fps_milli / 1_000_000).max(1);
    let boot_tenths = (boot_seconds.max(0.0) * 10.0) as u64;
    let presses: Vec<(u64, u32)> = presses.into_iter().map(|(at, b)| ((at.max(0.0) * 10.0) as u64, b)).collect();
    let mut seen_magic: HashSet<(usize, &str)> = HashSet::new();
    let mut seen_song: HashSet<Vec<u8>> = HashSet::new();
    let mut finds: Vec<Find> = Vec::new();
    let mut last_sound = 0usize;
    for frame in 0..frames {
        let now = tenths(frame);
        let mut held = 0u32;
        // From 8 s on, every 1.5 s: Start, then Cross, held a tenth of a second.
        if !no_mash && now >= 80 {
            let since = now - 80;
            if since % 15 == 0 {
                held |= 1 << if (since / 15) % 2 == 0 { 3 } else { 0 };
            }
        }
        for &(at, b) in &presses {
            if now == at {
                held |= 1 << b;
            }
        }
        with(|s| s.held = held);
        unsafe { (core.run)() };
        if (frame + 1) % every_frames != 0 && frame + 1 != frames {
            continue;
        }
        let ram = unsafe {
            let p = (core.memory_data)(MEMORY_SYSTEM_RAM) as *const u8;
            let n = (core.memory_size)(MEMORY_SYSTEM_RAM);
            if p.is_null() || n == 0 {
                continue;
            }
            std::slice::from_raw_parts(p, n).to_vec()
        };
        let t_tenths = tenths(frame + 1);
        let t = format!("{}.{}", t_tenths / 10, t_tenths % 10);
        let loud = with(|s| {
            let r = rms(&s.sound[last_sound..]);
            last_sound = s.sound.len();
            r
        });
        let mut news = Vec::new();
        for m in MAGIC {
            let mb = m.as_bytes();
            for (at, _) in ram.windows(mb.len()).enumerate().filter(|(_, w)| *w == mb) {
                if seen_magic.insert((at, m)) {
                    let needle = ram[at..(at + NEEDLE).min(ram.len())].to_vec();
                    // What is there before the game starts is the BIOS's own (its boot jingle).
                    if t_tenths <= boot_tenths {
                        news.push(format!("{m}@{at:#x} (BIOS, left out)"));
                        continue;
                    }
                    news.push(format!("{m}@{at:#x}"));
                    if interesting(&needle) {
                        finds.push(Find { what: format!("{m} magic"), at: t.clone(), ram_offset: at, needle });
                    }
                }
            }
        }
        match vgmtrans::songs(&ram, "ram.bin", &formats, 0) {
            Ok(songs) => {
                for song in songs {
                    let at = song.offset as usize;
                    let needle = ram[at.min(ram.len())..(at + NEEDLE).min(ram.len())].to_vec();
                    if !interesting(&needle) || !seen_song.insert(needle.clone()) {
                        continue;
                    }
                    news.push(format!("song [{}] {} @{at:#x} {:.1}s", song.format, song.name, song.length_ms as f64 / 1000.0));
                    finds.push(Find { what: format!("[{}] song {}", song.format, song.name), at: t.clone(), ram_offset: at, needle });
                }
            }
            Err(e) => news.push(format!("scan error: {e}")),
        }
        println!("  {t:>6}s sound rms {loud:6.0}{}{}", if news.is_empty() { "" } else { "  new: " }, news.join(", "));
        if dump {
            std::fs::write(out.join(format!("ram_{:05}.bin", t_tenths / 10)), &ram).expect("write RAM dump");
            if let Some(spu) = spu.then(|| spu_ram(&core)).flatten() {
                std::fs::write(out.join(format!("spu_{:05}.bin", t_tenths / 10)), &spu).expect("write SPU RAM dump");
            }
        }
    }
    unsafe {
        (core.unload)();
        (core.deinit)();
    }
    let sound = with(|s| std::mem::take(&mut s.sound));
    {
        let mut f = std::fs::File::create(out.join("sound.wav")).expect("create sound.wav");
        f.write_all(&rom_audio::wav_header(rate_hz, 2, sound.len() as u64 / 2)).unwrap();
        let bytes: Vec<u8> = sound.iter().flat_map(|s| s.to_le_bytes()).collect();
        f.write_all(&bytes).unwrap();
    }
    println!("ran {seconds:.0}s of console time in {:.1}s; {} finds in RAM", started.elapsed().as_secs_f64(), finds.len());
    if finds.is_empty() {
        return;
    }
    locate(&disc, &finds, max_bytes);
}

/// The sound chip's 512 KB of RAM (its samples), out of a save state: Beetle writes each
/// value after its name (length-prefixed) and its size.
fn spu_ram(core: &Core) -> Option<Vec<u8>> {
    let size = unsafe { (core.serialize_size)() };
    if size == 0 {
        return None;
    }
    let mut state = vec![0u8; size];
    if !unsafe { (core.serialize)(state.as_mut_ptr() as *mut c_void, size) } {
        return None;
    }
    const SPU_BYTES: usize = 512 * 1024;
    let mut marker = b"\x06SPURAM".to_vec();
    marker.extend_from_slice(&(SPU_BYTES as u32).to_le_bytes());
    let at = state.windows(marker.len()).position(|w| w == marker)? + marker.len();
    state.get(at..at + SPU_BYTES).map(<[u8]>::to_vec)
}

/// Searches every file of the disc for the finds' bytes.
fn locate(disc: &Path, finds: &[Find], max_bytes: u64) {
    let rom = match rom_audio::open_rom(disc) {
        Ok(r) => r,
        Err(e) => {
            println!("cannot read the disc to locate the finds: {e}");
            return;
        }
    };
    // First 8 bytes of each needle -> the finds that start so.
    let mut by_head: HashMap<[u8; 8], Vec<usize>> = HashMap::new();
    for (i, f) in finds.iter().enumerate() {
        if f.needle.len() >= 8 {
            by_head.entry(f.needle[..8].try_into().unwrap()).or_default().push(i);
        }
    }
    let mut found: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for view in &rom.data {
        let Ok(listing) = disc_files::list(view) else { continue };
        for file in &listing.files {
            if file.size < 8 || file.size > max_bytes {
                continue;
            }
            let Ok(bytes) = disc_seq::read_whole(view, file) else { continue };
            for at in 0..bytes.len().saturating_sub(7) {
                let head: [u8; 8] = bytes[at..at + 8].try_into().unwrap();
                let Some(list) = by_head.get(&head) else { continue };
                for &i in list {
                    let n = finds[i].needle.len();
                    if bytes.len() - at >= n && bytes[at..at + n] == finds[i].needle[..] {
                        found.entry(i).or_default().push(format!("{} @{at:#x}", file.path));
                    }
                }
            }
        }
    }
    println!("where the finds are on the disc:");
    for (i, f) in finds.iter().enumerate() {
        let places = found.get(&i).map(|p| {
            let more = if p.len() > 4 { format!(" (+{} more)", p.len() - 4) } else { String::new() };
            format!("{}{more}", p.iter().take(4).cloned().collect::<Vec<_>>().join(", "))
        });
        println!(
            "  {:>6}s RAM {:#08x} {}: {}",
            f.at,
            f.ram_offset,
            f.what,
            places.unwrap_or_else(|| "not stored as is (packed or built at run time)".into())
        );
    }
}
