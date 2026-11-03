//! Structural/magic-signature recognition for NAND dumps and SD-card images -- see
//! `docs/plan/bios.md`'s "NAND/large-firmware class" section for the full design.
//!
//! **Deliberately not wired into the per-file ROM detection cycle.** Every existing
//! signature check in `disc_dispatch.rs`/`platforms::mycom` feeds `platforms::deep_try_platform`
//! (via `has_content_signature_parser`/`read_non_disc_header_metadata`), which treats the
//! file as a candidate ROM for one or more platforms. A NAND/SD-image dump is never that
//! -- it isn't a game, so trying it against the cycle would be a category error, not just
//! a wasted read. These functions are meant to run behind a directory-context-gated
//! pre-filter instead (a file already sitting in a known NAND-capable platform's
//! directory, sized like a plausible NAND/SD dump rather than a small BIOS file) -- that
//! trigger depends on the file-scanning/ownership-table pass this project doesn't have
//! yet (see `docs/plan/database.md`'s "Implementation status"), so this module is the
//! standalone, unit-testable signature layer, ready for whenever that scanner exists.
//!
//! Unlike a BIOS file, a NAND dump can't be identified by hash against a shared catalog
//! -- it bakes in per-console keys/CID, so no two consoles' NAND dumps are byte-identical.
//! These functions only classify "this looks structurally like a NAND/SD-image dump of
//! platform X", never confirm identity the way `retrobios` hash-matching does for BIOS
//! files.
//!
//! Confidence varies a lot by platform (see each section below) and is stated honestly
//! rather than presented as uniform -- some of this was verified against primary specs
//! (UEFI GPT, GBATEK), some against a single secondary source that couldn't be
//! cross-checked during research.

use std::path::Path;

use super::disc_fs::{read_exact_at, read_u16_le, read_u32_le};

// ---------------------------------------------------------------------------
// Nintendo Switch: GPT "EFI PART" at LBA1 -- high confidence, rawnand/GPP only
// ---------------------------------------------------------------------------

const GPT_SIGNATURE: &[u8] = b"EFI PART";
/// LBA1 on a 512-byte-sector disk: byte offset 0x200, right after the protective MBR at
/// LBA0. Confirmed against the UEFI spec directly (not scanned/approximate).
const GPT_SIGNATURE_OFFSET: u64 = 0x200;

/// A Nintendo Switch full NAND backup (BOOT0 + BOOT1 + rawnand, Hekate's own dump
/// convention) has its GPT-partitioned `rawnand`/GPP portion start with a protective MBR
/// at LBA0 followed by the real GPT header at LBA1 -- the standard UEFI GPT signature.
///
/// **Not Switch-specific by itself** -- any GPT-partitioned disk image matches this; it
/// only becomes a Switch signal combined with the directory-context precondition (already
/// in a Switch platform directory). **Does not cover BOOT0/BOOT1** -- those are small raw
/// partition dumps with no verified signature found during research; not guessed here.
pub fn switch_nand_gpt_signature(path: &Path) -> Option<bool> {
    let bytes = read_exact_at(path, GPT_SIGNATURE_OFFSET, GPT_SIGNATURE.len()).ok()?;
    Some(bytes == GPT_SIGNATURE)
}

pub fn switch_nand_gpt_signature_from_bytes(bytes: &[u8]) -> bool {
    let start = GPT_SIGNATURE_OFFSET as usize;
    bytes.get(start..start + GPT_SIGNATURE.len()) == Some(GPT_SIGNATURE)
}

// ---------------------------------------------------------------------------
// Nintendo DSi NAND: nocash/no$gba-style footer -- medium confidence, footer-dependent
// ---------------------------------------------------------------------------

const DSI_FOOTER_MARKER: &[u8] = b"DSi eMMC CID/CPU";
/// The footer holding the eMMC CID + console ID (needed for decryption) is appended by
/// dumping tools (dumpTool, SafeNANDManager) at an offset that varies by eMMC vendor
/// (Samsung vs. ST use different conventions/sizes) -- no single fixed offset exists, so
/// this scans a generous tail window instead of trusting one number. Same tolerant-scan
/// philosophy as `chd_disc::ChdDisc::grow_prefix`, just anchored to the end of the file.
const DSI_FOOTER_SCAN_WINDOW: u64 = 2 * 1024 * 1024;

