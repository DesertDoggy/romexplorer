//! The Famicom Disk System for the music search: reading `.fds` disk images, and a stand-in
//! for the Disk System's BIOS, so no BIOS file is needed.
//!
//! The stand-in runs in place of the BIOS's code: when the CPU reaches an address in the
//! BIOS ($E000-$FFFF), [`hle`] does what that routine does and returns to the game. It loads
//! files straight from the image (no disk drive timing), passes the interrupts on to the
//! game's vectors as the BIOS does, and does the BIOS's small jobs games call (controller
//! reads, delays, waiting for the vertical blank). Routines that only draw are skipped, since
//! nothing is drawn.

use std::sync::Arc;

use super::m6502::{Bus, Cpu};

/// One side of a disk: its disk info block, and its files.
#[derive(Debug, Clone)]
pub struct Side {
    /// Block 1 (56 bytes, block code included).
    pub info: Vec<u8>,
    pub files: Vec<File>,
}

#[derive(Debug, Clone)]
pub struct File {
    pub id: u8,
    pub name: [u8; 8],
    pub addr: u16,
    /// 0 program (CPU memory), 1 pattern table, 2 name table (both picture memory).
    pub kind: u8,
    pub data: Vec<u8>,
}

impl Side {
    /// The disk ID the BIOS compares: disk info bytes $0F-$18.
    pub fn disk_id(&self) -> &[u8] {
        &self.info[0x0F..0x19]
    }

    /// Files with an ID up to this load at boot.
    pub fn boot_code(&self) -> u8 {
        self.info[0x19]
    }
}

/// A whole disk image.
#[derive(Debug, Clone)]
pub struct Disk {
    pub sides: Vec<Side>,
}

const SIDE_BYTES: usize = 65500;
const MAGIC: &[u8] = b"*NINTENDO-HVC*";

/// Whether `image` is a Disk System image (with or without the 16-byte header).
pub fn is_disk(image: &[u8]) -> bool {
    let body = strip_header(image);
    body.len() > 15 && body[0] == 1 && &body[1..15] == MAGIC
}

fn strip_header(image: &[u8]) -> &[u8] {
    if image.len() >= 16 && &image[0..4] == b"FDS\x1A" {
        &image[16..]
    } else {
        image
    }
}

/// Reads a `.fds` image: every side, every file the side's file count lists.
pub fn parse(image: &[u8]) -> Result<Disk, String> {
    let body = strip_header(image);
    let mut sides = Vec::new();
    for raw in body.chunks(SIDE_BYTES) {
        if raw.len() < 58 || raw[0] != 1 || &raw[1..15] != MAGIC {
            break;
        }
        let info = raw[0..56].to_vec();
        if raw[56] != 2 {
            return Err("the disk image's file count block is missing".into());
        }
        let count = raw[57] as usize;
        let mut files = Vec::new();
        let mut at = 58;
        while files.len() < count {
            let Some(h) = raw.get(at..at + 16) else {
                break;
            };
            if h[0] != 3 {
                break;
            }
            let size = u16::from_le_bytes([h[13], h[14]]) as usize;
            let Some(d) = raw.get(at + 16..at + 17 + size) else {
                break;
            };
            if d[0] != 4 {
                break;
            }
            let mut name = [0; 8];
            name.copy_from_slice(&h[3..11]);
            files.push(File {
                id: h[2],
                name,
                addr: u16::from_le_bytes([h[11], h[12]]),
                kind: h[15],
                data: d[1..].to_vec(),
            });
            at += 17 + size;
        }
        sides.push(Side { info, files });
    }
    if sides.is_empty() {
        return Err("not a Famicom Disk System image".into());
    }
    Ok(Disk { sides })
}

