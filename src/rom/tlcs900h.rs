//! A TLCS-900/H interpreter: the Neo Geo Pocket's main CPU, Toshiba's 16/32-bit core. It
//! runs a game's own code to drive the sound hardware (see [`super::ngp_music`]),
//! instruction by instruction, counting roughly what each takes. It follows ares's core
//! instruction for instruction (including what it does with undefined forms), and is
//! checked against the SingleStepTests TLCS-900/H suite, which was made from that core.
//!
//! Like [`super::v30mz`] it keeps a shadow of the subroutine calls in progress, which is how
//! the music search tells which routine wrote to the sound hardware; calls are named by the
//! 24-bit address they enter.

/// What the CPU is attached to: 24-bit addresses, read and written a byte at a time.
pub trait Bus {
    fn read(&mut self, addr: u32) -> u8;
    fn write(&mut self, addr: u32, value: u8);
}

/// Flag bits in F.
pub const CF: u8 = 0x01;
pub const NF: u8 = 0x02;
pub const VF: u8 = 0x04;
pub const HF: u8 = 0x10;
pub const ZF: u8 = 0x40;
pub const SF: u8 = 0x80;
const FLAGS: u8 = 0xD7;

/// Indexes into [`Cpu::r`]: bank `b`'s XWA, XBC, XDE, XHL are `4 * b + 0..4`; then these.
pub const XIX: usize = 16;
pub const XIY: usize = 17;
pub const XIZ: usize = 18;
pub const XSP: usize = 19;

/// Register codes, as the full-byte form encodes them (current bank).
pub const A: u8 = 0xE0;
pub const W: u8 = 0xE1;
pub const C: u8 = 0xE4;
pub const B: u8 = 0xE5;
pub const BC: u8 = 0xE4;
pub const XDE: u8 = 0xE8;
pub const XHL: u8 = 0xEC;
pub const XIX_CODE: u8 = 0xF0;
pub const XIY_CODE: u8 = 0xF4;
pub const XSP_CODE: u8 = 0xFC;

/// How deep the shadow of calls in progress goes; deeper calls are not tracked.
const MAX_CALLS: usize = 32;

const R8: [u8; 8] = [0xE1, 0xE0, 0xE5, 0xE4, 0xE9, 0xE8, 0xED, 0xEC];
const R16: [u8; 8] = [0xE0, 0xE4, 0xE8, 0xEC, 0xF0, 0xF4, 0xF8, 0xFC];

/// An operand: a register (by its code), memory, a constant or a control register.
#[derive(Clone, Copy, Debug)]
enum Loc {
    Reg(u8),
    Mem(u32),
    Imm(u32),
    Cr(u8),
}

fn mask(sz: u32) -> u32 {
    if sz >= 4 {
        u32::MAX
    } else {
        (1 << (sz * 8)) - 1
    }
}

fn msb(v: u32, sz: u32) -> bool {
    v >> (sz * 8 - 1) & 1 != 0
}

fn sext(v: u32, sz: u32) -> i64 {
    let bits = sz * 8;
    ((v as i64) << (64 - bits)) >> (64 - bits)
}

/// The register `n` of a 3-bit field, at size `sz`.
fn r3(sz: u32, n: u8) -> u8 {
    if sz == 1 {
        R8[n as usize & 7]
    } else {
        R16[n as usize & 7]
    }
}

/// The register twice the size that holds register `code` of size `sz`.
fn expand(code: u8, sz: u32) -> u8 {
    if sz == 1 {
        code & !1
    } else {
        code & !3
    }
}

/// The registers, and the calls in progress.
#[derive(Clone, Debug)]
pub struct Cpu {
    /// XWA, XBC, XDE, XHL of the four banks, then XIX, XIY, XIZ, XSP.
    pub r: [u32; 20],
    pub pc: u32,
    /// DMAS0-3, DMAD0-3, DMAM0-3, then INTNEST.
    pub cr: [u32; 13],
    pub f: u8,
    /// F', swapped in by EX F,F'.
    pub fp: u8,
    /// The register bank in use.
    pub rfp: u8,
    /// The interrupt mask: levels below it are not taken.
    pub iff: u8,
    /// Stopped by HALT until an interrupt.
    pub halted: bool,
    /// The entry points of the subroutines in progress, outermost first, and the stack
    /// pointer before each call pushed.
    pub calls: Vec<(u32, u32)>,
    /// When set, software interrupts are not taken but left in [`Cpu::swi`] for the machine
    /// to act on (a BIOS stand-in).
    pub swi_trap: bool,
    pub swi: Option<u8>,
    /// The instruction's first byte.
    op: u8,
    /// Clocks the instruction took, roughly.
    clk: u32,
}

impl Cpu {
    /// The state after reset (ares's power-on state): stack at 0x100, interrupts masked.
    pub fn reset() -> Self {
        let mut r = [0; 20];
        r[XSP] = 0x100;
        Self {
            r,
            pc: 0,
            cr: [0; 13],
            f: 0,
            fp: 0,
            rfp: 0,
            iff: 7,
            halted: false,
            calls: Vec::new(),
            swi_trap: false,
            swi: None,
            op: 0,
            clk: 0,
        }
    }

    /// The status register: F, the bank, the interrupt mask, and the bits always set.
    pub fn sr(&self) -> u16 {
        self.f as u16 | (self.rfp as u16) << 8 | 1 << 11 | (self.iff as u16) << 12 | 1 << 15
    }

    pub fn set_sr(&mut self, v: u16) {
        self.f = v as u8 & FLAGS;
        self.rfp = (v >> 8) as u8 & 3;
        self.iff = (v >> 12) as u8 & 7;
    }

    fn flag(&self, f: u8) -> bool {
        self.f & f != 0
    }

    fn set(&mut self, f: u8, on: bool) {
        if on {
            self.f |= f;
        } else {
            self.f &= !f;
        }
    }

    // Registers.

    fn slot(&self, code: u8) -> Option<usize> {
        let n = (code >> 2 & 3) as usize;
        match code {
            0x00..=0x3F => Some((code >> 2) as usize),
            0xD0..=0xDF => Some((self.rfp.wrapping_sub(1) & 3) as usize * 4 + n),
            0xE0..=0xEF => Some(self.rfp as usize * 4 + n),
            0xF0..=0xFF => Some(16 + n),
            _ => None,
        }
    }

    fn shift(code: u8, sz: u32) -> u32 {
        match sz {
            1 => (code & 3) as u32 * 8,
            2 => (code & 2) as u32 * 8,
            _ => 0,
        }
    }

    /// Register `code` (as instructions encode it) at size `sz`; codes that name no
    /// register read 0.
    pub fn reg(&self, code: u8, sz: u32) -> u32 {
        match self.slot(code) {
            Some(i) => self.r[i] >> Self::shift(code, sz) & mask(sz),
            None => 0,
        }
    }