/// **Real limitation, not papered over**: a raw NAND dump made without this optional
/// footer (some tools/workflows skip it) will not be positively identifiable this way --
/// medium confidence, never a hard match.
pub fn dsi_nand_footer_signature(path: &Path) -> Option<bool> {
    let size = std::fs::metadata(path).ok()?.len();
    let window = DSI_FOOTER_SCAN_WINDOW.min(size);
    let start = size - window;
    let tail = read_exact_at(path, start, window as usize).ok()?;
    Some(dsi_nand_footer_signature_from_bytes(&tail))
}

pub fn dsi_nand_footer_signature_from_bytes(bytes: &[u8]) -> bool {
    bytes.windows(DSI_FOOTER_MARKER.len()).any(|window| window == DSI_FOOTER_MARKER)
}

// ---------------------------------------------------------------------------
// Xbox 360 NAND: composite structural header -- medium confidence, single-source
// ---------------------------------------------------------------------------

fn read_u32_be(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?))
}

/// Not a single magic -- a composite structural check across several header fields,
/// closer in spirit to `platforms::mycom`'s PC-98 MBR+IPL-string composite check than to
/// a plain magic-offset lookup. Field values are read big-endian: the Xbox 360's Xenon
/// CPU is PowerPC, and every other big-endian-architecture magic already in this codebase
/// (GameCube/Wii's disc magics in `disc_dispatch.rs`) follows the same convention -- but
/// this specific choice was **not independently confirmed** against a primary source this
/// session (see below), so treat it as the best available assumption, not a verified fact.
///
/// `bytes` is just the header window (0x80 bytes is enough -- every field this check
/// reads sits at or before offset 0x7C); `file_len` is the *real* file size, needed
/// separately since the pointer fields point deep into the multi-MB/GB NAND image, far
/// past the small header window itself.
///
/// **Flag honestly**: verified against one auto-summarized source (the Free60 wiki); the
/// primary reference (ConsoleMods "Standard NAND") returned HTTP 403 during research and
/// was never cross-checked. Recommend confirming exact field offsets/endianness against
/// that page (or another mirror) before relying on this for more than a soft/contextual
/// classification.
pub fn xbox360_nand_signature_from_bytes(bytes: &[u8], file_len: u64) -> bool {
    let Some(0xFF) = bytes.first().copied() else { return false };
    let points_in_range = |offset: usize| read_u32_be(bytes, offset).is_some_and(|value| value != 0 && (value as u64) < file_len);
    // CB (bootloader) offset 0x08, CF1 offset 0x0C, keyvault offset 0x6C, SMC offset 0x7C
    // -- pointer fields that should all land inside the file. SMC length (0x78) is a
    // length, not a pointer, so it only needs to be non-zero and no larger than the file.
    let pointers_ok = points_in_range(0x08) && points_in_range(0x0C) && points_in_range(0x6C) && points_in_range(0x7C);
    let smc_len_ok = read_u32_be(bytes, 0x78).is_some_and(|value| value != 0 && (value as u64) < file_len);
    pointers_ok && smc_len_ok
}

pub fn xbox360_nand_signature(path: &Path) -> Option<bool> {
    let file_len = std::fs::metadata(path).ok()?.len();
    // 0x80 bytes covers every field this check reads (up to the SMC offset at 0x7C).
    let bytes = read_exact_at(path, 0, 0x80).ok()?;
    Some(xbox360_nand_signature_from_bytes(&bytes, file_len))
}

// ---------------------------------------------------------------------------
// Generic partitioned/FAT disk image check -- shared primitive, not platform-specific
// ---------------------------------------------------------------------------

/// MBR boot signature at byte 510-511. Same primitive `platforms::mycom`'s PC-98
/// hard-disk check already uses (`0x55 0xAA` at 510) -- shared here rather than
/// duplicated, since both the DSi SD-card heuristic and the Nintendo SD-family raw-image
/// reader below need to confirm "this is a partitioned disk image" before going further.
pub fn has_mbr_signature(bytes: &[u8]) -> bool {
    bytes.get(510..512) == Some(&[0x55, 0xAA])
}

pub fn is_partitioned_disk_image(path: &Path) -> Option<bool> {
    let head = read_exact_at(path, 0, 512).ok()?;
    Some(has_mbr_signature(&head) || switch_nand_gpt_signature_from_bytes(&read_exact_at(path, 0, 0x208).ok()?))
}

