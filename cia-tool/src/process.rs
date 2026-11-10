//! Per-file processing: detect the format, pick the output name, decrypt.

use crate::cci::{self, CciOutcome, Ncsd};
use crate::cia::{self, Cia, CiaOutcome};
use crate::keys::KeyStore;
use crate::naming;
use crate::ncch::{self, AppTitle, NcchHeader, SMDH_LANGS};
use crate::seeddb::SeedDb;
use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub out_dir: Option<PathBuf>,
    /// Convert CIA game titles into CCI instead of a decrypted CIA.
    pub to_cci: bool,
    /// Keep unused cartridge space when decrypting a CCI.
    pub no_trim: bool,
    /// Name outputs after the SMDH title instead of the input file name.
    pub name_from_title: bool,
    /// Preferred SMDH language index (see `ncch::SMDH_LANGS`).
    pub lang: Option<usize>,
    pub overwrite: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Cia,
    Cci,
    Ncch,
}

pub fn detect(f: &mut File) -> Result<Format> {
    let mut head = vec![0u8; 0x200];
    f.seek(SeekFrom::Start(0))?;
    let n = f.read(&mut head)?;
    head.truncate(n);
    f.seek(SeekFrom::Start(0))?;
    if cci::is_ncsd(&head) {
        Ok(Format::Cci)
    } else if ncch::is_ncch(&head) {
        Ok(Format::Ncch)
    } else if cia::is_cia(&head) {
        Ok(Format::Cia)
    } else {
        bail!("not a CIA, CCI/3DS or NCCH file")
    }
}

#[derive(Debug)]
pub enum Outcome {
    Written(PathBuf),
    AlreadyDecrypted,
    OutputExists(PathBuf),
}

/// Pick a title: preferred language, then English, then Japanese, then any.
pub fn pick_title(titles: &[AppTitle], lang: Option<usize>) -> Option<&AppTitle> {
    let order = lang.into_iter().chain([1usize, 0]).chain(0..16);
    for i in order {
        if let Some(t) = titles.get(i)
            && !t.short.trim().is_empty()
        {
            return Some(t);
        }
    }
    None
}

pub fn lang_index(code: &str) -> Option<usize> {
    let c = code.to_ascii_lowercase();
    let c = match c.as_str() {
        "jp" | "jpn" | "japanese" => "ja",
        "eng" | "english" => "en",
        "zh" | "zh-cn" | "chs" => "zh-hans",
        "zh-tw" | "cht" => "zh-hant",
        "kr" => "ko",
        o => o,
    };
    SMDH_LANGS.iter().position(|&l| l == c)
}

fn title_stem(smdh: Option<Vec<u8>>, lang: Option<usize>) -> Option<OsString> {
    let titles = ncch::parse_smdh(&smdh?)?;
    let t = pick_title(&titles, lang)?;
    Some(OsString::from(naming::sanitize_filename(&t.short)))
}

