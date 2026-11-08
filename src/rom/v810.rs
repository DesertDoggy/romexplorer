//! A V810 interpreter: the Virtual Boy's CPU, NEC's 32-bit RISC core with the Virtual Boy's
//! extra instructions (XB, XH, REV, MPYHW). It runs a game's own code to drive the sound unit
//! (see [`super::vb_music`]), instruction by instruction, counting roughly what each takes.
//! Its behaviour follows Mednafen's V810 core (`external/beetle-vb-libretro`): the flags of
//! every instruction, the bit-string instructions, the floating-point unit and its exceptions,
//! interrupts and RETI. The cache and its control word are not modelled.
//!
//! Like [`super::m6502`] it keeps a shadow of the subroutine calls in progress, which is how
//! the music search tells which routine wrote to the sound unit. JAL is a call, and so is a
//! JMP through a register when the link register already holds the return address (how
//! compilers call through pointers); JMP [r31] returns. Calls are tracked by the stack
//! pointer (r3) they were made at, so a return pops every call made at or below it.

/// What the CPU is attached to: a 32-bit address space on a 16-bit bus.
pub trait Bus {
    fn read8(&mut self, addr: u32) -> u8;
    fn read16(&mut self, addr: u32) -> u16;
    fn write8(&mut self, addr: u32, value: u8);
    fn write16(&mut self, addr: u32, value: u16);
}

/// System registers, by number.
pub const EIPC: usize = 0;
pub const EIPSW: usize = 1;
pub const FEPC: usize = 2;
pub const FEPSW: usize = 3;
pub const ECR: usize = 4;
pub const PSW: usize = 5;
pub const PIR: usize = 6;
pub const TKCW: usize = 7;
pub const CHCW: usize = 24;
pub const ADTRE: usize = 25;

/// PSW bits.
pub const Z: u32 = 0x0001;
pub const S: u32 = 0x0002;
pub const OV: u32 = 0x0004;
pub const CY: u32 = 0x0008;
const FPR: u32 = 0x0010;
const FUD: u32 = 0x0020;
const FOV: u32 = 0x0040;
const FZD: u32 = 0x0080;
const FIV: u32 = 0x0100;
const FRO: u32 = 0x0200;
pub const ID: u32 = 0x1000;
const AE: u32 = 0x2000;
pub const EP: u32 = 0x4000;
pub const NP: u32 = 0x8000;
const IA: u32 = 0xF_0000;

/// Exception handlers and their codes.
const INVALID_OP: (u32, u16) = (0xFFFF_FF90, 0xFF90);
const ZERO_DIV: (u32, u16) = (0xFFFF_FF80, 0xFF80);
const FPU_HANDLER: u32 = 0xFFFF_FF60;

/// How deep the shadow of calls in progress goes; deeper calls are not tracked.
const MAX_CALLS: usize = 32;

fn sext(v: u32, bits: u32) -> u32 {
    (((v << (32 - bits)) as i32) >> (32 - bits)) as u32
}

/// The registers, and the calls in progress.
#[derive(Clone, Debug)]
pub struct Cpu {
    /// r0-r31 (r0 is always 0; r3 is the stack pointer, r31 the link register).
    pub r: [u32; 32],
    pub pc: u32,
    /// The system registers.
    pub sr: [u32; 32],
    /// Stopped by HALT until an interrupt.
    pub halted: bool,
    /// Stopped for good by an exception during a fatal one.
    pub fatal: bool,
    /// The entry points of the subroutines in progress, outermost first, and the stack
    /// pointer each was called at.
    pub calls: Vec<(u32, u32)>,
    /// How many calls were in progress when each exception or interrupt being handled came.
    exceptions: Vec<usize>,
}

impl Cpu {
    /// The CPU after a reset: at FFFFFFF0, in the NMI-pending state the hardware starts in.
    pub fn reset() -> Self {
        let mut sr = [0; 32];
        sr[ECR] = 0x0000_FFF0;
        sr[PSW] = NP;
        sr[PIR] = 0x0000_5346;
        sr[TKCW] = 0x0000_00E0;
        Self {
            r: [0; 32],
            pc: 0xFFFF_FFF0,
            sr,
            halted: false,
            fatal: false,
            calls: Vec::new(),
            exceptions: Vec::new(),
        }
    }

    fn psw(&self, f: u32) -> bool {
        self.sr[PSW] & f != 0
    }

    fn set(&mut self, f: u32, on: bool) {
        if on {
            self.sr[PSW] |= f;
        } else {
            self.sr[PSW] &= !f;
        }
    }

    fn sz(&mut self, v: u32) {
        self.set(Z, v == 0);
        self.set(S, v & 0x8000_0000 != 0);
    }

    fn put(&mut self, n: usize, v: u32) {
        if n != 0 {
            self.r[n] = v;
        }
    }

    /// Condition `n` of the branches and SETF.
    pub fn condition(&self, n: u32) -> bool {
        let s_ov = self.psw(S) != self.psw(OV);
        let base = match n & 7 {
            0 => self.psw(OV),
            1 => self.psw(CY),
            2 => self.psw(Z),
            3 => self.psw(Z) || self.psw(CY),
            4 => self.psw(S),
            5 => true,
            6 => s_ov,
            _ => s_ov || self.psw(Z),
        };
        base != (n & 8 != 0)
    }

    fn read32<B: Bus>(bus: &mut B, a: u32) -> u32 {
        bus.read16(a) as u32 | (bus.read16(a | 2) as u32) << 16
    }