// ---------------------------------------------------------------------------
// Nintendo SD card family: Wii/3DS/Switch root-folder markers
// ---------------------------------------------------------------------------
//
// Confirmed this session: Wii, 3DS and Switch each write a distinctive, documented
// root-level folder structure to any SD card they've used -- a much stronger, more
// specific signal than a generic MBR/GPT check, since it identifies *which* platform, not
// just "this is a disk image". Two mechanisms, by how the SD content is presented:
//
// 1. Already-unpacked directory tree (the common case -- these platforms' emulation
//    normally works from an unpacked `sdmc`-style folder, not a raw image): a plain
//    root-level presence check, see `sd_family_marker_in_directory`.
// 2. Raw disk-image file (`.img`/`.bin`): needs the root directory's entries without a
//    full FAT driver or full extraction, see `sd_family_marker_in_fat_image` below.

/// One marker folder's short (8.3) name and which platform it identifies. Matched by
/// short name only (see [`sd_family_marker_in_fat_image`]'s doc comment for why).
const WII_MARKERS: &[&str] = &["PRIVATE", "APPS"];
/// FAT's numeric-tail collision suffix for a long name that doesn't fit 8.3 -- "Nintendo
/// 3DS" sanitizes to `NINTEN~1` (first 6 chars of the basename + `~1`), confirmed via
/// 3dbrew's SD Filesystem page for the long name; the short-name algorithm itself is the
/// standard FAT/VFAT spec. Matched by prefix to tolerate a `~2`/`~3` collision suffix.
const NINTENDO_3DS_MARKER_PREFIX: &str = "NINTEN~";
/// Switch's own `Nintendo` folder is exactly 8 characters -- fits 8.3 with no truncation,
/// so its short name is the literal name, no tilde.
const SWITCH_NINTENDO_MARKER: &str = "NINTENDO";
/// `atmosphere` (10 chars) truncates to `ATMOSPH~1` (7 + `~1`).
const SWITCH_ATMOSPHERE_MARKER_PREFIX: &str = "ATMOSPH~";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NintendoSdPlatform {
    Wii,
    Nintendo3ds,
    Switch,
}

/// Mechanism 1: an already-unpacked SD card directory tree. Trivial presence check, no
/// byte parsing -- `docs/plan/bios.md`'s own scope note (this heuristic only applies
/// within an already-platform-resolved directory) still applies; this just confirms the
/// SD-family marker within it.
pub fn sd_family_marker_in_directory(root: &Path) -> Option<NintendoSdPlatform> {
    if root.join("private").join("wii").is_dir() || root.join("apps").is_dir() {
        return Some(NintendoSdPlatform::Wii);
    }
    if root.join("Nintendo 3DS").is_dir() {
        return Some(NintendoSdPlatform::Nintendo3ds);
    }
    if root.join("Nintendo").join("Contents").join("registered").is_dir() || root.join("atmosphere").is_dir() {
        return Some(NintendoSdPlatform::Switch);
    }
    None
}

/// A minimal FAT12/16/32 boot-sector parse, just enough to locate the root directory
/// region -- not a general FAT driver. FAT32's root directory is a cluster chain in the
/// data area rather than a fixed sector range like FAT12/16; this reads only its *first*
/// cluster, which covers the overwhelming majority of real root directories (a handful of
/// top-level folders rarely spans a cluster boundary). A root directory that does span
/// multiple clusters simply won't have its later entries seen -- a known, documented
/// limitation, not a silent wrong answer.
struct FatRootDir {
    offset: u64,
    len: usize,
}

fn locate_fat_root_dir(boot_sector: &[u8]) -> Option<FatRootDir> {
    let bytes_per_sector = read_u16_le(boot_sector, 0x0B)? as u64;
    let sectors_per_cluster = *boot_sector.get(0x0D)? as u64;
    let reserved_sectors = read_u16_le(boot_sector, 0x0E)? as u64;
    let num_fats = *boot_sector.get(0x10)? as u64;
    let root_entry_count = read_u16_le(boot_sector, 0x11)? as u64;
    let sectors_per_fat16 = read_u16_le(boot_sector, 0x16)? as u64;

    if bytes_per_sector == 0 || sectors_per_cluster == 0 {
        return None;
    }

    if root_entry_count > 0 && sectors_per_fat16 > 0 {
        // FAT12/16: a fixed-size root directory region right after the FAT copies.
        let root_dir_sector = reserved_sectors + num_fats * sectors_per_fat16;
        let root_dir_bytes = (root_entry_count * 32).div_ceil(bytes_per_sector) * bytes_per_sector;
        return Some(FatRootDir { offset: root_dir_sector * bytes_per_sector, len: root_dir_bytes as usize });
    }

    // FAT32: root directory is a cluster chain starting at `root_cluster` (BPB32 offset
    // 0x2C); read only its first cluster (see doc comment above).
    let sectors_per_fat32 = read_u32_le(boot_sector, 0x24)? as u64;
    let root_cluster = read_u32_le(boot_sector, 0x2C)? as u64;
    if sectors_per_fat32 == 0 || root_cluster < 2 {
        return None;
    }
    let data_start_sector = reserved_sectors + num_fats * sectors_per_fat32;
    let cluster_sector = data_start_sector + (root_cluster - 2) * sectors_per_cluster;
    Some(FatRootDir { offset: cluster_sector * bytes_per_sector, len: (sectors_per_cluster * bytes_per_sector) as usize })
}

