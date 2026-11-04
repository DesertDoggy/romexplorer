//! Audio in a form a player can use: which files play as they are, which are converted
//! first, and the cache the conversions live in.
//!
//! Two reasons to convert. A **format the browser cannot play** (DSD, ALAC, AIFF) is
//! converted for every listener, into the format the `audio_dsd_cache_format` setting names.
//! And a **client keeping its own copy** may ask for a smaller one
//! (`GET /audio/{id}/file?format=aac|xhe_aac&bitrate=N`): a track above that bitrate is
//! converted, one at or below it is sent as it is -- re-encoding something already that
//! small would cost quality and save nothing.
//!
//! The pieces: `symphonia` decodes (`rom::audio_decode`), `rdsd2pcm` turns DSD into PCM,
//! libxaac encodes AAC and xHE-AAC (`rom::xaac`, a native library that may not be built on
//! this machine -- then AAC is simply not on offer and the lossless path is used), and
//! `flac-codec` writes FLAC. All but libxaac are pure Rust.
//!
//! A conversion is done whole, once, into `<media>/_audio_cache/`, and the file is then
//! served like any other, so a player can seek in it. Converting while streaming would put
//! the first note a second or two earlier and take seeking away for the whole first play.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::database::settings as db_settings;
use crate::rom::{audio_decode, m4a, xaac};

use super::{App, Error, Result};

/// What a `<audio>` element plays in the embedded browser on every desktop target. An
/// `.m4a` is only one of these when it holds AAC -- ALAC uses the same container.
const BROWSER_PLAYABLE: [&str; 9] = ["mp3", "m4a", "m4b", "aac", "flac", "wav", "ogg", "oga", "opus"];
const DSD: [&str; 2] = ["dsf", "dff"];
/// Containers that can hold ALAC.
const MP4_FAMILY: [&str; 4] = ["m4a", "m4b", "mp4", "alac"];
const AIFF: [&str; 2] = ["aiff", "aif"];

/// How a file reaches a player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    /// The browser plays the file itself.
    Direct,
    /// Decodable here but not playable in a browser: served converted.
    Converted,
    /// Nothing here decodes it (WMA, APE, WavPack, ...). Listed, never played or copied.
    Unsupported,
}

impl Support {
    pub fn name(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Converted => "converted",
            Self::Unsupported => "unsupported",
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name {
            "direct" => Self::Direct,
            "converted" => Self::Converted,
            _ => Self::Unsupported,
        }
    }

    /// From the extension and what the tag reader saw inside an MP4 (`codec_playable` is
    /// false for ALAC).
    pub fn of(format: &str, codec_playable: bool) -> Self {
        if BROWSER_PLAYABLE.contains(&format) && codec_playable {
            Self::Direct
        } else if DSD.contains(&format) || AIFF.contains(&format) || (MP4_FAMILY.contains(&format) && !codec_playable) {
            Self::Converted
        } else {
            Self::Unsupported
        }
    }
}

/// What a conversion produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Aac { kbps: u32 },
    XheAac { kbps: u32 },
    Flac,
}

impl Target {
    pub(crate) fn ext(self) -> &'static str {
        match self {
            Self::Flac => "flac",
            _ => "m4a",
        }
    }

    /// The part of the cache file's name that says what is in it.
    pub(crate) fn tag(self) -> String {
        match self {
            Self::Aac { kbps } => format!("aac{kbps}"),
            Self::XheAac { kbps } => format!("xhe{kbps}"),
            Self::Flac => "flac".to_string(),
        }
    }

    fn encoder(self) -> Option<(xaac::Format, u32)> {
        match self {
            Self::Aac { kbps } => Some((xaac::Format::Aac, kbps * 1000)),
            Self::XheAac { kbps } => Some((xaac::Format::XheAac, kbps * 1000)),
            Self::Flac => None,
        }
    }
}

/// The bitrate range a client may ask for, in kbps.
pub const BITRATE_RANGE: std::ops::RangeInclusive<u32> = 48..=320;
/// What a browser-unplayable track is cached at when the cache format is AAC: high enough
/// that the conversion is not what limits it.
pub(crate) const CACHE_AAC_KBPS: u32 = 256;

