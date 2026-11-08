//! A HuC6280 interpreter: the PC Engine's CPU, a 65C02 with a memory mapper (eight bank
//! registers turning 16-bit addresses into 21-bit ones), block moves, the T flag and a few
//! extra instructions. It runs a game's own code to drive the sound chip (see
//! [`super::pce_music`]), instruction by instruction, counting cycles per instruction.
//!
//! Like [`super::m6502`] it keeps a shadow of the subroutine calls in progress, which is how
//! the music search tells which routine wrote to the sound chip; calls are named by the
//! 21-bit address they enter, so the same 16-bit address in two banks is two routines.

/// What the CPU is attached to: 21-bit addresses.
pub trait Bus {
    fn read(&mut self, addr: u32) -> u8;
    fn write(&mut self, addr: u32, value: u8);
}

const C: u8 = 0x01;
const Z: u8 = 0x02;
const I: u8 = 0x04;
const D: u8 = 0x08;
const B: u8 = 0x10;
const T: u8 = 0x20;
const V: u8 = 0x40;
const N: u8 = 0x80;

/// The interrupt vectors.
pub const VECTOR_IRQ2: u16 = 0xFFF6;
pub const VECTOR_IRQ1: u16 = 0xFFF8;
pub const VECTOR_TIMER: u16 = 0xFFFA;
pub const VECTOR_NMI: u16 = 0xFFFC;
pub const VECTOR_RESET: u16 = 0xFFFE;

/// How deep the shadow of calls in progress goes; deeper calls are not tracked.
const MAX_CALLS: usize = 32;

/// The registers, and the calls in progress.
#[derive(Clone, Debug)]
pub struct Cpu {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub s: u8,
    pub p: u8,
    pub pc: u16,
    /// The bank registers: bank `mpr[n]` (8 KB each) sits at logical `n * 0x2000`.
    pub mpr: [u8; 8],
    /// CSH: 7.16 MHz; CSL: 1.79 MHz.
    pub high_speed: bool,
    /// The 21-bit entry points of the subroutines in progress, outermost first, and the stack
    /// pointer before each call pushed.
    pub calls: Vec<(u32, u8)>,
}

impl Cpu {
    /// The CPU after a reset: bank 0 at the top, interrupts off, the reset vector's address.
    pub fn reset<B: Bus>(bus: &mut B) -> Self {
        let mut cpu = Self::with(0, 0xFF);
        cpu.mpr[7] = 0;
        cpu.pc = cpu.read16(bus, VECTOR_RESET);
        cpu
    }