/// Short (8.3) directory-entry names at FAT's root directory. Long-filename (VFAT LFN)
/// entries are deliberately not parsed -- FAT always writes a short name alongside any
/// long name for DOS back-compat, so matching the short name is sufficient here and much
/// simpler than reconstructing UTF-16 LFN fragments.
fn fat_root_dir_short_names(root_dir: &[u8]) -> Vec<String> {
    const DIR_ENTRY_SIZE: usize = 32;
    const ATTR_DIRECTORY: u8 = 0x10;
    const ATTR_LFN: u8 = 0x0F;
    const ATTR_VOLUME_LABEL: u8 = 0x08;

    let mut names = Vec::new();
    for entry in root_dir.chunks_exact(DIR_ENTRY_SIZE) {
        match entry[0] {
            0x00 => break,       // end of directory
            0xE5 => continue,    // deleted entry
            _ => {}
        }
        let attr = entry[0x0B];
        if attr == ATTR_LFN || attr & ATTR_VOLUME_LABEL != 0 || attr & ATTR_DIRECTORY == 0 {
            continue;
        }
        let name = String::from_utf8_lossy(&entry[0..8]).trim_end().to_string();
        if !name.is_empty() {
            names.push(name);
        }
    }
    names
}

/// Mechanism 2: a raw `.img`/`.bin` disk image. Parses just the boot sector + root
/// directory entries (see [`locate_fat_root_dir`]'s scoping notes) -- no extraction, no
/// full FAT driver.
pub fn sd_family_marker_in_fat_image(path: &Path) -> Option<NintendoSdPlatform> {
    let boot_sector = read_exact_at(path, 0, 512).ok()?;
    let root_dir = locate_fat_root_dir(&boot_sector)?;
    let bytes = read_exact_at(path, root_dir.offset, root_dir.len).ok()?;
    sd_family_marker_from_short_names(&fat_root_dir_short_names(&bytes))
}

fn sd_family_marker_from_short_names(names: &[String]) -> Option<NintendoSdPlatform> {
    if names.iter().any(|name| WII_MARKERS.contains(&name.as_str())) {
        return Some(NintendoSdPlatform::Wii);
    }
    if names.iter().any(|name| name.starts_with(NINTENDO_3DS_MARKER_PREFIX)) {
        return Some(NintendoSdPlatform::Nintendo3ds);
    }
    if names.iter().any(|name| name == SWITCH_NINTENDO_MARKER || name.starts_with(SWITCH_ATMOSPHERE_MARKER_PREFIX)) {
        return Some(NintendoSdPlatform::Switch);
    }
    None
}

// ---------------------------------------------------------------------------
// Nintendo DSi SD card image -- contextual, not a true platform signature
// ---------------------------------------------------------------------------

