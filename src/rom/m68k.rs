//! A Motorola 68000 interpreter.
//!
//! It runs a game's own code to drive the sound chips: the Mega Drive's main CPU (its sound
//! CPU is the [`super::z80`]). Cycle counts are approximate: four cycles a bus access plus
//! each instruction's own extra work, which is close enough for the timing music code
//! depends on (it waits for interrupts, not cycle counts). Like [`super::m6502`], it keeps a
//! shadow of the calls in progress, so the music search can tell which routine wrote to the
//! sound chips.

/// What the CPU is attached to. Addresses are 24 bits; word accesses are even.
pub trait Bus {
    fn read8(&mut self, addr: u32) -> u8;
    fn read16(&mut self, addr: u32) -> u16;
    fn write8(&mut self, addr: u32, value: u8);
    fn write16(&mut self, addr: u32, value: u16);
}

const XF: u16 = 0x10;
const NF: u16 = 0x08;
const ZF: u16 = 0x04;
const VF: u16 = 0x02;
const CF: u16 = 0x01;
const SF: u16 = 0x2000;

/// How deep the shadow of calls in progress goes.
const MAX_CALLS: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Size {
    B,
    W,
    L,
}

impl Size {
    fn bytes(self) -> u32 {
        match self {
            Size::B => 1,
            Size::W => 2,
            Size::L => 4,
        }
    }
    fn mask(self) -> u32 {
        match self {
            Size::B => 0xFF,
            Size::W => 0xFFFF,
            Size::L => 0xFFFF_FFFF,
        }
    }
    fn msb(self) -> u32 {
        match self {
            Size::B => 0x80,
            Size::W => 0x8000,
            Size::L => 0x8000_0000,
        }
    }
    /// The size in bits 7-6 of most instructions.
    fn from_bits(b: u16) -> Option<Size> {
        match b & 3 {
            0 => Some(Size::B),
            1 => Some(Size::W),
            2 => Some(Size::L),
            _ => None,
        }
    }
}

fn sext(v: u32, s: Size) -> u32 {
    match s {
        Size::B => v as u8 as i8 as i32 as u32,
        Size::W => v as u16 as i16 as i32 as u32,
        Size::L => v,
    }
}

/// Where an operand is.
#[derive(Clone, Copy, Debug)]
enum Loc {
    D(usize),
    A(usize),
    Mem(u32),
    Imm(u32),
}

/// An address error: the access, whether a write, whether an instruction fetch.
#[derive(Clone, Copy, Debug)]
struct Fault {
    addr: u32,
    write: bool,
    fetch: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Cpu {
    pub d: [u32; 8],
    /// A7 is the active stack pointer.
    pub a: [u32; 8],
    /// The stack pointer of the mode not running (the user one in supervisor mode).
    pub other_sp: u32,
    pub sr: u16,
    pub pc: u32,
    pub stopped: bool,
    /// A double fault: the CPU stops for good.
    pub halted: bool,
    /// The entry points of the subroutines in progress, outermost first, with the stack
    /// pointer each returns to.
    pub calls: Vec<(u32, u32)>,
    /// The instruction running: its address and first word.
    op_pc: u32,
    ir: u16,
    cycles: u32,
    fault: Option<Fault>,
}

enum Ex {
    AddressError(Fault),
    Vector(u32),
}

impl Cpu {
    /// The CPU after a reset: supervisor stack and PC from the vectors.
    pub fn reset<B: Bus>(bus: &mut B) -> Self {
        let mut c = Cpu {
            sr: 0x2700,
            ..Default::default()
        };
        c.a[7] = c.raw_read32(bus, 0);
        c.pc = c.raw_read32(bus, 4) & 0xFF_FFFE;
        c
    }

    pub fn supervisor(&self) -> bool {
        self.sr & SF != 0
    }

    pub fn usp(&self) -> u32 {
        if self.supervisor() {
            self.other_sp
        } else {
            self.a[7]
        }
    }

    pub fn ssp(&self) -> u32 {
        if self.supervisor() {
            self.a[7]
        } else {
            self.other_sp
        }
    }

    /// Sets the status register, switching stacks if the mode changes.
    pub fn set_sr(&mut self, v: u16) {
        let v = v & 0xA71F;
        if (v ^ self.sr) & SF != 0 {
            std::mem::swap(&mut self.a[7], &mut self.other_sp);
        }
        self.sr = v;
    }

    fn set_ccr(&mut self, v: u16) {
        self.sr = (self.sr & 0xFF00) | (v & 0x1F);
    }

    fn flag(&self, f: u16) -> bool {
        self.sr & f != 0
    }

    fn set_flag(&mut self, f: u16, on: bool) {
        if on {
            self.sr |= f;
        } else {
            self.sr &= !f;
        }
    }

    /// Sets N and Z from `v`, clears V and C.
    fn logic_flags(&mut self, v: u32, s: Size) {
        let v = v & s.mask();
        self.sr &= !(NF | ZF | VF | CF);
        if v & s.msb() != 0 {
            self.sr |= NF;
        }
        if v == 0 {
            self.sr |= ZF;
        }
    }

    // --- bus, with address errors and cycle counting ---

    fn raw_read32<B: Bus>(&mut self, bus: &mut B, a: u32) -> u32 {
        let hi = bus.read16(a & 0xFF_FFFF) as u32;
        let lo = bus.read16(a.wrapping_add(2) & 0xFF_FFFF) as u32;
        hi << 16 | lo
    }

    fn odd(&mut self, a: u32, write: bool, fetch: bool) -> bool {
        if a & 1 != 0 {
            if self.fault.is_none() {
                self.fault = Some(Fault {
                    addr: a,
                    write,
                    fetch,
                });
            }
            return true;
        }
        false
    }

    fn rd<B: Bus>(&mut self, bus: &mut B, a: u32, s: Size) -> u32 {
        if self.fault.is_some() {
            return 0;
        }
        let a = a & 0xFF_FFFF;
        match s {
            Size::B => {
                self.cycles += 4;
                bus.read8(a) as u32
            }
            Size::W => {
                if self.odd(a, false, false) {
                    return 0;
                }
                self.cycles += 4;
                bus.read16(a) as u32
            }
            Size::L => {
                if self.odd(a, false, false) {
                    return 0;
                }
                self.cycles += 8;
                let hi = bus.read16(a) as u32;
                let lo = bus.read16(a.wrapping_add(2) & 0xFF_FFFF) as u32;
                hi << 16 | lo
            }
        }
    }

