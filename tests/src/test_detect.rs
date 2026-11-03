//! Unit/acceptance tests for the rom module's detection layer (heuristics, header parsers,
//! the 4-mode flow).
//!


use std::fs;
use std::path::{Path, PathBuf};

#[path = "support/fixtures.rs"]
mod fixtures;

use romexplorer::rom::detect::{
    self, descriptor, disc_dispatch, infer_media_type_from_heuristics, infer_platforms_from_extension_heuristics,
    narrow_extension_candidates_by_path, platform_compatible_for_cycle, reason::RomReason,
};
use romexplorer::rom::models::RomByteOrder;
use romexplorer::rom::platforms::{mycom, nintendo, other, sega, sony};
use romexplorer::common::models::{MediaType, Platform};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("romexplorer_test_archiver_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("failed to create temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Creates `relative` (creating parent directories) with `contents`.
    fn write(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("failed to create parent dir");
        }
        fs::write(&path, contents).expect("failed to write temp file");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn buffer(len: usize) -> Vec<u8> {
    vec![0u8; len]
}

fn put(buf: &mut [u8], offset: usize, bytes: &[u8]) {
    buf[offset..offset + bytes.len()].copy_from_slice(bytes);
}

/// The 156-byte Nintendo boot logo GBA (at 0x04) and DS/DSi (at 0xC0) headers carry.
const NINTENDO_LOGO_156: [u8; 156] = [
    0x24, 0xFF, 0xAE, 0x51, 0x69, 0x9A, 0xA2, 0x21, 0x3D, 0x84, 0x82, 0x0A, 0x84, 0xE4, 0x09, 0xAD, 0x11, 0x24, 0x8B,
    0x98, 0xC0, 0x81, 0x7F, 0x21, 0xA3, 0x52, 0xBE, 0x19, 0x93, 0x09, 0xCE, 0x20, 0x10, 0x46, 0x4A, 0x4A, 0xF8, 0x27,
    0x31, 0xEC, 0x58, 0xC7, 0xE8, 0x33, 0x82, 0xE3, 0xCE, 0xBF, 0x85, 0xF4, 0xDF, 0x94, 0xCE, 0x4B, 0x09, 0xC1, 0x94,
    0x56, 0x8A, 0xC0, 0x13, 0x72, 0xA7, 0xFC, 0x9F, 0x84, 0x4D, 0x73, 0xA3, 0xCA, 0x9A, 0x61, 0x58, 0x97, 0xA3, 0x27,
    0xFC, 0x03, 0x98, 0x76, 0x23, 0x1D, 0xC7, 0x61, 0x03, 0x04, 0xAE, 0x56, 0xBF, 0x38, 0x84, 0x00, 0x40, 0xA7, 0x0E,
    0xFD, 0xFF, 0x52, 0xFE, 0x03, 0x6F, 0x95, 0x30, 0xF1, 0x97, 0xFB, 0xC0, 0x85, 0x60, 0xD6, 0x80, 0x25, 0xA9, 0x63,
    0xBE, 0x03, 0x01, 0x4E, 0x38, 0xE2, 0xF9, 0xA2, 0x34, 0xFF, 0xBB, 0x3E, 0x03, 0x44, 0x78, 0x00, 0x90, 0xCB, 0x88,
    0x11, 0x3A, 0x94, 0x65, 0xC0, 0x7C, 0x63, 0x87, 0xF0, 0x3C, 0xAF, 0xD6, 0x25, 0xE4, 0x8B, 0x38, 0x0A, 0xAC, 0x72,
    0x21, 0xD4, 0xF8, 0x07,
];

/// Sets the GBA header's fixed byte and complement check over whatever fields are in place.
fn seal_gba_header(rom: &mut [u8]) {
    put(rom, 0x04, &NINTENDO_LOGO_156);
    rom[0xB2] = 0x96;
    let sum = rom[0xA0..=0xBC].iter().fold(0u8, |acc, &b| acc.wrapping_sub(b)).wrapping_sub(0x19);
    rom[0xBD] = sum;
}

/// Sets the Game Boy header checksum over whatever fields are in place.
fn seal_gameboy_header(rom: &mut [u8]) {
    rom[0x14D] = rom[0x134..=0x14C].iter().fold(0u8, |acc, &b| acc.wrapping_sub(b).wrapping_sub(1));
}

// ---------------------------------------------------------------------------
// The GD-ROM cue/gdi preference fix
// ---------------------------------------------------------------------------

// reference/old_archiver.rs did `cue_member.or(gdi_member)` (line 1596), always preferring
// .cue when both were present -- contradicting docs/plan/archive_module.md's explicit "gdi
// is preferred over cue". This asserts the corrected order. Old code had no test for this.
#[test]
fn sidecar_descriptor_prefers_gdi_over_cue() {
    let dir = TempDir::new("sidecar_pref");
    dir.write("game.chd", b"MComprHD");
    dir.write("game.cue", b"FILE \"game.bin\" BINARY\n");
    dir.write("game.gdi", b"3\n");

    let found = detect::find_sidecar_descriptor(&dir.path().join("game.chd")).expect("expected a sidecar descriptor");

    assert_eq!(found.extension().and_then(|e| e.to_str()), Some("gdi"), "gdi must win when both descriptors exist");
}

#[test]
fn sidecar_descriptor_falls_back_to_cue_when_no_gdi() {
    let dir = TempDir::new("sidecar_cue_only");
    dir.write("game.chd", b"MComprHD");
    dir.write("game.cue", b"FILE \"game.bin\" BINARY\n");

    let found = detect::find_sidecar_descriptor(&dir.path().join("game.chd")).expect("expected a sidecar descriptor");

    assert_eq!(found.extension().and_then(|e| e.to_str()), Some("cue"));
}

// ---------------------------------------------------------------------------
// Extension narrows candidates; it never reaches the per-platform parsers
// ---------------------------------------------------------------------------

// Per-platform parsers answer "is this file mine, by its content?" and must answer it for
// any extension, or none. Extension-based exclusion lives solely in the extension
// heuristic, which narrows the candidate list before the cycle runs -- and which the caller
// can switch off. reference/old_archiver.rs instead buried extension vetoes inside these
// parsers, so a misnamed dump was undetectable whatever the heuristic settings said.
#[test]
fn virtual_boy_parser_accepts_its_shape_under_any_extension() {
    let dir = TempDir::new("vb_any_ext");
    let mut rom = buffer(0x20000);
    put(&mut rom, 0x20000 - 544, b"VUE DATA TEST TITLE ");
    put(&mut rom, 0x20000 - 544 + 0x19, b"TST000"); // maker + game code (5 reserved zero bytes precede it)
    put(&mut rom, 0x20000 - 16, &[0x60, 0xBD, 0xF0, 0xFF]); // reset vector code

    for name in ["game.vb", "game.bin", "game.gb", "game"] {
        let path = dir.write(name, &rom);
        assert!(
            other::parse_virtual_boy_rom_shape(&path).is_some(),
            "{name}: the parser must judge on content, not on the extension"
        );
    }
}

#[test]
fn sg1000_parser_accepts_its_shape_under_any_extension() {
    let dir = TempDir::new("sg_any_ext");
    let mut rom = buffer(0x4000);
    rom[0] = 0xC3; // Z80 JP entry opcode

    for name in ["game.sg", "game.rom", "game.sms", "game"] {
        let path = dir.write(name, &rom);
        assert!(other_sg1000_parses(&path), "{name}: the parser must judge on content, not on the extension");
    }
}

fn other_sg1000_parses(path: &Path) -> bool {
    sega::parse_sg1000_rom_shape(path).is_some()
}

// The exclusion that *does* happen lives here instead: the heuristic simply never puts
// Virtual Boy in the candidate list for a .gb file, so the VB parser is never asked.
#[test]
fn extension_heuristic_is_where_platform_exclusion_happens() {
    let gb_candidates = infer_platforms_from_extension_heuristics("gb");
    assert_eq!(gb_candidates, vec![Platform::GameBoy]);
    assert!(!gb_candidates.contains(&Platform::VirtualBoy));

    let vb_candidates = infer_platforms_from_extension_heuristics("vb");
    assert_eq!(vb_candidates, vec![Platform::VirtualBoy]);
}

// ...and an extension the heuristic doesn't recognize excludes nothing at all, which is
// what lets the cycle correct a misnamed file.
#[test]
fn an_unrecognized_extension_narrows_nothing() {
    assert!(infer_platforms_from_extension_heuristics("qqq").is_empty());
    assert!(infer_platforms_from_extension_heuristics("").is_empty());
}

// The WUX0 magic is proof on its own; old code additionally required a `.wux` name (5730).
#[test]
fn wiiu_container_is_detected_by_magic_regardless_of_extension() {
    let mut wux = buffer(64);
    put(&mut wux, 0, b"WUX0");

    assert!(nintendo::parse_wiiu_container_metadata_from_bytes(&wux).is_some());
    assert!(nintendo::parse_wiiu_container_metadata_from_bytes(&buffer(64)).is_none(), "a blank buffer is not a WUX");
}

// ---------------------------------------------------------------------------
// Candidate narrowing and compatibility
// ---------------------------------------------------------------------------

#[test]
fn path_heuristic_narrows_a_multi_candidate_extension() {
    let iso_candidates = infer_platforms_from_extension_heuristics("iso");
    assert!(iso_candidates.len() > 1, ".iso must be ambiguous across several platforms");

    let narrowed = narrow_extension_candidates_by_path(&iso_candidates, Platform::GameCube);
    assert_eq!(narrowed, Some(Platform::GameCube));
}

#[test]
fn narrowing_falls_back_to_a_compatible_candidate() {
    // TurboGrafx-16 isn't in .cue's candidate list, but TurboGrafx-CD is, and the two are
    // cycle-compatible.
    let cue_candidates = infer_platforms_from_extension_heuristics("cue");
    assert!(!cue_candidates.contains(&Platform::TurboGrafx16));

    assert_eq!(narrow_extension_candidates_by_path(&cue_candidates, Platform::TurboGrafx16), Some(Platform::TurboGrafxCd));
}

#[test]
fn revision_pairs_are_cycle_compatible_but_unrelated_platforms_are_not() {
    assert!(platform_compatible_for_cycle(Platform::Nes, Platform::Famicom));
    assert!(platform_compatible_for_cycle(Platform::NintendoDsi, Platform::NintendoDs));
    assert!(platform_compatible_for_cycle(Platform::Unknown, Platform::Wii), "Unknown is compatible with anything");
    assert!(!platform_compatible_for_cycle(Platform::Nes, Platform::Wii));
}

#[test]
fn media_type_inference_depends_on_both_extension_and_platform() {
    assert_eq!(infer_media_type_from_heuristics("gdi", Platform::Dreamcast), MediaType::GdRom);
    assert_eq!(infer_media_type_from_heuristics("gba", Platform::GameBoyAdvance), MediaType::Cartridge);
    assert_eq!(infer_media_type_from_heuristics("cas", Platform::Msx), MediaType::Tape);
    assert_eq!(infer_media_type_from_heuristics("d88", Platform::Pc8801), MediaType::FloppyDisk);
    // Same extension, different medium, decided by the platform.
    assert_eq!(infer_media_type_from_heuristics("chd", Platform::PlayStation), MediaType::CdRom);
    assert_eq!(infer_media_type_from_heuristics("chd", Platform::Xbox), MediaType::Dvd);
}

// ---------------------------------------------------------------------------
// Cartridge header parsers
// ---------------------------------------------------------------------------

#[test]
fn gba_header_yields_title_serial_and_maker() {
    let mut rom = buffer(0x200);
    put(&mut rom, 0x0A0, b"TEST TITLE");
    put(&mut rom, 0x0AC, b"TSTA");
    put(&mut rom, 0x0B0, b"01");
    seal_gba_header(&mut rom);

    let (title, serial, maker) = nintendo::parse_gba_header_metadata_from_bytes(&rom).expect("valid GBA header must parse");
    assert_eq!(title.as_deref(), Some("TEST TITLE"));
    assert_eq!(serial.as_deref(), Some("TSTA"));
    assert_eq!(maker.as_deref(), Some("01"));
}

#[test]
fn gba_header_without_its_magic_byte_is_rejected() {
    let mut rom = buffer(0x200);
    put(&mut rom, 0x0A0, b"NOT A GBA");
    assert!(nintendo::parse_gba_header_metadata_from_bytes(&rom).is_none());
}

#[test]
fn nds_header_requires_a_well_formed_game_code() {
    let dir = TempDir::new("nds_header");
    let mut rom = buffer(0x200);
    put(&mut rom, 0x00, b"TEST TITLE");
    put(&mut rom, 0x0C, b"TSTB");
    put(&mut rom, 0x10, b"01");
    put(&mut rom, 0xC0, &NINTENDO_LOGO_156);
    let path = dir.write("game.nds", &rom);

    let meta = nintendo::parse_nds_header_metadata_from_bytes(&path, &rom, Platform::NintendoDs).expect("valid NDS header");
    assert_eq!(meta.title.as_deref(), Some("TEST TITLE"));
    assert_eq!(meta.serial.as_deref(), Some("TSTB"));

    // Lowercase game code isn't a valid one, so the parse must fail rather than guess.
    let mut bad = rom.clone();
    put(&mut bad, 0x0C, b"tstb");
    let bad_path = dir.write("bad.nds", &bad);
    assert!(nintendo::parse_nds_header_metadata_from_bytes(&bad_path, &bad, Platform::NintendoDs).is_none());
}

#[test]
fn nds_and_dsi_are_separated_by_the_unit_code() {
    let dir = TempDir::new("nds_dsi_unit_code");
    let mut rom = buffer(0x200);
    put(&mut rom, 0x00, b"DSI TITLE");
    put(&mut rom, 0x0C, b"TSTC");
    rom[0x12] = 0x03; // DSi-exclusive
    put(&mut rom, 0xC0, &NINTENDO_LOGO_156);
    let path = dir.write("game.dsi", &rom);

    assert!(nintendo::parse_nds_header_metadata_from_bytes(&path, &rom, Platform::NintendoDsi).is_some());
    assert!(
        nintendo::parse_nds_header_metadata_from_bytes(&path, &rom, Platform::NintendoDs).is_none(),
        "a DSi-exclusive title must not be reported as plain DS"
    );
}

#[test]
fn gameboy_title_requires_the_nintendo_boot_logo() {
    const LOGO: [u8; 48] = [
        0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D, 0x00, 0x08, 0x11,
        0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99, 0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E,
        0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
    ];
    let mut rom = buffer(0x200);
    put(&mut rom, 0x104, &LOGO);
    put(&mut rom, 0x134, b"TEST TITLE");
    seal_gameboy_header(&mut rom);

    assert_eq!(nintendo::parse_gameboy_title_from_bytes(&rom).as_deref(), Some("TEST TITLE"));

    let mut no_logo = buffer(0x200);
    put(&mut no_logo, 0x134, b"TEST TITLE");
    assert!(nintendo::parse_gameboy_title_from_bytes(&no_logo).is_none(), "title without the boot logo is not trustworthy");
}

#[test]
fn gameboy_color_flag_separates_gb_from_gbc() {
    const LOGO: [u8; 48] = [
        0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D, 0x00, 0x08, 0x11,
        0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99, 0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E,
        0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
    ];
    let mut rom = buffer(0x200);
    put(&mut rom, 0x104, &LOGO);
    put(&mut rom, 0x134, b"TEST TITLE");
    rom[0x143] = 0x80; // CGB-compatible
    seal_gameboy_header(&mut rom);

    assert!(nintendo::parse_gameboy_metadata_from_bytes(&rom, Platform::GameBoyColor).is_some());
    assert!(nintendo::parse_gameboy_metadata_from_bytes(&rom, Platform::GameBoy).is_none());
}

#[test]
fn genesis_header_prefers_the_domestic_title_and_normalizes_the_serial() {
    let mut rom = buffer(0x200);
    put(&mut rom, 0x100, b"SEGA MEGA DRIVE ");
    put(&mut rom, 0x120, b"TEST TITLE (JP)");
    put(&mut rom, 0x150, b"TEST TITLE (US)");
    put(&mut rom, 0x180, b"GM 00000000-00");

    let (title, serial, _) =
        sega::parse_genesis_header_metadata_from_bytes(&rom, Platform::SegaGenesis).expect("valid Genesis header");
    assert_eq!(title.as_deref(), Some("TEST TITLE (JP)"), "domestic title wins");
    assert_eq!(serial.as_deref(), Some("GM 00000000-00"));
}

#[test]
fn genesis_and_pico_are_separated_by_the_console_name() {
    let mut pico = buffer(0x200);
    put(&mut pico, 0x100, b"SEGA PICO       ");
    put(&mut pico, 0x120, b"PICO TITLE");
    put(&mut pico, 0x180, b"MK-00000-00");

    assert!(sega::parse_genesis_header_metadata_from_bytes(&pico, Platform::SegaPico).is_some());
    assert!(
        sega::parse_genesis_header_metadata_from_bytes(&pico, Platform::SegaGenesis).is_none(),
        "a Pico cart must not be reported as Genesis"
    );
}

#[test]
fn sega_32x_header_is_rejected_for_a_plain_genesis_rom() {
    let mut rom = buffer(0x200);
    put(&mut rom, 0x100, b"SEGA MEGA DRIVE ");
    put(&mut rom, 0x120, b"NOT A 32X GAME");
    assert!(sega::parse_sega_32x_header_metadata_from_bytes(&rom).is_none());
}

#[test]
fn fds_metadata_requires_the_nintendo_hvc_marker() {
    let mut disk = buffer(0x40);
    disk[0] = 0x01;
    put(&mut disk, 1, b"*NINTENDO-HVC*");
    disk[0x0F] = 0x31;
    put(&mut disk, 0x10, b"ABC");

    let (_, serial, maker) = nintendo::parse_fds_qd_metadata_from_bytes(&disk).expect("valid FDS disk");
    assert_eq!(serial.as_deref(), Some("ABC"));
    assert_eq!(maker.as_deref(), Some("31"));

    assert!(nintendo::parse_fds_qd_metadata_from_bytes(&buffer(0x40)).is_none());
}

#[test]
fn headered_fds_image_parses_the_same_as_a_raw_one() {
    let mut payload = buffer(0x40);
    payload[0] = 0x01;
    put(&mut payload, 1, b"*NINTENDO-HVC*");
    put(&mut payload, 0x10, b"XYZ");

    let mut headered = buffer(16);
    put(&mut headered, 0, b"FDS\x1A");
    headered.extend_from_slice(&payload);

    let (_, serial, _) = nintendo::parse_fds_qd_metadata_from_bytes(&headered).expect("headered FDS image");
    assert_eq!(serial.as_deref(), Some("XYZ"));
}

#[test]
fn neogeo_pocket_color_mode_byte_separates_the_two_models() {
    let mut rom = buffer(0x40);
    put(&mut rom, 0x00, b" LICENSED BY SNK CORPORATION");
    rom[0x20] = 0x34;
    rom[0x21] = 0x12;
    rom[0x22] = 0x05;
    rom[0x23] = 0x10; // colour
    put(&mut rom, 0x24, b"TEST TITLE");

    let (title, serial, _) =
        other::parse_neogeo_pocket_header_metadata_from_bytes(&rom, Platform::NeoGeoPocketColor).expect("valid NGPC header");
    assert_eq!(title.as_deref(), Some("TEST TITLE"));
    assert_eq!(serial.as_deref(), Some("1234-05"));

    assert!(
        other::parse_neogeo_pocket_header_metadata_from_bytes(&rom, Platform::NeoGeoPocket).is_none(),
        "a colour cart must not be reported as mono NGP"
    );
}

#[test]
fn n64_roms_are_normalized_across_all_three_byte_orders() {
    let mut big_endian = buffer(0x40);
    put(&mut big_endian, 0, &[0x80, 0x37, 0x12, 0x40]);
    put(&mut big_endian, 0x20, b"TEST TITLE          ");
    put(&mut big_endian, 0x3B, b"TSTD");

    let byte_swapped: Vec<u8> = big_endian.chunks_exact(2).flat_map(|c| [c[1], c[0]]).collect();
    let little_endian: Vec<u8> = big_endian.chunks_exact(4).flat_map(|c| [c[3], c[2], c[1], c[0]]).collect();

    for (label, rom, expected_order) in [
        ("z64", &big_endian, RomByteOrder::BigEndian),
        ("v64", &byte_swapped, RomByteOrder::ByteSwapped),
        ("n64", &little_endian, RomByteOrder::LittleEndian),
    ] {
        let meta = nintendo::parse_n64_header_metadata_from_bytes(rom).unwrap_or_else(|| panic!("{label} byte order must parse"));
        assert_eq!(meta.title.as_deref(), Some("TEST TITLE"), "{label}");
        assert_eq!(meta.serial.as_deref(), Some("TSTD"), "{label}");
        assert_eq!(meta.byte_order, Some(expected_order), "{label}");
    }
}

// ---------------------------------------------------------------------------
// Container parsers
// ---------------------------------------------------------------------------

#[test]
fn dos_boot_sector_yields_label_and_serial_and_validates_its_bpb() {
    let mut sector = buffer(512);
    sector[0] = 0xEB;
    sector[11] = 0x00; // bytes-per-sector low
    sector[12] = 0x02; // = 512
    sector[13] = 0x01; // sectors per cluster
    sector[38] = 0x29; // extended boot signature
    put(&mut sector, 39, &[0x78, 0x56, 0x34, 0x12]);
    put(&mut sector, 43, b"GAME DISK  ");

    let (label, serial) = mycom::parse_dos_boot_sector_label_and_serial(&sector);
    assert_eq!(label.as_deref(), Some("GAME DISK"));
    assert_eq!(serial.as_deref(), Some("12345678"));

    // A buffer that fails the BPB sanity check yields nothing, so random data can't match.
    let mut bad = sector.clone();
    bad[11] = 0x00;
    bad[12] = 0x00; // bytes-per-sector 0
    assert_eq!(mycom::parse_dos_boot_sector_label_and_serial(&bad), (None, None));
}

#[test]
fn placeholder_volume_labels_are_rejected() {
    let mut sector = buffer(512);
    sector[0] = 0xEB;
    sector[12] = 0x02;
    sector[13] = 0x01;
    put(&mut sector, 43, b"NO NAME    ");

    let (label, _) = mycom::parse_dos_boot_sector_label_and_serial(&sector);
    assert!(label.is_none(), "'NO NAME' is a placeholder, not a title");
}

#[test]
fn msx_cas_header_is_recognized() {
    let mut tape = buffer(32);
    put(&mut tape, 0, &[0x1F, 0xA6, 0xDE, 0xBA, 0xCC, 0x13, 0x7D, 0x74]);

    let (_, serial, _) = mycom::parse_audio_container_metadata_from_bytes(&tape).expect("CAS header must be recognized");
    assert_eq!(serial.as_deref(), Some("MSX-CAS"));
}

#[test]
fn wav_container_title_is_read_from_its_list_info_chunk() {
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&0u32.to_le_bytes()); // size, unused by the parser
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"LIST");
    let info: Vec<u8> = {
        let mut v = Vec::new();
        v.extend_from_slice(b"INFO");
        v.extend_from_slice(b"INAM");
        let name = b"TEST TITLE\0";
        v.extend_from_slice(&(name.len() as u32).to_le_bytes());
        v.extend_from_slice(name);
        v
    };
    wav.extend_from_slice(&(info.len() as u32).to_le_bytes());
    wav.extend_from_slice(&info);

    let title = mycom::parse_wav_container_title_from_bytes(&wav).expect("a RIFF/WAVE file must be recognized");
    assert_eq!(title.as_deref(), Some("TEST TITLE"));
}