/// No DSi-specific byte signature exists for a raw SD card image (it's just a
/// FAT-formatted disk). Per the confirmed design: classify a raw image as a likely DSi SD
/// card only when it (1) *is* a partitioned/FAT disk image, (2) matches none of the three
/// Nintendo SD-family folder markers above, and (3) isn't a known older system's HDD
/// image (reusing `platforms::mycom`'s PC-98 MBR+IPL-string exclusion). This is a
/// medium/contextual classification, never a hard match -- it depends on the
/// directory-context precondition (already in a DSi platform directory) documented in
/// `docs/plan/bios.md`, which this function does not itself check.
pub fn is_likely_dsi_sd_card_image(path: &Path) -> Option<bool> {
    if !is_partitioned_disk_image(path)? {
        return Some(false);
    }
    if sd_family_marker_in_fat_image(path).is_some() {
        return Some(false);
    }
    if matches!(crate::rom::platforms::mycom::verdict_for_file(path, crate::common::models::Platform::Pc98), crate::rom::platforms::mycom::MycomVerdict::Match(_)) {
        return Some(false);
    }
    Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn padded(bytes: &[u8], len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        buf[..bytes.len()].copy_from_slice(bytes);
        buf
    }

    #[test]
    fn switch_gpt_signature_matches_at_lba1() {
        let mut buf = vec![0u8; 0x300];
        buf[0x200..0x208].copy_from_slice(GPT_SIGNATURE);
        assert!(switch_nand_gpt_signature_from_bytes(&buf));
    }

    #[test]
    fn switch_gpt_signature_rejects_plain_mbr() {
        let mut buf = vec![0u8; 0x300];
        buf[510] = 0x55;
        buf[511] = 0xAA;
        assert!(!switch_nand_gpt_signature_from_bytes(&buf));
    }

    #[test]
    fn dsi_footer_marker_found_anywhere_in_tail() {
        let mut buf = vec![0u8; 4096];
        let marker_at = 1234;
        buf[marker_at..marker_at + DSI_FOOTER_MARKER.len()].copy_from_slice(DSI_FOOTER_MARKER);
        assert!(dsi_nand_footer_signature_from_bytes(&buf));
    }

    #[test]
    fn dsi_footer_marker_absent() {
        let buf = vec![0u8; 4096];
        assert!(!dsi_nand_footer_signature_from_bytes(&buf));
    }

    fn xbox360_header_fixture() -> Vec<u8> {
        let mut buf = vec![0u8; 0x100];
        buf[0] = 0xFF;
        buf[0x08..0x0C].copy_from_slice(&40u32.to_be_bytes());
        buf[0x0C..0x10].copy_from_slice(&80u32.to_be_bytes());
        buf[0x6C..0x70].copy_from_slice(&120u32.to_be_bytes());
        buf[0x78..0x7C].copy_from_slice(&4096u32.to_be_bytes());
        buf[0x7C..0x80].copy_from_slice(&160u32.to_be_bytes());
        buf
    }

    /// A generous fake file size -- larger than any pointer value the fixture uses, and
    /// larger than the 0x100-byte header window itself, mirroring the real gap between a
    /// small header and a multi-MB/GB NAND image.
    const XBOX360_FIXTURE_FILE_LEN: u64 = 512 * 1024 * 1024;

    #[test]
    fn xbox360_signature_matches_well_formed_header() {
        assert!(xbox360_nand_signature_from_bytes(&xbox360_header_fixture(), XBOX360_FIXTURE_FILE_LEN));
    }

    #[test]
    fn xbox360_signature_rejects_wrong_first_byte() {
        let mut buf = xbox360_header_fixture();
        buf[0] = 0x00;
        assert!(!xbox360_nand_signature_from_bytes(&buf, XBOX360_FIXTURE_FILE_LEN));
    }

    #[test]
    fn xbox360_signature_rejects_zeroed_pointers() {
        let mut buf = xbox360_header_fixture();
        buf[0x08..0x0C].copy_from_slice(&0u32.to_be_bytes());
        assert!(!xbox360_nand_signature_from_bytes(&buf, XBOX360_FIXTURE_FILE_LEN));
    }

    #[test]
    fn xbox360_signature_rejects_pointer_past_file_end() {
        let mut buf = xbox360_header_fixture();
        // A pointer value that's in-range against a huge file must still fail against a
        // file too small to contain it -- catches the exact bug this test suite found
        // (comparing pointers against the header buffer's length instead of the real
        // file size, which would reject every legitimate pointer).
        assert!(!xbox360_nand_signature_from_bytes(&buf, 32));
        buf[0x7C..0x80].copy_from_slice(&(64u32).to_be_bytes());
        assert!(!xbox360_nand_signature_from_bytes(&buf, 32));
    }

    #[test]
    fn mbr_signature_detected() {
        let buf = padded(&[], 512).into_iter().enumerate().map(|(i, b)| if i == 510 { 0x55 } else if i == 511 { 0xAA } else { b }).collect::<Vec<u8>>();
        assert!(has_mbr_signature(&buf));
    }

    fn fat16_boot_sector_with_root_entries(entries: &[(&str, u8)]) -> Vec<u8> {
        // A minimal, synthetic FAT16 boot sector: 512-byte sectors, 1 sector/cluster, 1
        // reserved sector, 1 FAT of 1 sector, root dir sized to hold the given entries.
        let mut boot = vec![0u8; 512];
        let bytes_per_sector: u16 = 512;
        let root_entry_count: u16 = 16;
        boot[0x0B..0x0D].copy_from_slice(&bytes_per_sector.to_le_bytes());
        boot[0x0D] = 1; // sectors per cluster
        boot[0x0E..0x10].copy_from_slice(&1u16.to_le_bytes()); // reserved sectors
        boot[0x10] = 1; // number of FATs
        boot[0x11..0x13].copy_from_slice(&root_entry_count.to_le_bytes());
        boot[0x16..0x18].copy_from_slice(&1u16.to_le_bytes()); // sectors per FAT16

        // Root dir starts right after reserved + FAT sectors == sector 2 == byte 1024.
        let root_dir_bytes = (root_entry_count as usize * 32).div_ceil(bytes_per_sector as usize) * bytes_per_sector as usize;
        let mut root_dir = vec![0u8; root_dir_bytes];
        for (i, (name, attr)) in entries.iter().enumerate() {
            let entry_start = i * 32;
            let mut short_name = [b' '; 8];
            let name_bytes = name.as_bytes();
            short_name[..name_bytes.len().min(8)].copy_from_slice(&name_bytes[..name_bytes.len().min(8)]);
            root_dir[entry_start..entry_start + 8].copy_from_slice(&short_name);
            root_dir[entry_start + 0x0B] = *attr;
        }

        let mut image = boot;
        image.extend(vec![0u8; 512]); // the 1 FAT sector, contents irrelevant to this check
        image.extend(root_dir);
        image
    }

    #[test]
    fn fat_image_finds_wii_marker() {
        let image = fat16_boot_sector_with_root_entries(&[("PRIVATE", 0x10)]);
        let boot_sector = &image[0..512];
        let root_dir = locate_fat_root_dir(boot_sector).expect("root dir located");
        let names = fat_root_dir_short_names(&image[root_dir.offset as usize..root_dir.offset as usize + root_dir.len]);
        assert_eq!(sd_family_marker_from_short_names(&names), Some(NintendoSdPlatform::Wii));
    }

    #[test]
    fn fat_image_finds_3ds_marker_with_collision_suffix() {
        let image = fat16_boot_sector_with_root_entries(&[("NINTEN~2", 0x10)]);
        let boot_sector = &image[0..512];
        let root_dir = locate_fat_root_dir(boot_sector).expect("root dir located");
        let names = fat_root_dir_short_names(&image[root_dir.offset as usize..root_dir.offset as usize + root_dir.len]);
        assert_eq!(sd_family_marker_from_short_names(&names), Some(NintendoSdPlatform::Nintendo3ds));
    }

    #[test]
    fn fat_image_finds_switch_marker_exact_name() {
        let image = fat16_boot_sector_with_root_entries(&[("NINTENDO", 0x10)]);
        let boot_sector = &image[0..512];
        let root_dir = locate_fat_root_dir(boot_sector).expect("root dir located");
        let names = fat_root_dir_short_names(&image[root_dir.offset as usize..root_dir.offset as usize + root_dir.len]);
        assert_eq!(sd_family_marker_from_short_names(&names), Some(NintendoSdPlatform::Switch));
    }

    #[test]
    fn fat_image_no_marker_found() {
        let image = fat16_boot_sector_with_root_entries(&[("RANDOM", 0x10)]);
        let boot_sector = &image[0..512];
        let root_dir = locate_fat_root_dir(boot_sector).expect("root dir located");
        let names = fat_root_dir_short_names(&image[root_dir.offset as usize..root_dir.offset as usize + root_dir.len]);
        assert_eq!(sd_family_marker_from_short_names(&names), None);
    }

    #[test]
    fn lfn_entries_are_skipped_not_matched() {
        // Attribute 0x0F marks an LFN fragment -- even if its bytes happened to spell a
        // marker name, it must not be treated as a short-name directory entry.
        let image = fat16_boot_sector_with_root_entries(&[("NINTENDO", 0x0F)]);
        let boot_sector = &image[0..512];
        let root_dir = locate_fat_root_dir(boot_sector).expect("root dir located");
        let names = fat_root_dir_short_names(&image[root_dir.offset as usize..root_dir.offset as usize + root_dir.len]);
        assert!(names.is_empty());
    }
}