    fn write32<B: Bus>(bus: &mut B, a: u32, v: u32) {
        bus.write16(a, v as u16);
        bus.write16(a | 2, (v >> 16) as u16);
    }

    // The call shadow.

    fn enter(&mut self, target: u32) {
        if self.calls.len() < MAX_CALLS {
            self.calls.push((target, self.r[3]));
        }
    }

    /// A return: pops every tracked call the stack pointer has now left.
    fn leave(&mut self) {
        while let Some(&(_, sp)) = self.calls.last() {
            if self.r[3].wrapping_sub(sp) as i32 >= 0 {
                self.calls.pop();
            } else {
                break;
            }
        }
    }

    /// Takes an exception: to `handler`, returning to `back`. Returns the cycles it took.
    fn exception(&mut self, handler: u32, code: u16, back: u32) -> u32 {
        if self.psw(NP) {
            self.fatal = true;
            self.halted = true;
            return 1;
        }
        let (pc, code) = if self.psw(EP) {
            self.sr[FEPC] = back;
            self.sr[FEPSW] = self.sr[PSW];
            self.sr[ECR] = (self.sr[ECR] & 0xFFFF) | (code as u32) << 16;
            self.sr[PSW] |= NP;
            (0xFFFF_FFD0, code)
        } else {
            self.sr[EIPC] = back;
            self.sr[EIPSW] = self.sr[PSW];
            self.sr[ECR] = (self.sr[ECR] & 0xFFFF_0000) | code as u32;
            self.sr[PSW] |= EP;
            (handler, code)
        };
        let _ = code;
        self.sr[PSW] |= ID;
        self.sr[PSW] &= !AE;
        self.pc = pc;
        self.exceptions.push(self.calls.len());
        self.enter(pc);
        10
    }

    /// Whether an interrupt of `level` (0-15) would be taken now.
    pub fn accepts(&self, level: u8) -> bool {
        !self.fatal
            && self.sr[PSW] & (NP | EP | ID) == 0
            && level as u32 >= (self.sr[PSW] & IA) >> 16
    }

    /// Takes an interrupt of `level` when it is accepted: to FFFFFE00 + level * 16, masking
    /// it and lower levels. Returns the cycles it took, or None.
    pub fn interrupt(&mut self, level: u8) -> Option<u32> {
        if !self.accepts(level) {
            return None;
        }
        let vector = 0xFE00 | (level as u32) << 4;
        self.sr[EIPC] = self.pc;
        self.sr[EIPSW] = self.sr[PSW];
        self.sr[ECR] = vector;
        self.sr[PSW] |= EP | ID;
        self.sr[PSW] &= !(AE | IA);
        self.sr[PSW] |= ((level as u32 + 1).min(15)) << 16;
        self.halted = false;
        self.pc = 0xFFFF_0000 | vector;
        self.exceptions.push(self.calls.len());
        self.enter(self.pc);
        Some(10)
    }

    /// Forgets every call in progress but the innermost, so that routine can be run again
    /// on its own (returning from it ends it, by JMP [lp] or, for a handler, RETI).
    pub fn keep_innermost(&mut self) {
        let n = self.calls.len();
        let by_exception = n > 0 && self.exceptions.last() == Some(&(n - 1));
        if n > 1 {
            self.calls.drain(..n - 1);
        }
        self.exceptions = if by_exception { vec![0] } else { Vec::new() };
    }

    fn set_sreg(&mut self, n: usize, v: u32) {
        match n {
            EIPSW | FEPSW | PSW => self.sr[n] = v & 0xFF3FF,
            EIPC | FEPC | ADTRE => self.sr[n] = v & !1,
            CHCW => self.sr[n] = v & 2,
            _ => {}
        }
    }