/// A track's own bitrate in kbps, from its size and length. `None` when the length is not
/// known (DSD and anything the tag reader could not parse).
pub fn source_kbps(size: u64, duration_ms: Option<u32>) -> Option<u32> {
    let ms = u64::from(duration_ms.filter(|d| *d > 0)?);
    Some((size.saturating_mul(8) / ms) as u32)
}

/// Whether a client asking for at most `limit_kbps` gets a conversion. A track the browser
/// cannot play is always converted (the copy has to be playable where it lands). Otherwise
/// only one that is above the limit; with no known bitrate there is no case for converting,
/// so the original goes.
pub fn should_convert(support: Support, source_kbps: Option<u32>, limit_kbps: u32) -> bool {
    match support {
        Support::Unsupported => false,
        Support::Converted => true,
        Support::Direct => source_kbps.is_some_and(|kbps| kbps > limit_kbps),
    }
}

/// What a client asked for with `?format=&bitrate=`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileQuery {
    pub format: Option<String>,
    pub bitrate: Option<u32>,
}

impl FileQuery {
    /// `None` for a plain request; an error for a format or bitrate that is not on offer.
    pub fn wanted(&self) -> Result<Option<(xaac::Format, u32)>> {
        let Some(name) = self.format.as_deref().filter(|f| !f.is_empty() && *f != "none") else { return Ok(None) };
        let format = xaac::Format::from_name(name).ok_or_else(|| Error::invalid("format must be aac or xhe_aac"))?;
        let kbps = self.bitrate.unwrap_or(128);
        if !BITRATE_RANGE.contains(&kbps) {
            return Err(Error::invalid(format!("bitrate must be {}-{} (kbps)", BITRATE_RANGE.start(), BITRATE_RANGE.end())));
        }
        Ok(Some((format, kbps)))
    }
}

/// The file to send for a track.
#[derive(Debug, Clone)]
pub struct Served {
    pub path: PathBuf,
    pub mime: &'static str,
    /// The extension a copy of it should be saved under.
    pub ext: String,
    pub converted: bool,
    /// Why the original went out when a conversion was asked for.
    pub note: Option<String>,
}

fn original(path: PathBuf, note: Option<String>) -> Served {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    Served { mime: crate::database::media_store::mime_for(&path), path, ext, converted: false, note }
}

/// `<media>/_audio_cache/`.
pub fn cache_dir(app: &App) -> PathBuf {
    app.paths.media_dir().join("_audio_cache")
}

/// `<id>-<what>-<source mtime>.<ext>`: a source file that changed gets a new name, and the
/// old one is left for the size cap to remove.
fn cache_name(id: u64, target: Target, modified: Option<chrono::NaiveDateTime>) -> String {
    let stamp = modified.map(|m| m.and_utc().timestamp()).unwrap_or(0);
    format!("{id}-{}-{stamp}.{}", target.tag(), target.ext())
}

