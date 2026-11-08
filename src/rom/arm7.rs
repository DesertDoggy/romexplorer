//! An ARM7TDMI interpreter: the Game Boy Advance's CPU, ARM and Thumb instructions, the
//! banked registers of its seven modes, and its exceptions. It runs a game's own code to
//! drive the sound hardware (see [`super::gba_music`]), instruction by instruction, counting
//! roughly what each takes from the wait states the bus reports.
//!
//! Like [`super::m6502`] it keeps a shadow of the subroutine calls in progress, which is how
//! the music search tells which routine wrote to the sound hardware. ARM code returns by
//! jumping to the address the call left in LR, wherever it was saved, so each tracked call
//! remembers where it returns to: any jump there ends it (BX LR, MOV PC,LR, POP {PC}, LDM
//! or LDR into PC). A BL is a call; so is a BX made with LR already pointing just past it
//! (how ARM code calls through a register). Any other BX made from inside a call is a tail
//! call: the call is renamed to its new address. An exception is a call too, ended by the
//! exception return (MOVS PC / SUBS PC / LDM^ restoring CPSR), which also drops whatever the
//! handler left open.

/// What the CPU is attached to. Halfword and word accesses come aligned.
pub trait Bus {
    fn read8(&mut self, addr: u32) -> u8;
    fn read16(&mut self, addr: u32) -> u16;
    fn read32(&mut self, addr: u32) -> u32;
    fn write8(&mut self, addr: u32, v: u8);
    fn write16(&mut self, addr: u32, v: u16);
    fn write32(&mut self, addr: u32, v: u32);
    /// The cycles an access of `size` bytes at `addr` takes (1 with no wait states).
    fn cycles(&self, _addr: u32, _size: u32) -> u32 {
        1
    }
}

pub const MODE_USR: u32 = 0x10;
pub const MODE_FIQ: u32 = 0x11;
pub const MODE_IRQ: u32 = 0x12;
pub const MODE_SVC: u32 = 0x13;
pub const MODE_ABT: u32 = 0x17;
pub const MODE_UND: u32 = 0x1B;
pub const MODE_SYS: u32 = 0x1F;

const N: u32 = 1 << 31;
const Z: u32 = 1 << 30;
const C: u32 = 1 << 29;
const V: u32 = 1 << 28;
/// IRQs off, FIQs off, Thumb.
pub const I: u32 = 1 << 7;
pub const F: u32 = 1 << 6;
pub const T: u32 = 1 << 5;

/// The exception vectors.
pub const VECTOR_UND: u32 = 0x04;
pub const VECTOR_SWI: u32 = 0x08;
pub const VECTOR_IRQ: u32 = 0x18;

/// How deep the shadow of calls in progress goes; deeper calls are not tracked.
const MAX_CALLS: usize = 32;

/// The register bank a mode uses: user/system, FIQ, IRQ, supervisor, abort, undefined.
fn bank_of(mode: u32) -> usize {
    match mode {
        MODE_FIQ => 1,
        MODE_IRQ => 2,
        MODE_SVC => 3,
        MODE_ABT => 4,
        MODE_UND => 5,
        _ => 0,
    }
}

/// A shifter result: the value and the carry out.
fn shift(v: u32, ty: u32, amount: u32, carry: bool, by_register: bool) -> (u32, bool) {
    if by_register {
        if amount == 0 {
            return (v, carry);
        }
        match ty {
            0 => match amount {
                1..=31 => (v << amount, v >> (32 - amount) & 1 != 0),
                32 => (0, v & 1 != 0),
                _ => (0, false),
            },
            1 => match amount {
                1..=31 => (v >> amount, v >> (amount - 1) & 1 != 0),
                32 => (0, v >> 31 != 0),
                _ => (0, false),
            },
            2 => {
                if amount < 32 {
                    (((v as i32) >> amount) as u32, v >> (amount - 1) & 1 != 0)
                } else {
                    let s = ((v as i32) >> 31) as u32;
                    (s, s & 1 != 0)
                }
            }
            _ => {
                let a = amount & 31;
                if a == 0 {
                    (v, v >> 31 != 0)
                } else {
                    (v.rotate_right(a), v >> (a - 1) & 1 != 0)
                }
            }
        }
    } else {
        match ty {
            0 => {
                if amount == 0 {
                    (v, carry)
                } else {
                    (v << amount, v >> (32 - amount) & 1 != 0)
                }
            }
            1 => {
                if amount == 0 {
                    (0, v >> 31 != 0)
                } else {
                    (v >> amount, v >> (amount - 1) & 1 != 0)
                }
            }
            2 => {
                if amount == 0 {
                    let s = ((v as i32) >> 31) as u32;
                    (s, s & 1 != 0)
                } else {
                    (((v as i32) >> amount) as u32, v >> (amount - 1) & 1 != 0)
                }
            }
            _ => {
                if amount == 0 {
                    ((v >> 1) | ((carry as u32) << 31), v & 1 != 0)
                } else {
                    (v.rotate_right(amount), v >> (amount - 1) & 1 != 0)
                }
            }
        }
    }
}

/// a + b + carry, with the carry and overflow out.
fn add(a: u32, b: u32, carry: bool) -> (u32, bool, bool) {
    let r = a as u64 + b as u64 + carry as u64;
    let res = r as u32;
    let v = (a ^ res) & (b ^ res) & 0x8000_0000 != 0;
    (res, r > 0xFFFF_FFFF, v)
}

/// The registers, and the calls in progress.
#[derive(Clone, Debug)]
pub struct Cpu {
    /// The current mode's registers; `r[15]` is the address of the next instruction.
    pub r: [u32; 16],
    pub cpsr: u32,
    /// R13 and R14 of each bank (the current bank's are in `r`).
    banked: [[u32; 2]; 6],
    /// R8-R12 of FIQ mode, and of the other modes while in FIQ mode.
    fiq_hi: [u32; 5],
    usr_hi: [u32; 5],
    /// The saved CPSR of each bank (none for user/system).
    spsr: [u32; 6],
    /// Stopped until an interrupt (the BIOS's Halt).
    pub halted: bool,
    /// The entry points of the subroutines in progress, outermost first, and R13 when each
    /// was entered.
    pub calls: Vec<(u32, u32)>,
    /// Where each call in `calls` returns to.
    rets: Vec<u32>,
    /// How many calls were open when each exception in progress was taken.
    exceptions: Vec<usize>,
    /// The address of the instruction being run.
    now: u32,
    /// Cycles of the instruction being run.
    cyc: u32,
}

impl Cpu {
    /// The CPU after a reset: supervisor mode, ARM, interrupts off, at 0.
    pub fn reset() -> Self {
        Self {
            r: [0; 16],
            cpsr: MODE_SVC | I | F,
            banked: [[0; 2]; 6],
            fiq_hi: [0; 5],
            usr_hi: [0; 5],
            spsr: [0; 6],
            halted: false,
            calls: Vec::new(),
            rets: Vec::new(),
            exceptions: Vec::new(),
            now: 0,
            cyc: 0,
        }
    }

    pub fn thumb(&self) -> bool {
        self.cpsr & T != 0
    }

    pub fn mode(&self) -> u32 {
        self.cpsr & 0x1F
    }