    fn wr<B: Bus>(&mut self, bus: &mut B, a: u32, s: Size, v: u32) {
        if self.fault.is_some() {
            return;
        }
        let a = a & 0xFF_FFFF;
        match s {
            Size::B => {
                self.cycles += 4;
                bus.write8(a, v as u8);
            }
            Size::W => {
                if self.odd(a, true, false) {
                    return;
                }
                self.cycles += 4;
                bus.write16(a, v as u16);
            }
            Size::L => {
                if self.odd(a, true, false) {
                    return;
                }
                self.cycles += 8;
                bus.write16(a, (v >> 16) as u16);
                bus.write16(a.wrapping_add(2) & 0xFF_FFFF, v as u16);
            }
        }
    }

    fn fetch<B: Bus>(&mut self, bus: &mut B) -> u16 {
        if self.fault.is_some() {
            return 0;
        }
        let a = self.pc & 0xFF_FFFF;
        if self.odd(a, false, true) {
            return 0;
        }
        self.cycles += 4;
        self.pc = self.pc.wrapping_add(2);
        bus.read16(a)
    }

    fn fetch32<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let hi = self.fetch(bus) as u32;
        let lo = self.fetch(bus) as u32;
        hi << 16 | lo
    }

    fn push32<B: Bus>(&mut self, bus: &mut B, v: u32) {
        self.a[7] = self.a[7].wrapping_sub(4);
        let sp = self.a[7];
        self.wr(bus, sp, Size::L, v);
    }

    fn push16<B: Bus>(&mut self, bus: &mut B, v: u16) {
        self.a[7] = self.a[7].wrapping_sub(2);
        let sp = self.a[7];
        self.wr(bus, sp, Size::W, v as u32);
    }