#[test]
fn a_non_wav_buffer_is_not_claimed_as_a_wav() {
    assert!(mycom::parse_wav_container_title_from_bytes(b"not a riff file at all").is_none());
}

#[test]
fn neogeo_cd_game_id_is_decoded_from_its_bcd_marker() {
    let mut bytes = buffer(64);
    put(&mut bytes, 4, b"NEO-GEO");
    bytes[4 + 8] = 0x00;
    bytes[4 + 9] = 0x45;

    assert_eq!(other::infer_neogeo_cd_game_id_from_bytes(&bytes).as_deref(), Some("0045"));
}

#[test]
fn neogeo_cd_rejects_non_bcd_and_all_zero_ids() {
    let mut non_bcd = buffer(64);
    put(&mut non_bcd, 0, b"NEO-GEO");
    non_bcd[8] = 0xAF;
    assert!(other::infer_neogeo_cd_game_id_from_bytes(&non_bcd).is_none());

    let mut zeros = buffer(64);
    put(&mut zeros, 0, b"NEO-GEO");
    assert!(other::infer_neogeo_cd_game_id_from_bytes(&zeros).is_none(), "an all-zero id is not a real game id");
}

// ---------------------------------------------------------------------------
// Disc content parsers
// ---------------------------------------------------------------------------