    /// R13 of bank `mode` (whichever is current).
    pub fn sp_of(&self, mode: u32) -> u32 {
        let b = bank_of(mode);
        if b == bank_of(self.mode()) {
            self.r[13]
        } else {
            self.banked[b][0]
        }
    }

    /// Sets R13 of `mode`.
    pub fn set_sp_of(&mut self, mode: u32, v: u32) {
        let b = bank_of(mode);
        if b == bank_of(self.mode()) {
            self.r[13] = v;
        } else {
            self.banked[b][0] = v;
        }
    }

    /// Switches the visible registers to `mode`'s bank.
    pub fn set_mode(&mut self, mode: u32) {
        let old = bank_of(self.mode());
        let new = bank_of(mode);
        if old != new {
            self.banked[old] = [self.r[13], self.r[14]];
            if old == 1 {
                self.fiq_hi.copy_from_slice(&self.r[8..13]);
                self.r[8..13].copy_from_slice(&self.usr_hi);
            }
            if new == 1 {
                self.usr_hi.copy_from_slice(&self.r[8..13]);
                self.r[8..13].copy_from_slice(&self.fiq_hi);
            }
            [self.r[13], self.r[14]] = self.banked[new];
        }
        self.cpsr = (self.cpsr & !0x1F) | (mode & 0x1F);
    }

    fn set_cpsr(&mut self, v: u32) {
        self.set_mode(v & 0x1F);
        self.cpsr = v;
    }

    pub fn spsr(&self) -> Option<u32> {
        let b = bank_of(self.mode());
        (b != 0).then(|| self.spsr[b])
    }

    fn flag(&self, f: u32) -> bool {
        self.cpsr & f != 0
    }

    fn set_nz(&mut self, v: u32) {
        self.cpsr = (self.cpsr & !(N | Z)) | (v & N) | if v == 0 { Z } else { 0 };
    }

    fn set_cv(&mut self, c: bool, v: bool) {
        self.cpsr = (self.cpsr & !(C | V)) | if c { C } else { 0 } | if v { V } else { 0 };
    }

    fn set_c(&mut self, c: bool) {
        self.cpsr = (self.cpsr & !C) | if c { C } else { 0 };
    }

    fn condition(&self, cond: u32) -> bool {
        let (n, z, c, v) = (self.flag(N), self.flag(Z), self.flag(C), self.flag(V));
        match cond {
            0 => z,
            1 => !z,
            2 => c,
            3 => !c,
            4 => n,
            5 => !n,
            6 => v,
            7 => !v,
            8 => c && !z,
            9 => !c || z,
            10 => n == v,
            11 => n != v,
            12 => !z && n == v,
            13 => z || n != v,
            14 => true,
            _ => false,
        }
    }

    /// Register `n` as an ARM instruction reads it (the PC 8 ahead).
    fn reg(&self, n: u32) -> u32 {
        if n == 15 {
            self.now.wrapping_add(8)
        } else {
            self.r[n as usize]
        }
    }

    // Memory, with cycles counted.

    fn ld8<B: Bus>(&mut self, bus: &mut B, a: u32) -> u32 {
        self.cyc += bus.cycles(a, 1);
        bus.read8(a) as u32
    }

    fn ld16<B: Bus>(&mut self, bus: &mut B, a: u32) -> u32 {
        self.cyc += bus.cycles(a, 2);
        bus.read16(a & !1) as u32
    }

    fn ld32<B: Bus>(&mut self, bus: &mut B, a: u32) -> u32 {
        self.cyc += bus.cycles(a, 4);
        bus.read32(a & !3)
    }

    /// A word load as LDR does it: misaligned addresses rotate the word.
    fn ldr<B: Bus>(&mut self, bus: &mut B, a: u32) -> u32 {
        self.ld32(bus, a).rotate_right((a & 3) * 8)
    }

    fn st8<B: Bus>(&mut self, bus: &mut B, a: u32, v: u32) {
        self.cyc += bus.cycles(a, 1);
        bus.write8(a, v as u8)
    }

    fn st16<B: Bus>(&mut self, bus: &mut B, a: u32, v: u32) {
        self.cyc += bus.cycles(a, 2);
        bus.write16(a & !1, v as u16)
    }

    fn st32<B: Bus>(&mut self, bus: &mut B, a: u32, v: u32) {
        self.cyc += bus.cycles(a, 4);
        bus.write32(a & !3, v)
    }

    // The call shadow.

    fn enter(&mut self, target: u32, ret: u32) {
        if self.calls.len() < MAX_CALLS {
            self.calls.push((target & !1, self.r[13]));
            self.rets.push(ret & !1);
        }
    }

    /// A jump to `target` from anything but a call: ends the calls it returns from.
    fn jumped(&mut self, target: u32) {
        let t = target & !1;
        if let Some(i) = self.rets.iter().rposition(|&r| r == t) {
            let floor = self.exceptions.last().copied().unwrap_or(0);
            if i >= floor {
                self.calls.truncate(i);
                self.rets.truncate(i);
            }
        }
    }

    /// A BX: a return, a call through a register, or a tail call.
    fn bx_shadow(&mut self, target: u32, next: u32) {
        let t = target & !1;
        if self.rets.last() == Some(&t) || self.rets.contains(&t) {
            self.jumped(t);
        } else if self.r[14] & !1 == next & !1 {
            self.enter(t, next);
        } else {
            let floor = self.exceptions.last().copied().unwrap_or(0);
            if self.calls.len() > floor {
                if let Some(top) = self.calls.last_mut() {
                    top.0 = t;
                }
            }
        }
    }

    /// An exception return (CPSR restored from SPSR): ends the handler and what it left open.
    fn exception_return(&mut self) {
        if let Some(depth) = self.exceptions.pop() {
            self.calls.truncate(depth);
            self.rets.truncate(depth);
        }
    }

    /// Takes an exception: `mode`, the vector, LR as given.
    fn exception(&mut self, mode: u32, vector: u32, lr: u32) {
        let old = self.cpsr;
        self.set_mode(mode);
        self.spsr[bank_of(mode)] = old;
        self.r[14] = lr;
        self.cpsr = (self.cpsr & !T) | I;
        self.r[15] = vector;
        self.exceptions.push(self.calls.len());
        self.enter(vector, u32::MAX);
        self.cyc += 3;
    }

    /// Takes an IRQ when they are on. Returns the cycles it took (0 if not taken).
    pub fn irq(&mut self) -> u32 {
        self.halted = false;
        if self.flag(I) {
            return 0;
        }
        let next = self.r[15];
        self.cyc = 0;
        self.exception(MODE_IRQ, VECTOR_IRQ, next.wrapping_add(4));
        self.cyc
    }

    /// Returns from a routine run natively (a BIOS stand-in): to LR, as BX LR would.
    pub fn hle_return(&mut self) {
        let lr = self.r[14];
        if lr & 1 != 0 {
            self.cpsr |= T;
            self.r[15] = lr & !1;
        } else {
            self.cpsr &= !T;
            self.r[15] = lr & !3;
        }
        self.jumped(lr);
    }

