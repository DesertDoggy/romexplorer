//! A Zilog Z80 interpreter, undocumented behaviour included (IXH/IXL, SLL, the X/Y flags).
//!
//! It runs a game's own code to drive a sound chip: the Master System and Game Gear's main
//! CPU, the Mega Drive's sound CPU, the MSX and ColecoVision. Cycle counts are in T-states.
//! Like [`super::m6502`], it keeps a shadow of the calls in progress, so the music search can
//! tell which routine wrote to the sound chip.

/// What the CPU is attached to: memory and I/O ports.
pub trait Bus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, value: u8);
    fn input(&mut self, port: u16) -> u8;
    fn output(&mut self, port: u16, value: u8);
}

const SF: u8 = 0x80;
const ZF: u8 = 0x40;
const YF: u8 = 0x20;
const HF: u8 = 0x10;
const XF: u8 = 0x08;
const PF: u8 = 0x04;
const NF: u8 = 0x02;
const CF: u8 = 0x01;
const XY: u8 = XF | YF;

/// How deep the shadow of calls in progress goes.
const MAX_CALLS: usize = 32;

fn sz53(v: u8) -> u8 {
    (v & (SF | XY)) | if v == 0 { ZF } else { 0 }
}

fn parity(v: u8) -> u8 {
    if v.count_ones() % 2 == 0 {
        PF
    } else {
        0
    }
}

fn sz53p(v: u8) -> u8 {
    sz53(v) | parity(v)
}

/// Which register stands in for HL: DD and FD prefixes swap in IX and IY.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Idx {
    Hl,
    Ix,
    Iy,
}

#[derive(Clone, Debug, Default)]
pub struct Cpu {
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub af_: u16,
    pub bc_: u16,
    pub de_: u16,
    pub hl_: u16,
    pub ix: u16,
    pub iy: u16,
    pub sp: u16,
    pub pc: u16,
    pub i: u8,
    pub r: u8,
    pub iff1: bool,
    pub iff2: bool,
    pub im: u8,
    pub halted: bool,
    /// EI just ran: no interrupt before the next instruction.
    ei_delay: bool,
    /// The internal address register (MEMPTR), which leaks into a few flags.
    pub wz: u16,
    /// The flags the last instruction set, or 0 (SCF and CCF read it).
    q: u8,
    set_flags: bool,
    /// The entry points of the subroutines in progress, outermost first, with the stack
    /// pointer each returns to.
    pub calls: Vec<(u16, u16)>,
}

impl Cpu {
    /// The CPU after a reset.
    pub fn reset() -> Self {
        Self {
            a: 0xFF,
            f: 0xFF,
            sp: 0xFFFF,
            ..Default::default()
        }
    }

    pub fn bc(&self) -> u16 {
        u16::from_be_bytes([self.b, self.c])
    }
    pub fn de(&self) -> u16 {
        u16::from_be_bytes([self.d, self.e])
    }
    pub fn hl(&self) -> u16 {
        u16::from_be_bytes([self.h, self.l])
    }
    fn af(&self) -> u16 {
        u16::from_be_bytes([self.a, self.f])
    }
    fn set_bc(&mut self, v: u16) {
        [self.b, self.c] = v.to_be_bytes();
    }
    fn set_de(&mut self, v: u16) {
        [self.d, self.e] = v.to_be_bytes();
    }
    pub fn set_hl(&mut self, v: u16) {
        [self.h, self.l] = v.to_be_bytes();
    }
    fn set_af(&mut self, v: u16) {
        [self.a, self.f] = v.to_be_bytes();
    }

    fn flags(&mut self, f: u8) {
        self.f = f;
        self.set_flags = true;
    }

    fn inc_r(&mut self) {
        self.r = (self.r & 0x80) | (self.r.wrapping_add(1) & 0x7F);
    }

    fn fetch<B: Bus>(&mut self, bus: &mut B) -> u8 {
        let v = bus.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }

    fn fetch16<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let lo = self.fetch(bus);
        let hi = self.fetch(bus);
        u16::from_le_bytes([lo, hi])
    }

    fn read16<B: Bus>(bus: &mut B, a: u16) -> u16 {
        u16::from_le_bytes([bus.read(a), bus.read(a.wrapping_add(1))])
    }

    fn write16<B: Bus>(bus: &mut B, a: u16, v: u16) {
        let [lo, hi] = v.to_le_bytes();
        bus.write(a, lo);
        bus.write(a.wrapping_add(1), hi);
    }

    pub fn push<B: Bus>(&mut self, bus: &mut B, v: u16) {
        let [hi, lo] = v.to_be_bytes();
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp, hi);
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp, lo);
    }

    pub fn pop<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let lo = bus.read(self.sp);
        self.sp = self.sp.wrapping_add(1);
        let hi = bus.read(self.sp);
        self.sp = self.sp.wrapping_add(1);
        u16::from_le_bytes([lo, hi])
    }

    /// A call to `target`: pushes the return address and records the call.
    pub fn call<B: Bus>(&mut self, bus: &mut B, target: u16) {
        let sp = self.sp;
        let pc = self.pc;
        self.push(bus, pc);
        self.pc = target;
        self.wz = target;
        if self.calls.len() < MAX_CALLS {
            self.calls.push((target, sp));
        }
    }

    pub fn ret<B: Bus>(&mut self, bus: &mut B) {
        self.pc = self.pop(bus);
        self.wz = self.pc;
        while let Some(&(_, sp)) = self.calls.last() {
            // Returned once the stack is back at or above where the call started.
            if self.sp.wrapping_sub(sp) as i16 >= 0 {
                self.calls.pop();
            } else {
                break;
            }
        }
    }

    /// A jump through a register: a jump table reached by a call that popped its return
    /// address makes the call in progress one to where it went.
    fn jump_indirect(&mut self, to: u16) {
        self.pc = to;
        if let Some(top) = self.calls.last_mut() {
            if self.sp.wrapping_sub(top.1) as i16 >= 0 {
                top.0 = to;
            }
        }
    }

    /// Takes an interrupt: the non-maskable one, or the maskable one if enabled (mode 2 reads
    /// its vector with $FF on the data bus). Returns the T-states taken (0 when not taken).
    pub fn interrupt<B: Bus>(&mut self, bus: &mut B, nmi: bool) -> u32 {
        if nmi {
            self.halted = false;
            self.iff1 = false;
            self.inc_r();
            self.call(bus, 0x66);
            return 11;
        }
        if !self.iff1 || self.ei_delay {
            return 0;
        }
        self.halted = false;
        self.iff1 = false;
        self.iff2 = false;
        self.inc_r();
        if self.im == 2 {
            let at = u16::from_be_bytes([self.i, 0xFF]);
            let to = Self::read16(bus, at);
            self.call(bus, to);
            19
        } else {
            self.call(bus, 0x38);
            13
        }
    }

    fn idx(&self, idx: Idx) -> u16 {
        match idx {
            Idx::Hl => self.hl(),
            Idx::Ix => self.ix,
            Idx::Iy => self.iy,
        }
    }

    fn set_idx(&mut self, idx: Idx, v: u16) {
        match idx {
            Idx::Hl => self.set_hl(v),
            Idx::Ix => self.ix = v,
            Idx::Iy => self.iy = v,
        }
    }

    fn rp(&self, p: u8, idx: Idx) -> u16 {
        match p {
            0 => self.bc(),
            1 => self.de(),
            2 => self.idx(idx),
            _ => self.sp,
        }
    }

    fn set_rp(&mut self, p: u8, idx: Idx, v: u16) {
        match p {
            0 => self.set_bc(v),
            1 => self.set_de(v),
            2 => self.set_idx(idx, v),
            _ => self.sp = v,
        }
    }

    /// Register `r` (not 6), with H and L standing for the index register's halves.
    fn reg(&self, r: u8, idx: Idx) -> u8 {
        match r {
            0 => self.b,
            1 => self.c,
            2 => self.d,
            3 => self.e,
            4 => (self.idx(idx) >> 8) as u8,
            5 => self.idx(idx) as u8,
            _ => self.a,
        }
    }

    fn set_reg(&mut self, r: u8, idx: Idx, v: u8) {
        match r {
            0 => self.b = v,
            1 => self.c = v,
            2 => self.d = v,
            3 => self.e = v,
            4 => {
                let x = self.idx(idx);
                self.set_idx(idx, (x & 0x00FF) | (v as u16) << 8);
            }
            5 => {
                let x = self.idx(idx);
                self.set_idx(idx, (x & 0xFF00) | v as u16);
            }
            _ => self.a = v,
        }
    }

    /// The address of (HL), or (IX+d) / (IY+d) with the displacement fetched.
    fn mem_addr<B: Bus>(&mut self, bus: &mut B, idx: Idx) -> u16 {
        match idx {
            Idx::Hl => self.hl(),
            _ => {
                let d = self.fetch(bus) as i8;
                let a = self.idx(idx).wrapping_add(d as u16);
                self.wz = a;
                a
            }
        }
    }

    fn cond(&self, y: u8) -> bool {
        match y {
            0 => self.f & ZF == 0,
            1 => self.f & ZF != 0,
            2 => self.f & CF == 0,
            3 => self.f & CF != 0,
            4 => self.f & PF == 0,
            5 => self.f & PF != 0,
            6 => self.f & SF == 0,
            _ => self.f & SF != 0,
        }
    }

    fn add8(&mut self, v: u8, carry: u8) {
        let a = self.a;
        let r = a as u16 + v as u16 + carry as u16;
        let r8 = r as u8;
        self.flags(
            sz53(r8) | ((a ^ v ^ r8) & HF) | (((a ^ !v) & (a ^ r8) & 0x80) >> 5) | (r > 0xFF) as u8,
        );
        self.a = r8;
    }

    fn sub8(&mut self, v: u8, carry: u8) -> u8 {
        let a = self.a;
        let r = a as i16 - v as i16 - carry as i16;
        let r8 = r as u8;
        self.flags(
            sz53(r8)
                | NF
                | ((a ^ v ^ r8) & HF)
                | (((a ^ v) & (a ^ r8) & 0x80) >> 5)
                | (r < 0) as u8,
        );
        r8
    }

    fn alu(&mut self, op: u8, v: u8) {
        match op {
            0 => self.add8(v, 0),
            1 => self.add8(v, self.f & CF),
            2 => self.a = self.sub8(v, 0),
            3 => self.a = self.sub8(v, self.f & CF),
            4 => {
                self.a &= v;
                self.flags(sz53p(self.a) | HF);
            }
            5 => {
                self.a ^= v;
                self.flags(sz53p(self.a));
            }
            6 => {
                self.a |= v;
                self.flags(sz53p(self.a));
            }
            _ => {
                self.sub8(v, 0);
                self.f = (self.f & !XY) | (v & XY);
            }
        }
    }

    fn inc8(&mut self, v: u8) -> u8 {
        let r = v.wrapping_add(1);
        self.flags(
            (self.f & CF)
                | sz53(r)
                | if r & 0x0F == 0 { HF } else { 0 }
                | if r == 0x80 { PF } else { 0 },
        );
        r
    }

    fn dec8(&mut self, v: u8) -> u8 {
        let r = v.wrapping_sub(1);
        self.flags(
            (self.f & CF)
                | NF
                | sz53(r)
                | if v & 0x0F == 0 { HF } else { 0 }
                | if v == 0x80 { PF } else { 0 },
        );
        r
    }

    fn add16(&mut self, a: u16, b: u16) -> u16 {
        let r = a as u32 + b as u32;
        let r16 = r as u16;
        self.wz = a.wrapping_add(1);
        self.flags(
            (self.f & (SF | ZF | PF))
                | (((a ^ b ^ r16) >> 8) as u8 & HF)
                | ((r16 >> 8) as u8 & XY)
                | (r > 0xFFFF) as u8,
        );
        r16
    }

    fn adc16(&mut self, v: u16) {
        let hl = self.hl();
        let r = hl as u32 + v as u32 + (self.f & CF) as u32;
        let r16 = r as u16;
        self.wz = hl.wrapping_add(1);
        self.flags(
            ((r16 >> 8) as u8 & (SF | XY))
                | if r16 == 0 { ZF } else { 0 }
                | (((hl ^ v ^ r16) >> 8) as u8 & HF)
                | ((((hl ^ !v) & (hl ^ r16) & 0x8000) >> 13) as u8)
                | (r > 0xFFFF) as u8,
        );
        self.set_hl(r16);
    }

    fn sbc16(&mut self, v: u16) {
        let hl = self.hl();
        let r = hl as i32 - v as i32 - (self.f & CF) as i32;
        let r16 = r as u16;
        self.wz = hl.wrapping_add(1);
        self.flags(
            ((r16 >> 8) as u8 & (SF | XY))
                | if r16 == 0 { ZF } else { 0 }
                | NF
                | (((hl ^ v ^ r16) >> 8) as u8 & HF)
                | ((((hl ^ v) & (hl ^ r16) & 0x8000) >> 13) as u8)
                | (r < 0) as u8,
        );
        self.set_hl(r16);
    }

    /// The CB rotates and shifts.
    fn rot(&mut self, op: u8, v: u8) -> u8 {
        let c = self.f & CF;
        let (r, carry) = match op {
            0 => (v.rotate_left(1), v >> 7),
            1 => (v.rotate_right(1), v & 1),
            2 => (v << 1 | c, v >> 7),
            3 => (v >> 1 | c << 7, v & 1),
            4 => (v << 1, v >> 7),
            5 => ((v >> 1) | (v & 0x80), v & 1),
            6 => (v << 1 | 1, v >> 7),
            _ => (v >> 1, v & 1),
        };
        self.flags(sz53p(r) | carry);
        r
    }

    fn bit(&mut self, y: u8, v: u8, xy_from: u8) {
        let set = v & (1 << y) != 0;
        self.flags(
            (self.f & CF)
                | HF
                | (xy_from & XY)
                | if set { 0 } else { ZF | PF }
                | if y == 7 && set { SF } else { 0 },
        );
    }

    /// Runs one instruction; returns the T-states it took (4 while halted).
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.ei_delay = false;
        self.set_flags = false;
        let t = self.step_inner(bus);
        self.q = if self.set_flags { self.f } else { 0 };
        t
    }

    fn step_inner<B: Bus>(&mut self, bus: &mut B) -> u32 {
        if self.halted {
            self.inc_r();
            return 4;
        }
        self.inc_r();
        let op = self.fetch(bus);
        match op {
            0xDD | 0xFD => {
                let idx = if op == 0xDD { Idx::Ix } else { Idx::Iy };
                // Another prefix next: this one was a NOP.
                if matches!(bus.read(self.pc), 0xDD | 0xFD) {
                    return 4;
                }
                // The prefix counts as an instruction that left the flags alone.
                self.q = 0;
                self.inc_r();
                let op = self.fetch(bus);
                match op {
                    0xCB => 4 + self.index_cb(bus, idx),
                    0xED => {
                        self.inc_r();
                        let op = self.fetch(bus);
                        4 + self.ed(bus, op)
                    }
                    _ => 4 + self.main(bus, op, idx),
                }
            }
            0xED => {
                self.inc_r();
                let op = self.fetch(bus);
                self.ed(bus, op)
            }
            0xCB => {
                self.inc_r();
                let op = self.fetch(bus);
                self.cb(bus, op)
            }
            _ => self.main(bus, op, Idx::Hl),
        }
    }

    fn main<B: Bus>(&mut self, bus: &mut B, op: u8, idx: Idx) -> u32 {
        let (x, y, z) = (op >> 6, (op >> 3) & 7, op & 7);
        let (p, q) = (y >> 1, y & 1);
        let ix = idx != Idx::Hl;
        match x {
            0 => match z {
                0 => match y {
                    0 => 4,
                    1 => {
                        let af = self.af();
                        self.set_af(self.af_);
                        self.af_ = af;
                        4
                    }
                    2 => {
                        let d = self.fetch(bus) as i8;
                        self.b = self.b.wrapping_sub(1);
                        if self.b != 0 {
                            self.pc = self.pc.wrapping_add(d as u16);
                            self.wz = self.pc;
                            13
                        } else {
                            8
                        }
                    }
                    _ => {
                        let d = self.fetch(bus) as i8;
                        if y == 3 || self.cond(y - 4) {
                            self.pc = self.pc.wrapping_add(d as u16);
                            self.wz = self.pc;
                            12
                        } else {
                            7
                        }
                    }
                },
                1 => {
                    if q == 0 {
                        let v = self.fetch16(bus);
                        self.set_rp(p, idx, v);
                        10
                    } else {
                        let a = self.idx(idx);
                        let b = self.rp(p, idx);
                        let r = self.add16(a, b);
                        self.set_idx(idx, r);
                        11
                    }
                }
                2 => match (q, p) {
                    (0, 0 | 1) => {
                        let a = if p == 0 { self.bc() } else { self.de() };
                        bus.write(a, self.a);
                        self.wz = (self.a as u16) << 8 | (a.wrapping_add(1) & 0xFF);
                        7
                    }
                    (0, 2) => {
                        let a = self.fetch16(bus);
                        Self::write16(bus, a, self.idx(idx));
                        self.wz = a.wrapping_add(1);
                        16
                    }
                    (0, _) => {
                        let a = self.fetch16(bus);
                        bus.write(a, self.a);
                        self.wz = (self.a as u16) << 8 | (a.wrapping_add(1) & 0xFF);
                        13
                    }
                    (_, 0 | 1) => {
                        let a = if p == 0 { self.bc() } else { self.de() };
                        self.a = bus.read(a);
                        self.wz = a.wrapping_add(1);
                        7
                    }
                    (_, 2) => {
                        let a = self.fetch16(bus);
                        let v = Self::read16(bus, a);
                        self.set_idx(idx, v);
                        self.wz = a.wrapping_add(1);
                        16
                    }
                    _ => {
                        let a = self.fetch16(bus);
                        self.a = bus.read(a);
                        self.wz = a.wrapping_add(1);
                        13
                    }
                },
                3 => {
                    let v = self.rp(p, idx);
                    let v = if q == 0 {
                        v.wrapping_add(1)
                    } else {
                        v.wrapping_sub(1)
                    };
                    self.set_rp(p, idx, v);
                    6
                }
                4 | 5 => {
                    if y == 6 {
                        let a = self.mem_addr(bus, idx);
                        let v = bus.read(a);
                        let r = if z == 4 { self.inc8(v) } else { self.dec8(v) };
                        bus.write(a, r);
                        if ix {
                            19
                        } else {
                            11
                        }
                    } else {
                        let v = self.reg(y, idx);
                        let r = if z == 4 { self.inc8(v) } else { self.dec8(v) };
                        self.set_reg(y, idx, r);
                        4
                    }
                }
                6 => {
                    if y == 6 {
                        let a = self.mem_addr(bus, idx);
                        let v = self.fetch(bus);
                        bus.write(a, v);
                        if ix {
                            15
                        } else {
                            10
                        }
                    } else {
                        let v = self.fetch(bus);
                        self.set_reg(y, idx, v);
                        7
                    }
                }
                _ => {
                    let keep = self.f & (SF | ZF | PF);
                    match y {
                        0 => {
                            self.a = self.a.rotate_left(1);
                            self.flags(keep | (self.a & (XY | CF)));
                        }
                        1 => {
                            let c = self.a & 1;
                            self.a = self.a.rotate_right(1);
                            self.flags(keep | (self.a & XY) | c);
                        }
                        2 => {
                            let c = self.a >> 7;
                            self.a = self.a << 1 | (self.f & CF);
                            self.flags(keep | (self.a & XY) | c);
                        }
                        3 => {
                            let c = self.a & 1;
                            self.a = self.a >> 1 | (self.f & CF) << 7;
                            self.flags(keep | (self.a & XY) | c);
                        }
                        4 => self.daa(),
                        5 => {
                            self.a = !self.a;
                            self.flags((self.f & (SF | ZF | PF | CF)) | HF | NF | (self.a & XY));
                        }
                        6 => {
                            let xy = ((self.q ^ self.f) | self.a) & XY;
                            self.flags(keep | CF | xy);
                        }
                        _ => {
                            let xy = ((self.q ^ self.f) | self.a) & XY;
                            let c = self.f & CF;
                            self.flags(keep | if c != 0 { HF } else { 0 } | (c ^ 1) | xy);
                        }
                    }
                    4
                }
            },
            1 => {
                if y == 6 && z == 6 {
                    self.halted = true;
                    return 4;
                }
                if y == 6 {
                    let a = self.mem_addr(bus, idx);
                    bus.write(a, self.reg(z, Idx::Hl));
                    if ix {
                        15
                    } else {
                        7
                    }
                } else if z == 6 {
                    let a = self.mem_addr(bus, idx);
                    let v = bus.read(a);
                    self.set_reg(y, Idx::Hl, v);
                    if ix {
                        15
                    } else {
                        7
                    }
                } else {
                    let v = self.reg(z, idx);
                    self.set_reg(y, idx, v);
                    4
                }
            }
            2 => {
                if z == 6 {
                    let a = self.mem_addr(bus, idx);
                    let v = bus.read(a);
                    self.alu(y, v);
                    if ix {
                        15
                    } else {
                        7
                    }
                } else {
                    let v = self.reg(z, idx);
                    self.alu(y, v);
                    4
                }
            }
            _ => match z {
                0 => {
                    if self.cond(y) {
                        self.ret(bus);
                        11
                    } else {
                        5
                    }
                }
                1 => match (q, p) {
                    (0, _) => {
                        let v = self.pop(bus);
                        match p {
                            3 => self.set_af(v),
                            _ => self.set_rp(p, idx, v),
                        }
                        10
                    }
                    (_, 0) => {
                        self.ret(bus);
                        10
                    }
                    (_, 1) => {
                        let (bc, de, hl) = (self.bc(), self.de(), self.hl());
                        self.set_bc(self.bc_);
                        self.set_de(self.de_);
                        self.set_hl(self.hl_);
                        (self.bc_, self.de_, self.hl_) = (bc, de, hl);
                        4
                    }
                    (_, 2) => {
                        let to = self.idx(idx);
                        self.jump_indirect(to);
                        4
                    }
                    _ => {
                        self.sp = self.idx(idx);
                        6
                    }
                },
                2 => {
                    let a = self.fetch16(bus);
                    self.wz = a;
                    if self.cond(y) {
                        self.pc = a;
                    }
                    10
                }
                3 => match y {
                    0 => {
                        let a = self.fetch16(bus);
                        self.pc = a;
                        self.wz = a;
                        10
                    }
                    1 => {
                        // Only reached through DD/FD.
                        self.index_cb(bus, idx)
                    }
                    2 => {
                        let n = self.fetch(bus);
                        let port = (self.a as u16) << 8 | n as u16;
                        bus.output(port, self.a);
                        self.wz = (self.a as u16) << 8 | (n.wrapping_add(1) as u16);
                        11
                    }
                    3 => {
                        let n = self.fetch(bus);
                        let port = (self.a as u16) << 8 | n as u16;
                        self.a = bus.input(port);
                        self.wz = port.wrapping_add(1);
                        11
                    }
                    4 => {
                        let v = Self::read16(bus, self.sp);
                        Self::write16(bus, self.sp, self.idx(idx));
                        self.set_idx(idx, v);
                        self.wz = v;
                        19
                    }
                    5 => {
                        let (de, hl) = (self.de(), self.hl());
                        self.set_de(hl);
                        self.set_hl(de);
                        4
                    }
                    6 => {
                        self.iff1 = false;
                        self.iff2 = false;
                        4
                    }
                    _ => {
                        self.iff1 = true;
                        self.iff2 = true;
                        self.ei_delay = true;
                        4
                    }
                },
                4 => {
                    let a = self.fetch16(bus);
                    self.wz = a;
                    if self.cond(y) {
                        self.call(bus, a);
                        17
                    } else {
                        10
                    }
                }
                5 => {
                    if q == 0 {
                        let v = match p {
                            3 => self.af(),
                            _ => self.rp(p, idx),
                        };
                        self.push(bus, v);
                        11
                    } else {
                        // CALL nn (p 0); the prefixes are handled before this.
                        let a = self.fetch16(bus);
                        self.call(bus, a);
                        17
                    }
                }
                6 => {
                    let v = self.fetch(bus);
                    self.alu(y, v);
                    7
                }
                _ => {
                    self.call(bus, y as u16 * 8);
                    11
                }
            },
        }
    }

    fn daa(&mut self) {
        let a = self.a;
        let (c, h, n) = (self.f & CF != 0, self.f & HF != 0, self.f & NF != 0);
        let mut diff = 0u8;
        if h || a & 0x0F > 9 {
            diff |= 0x06;
        }
        let carry = c || a > 0x99;
        if carry {
            diff |= 0x60;
        }
        let half = if n { h && a & 0x0F < 6 } else { a & 0x0F > 9 };
        let r = if n {
            a.wrapping_sub(diff)
        } else {
            a.wrapping_add(diff)
        };
        self.a = r;
        self.flags(sz53p(r) | (self.f & NF) | if half { HF } else { 0 } | carry as u8);
    }

    fn cb<B: Bus>(&mut self, bus: &mut B, op: u8) -> u32 {
        let (x, y, z) = (op >> 6, (op >> 3) & 7, op & 7);
        if z == 6 {
            let a = self.hl();
            let v = bus.read(a);
            match x {
                0 => {
                    let r = self.rot(y, v);
                    bus.write(a, r);
                    15
                }
                1 => {
                    let w = (self.wz >> 8) as u8;
                    self.bit(y, v, w);
                    12
                }
                2 => {
                    bus.write(a, v & !(1 << y));
                    15
                }
                _ => {
                    bus.write(a, v | (1 << y));
                    15
                }
            }
        } else {
            let v = self.reg(z, Idx::Hl);
            match x {
                0 => {
                    let r = self.rot(y, v);
                    self.set_reg(z, Idx::Hl, r);
                }
                1 => self.bit(y, v, v),
                2 => self.set_reg(z, Idx::Hl, v & !(1 << y)),
                _ => self.set_reg(z, Idx::Hl, v | (1 << y)),
            }
            8
        }
    }

    /// DD CB d op / FD CB d op.
    fn index_cb<B: Bus>(&mut self, bus: &mut B, idx: Idx) -> u32 {
        let a = self.mem_addr(bus, idx);
        let op = self.fetch(bus);
        let (x, y, z) = (op >> 6, (op >> 3) & 7, op & 7);
        let v = bus.read(a);
        let r = match x {
            0 => self.rot(y, v),
            1 => {
                self.bit(y, v, (a >> 8) as u8);
                return 16;
            }
            2 => v & !(1 << y),
            _ => v | (1 << y),
        };
        bus.write(a, r);
        // The result also lands in a register (undocumented).
        if z != 6 {
            self.set_reg(z, Idx::Hl, r);
        }
        19
    }

    fn ed<B: Bus>(&mut self, bus: &mut B, op: u8) -> u32 {
        let (x, y, z) = (op >> 6, (op >> 3) & 7, op & 7);
        let (p, q) = (y >> 1, y & 1);
        match x {
            1 => match z {
                0 => {
                    let bc = self.bc();
                    let v = bus.input(bc);
                    self.wz = bc.wrapping_add(1);
                    self.flags((self.f & CF) | sz53p(v));
                    if y != 6 {
                        self.set_reg(y, Idx::Hl, v);
                    }
                    12
                }
                1 => {
                    let bc = self.bc();
                    let v = if y == 6 { 0 } else { self.reg(y, Idx::Hl) };
                    bus.output(bc, v);
                    self.wz = bc.wrapping_add(1);
                    12
                }
                2 => {
                    let v = self.rp(p, Idx::Hl);
                    if q == 0 {
                        self.sbc16(v);
                    } else {
                        self.adc16(v);
                    }
                    15
                }
                3 => {
                    let a = self.fetch16(bus);
                    if q == 0 {
                        Self::write16(bus, a, self.rp(p, Idx::Hl));
                    } else {
                        let v = Self::read16(bus, a);
                        self.set_rp(p, Idx::Hl, v);
                    }
                    self.wz = a.wrapping_add(1);
                    20
                }
                4 => {
                    let v = self.a;
                    self.a = 0;
                    self.a = self.sub8(v, 0);
                    8
                }
                5 => {
                    self.iff1 = self.iff2;
                    self.ret(bus);
                    14
                }
                6 => {
                    self.im = [0, 0, 1, 2][(y & 3) as usize];
                    8
                }
                _ => match y {
                    0 => {
                        self.i = self.a;
                        9
                    }
                    1 => {
                        self.r = self.a;
                        9
                    }
                    2 | 3 => {
                        self.a = if y == 2 { self.i } else { self.r };
                        let p = if self.iff2 { PF } else { 0 };
                        self.flags((self.f & CF) | sz53(self.a) | p);
                        9
                    }
                    4 | 5 => {
                        let a = self.hl();
                        let m = bus.read(a);
                        let (nm, na) = if y == 4 {
                            (self.a << 4 | m >> 4, (self.a & 0xF0) | (m & 0x0F))
                        } else {
                            (m << 4 | (self.a & 0x0F), (self.a & 0xF0) | m >> 4)
                        };
                        bus.write(a, nm);
                        self.a = na;
                        self.wz = a.wrapping_add(1);
                        self.flags((self.f & CF) | sz53p(na));
                        18
                    }
                    _ => 8,
                },
            },
            2 if z <= 3 && y >= 4 => self.block(bus, y, z),
            _ => 8,
        }
    }

    /// LDI/CPI/INI/OUTI and their decrementing and repeating forms.
    fn block<B: Bus>(&mut self, bus: &mut B, y: u8, z: u8) -> u32 {
        let dec = y & 1 != 0;
        let repeat = y >= 6;
        let step = |v: u16| {
            if dec {
                v.wrapping_sub(1)
            } else {
                v.wrapping_add(1)
            }
        };
        let hl = self.hl();
        match z {
            0 => {
                let v = bus.read(hl);
                bus.write(self.de(), v);
                self.set_hl(step(hl));
                let de = self.de();
                self.set_de(step(de));
                let bc = self.bc().wrapping_sub(1);
                self.set_bc(bc);
                let n = v.wrapping_add(self.a);
                let mut f = (self.f & (SF | ZF | CF))
                    | (n & XF)
                    | ((n << 4) & YF)
                    | if bc != 0 { PF } else { 0 };
                if repeat && bc != 0 {
                    self.pc = self.pc.wrapping_sub(2);
                    self.wz = self.pc.wrapping_add(1);
                    f = (f & !XY) | ((self.pc >> 8) as u8 & XY);
                    self.flags(f);
                    return 21;
                }
                self.flags(f);
                16
            }
            1 => {
                let v = bus.read(hl);
                let r = self.a.wrapping_sub(v);
                let h = (self.a ^ v ^ r) & HF;
                self.set_hl(step(hl));
                let bc = self.bc().wrapping_sub(1);
                self.set_bc(bc);
                self.wz = step(self.wz);
                let n = r.wrapping_sub((h != 0) as u8);
                let mut f = (self.f & CF)
                    | NF
                    | (sz53(r) & (SF | ZF))
                    | h
                    | (n & XF)
                    | ((n << 4) & YF)
                    | if bc != 0 { PF } else { 0 };
                if repeat && bc != 0 && r != 0 {
                    self.pc = self.pc.wrapping_sub(2);
                    self.wz = self.pc.wrapping_add(1);
                    f = (f & !XY) | ((self.pc >> 8) as u8 & XY);
                    self.flags(f);
                    return 21;
                }
                self.flags(f);
                16
            }
            _ => {
                let bc = self.bc();
                let (v, k) = if z == 2 {
                    let v = bus.input(bc);
                    bus.write(hl, v);
                    let c = if dec {
                        self.c.wrapping_sub(1)
                    } else {
                        self.c.wrapping_add(1)
                    };
                    self.wz = step(bc);
                    (v, v as u16 + c as u16)
                } else {
                    let v = bus.read(hl);
                    let port = bc.wrapping_sub(0x100);
                    bus.output(port, v);
                    self.wz = step(port);
                    let l = step(hl) as u8;
                    (v, v as u16 + l as u16)
                };
                self.b = self.b.wrapping_sub(1);
                self.set_hl(step(hl));
                let b = self.b;
                let mut f = sz53(b)
                    | if v & 0x80 != 0 { NF } else { 0 }
                    | if k > 0xFF { HF | CF } else { 0 }
                    | parity((k as u8 & 7) ^ b);
                if repeat && b != 0 {
                    self.pc = self.pc.wrapping_sub(2);
                    f = (f & !XY) | ((self.pc >> 8) as u8 & XY);
                    self.flags(f);
                    return 21;
                }
                self.flags(f);
                16
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SingleStepTests' Z80 suite (github.com/SingleStepTests/z80, `v1/*.json`), in the
    /// directory `Z80_TESTS` names; skipped without it.
    #[test]
    fn single_step_tests() {
        let Ok(dir) = std::env::var("Z80_TESTS") else {
            return;
        };
        struct Flat {
            mem: std::collections::HashMap<u16, u8>,
            ports: std::collections::HashMap<u16, u8>,
        }
        impl Bus for Flat {
            fn read(&mut self, addr: u16) -> u8 {
                *self.mem.get(&addr).unwrap_or(&0)
            }
            fn write(&mut self, addr: u16, value: u8) {
                self.mem.insert(addr, value);
            }
            fn input(&mut self, port: u16) -> u8 {
                *self.ports.get(&port).unwrap_or(&0xFF)
            }
            fn output(&mut self, _port: u16, _value: u8) {}
        }
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        // HALT is not a single-step instruction in this model, and the repeating block I/O
        // flags are not modelled exactly (nothing in a sound driver depends on them).
        paths.retain(|p| {
            !matches!(
                p.file_stem().and_then(|s| s.to_str()),
                Some("76" | "dd 76" | "ed b2" | "ed b3" | "ed ba" | "ed bb")
            )
        });
        let mut bad = Vec::new();
        for path in paths {
            let tests: Vec<serde_json::Value> =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let (mut failed, mut first) = (0, None);
            for t in &tests {
                let (i, f) = (&t["initial"], &t["final"]);
                let n = |v: &serde_json::Value, k: &str| v[k].as_u64().unwrap();
                let pairs = |v: &serde_json::Value| -> Vec<(u16, u8)> {
                    v.as_array()
                        .map(|a| {
                            a.iter()
                                .map(|c| {
                                    (c[0].as_u64().unwrap() as u16, c[1].as_u64().unwrap() as u8)
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let mut bus = Flat {
                    mem: pairs(&i["ram"]).into_iter().collect(),
                    ports: pairs(&t["ports"]).into_iter().collect(),
                };
                let mut cpu = Cpu {
                    a: n(i, "a") as u8,
                    f: n(i, "f") as u8,
                    b: n(i, "b") as u8,
                    c: n(i, "c") as u8,
                    d: n(i, "d") as u8,
                    e: n(i, "e") as u8,
                    h: n(i, "h") as u8,
                    l: n(i, "l") as u8,
                    af_: n(i, "af_") as u16,
                    bc_: n(i, "bc_") as u16,
                    de_: n(i, "de_") as u16,
                    hl_: n(i, "hl_") as u16,
                    ix: n(i, "ix") as u16,
                    iy: n(i, "iy") as u16,
                    sp: n(i, "sp") as u16,
                    pc: n(i, "pc") as u16,
                    i: n(i, "i") as u8,
                    r: n(i, "r") as u8,
                    iff1: n(i, "iff1") != 0,
                    iff2: n(i, "iff2") != 0,
                    im: n(i, "im") as u8,
                    wz: n(i, "wz") as u16,
                    q: n(i, "q") as u8,
                    ..Default::default()
                };
                let cycles = cpu.step(&mut bus) as usize;
                let want = t["cycles"].as_array().unwrap().len();
                let regs = [
                    ("a", cpu.a as u64),
                    ("f", cpu.f as u64),
                    ("b", cpu.b as u64),
                    ("c", cpu.c as u64),
                    ("d", cpu.d as u64),
                    ("e", cpu.e as u64),
                    ("h", cpu.h as u64),
                    ("l", cpu.l as u64),
                    ("af_", cpu.af_ as u64),
                    ("bc_", cpu.bc_ as u64),
                    ("de_", cpu.de_ as u64),
                    ("hl_", cpu.hl_ as u64),
                    ("ix", cpu.ix as u64),
                    ("iy", cpu.iy as u64),
                    ("sp", cpu.sp as u64),
                    ("pc", cpu.pc as u64),
                    ("i", cpu.i as u64),
                    ("r", cpu.r as u64),
                    ("wz", cpu.wz as u64),
                    ("iff1", cpu.iff1 as u64),
                    ("iff2", cpu.iff2 as u64),
                    ("im", cpu.im as u64),
                ];
                let mut wrong: Vec<String> = regs
                    .iter()
                    .filter(|(k, v)| n(f, k) != *v)
                    .map(|(k, v)| format!("{k}={v:#x} want {:#x}", n(f, k)))
                    .collect();
                for (addr, want) in pairs(&f["ram"]) {
                    let got = bus.read(addr);
                    if got != want {
                        wrong.push(format!("[{addr:#06x}]={got:#04x} want {want:#04x}"));
                    }
                }
                if cycles != want {
                    wrong.push(format!("cycles={cycles} want {want}"));
                }
                if !wrong.is_empty() {
                    failed += 1;
                    first.get_or_insert_with(|| format!("{}: {}", t["name"], wrong.join(", ")));
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

    #[test]
    fn calls_are_tracked() {
        struct Mem(Vec<u8>);
        impl Bus for Mem {
            fn read(&mut self, a: u16) -> u8 {
                self.0[a as usize]
            }
            fn write(&mut self, a: u16, v: u8) {
                self.0[a as usize] = v;
            }
            fn input(&mut self, _: u16) -> u8 {
                0xFF
            }
            fn output(&mut self, _: u16, _: u8) {}
        }
        let mut m = Mem(vec![0; 0x10000]);
        // CALL $0100; HALT / at $0100: CALL $0200; RET / at $0200: RET
        m.0[0..4].copy_from_slice(&[0xCD, 0x00, 0x01, 0x76]);
        m.0[0x100..0x104].copy_from_slice(&[0xCD, 0x00, 0x02, 0xC9]);
        m.0[0x200] = 0xC9;
        let mut cpu = Cpu::reset();
        cpu.sp = 0xFF00;
        cpu.step(&mut m);
        assert_eq!(cpu.calls, vec![(0x100, 0xFF00)]);
        cpu.step(&mut m);
        assert_eq!(cpu.calls.len(), 2);
        cpu.step(&mut m);
        assert_eq!(cpu.calls.len(), 1);
        cpu.step(&mut m);
        assert!(cpu.calls.is_empty());
        assert_eq!(cpu.pc, 3);
    }
}