    /// Runs one instruction (nothing while halted). Returns the cycles taken.
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> u32 {
        if self.halted {
            return 1;
        }
        let at = self.pc;
        let op = bus.read16(at) as u32;
        // Format III: the conditional branches.
        if op >> 13 == 0b100 {
            return if self.condition((op >> 9) & 0xF) {
                self.pc = at.wrapping_add(sext(op & 0x1FF, 9) & !1);
                3
            } else {
                self.pc = at.wrapping_add(2);
                1
            };
        }
        let code = op >> 10;
        let reg1 = (op & 0x1F) as usize;
        let reg2 = ((op >> 5) & 0x1F) as usize;
        if code >= 0x28 {
            let imm = bus.read16(at.wrapping_add(2)) as u32;
            self.pc = at.wrapping_add(4);
            return self.long(bus, at, code, reg1, reg2, op, imm);
        }
        self.pc = at.wrapping_add(2);
        let (a, b) = (self.r[reg1], self.r[reg2]);
        let imm5 = reg1 as u32;
        match code {
            0x00 => self.put(reg2, a),
            0x01 | 0x11 => {
                let a = if code == 0x01 { a } else { sext(imm5, 5) };
                let t = b.wrapping_add(a);
                self.set(OV, (b ^ !a) & (b ^ t) & 0x8000_0000 != 0);
                self.set(CY, t < b);
                self.put(reg2, t);
                self.sz(t);
            }
            0x02 => {
                let t = self.sub(b, a);
                self.put(reg2, t);
            }
            0x03 | 0x13 => {
                let a = if code == 0x03 { a } else { sext(imm5, 5) };
                self.sub(b, a);
            }
            0x04 | 0x05 | 0x07 | 0x14 | 0x15 | 0x17 => {
                let n = if code < 0x10 { a & 0x1F } else { imm5 };
                let t = match code & 0x0F {
                    0x04 => {
                        self.set(CY, n != 0 && (b >> (32 - n)) & 1 != 0);
                        b << n
                    }
                    0x05 => {
                        self.set(CY, n != 0 && (b >> (n - 1)) & 1 != 0);
                        b >> n
                    }
                    _ => {
                        self.set(CY, n != 0 && (b >> (n - 1)) & 1 != 0);
                        ((b as i32) >> n) as u32
                    }
                };
                self.set(OV, false);
                self.put(reg2, t);
                self.sz(t);
            }
            0x06 => {
                self.pc = a & !1;
                if reg1 == 31 {
                    self.leave();
                } else if self.r[31] == at.wrapping_add(2) {
                    // The link register was set to come back here: a call through a pointer.
                    self.enter(self.pc);
                }
                return 3;
            }
            0x08 | 0x0A => {
                let p = if code == 0x08 {
                    (a as i32 as i64 * b as i32 as i64) as u64
                } else {
                    a as u64 * b as u64
                };
                let lo = p as u32;
                self.put(30, (p >> 32) as u32);
                self.put(reg2, lo);
                self.sz(lo);
                let over = if code == 0x08 { p as i64 != lo as i32 as i64 } else { p >> 32 != 0 };
                self.set(OV, over);
                return 13;
            }
            0x09 | 0x0B => {
                if a == 0 {
                    return self.exception(ZERO_DIV.0, ZERO_DIV.1, at) + 36;
                }
                let (q, rem, over) = if code == 0x0B {
                    (b / a, b % a, false)
                } else if b == 0x8000_0000 && a == 0xFFFF_FFFF {
                    (0x8000_0000, 0, true)
                } else {
                    let (x, y) = (b as i32, a as i32);
                    ((x / y) as u32, (x % y) as u32, false)
                };
                self.put(30, rem);
                self.put(reg2, q);
                self.set(OV, over);
                self.sz(q);
                return 36;
            }
            0x0C..=0x0F => {
                let t = match code {
                    0x0C => a | b,
                    0x0D => a & b,
                    0x0E => a ^ b,
                    _ => !a,
                };
                self.set(OV, false);
                self.put(reg2, t);
                self.sz(t);
            }
            0x10 => self.put(reg2, sext(imm5, 5)),
            0x12 => {
                let v = self.condition(imm5 & 0xF) as u32;
                self.put(reg2, v);
            }
            0x16 => self.set(ID, false),
            0x1E => self.set(ID, true),
            0x18 => {
                let handler = 0xFFFF_FFA0 + (imm5 & 0x10);
                return self.exception(handler, 0xFFA0 + (imm5 & 0x1F) as u16, self.pc) + 5;
            }
            0x19 => {
                if self.psw(NP) {
                    self.pc = self.sr[FEPC] & !1;
                    self.sr[PSW] = self.sr[FEPSW];
                } else {
                    self.pc = self.sr[EIPC] & !1;
                    self.sr[PSW] = self.sr[EIPSW];
                }
                if let Some(depth) = self.exceptions.pop() {
                    self.calls.truncate(depth);
                }
                return 10;
            }
            0x1A => self.halted = true,
            0x1C => self.set_sreg(imm5 as usize, b),
            0x1D => {
                let v = self.sr[imm5 as usize];
                self.put(reg2, v);
            }
            0x1F => return self.bit_string(bus, at, imm5),
            _ => return self.exception(INVALID_OP.0, INVALID_OP.1, at),
        }
        1
    }

    fn sub(&mut self, b: u32, a: u32) -> u32 {
        let t = b.wrapping_sub(a);
        self.sz(t);
        self.set(OV, (b ^ a) & (b ^ t) & 0x8000_0000 != 0);
        self.set(CY, t > b);
        t
    }

    /// The 32-bit instructions (formats IV to VII).
    #[allow(clippy::too_many_arguments)]
    fn long<B: Bus>(
        &mut self,
        bus: &mut B,
        at: u32,
        code: u32,
        reg1: usize,
        reg2: usize,
        op: u32,
        imm: u32,
    ) -> u32 {
        let a = self.r[reg1];
        let disp = sext(imm, 16);
        match code {
            0x28 => self.put(reg2, a.wrapping_add(disp)),
            0x29 => {
                let t = a.wrapping_add(disp);
                self.set(OV, (a ^ !disp) & (a ^ t) & 0x8000_0000 != 0);
                self.set(CY, t < a);
                self.put(reg2, t);
                self.sz(t);
            }
            0x2A | 0x2B => {
                let d = sext((op & 0x3FF) << 16 | imm, 26) & !1;
                if code == 0x2B {
                    self.r[31] = at.wrapping_add(4);
                }
                self.pc = at.wrapping_add(d);
                if code == 0x2B {
                    self.enter(self.pc);
                }
                return 3;
            }
            0x2C..=0x2E => {
                let t = match code {
                    0x2C => a | imm,
                    0x2D => a & imm,
                    _ => a ^ imm,
                };
                self.set(OV, false);
                self.put(reg2, t);
                self.sz(t);
            }
            0x2F => self.put(reg2, a.wrapping_add(imm << 16)),
            // Loads (and IN, which doesn't sign-extend).
            0x30 | 0x31 | 0x33 | 0x38 | 0x39 | 0x3B => {
                let addr = a.wrapping_add(disp);
                let v = match code {
                    0x30 => bus.read8(addr) as i8 as u32,
                    0x38 => bus.read8(addr) as u32,
                    0x31 => bus.read16(addr & !1) as i16 as u32,
                    0x39 => bus.read16(addr & !1) as u32,
                    _ => Self::read32(bus, addr & !3),
                };
                self.put(reg2, v);
                return 3;
            }
            // Stores (and OUT): [reg1 + disp] = reg2.
            0x34 | 0x35 | 0x37 | 0x3C | 0x3D | 0x3F => {
                let addr = a.wrapping_add(disp);
                let v = self.r[reg2];
                match code & 3 {
                    0 => bus.write8(addr, v as u8),
                    1 => bus.write16(addr & !1, v as u16),
                    _ => Self::write32(bus, addr & !3, v),
                }
                return 2;
            }
            0x3A => {
                let addr = a.wrapping_add(disp) & !3;
                let mem = Self::read32(bus, addr);
                let b = self.r[reg2];
                let t = self.sub(b, mem);
                let v = if t == 0 { self.r[30] } else { mem };
                Self::write32(bus, addr, v);
                self.put(reg2, mem);
                return 26;
            }
            0x3E => return self.float(at, (imm >> 10) & 0x3F, reg2, reg1),
            _ => return self.exception(INVALID_OP.0, INVALID_OP.1, at),
        }
        1
    }