    /// Returns from an exception run natively: MOVS PC, LR.
    pub fn hle_exception_return(&mut self) {
        let lr = self.r[14];
        if let Some(s) = self.spsr() {
            self.set_cpsr(s);
        }
        self.r[15] = if self.thumb() { lr & !1 } else { lr & !3 };
        self.exception_return();
    }

    /// The routine the CPU is in: where it returns to, and whether it is an exception
    /// handler (ended by the exception return instead).
    pub fn top_call(&self) -> Option<(u32, bool)> {
        let n = self.calls.len();
        let ret = *self.rets.last()?;
        Some((ret, self.exceptions.last() == Some(&(n - 1))))
    }

    /// Tracks the routine at the PC as the only call in progress, returning to `ret` (or by
    /// the exception return when `exception`), as [`Self::top_call`] described it.
    pub fn begin_call(&mut self, ret: u32, exception: bool) {
        self.calls = vec![(self.r[15] & !1, self.r[13])];
        self.rets = vec![ret & !1];
        self.exceptions = if exception { vec![0] } else { Vec::new() };
    }

    /// Writes the PC from an instruction other than a call.
    fn write_pc(&mut self, v: u32) {
        // ARM state keeps the low bits (instruction fetches ignore them).
        self.r[15] = if self.thumb() { v & !1 } else { v };
        self.cyc += 2;
        self.jumped(v);
    }

    /// Runs one instruction (or waits while halted). Returns the cycles taken.
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> u32 {
        if self.halted {
            return 1;
        }
        self.cyc = 0;
        if self.thumb() {
            let pc = self.r[15] & !1;
            self.cyc += bus.cycles(pc, 2);
            let op = bus.read16(pc);
            self.thumb_op(bus, op);
        } else {
            let pc = self.r[15] & !3;
            self.cyc += bus.cycles(pc, 4);
            let op = bus.read32(pc);
            self.arm_op(bus, op);
        }
        self.cyc.max(1)
    }

    /// Runs `op` as the ARM instruction at R15.
    pub fn arm_op<B: Bus>(&mut self, bus: &mut B, op: u32) {
        self.now = self.r[15];
        self.r[15] = self.now.wrapping_add(4);
        if !self.condition(op >> 28) {
            return;
        }
        self.arm(bus, op);
    }

    /// Runs `op` as the Thumb instruction at R15.
    pub fn thumb_op<B: Bus>(&mut self, bus: &mut B, op: u16) {
        self.now = self.r[15];
        self.r[15] = self.now.wrapping_add(2);
        self.thumb_exec(bus, op);
    }

    fn undefined(&mut self) {
        let next = self.r[15];
        self.exception(MODE_UND, VECTOR_UND, next);
    }

    fn arm<B: Bus>(&mut self, bus: &mut B, op: u32) {
        if op & 0x0FFF_FFF0 == 0x012F_FF10 {
            // BX
            let v = self.reg(op & 15);
            if v & 1 != 0 {
                self.cpsr |= T;
            }
            self.r[15] = v & !1;
            self.cyc += 2;
            let next = self.now.wrapping_add(4);
            self.bx_shadow(v, next);
            return;
        }
        match (op >> 25) & 7 {
            0 => {
                if op & 0x90 == 0x90 {
                    // Multiplies, swaps and halfword transfers.
                    if op & 0x60 == 0 {
                        if op & 0x0FC0_0000 == 0 {
                            self.mul(op);
                        } else if op & 0x0F80_0000 == 0x0080_0000 {
                            self.mull(op);
                        } else if op & 0x0FB0_0FF0 == 0x0100_0090 {
                            self.swp(bus, op);
                        } else {
                            self.undefined();
                        }
                    } else {
                        self.halfword(bus, op);
                    }
                } else if op & 0x0190_0000 == 0x0100_0000 {
                    self.psr(op);
                } else {
                    self.data(op);
                }
            }
            1 => {
                if op & 0x0190_0000 == 0x0100_0000 {
                    self.psr(op);
                } else {
                    self.data(op);
                }
            }
            2 | 3 => {
                if op & 0x0200_0010 == 0x0200_0010 {
                    self.undefined();
                } else {
                    self.single(bus, op);
                }
            }
            4 => self.block(bus, op),
            5 => {
                let off = (((op & 0xFF_FFFF) << 8) as i32 >> 6) as u32;
                let target = self.now.wrapping_add(8).wrapping_add(off);
                self.r[15] = target;
                self.cyc += 2;
                if op & (1 << 24) != 0 {
                    let next = self.now.wrapping_add(4);
                    self.r[14] = next;
                    self.enter(target, next);
                }
            }
            6 => self.undefined(),
            _ => {
                if op & (1 << 24) != 0 {
                    let next = self.now.wrapping_add(4);
                    self.exception(MODE_SVC, VECTOR_SWI, next);
                } else {
                    self.undefined();
                }
            }
        }
    }

    /// MRS and MSR.
    fn psr(&mut self, op: u32) {
        let spsr = op & (1 << 22) != 0;
        if op & (1 << 21) == 0 {
            // MRS
            let v = if spsr {
                self.spsr().unwrap_or(self.cpsr)
            } else {
                self.cpsr
            };
            self.r[((op >> 12) & 15) as usize] = v;
            return;
        }
        let v = if op & (1 << 25) != 0 {
            (op & 0xFF).rotate_right(((op >> 8) & 15) * 2)
        } else {
            self.reg(op & 15)
        };
        let mut mask = 0u32;
        for (bit, m) in [(16, 0xFF), (17, 0xFF00), (18, 0xFF_0000), (19, 0xFF00_0000)] {
            if op & (1 << bit) != 0 {
                mask |= m;
            }
        }
        if spsr {
            let b = bank_of(self.mode());
            if b != 0 {
                self.spsr[b] = (self.spsr[b] & !mask) | (v & mask);
            }
        } else {
            if self.mode() == MODE_USR {
                mask &= 0xFF00_0000;
            }
            let new = (self.cpsr & !mask) | (v & mask) | 0x10;
            self.set_cpsr(new);
        }
    }