#[test]
fn playstation_boot_serial_is_read_from_a_system_cnf() {
    let cnf = b"BOOT2 = cdrom0:\\SLUS_000.00;1\nVER = 1.00\n";
    let serial = disc_dispatch::parse_playstation_boot_serial_from_bytes(cnf, "BOOT2").expect("BOOT2 line must yield a serial");
    assert_eq!(serial, "SLUS_000.00");
}

#[test]
fn playstation_serial_scan_ignores_a_line_for_a_different_boot_key() {
    let cnf = b"BOOT = cdrom:\\SLPS_000.01;1\n";
    assert!(disc_dispatch::parse_playstation_boot_serial_from_bytes(cnf, "BOOT").is_some());
    // Scanning for BOOT2 in a BOOT-only file must not invent a match.
    assert!(disc_dispatch::parse_playstation_boot_serial_from_bytes(b"NOTHING HERE\n", "BOOT2").is_none());
}

#[test]
fn pce_cd_serial_comes_from_a_non_system_cnf_filename() {
    let bytes = b"SYSTEM.CNF and also YS4.CNF here";
    assert_eq!(disc_dispatch::parse_pce_cd_serial_from_bytes(bytes).as_deref(), Some("YS4"));
}

/// Builds a minimal PARAM.SFO with the given key/value pairs, in the binary layout
/// `parse_ps3_param_sfo_from_bytes` walks.
fn build_param_sfo(entries: &[(&str, &str)]) -> Vec<u8> {
    let header_len = 0x14 + entries.len() * 0x10;

    let mut key_table = Vec::new();
    let mut key_offsets = Vec::new();
    for (key, _) in entries {
        key_offsets.push(key_table.len() as u16);
        key_table.extend_from_slice(key.as_bytes());
        key_table.push(0);
    }
    while key_table.len() % 4 != 0 {
        key_table.push(0);
    }

    let mut data_table = Vec::new();
    let mut data_offsets = Vec::new();
    for (_, value) in entries {
        data_offsets.push(data_table.len() as u32);
        data_table.extend_from_slice(value.as_bytes());
        data_table.push(0);
    }

    let key_table_start = header_len as u32;
    let data_table_start = key_table_start + key_table.len() as u32;

    let mut sfo = Vec::new();
    sfo.extend_from_slice(&[0x00, b'P', b'S', b'F']);
    sfo.extend_from_slice(&[0x01, 0x01, 0x00, 0x00]); // version
    sfo.extend_from_slice(&key_table_start.to_le_bytes());
    sfo.extend_from_slice(&data_table_start.to_le_bytes());
    sfo.extend_from_slice(&(entries.len() as u32).to_le_bytes());

    for (index, (_, value)) in entries.iter().enumerate() {
        let len = value.len() as u32 + 1;
        sfo.extend_from_slice(&key_offsets[index].to_le_bytes());
        sfo.extend_from_slice(&[0x04, 0x02]); // utf-8 data format
        sfo.extend_from_slice(&len.to_le_bytes()); // used size
        sfo.extend_from_slice(&len.to_le_bytes()); // total size
        sfo.extend_from_slice(&data_offsets[index].to_le_bytes());
    }

    sfo.extend_from_slice(&key_table);
    sfo.extend_from_slice(&data_table);
    sfo
}

#[test]
fn param_sfo_yields_title_and_title_id() {
    let sfo = build_param_sfo(&[("TITLE", "TEST TITLE"), ("TITLE_ID", "ABCD00000")]);

    let (title, serial) = disc_dispatch::parse_ps3_param_sfo_from_bytes(&sfo).expect("a well-formed PARAM.SFO must parse");
    assert_eq!(title.as_deref(), Some("TEST TITLE"));
    assert_eq!(serial.as_deref(), Some("ABCD00000"));
}

#[test]
fn psp_and_ps3_share_one_param_sfo_parser() {
    let sfo = build_param_sfo(&[("TITLE", "TEST TITLE"), ("TITLE_ID", "ABCD00000")]);
    assert_eq!(disc_dispatch::parse_psp_param_sfo_from_bytes(&sfo), disc_dispatch::parse_ps3_param_sfo_from_bytes(&sfo));
}

#[test]
fn a_buffer_with_no_sfo_structure_yields_nothing() {
    assert!(disc_dispatch::parse_ps3_param_sfo_from_bytes(b"TITLE_ID\0 but no table behind it").is_none());
}

// ---------------------------------------------------------------------------
// Archive format sniffing
// ---------------------------------------------------------------------------

#[test]
fn archive_formats_are_sniffed_by_magic_bytes() {
    use romexplorer::common::models::ArchiveFormat;

    let zip = [0x50, 0x4b, 0x03, 0x04];
    let seven_zip = [0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c];

    assert_eq!(descriptor::detect_archive_format_from_bytes(b"MComprHD....", Path::new("x.unknown")), ArchiveFormat::Chd);
    assert_eq!(descriptor::detect_archive_format_from_bytes(&zip, Path::new("x.unknown")), ArchiveFormat::Zip);
    assert_eq!(descriptor::detect_archive_format_from_bytes(&seven_zip, Path::new("x.unknown")), ArchiveFormat::SevenZip);
    assert_eq!(descriptor::detect_archive_format_from_bytes(b"", Path::new("game.iso")), ArchiveFormat::RawIso);
    assert_eq!(descriptor::detect_archive_format_from_bytes(b"", Path::new("mystery.xyz")), ArchiveFormat::Unknown);
}

#[test]
fn cue_and_gdi_payload_paths_resolve_only_existing_siblings() {
    let dir = TempDir::new("descriptor_payloads");
    dir.write("game.bin", b"track data");
    let cue = dir.write("game.cue", b"FILE \"game.bin\" BINARY\n  TRACK 01 MODE1/2352\nFILE \"missing.bin\" BINARY\n");

    let payloads = descriptor::descriptor_payload_paths(&cue);
    assert_eq!(payloads.len(), 1, "only the sibling that exists on disk is returned");
    assert!(payloads[0].ends_with("game.bin"));
}

#[test]
fn gdi_payload_paths_skip_the_track_count_line() {
    let dir = TempDir::new("gdi_payloads");
    dir.write("track01.bin", b"data");
    let gdi = dir.write("game.gdi", b"1\n1 0 4 2352 track01.bin 0\n");

    let payloads = descriptor::descriptor_payload_paths(&gdi);
    assert_eq!(payloads.len(), 1);
    assert!(payloads[0].ends_with("track01.bin"));
}

// ---------------------------------------------------------------------------
// End-to-end: the four detection modes
// ---------------------------------------------------------------------------

#[test]
fn mode_1_forced_platform_overrides_every_heuristic() {
    let dir = TempDir::new("mode1");
    // Everything about this path says Game Boy; the forced platform must still win.
    let rom = dir.write("GameBoy/game.gb", b"not really a gameboy rom");

    let meta = detect::read_metadata_for_input(&rom, Some(Platform::SegaSaturn), true, true);

    assert_eq!(meta.platform, Platform::SegaSaturn);
    assert_eq!(meta.reason, RomReason::Forced);
    assert_eq!(meta.candidates, vec![Platform::SegaSaturn]);
}

#[test]
fn mode_3_extension_heuristic_resolves_an_unambiguous_extension() {
    let dir = TempDir::new("mode3");
    let rom = dir.write("unsorted/game.gba", b"not a real gba header");

    let meta = detect::read_metadata_for_input(&rom, None, true, true);

    assert_eq!(meta.platform, Platform::GameBoyAdvance, ".gba claims exactly one platform");
    assert_eq!(meta.media_type, MediaType::Cartridge);
    assert!(matches!(meta.reason, RomReason::ExtensionHeuristic | RomReason::DeepCycle));
}

#[test]
fn mode_2_path_heuristic_resolves_when_the_extension_says_nothing() {
    let dir = TempDir::new("mode2");
    let rom = dir.write("Nintendo 64/game.unknownext", b"mystery content");

    let meta = detect::read_metadata_for_input(&rom, None, true, true);

    assert_eq!(meta.platform, Platform::Nintendo64, "the parent directory names the platform");
    assert_eq!(meta.reason, RomReason::PathHeuristic);
}

#[test]
fn path_heuristic_narrows_an_ambiguous_extension_end_to_end() {
    let dir = TempDir::new("narrow_e2e");
    // .iso alone is ambiguous across seven platforms; the directory picks one.
    let rom = dir.write("PlayStation 2/game.iso", b"not a real iso");

    let meta = detect::read_metadata_for_input(&rom, None, true, true);

    assert_eq!(meta.platform, Platform::PlayStation2);
    assert!(meta.candidates.contains(&Platform::PlayStation2));
}

#[test]
fn disabling_the_path_heuristic_leaves_an_ambiguous_extension_unresolved() {
    let dir = TempDir::new("no_path_heuristic");
    let rom = dir.write("PlayStation 2/game.iso", b"not a real iso");

    let meta = detect::read_metadata_for_input(&rom, None, true, false);

    assert_eq!(meta.platform, Platform::Unknown, "without the path hint, .iso stays ambiguous");
    assert!(meta.candidates.len() > 1);
}