    pub fn set_reg(&mut self, code: u8, sz: u32, v: u32) {
        if let Some(i) = self.slot(code) {
            let sh = Self::shift(code, sz);
            let m = mask(sz) << sh;
            self.r[i] = self.r[i] & !m | (v << sh & m);
        }
    }

    /// The stack pointer.
    pub fn sp(&self) -> u32 {
        self.r[XSP]
    }

    fn cr_slot(id: u8, sz: u32) -> Option<(usize, u32)> {
        match sz {
            1 => match id {
                0x2E => Some((11, 24)),
                0x00..=0x2F => Some(((id >> 2) as usize, (id & 3) as u32 * 8)),
                0x3C | 0x3D => Some((12, (id & 1) as u32 * 8)),
                _ => None,
            },
            2 => match id & !1 {
                k @ 0x00..=0x2F => Some(((k >> 2) as usize, (k & 2) as u32 * 8)),
                0x3C => Some((12, 0)),
                _ => None,
            },
            _ => match id & !1 {
                k @ 0x00..=0x2F if k & 3 == 0 => Some(((k >> 2) as usize, 0)),
                0x3C => Some((12, 0)),
                _ => None,
            },
        }
    }

    fn cr_get(&self, id: u8, sz: u32) -> u32 {
        match Self::cr_slot(id, sz) {
            Some((i, sh)) => self.cr[i] >> sh & mask(sz),
            None => 0,
        }
    }

    fn cr_set(&mut self, id: u8, sz: u32, mut v: u32) {
        if sz == 4 && id & !3 == 0x3C {
            // INTNEST is 16 bits.
            v &= 0xFFFF;
        }
        if let Some((i, sh)) = Self::cr_slot(id, sz) {
            let m = mask(sz) << sh;
            self.cr[i] = self.cr[i] & !m | (v << sh & m);
        }
    }

    fn intnest_add(&mut self, d: u32) {
        let v = self.cr_get(0x3C, 2).wrapping_add(d);
        self.cr_set(0x3C, 2, v);
    }

    // Memory.

    fn rd<B: Bus>(&mut self, bus: &mut B, addr: u32, sz: u32) -> u32 {
        let mut v = 0;
        for i in 0..sz {
            v |= (bus.read(addr.wrapping_add(i) & 0xFF_FFFF) as u32) << (8 * i);
        }
        v
    }

    fn wr<B: Bus>(&mut self, bus: &mut B, addr: u32, sz: u32, v: u32) {
        for i in 0..sz {
            bus.write(addr.wrapping_add(i) & 0xFF_FFFF, (v >> (8 * i)) as u8);
        }
    }

    fn fetch<B: Bus>(&mut self, bus: &mut B, sz: u32) -> u32 {
        let mut v = 0;
        for i in 0..sz {
            v |= (bus.read(self.pc & 0xFF_FFFF) as u32) << (8 * i);
            self.pc = self.pc.wrapping_add(1);
        }
        v
    }

    fn fetch8<B: Bus>(&mut self, bus: &mut B) -> u8 {
        self.fetch(bus, 1) as u8
    }