    fn data(&mut self, op: u32) {
        let opc = (op >> 21) & 15;
        let s = op & (1 << 20) != 0;
        let rn = (op >> 16) & 15;
        let rd = (op >> 12) & 15;
        let carry = self.flag(C);
        let by_reg = op & (1 << 25) == 0 && op & 0x10 != 0;
        let (b, sc) = if op & (1 << 25) != 0 {
            let rot = ((op >> 8) & 15) * 2;
            let v = (op & 0xFF).rotate_right(rot);
            (v, if rot == 0 { carry } else { v >> 31 != 0 })
        } else {
            let rm = op & 15;
            let ty = (op >> 5) & 3;
            if by_reg {
                self.cyc += 1;
                let amount = self.reg((op >> 8) & 15) & 0xFF;
                let v = if rm == 15 {
                    self.now.wrapping_add(12)
                } else {
                    self.r[rm as usize]
                };
                shift(v, ty, amount, carry, true)
            } else {
                shift(self.reg(rm), ty, (op >> 7) & 31, carry, false)
            }
        };
        let a = if rn == 15 {
            self.now.wrapping_add(if by_reg { 12 } else { 8 })
        } else {
            self.r[rn as usize]
        };
        let (res, arith) = match opc {
            0 | 8 => (a & b, None),
            1 | 9 => (a ^ b, None),
            2 | 10 => {
                let (r, c, v) = add(a, !b, true);
                (r, Some((c, v)))
            }
            3 => {
                let (r, c, v) = add(b, !a, true);
                (r, Some((c, v)))
            }
            4 | 11 => {
                let (r, c, v) = add(a, b, false);
                (r, Some((c, v)))
            }
            5 => {
                let (r, c, v) = add(a, b, carry);
                (r, Some((c, v)))
            }
            6 => {
                let (r, c, v) = add(a, !b, carry);
                (r, Some((c, v)))
            }
            7 => {
                let (r, c, v) = add(b, !a, carry);
                (r, Some((c, v)))
            }
            12 => (a | b, None),
            13 => (b, None),
            14 => (a & !b, None),
            _ => (!b, None),
        };
        let writes = !(8..=11).contains(&opc);
        if s && rd == 15 && self.spsr().is_some() {
            // The exception return: CPSR from SPSR.
            if let Some(sp) = self.spsr() {
                self.set_cpsr(sp);
            }
            if writes {
                self.r[15] = if self.thumb() { res & !1 } else { res };
                self.cyc += 2;
            }
            self.exception_return();
            return;
        }
        if s {
            self.set_nz(res);
            match arith {
                Some((c, v)) => self.set_cv(c, v),
                None => self.set_c(sc),
            }
        }
        if writes {
            if rd == 15 {
                self.write_pc(res);
            } else {
                self.r[rd as usize] = res;
            }
        }
    }

    /// A register as multiplies and swaps read it (the PC 12 ahead).
    fn mreg(&self, n: u32) -> u32 {
        if n == 15 {
            self.now.wrapping_add(12)
        } else {
            self.r[n as usize]
        }
    }

    fn mul(&mut self, op: u32) {
        let rd = ((op >> 16) & 15) as usize;
        let rs = self.mreg((op >> 8) & 15);
        let rm = self.mreg(op & 15);
        let acc = op & (1 << 21) != 0;
        let add_in = if acc { self.mreg((op >> 12) & 15) } else { 0 };
        let res = rm.wrapping_mul(rs).wrapping_add(add_in);
        self.cyc += booth_cycles(rs, true) + acc as u32;
        if op & (1 << 20) != 0 {
            self.set_nz(res);
        }
        if rd == 15 {
            self.write_pc(res);
        } else {
            self.r[rd] = res;
        }
    }

    fn mull(&mut self, op: u32) {
        let hi = ((op >> 16) & 15) as usize;
        let lo = ((op >> 12) & 15) as usize;
        let rs = self.mreg((op >> 8) & 15);
        let rm = self.mreg(op & 15);
        let signed = op & (1 << 22) != 0;
        let acc = op & (1 << 21) != 0;
        let mut res = if signed {
            (rm as i32 as i64).wrapping_mul(rs as i32 as i64) as u64
        } else {
            rm as u64 * rs as u64
        };
        if acc {
            res = res.wrapping_add((self.mreg(hi as u32) as u64) << 32 | self.mreg(lo as u32) as u64);
        }
        self.cyc += booth_cycles(rs, signed) + 1 + acc as u32;
        if op & (1 << 20) != 0 {
            self.cpsr = (self.cpsr & !(N | Z))
                | if res >> 63 != 0 { N } else { 0 }
                | if res == 0 { Z } else { 0 };
        }
        self.r[lo] = res as u32;
        self.r[hi] = (res >> 32) as u32;
    }