    /// The bit-string instructions, run to the end of the string.
    fn bit_string<B: Bus>(&mut self, bus: &mut B, at: u32, sub: u32) -> u32 {
        if sub >= 0x10 || (sub & 8 == 0 && sub >= 4) {
            return self.exception(INVALID_OP.0, INVALID_OP.1, at);
        }
        let mut cycles = 1;
        if sub & 8 != 0 {
            let mut dstoff = self.r[26] & 0x1F;
            let mut srcoff = self.r[27] & 0x1F;
            let mut len = self.r[28];
            let mut dst = self.r[29] & !3;
            let mut src = self.r[30] & !3;
            let mut s_word = None;
            let mut d_word = None;
            while len > 0 {
                let s = *s_word.get_or_insert_with(|| Self::read32(bus, src));
                let d = d_word.get_or_insert_with(|| Self::read32(bus, dst));
                let bit = (s >> srcoff) & 1;
                let m = 1 << dstoff;
                match sub {
                    0x08 => *d |= bit << dstoff,
                    0x09 => *d &= !((bit ^ 1) << dstoff),
                    0x0A => *d ^= bit << dstoff,
                    0x0B => *d = (*d & !m) | bit << dstoff,
                    0x0C => *d |= (bit ^ 1) << dstoff,
                    0x0D => *d &= !(bit << dstoff),
                    0x0E => *d ^= (bit ^ 1) << dstoff,
                    _ => *d = (*d & !m) | (bit ^ 1) << dstoff,
                }
                srcoff = (srcoff + 1) & 0x1F;
                dstoff = (dstoff + 1) & 0x1F;
                len -= 1;
                cycles += 1;
                if srcoff == 0 {
                    src = src.wrapping_add(4);
                    s_word = None;
                }
                if dstoff == 0 {
                    Self::write32(bus, dst, d_word.take().unwrap());
                    dst = dst.wrapping_add(4);
                }
            }
            if let Some(d) = d_word {
                Self::write32(bus, dst, d);
            }
            self.r[26] = dstoff;
            self.r[27] = srcoff;
            self.r[28] = 0;
            self.r[29] = dst;
            self.r[30] = src;
            return cycles;
        }
        // The searches: SCH0BSU, SCH0BSD, SCH1BSU, SCH1BSD.
        let down = sub & 1 != 0;
        let want = (sub >> 1) & 1;
        let mut off = self.r[27] & 0x1F;
        let mut len = self.r[28];
        let mut skipped = self.r[29];
        let mut src = self.r[30] & !3;
        let mut word = None;
        let mut found = false;
        while len > 0 {
            let w = *word.get_or_insert_with(|| Self::read32(bus, src));
            cycles += 1;
            if (w >> off) & 1 == want {
                found = true;
                // Left one bit before the one found.
                if down {
                    if off == 31 {
                        off = 0;
                        src = src.wrapping_add(4);
                    } else {
                        off += 1;
                    }
                } else if off == 0 {
                    off = 31;
                    src = src.wrapping_sub(4);
                } else {
                    off -= 1;
                }
                break;
            }
            skipped = skipped.wrapping_add(1);
            len -= 1;
            if down {
                if off == 0 {
                    off = 31;
                    src = src.wrapping_sub(4);
                    word = None;
                } else {
                    off -= 1;
                }
            } else {
                off = (off + 1) & 0x1F;
                if off == 0 {
                    src = src.wrapping_add(4);
                    word = None;
                }
            }
        }
        self.r[27] = off;
        self.r[28] = len;
        self.r[29] = skipped;
        self.r[30] = src;
        if found {
            self.set(Z, false);
        } else {
            self.set(Z, true);
        }
        cycles
    }

    fn fp_flags(&mut self, v: u32) {
        self.set(OV, false);
        if v & 0x7FFF_FFFF == 0 {
            self.set(Z, true);
            self.set(S, false);
            self.set(CY, false);
        } else {
            self.set(Z, false);
            self.set(S, v & 0x8000_0000 != 0);
            self.set(CY, v & 0x8000_0000 != 0);
        }
    }

    /// A reserved operand (not a number, infinity, or subnormal): an exception.
    fn reserved(&mut self, v: u32, at: u32) -> bool {
        if v & 0x7FFF_FFFF != 0 && matches!((v >> 23) & 0xFF, 0 | 0xFF) {
            self.sr[PSW] |= FRO;
            self.exception(FPU_HANDLER, 0xFF60, at);
            return true;
        }
        false
    }