    fn fetch_s8<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.fetch(bus, 1) as i8 as i32 as u32
    }

    fn fetch_s16<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.fetch(bus, 2) as i16 as i32 as u32
    }

    pub fn push<B: Bus>(&mut self, bus: &mut B, sz: u32, v: u32) {
        self.r[XSP] = self.r[XSP].wrapping_sub(sz);
        let sp = self.r[XSP];
        self.wr(bus, sp, sz, v);
    }

    pub fn pop<B: Bus>(&mut self, bus: &mut B, sz: u32) -> u32 {
        let sp = self.r[XSP];
        let v = self.rd(bus, sp, sz);
        self.r[XSP] = self.r[XSP].wrapping_add(sz);
        v
    }

    /// Pushes an operand, read after the stack pointer moves (so PUSH XSP pushes the new
    /// value).
    fn push_loc<B: Bus>(&mut self, bus: &mut B, l: Loc, sz: u32) {
        self.r[XSP] = self.r[XSP].wrapping_sub(sz);
        let v = self.load(bus, l, sz);
        let sp = self.r[XSP];
        self.wr(bus, sp, sz, v);
    }

    /// Pops into an operand, before the stack pointer moves (so POP XSP adds to the popped
    /// value).
    fn pop_loc<B: Bus>(&mut self, bus: &mut B, l: Loc, sz: u32) {
        let sp = self.r[XSP];
        let v = self.rd(bus, sp, sz);
        self.store(bus, l, sz, v);
        self.r[XSP] = self.r[XSP].wrapping_add(sz);
    }

    fn load<B: Bus>(&mut self, bus: &mut B, l: Loc, sz: u32) -> u32 {
        match l {
            Loc::Reg(c) => self.reg(c, sz),
            Loc::Mem(a) => self.rd(bus, a, sz),
            Loc::Imm(v) => v & mask(sz),
            Loc::Cr(c) => self.cr_get(c, sz),
        }
    }

    fn store<B: Bus>(&mut self, bus: &mut B, l: Loc, sz: u32, v: u32) {
        match l {
            Loc::Reg(c) => self.set_reg(c, sz, v),
            Loc::Mem(a) => self.wr(bus, a, sz, v & mask(sz)),
            Loc::Imm(_) => {}
            Loc::Cr(c) => self.cr_set(c, sz, v),
        }
    }

    // The call shadow.

    fn enter(&mut self, target: u32, sp: u32) {
        if self.calls.len() < MAX_CALLS {
            self.calls.push((target, sp));
        }
    }

    /// A return: pops every tracked call the stack pointer has now left.
    fn leave(&mut self) {
        while let Some(&(_, sp)) = self.calls.last() {
            if self.r[XSP].wrapping_sub(sp) as i32 >= 0 {
                self.calls.pop();
            } else {
                break;
            }
        }
    }

    fn call<B: Bus>(&mut self, bus: &mut B, target: u32) {
        let sp = self.r[XSP];
        let pc = self.pc;
        self.push(bus, 4, pc);
        self.pc = target;
        self.enter(target, sp);
    }

    fn ret<B: Bus>(&mut self, bus: &mut B) {
        self.pc = self.pop(bus, 4);
        self.leave();
    }

    /// The CPU's own interrupt: PC and SR pushed, through the vector table at 0xFFFF00.
    pub fn interrupt<B: Bus>(&mut self, bus: &mut B, vector: u8) {
        self.halted = false;
        let sp = self.r[XSP];
        let pc = self.pc;
        self.push(bus, 4, pc);
        let sr = self.sr() as u32;
        self.push(bus, 2, sr);
        self.pc = self.rd(bus, 0xFF_FF00 | vector as u32, 4);
        self.intnest_add(1);
        self.enter(self.pc, sp);
    }

    /// An interrupt taken straight to `target` at priority `level`, as the Neo Geo Pocket's
    /// BIOS does through its table in RAM: PC and SR pushed, the mask raised past `level`.
    pub fn interrupt_to<B: Bus>(&mut self, bus: &mut B, target: u32, level: u8) {
        self.halted = false;
        let sp = self.r[XSP];
        let pc = self.pc;
        self.push(bus, 4, pc);
        let sr = self.sr() as u32;
        self.push(bus, 2, sr);
        self.iff = if level < 7 { level + 1 } else { 7 };
        self.pc = target;
        self.enter(target, sp);
    }

    /// Returns from an interrupt (what RETI does).
    pub fn reti<B: Bus>(&mut self, bus: &mut B) {
        let sr = self.pop(bus, 2) as u16;
        self.set_sr(sr);
        self.ret(bus);
        self.intnest_add(u32::MAX);
    }

    /// Returns from a subroutine (what RET does).
    pub fn ret_now<B: Bus>(&mut self, bus: &mut B) {
        self.ret(bus);
    }

    fn swi<B: Bus>(&mut self, bus: &mut B, n: u8) {
        if self.swi_trap {
            self.swi = Some(n);
        } else {
            self.interrupt(bus, n << 2);
        }
    }

    fn undefined<B: Bus>(&mut self, bus: &mut B) {
        self.swi(bus, 2);
    }

    // Arithmetic.

    fn parity(v: u32, sz: u32) -> bool {
        sz < 4 && (v & mask(sz)).count_ones() % 2 == 0
    }

    fn zs(&mut self, r: u32, sz: u32) {
        self.set(ZF, r & mask(sz) == 0);
        self.set(SF, msb(r, sz));
    }

    fn add(&mut self, t: u32, s: u32, c: u32, sz: u32) -> u32 {
        let r = (t as u64 + s as u64 + c as u64) as u32 & mask(sz);
        let carries = t ^ s ^ r;
        let ov = (t ^ r) & (s ^ r);
        self.set(CF, msb(carries ^ ov, sz));
        self.set(NF, false);
        self.set(VF, msb(ov, sz));
        self.set(HF, sz < 4 && carries & 0x10 != 0);
        self.zs(r, sz);
        r
    }

    fn sub(&mut self, t: u32, s: u32, c: u32, sz: u32) -> u32 {
        let r = t.wrapping_sub(s).wrapping_sub(c) & mask(sz);
        let carries = t ^ s ^ r;
        let ov = (t ^ r) & (s ^ t);
        self.set(CF, msb(carries ^ ov, sz));
        self.set(NF, true);
        self.set(VF, msb(ov, sz));
        self.set(HF, sz < 4 && carries & 0x10 != 0);
        self.zs(r, sz);
        r
    }

    fn logic(&mut self, r: u32, h: bool, sz: u32) -> u32 {
        self.set(CF, false);
        self.set(NF, false);
        self.set(VF, Self::parity(r, sz));
        self.set(HF, h);
        self.zs(r, sz);
        r
    }

    fn inc(&mut self, t: u32, s: u32, sz: u32) -> u32 {
        let r = t.wrapping_add(s) & mask(sz);
        self.set(NF, false);
        self.set(VF, msb((t ^ r) & (s ^ r), sz));
        self.set(HF, (t ^ s ^ r) & 0x10 != 0);
        self.zs(r, sz);
        r
    }

    fn dec(&mut self, t: u32, s: u32, sz: u32) -> u32 {
        let r = t.wrapping_sub(s) & mask(sz);
        self.set(NF, true);
        self.set(VF, msb((t ^ s) & (t ^ r), sz));
        self.set(HF, (t ^ s ^ r) & 0x10 != 0);
        self.zs(r, sz);
        r
    }

    /// Flags after a rotate or shift (the carry is already set).
    fn shifted(&mut self, r: u32, sz: u32) -> u32 {
        self.set(NF, false);
        self.set(VF, Self::parity(r, sz));
        self.set(HF, false);
        self.zs(r, sz);
        r
    }

    fn condition(&self, cc: u8) -> bool {
        let (s, z, v, c) = (self.flag(SF), self.flag(ZF), self.flag(VF), self.flag(CF));
        let t = match cc & 7 {
            0 => false,
            1 => s ^ v,
            2 => z | (s ^ v),
            3 => c | z,
            4 => v,
            5 => s,
            6 => z,
            _ => c,
        };
        t ^ (cc & 8 != 0)
    }

    /// ADD, ADC, SUB, SBC, AND, XOR, OR, CP, as their encodings number them.
    fn alu<B: Bus>(&mut self, bus: &mut B, op: u8, t: Loc, s: Loc, sz: u32) {
        let a = self.load(bus, t, sz);
        let b = self.load(bus, s, sz);
        let c = self.flag(CF) as u32;
        let r = match op & 7 {
            0 => self.add(a, b, 0, sz),
            1 => self.add(a, b, c, sz),
            2 => self.sub(a, b, 0, sz),
            3 => self.sub(a, b, c, sz),
            4 => self.logic(a & b, true, sz),
            5 => self.logic(a ^ b, false, sz),
            6 => self.logic(a | b, false, sz),
            _ => {
                self.sub(a, b, 0, sz);
                return;
            }
        };
        self.store(bus, t, sz, r);
    }

    fn ld<B: Bus>(&mut self, bus: &mut B, t: Loc, s: Loc, sz: u32) {
        let v = self.load(bus, s, sz);
        self.store(bus, t, sz, v);
    }

    fn ex<B: Bus>(&mut self, bus: &mut B, t: Loc, s: Loc, sz: u32) {
        let a = self.load(bus, t, sz);
        let b = self.load(bus, s, sz);
        self.store(bus, t, sz, b);
        self.store(bus, s, sz, a);
    }

    fn incdec<B: Bus>(&mut self, bus: &mut B, t: Loc, n: u8, sz: u32, down: bool) {
        let n = if n & 7 == 0 { 8 } else { (n & 7) as u32 };
        let v = self.load(bus, t, sz);
        let r = if matches!(t, Loc::Reg(_)) && sz > 1 {
            // INCW/INCL and DECW/DECL of a register leave the flags alone.
            if down {
                v.wrapping_sub(n)
            } else {
                v.wrapping_add(n)
            }
        } else if down {
            self.dec(v, n, sz)
        } else {
            self.inc(v, n, sz)
        };
        self.store(bus, t, sz, r);
    }

    /// The rotates and shifts: RLC, RRC, RL, RR, SLA, SRA, SLL, SRL by `amount` (0 is 16).
    fn rotate<B: Bus>(&mut self, bus: &mut B, op: u8, t: Loc, amount: u32, sz: u32) {
        let mut r = self.load(bus, t, sz);
        let len = match amount & 15 {
            0 => 16,
            n => n,
        };
        let top = sz * 8 - 1;
        let m = mask(sz);
        let mut c = self.flag(CF);
        for _ in 0..len {
            match op & 7 {
                0 => {
                    c = msb(r, sz);
                    r = (r << 1 | c as u32) & m;
                }
                1 => {
                    c = r & 1 != 0;
                    r = (c as u32) << top | r >> 1;
                }
                2 => {
                    let out = msb(r, sz);
                    r = (r << 1 | c as u32) & m;
                    c = out;
                }
                3 => {
                    let out = r & 1 != 0;
                    r = (c as u32) << top | r >> 1;
                    c = out;
                }
                4 | 6 => {
                    c = msb(r, sz);
                    r = r << 1 & m;
                }
                5 => {
                    c = r & 1 != 0;
                    r = r >> 1 | r & (1 << top);
                }
                _ => {
                    c = r & 1 != 0;
                    r >>= 1;
                }
            }
        }
        self.set(CF, c);
        let r = self.shifted(r, sz);
        self.store(bus, t, sz, r);
    }

    /// ANDCF, ORCF, XORCF, LDCF: the carry with bit `off` of `src`. A byte's bit taken from
    /// A past 7 is undefined, and does nothing.
    fn carry_op<B: Bus>(&mut self, bus: &mut B, op: u8, src: Loc, off: u32, from_a: bool, sz: u32) {
        if sz == 1 && from_a && off & 8 != 0 {
            return;
        }
        let v = self.load(bus, src, sz);
        let b = v >> (off & (sz * 8 - 1)) & 1 != 0;
        let c = self.flag(CF);
        let r = match op {
            0 => c & b,
            1 => c | b,
            2 => c ^ b,
            _ => b,
        };
        self.set(CF, r);
    }

    fn store_carry<B: Bus>(&mut self, bus: &mut B, t: Loc, off: u32, sz: u32) {
        if sz == 1 && off & 8 != 0 {
            return;
        }
        let mut v = self.load(bus, t, sz);
        // ares shifts a 64-bit value by the offset, which x86 takes modulo 64.
        let off = off & 63;
        if off < sz * 8 {
            v = v & !(1 << off) | (self.flag(CF) as u32) << off;
        }
        self.store(bus, t, sz, v);
    }

    fn bit<B: Bus>(&mut self, bus: &mut B, src: Loc, off: u32, sz: u32) {
        let v = self.load(bus, src, sz);
        self.set(NF, false);
        self.set(VF, false);
        self.set(HF, true);
        self.set(ZF, v >> off & 1 == 0);
        self.set(SF, false);
    }

    /// RES, SET, CHG, TSET of bit `off`.
    fn bit_op<B: Bus>(&mut self, bus: &mut B, op: u8, t: Loc, off: u32, sz: u32) {
        let mut v = self.load(bus, t, sz);
        let b = 1 << (off & (sz * 8 - 1));
        match op {
            0 => v &= !b,
            1 => v |= b,
            2 => v ^= b,
            _ => {
                self.set(NF, false);
                self.set(VF, false);
                self.set(HF, true);
                self.set(ZF, v & b != 0);
                self.set(SF, false);
                v |= b;
            }
        }
        self.store(bus, t, sz, v);
    }

    fn mul<B: Bus>(&mut self, bus: &mut B, t: u8, s: Loc, sz: u32, signed: bool) {
        let a = self.reg(t, sz);
        let b = self.load(bus, s, sz);
        let r = if signed {
            (sext(a, sz) * sext(b, sz)) as u32
        } else {
            (a as u64 * b as u64) as u32
        };
        self.set_reg(expand(t, sz), sz * 2, r);
    }

    fn div<B: Bus>(&mut self, bus: &mut B, t: u8, s: Loc, sz: u32, signed: bool) {
        let bits = sz * 8;
        let tm = mask(sz) as u64;
        let em = mask(sz * 2) as u64;
        let e = expand(t, sz);
        let dividend = self.reg(e, sz * 2) as u64;
        let divisor = self.load(bus, s, sz) as u64;
        let (q, r) = if !signed {
            if divisor != 0 {
                (dividend / divisor, dividend % divisor)
            } else {
                (!(dividend >> bits) & tm, dividend & tm)
            }
        } else {
            let dd = sext(dividend as u32, sz * 2);
            let dv = sext(divisor as u32, sz);
            if dv != 0 {
                ((dd.wrapping_div(dv)) as u64 & em, (dd.wrapping_rem(dv)) as u64 & em)
            } else {
                (!(dd >> bits) as u64 & tm, dividend & tm)
            }
        };
        self.set_reg(e, sz * 2, ((r & tm) << bits | (q & tm)) as u32);
        self.set(VF, divisor == 0 || r >> bits != 0);
    }

    /// LDI, LDD (and with `repeat`, LDIR, LDDR).
    fn block_ld<B: Bus>(&mut self, bus: &mut B, sz: u32, down: bool, repeat: bool) {
        let (t, s) = if self.op & 7 == 5 { (XIX_CODE, XIY_CODE) } else { (XDE, XHL) };
        let adj = if down { sz.wrapping_neg() } else { sz };
        loop {
            self.clk += 14;
            let ta = self.reg(t, 4);
            let sa = self.reg(s, 4);
            let v = self.rd(bus, sa, sz);
            self.wr(bus, ta, sz, v);
            self.set_reg(s, 4, sa.wrapping_add(adj));
            self.set_reg(t, 4, ta.wrapping_add(adj));
            let bc = self.reg(BC, 2).wrapping_sub(1) & 0xFFFF;
            self.set_reg(BC, 2, bc);
            self.set(NF, false);
            self.set(VF, bc != 0);
            self.set(HF, false);
            if !repeat || bc == 0 {
                break;
            }
        }
    }

    /// CPI, CPD (and with `repeat`, CPIR, CPDR).
    fn block_cp<B: Bus>(&mut self, bus: &mut B, sz: u32, down: bool, repeat: bool) {
        let s = r3(4, self.op);
        let t = if sz == 1 { A } else { 0xE0 };
        let adj = if down { sz.wrapping_neg() } else { sz };
        loop {
            self.clk += 14;
            let c = self.flag(CF);
            let a = self.reg(t, sz);
            let sa = self.reg(s, 4);
            let v = self.rd(bus, sa, sz);
            self.sub(a, v, 0, sz);
            self.set_reg(s, 4, sa.wrapping_add(adj));
            let bc = self.reg(BC, 2).wrapping_sub(1) & 0xFFFF;
            self.set_reg(BC, 2, bc);
            self.set(CF, c);
            self.set(VF, bc != 0);
            if !repeat || bc == 0 || self.flag(ZF) {
                break;
            }
        }
    }

    fn daa(&mut self, code: u8) {
        let input = self.reg(code, 1) as u8;
        let mut v = input;
        let n = self.flag(NF);
        if self.flag(CF) || v > 0x99 {
            v = if n { v.wrapping_sub(0x60) } else { v.wrapping_add(0x60) };
        }
        if self.flag(HF) || v & 15 > 9 {
            v = if n { v.wrapping_sub(6) } else { v.wrapping_add(6) };
        }
        if !n && v < input || n && v > input {
            self.set(CF, true);
        }
        self.set(VF, Self::parity(v as u32, 1));
        self.set(HF, (v ^ input) & 0x10 != 0);
        self.set(ZF, v == 0);
        self.set(SF, v & 0x80 != 0);
        self.set_reg(code, 1, v as u32);
    }

    // Decoding.

    /// Runs one instruction (or waits while halted). Returns the clocks taken, at the
    /// system clock (6.144 MHz on the Neo Geo Pocket), roughly.
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> u32 {
        if self.halted {
            return 8;
        }
        self.clk = 0;
        self.swi = None;
        let op = self.fetch8(bus);
        self.op = op;
        self.instruction(bus, op);
        (self.clk / 2).max(2)
    }

    fn instruction<B: Bus>(&mut self, bus: &mut B, op: u8) {
        self.clk += 4;
        match op {
            0x00 => {}
            0x01 | 0x04 | 0x1F | 0x50..=0x57 | 0xC6 | 0xD6 | 0xE6 | 0xF6 => self.undefined(bus),
            0x02 => {
                let sr = self.sr() as u32;
                self.push(bus, 2, sr);
            }
            0x03 => {
                let v = self.pop(bus, 2) as u16;
                self.set_sr(v);
            }
            0x05 => self.halted = true,
            0x06 => self.iff = self.fetch8(bus) & 7,
            0x07 => {
                self.clk += 16;
                self.reti(bus);
            }
            0x08 => {
                let a = self.fetch(bus, 1);
                let v = self.fetch(bus, 1);
                self.wr(bus, a, 1, v);
            }
            0x09 => {
                let v = self.fetch(bus, 1);
                self.push(bus, 1, v);
            }
            0x0A => {
                let a = self.fetch(bus, 1);
                let v = self.fetch(bus, 2);
                self.wr(bus, a, 2, v);
            }
            0x0B => {
                let v = self.fetch(bus, 2);
                self.push(bus, 2, v);
            }
            0x0C => self.rfp = self.rfp.wrapping_add(1) & 3,
            0x0D => self.rfp = self.rfp.wrapping_sub(1) & 3,
            0x0E => {
                self.clk += 12;
                self.ret(bus);
            }
            0x0F => {
                let d = self.fetch_s16(bus);
                self.clk += 16;
                self.ret(bus);
                self.r[XSP] = self.r[XSP].wrapping_add(d);
            }
            0x10 | 0x11 => {
                self.set(CF, op == 0x11);
                self.set(NF, false);
                self.set(HF, false);
            }
            0x12 | 0x13 => {
                let v = if op == 0x12 { self.flag(CF) } else { self.flag(ZF) };
                self.set(CF, !v);
                self.set(NF, false);
                self.set(HF, false);
            }
            0x14 => {
                let a = self.reg(A, 1);
                self.push(bus, 1, a);
            }
            0x15 => {
                let v = self.pop(bus, 1);
                self.set_reg(A, 1, v);
            }
            0x16 => std::mem::swap(&mut self.f, &mut self.fp),
            0x17 => self.rfp = self.fetch8(bus) & 3,
            0x18 => {
                let f = self.f as u32;
                self.push(bus, 1, f);
            }
            0x19 => self.f = self.pop(bus, 1) as u8 & FLAGS,
            0x1A | 0x1B => {
                self.pc = self.fetch(bus, if op == 0x1A { 2 } else { 3 });
                self.clk += 6;
            }
            0x1C | 0x1D => {
                let t = self.fetch(bus, if op == 0x1C { 2 } else { 3 });
                self.clk += 12;
                self.call(bus, t);
            }
            0x1E => {
                let d = self.fetch_s16(bus);
                self.clk += 12;
                let t = self.pc.wrapping_add(d);
                self.call(bus, t);
            }
            0x20..=0x27 => {
                let v = self.fetch(bus, 1);
                self.set_reg(r3(1, op), 1, v);
            }
            0x28..=0x2F => self.push_loc(bus, Loc::Reg(r3(2, op)), 2),
            0x30..=0x37 => {
                let v = self.fetch(bus, 2);
                self.set_reg(r3(2, op), 2, v);
            }
            0x38..=0x3F => self.push_loc(bus, Loc::Reg(r3(4, op)), 4),
            0x40..=0x47 => {
                let v = self.fetch(bus, 4);
                self.set_reg(r3(4, op), 4, v);
            }
            0x48..=0x4F => self.pop_loc(bus, Loc::Reg(r3(2, op)), 2),
            0x58..=0x5F => self.pop_loc(bus, Loc::Reg(r3(4, op)), 4),
            0x60..=0x7F => {
                let d = if op < 0x70 { self.fetch_s8(bus) } else { self.fetch_s16(bus) };
                if self.condition(op & 15) {
                    self.clk += 6;
                    self.pc = self.pc.wrapping_add(d);
                }
            }
            0x80..=0xAF => {
                let sz = [1, 2, 4][((op - 0x80) >> 4) as usize];
                let mut a = self.reg(r3(4, op), 4);
                if op & 8 != 0 {
                    a = a.wrapping_add(self.fetch_s8(bus));
                }
                self.source_memory(bus, a, sz);
            }
            0xB0..=0xBF => {
                let mut a = self.reg(r3(4, op), 4);
                if op & 8 != 0 {
                    a = a.wrapping_add(self.fetch_s8(bus));
                }
                self.target_memory(bus, a);
            }
            0xC7 | 0xD7 | 0xE7 => {
                let sz = [1, 2, 4][((op - 0xC0) >> 4) as usize];
                let r = self.fetch8(bus);
                self.register(bus, r, sz);
            }
            0xC8..=0xCF | 0xD8..=0xDF | 0xE8..=0xEF => {
                let sz = [1, 2, 4][((op - 0xC0) >> 4) as usize];
                self.register(bus, r3(sz, op), sz);
            }
            0xF7 => {
                self.fetch(bus, 1);
                let a = self.fetch(bus, 1);
                self.fetch(bus, 1);
                let v = self.fetch(bus, 1);
                self.fetch(bus, 1);
                self.wr(bus, a, 1, v);
            }
            0xF8..=0xFF => self.swi(bus, op & 7),
            // 0xC0-0xC5, 0xD0-0xD5, 0xE0-0xE5: source memory; 0xF0-0xF5: target memory.
            _ => {
                let target = op >= 0xF0;
                let sz = [1, 2, 4, 0][((op - 0xC0) >> 4) as usize];
                let Some(a) = self.address(bus, op & 7, target) else {
                    return;
                };
                if target {
                    self.target_memory(bus, a);
                } else {
                    self.source_memory(bus, a, sz);
                }
            }
        }
    }

    /// The address of the extended addressing modes (the low 3 bits of 0xC0-0xC5 and the
    /// rest), or None when the instruction was decided here (LDAR, or undefined).
    fn address<B: Bus>(&mut self, bus: &mut B, mode: u8, target: bool) -> Option<u32> {
        match mode {
            0 => Some(self.fetch(bus, 1)),
            1 => Some(self.fetch(bus, 2)),
            2 => Some(self.fetch(bus, 3)),
            3 => {
                let d = self.fetch8(bus);
                match d & 3 {
                    0 => return Some(self.reg(d, 4)),
                    1 => {
                        let r = self.reg(d, 4);
                        return Some(r.wrapping_add(self.fetch_s16(bus)));
                    }
                    _ => {}
                }
                match d {
                    0x03 => {
                        let r = self.fetch8(bus);
                        let r = self.reg(r, 4);
                        let x = self.fetch8(bus);
                        let x = self.reg(x, 1) as i8 as i32 as u32;
                        Some(r.wrapping_add(x))
                    }
                    0x07 => {
                        let r = self.fetch8(bus);
                        let r = self.reg(r, 4);
                        let x = self.fetch8(bus);
                        let x = self.reg(x, 2) as i16 as i32 as u32;
                        Some(r.wrapping_add(x))
                    }
                    0x13 if target => {
                        // LDAR R,PC+dd
                        let d = self.fetch_s16(bus);
                        let a = self.pc.wrapping_add(d);
                        let r = self.fetch8(bus);
                        match r {
                            0x20..=0x27 => self.set_reg(r3(2, r), 2, a),
                            0x30..=0x37 => self.set_reg(r3(4, r), 4, a),
                            _ => self.undefined(bus),
                        }
                        self.clk += 14;
                        None
                    }
                    _ => {
                        self.undefined(bus);
                        None
                    }
                }
            }
            4 | 5 => {
                let d = self.fetch8(bus);
                let r = self.reg(d, 4);
                let step = match d & 3 {
                    0 => 1,
                    1 => 2,
                    2 => 4,
                    _ => 0,
                };
                if mode == 4 {
                    let a = r.wrapping_sub(step);
                    if step != 0 {
                        self.set_reg(d, 4, a);
                    }
                    Some(a)
                } else {
                    if step != 0 {
                        self.set_reg(d, 4, r.wrapping_add(step));
                    }
                    Some(r)
                }
            }
            _ => {
                self.undefined(bus);
                None
            }
        }
    }

    /// The second byte after a register prefix: `r` is the register's code, `sz` its size.
    fn register<B: Bus>(&mut self, bus: &mut B, r: u8, sz: u32) {
        let data = self.fetch8(bus);
        let reg = Loc::Reg(r);
        let (byte, word, long) = (sz == 1, sz == 2, sz == 4);
        match data {
            0x03 => {
                let v = self.fetch(bus, sz);
                self.set_reg(r, sz, v);
            }
            0x04 => self.push_loc(bus, reg, sz),
            0x05 => self.pop_loc(bus, reg, sz),
            0x06 if !long => {
                let v = !self.reg(r, sz);
                self.set_reg(r, sz, v);
                self.set(NF, true);
                self.set(HF, true);
            }
            0x07 if !long => {
                let v = self.reg(r, sz);
                let v = self.sub(0, v, 0, sz);
                self.set_reg(r, sz, v);
            }
            0x08..=0x0B if !long => {
                let v = self.fetch(bus, sz);
                self.clk += 20;
                match data {
                    0x08 => self.mul(bus, r, Loc::Imm(v), sz, false),
                    0x09 => self.mul(bus, r, Loc::Imm(v), sz, true),
                    0x0A => self.div(bus, r, Loc::Imm(v), sz, false),
                    _ => self.div(bus, r, Loc::Imm(v), sz, true),
                }
            }
            0x0C if long => {
                // LINK
                let d = self.fetch_s16(bus);
                self.push_loc(bus, reg, 4);
                let sp = self.r[XSP];
                self.set_reg(r, 4, sp);
                self.r[XSP] = self.r[XSP].wrapping_add(d);
            }
            0x0D if long => {
                // UNLK
                self.r[XSP] = self.reg(r, 4);
                self.pop_loc(bus, reg, 4);
            }
            0x0E | 0x0F if word => {
                let v = self.reg(r, 2);
                let found = if data == 0x0E {
                    (0..16).find(|&i| v >> i & 1 != 0)
                } else {
                    (0..16).rev().find(|&i| v >> i & 1 != 0)
                };
                match found {
                    Some(i) => {
                        self.set(VF, false);
                        self.set_reg(A, 1, i);
                    }
                    None => self.set(VF, true),
                }
            }
            0x10 if byte => self.daa(r),
            0x12 | 0x13 if !byte => {
                let half = sz / 2;
                let v = self.reg(r, half);
                let v = if data == 0x13 { sext(v, half) as u32 } else { v };
                self.set_reg(r, sz, v);
            }
            0x14 if !byte => {
                let v = self.reg(r, sz);
                self.set_reg(r, sz, v.wrapping_add(v & 1));
            }
            0x16 if word => {
                let v = (self.reg(r, 2) as u16).reverse_bits();
                self.set_reg(r, 2, v as u32);
            }
            0x19 if word => {
                // MULA
                let e = expand(r, 2);
                let xde = self.reg(XDE, 4);
                let xhl = self.reg(XHL, 4);
                let a = self.rd(bus, xde, 2) as i16 as i32;
                let b = self.rd(bus, xhl, 2) as i16 as i32;
                let source = self.reg(e, 4);
                let target = a.wrapping_mul(b) as u32;
                let result = source.wrapping_add(target);
                self.set_reg(e, 4, result);
                let xhl = self.reg(XHL, 4);
                self.set_reg(XHL, 4, xhl.wrapping_sub(2));
                let result = self.reg(e, 4);
                self.set(VF, ((target ^ result) & (source ^ result)) >> 31 != 0);
                self.set(ZF, result == 0);
                self.set(SF, result >> 31 != 0);
                self.clk += 30;
            }
            0x1C if !long => {
                // DJNZ
                let d = self.fetch_s8(bus);
                let v = self.reg(r, sz).wrapping_sub(1) & mask(sz);
                self.set_reg(r, sz, v);
                if v != 0 {
                    self.pc = self.pc.wrapping_add(d);
                }
            }
            0x20..=0x24 if !long => {
                let n = self.fetch(bus, 1);
                if data == 0x24 {
                    self.store_carry(bus, reg, n, sz);
                } else {
                    self.carry_op(bus, data & 3, reg, n, false, sz);
                }
            }
            0x28..=0x2C if !long => {
                let n = self.reg(A, 1);
                if data == 0x2C {
                    self.store_carry(bus, reg, n, sz);
                } else {
                    self.carry_op(bus, data & 3, reg, n, true, sz);
                }
            }
            0x2E => {
                let c = self.fetch8(bus);
                self.ld(bus, Loc::Cr(c), reg, sz);
            }
            0x2F => {
                let c = self.fetch8(bus);
                self.ld(bus, reg, Loc::Cr(c), sz);
            }
            0x30..=0x32 | 0x34 if !long => {
                let n = self.fetch(bus, 1);
                let op = if data == 0x34 { 3 } else { data & 3 };
                self.bit_op(bus, op, reg, n, sz);
            }
            0x33 if !long => {
                let n = self.fetch(bus, 1) & 15;
                self.bit(bus, reg, n, sz);
            }
            0x38..=0x3A | 0x3C..=0x3E if word => {
                let n = self.fetch(bus, 2);
                let modulo = 1 << (data & 3);
                let v = self.reg(r, 2);
                let v = if data < 0x3C {
                    if v & n == n { v.wrapping_sub(n) } else { v.wrapping_add(modulo) }
                } else if v & n == n {
                    v.wrapping_add(n)
                } else {
                    v.wrapping_sub(modulo)
                };
                self.set_reg(r, 2, v);
            }
            0x40..=0x5F if !long => {
                self.clk += 20;
                let t = r3(sz, data);
                let signed = data & 8 != 0;
                if data < 0x50 {
                    self.mul(bus, t, reg, sz, signed);
                } else {
                    self.div(bus, t, reg, sz, signed);
                }
            }
            0x60..=0x6F => self.incdec(bus, reg, data, sz, data >= 0x68),
            0x70..=0x7F if !long => {
                let v = self.condition(data & 15) as u32;
                self.set_reg(r, sz, v);
            }
            0x80..=0xC7 | 0xD0..=0xD7 | 0xE0..=0xE7 | 0xF0..=0xF7 => {
                let rr = Loc::Reg(r3(sz, data));
                match data & 0xF8 {
                    0x88 => self.ld(bus, rr, reg, sz),
                    0x98 => self.ld(bus, reg, rr, sz),
                    0xA8 => self.set_reg(r, sz, (data & 7) as u32),
                    0xB8 if long => self.undefined(bus),
                    0xB8 => self.ex(bus, rr, reg, sz),
                    _ => self.alu(bus, (data - 0x80) >> 4, rr, reg, sz),
                }
            }
            0xC8..=0xCF => {
                let v = self.fetch(bus, sz);
                self.alu(bus, data & 7, reg, Loc::Imm(v), sz);
            }
            0xD8..=0xDF if !long => self.alu(bus, 7, reg, Loc::Imm((data & 7) as u32), sz),
            0xE8..=0xEF => {
                let n = self.fetch(bus, 1);
                self.rotate(bus, data & 7, reg, n, sz);
            }
            0xF8..=0xFF => {
                let n = self.reg(A, 1);
                self.rotate(bus, data & 7, reg, n, sz);
            }
            _ => self.undefined(bus),
        }
    }

    /// The second byte after a source-memory prefix: `a` is the address, `sz` the size.
    fn source_memory<B: Bus>(&mut self, bus: &mut B, a: u32, sz: u32) {
        let data = self.fetch8(bus);
        let mem = Loc::Mem(a);
        let (byte, long) = (sz == 1, sz == 4);
        match data {
            0x04 if !long => self.push_loc(bus, mem, sz),
            0x06 | 0x07 if byte => {
                let l = self.reg(A, 1);
                let m = self.rd(bus, a, 1);
                let (l2, m2) = if data == 0x06 {
                    (l & 0xF0 | m >> 4, (m << 4 | l & 15) & 0xFF)
                } else {
                    (l & 0xF0 | m & 15, (l << 4 | m >> 4) & 0xFF)
                };
                self.set_reg(A, 1, l2);
                self.wr(bus, a, 1, m2);
                self.set(NF, false);
                self.set(VF, Self::parity(l2, 1));
                self.set(HF, false);
                self.zs(l2, 1);
            }
            0x10..=0x13 if !long => self.block_ld(bus, sz, data & 2 != 0, data & 1 != 0),
            0x14 | 0x15 if !long => self.block_cp(bus, sz, false, data & 1 != 0),
            0x16 | 0x17 if !long => self.block_cp(bus, sz, true, data & 1 != 0),
            0x19 if !long => {
                let t = self.fetch(bus, 2);
                self.ld(bus, Loc::Mem(t), mem, sz);
            }
            0x20..=0x27 => self.ld(bus, Loc::Reg(r3(sz, data)), mem, sz),
            0x30..=0x37 if !long => self.ex(bus, mem, Loc::Reg(r3(sz, data)), sz),
            0x38..=0x3F if !long => {
                let v = self.fetch(bus, sz);
                self.alu(bus, data & 7, mem, Loc::Imm(v), sz);
            }
            0x40..=0x5F if !long => {
                self.clk += 20;
                let t = r3(sz, data);
                let signed = data & 8 != 0;
                if data < 0x50 {
                    self.mul(bus, t, mem, sz, signed);
                } else {
                    self.div(bus, t, mem, sz, signed);
                }
            }
            0x60..=0x6F if !long => self.incdec(bus, mem, data, sz, data >= 0x68),
            0x78..=0x7F if !long => self.rotate(bus, data & 7, mem, 1, sz),
            0x80..=0xFF => {
                let rr = Loc::Reg(r3(sz, data));
                let op = (data - 0x80) >> 4;
                if data & 8 != 0 {
                    self.alu(bus, op, mem, rr, sz);
                } else {
                    self.alu(bus, op, rr, mem, sz);
                }
            }
            _ => self.undefined(bus),
        }
    }

    /// The second byte after a target-memory prefix: `a` is the address.
    fn target_memory<B: Bus>(&mut self, bus: &mut B, a: u32) {
        let data = self.fetch8(bus);
        let mem = Loc::Mem(a);
        match data {
            0x00 => {
                let v = self.fetch(bus, 1);
                self.wr(bus, a, 1, v);
            }
            0x04 => self.pop_loc(bus, mem, 1),
            0x06 => self.pop_loc(bus, mem, 2),
            0x02 => {
                let v = self.fetch(bus, 2);
                self.wr(bus, a, 2, v);
            }

            0x14 | 0x16 => {
                let sz = if data == 0x14 { 1 } else { 2 };
                let s = self.fetch(bus, 2);
                self.ld(bus, mem, Loc::Mem(s), sz);
            }
            0x20..=0x27 => self.set_reg(r3(2, data), 2, a),
            0x28..=0x2B => {
                let n = self.reg(A, 1);
                self.carry_op(bus, data & 3, mem, n, true, 1);
            }
            0x2C => {
                let n = self.reg(A, 1);
                self.store_carry(bus, mem, n, 1);
            }
            0x30..=0x37 => self.set_reg(r3(4, data), 4, a),
            0x40..=0x47 => self.ld(bus, mem, Loc::Reg(r3(1, data)), 1),
            0x50..=0x57 => self.ld(bus, mem, Loc::Reg(r3(2, data)), 2),
            0x60..=0x67 => self.ld(bus, mem, Loc::Reg(r3(4, data)), 4),
            0x80..=0x9F => self.carry_op(bus, (data >> 3) & 3, mem, (data & 7) as u32, false, 1),
            0xA0..=0xA7 => self.store_carry(bus, mem, (data & 7) as u32, 1),
            0xA8..=0xAF => self.bit_op(bus, 3, mem, (data & 7) as u32, 1),
            0xB0..=0xC7 => self.bit_op(bus, (data - 0xB0) >> 3, mem, (data & 7) as u32, 1),
            0xC8..=0xCF => self.bit(bus, mem, (data & 7) as u32, 1),
            0xD0..=0xDF => {
                if self.condition(data & 15) {
                    self.clk += 6;
                    self.pc = a;
                }
            }
            0xE0..=0xEF => {
                if self.condition(data & 15) {
                    self.clk += 18;
                    self.call(bus, a);
                }
            }
            0xF0..=0xFF => {
                if self.condition(data & 15) {
                    self.clk += 18;
                    self.ret(bus);
                }
            }
            _ => self.undefined(bus),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Checked against the SingleStepTests TLCS-900/H suite
    //! (github.com/SingleStepTests/tlcs900h): set `TLCS900H_TESTS` to its `v1` directory of
    //! `.bin` files (and `TLCS900H_ONLY` to one file's stem to run just that). Registers,
    //! flags, the bank, the interrupt mask, HALT and RAM are compared; the prefetch queue
    //! and bus timing are not modelled.
    use super::*;
    use std::collections::HashMap;

    struct Ram(HashMap<u32, u8>);

    impl Bus for Ram {
        fn read(&mut self, a: u32) -> u8 {
            *self.0.get(&a).unwrap_or(&0)
        }
        fn write(&mut self, a: u32, v: u8) {
            self.0.insert(a, v);
        }
    }

    struct Reader<'a>(&'a [u8]);

    impl Reader<'_> {
        fn u32(&mut self) -> u32 {
            let (h, t) = self.0.split_at(4);
            self.0 = t;
            u32::from_le_bytes(h.try_into().unwrap())
        }
        fn bytes(&mut self) -> Vec<u8> {
            let n = self.u32() as usize;
            let (h, t) = self.0.split_at(n);
            self.0 = t;
            h.to_vec()
        }
        fn state(&mut self) -> Vec<u32> {
            (0..58).map(|_| self.u32()).collect()
        }
        fn ram(&mut self) -> Vec<(u32, u8)> {
            let n = self.u32();
            (0..n).map(|_| (self.u32(), self.u32() as u8)).collect()
        }
        fn skip_cycles(&mut self) {
            let n = self.u32() as usize;
            self.0 = &self.0[n * 28..];
        }
    }

    /// The fields compared: banks, index registers, PC, DMA registers, INTNEST, the 12
    /// flags, RFP, IFF, HALT.
    const COMPARED: usize = 50;

    fn names(i: usize) -> String {
        match i {
            0..=15 => format!("{}{}", ["xwa", "xbc", "xde", "xhl"][i / 4], i % 4),
            16..=20 => ["xix", "xiy", "xiz", "xsp", "pc"][i - 16].into(),
            21..=32 => format!("{}{}", ["dmas", "dmad", "dmam"][(i - 21) / 4], (i - 21) % 4),
            33 => "intnest".into(),
            _ => ["cf", "nf", "vf", "hf", "zf", "sf", "ca", "na", "va", "ha", "za", "sa", "rfp", "iff", "halt", "?"]
                [(i - 34).min(15)]
            .into(),
        }
    }

    const BITS: [u8; 6] = [CF, NF, VF, HF, ZF, SF];

    fn set_state(cpu: &mut Cpu, s: &[u32]) {
        for i in 0..16 {
            cpu.r[(i % 4) * 4 + i / 4] = s[i];
        }
        cpu.r[16..20].copy_from_slice(&s[16..20]);
        cpu.pc = s[20];
        cpu.cr.copy_from_slice(&s[21..34]);
        cpu.f = 0;
        cpu.fp = 0;
        for (k, b) in BITS.iter().enumerate() {
            if s[34 + k] != 0 {
                cpu.f |= b;
            }
            if s[40 + k] != 0 {
                cpu.fp |= b;
            }
        }
        cpu.rfp = s[46] as u8;
        cpu.iff = s[47] as u8;
        cpu.halted = s[48] != 0;
    }

    fn get_state(cpu: &Cpu) -> Vec<u32> {
        let mut s = vec![0; COMPARED];
        for i in 0..16 {
            s[i] = cpu.r[(i % 4) * 4 + i / 4];
        }
        s[16..20].copy_from_slice(&cpu.r[16..20]);
        s[20] = cpu.pc;
        s[21..34].copy_from_slice(&cpu.cr);
        for (k, b) in BITS.iter().enumerate() {
            s[34 + k] = (cpu.f & b != 0) as u32;
            s[40 + k] = (cpu.fp & b != 0) as u32;
        }
        s[46] = cpu.rfp as u32;
        s[47] = cpu.iff as u32;
        s[48] = cpu.halted as u32;
        s
    }

    #[test]
    fn single_step_tests() {
        let Ok(dir) = std::env::var("TLCS900H_TESTS") else {
            return;
        };
        let only = std::env::var("TLCS900H_ONLY").ok();
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".bin"))
            .collect();
        files.sort();
        let (mut failures, mut run, mut failed_files) = (0, 0, 0);
        for f in files {
            let stem = f.trim_end_matches(".bin");
            if only.as_deref().is_some_and(|o| o != stem) {
                continue;
            }
            let data = std::fs::read(format!("{dir}/{f}")).unwrap();
            assert_eq!(&data[..8], b"T9SSTB1\0");
            let mut rd = Reader(&data[8..]);
            rd.u32();
            let count = rd.u32();
            rd.bytes();
            let mut bad = 0;
            for _ in 0..count {
                let name = String::from_utf8(rd.bytes()).unwrap();
                let opcode = rd.bytes();
                let init = rd.state();
                let fin = rd.state();
                let ram0 = rd.ram();
                let ram1 = rd.ram();
                rd.skip_cycles();
                let mut cpu = Cpu::reset();
                set_state(&mut cpu, &init);
                let mut ram = Ram(ram0.into_iter().collect());
                cpu.step(&mut ram);
                let got = get_state(&cpu);
                let diff: Vec<String> = (0..49)
                    .filter(|&i| got[i] != fin[i])
                    .map(|i| format!("{} {:X} want {:X}", names(i), got[i], fin[i]))
                    .collect();
                let wrong_ram: Vec<_> = ram1
                    .iter()
                    .map(|&(a, v)| (a, ram.read(a), v))
                    .filter(|&(_, got, want)| got != want)
                    .take(3)
                    .collect();
                if !diff.is_empty() || !wrong_ram.is_empty() {
                    bad += 1;
                    if bad <= 3 {
                        eprintln!("{name}: {diff:?} ram {wrong_ram:X?}");
                        if std::env::var("TLCS900H_VERBOSE").is_ok() {
                            eprintln!("  opcode {opcode:02X?} init {init:X?}");
                        }
                    }
                }
                run += 1;
            }
            if bad > 0 {
                eprintln!("{stem}: {bad} of {count} failed");
                failures += bad;
                failed_files += 1;
            }
        }
        eprintln!("TLCS-900/H: {} of {run} passed; {failed_files} forms with failures", run - failures);
        assert!(run > 0);
        assert_eq!(failures, 0, "{failures} failures in {run} tests");
    }
}