    fn swp<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let addr = self.mreg((op >> 16) & 15);
        let rm = self.mreg(op & 15);
        let rd = ((op >> 12) & 15) as usize;
        let v = if op & (1 << 22) != 0 {
            let v = self.ld8(bus, addr);
            self.st8(bus, addr, rm);
            v
        } else {
            let v = self.ldr(bus, addr);
            self.st32(bus, addr, rm);
            v
        };
        self.cyc += 1;
        if rd == 15 {
            self.write_pc(v);
        } else {
            self.r[rd] = v;
        }
    }

    /// LDRH, STRH, LDRSB, LDRSH.
    fn halfword<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let p = op & (1 << 24) != 0;
        let u = op & (1 << 23) != 0;
        let w = op & (1 << 21) != 0;
        let l = op & (1 << 20) != 0;
        let rn = (op >> 16) & 15;
        let rd = (op >> 12) & 15;
        let off = if op & (1 << 22) != 0 {
            ((op >> 4) & 0xF0) | (op & 0xF)
        } else {
            self.reg(op & 15)
        };
        let base = self.reg(rn);
        let moved = if u {
            base.wrapping_add(off)
        } else {
            base.wrapping_sub(off)
        };
        let addr = if p { moved } else { base };
        let sh = (op >> 5) & 3;
        if l {
            let v = match sh {
                1 => self.ld16(bus, addr).rotate_right((addr & 1) * 8),
                2 => self.ld8(bus, addr) as u8 as i8 as i32 as u32,
                _ => {
                    if addr & 1 != 0 {
                        // Misaligned: the halfword's high byte, sign-extended.
                        (self.ld16(bus, addr) >> 8) as u8 as i8 as i32 as u32
                    } else {
                        self.ld16(bus, addr) as u16 as i16 as i32 as u32
                    }
                }
            };
            self.cyc += 1;
            if !p || w {
                self.writeback(rn, moved);
            }
            if rd == 15 {
                self.write_pc(v);
            } else {
                self.r[rd as usize] = v;
            }
        } else {
            let v = if rd == 15 {
                self.now.wrapping_add(12)
            } else {
                self.r[rd as usize]
            };
            if sh == 1 {
                self.st16(bus, addr, v);
            }
            if !p || w {
                self.writeback(rn, moved);
            }
        }
    }

    /// LDR, STR, LDRB, STRB.
    fn single<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let p = op & (1 << 24) != 0;
        let u = op & (1 << 23) != 0;
        let byte = op & (1 << 22) != 0;
        let w = op & (1 << 21) != 0;
        let l = op & (1 << 20) != 0;
        let rn = (op >> 16) & 15;
        let rd = (op >> 12) & 15;
        let off = if op & (1 << 25) == 0 {
            op & 0xFFF
        } else {
            let carry = self.flag(C);
            shift(self.reg(op & 15), (op >> 5) & 3, (op >> 7) & 31, carry, false).0
        };
        let base = self.reg(rn);
        let moved = if u {
            base.wrapping_add(off)
        } else {
            base.wrapping_sub(off)
        };
        let addr = if p { moved } else { base };
        if l {
            let v = if byte {
                self.ld8(bus, addr)
            } else {
                self.ldr(bus, addr)
            };
            self.cyc += 1;
            if !p || w {
                self.writeback(rn, moved);
            }
            if rd == 15 {
                self.write_pc(v);
            } else {
                self.r[rd as usize] = v;
            }
        } else {
            let v = if rd == 15 {
                self.now.wrapping_add(12)
            } else {
                self.r[rd as usize]
            };
            if byte {
                self.st8(bus, addr, v);
            } else {
                self.st32(bus, addr, v);
            }
            if !p || w {
                self.writeback(rn, moved);
            }
        }
    }

    fn writeback(&mut self, rn: u32, v: u32) {
        if rn == 15 {
            self.write_pc(v);
        } else {
            self.r[rn as usize] = v;
        }
    }

    /// A user-bank register, for LDM/STM with the S bit.
    fn user_reg(&self, n: usize) -> u32 {
        let b = bank_of(self.mode());
        match n {
            8..=12 if b == 1 => self.usr_hi[n - 8],
            13 | 14 if b != 0 => self.banked[0][n - 13],
            _ => self.r[n],
        }
    }

    fn set_user_reg(&mut self, n: usize, v: u32) {
        let b = bank_of(self.mode());
        match n {
            8..=12 if b == 1 => self.usr_hi[n - 8] = v,
            13 | 14 if b != 0 => self.banked[0][n - 13] = v,
            _ => self.r[n] = v,
        }
    }

    /// LDM and STM.
    fn block<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let p = op & (1 << 24) != 0;
        let u = op & (1 << 23) != 0;
        let s = op & (1 << 22) != 0;
        let w = op & (1 << 21) != 0;
        let l = op & (1 << 20) != 0;
        let rn = ((op >> 16) & 15) as usize;
        let list = op & 0xFFFF;
        let base = self.reg(rn as u32);
        let bytes = if list == 0 { 0x40 } else { list.count_ones() * 4 };
        let mut addr = match (p, u) {
            (false, true) => base,
            (true, true) => base.wrapping_add(4),
            (false, false) => base.wrapping_sub(bytes).wrapping_add(4),
            (true, false) => base.wrapping_sub(bytes),
        };
        let new_base = if u {
            base.wrapping_add(bytes)
        } else {
            base.wrapping_sub(bytes)
        };
        let regs: Vec<usize> = if list == 0 {
            vec![15]
        } else {
            (0..16).filter(|i| list & (1 << i) != 0).collect()
        };
        let user = s && !(l && list & 0x8000 != 0);
        if l {
            if w {
                if user {
                    self.set_user_reg(rn, new_base);
                } else {
                    self.r[rn] = new_base;
                }
            }
            let mut pc = None;
            for &reg in &regs {
                let v = self.ld32(bus, addr);
                addr = addr.wrapping_add(4);
                if reg == 15 {
                    pc = Some(v);
                } else if user {
                    self.set_user_reg(reg, v);
                } else {
                    self.r[reg] = v;
                }
            }
            self.cyc += 1;
            if let Some(v) = pc {
                if s {
                    if let Some(sp) = self.spsr() {
                        self.set_cpsr(sp);
                    }
                    self.r[15] = if self.thumb() { v & !1 } else { v };
                    self.cyc += 2;
                    self.exception_return();
                } else {
                    self.write_pc(v);
                }
            }
        } else {
            for (i, &reg) in regs.iter().enumerate() {
                let v = if reg == 15 {
                    self.now.wrapping_add(12)
                } else if reg == rn && i > 0 && w {
                    new_base
                } else if user {
                    self.user_reg(reg)
                } else {
                    self.r[reg]
                };
                self.st32(bus, addr, v);
                addr = addr.wrapping_add(4);
            }
            if w {
                if user {
                    self.set_user_reg(rn, new_base);
                } else {
                    self.r[rn] = new_base;
                }
            }
        }
    }

    // Thumb.

    /// A low register's value, or the PC as Thumb reads it.
    fn treg(&self, n: u32) -> u32 {
        if n == 15 {
            self.now.wrapping_add(4)
        } else {
            self.r[n as usize]
        }
    }

    fn thumb_exec<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let op = op as u32;
        let carry = self.flag(C);
        match op >> 11 {
            // LSL, LSR, ASR by an immediate.
            0..=2 => {
                let rs = self.r[((op >> 3) & 7) as usize];
                let (v, c) = shift(rs, op >> 11, (op >> 6) & 31, carry, false);
                self.r[(op & 7) as usize] = v;
                self.set_nz(v);
                self.set_c(c);
            }
            // ADD/SUB register or 3-bit immediate.
            3 => {
                let a = self.r[((op >> 3) & 7) as usize];
                let b = if op & (1 << 10) != 0 {
                    (op >> 6) & 7
                } else {
                    self.r[((op >> 6) & 7) as usize]
                };
                let (r, c, v) = if op & (1 << 9) != 0 {
                    add(a, !b, true)
                } else {
                    add(a, b, false)
                };
                self.r[(op & 7) as usize] = r;
                self.set_nz(r);
                self.set_cv(c, v);
            }
            // MOV/CMP/ADD/SUB with an 8-bit immediate.
            4..=7 => {
                let rd = ((op >> 8) & 7) as usize;
                let imm = op & 0xFF;
                let a = self.r[rd];
                match (op >> 11) & 3 {
                    0 => {
                        self.r[rd] = imm;
                        self.set_nz(imm);
                    }
                    1 => {
                        let (r, c, v) = add(a, !imm, true);
                        self.set_nz(r);
                        self.set_cv(c, v);
                    }
                    2 => {
                        let (r, c, v) = add(a, imm, false);
                        self.r[rd] = r;
                        self.set_nz(r);
                        self.set_cv(c, v);
                    }
                    _ => {
                        let (r, c, v) = add(a, !imm, true);
                        self.r[rd] = r;
                        self.set_nz(r);
                        self.set_cv(c, v);
                    }
                }
            }
            8 => {
                if op & (1 << 10) == 0 {
                    self.thumb_alu(op);
                } else {
                    self.thumb_hi(op);
                }
            }
            // LDR PC-relative.
            9 => {
                let addr = (self.now.wrapping_add(4) & !2).wrapping_add((op & 0xFF) * 4);
                let v = self.ld32(bus, addr);
                self.cyc += 1;
                self.r[((op >> 8) & 7) as usize] = v;
            }
            // Loads and stores with a register offset.
            10 | 11 => {
                let addr = self.r[((op >> 3) & 7) as usize]
                    .wrapping_add(self.r[((op >> 6) & 7) as usize]);
                let rd = (op & 7) as usize;
                match (op >> 9) & 7 {
                    0 => self.st32(bus, addr, self.r[rd]),
                    1 => self.st16(bus, addr, self.r[rd]),
                    2 => self.st8(bus, addr, self.r[rd]),
                    3 => {
                        self.r[rd] = self.ld8(bus, addr) as u8 as i8 as i32 as u32;
                        self.cyc += 1;
                    }
                    4 => {
                        self.r[rd] = self.ldr(bus, addr);
                        self.cyc += 1;
                    }
                    5 => {
                        self.r[rd] = self.ld16(bus, addr).rotate_right((addr & 1) * 8);
                        self.cyc += 1;
                    }
                    6 => {
                        self.r[rd] = self.ld8(bus, addr);
                        self.cyc += 1;
                    }
                    _ => {
                        self.r[rd] = if addr & 1 != 0 {
                            (self.ld16(bus, addr) >> 8) as u8 as i8 as i32 as u32
                        } else {
                            self.ld16(bus, addr) as u16 as i16 as i32 as u32
                        };
                        self.cyc += 1;
                    }
                }
            }
            // Loads and stores with a 5-bit immediate offset: words, bytes.
            12..=15 => {
                let byte = op & (1 << 12) != 0;
                let off = (op >> 6) & 31;
                let base = self.r[((op >> 3) & 7) as usize];
                let rd = (op & 7) as usize;
                let addr = base.wrapping_add(if byte { off } else { off * 4 });
                match (byte, op & (1 << 11) != 0) {
                    (false, false) => self.st32(bus, addr, self.r[rd]),
                    (false, true) => {
                        self.r[rd] = self.ldr(bus, addr);
                        self.cyc += 1;
                    }
                    (true, false) => self.st8(bus, addr, self.r[rd]),
                    (true, true) => {
                        self.r[rd] = self.ld8(bus, addr);
                        self.cyc += 1;
                    }
                }
            }
            // Halfwords with an immediate offset.
            16 | 17 => {
                let addr = self.r[((op >> 3) & 7) as usize].wrapping_add(((op >> 6) & 31) * 2);
                let rd = (op & 7) as usize;
                if op & (1 << 11) != 0 {
                    self.r[rd] = self.ld16(bus, addr).rotate_right((addr & 1) * 8);
                    self.cyc += 1;
                } else {
                    self.st16(bus, addr, self.r[rd]);
                }
            }
            // SP-relative.
            18 | 19 => {
                let addr = self.r[13].wrapping_add((op & 0xFF) * 4);
                let rd = ((op >> 8) & 7) as usize;
                if op & (1 << 11) != 0 {
                    self.r[rd] = self.ldr(bus, addr);
                    self.cyc += 1;
                } else {
                    self.st32(bus, addr, self.r[rd]);
                }
            }
            // ADD Rd, PC/SP, #imm.
            20 | 21 => {
                let base = if op & (1 << 11) != 0 {
                    self.r[13]
                } else {
                    self.now.wrapping_add(4) & !2
                };
                self.r[((op >> 8) & 7) as usize] = base.wrapping_add((op & 0xFF) * 4);
            }
            22 | 23 => {
                if op & 0x0F00 == 0 {
                    // ADD SP, #±imm
                    let imm = (op & 0x7F) * 4;
                    self.r[13] = if op & 0x80 != 0 {
                        self.r[13].wrapping_sub(imm)
                    } else {
                        self.r[13].wrapping_add(imm)
                    };
                } else if op & 0x0600 == 0x0400 {
                    self.push_pop(bus, op);
                } else {
                    self.undefined();
                }
            }
            // STMIA/LDMIA.
            24 | 25 => {
                let rb = ((op >> 8) & 7) as usize;
                let list = op & 0xFF;
                let base = self.r[rb];
                let l = op & (1 << 11) != 0;
                if list == 0 {
                    // The ARM7TDMI moves the PC, and steps the base by 0x40.
                    if l {
                        let v = self.ld32(bus, base);
                        self.r[rb] = base.wrapping_add(0x40);
                        self.write_pc(v);
                        self.r[15] = v;
                    } else {
                        let v = self.now.wrapping_add(6);
                        self.st32(bus, base, v);
                        self.r[rb] = base.wrapping_add(0x40);
                    }
                    return;
                }
                let new_base = base.wrapping_add(list.count_ones() * 4);
                let mut addr = base;
                if l {
                    self.r[rb] = new_base;
                    for i in 0..8 {
                        if list & (1 << i) != 0 {
                            self.r[i] = self.ld32(bus, addr);
                            addr = addr.wrapping_add(4);
                        }
                    }
                    self.cyc += 1;
                } else {
                    let mut first = true;
                    for i in 0..8 {
                        if list & (1 << i) != 0 {
                            let v = if i == rb && !first {
                                new_base
                            } else {
                                self.r[i]
                            };
                            self.st32(bus, addr, v);
                            addr = addr.wrapping_add(4);
                            first = false;
                        }
                    }
                    self.r[rb] = new_base;
                }
            }
            // Conditional branches, SWI.
            26 | 27 => {
                let cond = (op >> 8) & 15;
                if cond == 15 {
                    let next = self.now.wrapping_add(2);
                    self.exception(MODE_SVC, VECTOR_SWI, next);
                } else if self.condition(cond) {
                    let off = ((op & 0xFF) as u8 as i8 as i32 * 2) as u32;
                    self.r[15] = self.now.wrapping_add(4).wrapping_add(off);
                    self.cyc += 2;
                }
            }
            28 => {
                let off = (((op & 0x7FF) << 21) as i32 >> 20) as u32;
                self.r[15] = self.now.wrapping_add(4).wrapping_add(off);
                self.cyc += 2;
            }
            // BLX suffix (ARMv5): undefined here.
            29 => self.undefined(),
            30 => {
                let off = (((op & 0x7FF) << 21) as i32 >> 9) as u32;
                self.r[14] = self.now.wrapping_add(4).wrapping_add(off);
            }
            _ => {
                let target = self.r[14].wrapping_add((op & 0x7FF) * 2);
                let next = self.now.wrapping_add(2);
                self.r[14] = next | 1;
                self.r[15] = target & !1;
                self.cyc += 2;
                self.enter(target, next);
            }
        }
    }

    fn thumb_alu(&mut self, op: u32) {
        let rd = (op & 7) as usize;
        let rs = self.r[((op >> 3) & 7) as usize];
        let a = self.r[rd];
        let carry = self.flag(C);
        let shifted = |ty| shift(a, ty, rs & 0xFF, carry, true);
        match (op >> 6) & 15 {
            0 => {
                self.r[rd] = a & rs;
                self.set_nz(a & rs);
            }
            1 => {
                self.r[rd] = a ^ rs;
                self.set_nz(a ^ rs);
            }
            n @ (2 | 3 | 4 | 7) => {
                let ty = match n {
                    2 => 0,
                    3 => 1,
                    4 => 2,
                    _ => 3,
                };
                let (v, c) = shifted(ty);
                self.r[rd] = v;
                self.set_nz(v);
                self.set_c(c);
                self.cyc += 1;
            }
            5 => {
                let (r, c, v) = add(a, rs, carry);
                self.r[rd] = r;
                self.set_nz(r);
                self.set_cv(c, v);
            }
            6 => {
                let (r, c, v) = add(a, !rs, carry);
                self.r[rd] = r;
                self.set_nz(r);
                self.set_cv(c, v);
            }
            8 => self.set_nz(a & rs),
            9 => {
                let (r, c, v) = add(0, !rs, true);
                self.r[rd] = r;
                self.set_nz(r);
                self.set_cv(c, v);
            }
            10 => {
                let (r, c, v) = add(a, !rs, true);
                self.set_nz(r);
                self.set_cv(c, v);
            }
            11 => {
                let (r, c, v) = add(a, rs, false);
                self.set_nz(r);
                self.set_cv(c, v);
            }
            12 => {
                self.r[rd] = a | rs;
                self.set_nz(a | rs);
            }
            13 => {
                let r = a.wrapping_mul(rs);
                self.r[rd] = r;
                self.set_nz(r);
                self.cyc += booth_cycles(a, true);
            }
            14 => {
                self.r[rd] = a & !rs;
                self.set_nz(a & !rs);
            }
            _ => {
                self.r[rd] = !rs;
                self.set_nz(!rs);
            }
        }
    }

    /// ADD/CMP/MOV with high registers, and BX.
    fn thumb_hi(&mut self, op: u32) {
        let rd = (op & 7) | ((op >> 4) & 8);
        let rs = (op >> 3) & 15;
        let b = self.treg(rs);
        match (op >> 8) & 3 {
            0 => {
                let r = self.treg(rd).wrapping_add(b);
                if rd == 15 {
                    self.write_pc(r);
                } else {
                    self.r[rd as usize] = r;
                }
            }
            1 => {
                let (r, c, v) = add(self.treg(rd), !b, true);
                self.set_nz(r);
                self.set_cv(c, v);
            }
            2 => {
                if rd == 15 {
                    self.write_pc(b);
                } else {
                    self.r[rd as usize] = b;
                }
            }
            _ => {
                if b & 1 == 0 {
                    self.cpsr &= !T;
                }
                self.r[15] = b & !1;
                self.cyc += 2;
                let next = self.now.wrapping_add(2);
                self.bx_shadow(b, next | 1);
            }
        }
    }

    /// PUSH and POP.
    fn push_pop<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let pop = op & (1 << 11) != 0;
        let extra = op & (1 << 8) != 0;
        let list = op & 0xFF;
        let n = list.count_ones() + extra as u32;
        if pop {
            let mut addr = self.r[13];
            if n == 0 {
                // The ARM7TDMI pops the PC, and steps SP by 0x40.
                let v = self.ld32(bus, addr);
                self.r[13] = addr.wrapping_add(0x40);
                self.write_pc(v);
                self.r[15] = v;
                return;
            }
            for i in 0..8 {
                if list & (1 << i) != 0 {
                    self.r[i] = self.ld32(bus, addr);
                    addr = addr.wrapping_add(4);
                }
            }
            self.cyc += 1;
            if extra {
                let v = self.ld32(bus, addr);
                addr = addr.wrapping_add(4);
                self.r[13] = addr;
                self.write_pc(v);
            } else {
                self.r[13] = addr;
            }
        } else {
            if n == 0 {
                let addr = self.r[13].wrapping_sub(0x40);
                let v = self.now.wrapping_add(6);
                self.st32(bus, addr, v);
                self.r[13] = addr;
                return;
            }
            let mut addr = self.r[13].wrapping_sub(n * 4);
            self.r[13] = addr;
            for i in 0..8 {
                if list & (1 << i) != 0 {
                    self.st32(bus, addr, self.r[i]);
                    addr = addr.wrapping_add(4);
                }
            }
            if extra {
                self.st32(bus, addr, self.r[14]);
            }
        }
    }

    /// The registers as a test or a save sees them: the current mode's, then each bank.
    pub fn bank_regs(&self, mode: u32) -> [u32; 7] {
        let b = bank_of(mode);
        let mut out = [0; 7];
        if b == 1 {
            out[..5].copy_from_slice(&self.fiq_hi);
        } else {
            out[..5].copy_from_slice(&self.usr_hi);
        }
        out[5] = self.banked[b][0];
        out[6] = self.banked[b][1];
        out
    }
}