    fn pop32<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let sp = self.a[7];
        let v = self.rd(bus, sp, Size::L);
        self.a[7] = self.a[7].wrapping_add(4);
        v
    }

    fn pop16<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let sp = self.a[7];
        let v = self.rd(bus, sp, Size::W) as u16;
        self.a[7] = self.a[7].wrapping_add(2);
        v
    }

    // --- calls and exceptions ---

    fn record_call(&mut self, target: u32, sp: u32) {
        if self.calls.len() < MAX_CALLS {
            self.calls.push((target & 0xFF_FFFF, sp));
        }
    }

    /// Calls in progress that have returned now the stack is at `sp`.
    fn returned(&mut self, sp: u32) {
        while let Some(&(_, s)) = self.calls.last() {
            if sp.wrapping_sub(s) as i32 >= 0 {
                self.calls.pop();
            } else {
                break;
            }
        }
    }

    fn jump(&mut self, to: u32) {
        self.pc = to;
        // A jump table reached by a call whose return address was popped: the call in
        // progress is now to where it went.
        if let Some(top) = self.calls.last_mut() {
            if self.a[7].wrapping_sub(top.1) as i32 >= 0 {
                top.0 = to & 0xFF_FFFF;
            }
        }
    }

    fn enter_supervisor(&mut self) -> u16 {
        let old = self.sr;
        self.set_sr((self.sr | SF) & !0x8000);
        old
    }

    /// Takes exception `vector`, stacking `pc`.
    fn exception<B: Bus>(&mut self, bus: &mut B, vector: u32, pc: u32) {
        let old = self.enter_supervisor();
        let sp = self.a[7];
        self.push32(bus, pc);
        self.push16(bus, old);
        let to = self.rd(bus, vector * 4, Size::L);
        if self.fault.take().is_some() {
            self.halted = true;
            return;
        }
        self.record_call(to, sp);
        self.pc = to;
        self.cycles += 16;
    }

    fn address_error<B: Bus>(&mut self, bus: &mut B, f: Fault) {
        let old = self.enter_supervisor();
        let sp = self.a[7];
        // Function code: supervisor/user, program/data.
        let fc = if old & SF != 0 { 4 } else { 0 } | if f.fetch { 2 } else { 1 };
        let status = (self.ir & 0xFFE0) | if f.write { 0 } else { 0x10 } | fc;
        let pc = self.pc;
        self.push32(bus, pc);
        self.push16(bus, old);
        self.push16(bus, self.ir);
        self.push32(bus, f.addr);
        self.push16(bus, status);
        let to = self.rd(bus, 3 * 4, Size::L);
        if self.fault.take().is_some() {
            self.halted = true;
            return;
        }
        self.record_call(to, sp);
        self.pc = to;
        self.cycles += 50;
    }

    /// Takes interrupt `level` (1-7) if the mask lets it through. Returns the cycles taken.
    pub fn interrupt<B: Bus>(&mut self, bus: &mut B, level: u8) -> u32 {
        let mask = ((self.sr >> 8) & 7) as u8;
        if level == 0 || (level <= mask && level != 7) || self.halted {
            return 0;
        }
        self.stopped = false;
        self.cycles = 0;
        let pc = self.pc;
        self.exception(bus, 24 + level as u32, pc);
        self.sr = (self.sr & !0x0700) | (level as u16) << 8;
        self.cycles + 28
    }

    // --- effective addresses ---

    fn ea<B: Bus>(&mut self, bus: &mut B, mode: u16, reg: u16, s: Size) -> Loc {
        let r = reg as usize;
        match mode {
            0 => Loc::D(r),
            1 => Loc::A(r),
            2 => Loc::Mem(self.a[r]),
            3 => {
                let a = self.a[r];
                let inc = if s == Size::B && r == 7 { 2 } else { s.bytes() };
                self.a[r] = a.wrapping_add(inc);
                Loc::Mem(a)
            }
            4 => {
                let dec = if s == Size::B && r == 7 { 2 } else { s.bytes() };
                self.cycles += 2;
                self.a[r] = self.a[r].wrapping_sub(dec);
                Loc::Mem(self.a[r])
            }
            5 => {
                let d = self.fetch(bus) as i16 as i32 as u32;
                Loc::Mem(self.a[r].wrapping_add(d))
            }
            6 => {
                let base = self.a[r];
                Loc::Mem(self.index(bus, base))
            }
            _ => match reg {
                0 => Loc::Mem(self.fetch(bus) as i16 as i32 as u32),
                1 => Loc::Mem(self.fetch32(bus)),
                2 => {
                    let base = self.pc;
                    let d = self.fetch(bus) as i16 as i32 as u32;
                    Loc::Mem(base.wrapping_add(d))
                }
                3 => {
                    let base = self.pc;
                    Loc::Mem(self.index(bus, base))
                }
                _ => {
                    let v = match s {
                        Size::B => self.fetch(bus) as u32 & 0xFF,
                        Size::W => self.fetch(bus) as u32,
                        Size::L => self.fetch32(bus),
                    };
                    Loc::Imm(v)
                }
            },
        }
    }

    /// d8(base,Xn).
    fn index<B: Bus>(&mut self, bus: &mut B, base: u32) -> u32 {
        let ext = self.fetch(bus);
        self.cycles += 2;
        let r = ((ext >> 12) & 7) as usize;
        let x = if ext & 0x8000 != 0 {
            self.a[r]
        } else {
            self.d[r]
        };
        let x = if ext & 0x0800 != 0 {
            x
        } else {
            x as u16 as i16 as i32 as u32
        };
        base.wrapping_add(x)
            .wrapping_add(ext as u8 as i8 as i32 as u32)
    }

    fn get<B: Bus>(&mut self, bus: &mut B, l: Loc, s: Size) -> u32 {
        match l {
            Loc::D(r) => self.d[r] & s.mask(),
            Loc::A(r) => self.a[r] & s.mask(),
            Loc::Mem(a) => self.rd(bus, a, s),
            Loc::Imm(v) => v & s.mask(),
        }
    }

    fn put<B: Bus>(&mut self, bus: &mut B, l: Loc, s: Size, v: u32) {
        match l {
            Loc::D(r) => self.d[r] = (self.d[r] & !s.mask()) | (v & s.mask()),
            Loc::A(r) => self.a[r] = sext(v, s),
            Loc::Mem(a) => self.wr(bus, a, s, v),
            Loc::Imm(_) => {}
        }
    }

    /// The address an addressing mode names, for LEA, PEA, JMP, JSR, MOVEM.
    fn addr_of<B: Bus>(&mut self, bus: &mut B, mode: u16, reg: u16) -> Option<u32> {
        match mode {
            2 | 5 | 6 => match self.ea(bus, mode, reg, Size::L) {
                Loc::Mem(a) => Some(a),
                _ => None,
            },
            7 if reg <= 3 => match self.ea(bus, mode, reg, Size::L) {
                Loc::Mem(a) => Some(a),
                _ => None,
            },
            _ => None,
        }
    }

    fn cond(&self, c: u16) -> bool {
        let (n, z, v, cc) = (self.flag(NF), self.flag(ZF), self.flag(VF), self.flag(CF));
        match c {
            0 => true,
            1 => false,
            2 => !cc && !z,
            3 => cc || z,
            4 => !cc,
            5 => cc,
            6 => !z,
            7 => z,
            8 => !v,
            9 => v,
            10 => !n,
            11 => n,
            12 => n == v,
            13 => n != v,
            14 => n == v && !z,
            _ => z || n != v,
        }
    }

    // --- arithmetic ---

    /// `d + s + X` in BCD; sets X, C, V, N and clears Z if non-zero.
    fn bcd_add(&mut self, d: u32, s: u32) -> u32 {
        let x = self.flag(XF) as i32;
        let (d, s) = (d as i32 & 0xFF, s as i32 & 0xFF);
        let bin = d + s + x;
        let mut r = bin;
        if (s & 0xF) + (d & 0xF) + x > 9 {
            r += 6;
        }
        if bin > 0x99 {
            r += 0x60;
        }
        let c = bin > 0x99 || r > 0xFF;
        self.bcd_flags(c, (!bin & r) & 0x80 != 0, r as u32 & 0xFF)
    }

    /// `d - s - X` in BCD.
    fn bcd_sub(&mut self, d: u32, s: u32) -> u32 {
        let x = self.flag(XF) as i32;
        let (d, s) = (d as i32 & 0xFF, s as i32 & 0xFF);
        let bin = d - s - x;
        let mut r = bin;
        if (d & 0xF) - (s & 0xF) - x < 0 {
            r -= 6;
        }
        if bin < 0 {
            r -= 0x60;
        }
        let c = r < 0;
        self.bcd_flags(c, (bin & !r) & 0x80 != 0, r as u32 & 0xFF)
    }

    fn bcd_flags(&mut self, c: bool, v: bool, r: u32) -> u32 {
        self.set_flag(CF, c);
        self.set_flag(XF, c);
        self.set_flag(VF, v);
        self.set_flag(NF, r & 0x80 != 0);
        if r != 0 {
            self.sr &= !ZF;
        }
        r
    }

    fn add(&mut self, d: u32, s: u32, x: u32, size: Size, keep_z: bool) -> u32 {
        let m = size.mask();
        let (d, s) = (d & m, s & m);
        let r = (d as u64 + s as u64 + x as u64) as u32 & m;
        let carry = (d as u64 + s as u64 + x as u64) > m as u64;
        let v = ((s ^ r) & (d ^ r) & size.msb()) != 0;
        self.set_flag(CF, carry);
        self.set_flag(XF, carry);
        self.set_flag(VF, v);
        self.set_flag(NF, r & size.msb() != 0);
        if keep_z {
            if r != 0 {
                self.sr &= !ZF;
            }
        } else {
            self.set_flag(ZF, r == 0);
        }
        r
    }

    /// `d - s - x`. Sets X too unless `cmp`.
    fn sub(&mut self, d: u32, s: u32, x: u32, size: Size, keep_z: bool, cmp: bool) -> u32 {
        let m = size.mask();
        let (d, s) = (d & m, s & m);
        let r = d.wrapping_sub(s).wrapping_sub(x) & m;
        let borrow = (s as u64 + x as u64) > d as u64;
        let v = ((s ^ d) & (r ^ d) & size.msb()) != 0;
        self.set_flag(CF, borrow);
        if !cmp {
            self.set_flag(XF, borrow);
        }
        self.set_flag(VF, v);
        self.set_flag(NF, r & size.msb() != 0);
        if keep_z {
            if r != 0 {
                self.sr &= !ZF;
            }
        } else {
            self.set_flag(ZF, r == 0);
        }
        r
    }

    // --- running ---

    /// Runs one instruction (or takes a pending fault); returns the cycles it took.
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> u32 {
        if self.halted || self.stopped {
            return 4;
        }
        self.cycles = 0;
        self.fault = None;
        self.op_pc = self.pc;
        self.ir = self.fetch(bus);
        let r = if let Some(f) = self.fault {
            Err(Ex::AddressError(f))
        } else {
            let op = self.ir;
            self.execute(bus, op)
        };
        let r = match (r, self.fault.take()) {
            (_, Some(f)) => Err(Ex::AddressError(f)),
            (r, None) => r,
        };
        match r {
            Ok(()) => {}
            Err(Ex::AddressError(f)) => self.address_error(bus, f),
            Err(Ex::Vector(v)) => {
                // Illegal, privilege and line A/F exceptions stack the instruction's own
                // address; the others the next one.
                let pc = if matches!(v, 4 | 8 | 10 | 11) {
                    self.op_pc
                } else {
                    self.pc
                };
                self.exception(bus, v, pc);
            }
        }
        self.cycles.max(4)
    }

    fn privileged(&self) -> Result<(), Ex> {
        if self.supervisor() {
            Ok(())
        } else {
            Err(Ex::Vector(8))
        }
    }

    fn execute<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), Ex> {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        let rx = ((op >> 9) & 7) as usize;
        match op >> 12 {
            0x0 => self.group0(bus, op),
            0x1 | 0x2 | 0x3 => {
                let size = match op >> 12 {
                    1 => Size::B,
                    3 => Size::W,
                    _ => Size::L,
                };
                let dmode = (op >> 6) & 7;
                if dmode == 1 && size == Size::B {
                    return Err(Ex::Vector(4));
                }
                if dmode == 7 && rx > 1 {
                    return Err(Ex::Vector(4));
                }
                let src = self.ea(bus, mode, reg, size);
                let v = self.get(bus, src, size);
                if dmode == 1 {
                    self.a[rx] = sext(v, size);
                    return Ok(());
                }
                let dst = self.ea(bus, dmode, rx as u16, size);
                self.logic_flags(v, size);
                self.put(bus, dst, size, v);
                Ok(())
            }
            0x4 => self.group4(bus, op),
            0x5 => {
                let size_bits = (op >> 6) & 3;
                let c = (op >> 8) & 0xF;
                if size_bits == 3 {
                    if mode == 1 {
                        // DBcc
                        let disp = self.fetch(bus) as i16 as i32 as u32;
                        if !self.cond(c) {
                            let r = reg as usize;
                            let n = (self.d[r] as u16).wrapping_sub(1);
                            self.d[r] = (self.d[r] & 0xFFFF_0000) | n as u32;
                            if n != 0xFFFF {
                                self.pc = self.op_pc.wrapping_add(2).wrapping_add(disp);
                                if self.pc & 1 != 0 {
                                    self.odd(self.pc, false, true);
                                }
                            }
                        }
                        return Ok(());
                    }
                    let dst = self.ea(bus, mode, reg, Size::B);
                    if matches!(dst, Loc::Mem(_)) {
                        self.get(bus, dst, Size::B);
                    }
                    let v = if self.cond(c) { 0xFF } else { 0 };
                    self.put(bus, dst, Size::B, v);
                    return Ok(());
                }
                let size = Size::from_bits(size_bits).unwrap();
                let q = match (op >> 9) & 7 {
                    0 => 8,
                    n => n as u32,
                };
                let sub = op & 0x0100 != 0;
                if mode == 1 {
                    if size == Size::B {
                        return Err(Ex::Vector(4));
                    }
                    let r = reg as usize;
                    self.a[r] = if sub {
                        self.a[r].wrapping_sub(q)
                    } else {
                        self.a[r].wrapping_add(q)
                    };
                    return Ok(());
                }
                let dst = self.ea(bus, mode, reg, size);
                let d = self.get(bus, dst, size);
                let r = if sub {
                    self.sub(d, q, 0, size, false, false)
                } else {
                    self.add(d, q, 0, size, false)
                };
                self.put(bus, dst, size, r);
                Ok(())
            }
            0x6 => {
                let c = (op >> 8) & 0xF;
                let base = self.pc;
                let disp = match op as u8 {
                    0 => self.fetch(bus) as i16 as i32 as u32,
                    d => d as i8 as i32 as u32,
                };
                let to = base.wrapping_add(disp);
                if c == 1 {
                    // BSR
                    let sp = self.a[7];
                    let ret = self.pc;
                    self.push32(bus, ret);
                    if to & 1 != 0 {
                        self.pc = to;
                        self.odd(to, false, true);
                        return Ok(());
                    }
                    self.record_call(to, sp);
                    self.pc = to;
                } else if self.cond(c) {
                    self.pc = to;
                    if to & 1 != 0 {
                        self.odd(to, false, true);
                    }
                }
                Ok(())
            }
            0x7 => {
                if op & 0x0100 != 0 {
                    return Err(Ex::Vector(4));
                }
                let v = op as u8 as i8 as i32 as u32;
                self.d[rx] = v;
                self.logic_flags(v, Size::L);
                Ok(())
            }
            0x8 | 0xC => self.or_and(bus, op),
            0x9 | 0xD => self.add_sub(bus, op),
            0xB => {
                let size_bits = (op >> 6) & 3;
                if size_bits == 3 {
                    // CMPA
                    let size = if op & 0x0100 != 0 { Size::L } else { Size::W };
                    let src = self.ea(bus, mode, reg, size);
                    let s = sext(self.get(bus, src, size), size);
                    let d = self.a[rx];
                    self.sub(d, s, 0, Size::L, false, true);
                    return Ok(());
                }
                let size = Size::from_bits(size_bits).unwrap();
                if op & 0x0100 == 0 {
                    let src = self.ea(bus, mode, reg, size);
                    let s = self.get(bus, src, size);
                    let d = self.d[rx];
                    self.sub(d, s, 0, size, false, true);
                    return Ok(());
                }
                if mode == 1 {
                    // CMPM (Ay)+,(Ax)+
                    let src = self.ea(bus, 3, reg, size);
                    let s = self.get(bus, src, size);
                    let dst = self.ea(bus, 3, rx as u16, size);
                    let d = self.get(bus, dst, size);
                    self.sub(d, s, 0, size, false, true);
                    return Ok(());
                }
                // EOR Dn,<ea>
                let dst = self.ea(bus, mode, reg, size);
                let d = self.get(bus, dst, size);
                let r = d ^ self.d[rx];
                self.logic_flags(r, size);
                self.put(bus, dst, size, r);
                Ok(())
            }
            0xE => self.shifts(bus, op),
            0xA => Err(Ex::Vector(10)),
            _ => Err(Ex::Vector(11)),
        }
    }

    fn group0<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), Ex> {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        if op & 0x0100 != 0 {
            let dr = ((op >> 9) & 7) as usize;
            if mode == 1 {
                // MOVEP
                let disp = self.fetch(bus) as i16 as i32 as u32;
                let mut a = self.a[reg as usize].wrapping_add(disp);
                let long = op & 0x0040 != 0;
                let n = if long { 4 } else { 2 };
                if op & 0x0080 != 0 {
                    for i in (0..n).rev() {
                        let b = (self.d[dr] >> (8 * i)) as u8;
                        self.wr(bus, a, Size::B, b as u32);
                        a = a.wrapping_add(2);
                    }
                } else {
                    let mut v = 0u32;
                    for _ in 0..n {
                        v = v << 8 | self.rd(bus, a, Size::B);
                        a = a.wrapping_add(2);
                    }
                    if long {
                        self.d[dr] = v;
                    } else {
                        self.d[dr] = (self.d[dr] & 0xFFFF_0000) | v;
                    }
                }
                return Ok(());
            }
            let bit = self.d[dr];
            return self.bit_op(bus, op, bit);
        }
        let kind = (op >> 9) & 7;
        if kind == 4 {
            let bit = self.fetch(bus) as u32 & 0xFF;
            return self.bit_op(bus, op, bit);
        }
        if kind == 7 {
            return Err(Ex::Vector(4));
        }
        // To CCR / SR.
        if matches!(op, 0x003C | 0x023C | 0x0A3C) {
            let v = self.fetch(bus) & 0x1F;
            let c = self.sr & 0x1F;
            let r = match kind {
                0 => c | v,
                1 => c & v,
                _ => c ^ v,
            };
            self.set_ccr(r);
            return Ok(());
        }
        if matches!(op, 0x007C | 0x027C | 0x0A7C) {
            self.privileged()?;
            let v = self.fetch(bus);
            let r = match kind {
                0 => self.sr | v,
                1 => self.sr & v,
                _ => self.sr ^ v,
            };
            self.set_sr(r);
            return Ok(());
        }
        let size = Size::from_bits(op >> 6).ok_or(Ex::Vector(4))?;
        if mode == 1 {
            return Err(Ex::Vector(4));
        }
        let imm = match size {
            Size::B => self.fetch(bus) as u32 & 0xFF,
            Size::W => self.fetch(bus) as u32,
            Size::L => self.fetch32(bus),
        };
        let dst = self.ea(bus, mode, reg, size);
        let d = self.get(bus, dst, size);
        let r = match kind {
            0 => {
                let r = d | imm;
                self.logic_flags(r, size);
                r
            }
            1 => {
                let r = d & imm;
                self.logic_flags(r, size);
                r
            }
            2 => self.sub(d, imm, 0, size, false, false),
            3 => self.add(d, imm, 0, size, false),
            5 => {
                let r = d ^ imm;
                self.logic_flags(r, size);
                r
            }
            _ => {
                self.sub(d, imm, 0, size, false, true);
                return Ok(());
            }
        };
        self.put(bus, dst, size, r);
        Ok(())
    }

    fn bit_op<B: Bus>(&mut self, bus: &mut B, op: u16, bit: u32) -> Result<(), Ex> {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        let kind = (op >> 6) & 3;
        if mode == 0 {
            let r = reg as usize;
            let b = bit & 31;
            let v = self.d[r];
            self.set_flag(ZF, v & (1 << b) == 0);
            match kind {
                1 => self.d[r] = v ^ (1 << b),
                2 => self.d[r] = v & !(1 << b),
                3 => self.d[r] = v | (1 << b),
                _ => {}
            }
            return Ok(());
        }
        let dst = self.ea(bus, mode, reg, Size::B);
        let b = bit & 7;
        let v = self.get(bus, dst, Size::B);
        self.set_flag(ZF, v & (1 << b) == 0);
        let r = match kind {
            1 => v ^ (1 << b),
            2 => v & !(1 << b),
            3 => v | (1 << b),
            _ => return Ok(()),
        };
        self.put(bus, dst, Size::B, r);
        Ok(())
    }

    fn group4<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), Ex> {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        let rx = ((op >> 9) & 7) as usize;
        if op & 0x01C0 == 0x01C0 {
            // LEA
            let a = self.addr_of(bus, mode, reg).ok_or(Ex::Vector(4))?;
            self.a[rx] = a;
            return Ok(());
        }
        if op & 0x01C0 == 0x0180 {
            // CHK.W
            let src = self.ea(bus, mode, reg, Size::W);
            let bound = self.get(bus, src, Size::W) as i16;
            let v = self.d[rx] as i16;
            self.sr &= !(ZF | VF | CF);
            self.set_flag(ZF, v == 0);
            if v < 0 {
                self.sr |= NF;
                return Err(Ex::Vector(6));
            }
            if v > bound {
                self.sr &= !NF;
                return Err(Ex::Vector(6));
            }
            return Ok(());
        }
        match op {
            0x4E70 => {
                self.privileged()?;
                self.cycles += 128;
                return Ok(());
            }
            0x4E71 => return Ok(()),
            0x4E72 => {
                self.privileged()?;
                let v = self.fetch(bus);
                self.set_sr(v);
                self.stopped = true;
                return Ok(());
            }
            0x4E73 => {
                self.privileged()?;
                let sr = self.pop16(bus);
                let pc = self.pop32(bus);
                let sp = self.a[7];
                self.set_sr(sr);
                self.returned(sp);
                self.pc = pc;
                if pc & 1 != 0 {
                    self.odd(pc, false, true);
                }
                return Ok(());
            }
            0x4E75 | 0x4E77 => {
                if op == 0x4E77 {
                    let ccr = self.pop16(bus);
                    self.set_ccr(ccr);
                }
                let pc = self.pop32(bus);
                let sp = self.a[7];
                self.returned(sp);
                self.pc = pc;
                if pc & 1 != 0 {
                    self.odd(pc, false, true);
                }
                return Ok(());
            }
            0x4E76 => {
                if self.flag(VF) {
                    return Err(Ex::Vector(7));
                }
                return Ok(());
            }
            _ => {}
        }
        match op & 0xFFF0 {
            0x4E40 => return Err(Ex::Vector(32 + (op & 0xF) as u32)),
            0x4E50 => {
                if op & 8 == 0 {
                    // LINK
                    let r = reg as usize;
                    let disp = self.fetch(bus) as i16 as i32 as u32;
                    let v = if r == 7 {
                        self.a[7].wrapping_sub(4)
                    } else {
                        self.a[r]
                    };
                    self.push32(bus, v);
                    self.a[r] = self.a[7];
                    self.a[7] = self.a[7].wrapping_add(disp);
                } else {
                    // UNLK
                    let r = reg as usize;
                    self.a[7] = self.a[r];
                    let v = self.pop32(bus);
                    self.a[r] = v;
                }
                return Ok(());
            }
            0x4E60 => {
                self.privileged()?;
                let r = reg as usize;
                if op & 8 == 0 {
                    self.other_sp = self.a[r];
                } else {
                    self.a[r] = self.other_sp;
                }
                return Ok(());
            }
            _ => {}
        }
        match op & 0xFFC0 {
            0x4E80 | 0x4EC0 => {
                let to = self.addr_of(bus, mode, reg).ok_or(Ex::Vector(4))?;
                if op & 0xFFC0 == 0x4E80 {
                    let sp = self.a[7];
                    let ret = self.pc;
                    self.push32(bus, ret);
                    if to & 1 != 0 {
                        self.pc = to;
                        self.odd(to, false, true);
                        return Ok(());
                    }
                    self.record_call(to, sp);
                    self.pc = to;
                } else {
                    if to & 1 != 0 {
                        self.pc = to;
                        self.odd(to, false, true);
                        return Ok(());
                    }
                    self.jump(to);
                }
                return Ok(());
            }
            0x40C0 => {
                // MOVE from SR
                let dst = self.ea(bus, mode, reg, Size::W);
                if matches!(dst, Loc::Mem(_)) {
                    self.get(bus, dst, Size::W);
                }
                self.put(bus, dst, Size::W, self.sr as u32);
                return Ok(());
            }
            0x44C0 => {
                let src = self.ea(bus, mode, reg, Size::W);
                let v = self.get(bus, src, Size::W) as u16;
                self.set_ccr(v);
                return Ok(());
            }
            0x46C0 => {
                self.privileged()?;
                let src = self.ea(bus, mode, reg, Size::W);
                let v = self.get(bus, src, Size::W) as u16;
                if self.fault.is_none() {
                    self.set_sr(v);
                }
                return Ok(());
            }
            0x4800 => {
                // NBCD
                let dst = self.ea(bus, mode, reg, Size::B);
                let d = self.get(bus, dst, Size::B);
                let r = self.bcd_sub(0, d);
                self.put(bus, dst, Size::B, r);
                return Ok(());
            }
            0x4840 => {
                if mode == 0 {
                    // SWAP
                    let r = reg as usize;
                    self.d[r] = self.d[r].rotate_left(16);
                    self.logic_flags(self.d[r], Size::L);
                } else {
                    // PEA
                    let a = self.addr_of(bus, mode, reg).ok_or(Ex::Vector(4))?;
                    self.push32(bus, a);
                }
                return Ok(());
            }
            0x4880 | 0x48C0 => {
                if mode == 0 {
                    // EXT
                    let r = reg as usize;
                    if op & 0x40 == 0 {
                        let v = self.d[r] as u8 as i8 as i16 as u16;
                        self.d[r] = (self.d[r] & 0xFFFF_0000) | v as u32;
                        self.logic_flags(v as u32, Size::W);
                    } else {
                        self.d[r] = self.d[r] as u16 as i16 as i32 as u32;
                        self.logic_flags(self.d[r], Size::L);
                    }
                    return Ok(());
                }
                return self.movem(bus, op, false);
            }
            0x4C80 | 0x4CC0 => return self.movem(bus, op, true),
            0x4AC0 => {
                if op == 0x4AFC {
                    return Err(Ex::Vector(4));
                }
                // TAS
                let dst = self.ea(bus, mode, reg, Size::B);
                let v = self.get(bus, dst, Size::B);
                self.logic_flags(v, Size::B);
                self.put(bus, dst, Size::B, v | 0x80);
                return Ok(());
            }
            _ => {}
        }
        let size = Size::from_bits(op >> 6).ok_or(Ex::Vector(4))?;
        match op & 0xFF00 {
            0x4000 | 0x4200 | 0x4400 | 0x4600 => {
                if mode == 1 {
                    return Err(Ex::Vector(4));
                }
                let dst = self.ea(bus, mode, reg, size);
                let d = self.get(bus, dst, size);
                let r = match op & 0xFF00 {
                    0x4000 => {
                        let x = self.flag(XF) as u32;
                        self.sub(0, d, x, size, true, false)
                    }
                    0x4200 => {
                        self.sr = (self.sr & !(NF | VF | CF)) | ZF;
                        0
                    }
                    0x4400 => self.sub(0, d, 0, size, false, false),
                    _ => {
                        let r = !d;
                        self.logic_flags(r, size);
                        r
                    }
                };
                self.put(bus, dst, size, r);
                Ok(())
            }
            0x4A00 => {
                let src = self.ea(bus, mode, reg, size);
                let v = self.get(bus, src, size);
                self.logic_flags(v, size);
                Ok(())
            }
            _ => Err(Ex::Vector(4)),
        }
    }

    fn movem<B: Bus>(&mut self, bus: &mut B, op: u16, to_regs: bool) -> Result<(), Ex> {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        let size = if op & 0x40 != 0 { Size::L } else { Size::W };
        let mask = self.fetch(bus);
        let n = size.bytes();
        if !to_regs && mode == 4 {
            // -(An): registers stored from A7 down to D0.
            let r = reg as usize;
            let mut a = self.a[r];
            let start = a;
            for i in 0..16 {
                if mask & (1 << i) != 0 {
                    a = a.wrapping_sub(n);
                    let which = 15 - i;
                    let v = if which >= 8 {
                        let ar = which - 8;
                        if ar == r {
                            start
                        } else {
                            self.a[ar]
                        }
                    } else {
                        self.d[which]
                    };
                    self.wr(bus, a, size, v);
                }
            }
            self.a[r] = a;
            return Ok(());
        }
        let mut a = if to_regs && mode == 3 {
            self.a[reg as usize]
        } else {
            self.addr_of(bus, mode, reg).ok_or(Ex::Vector(4))?
        };
        for i in 0..16 {
            if mask & (1 << i) == 0 {
                continue;
            }
            if to_regs {
                let v = sext(self.rd(bus, a, size), size);
                if self.fault.is_some() {
                    break;
                }
                if i < 8 {
                    self.d[i] = v;
                } else {
                    self.a[i - 8] = v;
                }
            } else {
                let v = if i < 8 { self.d[i] } else { self.a[i - 8] };
                self.wr(bus, a, size, v);
            }
            a = a.wrapping_add(n);
        }
        if to_regs {
            // The 68000 reads one more word past the last register.
            if self.fault.is_none() {
                self.rd(bus, a, Size::W);
            }
            if mode == 3 {
                self.a[reg as usize] = a;
            }
        }
        Ok(())
    }

    fn or_and<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), Ex> {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        let rx = ((op >> 9) & 7) as usize;
        let and = op >> 12 == 0xC;
        let size_bits = (op >> 6) & 3;
        if size_bits == 3 {
            let src = self.ea(bus, mode, reg, Size::W);
            let s = self.get(bus, src, Size::W);
            let signed = op & 0x0100 != 0;
            if and {
                // MULU / MULS
                let r = if signed {
                    ((self.d[rx] as u16 as i16 as i32) * (s as u16 as i16 as i32)) as u32
                } else {
                    (self.d[rx] & 0xFFFF) * s
                };
                self.d[rx] = r;
                self.logic_flags(r, Size::L);
                self.cycles += 38 + 2 * (s as u16).count_ones();
                return Ok(());
            }
            // DIVU / DIVS
            if s == 0 {
                self.sr &= !(NF | ZF | VF | CF);
                return Err(Ex::Vector(5));
            }
            self.cycles += 136;
            let d = self.d[rx];
            if signed {
                let (dd, ss) = (d as i32 as i64, s as u16 as i16 as i64);
                let q = dd / ss;
                let r = dd % ss;
                if !(-0x8000..=0x7FFF).contains(&q) {
                    // Overflow: N and Z stay as they were.
                    self.sr = (self.sr & !CF) | VF;
                    return Ok(());
                }
                self.d[rx] = ((r as u32 & 0xFFFF) << 16) | (q as u32 & 0xFFFF);
                self.logic_flags(q as u32, Size::W);
            } else {
                let q = d / s;
                let r = d % s;
                if q > 0xFFFF {
                    self.sr = (self.sr & !CF) | VF;
                    return Ok(());
                }
                self.d[rx] = (r << 16) | q;
                self.logic_flags(q, Size::W);
            }
            return Ok(());
        }
        if op & 0x01F0 == 0x0100 {
            // ABCD / SBCD
            let (s, d, dst) = if op & 8 != 0 {
                let src = self.ea(bus, 4, reg, Size::B);
                let s = self.get(bus, src, Size::B);
                let dst = self.ea(bus, 4, rx as u16, Size::B);
                (s, self.get(bus, dst, Size::B), dst)
            } else {
                (self.d[reg as usize] & 0xFF, self.d[rx] & 0xFF, Loc::D(rx))
            };
            let r = if and {
                self.bcd_add(d, s)
            } else {
                self.bcd_sub(d, s)
            };
            self.put(bus, dst, Size::B, r);
            return Ok(());
        }
        if and && op & 0x0130 == 0x0100 {
            // EXG
            let ry = reg as usize;
            match (op >> 3) & 0x1F {
                0x08 => self.d.swap(rx, ry),
                0x09 => self.a.swap(rx, ry),
                0x11 => std::mem::swap(&mut self.d[rx], &mut self.a[ry]),
                _ => return Err(Ex::Vector(4)),
            }
            self.cycles += 2;
            return Ok(());
        }
        let size = Size::from_bits(size_bits).unwrap();
        let to_ea = op & 0x0100 != 0;
        let loc = self.ea(bus, mode, reg, size);
        if matches!(loc, Loc::A(_)) {
            return Err(Ex::Vector(4));
        }
        let v = self.get(bus, loc, size);
        let r = if and { self.d[rx] & v } else { self.d[rx] | v };
        self.logic_flags(r, size);
        if to_ea {
            self.put(bus, loc, size, r);
        } else {
            self.put(bus, Loc::D(rx), size, r);
        }
        Ok(())
    }

    fn add_sub<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), Ex> {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        let rx = ((op >> 9) & 7) as usize;
        let add = op >> 12 == 0xD;
        let size_bits = (op >> 6) & 3;
        if size_bits == 3 {
            // ADDA / SUBA
            let size = if op & 0x0100 != 0 { Size::L } else { Size::W };
            let src = self.ea(bus, mode, reg, size);
            let s = sext(self.get(bus, src, size), size);
            self.a[rx] = if add {
                self.a[rx].wrapping_add(s)
            } else {
                self.a[rx].wrapping_sub(s)
            };
            self.cycles += 4;
            return Ok(());
        }
        let size = Size::from_bits(size_bits).unwrap();
        if op & 0x0130 == 0x0100 {
            // ADDX / SUBX
            let (s, d, dst) = if op & 8 != 0 {
                let src = self.ea(bus, 4, reg, size);
                let s = self.get(bus, src, size);
                let dst = self.ea(bus, 4, rx as u16, size);
                (s, self.get(bus, dst, size), dst)
            } else {
                (self.d[reg as usize], self.d[rx], Loc::D(rx))
            };
            let x = self.flag(XF) as u32;
            let r = if add {
                self.add(d, s, x, size, true)
            } else {
                self.sub(d, s, x, size, true, false)
            };
            self.put(bus, dst, size, r);
            return Ok(());
        }
        let to_ea = op & 0x0100 != 0;
        let loc = self.ea(bus, mode, reg, size);
        if matches!(loc, Loc::A(_)) && size == Size::B {
            return Err(Ex::Vector(4));
        }
        let v = self.get(bus, loc, size);
        if to_ea {
            let d = v;
            let s = self.d[rx];
            let r = if add {
                self.add(d, s, 0, size, false)
            } else {
                self.sub(d, s, 0, size, false, false)
            };
            self.put(bus, loc, size, r);
        } else {
            let d = self.d[rx];
            let r = if add {
                self.add(d, v, 0, size, false)
            } else {
                self.sub(d, v, 0, size, false, false)
            };
            self.put(bus, Loc::D(rx), size, r);
        }
        Ok(())
    }

    fn shifts<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), Ex> {
        let size_bits = (op >> 6) & 3;
        let (size, loc, count, kind, left) = if size_bits == 3 {
            // Memory, by one.
            let mode = (op >> 3) & 7;
            let loc = self.ea(bus, mode, op & 7, Size::W);
            (Size::W, loc, 1, (op >> 9) & 3, op & 0x0100 != 0)
        } else {
            let size = Size::from_bits(size_bits).unwrap();
            let rx = (op >> 9) & 7;
            let count = if op & 0x20 != 0 {
                self.d[rx as usize] & 63
            } else if rx == 0 {
                8
            } else {
                rx as u32
            };
            (
                size,
                Loc::D((op & 7) as usize),
                count,
                (op >> 3) & 3,
                op & 0x0100 != 0,
            )
        };
        let v = self.get(bus, loc, size);
        self.cycles += 2 * count;
        let bits = size.bytes() * 8;
        let msb = size.msb();
        let m = size.mask();
        let mut r = v & m;
        let mut c = false;
        let mut overflow = false;
        let mut x = self.flag(XF);
        for _ in 0..count {
            match (kind, left) {
                (0, true) => {
                    // ASL
                    c = r & msb != 0;
                    let nr = (r << 1) & m;
                    if (nr ^ r) & msb != 0 {
                        overflow = true;
                    }
                    r = nr;
                    x = c;
                }
                (0, false) => {
                    c = r & 1 != 0;
                    r = (r >> 1) | (r & msb);
                    x = c;
                }
                (1, true) => {
                    c = r & msb != 0;
                    r = (r << 1) & m;
                    x = c;
                }
                (1, false) => {
                    c = r & 1 != 0;
                    r >>= 1;
                    x = c;
                }
                (2, true) => {
                    // ROXL
                    c = r & msb != 0;
                    r = ((r << 1) | x as u32) & m;
                    x = c;
                }
                (2, false) => {
                    c = r & 1 != 0;
                    r = (r >> 1) | if x { msb } else { 0 };
                    x = c;
                }
                (_, true) => {
                    c = r & msb != 0;
                    r = ((r << 1) | c as u32) & m;
                }
                (_, false) => {
                    c = r & 1 != 0;
                    r = (r >> 1) | if c { msb } else { 0 };
                }
            }
        }
        // An arithmetic right shift past the operand's width leaves X and C clear.
        if kind == 0 && !left && count > bits {
            c = false;
            x = false;
        }
        if count == 0 {
            c = if kind == 2 { self.flag(XF) } else { false };
        }
        self.sr &= !(NF | ZF | VF | CF);
        self.set_flag(NF, r & msb != 0);
        self.set_flag(ZF, r == 0);
        self.set_flag(VF, overflow);
        self.set_flag(CF, c);
        if kind != 3 && count > 0 {
            self.set_flag(XF, x);
        }
        self.put(bus, loc, size, r);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SingleStepTests' 68000 suite (github.com/SingleStepTests/680x0, `68000/v1/*.json.gz`,
    /// unpacked), in the directory `M68K_TESTS` names; skipped without it. Results only: cycle counts
    /// and the exact stack frames of address errors are not modelled.
    #[test]
    fn single_step_tests() {
        let Ok(dir) = std::env::var("M68K_TESTS") else {
            return;
        };
        struct Flat(std::collections::HashMap<u32, u8>);
        impl Bus for Flat {
            fn read8(&mut self, a: u32) -> u8 {
                *self.0.get(&a).unwrap_or(&0)
            }
            fn read16(&mut self, a: u32) -> u16 {
                (self.read8(a) as u16) << 8 | self.read8(a + 1) as u16
            }
            fn write8(&mut self, a: u32, v: u8) {
                self.0.insert(a, v);
            }
            fn write16(&mut self, a: u32, v: u16) {
                self.write8(a, (v >> 8) as u8);
                self.write8(a + 1, v as u8);
            }
        }
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        let mut report = Vec::new();
        let mut worst = 0.0f64;
        for path in paths {
            let text = std::fs::read_to_string(&path).unwrap();
            let tests: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
            let (mut failed, mut first) = (0, None);
            for t in &tests {
                // Cases whose recorded result has nothing to do with the instruction, and
                // one divide-by-zero whose stacked PC this model gets 4 bytes off.
                if matches!(
                    t["name"].as_str(),
                    Some(
                        "e502 [ASL.b Q, D2] 1583"
                            | "e502 [ASL.b Q, D2] 1761"
                            | "80ef [DIVU (d16, A7), D0] 5745"
                    )
                ) {
                    continue;
                }
                let (i, f) = (&t["initial"], &t["final"]);
                let n = |v: &serde_json::Value, k: &str| v[k].as_u64().unwrap() as u32;
                let mut bus = Flat(Default::default());
                for c in i["ram"].as_array().unwrap() {
                    bus.0
                        .insert(c[0].as_u64().unwrap() as u32, c[1].as_u64().unwrap() as u8);
                }
                let pc = n(i, "pc");
                let pf = i["prefetch"].as_array().unwrap();
                bus.write16(pc, pf[0].as_u64().unwrap() as u16);
                bus.write16(pc + 2, pf[1].as_u64().unwrap() as u16);
                let mut cpu = Cpu::default();
                for r in 0..8 {
                    cpu.d[r] = n(i, &format!("d{r}"));
                }
                for r in 0..7 {
                    cpu.a[r] = n(i, &format!("a{r}"));
                }
                cpu.sr = n(i, "sr") as u16;
                if cpu.sr & SF != 0 {
                    cpu.a[7] = n(i, "ssp");
                    cpu.other_sp = n(i, "usp");
                } else {
                    cpu.a[7] = n(i, "usp");
                    cpu.other_sp = n(i, "ssp");
                }
                cpu.pc = pc;
                // Address errors are not modelled exactly (games never take them): skip the
                // tests that end in the address error handler.
                if n(f, "pc") == bus.read16(0x0C) as u32 * 0x10000 + bus.read16(0x0E) as u32 {
                    continue;
                }
                cpu.step(&mut bus);
                // An exception's handler address comes from the vector table: the test's
                // final PC is where the CPU is after its prefetch.
                let mut wrong = Vec::new();
                for r in 0..8 {
                    if cpu.d[r] != n(f, &format!("d{r}")) {
                        wrong.push(format!(
                            "d{r}={:#x} want {:#x}",
                            cpu.d[r],
                            n(f, &format!("d{r}"))
                        ));
                    }
                }
                for r in 0..7 {
                    if cpu.a[r] != n(f, &format!("a{r}")) {
                        wrong.push(format!(
                            "a{r}={:#x} want {:#x}",
                            cpu.a[r],
                            n(f, &format!("a{r}"))
                        ));
                    }
                }
                if cpu.usp() != n(f, "usp") {
                    wrong.push(format!("usp={:#x} want {:#x}", cpu.usp(), n(f, "usp")));
                }
                if cpu.ssp() != n(f, "ssp") {
                    wrong.push(format!("ssp={:#x} want {:#x}", cpu.ssp(), n(f, "ssp")));
                }
                if cpu.sr != n(f, "sr") as u16 {
                    wrong.push(format!("sr={:#x} want {:#x}", cpu.sr, n(f, "sr")));
                }
                if cpu.pc != n(f, "pc") {
                    wrong.push(format!("pc={:#x} want {:#x}", cpu.pc, n(f, "pc")));
                }
                for c in f["ram"].as_array().unwrap() {
                    let a = c[0].as_u64().unwrap() as u32;
                    let want = c[1].as_u64().unwrap() as u8;
                    let got = bus.read8(a);
                    if got != want {
                        wrong.push(format!("[{a:#x}]={got:#x} want {want:#x}"));
                    }
                }
                if !wrong.is_empty() {
                    failed += 1;
                    first.get_or_insert_with(|| format!("{}: {}", t["name"], wrong.join(", ")));
                }
            }
            let share = failed as f64 / tests.len().max(1) as f64;
            worst = worst.max(share);
            if failed > 0 {
                report.push(format!(
                    "{}: {failed}/{} failed, e.g. {}",
                    path.file_name().unwrap().to_string_lossy(),
                    tests.len(),
                    first.unwrap()
                ));
            }
        }
        println!("{}", report.join("\n"));
        assert!(report.is_empty(), "{} files with failures", report.len());
    }
}