#[test]
fn mode_4_deep_cycle_reads_content_when_heuristics_are_ambiguous() {
    let dir = TempDir::new("mode4");

    // A real GBA header, but saved with an extension that claims nothing in particular, in
    // a directory that names no platform. Only reading the content can identify it.
    let mut rom = buffer(0x200);
    // The GBA title field is 12 bytes (0x0A0..0x0AC), so real titles are abbreviated.
    put(&mut rom, 0x0A0, b"TEST TITLE");
    put(&mut rom, 0x0AC, b"TSTE");
    put(&mut rom, 0x0B0, b"01");
    seal_gba_header(&mut rom);
    let path = dir.write("unsorted/mystery.gba", &rom);

    let meta = detect::read_metadata_for_input(&path, None, true, true);

    assert_eq!(meta.platform, Platform::GameBoyAdvance);
    assert_eq!(meta.title.as_deref(), Some("TEST TITLE"));
    assert_eq!(meta.serial_product_id.as_deref(), Some("TSTE"));
}

#[test]
fn detection_records_an_audit_trail_and_a_file_size() {
    let dir = TempDir::new("trail");
    let rom = dir.write("GameBoy Advance/game.gba", b"0123456789");

    let meta = detect::read_metadata_for_input(&rom, None, true, true);

    assert_eq!(meta.file_size, Some(10));
    assert!(!meta.detection_trail.is_empty(), "detection must record how it decided");
    assert!(!meta.reason_string.is_empty());
}

// ---------------------------------------------------------------------------
// Signature-required parsers
// ---------------------------------------------------------------------------

#[test]
fn gba_and_nds_headers_require_the_nintendo_boot_logo() {
    let dir = TempDir::new("logo_required");

    let mut gba = buffer(0x200);
    put(&mut gba, 0x0A0, b"TEST TITLE");
    put(&mut gba, 0x0AC, b"TSTA");
    seal_gba_header(&mut gba);
    assert!(nintendo::parse_gba_header_metadata_from_bytes(&gba).is_some());
    gba[0x10] ^= 0xFF; // corrupt the logo
    assert!(nintendo::parse_gba_header_metadata_from_bytes(&gba).is_none(), "a corrupted boot logo must reject");

    let mut nds = buffer(0x200);
    put(&mut nds, 0x00, b"TEST TITLE");
    put(&mut nds, 0x0C, b"TSTB");
    let path = dir.write("game.nds", &nds);
    assert!(
        nintendo::parse_nds_header_metadata_from_bytes(&path, &nds, Platform::NintendoDs).is_none(),
        "a plausible game code without the boot logo is not an NDS header"
    );
}

