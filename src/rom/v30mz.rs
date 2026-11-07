//! A V30MZ interpreter: the WonderSwan's CPU, NEC's 80186-compatible core. It runs a game's
//! own code to drive the sound unit (see [`super::wswan_music`]), instruction by
//! instruction, counting roughly what each takes. It has the 8086's instructions and the
//! 80186's additions (PUSHA/POPA, ENTER/LEAVE, BOUND, immediate pushes, shifts and IMUL,
//! INS/OUTS); NEC's own extensions are not on the V30MZ.
//!
//! Like [`super::m6502`] it keeps a shadow of the subroutine calls in progress, which is how
//! the music search tells which routine wrote to the sound unit; calls are named by the
//! 20-bit address they enter.

/// What the CPU is attached to: 20-bit memory addresses and 16-bit I/O ports.
pub trait Bus {
    fn read(&mut self, addr: u32) -> u8;
    fn write(&mut self, addr: u32, value: u8);
    fn input(&mut self, port: u16) -> u8;
    fn output(&mut self, port: u16, value: u8);
}

/// Register numbers, as instructions encode them.
pub const AX: usize = 0;
pub const CX: usize = 1;
pub const DX: usize = 2;
pub const BX: usize = 3;
pub const SP: usize = 4;
pub const BP: usize = 5;
pub const SI: usize = 6;
pub const DI: usize = 7;
/// Segment register numbers.
pub const ES: usize = 0;
pub const CS: usize = 1;
pub const SS: usize = 2;
pub const DS: usize = 3;

pub const CF: u16 = 0x0001;
pub const PF: u16 = 0x0004;
pub const AF: u16 = 0x0010;
pub const ZF: u16 = 0x0040;
pub const SF: u16 = 0x0080;
pub const TF: u16 = 0x0100;
pub const IF: u16 = 0x0200;
pub const DF: u16 = 0x0400;
pub const OF: u16 = 0x0800;
/// Bits that always read as set.
const FIXED: u16 = 0xF002;
const WRITABLE: u16 = 0x0FD5;

/// How deep the shadow of calls in progress goes; deeper calls are not tracked.
const MAX_CALLS: usize = 32;

/// The 20-bit address of `seg:off`.
pub fn physical(seg: u16, off: u16) -> u32 {
    (((seg as u32) << 4) + off as u32) & 0xFFFFF
}

/// An operand picked by a ModRM byte: a register, or memory at segment:offset.
#[derive(Clone, Copy)]
enum Place {
    Reg(usize),
    Mem(u16, u16),
}

/// The registers, and the calls in progress.
#[derive(Clone, Debug)]
pub struct Cpu {
    pub r: [u16; 8],
    pub seg: [u16; 4],
    pub ip: u16,
    pub flags: u16,
    /// Stopped by HLT until an interrupt.
    pub halted: bool,
    /// The 20-bit entry points of the subroutines in progress, outermost first, and the
    /// stack pointer before each call pushed.
    pub calls: Vec<(u32, u16)>,
    /// The segment prefix of the instruction being run.
    prefix: Option<usize>,
    /// The instruction's first byte, for faults that return to it.
    start: (u16, u16),
}

fn parity(v: u8) -> bool {
    v.count_ones() % 2 == 0
}

impl Cpu {
    /// The CPU after a reset: FFFF:0000, interrupts off.
    pub fn reset() -> Self {
        let mut cpu = Self::with(0xFFFF, 0);
        cpu.flags = FIXED;
        cpu
    }

    /// A CPU at `cs:ip` with zeroed registers and nothing in progress.
    pub fn with(cs: u16, ip: u16) -> Self {
        Self {
            r: [0; 8],
            seg: [0, cs, 0, 0],
            ip,
            flags: FIXED,
            halted: false,
            calls: Vec::new(),
            prefix: None,
            start: (cs, ip),
        }
    }

    /// The 20-bit address of the next instruction.
    pub fn pc(&self) -> u32 {
        physical(self.seg[CS], self.ip)
    }

    fn flag(&self, f: u16) -> bool {
        self.flags & f != 0
    }

    fn set(&mut self, f: u16, on: bool) {
        if on {
            self.flags |= f;
        } else {
            self.flags &= !f;
        }
    }

    fn set_flags(&mut self, v: u16) {
        self.flags = (v & WRITABLE) | FIXED;
    }

    fn szp(&mut self, v: u32, word: bool) {
        let v = if word { v & 0xFFFF } else { v & 0xFF };
        self.set(ZF, v == 0);
        self.set(SF, v & if word { 0x8000 } else { 0x80 } != 0);
        self.set(PF, parity(v as u8));
    }

    // Registers by number: 8-bit ones are AL CL DL BL AH CH DH BH.

    fn reg8(&self, n: usize) -> u8 {
        let w = self.r[n & 3];
        if n < 4 { w as u8 } else { (w >> 8) as u8 }
    }

    fn set_reg8(&mut self, n: usize, v: u8) {
        let w = &mut self.r[n & 3];
        if n < 4 {
            *w = (*w & 0xFF00) | v as u16;
        } else {
            *w = (*w & 0x00FF) | (v as u16) << 8;
        }
    }

    fn get_reg(&self, n: usize, word: bool) -> u32 {
        if word { self.r[n] as u32 } else { self.reg8(n) as u32 }
    }

    fn set_reg(&mut self, n: usize, word: bool, v: u32) {
        if word {
            self.r[n] = v as u16;
        } else {
            self.set_reg8(n, v as u8);
        }
    }

    // Memory.

    fn rd8<B: Bus>(&self, bus: &mut B, seg: u16, off: u16) -> u8 {
        bus.read(physical(seg, off))
    }

    fn rd16<B: Bus>(&self, bus: &mut B, seg: u16, off: u16) -> u16 {
        let lo = bus.read(physical(seg, off));
        let hi = bus.read(physical(seg, off.wrapping_add(1)));
        u16::from_le_bytes([lo, hi])
    }