/// The BIOS's entry points (and its vectors' targets).
mod at {
    pub const NMI: u16 = 0xE18B;
    pub const IRQ: u16 = 0xE1C7;
    pub const RESET: u16 = 0xEE24;
    /// Where the CPU idles while VINTWait waits for the vertical blank.
    pub const IDLE: u16 = 0xFF00;
    pub const DELAY131: u16 = 0xE149;
    pub const DELAYMS: u16 = 0xE153;
    pub const DIS_PF_OBJ: u16 = 0xE161;
    pub const EN_PF_OBJ: u16 = 0xE16B;
    pub const DIS_OBJ: u16 = 0xE171;
    pub const EN_OBJ: u16 = 0xE178;
    pub const DIS_PF: u16 = 0xE17E;
    pub const EN_PF: u16 = 0xE185;
    pub const VINT_WAIT: u16 = 0xE1B2;
    pub const LOAD_FILES: u16 = 0xE1F8;
    pub const APPEND_FILE: u16 = 0xE237;
    pub const WRITE_FILE: u16 = 0xE239;
    pub const CHECK_FILE_COUNT: u16 = 0xE2B7;
    pub const ADJUST_FILE_COUNT: u16 = 0xE2BB;
    pub const SET_FILE_COUNT1: u16 = 0xE301;
    pub const SET_FILE_COUNT: u16 = 0xE305;
    pub const GET_DISK_INFO: u16 = 0xE32A;
    pub const VRAM_STRUCT_WRITE: u16 = 0xE7BB;
    pub const FETCH_DIRECT_PTR: u16 = 0xE844;
    pub const WRITE_VRAM_BUFFER: u16 = 0xE86A;
    pub const READ_VRAM_BUFFER: u16 = 0xE8B3;
    pub const PREPARE_VRAM_STRING: u16 = 0xE8D2;
    pub const PREPARE_VRAM_STRINGS: u16 = 0xE8E1;
    pub const GET_VRAM_BUFFER_BYTE: u16 = 0xE94F;
    pub const PIXEL2NAM: u16 = 0xE97D;
    pub const NAM2PIXEL: u16 = 0xE997;
    pub const RANDOM: u16 = 0xE9B1;
    pub const SPRITE_DMA: u16 = 0xE9C8;
    pub const COUNTER_LOGIC: u16 = 0xE9D3;
    pub const READ_PADS: u16 = 0xE9EB;
    pub const OR_PADS: u16 = 0xEA0D;
    pub const READ_DOWN_PADS: u16 = 0xEA1A;
    pub const READ_OR_DOWN_PADS: u16 = 0xEA1F;
    pub const READ_DOWN_VERIFY_PADS: u16 = 0xEA36;
    pub const READ_OR_DOWN_VERIFY_PADS: u16 = 0xEA4C;
    pub const READ_DOWN_EXP_PADS: u16 = 0xEA68;
    pub const VRAM_FILL: u16 = 0xEA84;
    pub const MEM_FILL: u16 = 0xEAD2;
    pub const SET_SCROLL: u16 = 0xEAEA;
    /// SetScroll past its read of the video status (a few games call in here).
    pub const SET_SCROLL_NO_STATUS: u16 = 0xEAED;
    pub const JUMP_ENGINE: u16 = 0xEAFD;
    pub const READ_KEYBOARD: u16 = 0xEB13;
    pub const LOAD_TILESET: u16 = 0xEBAF;
    pub const UPLOAD_OBJECT: u16 = 0xEC22;
}

/// The 8 KB that sits at $E000: only its vectors are ever read as code addresses; any other
/// byte the game reads from the BIOS is $00.
pub fn bios_image() -> Vec<u8> {
    let mut b = vec![0u8; 0x2000];
    for (v, a) in [(0x1FFA, at::NMI), (0x1FFC, at::RESET), (0x1FFE, at::IRQ)] {
        b[v..v + 2].copy_from_slice(&a.to_le_bytes());
    }
    b
}

/// What the stand-in remembers between calls.
#[derive(Clone)]
pub struct State {
    pub disk: Arc<Disk>,
    /// The side in the drive.
    pub side: usize,
    /// VINTWait is waiting for the vertical blank's interrupt.
    waiting: bool,
    /// BIOS addresses reached that the stand-in doesn't know (for the test tool's notes).
    pub unknown: Vec<u16>,
    /// Reads of the drive's registers $4030-$4033, and LoadFiles calls (for the test
    /// tool's notes: a game with its own disk loader polls the drive).
    pub drive_reads: [u32; 4],
    pub loads: u32,
    /// The last addresses run (for the test tool's notes), newest at `trail_at - 1`.
    pub trail: [u16; 24],
    pub trail_at: usize,
}