/// One conversion at a time. A conversion uses a whole core for seconds; two listeners
/// opening the same DSD album should queue, not each start encoding every track, and the
/// second finds the first one's file when its turn comes.
pub(crate) static CONVERTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The file for track `id`: the original, or its conversion -- made now if this is the
/// first time it is asked for.
pub async fn serve(app: &App, id: u64, query: &FileQuery) -> Result<Served> {
    let wanted = query.wanted()?;
    let row = sqlx::query("SELECT path, format, support, size, duration_ms, last_modified FROM audio_files WHERE id = ? AND deleted = FALSE")
        .bind(id)
        .fetch_optional(&app.pool)
        .await?
        .ok_or_else(|| Error::not_found(format!("no audio file {id}")))?;
    let path = PathBuf::from(row.try_get::<String, _>("path")?);
    if !path.is_file() {
        return Err(Error::not_found(format!("audio file {id} is missing on disk")));
    }
    let format: String = row.try_get("format")?;
    let support = Support::from_name(&row.try_get::<String, _>("support")?);
    if support == Support::Unsupported {
        return Err(Error::not_found(format!("audio file {id} is in a format ({format}) nothing here can decode")));
    }
    let kbps = source_kbps(row.try_get("size")?, row.try_get("duration_ms")?);

    let target = match wanted {
        // A client's copy.
        Some((want, limit)) => {
            if !should_convert(support, kbps, limit) {
                return Ok(original(path, Some("at or below the requested bitrate".to_string())));
            }
            if !xaac::available() {
                return Ok(original(path, Some("the AAC encoder is not built on the server".to_string())));
            }
            match want {
                xaac::Format::Aac => Target::Aac { kbps: limit },
                xaac::Format::XheAac => Target::XheAac { kbps: limit },
            }
        }
        // Playing it here.
        None => match support {
            Support::Direct => return Ok(original(path, None)),
            _ => {
                let lossless = db_settings::get::<String>(&app.pool, db_settings::KEY_AUDIO_DSD_CACHE_FORMAT).await?.as_deref() == Some("flac");
                if lossless || !xaac::available() {
                    Target::Flac
                } else {
                    Target::Aac { kbps: CACHE_AAC_KBPS }
                }
            }
        },
    };

    let dir = cache_dir(app);
    let cached = dir.join(cache_name(id, target, row.try_get("last_modified")?));
    let _turn = CONVERTING.lock().await;
    if !cached.is_file() {
        let (source, out, is_dsd) = (path.clone(), cached.clone(), DSD.contains(&format.as_str()));
        let made = tokio::task::spawn_blocking(move || convert(&source, &out, target, is_dsd)).await.map_err(|e| Error::internal(format!("audio conversion task: {e}")))?;
        if let Err(reason) = made {
            log::warn!("audio {id}: {} could not be converted: {reason}", path.display());
            // A client's copy can still be the original; a track the browser cannot play
            // has nothing to fall back to.
            return match (wanted, support) {
                (Some(_), Support::Direct) => Ok(original(path, Some(reason))),
                _ => Err(Error::unavailable(format!("audio file {id} could not be converted: {reason}"))),
            };
        }
        let cap = db_settings::get::<u64>(&app.pool, db_settings::KEY_AUDIO_CACHE_MAX_MB).await?.unwrap_or(DEFAULT_CACHE_MB);
        let (dir, keep) = (dir.clone(), cached.clone());
        let _ = tokio::task::spawn_blocking(move || evict(&dir, cap.saturating_mul(1 << 20), &keep)).await;
    } else {
        // Recently used is what the cap keeps.
        let _ = std::fs::File::options().write(true).open(&cached).and_then(|f| f.set_modified(std::time::SystemTime::now()));
    }
    Ok(Served { mime: crate::database::media_store::mime_for(&cached), ext: target.ext().to_string(), path: cached, converted: true, note: None })
}

/// The cache's size cap when the setting is unset, in MiB.
pub const DEFAULT_CACHE_MB: u64 = 4096;