/// Run `write` into `<out>.part`, then rename to `out`.
fn write_atomically(out: &Path, write: impl FnOnce(&mut File) -> Result<bool>) -> Result<bool> {
    if let Some(d) = out.parent()
        && !d.as_os_str().is_empty()
    {
        std::fs::create_dir_all(d)
            .with_context(|| format!("cannot create folder {}", d.display()))?;
    }
    let part = naming::partial_path(out);
    let mut f = File::create(&part).with_context(|| format!("cannot create {}", part.display()))?;
    let res = write(&mut f);
    drop(f);
    match res {
        Ok(true) => {
            std::fs::rename(&part, out)
                .with_context(|| format!("cannot rename to {}", out.display()))?;
            Ok(true)
        }
        Ok(false) => {
            let _ = std::fs::remove_file(&part);
            Ok(false)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

pub fn process_file(
    input: &Path,
    opts: &Options,
    keys: &KeyStore,
    seeds: &SeedDb,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<Outcome> {
    let mut f = File::open(input).with_context(|| format!("cannot open {}", input.display()))?;
    let fmt = detect(&mut f)?;
    let default_stem = input
        .file_stem()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| "output".into());
    let out_dir = opts.out_dir.as_deref();

    match fmt {
        Format::Cia => {
            let cia = Cia::open(&mut f)?;
            let kind = cia::title_kind(cia.title_id);
            let stem = if opts.name_from_title {
                title_stem(cia::read_smdh(&mut f, &cia, keys, seeds), opts.lang)
                    .unwrap_or(default_stem)
            } else {
                default_stem
            };
            if opts.to_cci && kind == "Game" {
                let out =
                    naming::output_path(input, out_dir, &stem, &format!(" {kind}-decrypted.cci"));
                if out.exists() && !opts.overwrite {
                    return Ok(Outcome::OutputExists(out));
                }
                write_atomically(&out, |o| {
                    cci::cia_to_cci(&mut f, &cia, o, keys, seeds, progress)?;
                    Ok(true)
                })?;
                return Ok(Outcome::Written(out));
            }
            if opts.to_cci {
                eprintln!(
                    "  note: {kind} titles cannot be converted to CCI; writing a decrypted CIA instead"
                );
            }
            let out = naming::output_path(input, out_dir, &stem, &format!(" {kind}-decrypted.cia"));
            if out.exists() && !opts.overwrite {
                return Ok(Outcome::OutputExists(out));
            }
            let mut already = false;
            write_atomically(&out, |o| {
                match cia::decrypt_cia(&mut f, &cia, o, keys, seeds, progress)? {
                    CiaOutcome::Decrypted => Ok(true),
                    CiaOutcome::AlreadyDecrypted => {
                        already = true;
                        Ok(false)
                    }
                }
            })?;
            Ok(if already {
                Outcome::AlreadyDecrypted
            } else {
                Outcome::Written(out)
            })
        }
        Format::Cci => {
            let ncsd = Ncsd::open(&mut f)?;
            let stem = if opts.name_from_title {
                title_stem(cci::read_smdh(&mut f, &ncsd, keys, seeds), opts.lang)
                    .unwrap_or(default_stem)
            } else {
                default_stem
            };
            let out = naming::output_path(input, out_dir, &stem, "-decrypted.cci");
            if out.exists() && !opts.overwrite {
                return Ok(Outcome::OutputExists(out));
            }
            let mut already = false;
            write_atomically(&out, |o| {
                match cci::decrypt_cci(&mut f, &ncsd, o, !opts.no_trim, keys, seeds, progress)? {
                    CciOutcome::Decrypted => Ok(true),
                    CciOutcome::AlreadyDecrypted => {
                        already = true;
                        Ok(false)
                    }
                }
            })?;
            Ok(if already {
                Outcome::AlreadyDecrypted
            } else {
                Outcome::Written(out)
            })
        }
        Format::Ncch => {
            let mut h = [0u8; 0x200];
            f.read_exact(&mut h)?;
            if NcchHeader::parse(&h)?.is_decrypted() {
                return Ok(Outcome::AlreadyDecrypted);
            }
            let ext = input
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| "ncch".into());
            let stem = if opts.name_from_title {
                let mut rd = |off: u64, buf: &mut [u8]| -> Result<()> {
                    f.seek(SeekFrom::Start(off))?;
                    f.read_exact(buf)?;
                    Ok(())
                };
                title_stem(
                    ncch::read_ncch_icon(&mut rd, keys, seeds).ok().flatten(),
                    opts.lang,
                )
                .unwrap_or(default_stem)
            } else {
                default_stem
            };
            let out = naming::output_path(input, out_dir, &stem, &format!("-decrypted.{ext}"));
            if out.exists() && !opts.overwrite {
                return Ok(Outcome::OutputExists(out));
            }
            let len = f.metadata()?.len();
            f.seek(SeekFrom::Start(0))?;
            write_atomically(&out, |o| {
                let mut src = |b: &mut [u8]| -> Result<()> { Ok(f.read_exact(b)?) };
                let mut done = 0u64;
                let mut sink = |b: &[u8]| -> Result<()> {
                    o.write_all(b)?;
                    done += b.len() as u64;
                    if !progress(done, len) {
                        return Err(crate::Cancelled.into());
                    }
                    Ok(())
                };
                ncch::stream_ncch(len, &mut src, keys, seeds, &mut sink)?;
                Ok(true)
            })?;
            Ok(Outcome::Written(out))
        }
    }
}

fn describe_ncch(out: &mut String, label: &str, h: &NcchHeader) {
    use std::fmt::Write as _;
    let _ = writeln!(
        out,
        "  {label}: {} program {:016X}, {} MiB, crypto: {}",
        if h.product_code.is_empty() {
            "?"
        } else {
            &h.product_code
        },
        h.program_id,
        h.content_size >> 20,
        h.crypto_description()
    );
}

fn describe_titles(out: &mut String, smdh: Option<Vec<u8>>) {
    use std::fmt::Write as _;
    let Some(titles) = smdh.as_deref().and_then(ncch::parse_smdh) else {
        let _ = writeln!(out, "  Title names: (no SMDH icon found)");
        return;
    };
    let _ = writeln!(out, "  Title names:");
    // Group languages that share the same text so the list stays short.
    let mut groups: Vec<(Vec<&str>, &AppTitle)> = Vec::new();
    for (i, t) in titles.iter().enumerate().take(12) {
        if t.short.is_empty() && t.long.is_empty() {
            continue;
        }
        match groups
            .iter_mut()
            .find(|(_, g)| g.short == t.short && g.long == t.long && g.publisher == t.publisher)
        {
            Some((langs, _)) => langs.push(SMDH_LANGS[i]),
            None => groups.push((vec![SMDH_LANGS[i]], t)),
        }
    }
    for (langs, t) in groups {
        let long = t.long.replace('\n', " ");
        let _ = writeln!(out, "    [{}]", langs.join(", "));
        let _ = writeln!(out, "      Short:     {}", t.short);
        let _ = writeln!(out, "      Long:      {long}");
        let _ = writeln!(out, "      Publisher: {}", t.publisher);
    }
}

/// Human-readable description of a file, like ctrtool's summary.
pub fn info(input: &Path, keys: &KeyStore, seeds: &SeedDb) -> Result<String> {
    use std::fmt::Write as _;
    let mut f = File::open(input).with_context(|| format!("cannot open {}", input.display()))?;
    let mut s = String::new();
    let _ = writeln!(s, "{}", input.display());
    match detect(&mut f)? {
        Format::Cia => {
            let cia = Cia::open(&mut f)?;
            let _ = writeln!(s, "  Format: CIA");
            let _ = writeln!(
                s,
                "  Title ID: {:016X} ({})  Version: {} ({}.{}.{})",
                cia.title_id,
                cia::title_kind(cia.title_id),
                cia.title_version,
                cia.title_version >> 10,
                (cia.title_version >> 4) & 0x3F,
                cia.title_version & 0xF
            );
            let _ = writeln!(s, "  Common key index: {}", cia.common_key_index);
            let tk = if cia.any_encrypted() {
                cia.title_key(keys).ok()
            } else {
                None
            };
            let headers = cia::content_headers(&mut f, &cia, tk.as_ref());
            for (c, h) in cia.contents.iter().zip(headers) {
                let _ = writeln!(
                    s,
                    "  Content {:04X} id {:08x}: {} bytes, CIA layer {}",
                    c.index,
                    c.id,
                    c.size,
                    if c.encrypted() {
                        "encrypted"
                    } else {
                        "decrypted"
                    }
                );
                match h {
                    Some(h) => describe_ncch(&mut s, "    NCCH", &h),
                    None if c.encrypted() && tk.is_none() => {
                        let _ = writeln!(
                            s,
                            "    (cannot read: title key unavailable, check aes_keys.txt)"
                        );
                    }
                    None => {
                        let _ = writeln!(s, "    (not an NCCH, e.g. a DSi/TWL SRL)");
                    }
                }
            }
            describe_titles(&mut s, cia::read_smdh(&mut f, &cia, keys, seeds));
        }
        Format::Cci => {
            let ncsd = Ncsd::open(&mut f)?;
            let _ = writeln!(s, "  Format: CCI (NCSD)");
            let _ = writeln!(s, "  Media ID: {:016X}", ncsd.media_id);
            for &(idx, off, size) in &ncsd.partitions {
                let label = format!("Partition {idx} {} @ {off:#x}", cci::PARTITION_NAMES[idx]);
                match ncsd.partition_header(&mut f, idx) {
                    Some(h) => describe_ncch(&mut s, &label, &h),
                    None => {
                        let _ = writeln!(s, "  {label}: {size} bytes, not an NCCH");
                    }
                }
            }
            describe_titles(&mut s, cci::read_smdh(&mut f, &ncsd, keys, seeds));
        }
        Format::Ncch => {
            let mut h = [0u8; 0x200];
            f.read_exact(&mut h)?;
            let h = NcchHeader::parse(&h)?;
            let _ = writeln!(s, "  Format: NCCH");
            describe_ncch(&mut s, "NCCH", &h);
            let mut rd = |off: u64, buf: &mut [u8]| -> Result<()> {
                f.seek(SeekFrom::Start(off))?;
                f.read_exact(buf)?;
                Ok(())
            };
            describe_titles(
                &mut s,
                ncch::read_ncch_icon(&mut rd, keys, seeds).ok().flatten(),
            );
        }
    }
    Ok(s)
}