    /// The floating-point unit and the Virtual Boy's extra instructions: `dst` (reg2) is
    /// the destination and first operand, `src` (reg1) the second.
    fn float(&mut self, at: u32, sub: u32, dst: usize, src: usize) -> u32 {
        let (d, s) = (self.r[dst], self.r[src]);
        match sub {
            0x08 => {
                self.put(dst, (d & 0xFFFF_0000) | (d & 0xFF) << 8 | (d >> 8) & 0xFF);
                return 1;
            }
            0x09 => {
                self.put(dst, d.rotate_left(16));
                return 1;
            }
            0x0A => {
                self.put(dst, s.reverse_bits());
                return 1;
            }
            0x0C => {
                self.put(dst, d.wrapping_mul(sext(s & 0x1_FFFF, 17)));
                return 9;
            }
            0x02 => {
                let v = (s as i32 as f32).to_bits();
                self.put(dst, v);
                self.fp_flags(v);
                return 5;
            }
            0x03 | 0x0B => {
                if self.reserved(s, at) {
                    return 8;
                }
                let f = f32::from_bits(s) as f64;
                let r = if sub == 0x03 { f.round_ties_even() } else { f.trunc() };
                if r < i32::MIN as f64 || r > i32::MAX as f64 {
                    self.sr[PSW] |= FIV;
                    self.exception(FPU_HANDLER, 0xFF70, at);
                    return 8;
                }
                let v = r as i32 as u32;
                self.put(dst, v);
                self.set(OV, false);
                self.sz(v);
                return 8;
            }
            0x00 | 0x04..=0x07 => {}
            _ => return self.exception(INVALID_OP.0, INVALID_OP.1, at),
        }
        if self.reserved(d, at) || self.reserved(s, at) {
            return 6;
        }
        let (x, y) = (f32::from_bits(d), f32::from_bits(s));
        if sub == 0x00 {
            self.set(OV, false);
            if x == y {
                self.set(Z, true);
                self.set(S, false);
                self.set(CY, false);
            } else {
                self.set(Z, false);
                self.set(S, x < y);
                self.set(CY, x < y);
            }
            return 6;
        }
        let r = match sub {
            0x04 => x + y,
            0x05 => x - y,
            0x06 => x * y,
            _ => {
                if y == 0.0 {
                    let (flag, code) = if x == 0.0 { (FIV, 0xFF70) } else { (FZD, 0xFF68) };
                    self.sr[PSW] |= flag;
                    self.exception(FPU_HANDLER, code, at);
                    return 43;
                }
                x / y
            }
        };
        let mut v = r.to_bits();
        if r.is_subnormal() {
            self.sr[PSW] |= FUD | FPR;
            v &= 0x8000_0000;
        }
        self.fp_flags(v);
        self.put(dst, v);
        if r.is_infinite() {
            self.sr[PSW] |= FOV | FPR;
            self.exception(FPU_HANDLER, 0xFF64, at);
        }
        match sub {
            0x04 => 8,
            0x05 => 11,
            0x06 => 7,
            _ => 43,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 64 KB of memory at 0 and a copy at the top (where the vectors are).
    struct Mem(Vec<u8>);

    impl Bus for Mem {
        fn read8(&mut self, a: u32) -> u8 {
            self.0[(a & 0xFFFF) as usize]
        }
        fn read16(&mut self, a: u32) -> u16 {
            let a = (a & 0xFFFE) as usize;
            u16::from_le_bytes([self.0[a], self.0[a + 1]])
        }
        fn write8(&mut self, a: u32, v: u8) {
            self.0[(a & 0xFFFF) as usize] = v;
        }
        fn write16(&mut self, a: u32, v: u16) {
            let a = (a & 0xFFFE) as usize;
            self.0[a..a + 2].copy_from_slice(&v.to_le_bytes());
        }
    }

    // A small assembler for the formats.
    fn f1(code: u32, r1: u32, r2: u32) -> Vec<u16> {
        vec![(code << 10 | r2 << 5 | r1) as u16]
    }
    fn f2(code: u32, imm: i32, r2: u32) -> Vec<u16> {
        vec![(code << 10 | r2 << 5 | (imm as u32 & 0x1F)) as u16]
    }
    fn f5(code: u32, r1: u32, r2: u32, imm: u32) -> Vec<u16> {
        vec![(code << 10 | r2 << 5 | r1) as u16, imm as u16]
    }
    fn br(cond: u32, disp: i32) -> Vec<u16> {
        vec![(0b100 << 13 | cond << 9 | (disp as u32 & 0x1FF)) as u16]
    }
    fn jal(disp: i32) -> Vec<u16> {
        let d = disp as u32 & 0x03FF_FFFF;
        vec![(0x2B << 10 | d >> 16) as u16, d as u16]
    }
    fn fpp(sub: u32, r1: u32, r2: u32) -> Vec<u16> {
        vec![(0x3E << 10 | r2 << 5 | r1) as u16, (sub << 10) as u16]
    }

    /// Runs `code` from 0x1000 for `n` instructions.
    fn run(code: &[Vec<u16>], setup: impl FnOnce(&mut Cpu, &mut Mem), n: usize) -> (Cpu, Mem) {
        let mut m = Mem(vec![0; 0x10000]);
        let mut at = 0x1000;
        for w in code.iter().flatten() {
            m.0[at..at + 2].copy_from_slice(&w.to_le_bytes());
            at += 2;
        }
        let mut c = Cpu::reset();
        c.sr[PSW] = 0;
        c.pc = 0x1000;
        setup(&mut c, &mut m);
        for _ in 0..n {
            c.step(&mut m);
        }
        (c, m)
    }

    #[test]
    fn add_and_subtract_set_the_flags() {
        let (c, _) = run(&[f1(0x01, 1, 2)], |c, _| {
            c.r[1] = 1;
            c.r[2] = 0x7FFF_FFFF;
        }, 1);
        assert_eq!(c.r[2], 0x8000_0000);
        assert_eq!(c.sr[PSW] & (Z | S | OV | CY), S | OV);
        let (c, _) = run(&[f1(0x01, 1, 2)], |c, _| {
            c.r[1] = 1;
            c.r[2] = 0xFFFF_FFFF;
        }, 1);
        assert_eq!(c.sr[PSW] & (Z | S | OV | CY), Z | CY);
        // SUB r1, r2: r2 = r2 - r1, borrow when r1 > r2.
        let (c, _) = run(&[f1(0x02, 1, 2)], |c, _| {
            c.r[1] = 5;
            c.r[2] = 3;
        }, 1);
        assert_eq!(c.r[2], 0xFFFF_FFFE);
        assert_eq!(c.sr[PSW] & (Z | S | OV | CY), S | CY);
        // CMP imm: -16 against 0x7FFFFFFF overflows.
        let (c, _) = run(&[f2(0x13, -16, 2)], |c, _| c.r[2] = 0x7FFF_FFFF, 1);
        assert_eq!(c.r[2], 0x7FFF_FFFF);
        assert_eq!(c.sr[PSW] & (OV | S), OV | S);
        // ADD imm sign-extends; r0 stays 0.
        let (c, _) = run(&[f2(0x11, -1, 0), f2(0x11, -1, 4)], |c, _| c.r[4] = 1, 2);
        assert_eq!((c.r[0], c.r[4]), (0, 0));
    }

    #[test]
    fn shifts_carry_the_last_bit_out() {
        let (c, _) = run(&[f2(0x14, 1, 2)], |c, _| c.r[2] = 0x8000_0001, 1);
        assert_eq!(c.r[2], 2);
        assert!(c.sr[PSW] & CY != 0);
        let (c, _) = run(&[f2(0x17, 4, 2)], |c, _| c.r[2] = 0x8000_0018, 1);
        assert_eq!(c.r[2], 0xF800_0001);
        assert!(c.sr[PSW] & CY != 0);
        // By a register, only its low five bits count; by 0, no carry.
        let (c, _) = run(&[f1(0x05, 1, 2)], |c, _| {
            c.r[1] = 32;
            c.r[2] = 3;
            c.sr[PSW] = CY;
        }, 1);
        assert_eq!(c.r[2], 3);
        assert_eq!(c.sr[PSW] & CY, 0);
    }

    #[test]
    fn multiply_and_divide_use_r30() {
        let (c, _) = run(&[f1(0x08, 1, 2)], |c, _| {
            c.r[1] = 0xFFFF_FFFE;
            c.r[2] = 0x4000_0000;
        }, 1);
        assert_eq!((c.r[2], c.r[30]), (0x8000_0000, 0xFFFF_FFFF));
        assert!(c.sr[PSW] & OV == 0);
        let (c, _) = run(&[f1(0x0A, 1, 2)], |c, _| {
            c.r[1] = 0x10;
            c.r[2] = 0x1000_0001;
        }, 1);
        assert_eq!((c.r[2], c.r[30]), (0x10, 1));
        assert!(c.sr[PSW] & OV != 0);
        let (c, _) = run(&[f1(0x09, 1, 2)], |c, _| {
            c.r[1] = 0xFFFF_FFFD;
            c.r[2] = 7;
        }, 1);
        assert_eq!((c.r[2], c.r[30]), (0xFFFF_FFFE, 1));
        let (c, _) = run(&[f1(0x09, 1, 2)], |c, _| {
            c.r[1] = 0xFFFF_FFFF;
            c.r[2] = 0x8000_0000;
        }, 1);
        assert_eq!((c.r[2], c.r[30]), (0x8000_0000, 0));
        assert!(c.sr[PSW] & OV != 0);
        // Divide by zero: the exception, returning to the DIV itself.
        let (c, _) = run(&[f1(0x0B, 1, 2)], |c, _| c.r[2] = 9, 1);
        assert_eq!(c.pc, 0xFFFF_FF80);
        assert_eq!(c.sr[EIPC], 0x1000);
        assert_eq!(c.sr[ECR] & 0xFFFF, 0xFF80);
    }

    #[test]
    fn logic_immediates_are_unsigned() {
        let (c, _) = run(&[f5(0x2C, 1, 2, 0x8000), f5(0x2F, 0, 3, 0x1234), f5(0x28, 3, 3, 0xFFFF)], |c, _| {
            c.r[1] = 1
        }, 3);
        assert_eq!(c.r[2], 0x8001);
        assert!(c.sr[PSW] & S == 0);
        assert_eq!(c.r[3], 0x1233_FFFF);
        // ADDI sets flags, MOVEA doesn't.
        let (c, _) = run(&[f5(0x29, 1, 2, 0xFFFF)], |c, _| c.r[1] = 1, 1);
        assert_eq!(c.r[2], 0);
        assert_eq!(c.sr[PSW] & (Z | CY), Z | CY);
    }

    #[test]
    fn loads_extend_and_stores_align() {
        // ST.H r2, 3[r1]; LD.B r4, 1[r1]; IN.B r5, 1[r1]; LD.W r6, 2[r1]
        let (c, m) = run(
            &[f5(0x35, 1, 2, 3), f5(0x30, 1, 4, 1), f5(0x38, 1, 5, 1), f5(0x37, 1, 2, 4), f5(0x33, 1, 6, 6)],
            |c, _| {
                c.r[1] = 0x2000;
                c.r[2] = 0x1234_80FF;
            },
            5,
        );
        assert_eq!(&m.0[0x2002..0x2004], &[0xFF, 0x80]);
        assert_eq!(c.r[4], 0);
        assert_eq!(c.r[5], 0);
        assert_eq!(c.r[6], 0x1234_80FF);
        let (c, _) = run(&[f5(0x30, 1, 4, 0)], |c, m| {
            c.r[1] = 0x2000;
            m.0[0x2000] = 0x80;
        }, 1);
        assert_eq!(c.r[4], 0xFFFF_FF80);
    }

    #[test]
    fn branches_and_setf_follow_the_conditions() {
        // CMP r1, r2 with r2 < r1 (signed): BLT taken.
        let (c, _) = run(&[f1(0x03, 1, 2), br(6, 0x10)], |c, _| {
            c.r[1] = 1;
            c.r[2] = 0xFFFF_FFFF;
        }, 2);
        assert_eq!(c.pc, 0x1012);
        // ...but not below (unsigned): BL not taken.
        let (c, _) = run(&[f1(0x03, 1, 2), br(1, 0x10)], |c, _| {
            c.r[1] = 1;
            c.r[2] = 0xFFFF_FFFF;
        }, 2);
        assert_eq!(c.pc, 0x1004);
        // BR backwards; NOP never.
        let (c, _) = run(&[br(13, 0x40), br(5, -2)], |_, _| {}, 2);
        assert_eq!(c.pc, 0x1000);
        let (c, _) = run(&[f2(0x12, 5, 7), f2(0x12, 13, 8)], |c, _| c.r[8] = 9, 2);
        assert_eq!((c.r[7], c.r[8]), (1, 0));
    }

    #[test]
    fn calls_are_tracked_by_the_stack_pointer() {
        // 0x1000: JAL 0x1100. 0x1100: a routine that saves the link register, calls a leaf
        // at 0x1200 and returns. 0x1200: the leaf, JMP [lp].
        let mut flat: Vec<u16> = Vec::new();
        let mut put = |at: usize, w: Vec<u16>| {
            let i = (at - 0x1000) / 2;
            if flat.len() < i + w.len() {
                flat.resize(i + w.len(), 0);
            }
            flat[i..i + w.len()].copy_from_slice(&w);
        };
        put(0x1000, jal(0x100));
        put(0x1100, f5(0x28, 3, 3, 0xFFFC));
        put(0x1104, f5(0x37, 3, 31, 0));
        put(0x1108, jal(0xF8));
        put(0x110C, f5(0x33, 3, 31, 0));
        put(0x1110, f5(0x28, 3, 3, 4));
        put(0x1114, f1(0x06, 31, 0));
        put(0x1200, f1(0x06, 31, 0));
        let (mut c, mut m) = run(&[flat], |c, _| c.r[3] = 0x8000, 0);
        let mut depth = Vec::new();
        for _ in 0..9 {
            c.step(&mut m);
            depth.push(c.calls.len());
        }
        assert_eq!(depth, vec![1, 1, 1, 2, 1, 1, 1, 0, 0], "{:?}", c.calls);
        assert_eq!(c.pc, 0x1006);
    }

    #[test]
    fn interrupts_and_reti() {
        let (mut c, mut m) = run(&[f2(0x1A, 0, 0)], |c, _| c.r[3] = 0x8000, 1);
        assert!(c.halted);
        c.sr[PSW] = 3 << 16;
        assert!(!c.accepts(2));
        assert_eq!(c.interrupt(4), Some(10));
        assert!(!c.halted);
        assert_eq!(c.pc, 0xFFFF_FE40);
        assert_eq!(c.sr[EIPC], 0x1002);
        assert_eq!((c.sr[PSW] >> 16) & 0xF, 5);
        assert!(c.sr[PSW] & (EP | ID) == EP | ID);
        assert_eq!(c.calls.len(), 1);
        // RETI from the handler.
        m.write16(0xFE40, (0x19 << 10) as u16);
        c.step(&mut m);
        assert_eq!(c.pc, 0x1002);
        assert_eq!(c.sr[PSW], 3 << 16);
        assert!(c.calls.is_empty());
        // TRAP 0x13: to FFFFFFB0, returning after it.
        let (c, _) = run(&[f2(0x18, 0x13, 0)], |_, _| {}, 1);
        assert_eq!(c.pc, 0xFFFF_FFB0);
        assert_eq!(c.sr[EIPC], 0x1002);
        assert_eq!(c.sr[ECR] & 0xFFFF, 0xFFB3);
    }

    #[test]
    fn system_registers() {
        // LDSR r1, PSW; STSR PIR, r2; CLI (0x16); SEI (0x1E)
        let (c, _) = run(&[f2(0x1C, 5, 1), f2(0x1D, 6, 2), f2(0x16, 0, 0)], |c, _| c.r[1] = 0xFFFF_FFFF, 3);
        assert_eq!(c.sr[PSW], 0xFF3FF & !ID);
        assert_eq!(c.r[2], 0x5346);
    }

    #[test]
    fn caxi_swaps_when_equal() {
        let (c, m) = run(&[f5(0x3A, 1, 2, 4)], |c, m| {
            c.r[1] = 0x2000;
            c.r[2] = 7;
            c.r[30] = 0x55;
            m.0[0x2004] = 7;
        }, 1);
        assert_eq!(m.0[0x2004], 0x55);
        assert_eq!(c.r[2], 7);
        assert!(c.sr[PSW] & Z != 0);
        let (c, m) = run(&[f5(0x3A, 1, 2, 4)], |c, m| {
            c.r[1] = 0x2000;
            c.r[2] = 8;
            c.r[30] = 0x55;
            m.0[0x2004] = 7;
        }, 1);
        assert_eq!(m.0[0x2004], 7);
        assert_eq!(c.r[2], 7);
    }

    #[test]
    fn bit_strings_move_and_search() {
        // MOVBSU: 40 bits from 0x2000 bit 4 to 0x2100 bit 8.
        let (c, m) = run(&[vec![(0x1F << 10 | 0x0B) as u16]], |c, m| {
            for i in 0..8 {
                m.0[0x2000 + i] = 0xA5u8.wrapping_add(i as u8 * 17);
            }
            c.r[26] = 8;
            c.r[27] = 4;
            c.r[28] = 40;
            c.r[29] = 0x2100;
            c.r[30] = 0x2000;
        }, 1);
        let src = u64::from_le_bytes(m.0[0x2000..0x2008].try_into().unwrap());
        let dst = u64::from_le_bytes(m.0[0x2100..0x2108].try_into().unwrap());
        assert_eq!((dst >> 8) & ((1 << 40) - 1), (src >> 4) & ((1 << 40) - 1));
        assert_eq!(dst & 0xFF, 0);
        assert_eq!((c.r[26], c.r[27], c.r[28], c.r[29], c.r[30]), (16, 12, 0, 0x2104, 0x2004));
        // SCH1BSU: the first set bit at or after bit 3 of 0x2000 is bit 37.
        let (c, _) = run(&[vec![(0x1F << 10 | 0x02) as u16]], |c, m| {
            m.0[0x2004] = 0x20;
            c.r[27] = 3;
            c.r[28] = 100;
            c.r[29] = 0;
            c.r[30] = 0x2000;
        }, 1);
        assert!(c.sr[PSW] & Z == 0);
        assert_eq!((c.r[29], c.r[30], c.r[27], c.r[28]), (34, 0x2004, 4, 66));
        // SCH0BSD over all-ones: not found.
        let (c, _) = run(&[vec![(0x1F << 10 | 0x01) as u16]], |c, m| {
            m.0[0x2000..0x2008].fill(0xFF);
            c.r[27] = 31;
            c.r[28] = 10;
            c.r[30] = 0x2004;
        }, 1);
        assert!(c.sr[PSW] & Z != 0);
        assert_eq!((c.r[28], c.r[29], c.r[27]), (0, 10, 21));
    }

    #[test]
    fn floating_point() {
        let one = 1.0f32.to_bits();
        let half = 0.5f32.to_bits();
        // CVT.WS r1 -> r2; ADDF.S r3, r2; MULF.S r3, r2; CMPF.S r3, r2; TRNC.SW r2 -> r4
        let (c, _) = run(
            &[fpp(0x02, 1, 2), fpp(0x04, 3, 2), fpp(0x06, 3, 2), fpp(0x00, 3, 2), fpp(0x0B, 2, 4), fpp(0x03, 5, 6)],
            |c, _| {
                c.r[1] = 3;
                c.r[3] = half;
                c.r[5] = 2.5f32.to_bits();
            },
            6,
        );
        assert_eq!(f32::from_bits(c.r[2]), 1.75);
        assert_eq!(c.r[4], 1);
        assert_eq!(c.r[6], 2);
        assert!(c.sr[PSW] & (Z | CY) == 0);
        // CMPF: less than sets S and CY.
        let (c, _) = run(&[fpp(0x00, 1, 2)], |c, _| {
            c.r[1] = one;
            c.r[2] = half;
        }, 1);
        assert_eq!(c.sr[PSW] & (Z | S | CY), S | CY);
        // Dividing by zero is an exception and writes nothing.
        let (c, _) = run(&[fpp(0x07, 1, 2)], |c, _| c.r[2] = one, 1);
        assert_eq!(c.pc, 0xFFFF_FF60);
        assert_eq!(c.r[2], one);
        assert_eq!(c.sr[ECR] & 0xFFFF, 0xFF68);
        // The Virtual Boy's own: XB, XH, REV, MPYHW.
        let (c, _) = run(&[fpp(0x08, 0, 2), fpp(0x09, 0, 3), fpp(0x0A, 1, 4), fpp(0x0C, 5, 6)], |c, _| {
            c.r[1] = 1;
            c.r[2] = 0x1234_5678;
            c.r[3] = 0x1234_5678;
            c.r[5] = 0x1_FFFF;
            c.r[6] = 3;
        }, 4);
        assert_eq!(c.r[2], 0x1234_7856);
        assert_eq!(c.r[3], 0x5678_1234);
        assert_eq!(c.r[4], 0x8000_0000);
        assert_eq!(c.r[6], 0xFFFF_FFFD);
    }

    #[test]
    fn a_call_through_a_pointer_counts() {
        // MOVEA 0x1008 -> lp? The pattern: lp = the address after the JMP, then JMP [r10].
        let (c, _) = run(&[f5(0x28, 0, 31, 0x1006), f1(0x06, 10, 0)], |c, _| {
            c.r[10] = 0x1100;
            c.r[3] = 0x8000;
        }, 2);
        assert_eq!(c.pc, 0x1100);
        assert_eq!(c.calls, vec![(0x1100, 0x8000)]);
    }
}