impl State {
    pub fn new(disk: Arc<Disk>) -> Self {
        Self {
            disk,
            side: 0,
            waiting: false,
            unknown: Vec::new(),
            drive_reads: [0; 4],
            loads: 0,
            trail: [0; 24],
            trail_at: 0,
        }
    }
}

fn read16<B: Bus>(bus: &mut B, a: u16) -> u16 {
    u16::from_le_bytes([bus.read(a), bus.read(a.wrapping_add(1))])
}

fn pull<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u8 {
    cpu.s = cpu.s.wrapping_add(1);
    bus.read(0x100 | cpu.s as u16)
}

/// Drops the calls in progress the stack pointer has left.
fn settle(cpu: &mut Cpu) {
    while let Some(&(_, s)) = cpu.calls.last() {
        if cpu.s.wrapping_sub(s) as i8 >= 0 {
            cpu.calls.pop();
        } else {
            break;
        }
    }
}

/// RTS, after skipping `params` bytes of inline parameters after the JSR.
fn rts<B: Bus>(cpu: &mut Cpu, bus: &mut B, params: u16) {
    let lo = pull(cpu, bus);
    let hi = pull(cpu, bus);
    cpu.pc = u16::from_le_bytes([lo, hi])
        .wrapping_add(1)
        .wrapping_add(params);
    settle(cpu);
}

fn rti<B: Bus>(cpu: &mut Cpu, bus: &mut B) {
    cpu.p = (pull(cpu, bus) & !0x10) | 0x20;
    let lo = pull(cpu, bus);
    let hi = pull(cpu, bus);
    cpu.pc = u16::from_le_bytes([lo, hi]);
    settle(cpu);
}

/// The `n`th inline 16-bit parameter after the JSR that called the current routine.
fn param<B: Bus>(cpu: &Cpu, bus: &mut B, n: u16) -> u16 {
    let ret = u16::from_le_bytes([
        bus.read(0x100 | cpu.s.wrapping_add(1) as u16),
        bus.read(0x100 | cpu.s.wrapping_add(2) as u16),
    ]);
    read16(bus, ret.wrapping_add(1 + 2 * n))
}

fn set_a(cpu: &mut Cpu, v: u8) {
    cpu.a = v;
    cpu.p = (cpu.p & !0x82) | (v & 0x80) | if v == 0 { 0x02 } else { 0 };
}

/// Jumps to the game's vector at `vector`; the BIOS handler the interrupt entered becomes the
/// game's handler in the call shadow.
fn jump_vector<B: Bus>(cpu: &mut Cpu, bus: &mut B, vector: u16) {
    cpu.pc = read16(bus, vector);
    if let Some(top) = cpu.calls.last_mut() {
        top.0 = cpu.pc;
    }
}

/// The side whose disk ID matches `id` ($FF fields match anything).
fn find_side(disk: &Disk, id: &[u8], current: usize) -> usize {
    let matches = |s: &Side| {
        s.disk_id()
            .iter()
            .zip(id)
            .all(|(&d, &w)| w == 0xFF || d == w)
    };
    if disk.sides.get(current).is_some_and(matches) {
        return current;
    }
    disk.sides.iter().position(matches).unwrap_or(current)
}

fn read_id<B: Bus>(bus: &mut B, ptr: u16) -> Vec<u8> {
    (0..10).map(|i| bus.read(ptr.wrapping_add(i))).collect()
}

