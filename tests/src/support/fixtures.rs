//! Small synthetic ROMs whose platform signatures are real, so a test can exercise
//! signature-confirmed detection without shipping dumps: an SNES cartridge with a valid
//! LoROM header and checksum pair, and a PlayStation disc with an ISO9660 filesystem whose
//! `SYSTEM.CNF` names its boot file. Everything else in them is filler.

#![allow(dead_code)]

/// An SNES LoROM image of `size` bytes (at least 32 KiB): header at 0x7FC0 with `title`, map
/// mode 0x20, region 0x01, and a checksum/complement pair that adds up to 0xFFFF -- what the
/// Super Famicom parser checks. `fill` varies the bytes so two images differ.
pub fn snes_rom(title: &str, size: usize, fill: u8) -> Vec<u8> {
    let size = size.max(0x8000);
    let mut rom: Vec<u8> = (0..size).map(|i| (i as u8).wrapping_mul(31).wrapping_add(fill)).collect();
    let header = 0x7FC0;
    let mut name = [b' '; 21];
    for (slot, byte) in name.iter_mut().zip(title.bytes()) {
        *slot = byte;
    }
    rom[header..header + 21].copy_from_slice(&name);
    rom[header + 0x15] = 0x20; // LoROM
    rom[header + 0x16] = 0x00; // ROM only
    rom[header + 0x17] = 0x08; // 256 KiB
    rom[header + 0x19] = 0x01; // North America
    // The real checksum: the byte sum with the pair set to (0xFFFF, 0x0000) first, whose
    // bytes add 0x1FE -- the same as any complementary pair, so the sum doesn't move.
    rom[header + 0x1C..header + 0x20].copy_from_slice(&[0xFF, 0xFF, 0x00, 0x00]);
    let sum = rom.iter().fold(0u16, |acc, b| acc.wrapping_add(*b as u16));
    rom[header + 0x1C..header + 0x1E].copy_from_slice(&(!sum).to_le_bytes());
    rom[header + 0x1E..header + 0x20].copy_from_slice(&sum.to_le_bytes());
    rom
}

const SECTOR: usize = 2048;
const RAW_SECTOR: usize = 2352;

fn both_u32(value: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&value.to_le_bytes());
    out[4..].copy_from_slice(&value.to_be_bytes());
    out
}

fn both_u16(value: u16) -> [u8; 4] {
    let mut out = [0u8; 4];
    out[..2].copy_from_slice(&value.to_le_bytes());
    out[2..].copy_from_slice(&value.to_be_bytes());
    out
}

fn dir_record(lba: u32, len: u32, dir: bool, name: &[u8]) -> Vec<u8> {
    let mut record = vec![0u8; 33 + name.len() + (name.len() + 1) % 2];
    record[0] = record.len() as u8;
    record[2..10].copy_from_slice(&both_u32(lba));
    record[10..18].copy_from_slice(&both_u32(len));
    record[25] = if dir { 0x02 } else { 0x00 };
    record[28..32].copy_from_slice(&both_u16(1));
    record[32] = name.len() as u8;
    record[33..33 + name.len()].copy_from_slice(name);
    record
}

fn padded(text: &str, len: usize) -> Vec<u8> {
    let mut out = vec![b' '; len];
    out[..text.len().min(len)].copy_from_slice(&text.as_bytes()[..text.len().min(len)]);
    out
}

/// A PlayStation disc as a raw MODE2/2352 track (`.bin`) of `sectors` sectors: an ISO9660
/// volume ("PLAYSTATION" system id) whose root holds `SYSTEM.CNF` (`BOOT = cdrom:\<serial>;1`)
/// and the boot file it names. `serial` looks like `SLUS_000.01`. `fill` varies the padding.
pub fn ps1_disc(serial: &str, sectors: u32, fill: u8) -> Vec<u8> {
    let sectors = sectors.max(32);
    let mut user: Vec<Vec<u8>> = (0..sectors).map(|i| vec![(i as u8).wrapping_add(fill); SECTOR]).collect();

    let cnf = format!("BOOT = cdrom:\\{serial};1\r\nTCB = 4\r\nEVENT = 10\r\nSTACK = 801FFFF0\r\n");
    let (root_lba, cnf_lba, exe_lba) = (18u32, 19u32, 20u32);
    let exe = b"PS-X EXE".to_vec();

    let mut pvd = vec![0u8; SECTOR];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    pvd[8..40].copy_from_slice(&padded("PLAYSTATION", 32));
    pvd[40..72].copy_from_slice(&padded("TEST_DISC", 32));
    pvd[80..88].copy_from_slice(&both_u32(sectors));
    pvd[120..124].copy_from_slice(&both_u16(1));
    pvd[124..128].copy_from_slice(&both_u16(1));
    pvd[128..132].copy_from_slice(&both_u16(SECTOR as u16));
    let root = dir_record(root_lba, SECTOR as u32, true, &[0]);
    pvd[156..156 + root.len()].copy_from_slice(&root);
    user[16] = pvd;
    let mut terminator = vec![0u8; SECTOR];
    terminator[0] = 0xFF;
    terminator[1..6].copy_from_slice(b"CD001");
    terminator[6] = 1;
    user[17] = terminator;

    let mut dir = Vec::new();
    dir.extend(dir_record(root_lba, SECTOR as u32, true, &[0]));
    dir.extend(dir_record(root_lba, SECTOR as u32, true, &[1]));
    dir.extend(dir_record(exe_lba, exe.len() as u32, false, format!("{serial};1").as_bytes()));
    dir.extend(dir_record(cnf_lba, cnf.len() as u32, false, b"SYSTEM.CNF;1"));
    dir.resize(SECTOR, 0);
    user[root_lba as usize] = dir;
    let mut cnf_sector = cnf.into_bytes();
    cnf_sector.resize(SECTOR, 0);
    user[cnf_lba as usize] = cnf_sector;
    let mut exe_sector = exe;
    exe_sector.resize(SECTOR, 0);
    user[exe_lba as usize] = exe_sector;

    // Raw sectors: sync, header (MSF + mode 2), form-1 subheader, data, EDC/ECC left zero.
    let mut raw = Vec::with_capacity(sectors as usize * RAW_SECTOR);
    for (lba, data) in user.into_iter().enumerate() {
        let mut sector = vec![0u8; RAW_SECTOR];
        sector[1..11].fill(0xFF);
        let abs = lba as u32 + 150;
        let bcd = |v: u32| (((v / 10) << 4) | (v % 10)) as u8;
        sector[12] = bcd(abs / 4500);
        sector[13] = bcd((abs / 75) % 60);
        sector[14] = bcd(abs % 75);
        sector[15] = 2;
        sector[24..24 + SECTOR].copy_from_slice(&data);
        raw.extend(sector);
    }
    raw
}

/// The cue for a [`ps1_disc`] track named `bin`.
pub fn ps1_cue(bin: &str) -> String {
    format!("FILE \"{bin}\" BINARY\r\n  TRACK 01 MODE2/2352\r\n    INDEX 01 00:00:00\r\n")
}
