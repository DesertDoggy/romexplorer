//! The MSX as a [`Machine`] for the music search ([`super::music_search`]): the Z80
//! ([`super::z80`]), the slots (a stand-in for the BIOS in slot 0, the cartridge in slot 1,
//! RAM in slot 3), the cartridge's bank mapper (plain, Konami, Konami with SCC, ASCII 8 KB,
//! ASCII 16 KB), the video chip's vertical blank, the keyboard and joystick. Nothing is drawn.
//!
//! No BIOS file is needed: like the Disk System's ([`super::fds`]), the BIOS is a stand-in.
//! Its interrupt handler is real Z80 code that calls the game's hooks as the BIOS does; the
//! routines games call (slot switching, the PSG, video memory, the keyboard and joystick)
//! are done natively when the CPU reaches their addresses. The PSG and the SCC go to
//! libvgm's chips ([`super::vgm_chips`]).

use std::sync::Arc;

use super::music_search::{Machine, Step, Write};
use super::vgm_chips::{Chips, Kind};
use super::z80::{Bus, Cpu};

/// Z80 cycles a second, per scanline, and scanlines per frame (NTSC).
const CLOCK: u32 = 3_579_545;
const LINE: u32 = 228;
const LINES: u32 = 262;
const FRAME: u32 = LINE * LINES;
pub const RATE: f64 = CLOCK as f64 / FRAME as f64;
const PSG_CLOCK: u32 = CLOCK / 2;

/// Sound ports: the PSG's registers (chip 0); the SCC's, `SCC | part << 8 | register`.
const SCC: u32 = 0x1000;

/// Where the stand-in BIOS keeps things.
mod at {
    pub const HANDLER: u16 = 0x3E00;
    pub const TICK: u16 = 0x3F00;
    pub const SLOT_RETURN: u16 = 0x3F10;
    pub const IDLE: u16 = 0x3F20;
    /// System variables in RAM.
    pub const RG0SAV: u16 = 0xF3DF;
    pub const STATFL: u16 = 0xF3E7;
    pub const NEWKEY: u16 = 0xFBE5;
    pub const JIFFY: u16 = 0xFC9E;
    pub const EXPTBL: u16 = 0xFCC1;
    pub const H_KEYI: u16 = 0xFD9A;
    pub const H_TIMI: u16 = 0xFD9F;
    pub const HOOKS_END: u16 = 0xFFCA;
    pub const STACK: u16 = 0xF380;
}

/// The stand-in BIOS: the bytes games read from it, and the interrupt handler.
fn bios_image() -> Vec<u8> {
    let mut b = vec![0u8; 0x8000];
    // The video chip's ports, as the BIOS publishes them.
    b[0x0006] = 0x98;
    b[0x0007] = 0x98;
    // A Japanese MSX1 at 60 Hz.
    b[0x002B] = 0x00;
    b[0x002C] = 0x00;
    b[0x002D] = 0x00;
    b[0x0038..0x003B].copy_from_slice(&[0xC3, 0x00, 0x3E]);
    // PUSH HL DE BC AF; EXX; EX AF,AF'; PUSH HL DE BC AF IY IX; CALL H.KEYI; CALL TICK
    // (status into A); CALL H.TIMI; POP IX IY AF BC DE HL; EX AF,AF'; EXX; POP AF BC DE HL;
    // EI; RET
    let handler = [
        0xE5, 0xD5, 0xC5, 0xF5, 0xD9, 0x08, 0xE5, 0xD5, 0xC5, 0xF5, 0xFD, 0xE5, 0xDD, 0xE5,
        0xCD, 0x9A, 0xFD, 0xCD, 0x00, 0x3F, 0xCD, 0x9F, 0xFD, 0xDD, 0xE1, 0xFD, 0xE1, 0xF1,
        0xC1, 0xD1, 0xE1, 0x08, 0xD9, 0xF1, 0xC1, 0xD1, 0xE1, 0xFB, 0xC9,
    ];
    let h = at::HANDLER as usize;
    b[h..h + handler.len()].copy_from_slice(&handler);
    // IDLE: EI; HALT; JR IDLE
    let i = at::IDLE as usize;
    b[i..i + 4].copy_from_slice(&[0xFB, 0x76, 0x18, 0xFC]);
    b
}