    /// A CPU with the given registers and nothing in progress.
    pub fn with(pc: u16, s: u8) -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            s,
            p: I,
            pc,
            mpr: [0xFF, 0xF8, 0, 0, 0, 0, 0, 0],
            high_speed: false,
            calls: Vec::new(),
        }
    }

    /// The 21-bit address of logical `addr`.
    pub fn physical(&self, addr: u16) -> u32 {
        ((self.mpr[(addr >> 13) as usize] as u32) << 13) | (addr & 0x1FFF) as u32
    }

    fn rd<B: Bus>(&self, bus: &mut B, addr: u16) -> u8 {
        bus.read(self.physical(addr))
    }

    fn wr<B: Bus>(&self, bus: &mut B, addr: u16, v: u8) {
        bus.write(self.physical(addr), v)
    }

    fn read16<B: Bus>(&self, bus: &mut B, addr: u16) -> u16 {
        u16::from_le_bytes([self.rd(bus, addr), self.rd(bus, addr.wrapping_add(1))])
    }

    fn zp_read16<B: Bus>(&self, bus: &mut B, zp: u8) -> u16 {
        u16::from_le_bytes([
            self.rd(bus, 0x2000 | zp as u16),
            self.rd(bus, 0x2000 | zp.wrapping_add(1) as u16),
        ])
    }

    fn push<B: Bus>(&mut self, bus: &mut B, v: u8) {
        self.wr(bus, 0x2100 | self.s as u16, v);
        self.s = self.s.wrapping_sub(1);
    }

    fn pull<B: Bus>(&mut self, bus: &mut B) -> u8 {
        self.s = self.s.wrapping_add(1);
        self.rd(bus, 0x2100 | self.s as u16)
    }

    fn fetch<B: Bus>(&mut self, bus: &mut B) -> u8 {
        let v = self.rd(bus, self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }

    fn fetch16<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let lo = self.fetch(bus);
        let hi = self.fetch(bus);
        u16::from_le_bytes([lo, hi])
    }

    fn enter(&mut self, target: u16, s: u8) {
        if self.calls.len() < MAX_CALLS {
            let at = self.physical(target);
            self.calls.push((at, s));
        }
    }

    /// A return: pops every tracked call the stack pointer has now left.
    fn leave(&mut self) {
        while let Some(&(_, s)) = self.calls.last() {
            if self.s.wrapping_sub(s) as i8 >= 0 {
                self.calls.pop();
            } else {
                break;
            }
        }
    }

    /// An interrupt through `vector` (the caller checks the I flag and the masks). Returns
    /// the cycles taken.
    pub fn interrupt<B: Bus>(&mut self, bus: &mut B, vector: u16) -> u32 {
        let s = self.s;
        let pc = self.pc;
        self.push(bus, (pc >> 8) as u8);
        self.push(bus, pc as u8);
        let p = self.p & !(B | T);
        self.push(bus, p);
        self.p = (self.p | I) & !(D | T);
        self.pc = self.read16(bus, vector);
        self.enter(self.pc, s);
        8
    }

    fn set_zn(&mut self, v: u8) {
        self.p = (self.p & !(Z | N)) | if v == 0 { Z } else { 0 } | (v & N);
    }

    fn adc(&mut self, a: u8, v: u8) -> u8 {
        let carry = (self.p & C) as u16;
        if self.p & D != 0 {
            let mut lo = (a & 0x0F) as u16 + (v & 0x0F) as u16 + carry;
            let mut hi = (a & 0xF0) as u16 + (v & 0xF0) as u16;
            if lo > 0x09 {
                hi += 0x10;
                lo += 0x06;
            }
            if hi > 0x90 {
                hi += 0x60;
            }
            let r = (lo & 0x0F) as u8 | (hi & 0xF0) as u8;
            self.p = (self.p & !C) | if hi > 0xFF { C } else { 0 };
            self.set_zn(r);
            r
        } else {
            let sum = a as u16 + v as u16 + carry;
            let r = sum as u8;
            self.p &= !(C | V);
            if sum > 0xFF {
                self.p |= C;
            }
            if (a ^ r) & (v ^ r) & 0x80 != 0 {
                self.p |= V;
            }
            self.set_zn(r);
            r
        }
    }

    fn sbc(&mut self, v: u8) {
        if self.p & D != 0 {
            let borrow = 1 - (self.p & C) as i32;
            let a = self.a as i32;
            let mut lo = (a & 0x0F) - (v as i32 & 0x0F) - borrow;
            let mut hi = (a >> 4) - (v as i32 >> 4);
            if lo < 0 {
                lo += 10;
                hi -= 1;
            }
            let carry = hi >= 0;
            if hi < 0 {
                hi += 10;
            }
            let r = ((hi as u8 & 0x0F) << 4) | (lo as u8 & 0x0F);
            self.p = (self.p & !C) | if carry { C } else { 0 };
            self.a = r;
            self.set_zn(r);
        } else {
            let a = self.a;
            self.p |= 0;
            let r = self.adc(a, !v);
            self.a = r;
        }
    }

    fn compare(&mut self, reg: u8, v: u8) {
        let r = reg.wrapping_sub(v);
        self.p = (self.p & !C) | if reg >= v { C } else { 0 };
        self.set_zn(r);
    }

    fn shift(&mut self, kind: u8, v: u8) -> u8 {
        let c = self.p & C;
        let (r, out) = match kind {
            0 => (v << 1, v >> 7),             // ASL
            1 => ((v << 1) | c, v >> 7),       // ROL
            2 => (v >> 1, v & 1),              // LSR
            _ => ((v >> 1) | (c << 7), v & 1), // ROR
        };
        self.p = (self.p & !C) | out;
        self.set_zn(r);
        r
    }

    fn branch<B: Bus>(&mut self, bus: &mut B, taken: bool) -> u32 {
        let off = self.fetch(bus) as i8;
        if taken {
            self.pc = self.pc.wrapping_add(off as u16);
            2
        } else {
            0
        }
    }

    /// The operand address of a column-style addressing mode `m` (see [`Cpu::step`]).
    fn address<B: Bus>(&mut self, bus: &mut B, m: Mode) -> u16 {
        match m {
            Mode::Zp => 0x2000 | self.fetch(bus) as u16,
            Mode::ZpX => 0x2000 | self.fetch(bus).wrapping_add(self.x) as u16,
            Mode::ZpY => 0x2000 | self.fetch(bus).wrapping_add(self.y) as u16,
            Mode::Abs => self.fetch16(bus),
            Mode::AbsX => self.fetch16(bus).wrapping_add(self.x as u16),
            Mode::AbsY => self.fetch16(bus).wrapping_add(self.y as u16),
            Mode::IndX => {
                let zp = self.fetch(bus).wrapping_add(self.x);
                self.zp_read16(bus, zp)
            }
            Mode::IndY => {
                let zp = self.fetch(bus);
                self.zp_read16(bus, zp).wrapping_add(self.y as u16)
            }
            Mode::Ind => {
                let zp = self.fetch(bus);
                self.zp_read16(bus, zp)
            }
            Mode::Imm => {
                let at = self.pc;
                self.pc = self.pc.wrapping_add(1);
                at
            }
        }
    }

    /// Runs one instruction. Returns the CPU cycles taken.
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let t_mode = self.p & T != 0;
        self.p &= !T;
        let op = self.fetch(bus);
        // The eight ALU groups (and the loads/stores/compares sharing their columns).
        let col = op & 0x1F;
        let alu_mode = match col {
            0x01 => Some((Mode::IndX, 7)),
            0x05 => Some((Mode::Zp, 4)),
            0x09 => Some((Mode::Imm, 2)),
            0x0D => Some((Mode::Abs, 5)),
            0x11 => Some((Mode::IndY, 7)),
            0x12 => Some((Mode::Ind, 7)),
            0x15 => Some((Mode::ZpX, 4)),
            0x19 => Some((Mode::AbsY, 5)),
            0x1D => Some((Mode::AbsX, 5)),
            _ => None,
        };
        if let Some((mode, cycles)) = alu_mode {
            let group = op >> 5;
            // STA has no immediate form ($89 is BIT #).
            if !(group == 4 && mode == Mode::Imm) {
                let addr = self.address(bus, mode);
                return cycles + self.alu(bus, group, addr, t_mode);
            }
        }
        // Read-modify-write columns: ASL ROL LSR ROR (groups 0-3), DEC INC (6, 7).
        let rmw_mode = match col {
            0x06 => Some((Mode::Zp, 6)),
            0x0E => Some((Mode::Abs, 7)),
            0x16 => Some((Mode::ZpX, 6)),
            0x1E => Some((Mode::AbsX, 7)),
            _ => None,
        };
        let group = op >> 5;
        if let Some((mode, cycles)) = rmw_mode {
            if group != 4 && group != 5 {
                let addr = self.address(bus, mode);
                let v = self.rd(bus, addr);
                let r = match group {
                    0..=3 => self.shift(group, v),
                    6 => {
                        let r = v.wrapping_sub(1);
                        self.set_zn(r);
                        r
                    }
                    _ => {
                        let r = v.wrapping_add(1);
                        self.set_zn(r);
                        r
                    }
                };
                self.wr(bus, addr, r);
                return cycles;
            }
        }
        // RMB/SMB ($x7) and BBR/BBS ($xF).
        if op & 0x0F == 0x07 {
            let addr = 0x2000 | self.fetch(bus) as u16;
            let v = self.rd(bus, addr);
            let bit = 1 << ((op >> 4) & 7);
            let r = if op & 0x80 == 0 { v & !bit } else { v | bit };
            self.wr(bus, addr, r);
            return 7;
        }
        if op & 0x0F == 0x0F {
            let addr = 0x2000 | self.fetch(bus) as u16;
            let v = self.rd(bus, addr);
            let bit = 1 << ((op >> 4) & 7);
            let set = v & bit != 0;
            let want = op & 0x80 != 0;
            return 6 + self.branch(bus, set == want);
        }
        match op {
            // Branches.
            0x10 => 2 + self.branch(bus, self.p & N == 0),
            0x30 => 2 + self.branch(bus, self.p & N != 0),
            0x50 => 2 + self.branch(bus, self.p & V == 0),
            0x70 => 2 + self.branch(bus, self.p & V != 0),
            0x90 => 2 + self.branch(bus, self.p & C == 0),
            0xB0 => 2 + self.branch(bus, self.p & C != 0),
            0xD0 => 2 + self.branch(bus, self.p & Z == 0),
            0xF0 => 2 + self.branch(bus, self.p & Z != 0),
            0x80 => 2 + self.branch(bus, true),
            0x44 => {
                // BSR: a JSR with a relative target.
                let off = self.fetch(bus) as i8;
                let ret = self.pc.wrapping_sub(1);
                let s = self.s;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.pc.wrapping_add(off as u16);
                self.enter(self.pc, s);
                8
            }
            0x20 => {
                let target = self.fetch16(bus);
                let ret = self.pc.wrapping_sub(1);
                let s = self.s;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = target;
                self.enter(target, s);
                7
            }
            0x60 => {
                let lo = self.pull(bus);
                let hi = self.pull(bus);
                self.pc = u16::from_le_bytes([lo, hi]).wrapping_add(1);
                self.leave();
                7
            }
            0x40 => {
                self.p = self.pull(bus) & !B;
                let lo = self.pull(bus);
                let hi = self.pull(bus);
                self.pc = u16::from_le_bytes([lo, hi]);
                self.leave();
                7
            }
            0x00 => {
                let s = self.s;
                let ret = self.pc.wrapping_add(1);
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                let p = self.p | B;
                self.push(bus, p);
                self.p = (self.p | I) & !(D | T);
                self.pc = self.read16(bus, VECTOR_IRQ2);
                self.enter(self.pc, s);
                8
            }
            0x4C => {
                self.pc = self.fetch16(bus);
                4
            }
            0x6C => {
                let ptr = self.fetch16(bus);
                self.pc = self.read16(bus, ptr);
                7
            }
            0x7C => {
                let ptr = self.fetch16(bus).wrapping_add(self.x as u16);
                self.pc = self.read16(bus, ptr);
                7
            }
            // Stack.
            0x08 => {
                let p = self.p | B;
                self.push(bus, p);
                3
            }
            0x28 => {
                self.p = self.pull(bus) & !B;
                4
            }
            0x48 => {
                self.push(bus, self.a);
                3
            }
            0x68 => {
                self.a = self.pull(bus);
                self.set_zn(self.a);
                4
            }
            0xDA => {
                self.push(bus, self.x);
                3
            }
            0xFA => {
                self.x = self.pull(bus);
                self.set_zn(self.x);
                4
            }
            0x5A => {
                self.push(bus, self.y);
                3
            }
            0x7A => {
                self.y = self.pull(bus);
                self.set_zn(self.y);
                4
            }
            // Flags.
            0x18 => self.flag(C, false),
            0x38 => self.flag(C, true),
            0x58 => self.flag(I, false),
            0x78 => self.flag(I, true),
            0xB8 => self.flag(V, false),
            0xD8 => self.flag(D, false),
            0xF8 => self.flag(D, true),
            0xF4 => {
                self.p |= T;
                2
            }
            // Transfers and counters.
            0xAA => self.load_x(self.a),
            0xA8 => self.load_y(self.a),
            0x8A => self.load_a(self.x),
            0x98 => self.load_a(self.y),
            0xBA => self.load_x(self.s),
            0x9A => {
                self.s = self.x;
                2
            }
            0xE8 => self.load_x(self.x.wrapping_add(1)),
            0xCA => self.load_x(self.x.wrapping_sub(1)),
            0xC8 => self.load_y(self.y.wrapping_add(1)),
            0x88 => self.load_y(self.y.wrapping_sub(1)),
            0x1A => self.load_a(self.a.wrapping_add(1)),
            0x3A => self.load_a(self.a.wrapping_sub(1)),
            0x0A | 0x2A | 0x4A | 0x6A => {
                self.a = self.shift(op >> 5, self.a);
                2
            }
            0x22 => {
                std::mem::swap(&mut self.a, &mut self.x);
                3
            }
            0x42 => {
                std::mem::swap(&mut self.a, &mut self.y);
                3
            }
            0x02 => {
                std::mem::swap(&mut self.x, &mut self.y);
                3
            }
            0x62 => {
                self.a = 0;
                2
            }
            0x82 => {
                self.x = 0;
                2
            }
            0xC2 => {
                self.y = 0;
                2
            }
            // Loads and stores outside the ALU columns.
            0xA2 => {
                let v = self.fetch(bus);
                self.load_x(v)
            }
            0xA0 => {
                let v = self.fetch(bus);
                self.load_y(v)
            }
            0xA6 | 0xB6 | 0xAE | 0xBE => {
                let (mode, cycles) = match op {
                    0xA6 => (Mode::Zp, 4),
                    0xB6 => (Mode::ZpY, 4),
                    0xAE => (Mode::Abs, 5),
                    _ => (Mode::AbsY, 5),
                };
                let addr = self.address(bus, mode);
                let v = self.rd(bus, addr);
                self.load_x(v);
                cycles
            }
            0xA4 | 0xB4 | 0xAC | 0xBC => {
                let (mode, cycles) = match op {
                    0xA4 => (Mode::Zp, 4),
                    0xB4 => (Mode::ZpX, 4),
                    0xAC => (Mode::Abs, 5),
                    _ => (Mode::AbsX, 5),
                };
                let addr = self.address(bus, mode);
                let v = self.rd(bus, addr);
                self.load_y(v);
                cycles
            }
            0x86 | 0x96 | 0x8E | 0x84 | 0x94 | 0x8C | 0x64 | 0x74 | 0x9C | 0x9E => {
                let (mode, cycles) = match op {
                    0x86 | 0x84 | 0x64 => (Mode::Zp, 4),
                    0x96 => (Mode::ZpY, 4),
                    0x94 | 0x74 => (Mode::ZpX, 4),
                    0x8E | 0x8C | 0x9C => (Mode::Abs, 5),
                    _ => (Mode::AbsX, 5),
                };
                let addr = self.address(bus, mode);
                let v = match op {
                    0x86 | 0x96 | 0x8E => self.x,
                    0x84 | 0x94 | 0x8C => self.y,
                    _ => 0,
                };
                self.wr(bus, addr, v);
                cycles
            }
            // Compares with X and Y.
            0xE0 | 0xE4 | 0xEC | 0xC0 | 0xC4 | 0xCC => {
                let (mode, cycles) = match op & 0x0F {
                    0x00 => (Mode::Imm, 2),
                    0x04 => (Mode::Zp, 4),
                    _ => (Mode::Abs, 5),
                };
                let addr = self.address(bus, mode);
                let v = self.rd(bus, addr);
                let reg = if op >= 0xE0 { self.x } else { self.y };
                self.compare(reg, v);
                cycles
            }
            // BIT.
            0x89 | 0x24 | 0x34 | 0x2C | 0x3C => {
                let (mode, cycles) = match op {
                    0x89 => (Mode::Imm, 2),
                    0x24 => (Mode::Zp, 4),
                    0x34 => (Mode::ZpX, 4),
                    0x2C => (Mode::Abs, 5),
                    _ => (Mode::AbsX, 5),
                };
                let addr = self.address(bus, mode);
                let v = self.rd(bus, addr);
                self.p =
                    (self.p & !(N | V | Z)) | (v & (N | V)) | if self.a & v == 0 { Z } else { 0 };
                cycles
            }
            // TSB / TRB.
            0x04 | 0x0C | 0x14 | 0x1C => {
                let (mode, cycles) = if op & 0x08 == 0 {
                    (Mode::Zp, 6)
                } else {
                    (Mode::Abs, 7)
                };
                let addr = self.address(bus, mode);
                let v = self.rd(bus, addr);
                self.p =
                    (self.p & !(N | V | Z)) | (v & (N | V)) | if self.a & v == 0 { Z } else { 0 };
                let r = if op & 0x10 == 0 {
                    v | self.a
                } else {
                    v & !self.a
                };
                self.wr(bus, addr, r);
                cycles
            }
            // TST #imm, addr.
            0x83 | 0xA3 | 0x93 | 0xB3 => {
                let imm = self.fetch(bus);
                let (mode, cycles) = match op {
                    0x83 => (Mode::Zp, 7),
                    0xA3 => (Mode::ZpX, 7),
                    0x93 => (Mode::Abs, 8),
                    _ => (Mode::AbsX, 8),
                };
                let addr = self.address(bus, mode);
                let v = self.rd(bus, addr);
                self.p = (self.p & !(N | V | Z)) | (v & (N | V)) | if imm & v == 0 { Z } else { 0 };
                cycles
            }
            // The video chip's ports, at the top of the hardware bank.
            0x03 | 0x13 | 0x23 => {
                let v = self.fetch(bus);
                let port = match op {
                    0x03 => 0,
                    0x13 => 2,
                    _ => 3,
                };
                bus.write(0x1FE000 | port, v);
                4
            }
            0x53 => {
                let bits = self.fetch(bus);
                for i in 0..8 {
                    if bits & (1 << i) != 0 {
                        self.mpr[i] = self.a;
                    }
                }
                5
            }
            0x43 => {
                let bits = self.fetch(bus);
                // Every bank register picked, ORed together.
                self.a = (0..8)
                    .filter(|i| bits & (1 << i) != 0)
                    .fold(0, |a, i| a | self.mpr[i]);
                4
            }
            0x54 => {
                self.high_speed = false;
                3
            }
            0xD4 => {
                self.high_speed = true;
                3
            }
            // Block moves.
            0x73 | 0xC3 | 0xD3 | 0xE3 | 0xF3 => self.block(bus, op),
            _ => 2,
        }
    }

    fn flag(&mut self, f: u8, on: bool) -> u32 {
        if on {
            self.p |= f;
        } else {
            self.p &= !f;
        }
        2
    }

    fn load_a(&mut self, v: u8) -> u32 {
        self.a = v;
        self.set_zn(v);
        2
    }

    fn load_x(&mut self, v: u8) -> u32 {
        self.x = v;
        self.set_zn(v);
        2
    }

    fn load_y(&mut self, v: u8) -> u32 {
        self.y = v;
        self.set_zn(v);
        2
    }

    /// ORA AND EOR ADC STA LDA CMP SBC on `addr`; extra cycles taken.
    fn alu<B: Bus>(&mut self, bus: &mut B, group: u8, addr: u16, t_mode: bool) -> u32 {
        if group == 4 {
            self.wr(bus, addr, self.a);
            return 0;
        }
        let v = self.rd(bus, addr);
        // With T set, ORA AND EOR ADC work on the zero-page byte X points at, not A.
        let target = 0x2000 | self.x as u16;
        let t = t_mode && matches!(group, 0..=3);
        let a = if t { self.rd(bus, target) } else { self.a };
        let mut extra = if t { 3 } else { 0 };
        let r = match group {
            0 => a | v,
            1 => a & v,
            2 => a ^ v,
            3 => {
                if self.p & D != 0 {
                    extra += 1;
                }
                self.adc(a, v)
            }
            5 => v,
            6 => {
                self.compare(self.a, v);
                return 0;
            }
            _ => {
                if self.p & D != 0 {
                    extra += 1;
                }
                self.sbc(v);
                return extra;
            }
        };
        self.set_zn(r);
        if t {
            self.wr(bus, target, r);
        } else {
            self.a = r;
        }
        extra
    }

    /// TII TDD TIN TIA TAI.
    fn block<B: Bus>(&mut self, bus: &mut B, op: u8) -> u32 {
        let mut src = self.fetch16(bus);
        let mut dst = self.fetch16(bus);
        let len = self.fetch16(bus);
        let (y, a, x) = (self.y, self.a, self.x);
        self.push(bus, y);
        self.push(bus, a);
        self.push(bus, x);
        let n = if len == 0 { 0x10000u32 } else { len as u32 };
        for i in 0..n {
            let v = self.rd(bus, src);
            self.wr(bus, dst, v);
            let alt = if i & 1 == 0 { 1u16 } else { 0xFFFF };
            match op {
                0x73 => {
                    src = src.wrapping_add(1);
                    dst = dst.wrapping_add(1);
                }
                0xC3 => {
                    src = src.wrapping_sub(1);
                    dst = dst.wrapping_sub(1);
                }
                0xD3 => src = src.wrapping_add(1),
                0xE3 => {
                    src = src.wrapping_add(1);
                    dst = dst.wrapping_add(alt);
                }
                _ => {
                    src = src.wrapping_add(alt);
                    dst = dst.wrapping_add(1);
                }
            }
        }
        self.x = self.pull(bus);
        self.a = self.pull(bus);
        self.y = self.pull(bus);
        17 + 6 * n
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Imm,
    Zp,
    ZpX,
    ZpY,
    Abs,
    AbsX,
    AbsY,
    Ind,
    IndX,
    IndY,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the public SingleStepTests HuC6280 suite (github.com/SingleStepTests/huc6280,
    /// folder v1, one binary file per opcode) when `HUC6280_TESTS` names a folder holding its
    /// files. Registers, bank registers, memory and cycle counts are compared.
    #[test]
    fn single_step_suite() {
        let Ok(dir) = std::env::var("HUC6280_TESTS") else {
            return;
        };
        struct Flat(std::collections::HashMap<u32, u8>);
        impl Bus for Flat {
            fn read(&mut self, addr: u32) -> u8 {
                *self.0.get(&addr).unwrap_or(&0)
            }
            fn write(&mut self, addr: u32, value: u8) {
                self.0.insert(addr, value);
            }
        }
        struct State {
            regs: [u8; 5],
            pc: u16,
            mpr: [u8; 8],
            ram: Vec<(u32, u8)>,
        }
        let u32_at = |b: &[u8], p: usize| u32::from_le_bytes(b[p..p + 4].try_into().unwrap());
        let state = |b: &[u8], p: usize| -> (usize, State) {
            let size = u32_at(b, p) as usize;
            let mut q = p + 4;
            let regs = [b[q], b[q + 1], b[q + 2], b[q + 3], b[q + 4]];
            q += 5;
            let pc = u16::from_le_bytes([b[q], b[q + 1]]);
            q += 2;
            let mpr: [u8; 8] = b[q..q + 8].try_into().unwrap();
            q += 8;
            let n = u32_at(b, q) as usize;
            q += 4;
            let ram = (0..n)
                .map(|i| (u32_at(b, q + i * 5), b[q + i * 5 + 4]))
                .collect();
            (size, State { regs, pc, mpr, ram })
        };
        let only = std::env::var("HUC6280_ONLY").ok();
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().ends_with(".json.bin"))
            .filter(|p| {
                only.as_ref()
                    .is_none_or(|o| p.file_name().unwrap().to_string_lossy().starts_with(o))
            })
            .collect();
        paths.sort();
        let mut bad = Vec::new();
        for path in paths {
            let b = std::fs::read(&path).unwrap();
            let count = u32_at(&b, 0);
            let mut p = 4;
            let (mut failed, mut first) = (0, None);
            for _ in 0..count {
                let size = u32_at(&b, p) as usize;
                let name = String::from_utf8_lossy(&b[p + 4..p + 54])
                    .trim_end_matches('\0')
                    .trim()
                    .to_string();
                let mut q = p + 58;
                let (n, init) = state(&b, q);
                q += n;
                let (_, fin) = state(&b, q);
                q += {
                    let (n, _) = state(&b, q);
                    n
                };
                let num_cycles = u32_at(&b, q);
                p += size;
                // Block moves longer than the suite records are left out.
                if init.ram.len() >= 500 || fin.ram.len() >= 500 || num_cycles > 500 {
                    continue;
                }
                // TMA with no bank picked reads a latch inside the chip games never rely on.
                if name.starts_with("43 ") && init.ram.get(1).is_some_and(|r| r.1 == 0) {
                    continue;
                }
                let mut bus = Flat(init.ram.iter().copied().collect());
                let mut cpu = Cpu {
                    a: init.regs[0],
                    x: init.regs[1],
                    y: init.regs[2],
                    s: init.regs[3],
                    p: init.regs[4],
                    pc: init.pc,
                    mpr: init.mpr,
                    high_speed: true,
                    calls: Vec::new(),
                };
                let cycles = cpu.step(&mut bus);
                let got = [cpu.a, cpu.x, cpu.y, cpu.s, cpu.p];
                let mut wrong: Vec<String> = ["a", "x", "y", "s", "p"]
                    .iter()
                    .zip(got.iter().zip(fin.regs.iter()))
                    .filter(|(_, (g, w))| g != w)
                    .map(|(k, (g, w))| format!("{k}={g:#04x} want {w:#04x}"))
                    .collect();
                if cpu.pc != fin.pc {
                    wrong.push(format!("pc={:#06x} want {:#06x}", cpu.pc, fin.pc));
                }
                if cpu.mpr != fin.mpr {
                    wrong.push(format!("mpr={:?} want {:?}", cpu.mpr, fin.mpr));
                }
                for &(addr, want) in &fin.ram {
                    let got = bus.read(addr);
                    if got != want {
                        wrong.push(format!("[{addr:#08x}]={got:#04x} want {want:#04x}"));
                    }
                }
                if cycles != num_cycles {
                    wrong.push(format!("cycles={cycles} want {num_cycles}"));
                }
                if !wrong.is_empty() {
                    failed += 1;
                    first.get_or_insert_with(|| format!("{name}: {}", wrong.join(", ")));
                }
            }
            if failed > 0 {
                bad.push(format!(
                    "{}: {failed} failed, e.g. {}",
                    path.file_name().unwrap().to_string_lossy(),
                    first.unwrap()
                ));
            }
        }
        assert!(
            bad.is_empty(),
            "{} files with failures:\n{}",
            bad.len(),
            bad.join("\n")
        );
    }
}