/// Loads the files `list` names (or, with a list starting $FF, the boot files) from the
/// current side into CPU memory. Returns how many were loaded.
fn load<B: Bus>(st: &State, bus: &mut B, list: &[u8]) -> u8 {
    let side = &st.disk.sides[st.side];
    let boot = list.first() == Some(&0xFF);
    let mut n = 0u8;
    for f in &side.files {
        let wanted = if boot {
            f.id <= side.boot_code()
        } else {
            list.contains(&f.id)
        };
        if !wanted {
            continue;
        }
        n = n.wrapping_add(1);
        // Picture memory isn't kept: nothing is drawn.
        if f.kind != 0 || f.addr as usize + f.data.len() > 0x10000 {
            continue;
        }
        for (i, &b) in f.data.iter().enumerate() {
            let a = f.addr.wrapping_add(i as u16);
            if a < 0x2000 || (0x6000..0xE000).contains(&a) {
                bus.write(a, b);
            }
        }
    }
    n
}

/// Reads the controllers as ReadPads does: $F5/$F6 the pads, $00/$01 the expansion port.
fn read_pads<B: Bus>(bus: &mut B) {
    let latch = bus.read(0x00FB);
    bus.write(0x4016, latch | 1);
    bus.write(0x4016, latch);
    let (mut p1, mut p2, mut e1, mut e2) = (0u8, 0u8, 0u8, 0u8);
    for _ in 0..8 {
        let a = bus.read(0x4016);
        let b = bus.read(0x4017);
        p1 = p1 << 1 | (a & 1);
        e1 = e1 << 1 | ((a >> 1) & 1);
        p2 = p2 << 1 | (b & 1);
        e2 = e2 << 1 | ((b >> 1) & 1);
    }
    bus.write(0x00F5, p1);
    bus.write(0x00F6, p2);
    bus.write(0x0000, e1);
    bus.write(0x0001, e2);
}

fn or_pads<B: Bus>(bus: &mut B) {
    let (e1, e2) = (bus.read(0x0000), bus.read(0x0001));
    let p1 = bus.read(0x00F5) | e1;
    let p2 = bus.read(0x00F6) | e2;
    bus.write(0x00F5, p1);
    bus.write(0x00F6, p2);
}

/// $F5/$F6 become the buttons newly pressed; $F7/$F8 the buttons held.
fn down_pads<B: Bus>(bus: &mut B) {
    for (now_at, held_at) in [(0x00F5u16, 0x00F7u16), (0x00F6, 0x00F8)] {
        let now = bus.read(now_at);
        let before = bus.read(held_at);
        bus.write(now_at, now & !before);
        bus.write(held_at, now);
    }
}

fn mask<B: Bus>(bus: &mut B, clear: u8, set: u8) {
    let v = (bus.read(0x00FE) & !clear) | set;
    bus.write(0x00FE, v);
    bus.write(0x2001, v);
}