/// Internal cycles the multiplier takes for multiplier `rs`.
fn booth_cycles(rs: u32, signed: bool) -> u32 {
    let mut m = 4;
    for i in 1..4 {
        let top = rs >> (i * 8);
        if top == 0 || (signed && top == (u32::MAX >> (i * 8))) {
            m = i;
            break;
        }
    }
    m
}

#[cfg(test)]
mod tests {
    //! Checked against the SingleStepTests ARM7TDMI suite
    //! (github.com/SingleStepTests/ARM7TDMI, made with NanoBoyAdvance): set
    //! `ARM7TDMI_TESTS` to its `v1` directory (and `ARM7TDMI_ONLY` to one file's name to run
    //! just that one). Each test's data reads come from its list of bus transactions.
    use super::*;

    struct TestBus {
        reads: Vec<(u32, u32, u32)>,
        writes: Vec<(u32, u32, u32)>,
    }

    impl TestBus {
        fn find(&self, addr: u32, size: u32) -> u32 {
            for &(a, s, d) in &self.reads {
                if a <= addr && addr + size <= a + s {
                    return d >> ((addr - a) * 8);
                }
            }
            for &(a, _, d) in &self.reads {
                if a & !3 == addr & !3 {
                    return d;
                }
            }
            0
        }
    }