    fn wr8<B: Bus>(&self, bus: &mut B, seg: u16, off: u16, v: u8) {
        bus.write(physical(seg, off), v)
    }

    fn wr16<B: Bus>(&self, bus: &mut B, seg: u16, off: u16, v: u16) {
        bus.write(physical(seg, off), v as u8);
        bus.write(physical(seg, off.wrapping_add(1)), (v >> 8) as u8);
    }

    fn fetch<B: Bus>(&mut self, bus: &mut B) -> u8 {
        let v = self.rd8(bus, self.seg[CS], self.ip);
        self.ip = self.ip.wrapping_add(1);
        v
    }

    fn fetch16<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let lo = self.fetch(bus);
        let hi = self.fetch(bus);
        u16::from_le_bytes([lo, hi])
    }

    fn push<B: Bus>(&mut self, bus: &mut B, v: u16) {
        self.r[SP] = self.r[SP].wrapping_sub(2);
        self.wr16(bus, self.seg[SS], self.r[SP], v);
    }

    fn pop<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let v = self.rd16(bus, self.seg[SS], self.r[SP]);
        self.r[SP] = self.r[SP].wrapping_add(2);
        v
    }

    /// The segment a memory operand uses: the prefix's, or `default`.
    fn data_seg(&self, default: usize) -> u16 {
        self.seg[self.prefix.unwrap_or(default)]
    }

    /// Decodes a ModRM byte's operand.
    fn place<B: Bus>(&mut self, bus: &mut B, modrm: u8) -> Place {
        let md = modrm >> 6;
        let rm = (modrm & 7) as usize;
        if md == 3 {
            return Place::Reg(rm);
        }
        let r = &self.r;
        let (base, default) = match rm {
            0 => (r[BX].wrapping_add(r[SI]), DS),
            1 => (r[BX].wrapping_add(r[DI]), DS),
            2 => (r[BP].wrapping_add(r[SI]), SS),
            3 => (r[BP].wrapping_add(r[DI]), SS),
            4 => (r[SI], DS),
            5 => (r[DI], DS),
            6 => (r[BP], SS),
            _ => (r[BX], DS),
        };
        let off = match md {
            0 if rm == 6 => return Place::Mem(self.data_seg(DS), self.fetch16(bus)),
            0 => base,
            1 => base.wrapping_add(self.fetch(bus) as i8 as u16),
            _ => base.wrapping_add(self.fetch16(bus)),
        };
        Place::Mem(self.data_seg(default), off)
    }

    fn get<B: Bus>(&self, bus: &mut B, p: Place, word: bool) -> u32 {
        match p {
            Place::Reg(n) => self.get_reg(n, word),
            Place::Mem(s, o) if word => self.rd16(bus, s, o) as u32,
            Place::Mem(s, o) => self.rd8(bus, s, o) as u32,
        }
    }

    fn put<B: Bus>(&mut self, bus: &mut B, p: Place, word: bool, v: u32) {
        match p {
            Place::Reg(n) => self.set_reg(n, word, v),
            Place::Mem(s, o) if word => self.wr16(bus, s, o, v as u16),
            Place::Mem(s, o) => self.wr8(bus, s, o, v as u8),
        }
    }

    // The call shadow.

    fn enter(&mut self, target: u32, sp: u16) {
        if self.calls.len() < MAX_CALLS {
            self.calls.push((target, sp));
        }
    }

    /// A return: pops every tracked call the stack pointer has now left.
    fn leave(&mut self) {
        while let Some(&(_, sp)) = self.calls.last() {
            if self.r[SP].wrapping_sub(sp) as i16 >= 0 {
                self.calls.pop();
            } else {
                break;
            }
        }
    }

    /// Takes interrupt `n`: flags, CS and IP pushed, through the vector table at 0000.
    /// Returns the cycles it took.
    pub fn interrupt<B: Bus>(&mut self, bus: &mut B, n: u8) -> u32 {
        self.halted = false;
        let sp = self.r[SP];
        self.push(bus, self.flags);
        self.set(IF, false);
        self.set(TF, false);
        self.push(bus, self.seg[CS]);
        self.push(bus, self.ip);
        self.ip = self.rd16(bus, 0, n as u16 * 4);
        self.seg[CS] = self.rd16(bus, 0, n as u16 * 4 + 2);
        self.enter(self.pc(), sp);
        10
    }

    /// Whether maskable interrupts are taken.
    pub fn interrupts_on(&self) -> bool {
        self.flag(IF)
    }

    // Arithmetic.

    fn alu(&mut self, op: u8, a: u32, b: u32, word: bool) -> u32 {
        let (mask, sign) = if word { (0xFFFF, 0x8000) } else { (0xFF, 0x80) };
        let res = match op {
            0 | 2 => {
                let c = (op == 2 && self.flag(CF)) as u32;
                let res = a + b + c;
                self.set(CF, res > mask);
                self.set(OF, (a ^ res) & (b ^ res) & sign != 0);
                self.set(AF, (a ^ b ^ res) & 0x10 != 0);
                res & mask
            }
            3 | 5 | 7 => {
                let c = (op == 3 && self.flag(CF)) as u32;
                let res = a.wrapping_sub(b).wrapping_sub(c) & mask;
                self.set(CF, b + c > a);
                self.set(OF, (a ^ b) & (a ^ res) & sign != 0);
                self.set(AF, (a ^ b ^ res) & 0x10 != 0);
                res
            }
            1 | 4 | 6 => {
                let res = match op {
                    1 => a | b,
                    4 => a & b,
                    _ => a ^ b,
                };
                self.set(CF, false);
                self.set(OF, false);
                self.set(AF, false);
                res
            }
            _ => unreachable!(),
        };
        self.szp(res, word);
        res
    }

    fn inc_dec(&mut self, v: u32, word: bool, dec: bool) -> u32 {
        let cf = self.flag(CF);
        let r = if dec { self.alu(5, v, 1, word) } else { self.alu(0, v, 1, word) };
        self.set(CF, cf);
        r
    }

    /// The shifts and rotates, one bit at a time as the chip does them.
    fn shift(&mut self, op: u8, v: u32, count: u8, word: bool) -> u32 {
        if count == 0 {
            return v;
        }
        let (mask, sign) = if word { (0xFFFF, 0x8000) } else { (0xFF, 0x80) };
        let mut v = v;
        for _ in 0..count {
            let msb = v & sign != 0;
            let lsb = v & 1 != 0;
            let cf = self.flag(CF);
            v = match op {
                0 => {
                    self.set(CF, msb);
                    ((v << 1) | msb as u32) & mask
                }
                1 => {
                    self.set(CF, lsb);
                    (v >> 1) | if lsb { sign } else { 0 }
                }
                2 => {
                    self.set(CF, msb);
                    ((v << 1) | cf as u32) & mask
                }
                3 => {
                    self.set(CF, lsb);
                    (v >> 1) | if cf { sign } else { 0 }
                }
                4 | 6 => {
                    self.set(CF, msb);
                    (v << 1) & mask
                }
                5 => {
                    self.set(CF, lsb);
                    v >> 1
                }
                _ => {
                    self.set(CF, lsb);
                    (v >> 1) | (v & sign)
                }
            };
            let nmsb = v & sign != 0;
            let of = match op {
                0 | 2 | 4 | 6 => nmsb != self.flag(CF),
                1 | 3 => nmsb != (v & (sign >> 1) != 0),
                5 => msb,
                _ => false,
            };
            self.set(OF, of);
        }
        if op >= 4 {
            self.szp(v, word);
            self.set(AF, false);
        }
        v
    }

    fn jump_if<B: Bus>(&mut self, bus: &mut B, cond: bool) -> u32 {
        let d = self.fetch(bus) as i8 as u16;
        if cond {
            self.ip = self.ip.wrapping_add(d);
            4
        } else {
            1
        }
    }

    fn condition(&self, n: u8) -> bool {
        let r = match n >> 1 {
            0 => self.flag(OF),
            1 => self.flag(CF),
            2 => self.flag(ZF),
            3 => self.flag(CF) || self.flag(ZF),
            4 => self.flag(SF),
            5 => self.flag(PF),
            6 => self.flag(SF) != self.flag(OF),
            _ => self.flag(ZF) || self.flag(SF) != self.flag(OF),
        };
        r != (n & 1 != 0)
    }

    fn call_near(&mut self, bus: &mut impl Bus, target: u16) {
        let sp = self.r[SP];
        self.push(bus, self.ip);
        self.ip = target;
        self.enter(self.pc(), sp);
    }

    fn call_far(&mut self, bus: &mut impl Bus, cs: u16, ip: u16) {
        let sp = self.r[SP];
        self.push(bus, self.seg[CS]);
        self.push(bus, self.ip);
        self.seg[CS] = cs;
        self.ip = ip;
        self.enter(self.pc(), sp);
    }

    /// A divide fault: interrupt 0, returning to the faulting instruction.
    fn divide_error<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.seg[CS] = self.start.0;
        self.ip = self.start.1;
        self.interrupt(bus, 0) + 16
    }

    /// One string instruction, repeated while CX lasts when `rep` is given (0xF2 or 0xF3).
    fn string<B: Bus>(&mut self, bus: &mut B, op: u8, rep: Option<u8>) -> u32 {
        let word = op & 1 != 0;
        let step = if word { 2u16 } else { 1 };
        let step = |c: &Self| if c.flag(DF) { step.wrapping_neg() } else { step };
        let mut cycles = 0;
        loop {
            if rep.is_some() {
                if self.r[CX] == 0 {
                    break;
                }
            }
            let d = step(self);
            let src = self.data_seg(DS);
            match op {
                0x6C | 0x6D => {
                    let port = self.r[DX];
                    let v = if word {
                        u16::from_le_bytes([bus.input(port), bus.input(port.wrapping_add(1))]) as u32
                    } else {
                        bus.input(port) as u32
                    };
                    self.put(bus, Place::Mem(self.seg[ES], self.r[DI]), word, v);
                    self.r[DI] = self.r[DI].wrapping_add(d);
                    cycles += 6;
                }
                0x6E | 0x6F => {
                    let v = self.get(bus, Place::Mem(src, self.r[SI]), word);
                    bus.output(self.r[DX], v as u8);
                    if word {
                        bus.output(self.r[DX].wrapping_add(1), (v >> 8) as u8);
                    }
                    self.r[SI] = self.r[SI].wrapping_add(d);
                    cycles += 6;
                }
                0xA4 | 0xA5 => {
                    let v = self.get(bus, Place::Mem(src, self.r[SI]), word);
                    self.put(bus, Place::Mem(self.seg[ES], self.r[DI]), word, v);
                    self.r[SI] = self.r[SI].wrapping_add(d);
                    self.r[DI] = self.r[DI].wrapping_add(d);
                    cycles += 5;
                }
                0xA6 | 0xA7 => {
                    let a = self.get(bus, Place::Mem(src, self.r[SI]), word);
                    let b = self.get(bus, Place::Mem(self.seg[ES], self.r[DI]), word);
                    self.alu(7, a, b, word);
                    self.r[SI] = self.r[SI].wrapping_add(d);
                    self.r[DI] = self.r[DI].wrapping_add(d);
                    cycles += 6;
                }
                0xAA | 0xAB => {
                    let v = self.get_reg(AX, word);
                    self.put(bus, Place::Mem(self.seg[ES], self.r[DI]), word, v);
                    self.r[DI] = self.r[DI].wrapping_add(d);
                    cycles += 3;
                }
                0xAC | 0xAD => {
                    let v = self.get(bus, Place::Mem(src, self.r[SI]), word);
                    self.set_reg(AX, word, v);
                    self.r[SI] = self.r[SI].wrapping_add(d);
                    cycles += 3;
                }
                _ => {
                    let a = self.get_reg(AX, word);
                    let b = self.get(bus, Place::Mem(self.seg[ES], self.r[DI]), word);
                    self.alu(7, a, b, word);
                    self.r[DI] = self.r[DI].wrapping_add(d);
                    cycles += 4;
                }
            }
            let Some(rep) = rep else { break };
            self.r[CX] = self.r[CX].wrapping_sub(1);
            if matches!(op, 0xA6 | 0xA7 | 0xAE | 0xAF) && self.flag(ZF) != (rep == 0xF3) {
                break;
            }
        }
        cycles.max(1)
    }

    /// Runs one instruction (or waits a cycle while halted). Returns the cycles taken.
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> u32 {
        if self.halted {
            return 1;
        }
        self.start = (self.seg[CS], self.ip);
        self.prefix = None;
        let mut rep = None;
        let mut extra = 0;
        let op = loop {
            let b = self.fetch(bus);
            match b {
                0x26 => self.prefix = Some(ES),
                0x2E => self.prefix = Some(CS),
                0x36 => self.prefix = Some(SS),
                0x3E => self.prefix = Some(DS),
                0xF2 | 0xF3 => rep = Some(b),
                0xF0 => {}
                _ => break b,
            }
            extra += 1;
        };
        extra + self.exec(bus, op, rep)
    }

    fn exec<B: Bus>(&mut self, bus: &mut B, op: u8, rep: Option<u8>) -> u32 {
        let word = op & 1 != 0;
        match op {
            // ALU r/m and register, both ways, and the accumulator with an immediate.
            0x00..=0x3F if op & 7 < 6 => {
                let alu = op >> 3;
                match op & 7 {
                    0 | 1 => {
                        let m = self.fetch(bus);
                        let p = self.place(bus, m);
                        let a = self.get(bus, p, word);
                        let b = self.get_reg(((m >> 3) & 7) as usize, word);
                        let r = self.alu(alu, a, b, word);
                        if alu != 7 {
                            self.put(bus, p, word, r);
                        }
                        if matches!(p, Place::Mem(..)) { 3 } else { 1 }
                    }
                    2 | 3 => {
                        let word = op & 1 != 0;
                        let m = self.fetch(bus);
                        let p = self.place(bus, m);
                        let reg = ((m >> 3) & 7) as usize;
                        let a = self.get_reg(reg, word);
                        let b = self.get(bus, p, word);
                        let r = self.alu(alu, a, b, word);
                        if alu != 7 {
                            self.set_reg(reg, word, r);
                        }
                        if matches!(p, Place::Mem(..)) { 2 } else { 1 }
                    }
                    4 => {
                        let b = self.fetch(bus) as u32;
                        let a = self.reg8(AX) as u32;
                        let r = self.alu(alu, a, b, false);
                        if alu != 7 {
                            self.set_reg8(AX, r as u8);
                        }
                        1
                    }
                    _ => {
                        let b = self.fetch16(bus) as u32;
                        let a = self.r[AX] as u32;
                        let r = self.alu(alu, a, b, true);
                        if alu != 7 {
                            self.r[AX] = r as u16;
                        }
                        1
                    }
                }
            }
            0x06 | 0x0E | 0x16 | 0x1E => {
                self.push(bus, self.seg[(op >> 3) as usize]);
                2
            }
            0x07 | 0x17 | 0x1F => {
                let v = self.pop(bus);
                self.seg[(op >> 3) as usize] = v;
                3
            }
            0x27 | 0x2F => {
                let al = self.reg8(AX);
                let (mut v, cf, af) = (al, self.flag(CF), self.flag(AF));
                let sub = op == 0x2F;
                let mut ncf = false;
                if al & 0x0F > 9 || af {
                    v = if sub { v.wrapping_sub(6) } else { v.wrapping_add(6) };
                    self.set(AF, true);
                } else {
                    self.set(AF, false);
                }
                if al > 0x99 || cf {
                    v = if sub { v.wrapping_sub(0x60) } else { v.wrapping_add(0x60) };
                    ncf = true;
                }
                self.set(CF, ncf);
                self.set_reg8(AX, v);
                self.szp(v as u32, false);
                10
            }
            0x37 | 0x3F => {
                let al = self.reg8(AX);
                if al & 0x0F > 9 || self.flag(AF) {
                    let ah = self.reg8(4);
                    if op == 0x37 {
                        self.set_reg8(AX, al.wrapping_add(6));
                        self.set_reg8(4, ah.wrapping_add(1));
                    } else {
                        self.set_reg8(AX, al.wrapping_sub(6));
                        self.set_reg8(4, ah.wrapping_sub(1));
                    }
                    self.set(AF, true);
                    self.set(CF, true);
                } else {
                    self.set(AF, false);
                    self.set(CF, false);
                }
                let al = self.reg8(AX) & 0x0F;
                self.set_reg8(AX, al);
                9
            }
            0x40..=0x4F => {
                let n = (op & 7) as usize;
                let v = self.r[n] as u32;
                self.r[n] = self.inc_dec(v, true, op >= 0x48) as u16;
                1
            }
            0x50..=0x57 => {
                self.r[SP] = self.r[SP].wrapping_sub(2);
                let v = self.r[(op & 7) as usize];
                self.wr16(bus, self.seg[SS], self.r[SP], v);
                1
            }
            0x58..=0x5F => {
                let v = self.pop(bus);
                self.r[(op & 7) as usize] = v;
                1
            }
            0x60 => {
                let sp = self.r[SP];
                for n in 0..8 {
                    let v = if n == SP { sp } else { self.r[n] };
                    self.push(bus, v);
                }
                9
            }
            0x61 => {
                for n in (0..8).rev() {
                    let v = self.pop(bus);
                    if n != SP {
                        self.r[n] = v;
                    }
                }
                8
            }
            0x62 => {
                let m = self.fetch(bus);
                let p = self.place(bus, m);
                if let Place::Mem(s, o) = p {
                    let v = self.r[((m >> 3) & 7) as usize] as i16;
                    let lo = self.rd16(bus, s, o) as i16;
                    let hi = self.rd16(bus, s, o.wrapping_add(2)) as i16;
                    if v < lo || v > hi {
                        self.seg[CS] = self.start.0;
                        self.ip = self.start.1;
                        return self.interrupt(bus, 5) + 13;
                    }
                }
                13
            }
            0x68 => {
                let v = self.fetch16(bus);
                self.push(bus, v);
                1
            }
            0x6A => {
                let v = self.fetch(bus) as i8 as u16;
                self.push(bus, v);
                1
            }
            0x69 | 0x6B => {
                let m = self.fetch(bus);
                let p = self.place(bus, m);
                let a = self.get(bus, p, true) as u16 as i16 as i32;
                let b = if op == 0x69 {
                    self.fetch16(bus) as i16 as i32
                } else {
                    self.fetch(bus) as i8 as i32
                };
                let r = a * b;
                self.r[((m >> 3) & 7) as usize] = r as u16;
                let fits = r == r as i16 as i32;
                self.set(CF, !fits);
                self.set(OF, !fits);
                4
            }
            0x6C..=0x6F | 0xA4..=0xA7 | 0xAA..=0xAF => self.string(bus, op, rep),
            0x70..=0x7F => {
                let c = self.condition(op & 0x0F);
                self.jump_if(bus, c)
            }
            0x80..=0x83 => {
                let m = self.fetch(bus);
                let p = self.place(bus, m);
                let a = self.get(bus, p, word);
                let b = match op {
                    0x81 => self.fetch16(bus) as u32,
                    0x83 => self.fetch(bus) as i8 as u16 as u32,
                    _ => self.fetch(bus) as u32,
                };
                let alu = (m >> 3) & 7;
                let r = self.alu(alu, a, b, word);
                if alu != 7 {
                    self.put(bus, p, word, r);
                }
                if matches!(p, Place::Mem(..)) { 3 } else { 1 }
            }
            0x84 | 0x85 => {
                let m = self.fetch(bus);
                let p = self.place(bus, m);
                let a = self.get(bus, p, word);
                let b = self.get_reg(((m >> 3) & 7) as usize, word);
                self.alu(4, a, b, word);
                if matches!(p, Place::Mem(..)) { 2 } else { 1 }
            }
            0x86 | 0x87 => {
                let m = self.fetch(bus);
                let p = self.place(bus, m);
                let reg = ((m >> 3) & 7) as usize;
                let a = self.get(bus, p, word);
                let b = self.get_reg(reg, word);
                self.put(bus, p, word, b);
                self.set_reg(reg, word, a);
                3
            }
            0x88..=0x8B => {
                let m = self.fetch(bus);
                let p = self.place(bus, m);
                let reg = ((m >> 3) & 7) as usize;
                if op & 2 == 0 {
                    let v = self.get_reg(reg, word);
                    self.put(bus, p, word, v);
                } else {
                    let v = self.get(bus, p, word);
                    self.set_reg(reg, word, v);
                }
                1
            }
            0x8C => {
                let m = self.fetch(bus);
                let p = self.place(bus, m);
                let v = self.seg[((m >> 3) & 3) as usize];
                self.put(bus, p, true, v as u32);
                1
            }
            0x8D => {
                let m = self.fetch(bus);
                if let Place::Mem(_, o) = self.place(bus, m) {
                    self.r[((m >> 3) & 7) as usize] = o;
                }
                1
            }
            0x8E => {
                let m = self.fetch(bus);
                let p = self.place(bus, m);
                let v = self.get(bus, p, true) as u16;
                self.seg[((m >> 3) & 3) as usize] = v;
                2
            }
            0x8F => {
                let m = self.fetch(bus);
                let v = self.pop(bus);
                let p = self.place(bus, m);
                self.put(bus, p, true, v as u32);
                1
            }
            0x90 => 1,
            0x91..=0x97 => {
                let n = (op & 7) as usize;
                self.r.swap(AX, n);
                3
            }
            0x98 => {
                self.r[AX] = self.reg8(AX) as i8 as u16;
                1
            }
            0x99 => {
                self.r[DX] = if self.r[AX] & 0x8000 != 0 { 0xFFFF } else { 0 };
                1
            }
            0x9A => {
                let ip = self.fetch16(bus);
                let cs = self.fetch16(bus);
                self.call_far(bus, cs, ip);
                10
            }
            0x9B => 1,
            0x9C => {
                self.push(bus, self.flags);
                2
            }
            0x9D => {
                let v = self.pop(bus);
                self.set_flags(v);
                3
            }
            0x9E => {
                let ah = self.reg8(4) as u16;
                self.flags = (self.flags & 0xFF00) | (ah & 0xD5) | 0x02;
                4
            }
            0x9F => {
                self.set_reg8(4, self.flags as u8);
                2
            }
            0xA0..=0xA3 => {
                let o = self.fetch16(bus);
                let p = Place::Mem(self.data_seg(DS), o);
                if op < 0xA2 {
                    let v = self.get(bus, p, word);
                    self.set_reg(AX, word, v);
                } else {
                    let v = self.get_reg(AX, word);
                    self.put(bus, p, word, v);
                }
                1
            }
            0xA8 => {
                let b = self.fetch(bus) as u32;
                self.alu(4, self.reg8(AX) as u32, b, false);
                1
            }
            0xA9 => {
                let b = self.fetch16(bus) as u32;
                self.alu(4, self.r[AX] as u32, b, true);
                1
            }
            0xB0..=0xB7 => {
                let v = self.fetch(bus);
                self.set_reg8((op & 7) as usize, v);
                1
            }
            0xB8..=0xBF => {
                let v = self.fetch16(bus);
                self.r[(op & 7) as usize] = v;
                1
            }
            0xC0 | 0xC1 | 0xD0..=0xD3 => {
                let word = op & 1 != 0;
                let m = self.fetch(bus);
                let p = self.place(bus, m);
                let v = self.get(bus, p, word);
                let count = match op {
                    0xC0 | 0xC1 => self.fetch(bus),
                    0xD0 | 0xD1 => 1,
                    _ => self.reg8(CX),
                };
                let r = self.shift((m >> 3) & 7, v, count, word);
                self.put(bus, p, word, r);
                if matches!(p, Place::Mem(..)) { 3 } else { 1 }
            }
            0xC2 | 0xC3 => {
                let n = if op == 0xC2 { self.fetch16(bus) } else { 0 };
                self.ip = self.pop(bus);
                self.r[SP] = self.r[SP].wrapping_add(n);
                self.leave();
                6
            }
            0xC4 | 0xC5 => {
                let m = self.fetch(bus);
                if let Place::Mem(s, o) = self.place(bus, m) {
                    let v = self.rd16(bus, s, o);
                    let sg = self.rd16(bus, s, o.wrapping_add(2));
                    self.r[((m >> 3) & 7) as usize] = v;
                    self.seg[if op == 0xC4 { ES } else { DS }] = sg;
                }
                6
            }
            0xC6 | 0xC7 => {
                let m = self.fetch(bus);
                let p = self.place(bus, m);
                let v = if word { self.fetch16(bus) as u32 } else { self.fetch(bus) as u32 };
                self.put(bus, p, word, v);
                1
            }
            0xC8 => {
                let size = self.fetch16(bus);
                let level = self.fetch(bus) & 0x1F;
                self.push(bus, self.r[BP]);
                let frame = self.r[SP];
                for _ in 1..level {
                    self.r[BP] = self.r[BP].wrapping_sub(2);
                    let v = self.rd16(bus, self.seg[SS], self.r[BP]);
                    self.push(bus, v);
                }
                if level > 0 {
                    self.push(bus, frame);
                }
                self.r[BP] = frame;
                self.r[SP] = self.r[SP].wrapping_sub(size);
                8 + 4 * level as u32
            }
            0xC9 => {
                self.r[SP] = self.r[BP];
                self.r[BP] = self.pop(bus);
                2
            }
            0xCA | 0xCB => {
                let n = if op == 0xCA { self.fetch16(bus) } else { 0 };
                self.ip = self.pop(bus);
                self.seg[CS] = self.pop(bus);
                self.r[SP] = self.r[SP].wrapping_add(n);
                self.leave();
                9
            }
            0xCC => self.interrupt(bus, 3),
            0xCD => {
                let n = self.fetch(bus);
                self.interrupt(bus, n)
            }
            0xCE => {
                if self.flag(OF) {
                    self.interrupt(bus, 4)
                } else {
                    1
                }
            }
            0xCF => {
                self.ip = self.pop(bus);
                self.seg[CS] = self.pop(bus);
                let f = self.pop(bus);
                self.set_flags(f);
                self.leave();
                10
            }
            0xD4 => {
                let base = self.fetch(bus);
                if base == 0 {
                    return self.divide_error(bus);
                }
                let al = self.reg8(AX);
                self.set_reg8(4, al / base);
                self.set_reg8(AX, al % base);
                self.szp((al % base) as u32, false);
                17
            }
            0xD5 => {
                let base = self.fetch(bus);
                let v = self.reg8(4).wrapping_mul(base).wrapping_add(self.reg8(AX));
                self.r[AX] = v as u16;
                self.szp(v as u32, false);
                6
            }
            0xD6 => {
                self.set_reg8(AX, if self.flag(CF) { 0xFF } else { 0 });
                3
            }
            0xD7 => {
                let o = self.r[BX].wrapping_add(self.reg8(AX) as u16);
                let v = self.rd8(bus, self.data_seg(DS), o);
                self.set_reg8(AX, v);
                5
            }
            0xD8..=0xDF => {
                let m = self.fetch(bus);
                self.place(bus, m);
                1
            }
            0xE0..=0xE2 => {
                self.r[CX] = self.r[CX].wrapping_sub(1);
                let go = self.r[CX] != 0
                    && match op {
                        0xE0 => !self.flag(ZF),
                        0xE1 => self.flag(ZF),
                        _ => true,
                    };
                self.jump_if(bus, go) + 1
            }
            0xE3 => {
                let go = self.r[CX] == 0;
                self.jump_if(bus, go)
            }
            0xE4 | 0xE5 | 0xEC | 0xED => {
                let port = if op < 0xE8 { self.fetch(bus) as u16 } else { self.r[DX] };
                let lo = bus.input(port);
                if word {
                    let hi = bus.input(port.wrapping_add(1));
                    self.r[AX] = u16::from_le_bytes([lo, hi]);
                } else {
                    self.set_reg8(AX, lo);
                }
                6
            }
            0xE6 | 0xE7 | 0xEE | 0xEF => {
                let port = if op < 0xE8 { self.fetch(bus) as u16 } else { self.r[DX] };
                let v = self.r[AX];
                bus.output(port, v as u8);
                if word {
                    bus.output(port.wrapping_add(1), (v >> 8) as u8);
                }
                6
            }
            0xE8 => {
                let d = self.fetch16(bus);
                let target = self.ip.wrapping_add(d);
                self.call_near(bus, target);
                5
            }
            0xE9 => {
                let d = self.fetch16(bus);
                self.ip = self.ip.wrapping_add(d);
                4
            }
            0xEA => {
                let ip = self.fetch16(bus);
                let cs = self.fetch16(bus);
                self.ip = ip;
                self.seg[CS] = cs;
                7
            }
            0xEB => self.jump_if(bus, true),
            0xF4 => {
                self.halted = true;
                9
            }
            0xF5 => {
                let c = self.flag(CF);
                self.set(CF, !c);
                4
            }
            0xF6 | 0xF7 => self.group3(bus, word),
            0xF8..=0xFD => {
                let (f, on) = match op {
                    0xF8 => (CF, false),
                    0xF9 => (CF, true),
                    0xFA => (IF, false),
                    0xFB => (IF, true),
                    0xFC => (DF, false),
                    _ => (DF, true),
                };
                self.set(f, on);
                4
            }
            0xFE | 0xFF => {
                let m = self.fetch(bus);
                let p = self.place(bus, m);
                match (m >> 3) & 7 {
                    0 | 1 => {
                        let v = self.get(bus, p, word);
                        let r = self.inc_dec(v, word, m & 0x08 != 0);
                        self.put(bus, p, word, r);
                        if matches!(p, Place::Mem(..)) { 3 } else { 1 }
                    }
                    2 => {
                        let t = self.get(bus, p, true) as u16;
                        self.call_near(bus, t);
                        5
                    }
                    3 => {
                        if let Place::Mem(s, o) = p {
                            let ip = self.rd16(bus, s, o);
                            let cs = self.rd16(bus, s, o.wrapping_add(2));
                            self.call_far(bus, cs, ip);
                        }
                        12
                    }
                    4 => {
                        self.ip = self.get(bus, p, true) as u16;
                        4
                    }
                    5 => {
                        if let Place::Mem(s, o) = p {
                            self.ip = self.rd16(bus, s, o);
                            self.seg[CS] = self.rd16(bus, s, o.wrapping_add(2));
                        }
                        9
                    }
                    _ => {
                        // PUSH SP pushes the value after the push, as for opcode 54.
                        self.r[SP] = self.r[SP].wrapping_sub(2);
                        let v = self.get(bus, p, true) as u16;
                        self.wr16(bus, self.seg[SS], self.r[SP], v);
                        2
                    }
                }
            }
            // The remaining codes do nothing on the V30MZ.
            _ => 1,
        }
    }

    fn group3<B: Bus>(&mut self, bus: &mut B, word: bool) -> u32 {
        let m = self.fetch(bus);
        let p = self.place(bus, m);
        let v = self.get(bus, p, word);
        match (m >> 3) & 7 {
            0 | 1 => {
                let b = if word { self.fetch16(bus) as u32 } else { self.fetch(bus) as u32 };
                self.alu(4, v, b, word);
                1
            }
            2 => {
                let mask = if word { 0xFFFF } else { 0xFF };
                self.put(bus, p, word, !v & mask);
                1
            }
            3 => {
                let r = self.alu(5, 0, v, word);
                self.set(CF, v != 0);
                self.put(bus, p, word, r);
                1
            }
            4 => {
                if word {
                    let r = self.r[AX] as u32 * v;
                    self.r[AX] = r as u16;
                    self.r[DX] = (r >> 16) as u16;
                    let hi = self.r[DX] != 0;
                    self.set(CF, hi);
                    self.set(OF, hi);
                    self.szp(r, true);
                } else {
                    let r = self.reg8(AX) as u32 * v;
                    self.r[AX] = r as u16;
                    let hi = r > 0xFF;
                    self.set(CF, hi);
                    self.set(OF, hi);
                    self.szp(r, false);
                }
                3
            }
            5 => {
                if word {
                    let r = self.r[AX] as i16 as i32 * v as u16 as i16 as i32;
                    self.r[AX] = r as u16;
                    self.r[DX] = (r >> 16) as u16;
                    let over = r != r as i16 as i32;
                    self.set(CF, over);
                    self.set(OF, over);
                    self.szp(r as u32, true);
                } else {
                    let r = self.reg8(AX) as i8 as i16 * v as u8 as i8 as i16;
                    self.r[AX] = r as u16;
                    let over = r != r as i8 as i16;
                    self.set(CF, over);
                    self.set(OF, over);
                    self.szp(r as u32, false);
                }
                3
            }
            6 => {
                if v == 0 {
                    return self.divide_error(bus);
                }
                if word {
                    let n = (self.r[DX] as u32) << 16 | self.r[AX] as u32;
                    let q = n / v;
                    if q > 0xFFFF {
                        return self.divide_error(bus);
                    }
                    self.r[AX] = q as u16;
                    self.r[DX] = (n % v) as u16;
                    23
                } else {
                    let n = self.r[AX] as u32;
                    let q = n / v;
                    if q > 0xFF {
                        return self.divide_error(bus);
                    }
                    self.r[AX] = ((n % v) << 8 | q) as u16;
                    15
                }
            }
            _ => {
                if v == 0 {
                    return self.divide_error(bus);
                }
                if word {
                    let n = ((self.r[DX] as u32) << 16 | self.r[AX] as u32) as i32 as i64;
                    let d = v as u16 as i16 as i64;
                    let q = n / d;
                    if q > 0x7FFF || q < -0x8000 {
                        return self.divide_error(bus);
                    }
                    self.r[AX] = q as u16;
                    self.r[DX] = (n % d) as u16;
                    24
                } else {
                    let n = self.r[AX] as i16 as i32;
                    let d = v as u8 as i8 as i32;
                    let q = n / d;
                    if q > 0x7F || q < -0x80 {
                        return self.divide_error(bus);
                    }
                    self.r[AX] = ((n % d) as u8 as u16) << 8 | q as u8 as u16;
                    17
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Checked against the SingleStepTests 8086 suite (github.com/SingleStepTests/8086): set
    //! `V30MZ_TESTS` to its `v1` directory, unzipped (`gunzip *.gz`), (and `V30MZ_ONLY` to one file name to run just
    //! that). Codes the 80186 changed from the 8086, and cases where the 8086's own quirks
    //! show (REP before other instructions, divide faults, undefined flags), are left out.
    use super::*;
    use std::collections::HashMap;

    struct Ram(HashMap<u32, u8>, Vec<(u16, u8)>);

    impl Bus for Ram {
        fn read(&mut self, a: u32) -> u8 {
            *self.0.get(&a).unwrap_or(&0)
        }
        fn write(&mut self, a: u32, v: u8) {
            self.0.insert(a, v);
        }
        fn input(&mut self, _: u16) -> u8 {
            0xFF
        }
        fn output(&mut self, p: u16, v: u8) {
            self.1.push((p, v));
        }
    }

    const NAMES: [&str; 14] = [
        "ax", "cx", "dx", "bx", "sp", "bp", "si", "di", "es", "cs", "ss", "ds", "ip", "flags",
    ];

    fn regs_of(c: &Cpu) -> [u16; 14] {
        let mut v = [0; 14];
        v[..8].copy_from_slice(&c.r);
        v[8..12].copy_from_slice(&c.seg);
        v[12] = c.ip;
        v[13] = c.flags;
        v
    }

    /// Codes left out: changed by the 80186, 8086-only, or undefined.
    fn skipped(file: &str) -> bool {
        let code = &file[..2];
        // DAA and DAS: the 8086 decides on the high digit's adjustment differently.
        matches!(code, "0F" | "C0" | "C1" | "C8" | "C9" | "D6" | "F1" | "27" | "2F")
            || ("60".."70").contains(&code)
            || matches!(
                file,
                "8F.1" | "8F.2" | "8F.3" | "8F.4" | "8F.5" | "8F.6" | "8F.7" | "D0.6" | "D1.6"
                    | "D2.6" | "D3.6" | "FE.2" | "FE.3" | "FE.4" | "FE.5" | "FE.6" | "FE.7"
                    | "FF.7"
            )
    }

    #[test]
    fn single_step_tests() {
        let Ok(dir) = std::env::var("V30MZ_TESTS") else {
            return;
        };
        let only = std::env::var("V30MZ_ONLY").ok();
        let meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(format!("{dir}/metadata.json")).unwrap())
                .unwrap();
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".json") && n != "metadata.json")
            .collect();
        files.sort();
        let mut failures = 0;
        let mut run = 0;
        for f in files {
            let stem = f.trim_end_matches(".json");
            if skipped(stem) || only.as_deref().is_some_and(|o| o != stem) {
                continue;
            }
            let (code, reg) = stem.split_once('.').unwrap_or((stem, ""));
            let m = &meta["opcodes"][code];
            let m = if reg.is_empty() { m } else { &m["reg"][reg] };
            let mask = m["flags-mask"].as_u64().unwrap_or(0xFFFF) as u16;
            let json = std::fs::read(format!("{dir}/{f}")).unwrap();
            let tests: Vec<serde_json::Value> = serde_json::from_slice(&json).unwrap();
            let mut bad = 0;
            for t in &tests {
                let bytes: Vec<u8> = t["bytes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|b| b.as_u64().unwrap() as u8)
                    .collect();
                // The 8086's REP before a non-string instruction, and its LOCK, act oddly.
                let body = bytes
                    .iter()
                    .position(|b| !matches!(b, 0x26 | 0x2E | 0x36 | 0x3E | 0xF0 | 0xF2 | 0xF3))
                    .unwrap_or(0);
                let main = bytes[body];
                let is_string = matches!(main, 0xA4..=0xA7 | 0xAA..=0xAF);
                if bytes[..body].iter().any(|b| matches!(b, 0xF2 | 0xF3 | 0xF0)) && !is_string {
                    continue;
                }
                let init = &t["initial"];
                let mut cpu = Cpu::with(0, 0);
                let mut regs = [0u16; 14];
                for (i, n) in NAMES.iter().enumerate() {
                    regs[i] = init["regs"][n].as_u64().unwrap() as u16;
                }
                // Shift counts the 8086 takes past 31 (the 80186 masks them).
                if matches!(main, 0xD2 | 0xD3) && regs[CX] & 0xFF > 0x1F {
                    continue;
                }
                cpu.r.copy_from_slice(&regs[..8]);
                cpu.seg.copy_from_slice(&regs[8..12]);
                cpu.ip = regs[12];
                cpu.flags = regs[13];
                let mut ram = Ram(HashMap::new(), Vec::new());
                for e in init["ram"].as_array().unwrap() {
                    ram.0
                        .insert(e[0].as_u64().unwrap() as u32, e[1].as_u64().unwrap() as u8);
                }
                // LEA, LES and LDS of a register are undefined.
                let modrm = bytes.get(body + 1).copied().unwrap_or(0);
                if matches!(main, 0x8D | 0xC4 | 0xC5) && modrm >= 0xC0 {
                    continue;
                }
                cpu.step(&mut ram);
                // Divide faults return to a different place on the 8086, which also faults on a
                // quotient of -128.
                let divides = matches!(main, 0xD4 | 0xF6 | 0xF7) && (modrm >> 3) & 7 >= 6;
                let faulted = t["final"]["regs"]["cs"].as_u64() == Some(0);
                if (divides || main == 0xD4) && (!cpu.calls.is_empty() || faulted) {
                    continue;
                }
                let fin = &t["final"];
                let mut want = regs;
                for (i, n) in NAMES.iter().enumerate() {
                    if let Some(v) = fin["regs"][n].as_u64() {
                        want[i] = v as u16;
                    }
                }
                let mut got = regs_of(&cpu);
                got[13] &= mask;
                want[13] &= mask;
                let mut ok = got == want;
                let mut wrong_ram = None;
                for e in fin["ram"].as_array().unwrap() {
                    let a = e[0].as_u64().unwrap() as u32;
                    let v = e[1].as_u64().unwrap() as u8;
                    if ram.read(a) != v {
                        ok = false;
                        wrong_ram = Some((a, ram.read(a), v));
                    }
                }
                if !ok {
                    bad += 1;
                    if bad <= 3 {
                        let diff: Vec<String> = (0..14)
                            .filter(|&i| got[i] != want[i])
                            .map(|i| format!("{} {:04X} want {:04X}", NAMES[i], got[i], want[i]))
                            .collect();
                        eprintln!("{stem} {}: {:?} ram {:X?}", t["name"], diff, wrong_ram);
                    }
                }
                run += 1;
            }
            if bad > 0 {
                eprintln!("{stem}: {bad} of {} failed", tests.len());
                failures += bad;
            }
        }
        assert!(run > 0);
        assert_eq!(failures, 0, "{failures} failures in {run} tests");
    }
}