/// Stand-in addresses that run as Z80 code rather than natively.
fn is_code(pc: u16) -> bool {
    (0x0038..0x003B).contains(&pc)
        || (at::HANDLER..at::HANDLER + 0x40).contains(&pc)
        || (at::IDLE..at::IDLE + 4).contains(&pc)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mapper {
    /// No banking: the ROM at `base`.
    Plain { base: u16 },
    Konami,
    KonamiScc,
    Ascii8,
    Ascii16,
}

/// Which mapper a cartridge has, by the bank register writes (`LD (nnnn),A`) in its code.
fn guess_mapper(rom: &[u8]) -> Mapper {
    if rom.len() <= 0x10000 {
        let base = if rom.len() > 0x8000 && rom.get(0x4000..0x4002) == Some(b"AB") {
            0x0000
        } else {
            0x4000
        };
        return Mapper::Plain { base };
    }
    // Konami, Konami SCC, ASCII 8, ASCII 16.
    let mut votes = [0u32; 4];
    for w in rom.windows(3) {
        if w[0] != 0x32 {
            continue;
        }
        match u16::from_le_bytes([w[1], w[2]]) {
            0x5000 | 0x9000 | 0xB000 => votes[1] += 1,
            0x4000 | 0x8000 | 0xA000 => votes[0] += 1,
            0x6800 | 0x7800 => votes[2] += 1,
            0x6000 => {
                votes[0] += 1;
                votes[2] += 1;
                votes[3] += 1;
            }
            0x7000 => {
                votes[1] += 1;
                votes[2] += 1;
                votes[3] += 1;
            }
            0x77FF => votes[3] += 1,
            _ => {}
        }
    }
    let best = (0..4).max_by_key(|&i| (votes[i], 3 - i)).unwrap_or(2);
    match best {
        0 => Mapper::Konami,
        1 => Mapper::KonamiScc,
        3 => Mapper::Ascii16,
        _ => Mapper::Ascii8,
    }
}

/// Everything around the CPU. Plain data: cloned to run again from any moment.
#[derive(Clone)]
struct Board {
    bios: Arc<Vec<u8>>,
    rom: Arc<Vec<u8>>,
    mapper: Mapper,
    /// The cartridge's 8 KB banks at $4000, $6000, $8000, $A000 (16 KB mappers use 0 and 2).
    banks: [u8; 4],
    ram: Vec<u8>,
    /// The primary slot register (port $A8).
    slots: u8,
    vram: Vec<u8>,
    vdp: [u8; 8],
    vdp_addr: u16,
    vdp_latch: Option<u8>,
    vdp_buffer: u8,
    status: u8,
    /// Z80 cycles into the frame.
    cycle: u32,
    buttons: bool,
    key_row: u8,
    psg_latch: u8,
    psg: [u8; 16],
    scc_wave: [u8; 128],
    scc_freq: [u8; 10],
    scc_vol: [u8; 5],
    scc_key: u8,
    sound: Vec<Write>,
    stack: (u16, u16),
    /// Slot registers to put back when an inter-slot call returns.
    slot_stack: Vec<u8>,
    /// Stand-in addresses reached that it doesn't know.
    unknown: Vec<u16>,
    /// RAM accesses, when traced: (address, written, value).
    trace: Option<Vec<(u16, bool, u8)>>,
}

impl Board {
    fn line(&self) -> u32 {
        self.cycle / LINE
    }

    fn slot_of(&self, addr: u16) -> u8 {
        (self.slots >> ((addr >> 14) * 2)) & 3
    }

    fn scc_on(&self) -> bool {
        self.mapper == Mapper::KonamiScc && self.banks[2] & 0x3F == 0x3F
    }

    fn cart_read(&self, addr: u16) -> u8 {
        match self.mapper {
            Mapper::Plain { base } => {
                if addr < base {
                    return 0xFF;
                }
                let off = (addr - base) as usize;
                if off < self.rom.len() {
                    self.rom[off]
                } else if self.rom.len() <= 0x4000 && self.rom.len().is_power_of_two() {
                    // Small ROMs show again higher up.
                    self.rom[off % self.rom.len()]
                } else {
                    0xFF
                }
            }
            _ => {
                if !(0x4000..0xC000).contains(&addr) {
                    return 0xFF;
                }
                let w = ((addr - 0x4000) >> 13) as usize;
                let (bank, size) = match self.mapper {
                    Mapper::Ascii16 => (self.banks[w & 2] as usize, 0x4000),
                    _ => (self.banks[w] as usize, 0x2000),
                };
                let n = (self.rom.len() / size).max(1);
                let off = if size == 0x4000 {
                    addr as usize & 0x3FFF
                } else {
                    addr as usize & 0x1FFF
                };
                self.rom[(bank % n) * size + off]
            }
        }
    }

    fn cart_write(&mut self, addr: u16, v: u8) {
        match self.mapper {
            Mapper::Plain { .. } => {}
            Mapper::Konami => match addr {
                0x6000..=0x7FFF => self.banks[1] = v,
                0x8000..=0x9FFF => self.banks[2] = v,
                0xA000..=0xBFFF => self.banks[3] = v,
                _ => {}
            },
            Mapper::KonamiScc => {
                if (0x9800..0xA000).contains(&addr) && self.scc_on() {
                    return self.scc_write(addr, v);
                }
                match addr {
                    0x5000..=0x57FF => self.banks[0] = v,
                    0x7000..=0x77FF => self.banks[1] = v,
                    0x9000..=0x97FF => self.banks[2] = v,
                    0xB000..=0xB7FF => self.banks[3] = v,
                    _ => {}
                }
            }
            Mapper::Ascii8 => match addr {
                0x6000..=0x67FF => self.banks[0] = v,
                0x6800..=0x6FFF => self.banks[1] = v,
                0x7000..=0x77FF => self.banks[2] = v,
                0x7800..=0x7FFF => self.banks[3] = v,
                _ => {}
            },
            Mapper::Ascii16 => match addr {
                0x6000..=0x67FF => self.banks[0] = v,
                0x7000..=0x77FF => self.banks[2] = v,
                _ => {}
            },
        }
    }

    fn scc_write(&mut self, addr: u16, v: u8) {
        let off = (addr & 0xFF) as u8;
        let (part, reg) = match off {
            0x00..=0x7F => {
                self.scc_wave[off as usize] = v;
                (0u32, off)
            }
            0x80..=0x9F => match off & 0x8F {
                r @ 0x80..=0x89 => {
                    self.scc_freq[(r - 0x80) as usize] = v;
                    (1, r - 0x80)
                }
                r @ 0x8A..=0x8E => {
                    self.scc_vol[(r - 0x8A) as usize] = v;
                    (2, r - 0x8A)
                }
                _ => {
                    self.scc_key = v;
                    (3, 0)
                }
            },
            0xE0..=0xFF => (5, 0),
            _ => return,
        };
        self.sound(SCC | part << 8 | reg as u32, v);
    }

    fn sound(&mut self, port: u32, v: u8) {
        self.sound.push(Write {
            at: self.cycle,
            port,
            value: v as u16,
        });
    }

    fn traced(&mut self, addr: u16, write: bool, v: u8) {
        if let Some(t) = self.trace.as_mut() {
            t.push((addr, write, v));
        }
    }

    /// A read from slot `slot`, whatever is selected.
    fn read_slot(&mut self, slot: u8, addr: u16) -> u8 {
        match slot & 3 {
            0 => {
                if addr < 0x8000 {
                    self.bios[addr as usize]
                } else {
                    0xFF
                }
            }
            1 => self.cart_read(addr),
            3 => {
                let v = self.ram[addr as usize];
                self.traced(addr, false, v);
                v
            }
            _ => 0xFF,
        }
    }

    fn write_slot(&mut self, slot: u8, addr: u16, v: u8) {
        match slot & 3 {
            1 => self.cart_write(addr, v),
            3 => {
                self.ram[addr as usize] = v;
                self.traced(addr, true, v);
            }
            _ => {}
        }
    }

    fn vdp_write_reg(&mut self, reg: u8, v: u8) {
        self.vdp[(reg & 7) as usize] = v;
    }

    fn psg_read(&self) -> u8 {
        match self.psg_latch {
            // Joystick port 1: nothing pushed, trigger A when Start is held.
            14 => {
                if self.buttons {
                    0x6F
                } else {
                    0x7F
                }
            }
            r => self.psg[(r & 15) as usize],
        }
    }

    /// The keyboard row selected on port $AA (row 8 bit 0 is the space bar).
    fn key_row_bits(&self, row: u8) -> u8 {
        if row == 8 && self.buttons {
            0xFE
        } else {
            0xFF
        }
    }
}

impl Bus for Board {
    fn read(&mut self, addr: u16) -> u8 {
        let slot = self.slot_of(addr);
        self.read_slot(slot, addr)
    }

    fn write(&mut self, addr: u16, v: u8) {
        let slot = self.slot_of(addr);
        self.write_slot(slot, addr, v);
    }

    fn input(&mut self, port: u16) -> u8 {
        match port as u8 {
            0x98 => {
                let v = self.vdp_buffer;
                self.vdp_buffer = self.vram[(self.vdp_addr & 0x3FFF) as usize];
                self.vdp_addr = self.vdp_addr.wrapping_add(1) & 0x3FFF;
                self.vdp_latch = None;
                v
            }
            0x99 => {
                let v = self.status;
                self.status &= 0x1F;
                self.vdp_latch = None;
                v
            }
            0xA2 => self.psg_read(),
            0xA8 => self.slots,
            0xA9 => self.key_row_bits(self.key_row),
            0xAA => self.key_row,
            _ => 0xFF,
        }
    }

    fn output(&mut self, port: u16, v: u8) {
        match port as u8 {
            0x98 => {
                self.vram[(self.vdp_addr & 0x3FFF) as usize] = v;
                self.vdp_buffer = v;
                self.vdp_addr = self.vdp_addr.wrapping_add(1) & 0x3FFF;
                self.vdp_latch = None;
            }
            0x99 => match self.vdp_latch.take() {
                None => self.vdp_latch = Some(v),
                Some(lo) => {
                    if v & 0x80 != 0 {
                        self.vdp_write_reg(v, lo);
                    } else {
                        self.vdp_addr = (lo as u16 | ((v as u16 & 0x3F) << 8)) & 0x3FFF;
                        if v & 0x40 == 0 {
                            self.vdp_buffer = self.vram[self.vdp_addr as usize];
                            self.vdp_addr = (self.vdp_addr + 1) & 0x3FFF;
                        }
                    }
                }
            },
            0xA0 => self.psg_latch = v & 0x0F,
            0xA1 => {
                let r = self.psg_latch;
                self.psg[r as usize] = v;
                if r < 14 {
                    self.sound(r as u32, v);
                }
            }
            0xA8 => self.slots = v,
            0xAA => self.key_row = v & 0x0F,
            _ => {}
        }
    }
}

/// A moment the CPU entered a routine: enough to call it again from there.
#[derive(Clone)]
pub struct Entry {
    cpu: Cpu,
    slots: u8,
    banks: [u8; 4],
    /// The return address the call pushed.
    ret: [u8; 2],
    /// Cycle into the frame.
    cycle: u32,
}

/// An MSX: CPU and board.
#[derive(Clone)]
pub struct Msx {
    cpu: Cpu,
    board: Board,
}

const SF: u8 = 0x80;
const ZF: u8 = 0x40;
const CF: u8 = 0x01;

impl Msx {
    /// Stand-in addresses the game reached that it doesn't know.
    pub fn unknown_bios_calls(&self) -> Vec<u16> {
        self.board.unknown.clone()
    }

    fn in_bios(&self) -> bool {
        self.cpu.pc < 0x4000 && self.board.slot_of(0) == 0 && !is_code(self.cpu.pc)
    }

    /// One instruction, or one stand-in BIOS routine.
    fn exec(&mut self) -> u32 {
        if self.in_bios() {
            self.bios_call()
        } else {
            self.cpu.step(&mut self.board)
        }
    }

    fn set_a_flags(&mut self, v: u8) {
        self.cpu.a = v;
        self.cpu.f = (self.cpu.f & !(SF | ZF)) | (v & SF) | if v == 0 { ZF } else { 0 };
    }

    /// Selects `slot` (a slot ID: primary in bits 0-1) for the page holding `addr`.
    fn select(&mut self, slot: u8, addr: u16) {
        let shift = (addr >> 14) * 2;
        self.board.slots = (self.board.slots & !(3 << shift)) | ((slot & 3) << shift);
    }

    /// Calls `target` in `slot`, putting the slots back when it returns.
    fn inter_slot_call(&mut self, slot: u8, target: u16) {
        self.board.slot_stack.push(self.board.slots);
        self.select(slot, target);
        self.cpu.pc = at::SLOT_RETURN;
        self.cpu.call(&mut self.board, target);
    }

    fn boot_cart(&mut self) {
        let b = &mut self.board;
        for a in at::H_KEYI..at::HOOKS_END {
            b.ram[a as usize] = 0xC9;
        }
        for i in 0..4 {
            b.ram[at::EXPTBL as usize + i] = 0;
        }
        for i in 0..11 {
            b.ram[at::NEWKEY as usize + i] = 0xFF;
        }
        // The cartridge's header: "AB" and its start address.
        let (cart_page, header) = match b.mapper {
            Mapper::Plain { base: 0 } => (0x4000u16, 0x4000u16),
            _ => (0x4000, 0x4000),
        };
        let slots = 0b11_01_01_00;
        b.slots = slots;
        let init = u16::from_le_bytes([b.cart_read(header + 2), b.cart_read(header + 3)]);
        // Page 2 stays on the cartridge only when it has something there.
        if b.cart_read(0x8000) == 0xFF && b.cart_read(0x8001) == 0xFF && init < 0x8000 {
            b.slots = 0b11_11_01_00;
        }
        let _ = cart_page;
        self.cpu.sp = at::STACK;
        self.cpu.im = 1;
        self.cpu.iff1 = false;
        self.cpu.iff2 = false;
        self.cpu.calls.clear();
        // If the game's start routine returns, it idles with interrupts on.
        self.cpu.pc = at::IDLE;
        self.cpu.push(&mut self.board, at::IDLE);
        self.cpu.pc = if init == 0 { at::IDLE } else { init };
    }

    /// The stand-in BIOS routine at the CPU's address. Returns the cycles it took.
    fn bios_call(&mut self) -> u32 {
        let pc = self.cpu.pc;
        let mut cycles = 40;
        match pc {
            0x0000 => {
                self.boot_cart();
                return 1000;
            }
            at::TICK => {
                // The interrupt's bookkeeping: the video status (clearing it), the clock,
                // the keyboard.
                let status = self.board.input(0x99);
                self.board.ram[at::STATFL as usize] = status;
                let j = at::JIFFY as usize;
                let jiffy = u16::from_le_bytes([self.board.ram[j], self.board.ram[j + 1]])
                    .wrapping_add(1);
                self.board.ram[j..j + 2].copy_from_slice(&jiffy.to_le_bytes());
                self.board.ram[at::NEWKEY as usize + 8] = self.board.key_row_bits(8);
                self.cpu.a = status;
            }
            at::SLOT_RETURN => {
                if let Some(s) = self.board.slot_stack.pop() {
                    self.board.slots = s;
                }
            }
            // RDSLT: A = slot, HL = address; A = the byte.
            0x000C => {
                let v = self.board.read_slot(self.cpu.a, self.cpu.hl());
                self.cpu.a = v;
            }
            // WRSLT: A = slot, HL = address, E = the byte.
            0x0014 => {
                let (slot, hl, e) = (self.cpu.a, self.cpu.hl(), self.cpu.e);
                self.board.write_slot(slot, hl, e);
            }
            // CALSLT: IY high = slot, IX = address.
            0x001C => {
                let (slot, target) = ((self.cpu.iy >> 8) as u8, self.cpu.ix);
                // The CALL that got here returns to the caller once the slot call returns.
                self.inter_slot_call(slot, target);
                return 60;
            }
            // DCOMPR: compares HL with DE.
            0x0020 => {
                let (hl, de) = (self.cpu.hl(), self.cpu.de());
                self.cpu.f = (self.cpu.f & !(ZF | CF))
                    | if hl == de { ZF } else { 0 }
                    | if hl < de { CF } else { 0 };
            }
            // ENASLT: A = slot, H's top bits = page.
            0x0024 => {
                let (slot, h) = (self.cpu.a, (self.cpu.h as u16) << 8);
                self.select(slot, h);
            }
            // CALLF (RST 30h): the slot and address follow the RST.
            0x0030 => {
                let sp = self.cpu.sp;
                let ret = u16::from_le_bytes([self.board.read(sp), self.board.read(sp + 1)]);
                let slot = self.board.read(ret);
                let target =
                    u16::from_le_bytes([self.board.read(ret + 1), self.board.read(ret + 2)]);
                let fixed = ret.wrapping_add(3).to_le_bytes();
                self.board.write(sp, fixed[0]);
                self.board.write(sp + 1, fixed[1]);
                self.inter_slot_call(slot, target);
                return 60;
            }
            // DISSCR / ENASCR.
            0x0041 => self.board.vdp[1] &= !0x40,
            0x0044 => self.board.vdp[1] |= 0x40,
            // WRTVDP: B into register C.
            0x0047 => {
                let (reg, v) = (self.cpu.c & 7, self.cpu.b);
                self.board.vdp_write_reg(reg, v);
                self.board.ram[(at::RG0SAV + reg as u16) as usize] = v;
            }
            // RDVRM / WRTVRM / SETRD / SETWRT.
            0x004A => self.cpu.a = self.board.vram[(self.cpu.hl() & 0x3FFF) as usize],
            0x004D => self.board.vram[(self.cpu.hl() & 0x3FFF) as usize] = self.cpu.a,
            0x0050 => {
                self.board.vdp_addr = self.cpu.hl() & 0x3FFF;
                self.board.vdp_buffer = self.board.vram[self.board.vdp_addr as usize];
                self.board.vdp_addr = (self.board.vdp_addr + 1) & 0x3FFF;
            }
            0x0053 => self.board.vdp_addr = self.cpu.hl() & 0x3FFF,
            // FILVRM: HL address, BC length, A value.
            0x0056 => {
                let (a, n, v) = (self.cpu.hl(), self.cpu.bc(), self.cpu.a);
                for i in 0..n {
                    self.board.vram[(a.wrapping_add(i) & 0x3FFF) as usize] = v;
                }
                cycles += n as u32 * 10;
            }
            // LDIRMV: video HL to RAM DE, BC bytes.
            0x0059 => {
                let (src, dst, n) = (self.cpu.hl(), self.cpu.de(), self.cpu.bc());
                for i in 0..n {
                    let v = self.board.vram[(src.wrapping_add(i) & 0x3FFF) as usize];
                    self.board.write(dst.wrapping_add(i), v);
                }
                cycles += n as u32 * 10;
            }
            // LDIRVM: RAM HL to video DE, BC bytes.
            0x005C => {
                let (src, dst, n) = (self.cpu.hl(), self.cpu.de(), self.cpu.bc());
                for i in 0..n {
                    let v = self.board.read(src.wrapping_add(i));
                    self.board.vram[(dst.wrapping_add(i) & 0x3FFF) as usize] = v;
                }
                cycles += n as u32 * 10;
            }
            // CHGMOD, INITXT, INIT32, INIGRP, INIMLT: a screen mode, interrupts on.
            0x005F | 0x006C | 0x006F | 0x0072 | 0x0075 => {
                self.board.vdp[1] |= 0x60;
                self.board.ram[at::RG0SAV as usize + 1] = self.board.vdp[1];
                cycles = 20_000;
            }
            // SETTXT, SETT32, SETGRP, SETMLT: a mode's tables only.
            0x0078 | 0x007B | 0x007E | 0x0081 => {}
            // CALPAT / CALATR: a sprite's pattern or attribute address in video memory.
            // GSPSIZ: the sprite size (carry for 16x16).
            0x0084 => {
                let big = self.board.vdp[1] & 0x02 != 0;
                let base = (self.board.vdp[6] as u16 & 7) << 11;
                let n = self.cpu.a as u16;
                self.cpu.set_hl(base.wrapping_add(n * if big { 32 } else { 8 }));
            }
            0x0087 => {
                let base = (self.board.vdp[5] as u16 & 0x7F) << 7;
                self.cpu.set_hl(base.wrapping_add(self.cpu.a as u16 * 4));
            }
            0x008A => {
                let big = self.board.vdp[1] & 0x02 != 0;
                self.cpu.a = if big { 32 } else { 8 };
                self.cpu.f = (self.cpu.f & !CF) | if big { CF } else { 0 };
            }
            // FETCHC: the graphics cursor (A mask, HL address).
            0x0114 => {
                self.cpu.a = 0x80;
                self.cpu.set_hl(0);
            }
            // NSETRD / NSTWRT / NRDVRM / NWRVRM (the MSX2's wider video addresses, kept to
            // the 16 KB modelled here) and BIGFIL (FILVRM for them).
            0x016E => {
                self.board.vdp_addr = self.cpu.hl() & 0x3FFF;
                self.board.vdp_buffer = self.board.vram[self.board.vdp_addr as usize];
                self.board.vdp_addr = (self.board.vdp_addr + 1) & 0x3FFF;
            }
            0x0171 => self.board.vdp_addr = self.cpu.hl() & 0x3FFF,
            0x0174 => self.cpu.a = self.board.vram[(self.cpu.hl() & 0x3FFF) as usize],
            0x0177 => self.board.vram[(self.cpu.hl() & 0x3FFF) as usize] = self.cpu.a,
            0x016B => {
                let (a, n, v) = (self.cpu.hl(), self.cpu.bc(), self.cpu.a);
                for i in 0..n {
                    self.board.vram[(a.wrapping_add(i) & 0x3FFF) as usize] = v;
                }
                cycles += n as u32 * 10;
            }
            // PHYDIO (disk access): fails, as with no drive.
            0x0144 => {
                self.cpu.a = 2;
                self.cpu.f |= CF;
            }
            // GICINI: the PSG to silence.
            0x0090 => {
                for (r, v) in [(7u8, 0xB8u8), (8, 0), (9, 0), (10, 0)] {
                    self.board.psg[r as usize] = v;
                    self.board.sound(r as u32, v);
                }
            }
            // WRTPSG: A register, E value. RDPSG: A register, A value.
            0x0093 => {
                let (r, v) = (self.cpu.a & 15, self.cpu.e);
                self.board.psg[r as usize] = v;
                if r < 14 {
                    self.board.sound(r as u32, v);
                }
            }
            0x0096 => {
                self.board.psg_latch = self.cpu.a & 15;
                self.cpu.a = self.board.psg_read();
            }
            // CHSNS: Z when no key waits. CHGET: the space bar.
            0x009C => {
                if self.board.buttons {
                    self.cpu.f &= !ZF;
                } else {
                    self.cpu.f |= ZF;
                }
            }
            0x009F => self.cpu.a = b' ',
            // BREAKX: Ctrl+Stop not held.
            0x00B7 => self.cpu.f &= !CF,
            // GTSTCK: nothing pushed. GTTRIG: the space bar or trigger A when Start is held.
            0x00D5 => self.set_a_flags(0),
            0x00D8 => {
                let v = if self.board.buttons && matches!(self.cpu.a, 0 | 1) {
                    0xFF
                } else {
                    0
                };
                self.set_a_flags(v);
            }
            0x00DB | 0x00DE => self.set_a_flags(0),
            // RSLREG / WSLREG.
            0x0138 => self.cpu.a = self.board.slots,
            0x013B => self.board.slots = self.cpu.a,
            // RDVDP: the status register.
            0x013E => {
                let v = self.board.input(0x99);
                self.cpu.a = v;
            }
            // SNSMAT: A row, A bits.
            0x0141 => {
                let v = self.board.key_row_bits(self.cpu.a);
                self.cpu.a = v;
            }
            // GETCPU: a Z80.
            0x0183 => self.cpu.a = 0,
            // NMI: return.
            0x0066 => {}
            // Text, function keys, beep, BASIC, the sub-ROM: nothing to do for the music.
            0x0008 | 0x0010 | 0x0018 | 0x0028 | 0x003B | 0x003E | 0x0062 | 0x0069
            | 0x0099 | 0x00A2 | 0x00C0 | 0x00C3 | 0x00C6 | 0x00CC | 0x00CF | 0x00D2
            | 0x0156 | 0x0159 | 0x015C | 0x015F | 0x0180
            // GRPPRT, MAPXY, CHGCAP, CHGSND, CHKNEW, EOL: drawing, lamps, the cassette
            // relay.
            | 0x008D | 0x0111 | 0x0132 | 0x0135 | 0x0165 | 0x0168 => {}
            _ => {
                if !self.board.unknown.contains(&pc) && self.board.unknown.len() < 16 {
                    self.board.unknown.push(pc);
                }
            }
        }
        self.cpu.ret(&mut self.board);
        cycles
    }

    fn step_once(&mut self) -> u32 {
        let mut t = 0;
        if self.board.status & 0x80 != 0 && self.board.vdp[1] & 0x20 != 0 {
            t += self.cpu.interrupt(&mut self.board, false);
        }
        if let Some(tr) = self.board.trace.as_mut() {
            tr.clear();
        }
        t += self.exec();
        let before = self.board.line();
        self.board.cycle += t;
        if before < 192 && self.board.line() >= 192 {
            self.board.status |= 0x80;
        }
        t
    }

    /// Runs the routine of `entry` once, from its own registers and slots, until it returns.
    /// False when it doesn't within `budget` cycles.
    fn call(&mut self, entry: &Entry, set: Option<(u8, u8)>, budget: u32) -> bool {
        self.cpu = entry.cpu.clone();
        if let Some((reg, v)) = set {
            match reg {
                0 => self.cpu.a = v,
                1 => self.cpu.b = v,
                2 => self.cpu.c = v,
                3 => self.cpu.d = v,
                4 => self.cpu.e = v,
                5 => self.cpu.h = v,
                _ => self.cpu.l = v,
            }
        }
        self.board.slots = entry.slots;
        self.board.banks = entry.banks;
        self.board.slot_stack.clear();
        let sp = self.cpu.sp;
        self.board.write(sp, entry.ret[0]);
        self.board.write(sp.wrapping_add(1), entry.ret[1]);
        self.cpu.calls = vec![(self.cpu.pc, sp.wrapping_add(2))];
        self.cpu.halted = false;
        let start = self.board.cycle;
        while !self.cpu.calls.is_empty() {
            if self.board.cycle - start > budget || self.cpu.halted {
                return false;
            }
            self.board.cycle += self.exec();
        }
        true
    }
}

/// The PSG and, on a Konami SCC cartridge, the SCC.
pub struct Chip(Chips);

impl Machine for Msx {
    type Entry = Entry;
    type Chip = Chip;
    const REGISTERS: u8 = 7;
    const CHANNELS: u16 = 2;

    fn boot(rom: &[u8]) -> Result<Self, String> {
        if rom.len() < 0x2000 {
            return Err("not an MSX cartridge (too small)".into());
        }
        let mapper = guess_mapper(rom);
        let mut board = Board {
            bios: Arc::new(bios_image()),
            rom: Arc::new(rom.to_vec()),
            mapper,
            banks: [0, 1, 2, 3],
            ram: vec![0; 0x10000],
            slots: 0,
            vram: vec![0; 0x4000],
            vdp: [0; 8],
            vdp_addr: 0,
            vdp_latch: None,
            vdp_buffer: 0,
            status: 0,
            cycle: 0,
            buttons: false,
            key_row: 0,
            psg_latch: 0,
            psg: [0; 16],
            scc_wave: [0; 128],
            scc_freq: [0; 10],
            scc_vol: [0; 5],
            scc_key: 0,
            sound: Vec::new(),
            stack: (at::STACK, at::STACK),
            slot_stack: Vec::new(),
            unknown: Vec::new(),
            trace: None,
        };
        if matches!(mapper, Mapper::Ascii8 | Mapper::Ascii16) {
            board.banks = [0, 0, 0, 0];
        }
        let header = match mapper {
            Mapper::Plain { base } => (0x4000 - base) as usize,
            _ => 0,
        };
        if rom.get(header..header + 2) != Some(b"AB") {
            return Err("not an MSX cartridge (no AB header)".into());
        }
        let mut cpu = Cpu::reset();
        cpu.pc = 0;
        Ok(Self { cpu, board })
    }

    fn run_frame(
        &mut self,
        mut watch: Option<&mut dyn FnMut(&Step)>,
        stop_after: Option<u64>,
    ) -> bool {
        let mut n = 0u64;
        let mut calls: Vec<(u32, u32)> = Vec::new();
        let mut sound: Vec<Write> = Vec::new();
        let mut ram: Vec<(u32, bool, u8)> = Vec::new();
        while self.board.cycle < FRAME {
            let before = self.cpu.calls.len();
            let sound_at = self.board.sound.len();
            self.step_once();
            n += 1;
            if self.cpu.calls.len() > before {
                let sp = self.cpu.sp;
                let (lo, hi) = self.board.stack;
                self.board.stack = (lo.min(sp), hi.max(sp.wrapping_add(1)));
            }
            if let Some(w) = watch.as_deref_mut() {
                calls.clear();
                calls.extend(self.cpu.calls.iter().map(|&(r, s)| (r as u32, s as u32)));
                sound.clear();
                sound.extend_from_slice(&self.board.sound[sound_at..]);
                ram.clear();
                if let Some(t) = self.board.trace.as_ref() {
                    ram.extend(t.iter().map(|&(a, wr, v)| (a as u32, wr, v)));
                }
                let entered = calls.get(before.min(calls.len())..).unwrap_or(&[]);
                w(&Step {
                    calls: &calls,
                    entered,
                    sound: &sound,
                    ram: &ram,
                });
            }
            if stop_after == Some(n) {
                return true;
            }
        }
        self.board.cycle -= FRAME;
        false
    }

    fn take_sound(&mut self) -> Vec<Write> {
        std::mem::take(&mut self.board.sound)
    }

    fn set_trace(&mut self, on: bool) {
        self.board.trace = on.then(Vec::new);
    }

    fn press_start(&mut self, held: bool) {
        self.board.buttons = held;
    }

    fn entry(&self) -> Entry {
        let mut cpu = self.cpu.clone();
        cpu.calls.clear();
        let sp = self.cpu.sp;
        let mut b = self.board.clone();
        Entry {
            cpu,
            slots: self.board.slots,
            banks: self.board.banks,
            ret: [b.read(sp), b.read(sp.wrapping_add(1))],
            cycle: self.board.cycle,
        }
    }

    fn play_frame(&mut self, play: &Entry) -> Option<Vec<Write>> {
        self.board.cycle = play.cycle;
        let ok = self.call(play, None, FRAME * 4);
        self.board.cycle = 0;
        let writes = self.take_sound();
        ok.then_some(writes)
    }

    fn call_with(&mut self, entry: &Entry, reg: u8, v: u8) -> bool {
        let cycle = self.board.cycle;
        let (slots, banks) = (self.board.slots, self.board.banks);
        let cpu = self.cpu.clone();
        let ok = self.call(entry, Some((reg, v)), 800_000);
        self.board.cycle = cycle;
        self.board.slots = slots;
        self.board.banks = banks;
        self.cpu = cpu;
        for w in self.board.sound.iter_mut() {
            w.at = cycle;
        }
        ok
    }

    fn poke(&mut self, addr: u32, v: u8) {
        if addr < 0x10000 {
            self.board.ram[addr as usize] = v;
        }
    }

    fn incidental(w: &Write) -> bool {
        // The mixer: set once at boot by most games.
        w.port == 7
    }

    fn is_stack(&self, addr: u32) -> bool {
        let (lo, hi) = self.board.stack;
        (lo.saturating_sub(0x20) as u32..=hi as u32).contains(&addr)
    }

    fn restore_writes(&self) -> Vec<Write> {
        let w = |port, v: u8| Write {
            at: 0,
            port,
            value: v as u16,
        };
        let b = &self.board;
        let mut out = Vec::new();
        for r in 0..14u32 {
            // Volumes silent, and no envelope restart: the song sets its own.
            let v = match r {
                8..=10 => 0,
                13 => continue,
                _ => b.psg[r as usize],
            };
            out.push(w(r, v));
        }
        if b.mapper == Mapper::KonamiScc {
            for (i, &v) in b.scc_wave.iter().enumerate() {
                out.push(w(SCC | i as u32, v));
            }
            for (i, &v) in b.scc_freq.iter().enumerate() {
                out.push(w(SCC | 1 << 8 | i as u32, v));
            }
            for i in 0..5u32 {
                out.push(w(SCC | 2 << 8 | i, 0));
            }
            out.push(w(SCC | 3 << 8, b.scc_key));
        }
        out
    }

    fn new_chip(&self, rate: u32) -> Result<Chip, String> {
        let mut chips = vec![(Kind::Ay8910, PSG_CLOCK)];
        if self.board.mapper == Mapper::KonamiScc {
            chips.push((Kind::Scc, PSG_CLOCK));
        }
        Ok(Chip(Chips::new(rate, CLOCK as f64, &chips)?))
    }

    fn feed(&self, chip: &mut Chip, writes: &[Write], out: &mut Vec<i16>) {
        let scc = self.board.mapper == Mapper::KonamiScc;
        for w in writes {
            let t = w.at.min(FRAME - 1);
            let reg = (w.port & 0xFF) as u8;
            if w.port & SCC != 0 {
                if scc {
                    let part = ((w.port >> 8) & 0x0F) as u8;
                    chip.0.write(t, 1, part * 2, reg);
                    chip.0.write(t, 1, part * 2 + 1, w.value as u8);
                }
            } else {
                chip.0.write(t, 0, 0, reg);
                chip.0.write(t, 0, 1, w.value as u8);
            }
        }
        chip.0.end_frame(FRAME, out);
    }

    fn frame_length(&self) -> u32 {
        FRAME
    }

    fn frame_rate(&self) -> f64 {
        RATE
    }
}

#[cfg(test)]
mod tests {
    use super::super::music_search::{measure, search, Trigger};
    use super::*;

    /// A tiny 16 KB game at $4000: it hooks H.TIMI to a sound routine that plays song N (N
    /// written to $E000 by the main program, or 1 at boot) on PSG channel A through WRTPSG,
    /// changing note every 8 frames through 16 notes, then round again.
    fn tiny_game() -> Vec<u8> {
        let mut rom = vec![0u8; 0x4000];
        rom[0..4].copy_from_slice(&[b'A', b'B', 0x10, 0x40]);
        // $4010: DI; LD A,1; LD ($E000),A; LD A,$C3; LD ($FD9F),A; LD HL,$4100;
        // LD ($FDA0),HL; CALL $0072 (INIGRP: interrupts on); EI; loop: JR loop
        let main = [
            0xF3, 0x3E, 0x01, 0x32, 0x00, 0xE0, 0x3E, 0xC3, 0x32, 0x9F, 0xFD, 0x21, 0x00, 0x41,
            0x22, 0xA0, 0xFD, 0xCD, 0x72, 0x00, 0xFB, 0x18, 0xFE,
        ];
        rom[0x10..0x10 + main.len()].copy_from_slice(&main);
        // $4100: CALL $4200; RET
        rom[0x100..0x104].copy_from_slice(&[0xCD, 0x00, 0x42, 0xC9]);
        // $4200:
        //   LD A,($E000); OR A; JR Z,tick; LD ($E001),A; XOR A; LD ($E000),A; LD ($E002),A
        // tick: LD A,($E001); OR A; RET Z; LD HL,$E002; INC (HL); LD A,(HL); AND 7; RET NZ
        //   LD A,8; LD E,15; CALL WRTPSG              ; channel A at full volume
        //   LD A,(HL); RRCA; RRCA; RRCA; AND $0F; LD B,A; LD A,($E001); ADD A,B; ADD A,A;
        //   ADD A,A; ADD A,A; LD E,A; XOR A; CALL WRTPSG  ; period low
        //   LD A,1; LD E,1; CALL WRTPSG; RET             ; period high
        let sound = [
            0x3A, 0x00, 0xE0, 0xB7, 0x28, 0x0A, 0x32, 0x01, 0xE0, 0xAF, 0x32, 0x00, 0xE0, 0x32,
            0x02, 0xE0, 0x3A, 0x01, 0xE0, 0xB7, 0xC8, 0x21, 0x02, 0xE0, 0x34, 0x7E, 0xE6, 0x07,
            0xC0, 0x3E, 0x08, 0x1E, 0x0F, 0xCD, 0x93, 0x00, 0x7E, 0x0F, 0x0F, 0x0F, 0xE6, 0x0F,
            0x47, 0x3A, 0x01, 0xE0, 0x80, 0x87, 0x87, 0x87, 0x5F, 0xAF, 0xCD, 0x93, 0x00, 0x3E,
            0x01, 0x1E, 0x01, 0xCD, 0x93, 0x00, 0xC9,
        ];
        rom[0x200..0x200 + sound.len()].copy_from_slice(&sound);
        rom
    }

    #[test]
    fn the_tiny_game_has_its_songs() {
        if !super::super::vgm_chips::available() {
            return;
        }
        let rom = tiny_game();
        let found = search::<Msx>(&rom).unwrap();
        assert_eq!(found.why, None);
        assert_eq!(found.recipe.play, 0x4200);
        assert_eq!(found.recipe.trigger, Trigger::Byte(0xE000));
        assert!(
            (30..=34).contains(&found.songs.len()),
            "{} songs",
            found.songs.len()
        );
        let t = measure::<Msx>(&rom, &found.recipe, 3).unwrap();
        assert_eq!(t.looping.map(|l| l.1), Some(128), "{t:?}");
        let m = Msx::boot(&rom).unwrap();
        assert!(m.unknown_bios_calls().is_empty());
    }

    #[test]
    fn mappers_are_told_apart() {
        let mut rom = vec![0u8; 0x20000];
        for (i, a) in [0x5000u16, 0x7000, 0x9000, 0xB000].iter().enumerate() {
            let at = 0x100 + i * 3;
            rom[at] = 0x32;
            rom[at + 1..at + 3].copy_from_slice(&a.to_le_bytes());
        }
        assert_eq!(guess_mapper(&rom), Mapper::KonamiScc);
        let mut rom = vec![0u8; 0x20000];
        for (i, a) in [0x6000u16, 0x6800, 0x7000, 0x7800].iter().enumerate() {
            let at = 0x100 + i * 3;
            rom[at] = 0x32;
            rom[at + 1..at + 3].copy_from_slice(&a.to_le_bytes());
        }
        assert_eq!(guess_mapper(&rom), Mapper::Ascii8);
        assert_eq!(
            guess_mapper(&vec![0u8; 0x8000]),
            Mapper::Plain { base: 0x4000 }
        );
    }
}