/// Removes the least recently used files until the cache fits `cap` bytes (0 = no cap).
/// `keep` is never removed: it is what is about to be served.
pub(crate) fn evict(dir: &Path, cap: u64, keep: &Path) {
    if cap == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            meta.is_file().then(|| (meta.modified().unwrap_or(std::time::UNIX_EPOCH), meta.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (_, len, path) in files {
        if total <= cap {
            break;
        }
        if path != keep && std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

/// Makes `out` from `source`. Written beside its final name and renamed, so a file that is
/// in the cache is always a whole one.
fn convert(source: &Path, out: &Path, target: Target, is_dsd: bool) -> std::result::Result<(), String> {
    let dir = out.parent().ok_or("no cache directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let partial = out.with_extension(format!("{}.part", target.ext()));
    let _ = std::fs::remove_file(&partial);
    let made = if is_dsd {
        // rdsd2pcm writes into a directory of its choosing, under a name of its choosing.
        let scratch = dir.join(format!("dsd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        let result = match target {
            // 24-bit at twice the CD rate: what DSD64 holds that 16/44.1 would lose.
            Target::Flac => dsd_to_pcm(source, &scratch, rdsd2pcm::OutputType::Flac, 24, 88_200).and_then(|flac| std::fs::rename(&flac, &partial).map_err(|e| format!("move {}: {e}", flac.display()))),
            // AAC is a 16-bit, 44.1 kHz affair either way; the WAV is only a stepping stone.
            _ => dsd_to_pcm(source, &scratch, rdsd2pcm::OutputType::Wav, 16, 44_100).and_then(|wav| encode(&wav, &partial, target)),
        };
        let _ = std::fs::remove_dir_all(&scratch);
        result
    } else {
        encode(source, &partial, target)
    };
    match made {
        Ok(()) => std::fs::rename(&partial, out).map_err(|e| format!("move into the cache: {e}")),
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Err(e)
        }
    }
}

/// DSD to a PCM file in `scratch`; returns the file it wrote.
fn dsd_to_pcm(source: &Path, scratch: &Path, kind: rdsd2pcm::OutputType, bits: usize, rate: u32) -> std::result::Result<PathBuf, String> {
    std::fs::create_dir_all(scratch).map_err(|e| format!("create {}: {e}", scratch.display()))?;
    // rdsd2pcm resolves the source's folder with `canonicalize`, which some drives refuse
    // (RAM disks, some network shares: "Incorrect function"). A file on one of those is
    // copied next to the cache first and converted from there.
    let local;
    let source = if source.parent().is_some_and(|p| p.canonicalize().is_err()) {
        let inbox = scratch.join("in");
        std::fs::create_dir_all(&inbox).map_err(|e| format!("create {}: {e}", inbox.display()))?;
        local = inbox.join(source.file_name().ok_or("the DSD file has no name")?);
        std::fs::copy(source, &local).map_err(|e| format!("copy {}: {e}", source.display()))?;
        local.as_path()
    } else {
        source
    };
    let parent = source.parent().ok_or("the DSD file has no folder")?.to_path_buf();
    // `base_dir` = the file's own folder, so the output lands directly in `scratch` rather
    // than in a copy of the source tree under it. Any artwork beside the source is copied
    // there too; the whole directory is removed afterwards.
    let mut converter = rdsd2pcm::Rdsd2Pcm::from_container(bits, kind, 0.0, rate, Some(scratch.to_path_buf()), rdsd2pcm::DitherType::TPDF, rdsd2pcm::FilterType::Equiripple, false, parent, source.to_path_buf())
        .map_err(|e| format!("reading the DSD file: {e}"))?;
    converter.do_conversion(&std::sync::atomic::AtomicBool::new(false), None).map_err(|e| format!("DSD to PCM: {e}"))?;
    let wanted = if kind == rdsd2pcm::OutputType::Flac { "flac" } else { "wav" };
    std::fs::read_dir(scratch)
        .map_err(|e| format!("{}: {e}", scratch.display()))?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some(wanted))
        .ok_or_else(|| "the DSD conversion wrote no file".to_string())
}

/// A decodable file to `out` in `target`'s format, a packet at a time.
fn encode(source: &Path, out: &Path, target: Target) -> std::result::Result<(), String> {
    encode_with(out, target, |sink| audio_decode::decode_file_with(source, sink))
}

/// The sink a decoder pushes interleaved 16-bit PCM into, with its rate and channel count.
pub(crate) type PcmSink<'a> = &'a mut dyn FnMut(&[i16], u32, u16) -> std::result::Result<(), String>;

/// `out` in `target`'s format from whatever `decode` pushes into the sink it is given;
/// `decode` returns the rate and channel count. Shared with ROM audio (`api::rom_audio`).
pub(crate) fn encode_with(out: &Path, target: Target, decode: impl FnOnce(PcmSink<'_>) -> std::result::Result<(u32, u16), String>) -> std::result::Result<(), String> {
    match target.encoder() {
        Some((format, bitrate)) => {
            let mut stream: Option<xaac::StreamEncoder> = None;
            let (rate, channels) = decode(&mut |pcm, rate, channels| {
                if stream.is_none() {
                    stream = Some(xaac::StreamEncoder::new(rate, channels, bitrate, format)?);
                }
                stream.as_mut().expect("just set").push(pcm)
            })?;
            let encoded = stream.ok_or("nothing to encode")?.finish()?;
            let sync = (format == xaac::Format::XheAac).then(|| m4a::xhe_sync_frames(&encoded.frames));
            let file = m4a::write(&m4a::Track { frames: &encoded.frames, config: &encoded.config, sample_rate: rate, channels, frame_samples: encoded.frame_samples, bitrate, sync_frames: sync.as_deref() });
            std::fs::write(out, file).map_err(|e| format!("write {}: {e}", out.display()))
        }
        None => {
            // 16-bit: the decoder hands out 16-bit samples, so a 24-bit ALAC loses its low
            // byte here. DSD does not come this way (rdsd2pcm writes its FLAC itself).
            let mut writer: Option<flac_codec::encode::FlacSampleWriter<std::io::BufWriter<std::fs::File>>> = None;
            let mut wide = Vec::new();
            decode(&mut |pcm, rate, channels| {
                if writer.is_none() {
                    writer = Some(flac_codec::encode::FlacSampleWriter::create(out, flac_codec::encode::Options::default(), rate, 16, u8::try_from(channels).map_err(|_| "too many channels")?, None).map_err(|e| format!("create {}: {e}", out.display()))?);
                }
                wide.clear();
                wide.extend(pcm.iter().map(|s| i32::from(*s)));
                writer.as_mut().expect("just set").write(&wide).map_err(|e| format!("FLAC: {e}"))
            })?;
            writer.ok_or("nothing to encode")?.finalize().map_err(|e| format!("FLAC: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn support_follows_the_format_and_what_is_inside_an_mp4() {
        for format in ["mp3", "flac", "wav", "ogg", "opus", "aac"] {
            assert_eq!(Support::of(format, true), Support::Direct, "{format}");
        }
        assert_eq!(Support::of("m4a", true), Support::Direct, "AAC in an m4a");
        assert_eq!(Support::of("m4a", false), Support::Converted, "ALAC in an m4a");
        for format in ["dsf", "dff", "aiff", "aif"] {
            assert_eq!(Support::of(format, true), Support::Converted, "{format}");
        }
        for format in ["wma", "ape", "wv", "mpc", "tta", "mka"] {
            assert_eq!(Support::of(format, true), Support::Unsupported, "{format}");
        }
        for support in [Support::Direct, Support::Converted, Support::Unsupported] {
            assert_eq!(Support::from_name(support.name()), support);
        }
    }

    #[test]
    fn only_a_track_above_the_limit_is_converted() {
        // 5 MB over 4 minutes = 166 kbps.
        let kbps = source_kbps(5_000_000, Some(240_000));
        assert_eq!(kbps, Some(166));
        assert!(should_convert(Support::Direct, kbps, 128), "above");
        assert!(!should_convert(Support::Direct, kbps, 166), "equal is not above");
        assert!(!should_convert(Support::Direct, kbps, 192), "below");
        assert_eq!(source_kbps(5_000_000, None), None);
        assert_eq!(source_kbps(5_000_000, Some(0)), None);
        assert!(!should_convert(Support::Direct, None, 128), "no known bitrate: the original goes");
        assert!(should_convert(Support::Converted, None, 128), "DSD has no known bitrate and is always converted");
        assert!(!should_convert(Support::Unsupported, Some(1411), 128));
    }

    #[test]
    fn a_request_names_a_format_and_a_bitrate_in_range() {
        let q = |format: Option<&str>, bitrate| FileQuery { format: format.map(str::to_string), bitrate };
        assert_eq!(q(None, None).wanted().unwrap(), None);
        assert_eq!(q(Some("none"), Some(64)).wanted().unwrap(), None);
        assert_eq!(q(Some("aac"), None).wanted().unwrap(), Some((xaac::Format::Aac, 128)));
        assert_eq!(q(Some("xhe_aac"), Some(64)).wanted().unwrap(), Some((xaac::Format::XheAac, 64)));
        assert!(q(Some("opus"), Some(64)).wanted().is_err());
        assert!(q(Some("aac"), Some(32)).wanted().is_err());
        assert!(q(Some("aac"), Some(512)).wanted().is_err());
    }

    #[test]
    fn a_cache_file_is_named_for_the_track_the_format_and_the_source_version() {
        let at = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap().naive_utc();
        assert_eq!(cache_name(7, Target::Aac { kbps: 128 }, Some(at)), "7-aac128-1700000000.m4a");
        assert_eq!(cache_name(7, Target::XheAac { kbps: 64 }, Some(at)), "7-xhe64-1700000000.m4a");
        assert_eq!(cache_name(7, Target::Flac, None), "7-flac-0.flac");
        assert_ne!(cache_name(7, Target::Aac { kbps: 128 }, Some(at)), cache_name(7, Target::Aac { kbps: 128 }, Some(at + chrono::Duration::seconds(1))), "an edited source is a new file");
    }

    #[test]
    fn the_cache_drops_its_oldest_files_first_and_never_the_one_being_served() {
        let dir = std::env::temp_dir().join(format!("rx-audio-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = std::time::SystemTime::now();
        for (name, age) in [("old.m4a", 300), ("mid.m4a", 200), ("new.m4a", 100)] {
            let path = dir.join(name);
            std::fs::write(&path, vec![0u8; 1000]).unwrap();
            std::fs::File::options().write(true).open(&path).unwrap().set_modified(now - std::time::Duration::from_secs(age)).unwrap();
        }
        evict(&dir, 0, &dir.join("new.m4a"));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3, "0 = no cap");
        evict(&dir, 2500, &dir.join("new.m4a"));
        assert!(!dir.join("old.m4a").exists() && dir.join("mid.m4a").exists() && dir.join("new.m4a").exists());
        evict(&dir, 500, &dir.join("mid.m4a"));
        assert!(dir.join("mid.m4a").exists() && !dir.join("new.m4a").exists(), "the file being served stays, whatever its age");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A DSF file holding `seconds` of a 1 kHz sine at half scale on both channels: DSD64,
    /// stereo, made with a first-order sigma-delta modulator. Public so the integration test
    /// can put a real DSD file in a library.
    pub fn sine_dsf(seconds: u32) -> Vec<u8> {
        const RATE: u32 = 2_822_400;
        const BLOCK: usize = 4096;
        // Whole blocks only, so there is no padding to get wrong.
        let blocks = (RATE as usize * seconds as usize / 8).div_ceil(BLOCK);
        let bits = blocks * BLOCK * 8;
        let mut channel = vec![0u8; blocks * BLOCK];
        let (mut acc, mut out) = (0.0f64, 1.0f64);
        for n in 0..bits {
            let x = 0.5 * (std::f64::consts::TAU * 1000.0 * n as f64 / f64::from(RATE)).sin();
            acc += x - out;
            out = if acc >= 0.0 { 1.0 } else { -1.0 };
            if out > 0.0 {
                // Bits are stored least significant first.
                channel[n / 8] |= 1 << (n % 8);
            }
        }
        let data_len = (channel.len() * 2) as u64;
        let mut file = Vec::with_capacity(92 + data_len as usize);
        file.extend(b"DSD ");
        file.extend(28u64.to_le_bytes());
        file.extend((92 + data_len).to_le_bytes());
        file.extend(0u64.to_le_bytes());
        file.extend(b"fmt ");
        file.extend(52u64.to_le_bytes());
        for v in [1u32, 0, 2, 2, RATE, 1] {
            file.extend(v.to_le_bytes());
        }
        file.extend((bits as u64).to_le_bytes());
        file.extend((BLOCK as u32).to_le_bytes());
        file.extend(0u32.to_le_bytes());
        file.extend(b"data");
        file.extend((12 + data_len).to_le_bytes());
        // One block per channel, alternating.
        for block in channel.chunks(BLOCK) {
            file.extend(block);
            file.extend(block);
        }
        file
    }

    /// How many times a second the left channel crosses zero upwards: the tone's frequency.
    fn frequency(pcm: &[i16], rate: u32, channels: u16) -> f64 {
        let left: Vec<i16> = pcm.iter().step_by(usize::from(channels)).copied().collect();
        let body = &left[left.len() / 4..left.len() * 3 / 4];
        let crossings = body.windows(2).filter(|w| w[0] < 0 && w[1] >= 0).count();
        crossings as f64 * f64::from(rate) / body.len() as f64
    }

    /// DSD through rdsd2pcm: to FLAC at 88.2 kHz, and (with the encoder built) to AAC.
    #[test]
    fn a_dsd_file_becomes_a_flac_and_an_aac_that_hold_its_tone() {
        // Under the build directory, not the temp directory: the cache has to be somewhere
        // `canonicalize` works, and a temp directory on a RAM disk is not.
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("rx-audio-dsd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("album")).unwrap();
        let source = dir.join("album").join("tone.dsf");
        std::fs::write(&source, sine_dsf(2)).unwrap();
        // Artwork beside the source: rdsd2pcm copies it along, and it must not end up in the cache.
        std::fs::write(dir.join("album").join("cover.jpg"), b"jpeg").unwrap();
        let cache = dir.join("cache");

        let flac = cache.join("3-flac-0.flac");
        let started = std::time::Instant::now();
        convert(&source, &flac, Target::Flac, true).unwrap();
        eprintln!("DSD64 2 s -> FLAC 24/88.2 in {:.2?}", started.elapsed());
        let back = audio_decode::decode_file(&flac).unwrap();
        assert_eq!((back.sample_rate, back.channels), (88_200, 2));
        let seconds = back.pcm.len() as f64 / 2.0 / 88_200.0;
        assert!((1.9..2.2).contains(&seconds), "{seconds} s");
        let tone = frequency(&back.pcm, back.sample_rate, back.channels);
        assert!((990.0..1010.0).contains(&tone), "{tone} Hz");
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 1, "only the conversion is left in the cache: no scratch folder, no artwork");

        if xaac::available() {
            let m4a = cache.join("3-aac256-0.m4a");
            let started = std::time::Instant::now();
            convert(&source, &m4a, Target::Aac { kbps: 256 }, true).unwrap();
            eprintln!("DSD64 2 s -> AAC 256 in {:.2?}", started.elapsed());
            let back = audio_decode::decode_file(&m4a).unwrap();
            assert_eq!((back.sample_rate, back.channels), (44_100, 2));
            let tone = frequency(&back.pcm, back.sample_rate, back.channels);
            assert!((990.0..1010.0).contains(&tone), "{tone} Hz");
            assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 2);
        }
        // A source on a drive that cannot be canonicalized (where the temp directory is such
        // a drive) converts all the same; elsewhere this is just a second conversion.
        let away = std::env::temp_dir().join(format!("rx-audio-dsd-src-{}", std::process::id()));
        std::fs::create_dir_all(&away).unwrap();
        std::fs::copy(&source, away.join("tone.dsf")).unwrap();
        convert(&away.join("tone.dsf"), &cache.join("4-flac-0.flac"), Target::Flac, true).unwrap();
        assert!(cache.join("4-flac-0.flac").is_file());
        let _ = std::fs::remove_dir_all(&away);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// WAV in, FLAC out, and the same samples back: the lossless path needs no native library.
    #[test]
    fn a_wav_becomes_a_flac_with_the_same_samples() {
        let dir = std::env::temp_dir().join(format!("rx-audio-convert-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let samples: Vec<i16> = (0..8000u32).flat_map(|n| { let v = ((n as f32 * 0.05).sin() * 9000.0) as i16; [v, -v] }).collect();
        let mut wav = Vec::new();
        wav.extend(b"RIFF");
        wav.extend((36 + samples.len() as u32 * 2).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(8000u32.to_le_bytes());
        wav.extend(32_000u32.to_le_bytes());
        wav.extend(4u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend((samples.len() as u32 * 2).to_le_bytes());
        wav.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
        let source = dir.join("in.wav");
        std::fs::write(&source, wav).unwrap();

        let out = dir.join("7-flac-0.flac");
        convert(&source, &out, Target::Flac, false).unwrap();
        assert!(!dir.join("7-flac-0.flac.part").exists(), "nothing half-written is left behind");
        let back = audio_decode::decode_file(&out).unwrap();
        assert_eq!((back.sample_rate, back.channels), (8000, 2));
        assert_eq!(back.pcm, samples, "lossless");

        if xaac::available() {
            let m4a = dir.join("7-aac64-0.m4a");
            // 8 kHz is a rate AAC carries; the file must come out smaller than the WAV.
            convert(&source, &m4a, Target::Aac { kbps: 48 }, false).unwrap();
            assert!(std::fs::metadata(&m4a).unwrap().len() < std::fs::metadata(&source).unwrap().len());
            assert_eq!(audio_decode::decode_file(&m4a).unwrap().sample_rate, 8000);
        }
        // What cannot be decoded is an error with a reason, and leaves nothing behind.
        let junk = dir.join("junk.wma");
        std::fs::write(&junk, b"not audio at all").unwrap();
        assert!(convert(&junk, &dir.join("8-flac-0.flac"), Target::Flac, false).is_err());
        assert!(!dir.join("8-flac-0.flac").exists() && !dir.join("8-flac-0.flac.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