#[test]
fn wiiu_container_rejects_a_bare_key_file() {
    let key = [0xDAu8, 0xAC, 0xDF, 0x83, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    assert!(nintendo::parse_wiiu_container_metadata_from_bytes(&key).is_none());
}

#[test]
fn n64dd_requires_its_system_data_signature() {
    let dir = TempDir::new("n64dd");
    let mut disk = buffer(0x400);
    put(&mut disk, 0x00, &nintendo::N64DD_REGION_JP);
    disk[0x04] = 0x10;
    put(&mut disk, 0x18, &[0xFF; 4]);
    put(&mut disk, 0xE6, &[0xFF; 2]);
    let path = dir.write("game.ndd", &disk);
    assert!(nintendo::read_n64dd_system_data_metadata(&path).is_some());

    disk[0x04] = 0x00; // wrong format type
    let bad = dir.write("bad.ndd", &disk);
    assert!(nintendo::read_n64dd_system_data_metadata(&bad).is_none());
}

#[test]
fn pce_cd_needs_its_ipl_signature() {
    let mut track = buffer(0x4000);
    assert!(disc_dispatch::pce_cd_signature_offset(&track).is_none());
    put(&mut track, 0x820, disc_dispatch::PCE_CD_SIGNATURE);
    assert_eq!(disc_dispatch::pce_cd_signature_offset(&track), Some(0x820));
}

#[test]
fn nds_encryption_state_follows_the_secure_area_marker() {
    let dir = TempDir::new("nds_secure_area");
    let mut rom = buffer(0x5000);
    put(&mut rom, 0x00, b"TEST TITLE");
    put(&mut rom, 0x0C, b"TSTB");
    put(&mut rom, 0xC0, &NINTENDO_LOGO_156);
    put(&mut rom, 0x20, &0x4000u32.to_le_bytes()); // ARM9 binary at the secure area

    let state = |rom: &[u8], name: &str| {
        let path = dir.write(name, rom);
        nintendo::parse_nds_header_metadata_from_bytes(&path, rom, Platform::NintendoDs).expect("valid header").encrypted
    };

    put(&mut rom, 0x4000, &[0xFF, 0xDE, 0xFF, 0xE7, 0xFF, 0xDE, 0xFF, 0xE7]);
    assert_eq!(state(&rom, "decrypted.nds"), Some(false), "the E7FFDEFF pair marks a decrypted secure area");

    put(&mut rom, 0x4000, &[0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0]);
    assert_eq!(state(&rom, "encrypted.nds"), Some(true));

    put(&mut rom, 0x20, &0x200u32.to_le_bytes()); // homebrew layout: no secure area
    assert_eq!(state(&rom, "homebrew.nds"), None);
}

// ---------------------------------------------------------------------------
// Mycom family signatures
// ---------------------------------------------------------------------------

use romexplorer::rom::platforms::mycom::{flux_geometry_candidates, verdict_for_cd_tracks, verdict_for_file, FluxRpm, MycomVerdict};

fn is_match(verdict: &MycomVerdict) -> bool {
    matches!(verdict, MycomVerdict::Match(evidence) if !evidence.is_empty())
}

#[test]
fn mycom_s_rank_signatures_identify_their_system_and_conflict_with_the_others() {
    let dir = TempDir::new("mycom_s_rank");

    let mut cas = buffer(0x100);
    put(&mut cas, 0, &[0x1F, 0xA6, 0xDE, 0xBA, 0xCC, 0x13, 0x7D, 0x74]);
    let cas = dir.write("tape.cas", &cas);
    for msx in [Platform::Msx, Platform::Msx2, Platform::MsxTurbo, Platform::Msx2Plus] {
        assert!(is_match(&verdict_for_file(&cas, msx)), "{msx:?}: a CAS header is an MSX-family tape");
    }
    assert!(matches!(verdict_for_file(&cas, Platform::Pc98), MycomVerdict::Conflict));

    let mut t88 = buffer(0x100);
    put(&mut t88, 0, b"PC-8801 Tape Image(T88)\0");
    let t88 = dir.write("tape.t88", &t88);
    assert!(is_match(&verdict_for_file(&t88, Platform::Pc8801)));
    assert!(matches!(verdict_for_file(&t88, Platform::Fm7), MycomVerdict::Conflict));

    let mut vfd = buffer(0x200);
    put(&mut vfd, 0, b"VFD1.00");
    let vfd = dir.write("disk.fdd", &vfd);
    assert!(is_match(&verdict_for_file(&vfd, Platform::Pc98)));
    assert!(matches!(verdict_for_file(&vfd, Platform::Pc8801), MycomVerdict::Conflict));

    let mut nfd = buffer(0x200);
    put(&mut nfd, 0, b"T98FDDIMAGE.R0\0\0");
    let nfd = dir.write("disk.nfd", &nfd);
    assert!(is_match(&verdict_for_file(&nfd, Platform::Pc98)));

    let mut towns = buffer(0x400);
    put(&mut towns, 0, b"IPL4");
    let towns = dir.write("boot.bin", &towns);
    assert!(is_match(&verdict_for_file(&towns, Platform::FmTowns)));
    assert!(matches!(verdict_for_file(&towns, Platform::Pc98), MycomVerdict::Conflict), "IPL4 excludes PC-98");
}

#[test]
fn mycom_a_rank_signals_count_only_in_combination() {
    let dir = TempDir::new("mycom_a_rank");

    // IPL string alone is shared with other machines; with the 55 AA table signature it is PC-98.
    let mut ipl_only = buffer(0x400);
    put(&mut ipl_only, 4, b"IPL1");
    let ipl_only = dir.write("ipl_only.hdm", &ipl_only);
    assert!(matches!(verdict_for_file(&ipl_only, Platform::Pc98), MycomVerdict::Unknown));
    let mut hdd = buffer(0x400);
    put(&mut hdd, 4, b"IPL1");
    put(&mut hdd, 510, &[0x55, 0xAA]);
    let hdd = dir.write("disk.hdd", &hdd);
    assert!(is_match(&verdict_for_file(&hdd, Platform::Pc98)));

    // `AB` alone is two bytes; the address and zero-padding checks make it a signature.
    let mut bare = buffer(0x8000);
    put(&mut bare, 0, b"AB");
    let bare = dir.write("bare.rom", &bare);
    assert!(matches!(verdict_for_file(&bare, Platform::Msx), MycomVerdict::Unknown));
    let mut rom = buffer(0x8000);
    put(&mut rom, 0, b"AB");
    put(&mut rom, 2, &0x4010u16.to_le_bytes());
    let rom_path = dir.write("game.rom", &rom);
    assert!(is_match(&verdict_for_file(&rom_path, Platform::Msx2)));
    put(&mut rom, 2, &0x0100u16.to_le_bytes()); // INIT outside the cartridge slot
    let bad = dir.write("bad_init.rom", &rom);
    assert!(matches!(verdict_for_file(&bad, Platform::Msx), MycomVerdict::Unknown));
}

#[test]
fn mycom_shapes_without_a_signature_stay_unknown() {
    let dir = TempDir::new("mycom_unknown");
    // A D88/D77-shaped image and a FAT boot sector carry no system signature.
    let mut d88 = buffer(0x2B0);
    d88[0x1C..0x20].copy_from_slice(&0x2B0u32.to_le_bytes());
    d88[0x20..0x24].copy_from_slice(&0x2B0u32.to_le_bytes());
    let d88 = dir.write("disk.d88", &d88);
    let mut fat = buffer(0x200);
    put(&mut fat, 0, &[0xEB, 0xFE, 0x90]);
    let fat = dir.write("disk.dsk", &fat);
    for path in [&d88, &fat] {
        for platform in [Platform::Msx, Platform::Pc8801, Platform::Pc98, Platform::Fm7, Platform::FmTowns] {
            assert!(matches!(verdict_for_file(path, platform), MycomVerdict::Unknown), "{platform:?}");
        }
    }
}

#[test]
fn mycom_cd_signature_is_read_from_the_first_data_sector_of_a_track() {
    let dir = TempDir::new("mycom_cd");
    let sync = [0x00u8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    let raw_track = |signature: &[u8], pregap_sectors: usize| {
        let mut track = vec![0u8; 2352 * (pregap_sectors + 4)];
        for sector in 0..pregap_sectors + 4 {
            put(&mut track, sector * 2352, &sync);
        }
        put(&mut track, pregap_sectors * 2352 + 16, signature);
        track
    };

    let pc88 = dir.write("pc88.bin", &raw_track(b"NEC Personal Computer PC-8801 Series CD Type1   ", 225));
    assert!(is_match(&verdict_for_cd_tracks(&[pc88.clone()], Platform::Pc8801)));
    assert!(matches!(verdict_for_cd_tracks(&[pc88], Platform::FmTowns), MycomVerdict::Conflict));

    let towns = dir.write("towns.bin", &raw_track(b"IPL4", 150));
    assert!(is_match(&verdict_for_cd_tracks(&[towns], Platform::FmTowns)));

    // A leading audio track (no signature) is skipped; a data track of plain ISO 9660 is Unknown.
    let audio = dir.write("audio.bin", &vec![0x11u8; 2352 * 8]);
    let mut iso = raw_track(b"", 0);
    put(&mut iso, 16, &[0x01]);
    put(&mut iso, 17, b"CD001");
    let iso = dir.write("iso.bin", &iso);
    assert!(matches!(verdict_for_cd_tracks(&[audio.clone(), iso], Platform::Pc98), MycomVerdict::Unknown));
    let cooked = dir.write("cooked.bin", &{
        let mut track = vec![0u8; 2048 * 4];
        put(&mut track, 2048, b"IPL4");
        track
    });
    assert!(is_match(&verdict_for_cd_tracks(&[audio, cooked], Platform::FmTowns)));
}

#[test]
fn flux_geometry_only_narrows_to_a_family() {
    assert_eq!(flux_geometry_candidates(FluxRpm::Rpm360, 84), &[Platform::Pc98, Platform::FmTowns]);
    assert_eq!(flux_geometry_candidates(FluxRpm::Rpm300, 42), &[Platform::Pc8801, Platform::Fm7]);
    assert!(flux_geometry_candidates(FluxRpm::Rpm360, 10).is_empty());
}

// ---------------------------------------------------------------------------
// Directory and package games (DSi, 3DS, PS3, PSP)
// ---------------------------------------------------------------------------

use romexplorer::rom::platforms::download::{verdict_for_directory, verdict_for_file as download_verdict_for_file, DownloadVerdict};

fn is_download_match(verdict: &DownloadVerdict) -> bool {
    matches!(verdict, DownloadVerdict::Match(meta) if !meta.evidence.is_empty())
}

/// A TMD with the RSA-2048 layout (header at 0x140) and the given title id.
fn synthetic_tmd(title_id: u64) -> Vec<u8> {
    let mut tmd = buffer(0x208);
    put(&mut tmd, 0, &0x0001_0001u32.to_be_bytes());
    put(&mut tmd, 0x140 + 0x4C, &title_id.to_be_bytes());
    put(&mut tmd, 0x140 + 0x9E, &1u16.to_be_bytes());
    tmd
}

fn synthetic_sfo(entries: &[(&str, &str)]) -> Vec<u8> {
    let entries: Vec<(&str, SfoVal)> = entries.iter().map(|(k, v)| (*k, SfoVal::Text(v))).collect();
    synthetic_sfo_values(&entries)
}

enum SfoVal<'a> {
    Text(&'a str),
    Int(u32),
}

/// A PARAM.SFO with text and integer entries, in the layout the console writes.
fn synthetic_sfo_values(entries: &[(&str, SfoVal)]) -> Vec<u8> {
    let mut keys = Vec::new();
    let mut data = Vec::new();
    let mut index = Vec::new();
    for (key, value) in entries {
        let (format, bytes) = match value {
            SfoVal::Text(text) => (0x0204u16, [text.as_bytes(), &[0]].concat()),
            SfoVal::Int(n) => (0x0404u16, n.to_le_bytes().to_vec()),
        };
        index.extend_from_slice(&(keys.len() as u16).to_le_bytes());
        index.extend_from_slice(&format.to_le_bytes());
        index.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        index.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        index.extend_from_slice(&(data.len() as u32).to_le_bytes());
        keys.extend_from_slice(key.as_bytes());
        keys.push(0);
        data.extend_from_slice(&bytes);
    }
    let key_table = 0x14 + index.len();
    let data_table = key_table + keys.len();
    let mut sfo = vec![0x00, b'P', b'S', b'F', 0x01, 0x01, 0x00, 0x00];
    sfo.extend_from_slice(&(key_table as u32).to_le_bytes());
    sfo.extend_from_slice(&(data_table as u32).to_le_bytes());
    sfo.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    sfo.extend_from_slice(&index);
    sfo.extend_from_slice(&keys);
    sfo.extend_from_slice(&data);
    sfo
}

/// A PKG header as the PS Store serves it: magic, platform, content id, metadata (content
/// type, app version 1.02, and the SFO's place when one is given) and, for PSP/Vita, the
/// extended header with its key type. The SFO, when given, sits at 0x400 in plain text.
fn synthetic_pkg(platform: u16, content_type: Option<u32>, key_type: Option<u8>, content_id: &str, sfo: Option<&[u8]>) -> Vec<u8> {
    let mut meta = Vec::new();
    let mut count = 0u32;
    let mut entry = |meta: &mut Vec<u8>, id: u32, data: &[u8]| {
        meta.extend_from_slice(&id.to_be_bytes());
        meta.extend_from_slice(&(data.len() as u32).to_be_bytes());
        meta.extend_from_slice(data);
        count += 1;
    };
    if let Some(t) = content_type {
        entry(&mut meta, 2, &t.to_be_bytes());
    }
    entry(&mut meta, 8, &[0, 0, 0, 0, 0x01, 0x00, 0x01, 0x02]);
    if let Some(sfo) = sfo {
        let mut data = 0x400u32.to_be_bytes().to_vec();
        data.extend_from_slice(&(sfo.len() as u32).to_be_bytes());
        data.extend_from_slice(&[0u8; 0x30]);
        entry(&mut meta, 14, &data);
    }
    let mut pkg = buffer(0x400 + sfo.map_or(0, |s| s.len()) + 0x100);
    put(&mut pkg, 0, &[0x7F, b'P', b'K', b'G', 0x80, 0x00]);
    put(&mut pkg, 6, &platform.to_be_bytes());
    put(&mut pkg, 0x08, &0x100u32.to_be_bytes());
    put(&mut pkg, 0x0C, &count.to_be_bytes());
    put(&mut pkg, 0x10, &(meta.len() as u32).to_be_bytes());
    put(&mut pkg, 0x30, content_id.as_bytes());
    if let Some(key) = key_type {
        put(&mut pkg, 0xC0, &[0x7F, b'e', b'x', b't']);
        pkg[0xE7] = key;
    }
    put(&mut pkg, 0x100, &meta);
    if let Some(sfo) = sfo {
        put(&mut pkg, 0x400, sfo);
    }
    pkg
}

#[test]
fn tmd_title_id_high_word_separates_dsi_from_3ds() {
    let dsiware = TempDir::new("dl_dsiware");
    dsiware.write("content/title.tmd", &synthetic_tmd(0x0003_0004_4142_4344));
    let dsiware_root = dsiware.path();
    assert!(is_download_match(&verdict_for_directory(&dsiware_root, Platform::DsiWare)));
    assert!(matches!(verdict_for_directory(&dsiware_root, Platform::NintendoDsi), DownloadVerdict::Conflict), "DSiWare is its own platform");
    assert!(matches!(verdict_for_directory(&dsiware_root, Platform::Nintendo3ds), DownloadVerdict::Conflict));

    let system_app = TempDir::new("dl_dsi_system");
    system_app.write("title.tmd", &synthetic_tmd(0x0003_0005_4142_4344));
    assert!(is_download_match(&verdict_for_directory(&system_app.path(), Platform::NintendoDsi)));

    let twl = TempDir::new("dl_twl");
    twl.write("00000000.tmd", &synthetic_tmd(0x0004_8004_4142_4344));
    assert!(is_download_match(&verdict_for_directory(&twl.path(), Platform::DsiWare)), "DSiWare on a 3DS is still DSiWare");

    let ctr = TempDir::new("dl_3ds");
    ctr.write("title/00000000.tmd", &synthetic_tmd(0x0004_0000_0012_3400));
    assert!(is_download_match(&verdict_for_directory(&ctr.path(), Platform::Nintendo3ds)));
    assert!(is_download_match(&verdict_for_directory(&ctr.path(), Platform::NewNintendo3ds)));
    assert!(matches!(verdict_for_directory(&ctr.path(), Platform::DsiWare), DownloadVerdict::Conflict));

    let wii_u = TempDir::new("dl_wiiu");
    wii_u.write("title.tmd", &synthetic_tmd(0x0005_0000_1010_1000));
    assert!(matches!(verdict_for_directory(&wii_u.path(), Platform::NintendoDsi), DownloadVerdict::Conflict));

    let none = TempDir::new("dl_none");
    none.write("readme.txt", b"no signature here");
    assert!(matches!(verdict_for_directory(&none.path(), Platform::DsiWare), DownloadVerdict::Unknown));
}

#[test]
fn dsi_srl_and_3ds_containers_are_recognized_by_header() {
    let dir = TempDir::new("dl_headers");
    let mut srl = buffer(0x400);
    put(&mut srl, 0x00, b"TEST TITLE");
    put(&mut srl, 0x0C, b"TSTA");
    put(&mut srl, 0xC0, &NINTENDO_LOGO_156);
    put(&mut srl, 0x234, &[0x04, 0x00, 0x03, 0x00]);
    dir.write("app/00000000", &srl);
    match verdict_for_directory(&dir.path(), Platform::DsiWare) {
        DownloadVerdict::Match(meta) => {
            assert_eq!(meta.serial.as_deref(), Some("TSTA"));
            assert_eq!(meta.title.as_deref(), Some("TEST TITLE"));
        }
        other => panic!("expected a DSiWare match, got {other:?}"),
    }
    // The same header with a cartridge file type (00) is plain DSi, not DSiWare.
    let cartridge = TempDir::new("dl_srl_cart");
    put(&mut srl, 0x234, &[0x00, 0x00, 0x03, 0x00]);
    cartridge.write("game.srl", &srl);
    assert!(is_download_match(&verdict_for_directory(&cartridge.path(), Platform::NintendoDsi)));
    assert!(matches!(verdict_for_directory(&cartridge.path(), Platform::DsiWare), DownloadVerdict::Conflict));

    let ncch = TempDir::new("dl_ncch");
    let mut app = buffer(0x400);
    put(&mut app, 0x100, b"NCCH");
    ncch.write("content/00000001.app", &app);
    assert!(is_download_match(&verdict_for_directory(&ncch.path(), Platform::Nintendo3ds)));

    let homebrew = TempDir::new("dl_3dsx");
    homebrew.write("app.3dsx", b"3DSX\x20\x00\x2C\x00");
    assert!(is_download_match(&verdict_for_directory(&homebrew.path(), Platform::Nintendo3ds)));
}

#[test]
fn ps3_and_psp_game_folders_are_recognized_by_their_own_tree() {
    let ps3 = TempDir::new("dl_ps3");
    ps3.write("PS3_GAME/PARAM.SFO", &synthetic_sfo(&[("CATEGORY", "DG"), ("TITLE", "TEST TITLE"), ("TITLE_ID", "ABCD00000")]));
    match verdict_for_directory(&ps3.path(), Platform::PlayStation3) {
        DownloadVerdict::Match(meta) => assert_eq!(meta.serial.as_deref(), Some("ABCD00000")),
        other => panic!("expected a PS3 match, got {other:?}"),
    }
    assert!(matches!(verdict_for_directory(&ps3.path(), Platform::PlayStationPortable), DownloadVerdict::Conflict));

    let disc = TempDir::new("dl_ps3_disc");
    disc.write("PS3_DISC.SFB", b".SFB\x00\x00\x01\x01");
    assert!(is_download_match(&verdict_for_directory(&disc.path(), Platform::PlayStation3)));

    let psp = TempDir::new("dl_psp");
    psp.write("PSP_GAME/PARAM.SFO", &synthetic_sfo(&[("DISC_ID", "ABCD00000"), ("TITLE", "TEST TITLE")]));
    assert!(is_download_match(&verdict_for_directory(&psp.path(), Platform::PlayStationPortable)));
    assert!(matches!(verdict_for_directory(&psp.path(), Platform::PlayStation3), DownloadVerdict::Conflict));

    // A PARAM.SFO outside a PS3_GAME/PSP_GAME folder proves only the SFO format.
    let loose = TempDir::new("dl_sfo_loose");
    loose.write("misc/PARAM.SFO", &synthetic_sfo(&[("CATEGORY", "DG"), ("TITLE_ID", "ABCD00000")]));
    assert!(matches!(verdict_for_directory(&loose.path(), Platform::PlayStation3), DownloadVerdict::Unknown));
    // PS3_GAME with a category that is not a PS3 game one is not a match.
    let odd = TempDir::new("dl_sfo_odd");
    odd.write("PS3_GAME/PARAM.SFO", &synthetic_sfo(&[("CATEGORY", "ZZ"), ("TITLE_ID", "ABCD00000")]));
    assert!(matches!(verdict_for_directory(&odd.path(), Platform::PlayStation3), DownloadVerdict::Unknown));
}

#[test]
fn pkg_platform_field_separates_ps3_from_psp_and_vita() {
    let dir = TempDir::new("dl_pkg");
    let ps3 = dir.write("a.pkg", &synthetic_pkg(1, Some(0x04), None, "EP0000-ABCD00000_00-TESTCONTENT00001", None));
    match download_verdict_for_file(&ps3, Platform::PlayStation3) {
        DownloadVerdict::Match(meta) => assert_eq!(meta.serial.as_deref(), Some("ABCD00000")),
        other => panic!("expected a PS3 match, got {other:?}"),
    }
    assert!(sony::read_vita_metadata(&ps3).is_none(), "a PS3 package is not a Vita package");

    let vita = dir.write("b.pkg", &synthetic_pkg(2, Some(0x15), Some(2), "JP0000-ABCD00001_00-TESTCONTENT00001", None));
    assert!(matches!(download_verdict_for_file(&vita, Platform::PlayStation3), DownloadVerdict::Conflict));
    assert!(matches!(download_verdict_for_file(&vita, Platform::PlayStationPortable), DownloadVerdict::Conflict));
    let meta = sony::read_vita_metadata(&vita).expect("a Vita package");
    assert_eq!(meta.serial.as_deref(), Some("ABCD00001"), "without an SFO, the title id comes from the content id");
}

#[test]
fn pkg_content_type_separates_psp_and_ps1_classics_from_vita() {
    let dir = TempDir::new("dl_pkg_psp");
    // Platform 2 is shared; the content type decides.
    let psp = dir.write("psp.pkg", &synthetic_pkg(2, Some(0x07), Some(1), "EP0000-ABCD00002_00-TESTCONTENT00001", None));
    match download_verdict_for_file(&psp, Platform::PlayStationPortable) {
        DownloadVerdict::Match(meta) => assert_eq!(meta.serial.as_deref(), Some("ABCD00002")),
        other => panic!("expected a PSP match, got {other:?}"),
    }
    assert!(sony::read_vita_metadata(&psp).is_none(), "a PSP package is not a Vita package");
    assert!(matches!(download_verdict_for_file(&psp, Platform::PspPs1Classic), DownloadVerdict::Conflict));

    let ps1 = dir.write("ps1.pkg", &synthetic_pkg(2, Some(0x06), Some(1), "UP0000-ABCD00003_00-TESTCONTENT00001", None));
    assert!(is_download_match(&download_verdict_for_file(&ps1, Platform::PspPs1Classic)));
    assert!(sony::read_vita_metadata(&ps1).is_none());

    // No content type entry: the extended header's key type (1 = PSP) decides.
    let bare = dir.write("bare.pkg", &synthetic_pkg(2, None, Some(1), "EP0000-ABCD00004_00-TESTCONTENT00001", None));
    assert!(is_download_match(&download_verdict_for_file(&bare, Platform::PlayStationPortable)));
    assert!(sony::read_vita_metadata(&bare).is_none());
    let bare_vita = dir.write("bare_vita.pkg", &synthetic_pkg(2, None, Some(3), "EP0000-ABCD00005_00-TESTCONTENT00001", None));
    assert!(sony::read_vita_metadata(&bare_vita).is_some());
}

#[test]
fn vita_pkg_reads_its_plain_sfo_from_the_metadata() {
    let dir = TempDir::new("dl_pkg_vita_sfo");
    let sfo = synthetic_sfo_values(&[
        ("APP_VER", SfoVal::Text("01.02")),
        ("CATEGORY", SfoVal::Text("gp")),
        ("CONTENT_ID", SfoVal::Text("JP0000-ABCD00006_00-TESTCONTENT00001")),
        ("PSP2_DISP_VER", SfoVal::Text("03.630")),
        ("PSP2_SYSTEM_VER", SfoVal::Int(0x0363_0000)),
        ("STITLE", SfoVal::Text("SHORT")),
        ("TITLE", SfoVal::Text("TEST TITLE")),
        ("TITLE_ID", SfoVal::Text("ABCD00006")),
    ]);
    let path = dir.write("update.pkg", &synthetic_pkg(2, Some(0x15), Some(2), "JP0000-ABCD00006_00-TESTCONTENT00001", Some(&sfo)));
    let meta = sony::read_vita_metadata(&path).expect("a Vita package");
    assert_eq!(meta.title.as_deref(), Some("TEST TITLE"));
    assert_eq!(meta.serial.as_deref(), Some("ABCD00006"));
    let trail = meta.evidence.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n");
    assert!(trail.contains("CATEGORY gp (Vita update)"), "{trail}");
    assert!(trail.contains("content: update"), "{trail}");
    assert!(trail.contains("Japan"), "{trail}");
    assert!(trail.contains("app version 01.02"), "{trail}");

    // The in-memory reader agrees.
    let (title, serial, _) = sony::parse_psvita_pkg_metadata_from_bytes(&fs::read(&path).unwrap()).unwrap();
    assert_eq!((title.as_deref(), serial.as_deref()), (Some("TEST TITLE"), Some("ABCD00006")));
}

#[test]
fn psp_dlc_pbp_says_it_is_an_add_on() {
    let dir = TempDir::new("dl_psp_dlc");
    let sfo = synthetic_sfo_values(&[("BOOTABLE", SfoVal::Int(0)), ("CATEGORY", SfoVal::Text("MG")), ("DISC_ID", SfoVal::Text("ABCD00007")), ("TITLE", SfoVal::Text("TEST ADD-ON"))]);
    let mut pbp = buffer(0x28);
    put(&mut pbp, 0, &[0x00, b'P', b'B', b'P', 0x00, 0x00, 0x01, 0x00]);
    put(&mut pbp, 0x08, &0x28u32.to_le_bytes());
    let end = (0x28 + sfo.len()) as u32;
    for slot in 0..7 {
        put(&mut pbp, 0x0C + slot * 4, &end.to_le_bytes());
    }
    pbp.extend_from_slice(&sfo);
    dir.write("ABCD00007/PARAM.pbp", &pbp);
    dir.write("ABCD00007/00000000.edat", &[0x5A; 64]);
    match verdict_for_directory(&dir.path().join("ABCD00007"), Platform::PlayStationPortable) {
        DownloadVerdict::Match(meta) => {
            assert_eq!(meta.serial.as_deref(), Some("ABCD00007"));
            assert_eq!(meta.title.as_deref(), Some("TEST ADD-ON"));
            let trail = meta.evidence.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n");
            assert!(trail.contains("add-on"), "{trail}");
        }
        other => panic!("expected a PSP match, got {other:?}"),
    }
}

#[test]
fn pbp_psar_magic_separates_psp_from_ps1_classics() {
    let dir = TempDir::new("dl_pbp");
    let pbp = |psar_magic: &[u8]| {
        let mut file = buffer(0x100);
        put(&mut file, 0, &[0x00, b'P', b'B', b'P', 0x00, 0x00, 0x01, 0x00]);
        put(&mut file, 0x24, &0x80u32.to_le_bytes());
        put(&mut file, 0x80, psar_magic);
        file
    };
    let umd = dir.write("umd.pbp", &pbp(b"NPUMDIMG"));
    assert!(is_download_match(&download_verdict_for_file(&umd, Platform::PlayStationPortable)));
    let ps1 = dir.write("ps1.pbp", &pbp(b"PSISOIMG"));
    assert!(matches!(download_verdict_for_file(&ps1, Platform::PlayStationPortable), DownloadVerdict::Conflict));
    assert!(is_download_match(&download_verdict_for_file(&ps1, Platform::PspPs1Classic)));
    assert!(matches!(download_verdict_for_file(&umd, Platform::PspPs1Classic), DownloadVerdict::Conflict));
    let homebrew = dir.write("hb.pbp", &pbp(b""));
    assert!(is_download_match(&download_verdict_for_file(&homebrew, Platform::PlayStationPortable)));
}

#[test]
fn dsiware_and_ps1_classic_are_their_own_platforms() {
    assert_eq!(Platform::from_igdb_id(Platform::DsiWare.igdb_id()), Some(Platform::DsiWare));
    assert_eq!(Platform::from_igdb_id(Platform::PspPs1Classic.igdb_id()), Some(Platform::PspPs1Classic));
    assert_ne!(Platform::DsiWare, Platform::NintendoDsi);
    assert_ne!(Platform::PspPs1Classic, Platform::PlayStationPortable);

    // A single DSiWare SRL is DSiWare, not a plain DSi cartridge dump.
    let dir = TempDir::new("dsiware_file");
    let mut srl = buffer(0x5000);
    put(&mut srl, 0x00, b"TEST TITLE");
    put(&mut srl, 0x0C, b"TSTA");
    put(&mut srl, 0x12, &[0x03]);
    put(&mut srl, 0xC0, &NINTENDO_LOGO_156);
    put(&mut srl, 0x234, &[0x04, 0x00, 0x03, 0x00]);
    let path = dir.write("title.srl", &srl);
    assert!(nintendo::parse_nds_header_metadata_from_bytes(&path, &srl, Platform::NintendoDsi).is_none());
    assert!(is_download_match(&download_verdict_for_file(&path, Platform::DsiWare)));
}

// ---------------------------------------------------------------------------
// Platform hints (MetadataArgs::preferred_platform / detect::hinted_metadata_cycle)
// ---------------------------------------------------------------------------

fn genesis_rom() -> Vec<u8> {
    let mut rom = buffer(0x200);
    put(&mut rom, 0x100, b"SEGA MEGA DRIVE ");
    put(&mut rom, 0x120, b"TEST TITLE");
    put(&mut rom, 0x180, b"GM 00000000-00");
    rom
}

// Path heuristic off: the scratch dir plays the scan root here, and a root (or anything
// above it) never counts as path evidence -- only the hint under test may steer detection.
const EXT_ONLY: detect::HeuristicFlags = detect::HeuristicFlags { ext: true, path: false };

#[test]
fn a_matching_hint_settles_detection_as_hint() {
    let dir = TempDir::new("hint_match");
    let rom = dir.write("game.md", &genesis_rom());

    let outcome = detect::hinted_metadata_cycle(&rom, Some(Platform::SegaGenesis), EXT_ONLY);
    assert_eq!(outcome.platform, Platform::SegaGenesis);
    assert_eq!(outcome.metadata.reason, RomReason::Hint);
}

#[test]
fn a_wrong_but_plausible_hint_falls_back_to_normal_detection() {
    // A Genesis cart under a folder resolved as Pico (both use .md): the Pico parser rejects
    // the console name, and detection still lands on Genesis -- never a wrong answer.
    let dir = TempDir::new("hint_mislabeled");
    let rom = dir.write("game.md", &genesis_rom());

    let outcome = detect::hinted_metadata_cycle(&rom, Some(Platform::SegaPico), EXT_ONLY);
    assert_eq!(outcome.platform, Platform::SegaGenesis);
    assert_eq!(outcome.metadata.reason, RomReason::DeepCycle, "the hint didn't settle it");
    assert_eq!(outcome.trail.iter().filter(|(p, _)| *p == Platform::SegaPico).count(), 1, "hint tried exactly once");
}

#[test]
fn a_hint_the_extension_rules_out_is_never_tried() {
    // A stray .md under a PlayStation folder is a misplaced file, not a PlayStation game.
    let dir = TempDir::new("hint_implausible");
    let rom = dir.write("game.md", &genesis_rom());

    let outcome = detect::hinted_metadata_cycle(&rom, Some(Platform::PlayStation), EXT_ONLY);
    assert_eq!(outcome.platform, Platform::SegaGenesis);
    assert!(outcome.trail.iter().all(|(p, _)| *p != Platform::PlayStation), "implausible hint must not be tried: {:?}", outcome.trail);
}

#[test]
fn no_hint_is_identical_to_the_unhinted_cycle() {
    let dir = TempDir::new("hint_none");
    let rom = dir.write("game.md", &genesis_rom());

    let hinted = detect::hinted_metadata_cycle(&rom, None, EXT_ONLY);
    let plain = detect::metadata_cycle(&rom, detect::resolve_candidates(&rom, None, EXT_ONLY), EXT_ONLY);
    assert_eq!(hinted.platform, plain.platform);
    assert_eq!(hinted.remaining, plain.remaining);
    assert_eq!(hinted.trail, plain.trail);
}

// ---------------------------------------------------------------------------
// confirm_platform: a platform only on a positive signature
// ---------------------------------------------------------------------------

#[test]
fn confirm_platform_needs_the_platforms_own_signature() {
    use romexplorer::rom::detect::{confirm_platform, reason::RomReason};
    let dir = TempDir::new("confirm_platform");

    // SNES: the Super Famicom parser checks the header; the answer keeps the name asked for.
    let sfc = dir.write("Game (USA).sfc", &fixtures::snes_rom("TEST GAME", 0x40000, 1));
    let snes = confirm_platform(&sfc, Platform::Snes).expect("a valid header confirms SNES");
    assert_eq!(snes.platform, Platform::Snes);
    assert_eq!(snes.reason, RomReason::Signature);
    assert_eq!(confirm_platform(&sfc, Platform::SuperFamicom).map(|m| m.platform), Some(Platform::SuperFamicom));
    // Junk under the same name and extension: no header, no platform.
    let junk = dir.write("Junk (USA).sfc", &vec![3u8; 0x40000]);
    assert!(confirm_platform(&junk, Platform::Snes).is_none(), "an extension is not a signature");

    // PlayStation: SYSTEM.CNF in the disc's ISO9660 filesystem.
    let bin = dir.write("Disc (USA).bin", &fixtures::ps1_disc("SLUS_000.01", 64, 1));
    let cue = dir.write("Disc (USA).cue", fixtures::ps1_cue("Disc (USA).bin").as_bytes());
    for input in [&bin, &cue] {
        let ps1 = confirm_platform(input, Platform::PlayStation).unwrap_or_else(|| panic!("{} confirms PlayStation", input.display()));
        assert_eq!(ps1.platform, Platform::PlayStation);
    }
    assert!(confirm_platform(&bin, Platform::GameCube).is_none(), "a wrong hint is a negative");
    assert!(confirm_platform(&bin, Platform::SegaSaturn).is_none());
    // Printable bytes where a generic disc title would be read prove nothing.
    let mut fake = vec![0u8; 2352 * 64];
    for (i, b) in fake.iter_mut().enumerate() {
        *b = b'A' + (i % 26) as u8;
    }
    let fake = dir.write("Fake (USA).bin", &fake);
    assert!(confirm_platform(&fake, Platform::PlayStation).is_none(), "text bytes are not a filesystem");

    // NES: the iNES magic.
    let mut nes = b"NES\x1A".to_vec();
    nes.extend([2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    nes.extend((0..(2 * 16384 + 8192)).map(|i| (i % 251) as u8));
    let nes = dir.write("Cart (USA).nes", &nes);
    assert_eq!(confirm_platform(&nes, Platform::Nes).map(|m| m.platform), Some(Platform::Nes));

    // No signature check exists for this platform: never confirmed, whatever the file is.
    let a26 = dir.write("Atari (USA).a26", &vec![0xEAu8; 4096]);
    assert!(confirm_platform(&a26, Platform::Atari2600).is_none());
    assert!(confirm_platform(&a26, Platform::Unknown).is_none());
}

// ---------------------------------------------------------------------------
// PS Vita: folders, VPK, .psv, game-card images
// ---------------------------------------------------------------------------

fn vita_sfo(title_id: &str, category: &str) -> Vec<u8> {
    synthetic_sfo_values(&[
        ("APP_VER", SfoVal::Text("01.00")),
        ("CATEGORY", SfoVal::Text(category)),
        ("PSP2_SYSTEM_VER", SfoVal::Int(0)),
        ("TITLE", SfoVal::Text("TEST TITLE")),
        ("TITLE_ID", SfoVal::Text(title_id)),
    ])
}

#[test]
fn vita_app_folders_are_recognized_by_their_sce_sys() {
    let game = TempDir::new("vita_folder");
    game.write("sce_sys/param.sfo", &vita_sfo("ABCD00010", "gd"));
    game.write("sce_sys/package/work.bin", &[0u8; 512]);
    game.write("eboot.bin", b"SCE\0\x00\x00\x00\x03");
    // Game data that sorts before sce_sys must not hide it.
    for i in 0..8 {
        game.write(&format!("data/{i:03}.dat"), b"payload");
    }
    match verdict_for_directory(game.path(), Platform::PlayStationVita) {
        DownloadVerdict::Match(meta) => {
            assert_eq!(meta.serial.as_deref(), Some("ABCD00010"));
            assert_eq!(meta.title.as_deref(), Some("TEST TITLE"));
            let trail = meta.evidence.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n");
            assert!(trail.contains("NoNpDrm"), "{trail}");
        }
        other => panic!("expected a Vita match, got {other:?}"),
    }
    assert!(matches!(verdict_for_directory(game.path(), Platform::PlayStationPortable), DownloadVerdict::Conflict));

    // A patch folder says update.
    let patch = TempDir::new("vita_patch");
    patch.write("sce_sys/param.sfo", &vita_sfo("ABCD00010", "gp"));
    match verdict_for_directory(patch.path(), Platform::PlayStationVita) {
        DownloadVerdict::Match(meta) => assert!(meta.evidence.iter().any(|e| e.to_string().contains("content: update"))),
        other => panic!("expected a Vita match, got {other:?}"),
    }

    // A PS3 SFO under sce_sys is not a Vita one.
    let odd = TempDir::new("vita_folder_odd");
    odd.write("sce_sys/param.sfo", &synthetic_sfo(&[("CATEGORY", "HG"), ("TITLE_ID", "ABCD00011")]));
    assert!(matches!(verdict_for_directory(odd.path(), Platform::PlayStationVita), DownloadVerdict::Unknown));
}

#[test]
fn a_game_tree_with_subfolders_is_one_game_in_a_scan() {
    use romexplorer::rom::scan::{game_root_bundle, PlatformContext};
    let root = TempDir::new("vita_walk");
    root.write("Vita/One/sce_sys/param.sfo", &vita_sfo("ABCD00012", "gd"));
    root.write("Vita/One/eboot.bin", b"SCE\0");
    root.write("Vita/One/data/deep/file.bin", b"x");
    root.write("PS3/Two/PS3_GAME/PARAM.SFO", &synthetic_sfo(&[("CATEGORY", "DG"), ("TITLE_ID", "ABCD00013")]));
    root.write("PS3/Two/PS3_GAME/USRDIR/EBOOT.BIN", b"SCE\0");
    root.write("Other/Three/sub/file.bin", b"x");

    let ctx = romexplorer::rom::scan::ScanContext {
        dir_type: romexplorer::rom::detect::systemfile::DirType::Rom,
        platform: PlatformContext::None,
        heuristics: detect::HeuristicFlags::ALL,
        shapes: Vec::new(),
        files_seen: None,
    };
    let hint = |dir: &str| game_root_bundle(&root.path().join(dir), root.path(), &ctx).and_then(|b| b.platform_hint);
    assert_eq!(hint("Vita/One"), Some(Platform::PlayStationVita));
    assert_eq!(hint("PS3/Two"), Some(Platform::PlayStation3));
    assert_eq!(hint("Other/Three"), None);
    assert_eq!(game_root_bundle(root.path(), root.path(), &ctx).map(|b| b.path), None, "never the scan root");

    let forced = romexplorer::rom::scan::ScanContext { platform: PlatformContext::Forced(Platform::PlayStation3), ..ctx.clone() };
    assert!(game_root_bundle(&root.path().join("Vita/One"), root.path(), &forced).is_none(), "a forced platform that is another one wins");

    // The walk: each game tree is one candidate, nothing inside it is one of its own.
    let walked = romexplorer::rom::scan::scan(root.path(), &romexplorer::rom::scan::PlatformLayout::Unseparated, &ctx, false);
    let bundles: Vec<_> = walked.rom_candidates.iter().filter(|c| c.platform_hint.is_some()).map(|c| c.path.strip_prefix(root.path()).unwrap().to_path_buf()).collect();
    assert!(bundles.contains(&PathBuf::from("Vita/One")) && bundles.contains(&PathBuf::from("PS3/Two")), "{bundles:?}");
    assert!(walked.rom_candidates.iter().all(|c| !c.path.starts_with(root.path().join("Vita/One/data"))));
}

#[test]
fn psv_digital_dump_reads_the_package_inside() {
    let dir = TempDir::new("vita_psv_digital");
    let pkg = synthetic_pkg(2, Some(0x15), Some(2), "UP0000-ABCD00014_00-TESTCONTENT00001", Some(&vita_sfo("ABCD00014", "gd")));
    let mut psv = buffer(0x400);
    put(&mut psv, 0, b"PSV\0");
    put(&mut psv, 4, &1u32.to_le_bytes());
    put(&mut psv, 8, &2u32.to_le_bytes()); // digital
    put(&mut psv, 0x60, &(pkg.len() as u64).to_le_bytes());
    put(&mut psv, 0x68, &2u64.to_le_bytes()); // package at sector 2
    psv.extend_from_slice(&pkg);
    let path = dir.write("game.psv", &psv);
    let meta = sony::read_vita_metadata(&path).expect("a Vita .psv");
    assert_eq!(meta.serial.as_deref(), Some("ABCD00014"));
    assert_eq!(meta.title.as_deref(), Some("TEST TITLE"));
    let trail = meta.evidence.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n");
    assert!(trail.contains("licence and package") && trail.contains("0x400"), "{trail}");
}

/// A Vita game-card image: the MBR, one exFAT partition at sector 4 with 512-byte clusters, and
/// `app/<title id>/sce_sys/param.sfo` in it.
fn synthetic_card_image(title_id: &str, sfo: &[u8]) -> Vec<u8> {
    const PART: usize = 4 * 512;
    let mut img = buffer(PART + 16 * 512);
    put(&mut img, 0, b"Sony Computer Entertainment Inc.");
    put(&mut img, 0x50, &4u32.to_le_bytes());
    put(&mut img, 0x54, &16u32.to_le_bytes());
    put(&mut img, PART + 3, b"EXFAT   ");
    put(&mut img, PART + 0x50, &1u32.to_le_bytes()); // FAT at sector 1
    put(&mut img, PART + 0x58, &2u32.to_le_bytes()); // cluster heap at sector 2
    put(&mut img, PART + 0x60, &2u32.to_le_bytes()); // root in cluster 2
    img[PART + 0x6C] = 9;
    img[PART + 0x6D] = 0;
    // FAT: the root ends in its first cluster.
    put(&mut img, PART + 512 + 2 * 4, &0xFFFF_FFFFu32.to_le_bytes());
    let cluster = |n: usize| PART + 2 * 512 + (n - 2) * 512;
    let entry = |img: &mut Vec<u8>, at: usize, name: &str, dir: bool, first: u32, len: u64| {
        let units: Vec<u16> = name.encode_utf16().collect();
        img[at] = 0x85;
        img[at + 1] = 2;
        img[at + 4] = if dir { 0x10 } else { 0x20 };
        img[at + 32] = 0xC0;
        img[at + 33] = 0x03; // allocated, no FAT chain
        img[at + 35] = units.len() as u8;
        img[at + 32 + 0x14..at + 32 + 0x18].copy_from_slice(&first.to_le_bytes());
        img[at + 32 + 0x18..at + 32 + 0x20].copy_from_slice(&len.to_le_bytes());
        img[at + 64] = 0xC1;
        for (i, u) in units.iter().enumerate() {
            img[at + 66 + i * 2..at + 68 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
    };
    entry(&mut img, cluster(2), "app", true, 3, 512);
    entry(&mut img, cluster(3), title_id, true, 4, 512);
    entry(&mut img, cluster(4), "sce_sys", true, 5, 512);
    entry(&mut img, cluster(5), "param.sfo", false, 6, sfo.len() as u64);
    put(&mut img, cluster(6), sfo);
    img
}

#[test]
fn vita_card_images_give_their_title_id_from_the_exfat_tree() {
    let dir = TempDir::new("vita_card");
    let raw = dir.write("card.img", &synthetic_card_image("ABCD00015", &vita_sfo("ABCD00015", "gc")));
    let meta = sony::read_vita_metadata(&raw).expect("a Vita card image");
    assert_eq!(meta.serial.as_deref(), Some("ABCD00015"));
    assert_eq!(meta.title.as_deref(), Some("TEST TITLE"));

    // The SFO encrypted: the folder name still gives the title id.
    let sealed = dir.write("sealed.img", &synthetic_card_image("ABCD00016", &[0xA5; 64]));
    let meta = sony::read_vita_metadata(&sealed).expect("a Vita card image");
    assert_eq!(meta.serial.as_deref(), Some("ABCD00016"));
    assert_eq!(meta.title, None);

    // The same image inside a .psv.
    let mut psv = buffer(0x200);
    put(&mut psv, 0, b"PSV\0");
    put(&mut psv, 4, &1u32.to_le_bytes());
    put(&mut psv, 0x68, &1u64.to_le_bytes());
    psv.extend_from_slice(&synthetic_card_image("ABCD00017", &vita_sfo("ABCD00017", "gc")));
    let path = dir.write("card.psv", &psv);
    assert_eq!(sony::read_vita_metadata(&path).and_then(|m| m.serial).as_deref(), Some("ABCD00017"));

    // A plain zero-filled .img is nobody's.
    let blank = dir.write("blank.img", &buffer(0x1000));
    assert!(sony::read_vita_metadata(&blank).is_none());
}

#[test]
fn vpk_needs_both_eboot_and_param_sfo() {
    use romexplorer::rom::{native_lib::LibVariant, sevenzip_wrapper};
    if sevenzip_wrapper::find_library(LibVariant::Standard).is_none() && sevenzip_wrapper::find_library(LibVariant::Streaming).is_none() {
        eprintln!("7-Zip library not built: skipping");
        return;
    }
    let dir = TempDir::new("vita_vpk");
    let zip_of = |files: &[(&str, &[u8])]| {
        let mut out = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        for (name, data) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
            std::io::Write::write_all(&mut zip, data).unwrap();
        }
        zip.finish().unwrap();
        out.into_inner()
    };
    let sfo = vita_sfo("ABCD00018", "gd");
    let vpk = dir.write("game.vpk", &zip_of(&[("eboot.bin", b"SCE\0"), ("sce_sys/param.sfo", &sfo)]));
    let meta = sony::read_vita_metadata(&vpk).expect("a VPK");
    assert_eq!(meta.serial.as_deref(), Some("ABCD00018"));
    let no_eboot = dir.write("data.vpk", &zip_of(&[("sce_sys/param.sfo", &sfo)]));
    assert!(sony::read_vita_metadata(&no_eboot).is_none());
}

/// A PSP package whose item table and PARAM.SFO are encrypted the way the PSN writes them,
/// with the PSP key on the SFO's item (type byte 0x90).
fn synthetic_encrypted_psp_pkg(content_id: &str, sfo: &[u8]) -> Vec<u8> {
    const PSP_KEY: [u8; 16] = [0x07, 0xf2, 0xc6, 0x82, 0x90, 0xb5, 0x0d, 0x2c, 0x33, 0x81, 0x8d, 0x70, 0x9b, 0x60, 0xe6, 0x2b];
    let iv = [0x5Au8; 16];
    let mut pkg = synthetic_pkg(2, Some(0x07), Some(1), content_id, None);
    let body = 0x800usize;
    put(&mut pkg, 0x14, &1u32.to_be_bytes());
    put(&mut pkg, 0x20, &(body as u64).to_be_bytes());
    put(&mut pkg, 0x70, &iv);
    // Body: one 32-byte item entry, its name at 0x20, its data at 0x40.
    let mut entry = [0u8; 32];
    entry[0..4].copy_from_slice(&0x20u32.to_be_bytes());
    entry[4..8].copy_from_slice(&9u32.to_be_bytes());
    entry[8..16].copy_from_slice(&0x40u64.to_be_bytes());
    entry[16..24].copy_from_slice(&(sfo.len() as u64).to_be_bytes());
    entry[24] = 0x90;
    let mut plain = vec![0u8; 0x40 + sfo.len()];
    plain[..32].copy_from_slice(&entry);
    plain[0x20..0x29].copy_from_slice(b"PARAM.SFO");
    plain[0x40..].copy_from_slice(sfo);
    // CTR mode: encrypting is the same XOR as decrypting.
    sony::aes_ctr_xor(&PSP_KEY, &iv, 0, &mut plain);
    pkg.resize(body, 0);
    pkg.extend_from_slice(&plain);
    pkg
}

#[test]
fn psp_pkg_title_is_read_from_its_encrypted_sfo_only_when_asked() {
    let dir = TempDir::new("dl_pkg_psp_enc");
    let sfo = synthetic_sfo_values(&[
        ("CATEGORY", SfoVal::Text("EG")),
        ("DISC_ID", SfoVal::Text("ABCD00006")),
        ("TITLE", SfoVal::Text("TEST TITLE")),
    ]);
    let path = dir.write("enc.pkg", &synthetic_encrypted_psp_pkg("UP0000-ABCD00006_00-TESTCONTENT00001", &sfo));
    let pkg = sony::Pkg::read(&path).expect("a PKG header");
    sony::set_read_package_titles(true);
    let read = pkg.decrypt_sfo(&path);
    sony::set_read_package_titles(false);
    let read = read.expect("the SFO decrypts with the PSP key");
    assert_eq!(read.title().as_deref(), Some("TEST TITLE"));
    assert_eq!(read.title_id().as_deref(), Some("ABCD00006"));
    assert!(pkg.decrypt_sfo(&path).is_none(), "off by default: no decryption");
}

/// A PFS0 (an NSP's container) holding `files`, in order.
fn synthetic_pfs0(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut names = Vec::new();
    let mut entries = Vec::new();
    let mut data = Vec::new();
    for (name, bytes) in files {
        entries.extend_from_slice(&(data.len() as u64).to_le_bytes());
        entries.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        entries.extend_from_slice(&(names.len() as u32).to_le_bytes());
        entries.extend_from_slice(&0u32.to_le_bytes());
        names.extend_from_slice(name.as_bytes());
        names.push(0);
        data.extend_from_slice(bytes);
    }
    while names.len() % 0x10 != 0 {
        names.push(0);
    }
    let mut out = b"PFS0".to_vec();
    out.extend_from_slice(&(files.len() as u32).to_le_bytes());
    out.extend_from_slice(&(names.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&entries);
    out.extend_from_slice(&names);
    out.extend_from_slice(&data);
    out
}

#[test]
fn switch_nsp_gives_its_ncas_and_the_ticket_title_id() {
    use romexplorer::rom::platforms::switch;
    use romexplorer::rom::hash::HashFlags;
    let dir = TempDir::new("switch_nsp");
    let ticketed = dir.write(
        "ticketed.nsp",
        &synthetic_pfs0(&[
            ("00112233445566778899aabbccddeeff.nca", b"first nca"),
            ("ffeeddccbbaa99887766554433221100.cnmt.nca", b"meta"),
            ("0100abcd123450000000000000000001.tik", &[0u8; 0x40]),
        ]),
    );
    let meta = switch::read_switch_metadata(&ticketed).expect("an NSP");
    assert_eq!(meta.serial.as_deref(), Some("0100ABCD12345000"));
    assert!(romexplorer::rom::detect::confirm_platform(&ticketed, Platform::NintendoSwitch).is_some());

    // No ticket: the title id is only in the name.
    let bare = dir.write("TEST TITLE [0100ABCD12345800][v65536].nsp", &synthetic_pfs0(&[("00112233445566778899aabbccddeeff.nca", b"update nca")]));
    assert_eq!(switch::read_switch_metadata(&bare).expect("an NSP").serial, None);
    assert_eq!(switch::title_id_of(&bare), Some(("0100ABCD12345800".to_string(), false)));

    // Each NCA is hashed on its own; the ticket is not.
    let hashed = switch::hash_ncas(&ticketed, HashFlags::CRC32).unwrap();
    assert_eq!(hashed.len(), 2);
    assert_eq!(hashed[0].0.size, 9);
    assert_eq!(hashed[0].1.crc32.as_deref().map(str::to_ascii_lowercase), Some(format!("{:08x}", crc32fast::hash(b"first nca"))));

    // A PFS0 with no NCA, or a broken one, is not a Switch package.
    let empty = dir.write("empty.nsp", &synthetic_pfs0(&[("readme.txt", b"x")]));
    assert!(switch::read_switch_metadata(&empty).is_none());
    let mut broken = synthetic_pfs0(&[("00112233445566778899aabbccddeeff.nca", b"cut short")]);
    broken.truncate(broken.len() - 4);
    assert!(switch::read_switch_metadata(&dir.write("broken.nsp", &broken)).is_none());

    let mut xci = vec![0u8; 0x200];
    xci[0x100..0x104].copy_from_slice(b"HEAD");
    assert!(switch::read_switch_metadata(&dir.write("cart.xci", &xci)).is_some());
}