/// Runs the BIOS routine the CPU is at (`cpu.pc` in $E000-$FFFF). Returns the cycles it took.
pub fn hle<B: Bus>(cpu: &mut Cpu, bus: &mut B, st: &mut State) -> u32 {
    let pc = cpu.pc;
    match pc {
        at::RESET => {
            // What the BIOS leaves behind once the boot files have loaded.
            for (a, v) in [
                (0x00F9u16, 0xFFu8),
                (0x00FA, 0x2E),
                (0x00FB, 0x00),
                (0x00FC, 0x00),
                (0x00FD, 0x00),
                (0x00FE, 0x06),
                (0x00FF, 0x10),
            ] {
                bus.write(a, v);
            }
            bus.write(0x4023, 0x00);
            bus.write(0x4023, 0x83);
            bus.write(0x4025, 0x2E);
            bus.write(0x4026, 0xFF);
            bus.write(0x4017, 0xC0);
            bus.write(0x2000, 0x10);
            bus.write(0x2001, 0x06);
            st.side = 0;
            load(st, bus, &[0xFF]);
            bus.write(0x0100, 0xC0);
            bus.write(0x0101, 0x80);
            bus.write(0x0102, 0x35);
            bus.write(0x0103, 0xAC);
            cpu.s = 0xFF;
            cpu.p = 0x24;
            cpu.calls.clear();
            cpu.pc = read16(bus, 0xDFFC);
            return 1000;
        }
        at::NMI => {
            if st.waiting {
                // VINTWait's own handler: back to its caller with NMIs off.
                st.waiting = false;
                for _ in 0..3 {
                    pull(cpu, bus);
                }
                settle(cpu);
                let ctrl = bus.read(0x00FF) & 0x7F;
                bus.write(0x00FF, ctrl);
                bus.write(0x2000, ctrl);
                bus.read(0x2002);
                cpu.p |= 0x04;
                rts(cpu, bus, 0);
                return 20;
            }
            match bus.read(0x0100) >> 6 {
                0 => rti(cpu, bus),
                1 => jump_vector(cpu, bus, 0xDFF6),
                2 => jump_vector(cpu, bus, 0xDFF8),
                _ => jump_vector(cpu, bus, 0xDFFA),
            }
            return 10;
        }
        at::IRQ => {
            if bus.read(0x0101) >> 6 == 3 {
                jump_vector(cpu, bus, 0xDFFE);
            } else {
                // Acknowledge (the disk's transfers aren't emulated) and return.
                bus.read(0x4030);
                rti(cpu, bus);
            }
            return 10;
        }
        at::IDLE => return 7,
        _ => {}
    }
    // Everything else is a subroutine the game called.
    let mut cycles = 30;
    match pc {
        at::VINT_WAIT => {
            st.waiting = true;
            let ctrl = bus.read(0x00FF) | 0x80;
            bus.write(0x00FF, ctrl);
            bus.write(0x2000, ctrl);
            cpu.pc = at::IDLE;
            return 20;
        }
        at::LOAD_FILES => {
            let id_at = param(cpu, bus, 0);
            let id = read_id(bus, id_at);
            let list_at = param(cpu, bus, 1);
            let list: Vec<u8> = (0..20)
                .map(|i| bus.read(list_at.wrapping_add(i)))
                .take_while(|&b| b != 0xFF)
                .collect();
            let list = if list.is_empty() { vec![0xFF] } else { list };
            st.side = find_side(&st.disk, &id, st.side);
            st.loads += 1;
            cpu.y = load(st, bus, &list);
            set_a(cpu, 0);
            rts(cpu, bus, 4);
            return 50_000;
        }
        at::APPEND_FILE | at::WRITE_FILE => {
            set_a(cpu, 0);
            rts(cpu, bus, 4);
            return 50_000;
        }
        at::CHECK_FILE_COUNT | at::ADJUST_FILE_COUNT | at::SET_FILE_COUNT1 | at::SET_FILE_COUNT => {
            set_a(cpu, 0);
            rts(cpu, bus, 2);
        }
        at::GET_DISK_INFO => {
            let to = param(cpu, bus, 0);
            let side = &st.disk.sides[st.side];
            let mut out = side.disk_id().to_vec();
            out.push(side.files.len() as u8);
            let mut size = 0usize;
            for f in &side.files {
                out.push(f.id);
                out.extend_from_slice(&f.name);
                size += f.data.len();
            }
            out.extend_from_slice(&(size as u16).to_be_bytes());
            for (i, &b) in out.iter().enumerate() {
                bus.write(to.wrapping_add(i as u16), b);
            }
            set_a(cpu, 0);
            rts(cpu, bus, 2);
        }
        at::DELAY131 => {
            rts(cpu, bus, 0);
            return 131;
        }
        at::DELAYMS => {
            let y = if cpu.y == 0 { 256 } else { cpu.y as u32 };
            cpu.x = 0;
            cpu.y = 0;
            rts(cpu, bus, 0);
            return 1790 * y + 5;
        }
        at::DIS_PF_OBJ | at::EN_PF_OBJ | at::DIS_OBJ | at::EN_OBJ | at::DIS_PF | at::EN_PF => {
            let (clear, set) = match pc {
                at::DIS_PF_OBJ => (0x18, 0),
                at::EN_PF_OBJ => (0, 0x18),
                at::DIS_OBJ => (0x10, 0),
                at::EN_OBJ => (0, 0x10),
                at::DIS_PF => (0x08, 0),
                _ => (0, 0x08),
            };
            mask(bus, clear, set);
            rts(cpu, bus, 0);
        }
        at::VRAM_STRUCT_WRITE | at::LOAD_TILESET => {
            rts(cpu, bus, 2);
            return 2000;
        }
        at::PREPARE_VRAM_STRING | at::PREPARE_VRAM_STRINGS => {
            set_a(cpu, 0xFF);
            rts(cpu, bus, 2);
            return 500;
        }
        at::FETCH_DIRECT_PTR => {
            // The pointer sits after the JSR that called the routine that called this one.
            let ra = 0x100 | cpu.s.wrapping_add(3) as u16;
            let rb = 0x100 | cpu.s.wrapping_add(4) as u16;
            let ret = u16::from_le_bytes([bus.read(ra), bus.read(rb)]);
            let ptr = read16(bus, ret.wrapping_add(1));
            bus.write(0x0000, ptr as u8);
            bus.write(0x0001, (ptr >> 8) as u8);
            let fixed = ret.wrapping_add(2).to_le_bytes();
            bus.write(ra, fixed[0]);
            bus.write(rb, fixed[1]);
            rts(cpu, bus, 0);
        }
        at::WRITE_VRAM_BUFFER => {
            bus.write(0x0301, 0);
            bus.write(0x0302, 0xFF);
            rts(cpu, bus, 0);
            return 1000;
        }
        at::GET_VRAM_BUFFER_BYTE => {
            cpu.p |= 0x01;
            rts(cpu, bus, 0);
        }
        at::PIXEL2NAM => {
            let (y, x) = (bus.read(0x0002), bus.read(0x0003));
            bus.write(0x0000, 0x20 | (y >> 6));
            bus.write(0x0001, ((y & 0x38) << 2) | (x >> 3));
            rts(cpu, bus, 0);
        }
        at::NAM2PIXEL => {
            let (hi, lo) = (bus.read(0x0000), bus.read(0x0001));
            bus.write(0x0002, ((hi & 3) << 6) | ((lo & 0xE0) >> 2));
            bus.write(0x0003, (lo & 0x1F) << 3);
            rts(cpu, bus, 0);
        }
        at::RANDOM => {
            // A shift register over Y bytes from zero page X, fed back from bits 1 of the
            // first two.
            let x = cpu.x;
            let a0 = bus.read(x as u16) & 0x02;
            let a1 = bus.read(x.wrapping_add(1) as u16) & 0x02;
            let mut carry = (a0 ^ a1) != 0;
            for i in 0..cpu.y.max(1) {
                let at = x.wrapping_add(i) as u16;
                let v = bus.read(at);
                bus.write(at, (v >> 1) | if carry { 0x80 } else { 0 });
                carry = v & 1 != 0;
            }
            rts(cpu, bus, 0);
        }
        at::SPRITE_DMA => {
            bus.write(0x2003, 0);
            bus.write(0x4014, 0x02);
            rts(cpu, bus, 0);
            return 530;
        }
        at::COUNTER_LOGIC => {
            // The first counter divides by ten; when it wraps, the others (to A) count down.
            let first = cpu.x as u16;
            let last = cpu.a as u16;
            let d = bus.read(first);
            if d == 0 {
                bus.write(first, 9);
                for a in first + 1..=last.max(first) {
                    let v = bus.read(a);
                    if v != 0 {
                        bus.write(a, v - 1);
                    }
                }
            } else {
                bus.write(first, d - 1);
            }
            rts(cpu, bus, 0);
        }
        at::READ_PADS => {
            read_pads(bus);
            rts(cpu, bus, 0);
        }
        at::OR_PADS => {
            or_pads(bus);
            rts(cpu, bus, 0);
        }
        at::READ_DOWN_PADS | at::READ_DOWN_VERIFY_PADS => {
            read_pads(bus);
            down_pads(bus);
            rts(cpu, bus, 0);
        }
        at::READ_OR_DOWN_PADS | at::READ_OR_DOWN_VERIFY_PADS => {
            read_pads(bus);
            or_pads(bus);
            down_pads(bus);
            rts(cpu, bus, 0);
        }
        at::READ_DOWN_EXP_PADS => {
            read_pads(bus);
            let now = [0x00F5u16, 0x00F6, 0x0000, 0x0001].map(|a| bus.read(a));
            for (i, &v) in now.iter().enumerate() {
                let held = 0x00F5 + i as u16;
                let before = bus.read(held);
                bus.write(0x00F1 + i as u16, v & !before);
                bus.write(held, v);
            }
            rts(cpu, bus, 0);
        }
        at::MEM_FILL => {
            let (v, first, last) = (cpu.a, cpu.x, cpu.y);
            for page in first..=last.max(first) {
                // The stack page is left alone: the call's own return address lives there.
                if page == 1 {
                    continue;
                }
                for i in 0..=0xFFu16 {
                    bus.write((page as u16) << 8 | i, v);
                }
            }
            rts(cpu, bus, 0);
            cycles = 2000;
        }
        at::SET_SCROLL | at::SET_SCROLL_NO_STATUS => {
            if pc == at::SET_SCROLL {
                bus.read(0x2002);
            }
            let (x, y, ctrl) = (bus.read(0x00FD), bus.read(0x00FC), bus.read(0x00FF));
            bus.write(0x2005, x);
            bus.write(0x2005, y);
            bus.write(0x2000, ctrl);
            rts(cpu, bus, 0);
        }
        at::JUMP_ENGINE => {
            let lo = pull(cpu, bus);
            let hi = pull(cpu, bus);
            settle(cpu);
            let table = u16::from_le_bytes([lo, hi]).wrapping_add(1);
            cpu.pc = read16(bus, table.wrapping_add(2 * (cpu.a as u16 & 0x7F)));
        }
        at::READ_KEYBOARD => {
            set_a(cpu, 0);
            rts(cpu, bus, 0);
        }
        at::VRAM_FILL | at::READ_VRAM_BUFFER | at::UPLOAD_OBJECT => {
            rts(cpu, bus, 0);
            cycles = 500;
        }
        _ => {
            if !st.unknown.contains(&pc) && st.unknown.len() < 16 {
                st.unknown.push(pc);
            }
            rts(cpu, bus, 0);
        }
    }
    cycles
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A disk image: one side, the given files (id, address, kind, data), boot code `boot`.
    pub(crate) fn image(boot: u8, files: &[(u8, u16, u8, Vec<u8>)]) -> Vec<u8> {
        let mut d = vec![0u8; 56];
        d[0] = 1;
        d[1..15].copy_from_slice(MAGIC);
        d[0x10..0x13].copy_from_slice(b"TST");
        d[0x19] = boot;
        d.push(2);
        d.push(files.len() as u8);
        for (i, (id, addr, kind, data)) in files.iter().enumerate() {
            d.push(3);
            d.push(i as u8);
            d.push(*id);
            d.extend_from_slice(b"FILE    ");
            d.extend_from_slice(&addr.to_le_bytes());
            d.extend_from_slice(&(data.len() as u16).to_le_bytes());
            d.push(*kind);
            d.push(4);
            d.extend_from_slice(data);
        }
        d.resize(SIDE_BYTES, 0);
        let mut out = b"FDS\x1A\x01".to_vec();
        out.resize(16, 0);
        out.extend(d);
        out
    }

    #[test]
    fn a_disk_reads() {
        let img = image(
            1,
            &[(0, 0x6000, 0, vec![1, 2, 3]), (5, 0x2800, 2, vec![9; 4])],
        );
        assert!(is_disk(&img));
        assert!(is_disk(&img[16..]));
        let d = parse(&img).unwrap();
        assert_eq!(d.sides.len(), 1);
        let s = &d.sides[0];
        assert_eq!(s.boot_code(), 1);
        assert_eq!(s.files.len(), 2);
        assert_eq!(s.files[0].data, vec![1, 2, 3]);
        assert_eq!(s.files[1].addr, 0x2800);
        assert_eq!(&s.disk_id()[1..4], b"TST");
    }
}