    impl Bus for TestBus {
        fn read8(&mut self, a: u32) -> u8 {
            self.find(a, 1) as u8
        }
        fn read16(&mut self, a: u32) -> u16 {
            self.find(a, 2) as u16
        }
        fn read32(&mut self, a: u32) -> u32 {
            self.find(a, 4)
        }
        fn write8(&mut self, a: u32, v: u8) {
            self.writes.push((a, 1, v as u32));
        }
        fn write16(&mut self, a: u32, v: u16) {
            self.writes.push((a, 2, v as u32));
        }
        fn write32(&mut self, a: u32, v: u32) {
            self.writes.push((a, 4, v));
        }
    }

    struct State {
        r: [u32; 16],
        fiq: [u32; 7],
        svc: [u32; 2],
        abt: [u32; 2],
        irq: [u32; 2],
        und: [u32; 2],
        cpsr: u32,
        spsr: [u32; 5],
        pipeline: [u32; 2],
    }

    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
    }

    fn state(b: &[u8], at: usize) -> (usize, State) {
        let size = u32_at(b, at) as usize;
        let v: Vec<u32> = (0..40).map(|i| u32_at(b, at + 8 + i * 4)).collect();
        let mut s = State {
            r: [0; 16],
            fiq: [0; 7],
            svc: [0; 2],
            abt: [0; 2],
            irq: [0; 2],
            und: [0; 2],
            cpsr: v[31],
            spsr: [0; 5],
            pipeline: [v[37], v[38]],
        };
        s.r.copy_from_slice(&v[0..16]);
        s.fiq.copy_from_slice(&v[16..23]);
        s.svc.copy_from_slice(&v[23..25]);
        s.abt.copy_from_slice(&v[25..27]);
        s.irq.copy_from_slice(&v[27..29]);
        s.und.copy_from_slice(&v[29..31]);
        s.spsr.copy_from_slice(&v[32..37]);
        (size, s)
    }

    const SPSR_MODES: [u32; 5] = [MODE_FIQ, MODE_SVC, MODE_ABT, MODE_IRQ, MODE_UND];

    /// The test's registers are the user bank's in `r` and every other mode's in its own
    /// list, the current mode's included.
    fn load(s: &State) -> Cpu {
        let mut cpu = Cpu::reset();
        cpu.cpsr = MODE_USR;
        cpu.r = s.r;
        cpu.fiq_hi.copy_from_slice(&s.fiq[..5]);
        cpu.banked[1] = [s.fiq[5], s.fiq[6]];
        cpu.banked[3] = s.svc;
        cpu.banked[4] = s.abt;
        cpu.banked[2] = s.irq;
        cpu.banked[5] = s.und;
        for (i, m) in SPSR_MODES.iter().enumerate() {
            cpu.spsr[bank_of(*m)] = s.spsr[i];
        }
        cpu.set_mode(s.cpsr & 0x1F);
        cpu.cpsr = s.cpsr;
        cpu
    }

    /// The CPU's registers laid out as a test's are.
    fn view(cpu: &Cpu) -> State {
        let mut c = cpu.clone();
        c.set_mode(MODE_USR);
        let mut v = State {
            r: c.r,
            fiq: [0; 7],
            svc: c.banked[3],
            abt: c.banked[4],
            irq: c.banked[2],
            und: c.banked[5],
            cpsr: cpu.cpsr,
            spsr: [0; 5],
            pipeline: [0; 2],
        };
        v.fiq[..5].copy_from_slice(&c.fiq_hi);
        v.fiq[5] = c.banked[1][0];
        v.fiq[6] = c.banked[1][1];
        for (i, m) in SPSR_MODES.iter().enumerate() {
            v.spsr[i] = cpu.spsr[bank_of(*m)];
        }
        v
    }

    #[test]
    fn single_step_tests() {
        let Ok(dir) = std::env::var("ARM7TDMI_TESTS") else {
            return;
        };
        let only = std::env::var("ARM7TDMI_ONLY").ok();
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".json.bin"))
            .collect();
        files.sort();
        let mut total_bad = 0;
        let mut run = 0;
        for f in files {
            let stem = f.trim_end_matches(".json.bin").to_string();
            if only.as_deref().is_some_and(|o| o != stem) {
                continue;
            }
            let b = std::fs::read(format!("{dir}/{f}")).unwrap();
            let count = u32_at(&b, 4) as usize;
            let mut at = 8;
            let mut bad = 0;
            let mut skipped = 0;
            for _ in 0..count {
                let size = u32_at(&b, at) as usize;
                let mut p = at + 4;
                let (n, init) = state(&b, p);
                p += n;
                let (n, fin) = state(&b, p);
                p += n;
                let tsize = u32_at(&b, p) as usize;
                let tn = u32_at(&b, p + 8) as usize;
                let mut reads = Vec::new();
                let mut writes = Vec::new();
                for i in 0..tn {
                    let q = p + 12 + i * 24;
                    let kind = u32_at(&b, q);
                    let t = (u32_at(&b, q + 8), u32_at(&b, q + 4), u32_at(&b, q + 12));
                    match kind {
                        1 => reads.push(t),
                        2 => writes.push(t),
                        _ => {}
                    }
                }
                p += tsize;
                let _opcode = u32_at(&b, p + 8);
                at += size;
                let thumb = init.cpsr & T != 0;
                let op = init.pipeline[0];
                if skip(&stem, op, thumb) {
                    skipped += 1;
                    continue;
                }
                let mut cpu = load(&init);
                cpu.r[15] = init.r[15].wrapping_sub(if thumb { 4 } else { 8 });
                let mut bus = TestBus {
                    reads,
                    writes: Vec::new(),
                };
                if thumb {
                    cpu.thumb_op(&mut bus, op as u16);
                } else {
                    cpu.arm_op(&mut bus, op);
                }
                // MSR changing the T bit.
                let is_bx = op & 0x0FFF_FFF0 == 0x012F_FF10;
                if !thumb && !is_bx && op & 0x0DB0_F000 == 0x0120_F000 && (cpu.cpsr ^ init.cpsr) & T != 0 {
                    skipped += 1;
                    continue;
                }
                run += 1;
                let mut problems = Vec::new();
                let have = view(&cpu);
                let mut got = have.r;
                got[15] = cpu.r[15].wrapping_add(if cpu.thumb() { 4 } else { 8 });
                for i in 0..16 {
                    if got[i] != fin.r[i] {
                        problems.push(format!("r{i} {:08X} want {:08X}", got[i], fin.r[i]));
                    }
                }
                // The multiplier's carry out is not modelled (no game relies on it).
                let c_mask = if is_multiply(op, thumb) { !C } else { !0 };
                if cpu.cpsr & c_mask != fin.cpsr & c_mask {
                    problems.push(format!("cpsr {:08X} want {:08X}", cpu.cpsr, fin.cpsr));
                }
                if have.spsr != fin.spsr {
                    problems.push(format!("spsr {:08X?} want {:08X?}", have.spsr, fin.spsr));
                }
                for (name, h, w) in [
                    ("fiq", &have.fiq[..], &fin.fiq[..]),
                    ("svc", &have.svc[..], &fin.svc[..]),
                    ("abt", &have.abt[..], &fin.abt[..]),
                    ("irq", &have.irq[..], &fin.irq[..]),
                    ("und", &have.und[..], &fin.und[..]),
                ] {
                    if h != w {
                        problems.push(format!("{name} {h:08X?} want {w:08X?}"));
                    }
                }
                let mut want_w = writes.clone();
                want_w.iter_mut().for_each(|w| w.0 &= !(w.1 - 1));
                let mut got_w = bus.writes.clone();
                got_w.iter_mut().for_each(|w| w.0 &= !(w.1 - 1));
                if got_w != want_w {
                    problems.push(format!("writes {got_w:08X?} want {want_w:08X?}"));
                }
                if !problems.is_empty() {
                    bad += 1;
                    if bad <= 3 {
                        eprintln!(
                            "{stem} {} {:08X}: {}",
                            if thumb { "thumb" } else { "arm" },
                            op,
                            problems.join(", ")
                        );
                    }
                }
            }
            if bad > 0 || skipped > 0 {
                eprintln!("{stem}: {bad} of {count} failed, {skipped} skipped");
            }
            total_bad += bad;
        }
        assert!(run > 0);
        assert_eq!(total_bad, 0, "{total_bad} failures in {run} tests");
    }

    fn is_multiply(op: u32, thumb: bool) -> bool {
        if thumb {
            op & 0xFFC0 == 0x4340
        } else {
            op & 0x0F00_00F0 == 0x90 && op & 0x0F00_0000 == 0
        }
    }

    /// Cases left out: ones no assembler or compiler makes, whose result depends on the
    /// pipeline of the emulator the tests came from rather than on anything documented.
    ///
    /// - MRS into R15, and MSR changing the T bit (NanoBoyAdvance doesn't refill its
    ///   pipeline for them; the latter is checked in the test itself).
    /// - Loads and stores writing back into R15 as their base.
    fn skip(_file: &str, op: u32, thumb: bool) -> bool {
        if thumb {
            return false;
        }
        // MRS Rd=15.
        if op & 0x0FBF_0FFF == 0x010F_0000 && (op >> 12) & 15 == 15 {
            return true;
        }
        let rn = (op >> 16) & 15;
        match (op >> 25) & 7 {
            // LDR/STR with writeback (or post-indexed) on R15.
            2 | 3 => rn == 15 && (op & (1 << 24) == 0 || op & (1 << 21) != 0),
            // Halfword transfers the same.
            0 if op & 0x90 == 0x90 && op & 0x60 != 0 => {
                rn == 15 && (op & (1 << 24) == 0 || op & (1 << 21) != 0)
            }
            // LDM/STM writing back to R15.
            4 => rn == 15 && op & (1 << 21) != 0,
            _ => false,
        }
    }
}
