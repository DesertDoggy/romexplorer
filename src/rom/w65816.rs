//! A 65C816 interpreter: the SNES main CPU, instruction by instruction.
//!
//! It runs a game's own code just far enough to see what the game sends its sound chip (see
//! [`super::snes_capture`]): no graphics, and timing only roughly right. Every opcode is
//! implemented, including decimal mode and the block moves, because a game's unpacker may use
//! any of them.

/// What the CPU is attached to: 24-bit addresses.
pub trait Bus {
    fn read(&mut self, addr: u32) -> u8;
    fn write(&mut self, addr: u32, value: u8);
}

const C: u8 = 0x01;
const Z: u8 = 0x02;
const I: u8 = 0x04;
const D: u8 = 0x08;
const X: u8 = 0x10;
const M: u8 = 0x20;
const V: u8 = 0x40;
const N: u8 = 0x80;

/// How an instruction finds its operand.
#[derive(Clone, Copy)]
enum Mode {
    Imm,
    ImmX,
    Dp,
    DpX,
    DpY,
    DpInd,
    DpIndX,
    DpIndY,
    DpLong,
    DpLongY,
    Abs,
    AbsX,
    AbsY,
    Long,
    LongX,
    Sr,
    SrIndY,
}


// How cycle counts grow: +1 with a 16-bit accumulator (M1), +2 for a read-modify-write on a
// 16-bit accumulator (M2), +1 with 16-bit index registers (X1), +1 when the direct page is not
// page aligned (DP), +1 when an index crosses a page or index registers are 16-bit (IX), +1 in
// native mode (NAT).
const M1: u8 = 1;
const M2: u8 = 2;
const X1: u8 = 4;
const DP: u8 = 8;
const IX: u8 = 16;
const NAT: u8 = 32;

/// Base cycles (8-bit registers, aligned direct page, emulation mode) and the rules above, per
/// opcode, from the 65816 data sheet. Checked against the SingleStepTests suite.
const CYCLES: [(u8, u8); 256] = {
    let mut t = [(2u8, 0u8); 256];
    // The ALU group: ORA AND EOR ADC (STA) LDA CMP SBC share their addressing modes.
    let alu: [(u8, u8, u8); 15] = [
        (0x01, 6, M1 | DP), (0x03, 4, M1), (0x05, 3, M1 | DP), (0x07, 6, M1 | DP), (0x09, 2, M1), (0x0D, 4, M1),
        (0x0F, 5, M1), (0x11, 5, M1 | DP | IX), (0x12, 5, M1 | DP), (0x13, 7, M1), (0x15, 4, M1 | DP),
        (0x17, 6, M1 | DP), (0x19, 4, M1 | IX), (0x1D, 4, M1 | IX), (0x1F, 5, M1),
    ];
    let mut g = 0;
    while g < 8 {
        let mut i = 0;
        while i < alu.len() {
            let (low, c, r) = alu[i];
            t[(g * 0x20 + low as usize)] = (c, r);
            i += 1;
        }
        g += 1;
    }
    // STA writes without the index penalty, taking the extra cycle always.
    t[0x91] = (6, M1 | DP);
    t[0x99] = (5, M1);
    t[0x9D] = (5, M1);
    t[0x89] = (2, M1);
    // Read-modify-write: ASL ROL LSR ROR (dp, abs, dp,X, abs,X), INC DEC, TSB TRB.
    let rmw: [u8; 8] = [0x00, 0x20, 0x40, 0x60, 0xC0, 0xE0, 0, 0];
    let mut i = 0;
    while i < 6 {
        let b = rmw[i] as usize;
        t[b + 0x06] = (5, M2 | DP);
        t[b + 0x0E] = (6, M2);
        t[b + 0x16] = (6, M2 | DP);
        t[b + 0x1E] = (7, M2);
        i += 1;
    }
    t[0x04] = (5, M2 | DP);
    t[0x0C] = (6, M2);
    t[0x14] = (5, M2 | DP);
    t[0x1C] = (6, M2);
    // BIT, STZ
    t[0x24] = (3, M1 | DP);
    t[0x2C] = (4, M1);
    t[0x34] = (4, M1 | DP);
    t[0x3C] = (4, M1 | IX);
    t[0x64] = (3, M1 | DP);
    t[0x74] = (4, M1 | DP);
    t[0x9C] = (4, M1);
    t[0x9E] = (5, M1);
    // Index loads, stores, compares.
    t[0xA0] = (2, X1);
    t[0xA2] = (2, X1);
    t[0xA4] = (3, X1 | DP);
    t[0xA6] = (3, X1 | DP);
    t[0xAC] = (4, X1);
    t[0xAE] = (4, X1);
    t[0xB4] = (4, X1 | DP);
    t[0xB6] = (4, X1 | DP);
    t[0xBC] = (4, X1 | IX);
    t[0xBE] = (4, X1 | IX);
    t[0x84] = (3, X1 | DP);
    t[0x86] = (3, X1 | DP);
    t[0x8C] = (4, X1);
    t[0x8E] = (4, X1);
    t[0x94] = (4, X1 | DP);
    t[0x96] = (4, X1 | DP);
    t[0xC0] = (2, X1);
    t[0xE0] = (2, X1);
    t[0xC4] = (3, X1 | DP);
    t[0xE4] = (3, X1 | DP);
    t[0xCC] = (4, X1);
    t[0xEC] = (4, X1);
    // Branches and jumps.
    t[0x80] = (2, 0);
    t[0x82] = (4, 0);
    t[0x4C] = (3, 0);
    t[0x5C] = (4, 0);
    t[0x6C] = (5, 0);
    t[0x7C] = (6, 0);
    t[0xDC] = (6, 0);
    t[0x20] = (6, 0);
    t[0xFC] = (8, 0);
    t[0x22] = (8, 0);
    t[0x60] = (6, 0);
    t[0x6B] = (6, 0);
    t[0x40] = (6, NAT);
    t[0x00] = (7, NAT);
    t[0x02] = (7, NAT);
    // Stack.
    t[0x08] = (3, 0);
    t[0x28] = (4, 0);
    t[0x48] = (3, M1);
    t[0x68] = (4, M1);
    t[0xDA] = (3, X1);
    t[0x5A] = (3, X1);
    t[0xFA] = (4, X1);
    t[0x7A] = (4, X1);
    t[0x8B] = (3, 0);
    t[0xAB] = (4, 0);
    t[0x0B] = (4, 0);
    t[0x2B] = (5, 0);
    t[0x4B] = (3, 0);
    t[0xF4] = (5, 0);
    t[0xD4] = (6, DP);
    t[0x62] = (6, 0);
    // The rest of the two- and three-cycle ones.
    t[0xC2] = (3, 0);
    t[0xE2] = (3, 0);
    t[0xEB] = (3, 0);
    t[0xCB] = (4, 0);
    t[0xDB] = (4, 0);
    t
};

#[derive(Debug, Clone)]
pub struct Cpu {
    pub a: u16,
    pub x: u16,
    pub y: u16,
    pub s: u16,
    pub d: u16,
    pub dbr: u8,
    pub pbr: u8,
    pub pc: u16,
    pub p: u8,
    /// Emulation mode (6502 compatible), as after reset.
    pub e: bool,
    /// Waiting for an interrupt (WAI).
    pub waiting: bool,
    /// Stopped for good (STP).
    pub stopped: bool,
    /// Set while an instruction runs: its index crossed a page, or its branch was taken
    /// (2 when that also crossed a page in emulation mode). They cost a cycle each.
    pub page_crossed: bool,
    pub branch_taken: u8,
}

impl Cpu {
    /// The CPU as reset leaves it, about to run from the reset vector.
    pub fn reset(bus: &mut impl Bus) -> Self {
        let lo = bus.read(0xFFFC) as u16;
        let hi = bus.read(0xFFFD) as u16;
        Self { a: 0, x: 0, y: 0, s: 0x01FF, d: 0, dbr: 0, pbr: 0, pc: lo | hi << 8, p: M | X | I, e: true, waiting: false, stopped: false, page_crossed: false, branch_taken: 0 }
    }

    fn m8(&self) -> bool {
        self.p & M != 0
    }

    fn x8(&self) -> bool {
        self.p & X != 0
    }

    fn set(&mut self, flag: u8, on: bool) {
        if on {
            self.p |= flag;
        } else {
            self.p &= !flag;
        }
    }

    fn nz8(&mut self, v: u8) {
        self.set(Z, v == 0);
        self.set(N, v & 0x80 != 0);
    }

    fn nz16(&mut self, v: u16) {
        self.set(Z, v == 0);
        self.set(N, v & 0x8000 != 0);
    }

    fn nz(&mut self, v: u16, wide: bool) {
        if wide {
            self.nz16(v)
        } else {
            self.nz8(v as u8)
        }
    }

    /// After P changes: 8-bit index registers lose their high bytes.
    fn fix_p(&mut self) {
        if self.e {
            self.p |= M | X;
        }
        if self.x8() {
            self.x &= 0xFF;
            self.y &= 0xFF;
        }
    }

    fn fetch(&mut self, bus: &mut impl Bus) -> u8 {
        let v = bus.read((self.pbr as u32) << 16 | self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        v
    }

    fn fetch16(&mut self, bus: &mut impl Bus) -> u16 {
        let lo = self.fetch(bus) as u16;
        lo | (self.fetch(bus) as u16) << 8
    }

    fn fetch24(&mut self, bus: &mut impl Bus) -> u32 {
        let lo = self.fetch16(bus) as u32;
        lo | (self.fetch(bus) as u32) << 16
    }

    fn push(&mut self, bus: &mut impl Bus, v: u8) {
        bus.write(self.s as u32, v);
        self.s = self.s.wrapping_sub(1);
        if self.e {
            self.s = 0x0100 | (self.s & 0xFF);
        }
    }

    fn pull(&mut self, bus: &mut impl Bus) -> u8 {
        self.s = self.s.wrapping_add(1);
        if self.e {
            self.s = 0x0100 | (self.s & 0xFF);
        }
        bus.read(self.s as u32)
    }

    fn push16(&mut self, bus: &mut impl Bus, v: u16) {
        self.push(bus, (v >> 8) as u8);
        self.push(bus, v as u8);
    }

    fn pull16(&mut self, bus: &mut impl Bus) -> u16 {
        let lo = self.pull(bus) as u16;
        lo | (self.pull(bus) as u16) << 8
    }

    fn read16(bus: &mut impl Bus, addr: u32) -> u16 {
        bus.read(addr & 0xFF_FFFF) as u16 | (bus.read(addr.wrapping_add(1) & 0xFF_FFFF) as u16) << 8
    }

    /// A 16-bit pointer in bank 0 (direct page, stack), wrapping inside the bank.
    fn read16_bank0(bus: &mut impl Bus, addr: u16) -> u16 {
        bus.read(addr as u32) as u16 | (bus.read(addr.wrapping_add(1) as u32) as u16) << 8
    }

    fn read24_bank0(bus: &mut impl Bus, addr: u16) -> u32 {
        Self::read16_bank0(bus, addr) as u32 | (bus.read(addr.wrapping_add(2) as u32) as u32) << 16
    }

    fn data(&self, a: u16) -> u32 {
        (self.dbr as u32) << 16 | a as u32
    }

    /// In emulation mode with the direct page on a page boundary, direct page addressing stays
    /// inside that page, as on a 6502.
    fn page_wraps(&self) -> bool {
        self.e && self.d & 0xFF == 0
    }

    fn dp_indexed(&self, off: u8, index: u16) -> u16 {
        if self.page_wraps() {
            self.d | (off as u16).wrapping_add(index) & 0xFF
        } else {
            self.d.wrapping_add(off as u16).wrapping_add(index)
        }
    }

    /// A 16-bit pointer on the direct page.
    fn dp_pointer(&self, bus: &mut impl Bus, p: u16) -> u16 {
        let hi = if self.page_wraps() { (p & 0xFF00) | (p.wrapping_add(1) & 0xFF) } else { p.wrapping_add(1) };
        bus.read(p as u32) as u16 | (bus.read(hi as u32) as u16) << 8
    }

    /// A 16-bit value in `bank`, wrapping inside it.
    fn read16_bank(&self, bus: &mut impl Bus, bank: u8, a: u16) -> u16 {
        let b = (bank as u32) << 16;
        bus.read(b | a as u32) as u16 | (bus.read(b | a.wrapping_add(1) as u32) as u16) << 8
    }

    /// Runs `f` with the full 16-bit stack: the 65816's own stack instructions do not keep the
    /// stack in page 1 in emulation mode until they end.
    fn native_stack<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let e = self.e;
        self.e = false;
        let r = f(self);
        self.e = e;
        if e {
            self.s = 0x0100 | (self.s & 0xFF);
        }
        r
    }

    fn indexed(&mut self, base: u32, index: u16) -> u32 {
        let a = base.wrapping_add(index as u32) & 0xFF_FFFF;
        self.page_crossed = a & 0xFFFF00 != base & 0xFFFF00;
        a
    }

    /// The operand's address (immediate modes never come here).
    fn address(&mut self, bus: &mut impl Bus, mode: Mode) -> u32 {
        match mode {
            Mode::Imm | Mode::ImmX => unreachable!("immediate operands have no address"),
            Mode::Dp => self.d.wrapping_add(self.fetch(bus) as u16) as u32,
            Mode::DpX => {
                let off = self.fetch(bus);
                self.dp_indexed(off, self.x) as u32
            }
            Mode::DpY => {
                let off = self.fetch(bus);
                self.dp_indexed(off, self.y) as u32
            }
            Mode::DpInd => {
                let p = self.d.wrapping_add(self.fetch(bus) as u16);
                let ptr = self.dp_pointer(bus, p);
                self.data(ptr)
            }
            Mode::DpIndX => {
                let off = self.fetch(bus);
                let p = self.dp_indexed(off, self.x);
                let ptr = self.dp_pointer(bus, p);
                self.data(ptr)
            }
            Mode::DpIndY => {
                let p = self.d.wrapping_add(self.fetch(bus) as u16);
                let ptr = self.dp_pointer(bus, p);
                self.indexed(self.data(ptr), self.y)
            }
            Mode::DpLong => {
                let p = self.d.wrapping_add(self.fetch(bus) as u16);
                Self::read24_bank0(bus, p)
            }
            Mode::DpLongY => {
                let p = self.d.wrapping_add(self.fetch(bus) as u16);
                Self::read24_bank0(bus, p).wrapping_add(self.y as u32) & 0xFF_FFFF
            }
            Mode::Abs => {
                let a = self.fetch16(bus);
                self.data(a)
            }
            Mode::AbsX => {
                let a = self.fetch16(bus);
                self.indexed(self.data(a), self.x)
            }
            Mode::AbsY => {
                let a = self.fetch16(bus);
                self.indexed(self.data(a), self.y)
            }
            Mode::Long => self.fetch24(bus),
            Mode::LongX => self.fetch24(bus).wrapping_add(self.x as u32) & 0xFF_FFFF,
            Mode::Sr => self.s.wrapping_add(self.fetch(bus) as u16) as u32,
            Mode::SrIndY => {
                let p = self.s.wrapping_add(self.fetch(bus) as u16);
                self.data(Self::read16_bank0(bus, p)).wrapping_add(self.y as u32) & 0xFF_FFFF
            }
        }
    }

    /// The operand, 8 or 16 bits wide.
    fn operand(&mut self, bus: &mut impl Bus, mode: Mode, wide: bool) -> u16 {
        match mode {
            Mode::Imm | Mode::ImmX => {
                if wide {
                    self.fetch16(bus)
                } else {
                    self.fetch(bus) as u16
                }
            }
            _ => {
                let a = self.address(bus, mode);
                Self::load(bus, a, mode, wide)
            }
        }
    }

    /// The address of an operand's second byte: direct page and stack operands wrap inside
    /// bank 0, the rest run on into the next bank.
    fn next(addr: u32, mode: Mode) -> u32 {
        match mode {
            Mode::Dp | Mode::DpX | Mode::DpY | Mode::Sr => (addr as u16).wrapping_add(1) as u32,
            _ => addr.wrapping_add(1) & 0xFF_FFFF,
        }
    }

    fn load(bus: &mut impl Bus, addr: u32, mode: Mode, wide: bool) -> u16 {
        let lo = bus.read(addr) as u16;
        if wide {
            lo | (bus.read(Self::next(addr, mode)) as u16) << 8
        } else {
            lo
        }
    }

    fn store(bus: &mut impl Bus, addr: u32, mode: Mode, v: u16, wide: bool) {
        bus.write(addr, v as u8);
        if wide {
            bus.write(Self::next(addr, mode), (v >> 8) as u8);
        }
    }

    fn acc(&self) -> u16 {
        if self.m8() {
            self.a & 0xFF
        } else {
            self.a
        }
    }

    fn set_acc(&mut self, v: u16) {
        if self.m8() {
            self.a = (self.a & 0xFF00) | (v & 0xFF);
        } else {
            self.a = v;
        }
    }

    fn set_index(&self, v: u16) -> u16 {
        if self.x8() {
            v & 0xFF
        } else {
            v
        }
    }

    /// ADC, and SBC as ADC of the operand's complement. In decimal mode each digit is adjusted
    /// as it carries; V is taken before the top digit's adjustment, as the 65816 does.
    fn add(&mut self, v: u16, subtract: bool) {
        let wide = !self.m8();
        let (mask, sign): (u32, u32) = if wide { (0xFFFF, 0x8000) } else { (0xFF, 0x80) };
        let a = self.acc() as u32;
        let v = if subtract { !v as u32 & mask } else { v as u32 & mask };
        let mut carry = (self.p & C) as u32;
        let (result, overflow);
        if self.p & D == 0 {
            let r = a + v + carry;
            overflow = !(a ^ v) & (a ^ r) & sign != 0;
            carry = (r > mask) as u32;
            result = r & mask;
        } else {
            let digits = if wide { 4 } else { 2 };
            let (a, v) = (a as i32, v as i32);
            let mut r = 0i32;
            let mut v_flag = false;
            for i in 0..digits {
                let shift = i * 4;
                let digit = 0xFi32 << shift;
                // The lower digits, already added.
                let below = (1i32 << shift) - 1;
                r = (a & digit) + (v & digit) + ((carry as i32) << shift) + (r & below);
                if i == digits - 1 {
                    v_flag = !(a ^ v) & (a ^ r) & sign as i32 != 0;
                }
                let top = digit | below;
                if subtract {
                    if r <= top {
                        r -= 6 << shift;
                    }
                } else if r > (9 << shift) | below {
                    r += 6 << shift;
                }
                carry = (r > top) as u32;
            }
            overflow = v_flag;
            let r = r as u32;
            result = r & mask;
        }
        self.set(C, carry != 0);
        self.set(V, overflow);
        self.set_acc(result as u16);
        self.nz(result as u16, wide);
    }

    fn adc(&mut self, v: u16) {
        self.add(v, false)
    }

    fn sbc(&mut self, v: u16) {
        self.add(v, true)
    }

    fn compare(&mut self, reg: u16, v: u16, wide: bool) {
        let (reg, v) = if wide { (reg, v) } else { (reg & 0xFF, v & 0xFF) };
        let r = reg.wrapping_sub(v);
        self.set(C, reg >= v);
        self.nz(r, wide);
    }

    /// A read-modify-write on memory or, with `None`, the accumulator.
    fn rmw(&mut self, bus: &mut impl Bus, mode: Option<Mode>, f: impl Fn(&mut Self, u16, bool) -> u16) {
        let wide = !self.m8();
        match mode {
            None => {
                let v = self.acc();
                let r = f(self, v, wide);
                self.set_acc(r);
            }
            Some(mode) => {
                let addr = self.address(bus, mode);
                let v = Self::load(bus, addr, mode, wide);
                let r = f(self, v, wide);
                Self::store(bus, addr, mode, r, wide);
            }
        }
    }

    fn branch(&mut self, bus: &mut impl Bus, taken: bool) {
        let off = self.fetch(bus) as i8;
        if taken {
            let to = self.pc.wrapping_add(off as i16 as u16);
            self.branch_taken = if self.e && to & 0xFF00 != self.pc & 0xFF00 { 2 } else { 1 };
            self.pc = to;
        }
    }

    fn interrupt(&mut self, bus: &mut impl Bus, native: u16, emulation: u16) {
        self.interrupt_with(bus, native, emulation, false)
    }

    fn interrupt_with(&mut self, bus: &mut impl Bus, native: u16, emulation: u16, software: bool) {
        if !self.e {
            self.push(bus, self.pbr);
        }
        self.push16(bus, self.pc);
        // In emulation mode bit 4 (B) tells BRK and COP from hardware interrupts, which push
        // it clear.
        let p = if self.e && !software { self.p & !X } else { self.p };
        self.push(bus, p);
        self.p |= I;
        self.p &= !D;
        self.pbr = 0;
        let vector = if self.e { emulation } else { native };
        self.pc = Self::read16(bus, vector as u32);
        self.waiting = false;
    }

    /// Takes the non-maskable interrupt (vertical blank).
    pub fn nmi(&mut self, bus: &mut impl Bus) {
        if !self.stopped {
            self.interrupt(bus, 0xFFEA, 0xFFFA);
        }
    }

    /// Takes a maskable interrupt, if interrupts are enabled.
    pub fn irq(&mut self, bus: &mut impl Bus) {
        if self.p & I == 0 && !self.stopped {
            self.interrupt(bus, 0xFFEE, 0xFFFE);
        } else {
            self.waiting = false;
        }
    }

    /// Runs one instruction. Returns the CPU cycles it took, as the 65816 counts them.
    pub fn step(&mut self, bus: &mut impl Bus) -> u32 {
        if self.stopped || self.waiting {
            return 2;
        }
        let (m16, x16, native, dp_offset) = (!self.m8(), !self.x8(), !self.e, self.d & 0xFF != 0);
        self.page_crossed = false;
        self.branch_taken = 0;
        let op = bus.read((self.pbr as u32) << 16 | self.pc as u32);
        let moved = self.run(bus);
        if op == 0x44 || op == 0x54 {
            return moved;
        }
        let (base, rules) = CYCLES[op as usize];
        let mut n = base as u32;
        if rules & M1 != 0 && m16 {
            n += 1;
        }
        if rules & M2 != 0 && m16 {
            n += 2;
        }
        if rules & X1 != 0 && x16 {
            n += 1;
        }
        if rules & DP != 0 && dp_offset {
            n += 1;
        }
        if rules & IX != 0 && (x16 || self.page_crossed) {
            n += 1;
        }
        if rules & NAT != 0 && native {
            n += 1;
        }
        n + self.branch_taken as u32
    }

    fn run(&mut self, bus: &mut impl Bus) -> u32 {
        let op = self.fetch(bus);
        let m16 = !self.m8();
        let x16 = !self.x8();
        use Mode::*;
        // The eight ALU instructions share one operand layout.
        const ALU_MODES: [(u8, Mode); 15] = [
            (0x01, DpIndX), (0x03, Sr), (0x05, Dp), (0x07, DpLong), (0x09, Imm), (0x0D, Abs), (0x0F, Long),
            (0x11, DpIndY), (0x12, DpInd), (0x13, SrIndY), (0x15, DpX), (0x17, DpLongY), (0x19, AbsY), (0x1D, AbsX),
            (0x1F, LongX),
        ];
        let alu = if op & 0x03 == 0x01 || op & 0x03 == 0x03 || op & 0x1F == 0x12 {
            ALU_MODES.iter().find(|(low, _)| *low == op & 0x1F).map(|(_, m)| *m)
        } else {
            None
        };
        if let Some(mode) = alu {
            let group = op & 0xE0;
            if group == 0x80 {
                // STA (0x89 is BIT #, handled below)
                if op != 0x89 {
                    let addr = self.address(bus, mode);
                    Self::store(bus, addr, mode, self.acc(), m16);
                    return 4;
                }
            } else {
                let v = self.operand(bus, mode, m16);
                match group {
                    0x00 => {
                        let r = self.acc() | v;
                        self.set_acc(r);
                        self.nz(r, m16);
                    }
                    0x20 => {
                        let r = self.acc() & v;
                        self.set_acc(r);
                        self.nz(r, m16);
                    }
                    0x40 => {
                        let r = self.acc() ^ v;
                        self.set_acc(r);
                        self.nz(r, m16);
                    }
                    0x60 => self.adc(v),
                    0xA0 => {
                        self.set_acc(v);
                        self.nz(v, m16);
                    }
                    0xC0 => self.compare(self.acc(), v, m16),
                    _ => self.sbc(v),
                }
                return 4;
            }
        }
        let asl = |c: &mut Self, v: u16, w: bool| {
            let sign = if w { 0x8000 } else { 0x80 };
            c.set(C, v & sign != 0);
            let r = if w { v << 1 } else { (v << 1) & 0xFF };
            c.nz(r, w);
            r
        };
        let lsr = |c: &mut Self, v: u16, w: bool| {
            c.set(C, v & 1 != 0);
            let r = v >> 1;
            c.nz(r, w);
            r
        };
        let rol = |c: &mut Self, v: u16, w: bool| {
            let sign = if w { 0x8000 } else { 0x80 };
            let carry = (c.p & C) as u16;
            c.set(C, v & sign != 0);
            let r = if w { (v << 1) | carry } else { ((v << 1) | carry) & 0xFF };
            c.nz(r, w);
            r
        };
        let ror = |c: &mut Self, v: u16, w: bool| {
            let top = if w { 0x8000 } else { 0x80 };
            let carry = if c.p & C != 0 { top } else { 0 };
            c.set(C, v & 1 != 0);
            let r = (v >> 1) | carry;
            c.nz(r, w);
            r
        };
        let inc = |c: &mut Self, v: u16, w: bool| {
            let r = if w { v.wrapping_add(1) } else { v.wrapping_add(1) & 0xFF };
            c.nz(r, w);
            r
        };
        let dec = |c: &mut Self, v: u16, w: bool| {
            let r = if w { v.wrapping_sub(1) } else { v.wrapping_sub(1) & 0xFF };
            c.nz(r, w);
            r
        };
        match op {
            // shifts and rotates
            0x0A => self.rmw(bus, None, asl),
            0x06 => self.rmw(bus, Some(Dp), asl),
            0x0E => self.rmw(bus, Some(Abs), asl),
            0x16 => self.rmw(bus, Some(DpX), asl),
            0x1E => self.rmw(bus, Some(AbsX), asl),
            0x4A => self.rmw(bus, None, lsr),
            0x46 => self.rmw(bus, Some(Dp), lsr),
            0x4E => self.rmw(bus, Some(Abs), lsr),
            0x56 => self.rmw(bus, Some(DpX), lsr),
            0x5E => self.rmw(bus, Some(AbsX), lsr),
            0x2A => self.rmw(bus, None, rol),
            0x26 => self.rmw(bus, Some(Dp), rol),
            0x2E => self.rmw(bus, Some(Abs), rol),
            0x36 => self.rmw(bus, Some(DpX), rol),
            0x3E => self.rmw(bus, Some(AbsX), rol),
            0x6A => self.rmw(bus, None, ror),
            0x66 => self.rmw(bus, Some(Dp), ror),
            0x6E => self.rmw(bus, Some(Abs), ror),
            0x76 => self.rmw(bus, Some(DpX), ror),
            0x7E => self.rmw(bus, Some(AbsX), ror),
            // increments and decrements
            0x1A => self.rmw(bus, None, inc),
            0xE6 => self.rmw(bus, Some(Dp), inc),
            0xEE => self.rmw(bus, Some(Abs), inc),
            0xF6 => self.rmw(bus, Some(DpX), inc),
            0xFE => self.rmw(bus, Some(AbsX), inc),
            0x3A => self.rmw(bus, None, dec),
            0xC6 => self.rmw(bus, Some(Dp), dec),
            0xCE => self.rmw(bus, Some(Abs), dec),
            0xD6 => self.rmw(bus, Some(DpX), dec),
            0xDE => self.rmw(bus, Some(AbsX), dec),
            0xE8 => {
                self.x = self.set_index(self.x.wrapping_add(1));
                self.nz(self.x, x16);
            }
            0xC8 => {
                self.y = self.set_index(self.y.wrapping_add(1));
                self.nz(self.y, x16);
            }
            0xCA => {
                self.x = self.set_index(self.x.wrapping_sub(1));
                self.nz(self.x, x16);
            }
            0x88 => {
                self.y = self.set_index(self.y.wrapping_sub(1));
                self.nz(self.y, x16);
            }
            // test and set / reset bits
            0x04 | 0x0C | 0x14 | 0x1C => {
                let mode = if op & 0x08 != 0 { Abs } else { Dp };
                let set = op < 0x10;
                let acc = self.acc();
                self.rmw(bus, Some(mode), move |c, v, _| {
                    c.set(Z, v & acc == 0);
                    if set {
                        v | acc
                    } else {
                        v & !acc
                    }
                });
            }
            // BIT
            0x24 | 0x2C | 0x34 | 0x3C | 0x89 => {
                let mode = match op {
                    0x24 => Dp,
                    0x2C => Abs,
                    0x34 => DpX,
                    0x3C => AbsX,
                    _ => Imm,
                };
                let v = self.operand(bus, mode, m16);
                self.set(Z, v & self.acc() == 0);
                if op != 0x89 {
                    let (n, o) = if m16 { (0x8000, 0x4000) } else { (0x80, 0x40) };
                    self.set(N, v & n != 0);
                    self.set(V, v & o != 0);
                }
            }
            // STZ
            0x64 | 0x74 | 0x9C | 0x9E => {
                let mode = match op {
                    0x64 => Dp,
                    0x74 => DpX,
                    0x9C => Abs,
                    _ => AbsX,
                };
                let addr = self.address(bus, mode);
                Self::store(bus, addr, mode, 0, m16);
            }
            // index loads, stores and compares
            0xA2 | 0xA6 | 0xAE | 0xB6 | 0xBE => {
                let mode = match op {
                    0xA2 => ImmX,
                    0xA6 => Dp,
                    0xAE => Abs,
                    0xB6 => DpY,
                    _ => AbsY,
                };
                let v = self.operand(bus, mode, x16);
                self.x = v;
                self.nz(v, x16);
            }
            0xA0 | 0xA4 | 0xAC | 0xB4 | 0xBC => {
                let mode = match op {
                    0xA0 => ImmX,
                    0xA4 => Dp,
                    0xAC => Abs,
                    0xB4 => DpX,
                    _ => AbsX,
                };
                let v = self.operand(bus, mode, x16);
                self.y = v;
                self.nz(v, x16);
            }
            0x86 | 0x8E | 0x96 => {
                let mode = match op {
                    0x86 => Dp,
                    0x8E => Abs,
                    _ => DpY,
                };
                let addr = self.address(bus, mode);
                Self::store(bus, addr, mode, self.x, x16);
            }
            0x84 | 0x8C | 0x94 => {
                let mode = match op {
                    0x84 => Dp,
                    0x8C => Abs,
                    _ => DpX,
                };
                let addr = self.address(bus, mode);
                Self::store(bus, addr, mode, self.y, x16);
            }
            0xE0 | 0xE4 | 0xEC => {
                let mode = match op {
                    0xE0 => ImmX,
                    0xE4 => Dp,
                    _ => Abs,
                };
                let v = self.operand(bus, mode, x16);
                self.compare(self.x, v, x16);
            }
            0xC0 | 0xC4 | 0xCC => {
                let mode = match op {
                    0xC0 => ImmX,
                    0xC4 => Dp,
                    _ => Abs,
                };
                let v = self.operand(bus, mode, x16);
                self.compare(self.y, v, x16);
            }
            // branches
            0x10 => self.branch(bus, self.p & N == 0),
            0x30 => self.branch(bus, self.p & N != 0),
            0x50 => self.branch(bus, self.p & V == 0),
            0x70 => self.branch(bus, self.p & V != 0),
            0x90 => self.branch(bus, self.p & C == 0),
            0xB0 => self.branch(bus, self.p & C != 0),
            0xD0 => self.branch(bus, self.p & Z == 0),
            0xF0 => self.branch(bus, self.p & Z != 0),
            0x80 => self.branch(bus, true),
            0x82 => {
                let off = self.fetch16(bus);
                self.pc = self.pc.wrapping_add(off);
            }
            // jumps, calls and returns
            0x4C => self.pc = self.fetch16(bus),
            0x5C => {
                let t = self.fetch24(bus);
                self.pc = t as u16;
                self.pbr = (t >> 16) as u8;
            }
            0x6C => {
                let p = self.fetch16(bus);
                self.pc = Self::read16_bank0(bus, p);
            }
            0x7C => {
                let p = self.fetch16(bus).wrapping_add(self.x);
                self.pc = self.read16_bank(bus, self.pbr, p);
            }
            0xDC => {
                let p = self.fetch16(bus);
                let t = Self::read24_bank0(bus, p);
                self.pc = t as u16;
                self.pbr = (t >> 16) as u8;
            }
            0x20 => {
                let t = self.fetch16(bus);
                self.push16(bus, self.pc.wrapping_sub(1));
                self.pc = t;
            }
            0xFC => self.native_stack(|c| {
                // The return address goes on the stack between the operand's two bytes.
                let lo = c.fetch(bus) as u16;
                c.push16(bus, c.pc);
                let hi = c.fetch(bus) as u16;
                let p = (lo | hi << 8).wrapping_add(c.x);
                c.pc = c.read16_bank(bus, c.pbr, p);
            }),
            0x22 => self.native_stack(|c| {
                let t = c.fetch24(bus);
                c.push(bus, c.pbr);
                c.push16(bus, c.pc.wrapping_sub(1));
                c.pc = t as u16;
                c.pbr = (t >> 16) as u8;
            }),
            0x60 => self.pc = self.pull16(bus).wrapping_add(1),
            0x6B => self.native_stack(|c| {
                c.pc = c.pull16(bus).wrapping_add(1);
                c.pbr = c.pull(bus);
            }),
            0x40 => {
                self.p = self.pull(bus);
                if self.e {
                    self.p |= M | X;
                }
                self.fix_p();
                self.pc = self.pull16(bus);
                if !self.e {
                    self.pbr = self.pull(bus);
                }
            }
            0x00 | 0x02 => {
                self.fetch(bus);
                if op == 0x00 {
                    self.interrupt_with(bus, 0xFFE6, 0xFFFE, true);
                } else {
                    self.interrupt_with(bus, 0xFFE4, 0xFFF4, true);
                }
            }
            // stack
            0x08 => self.push(bus, self.p),
            0x28 => {
                self.p = self.pull(bus);
                self.fix_p();
            }
            0x48 => {
                if m16 {
                    self.push16(bus, self.a)
                } else {
                    self.push(bus, self.a as u8)
                }
            }
            0x68 => {
                let v = if m16 { self.pull16(bus) } else { self.pull(bus) as u16 };
                self.set_acc(v);
                self.nz(v, m16);
            }
            0xDA => {
                if x16 {
                    self.push16(bus, self.x)
                } else {
                    self.push(bus, self.x as u8)
                }
            }
            0xFA => {
                self.x = if x16 { self.pull16(bus) } else { self.pull(bus) as u16 };
                self.nz(self.x, x16);
            }
            0x5A => {
                if x16 {
                    self.push16(bus, self.y)
                } else {
                    self.push(bus, self.y as u8)
                }
            }
            0x7A => {
                self.y = if x16 { self.pull16(bus) } else { self.pull(bus) as u16 };
                self.nz(self.y, x16);
            }
            0x8B => self.push(bus, self.dbr),
            0xAB => self.native_stack(|c| {
                c.dbr = c.pull(bus);
                c.nz8(c.dbr);
            }),
            0x0B => self.native_stack(|c| c.push16(bus, c.d)),
            0x2B => self.native_stack(|c| {
                c.d = c.pull16(bus);
                c.nz16(c.d);
            }),
            0x4B => self.push(bus, self.pbr),
            0xF4 => self.native_stack(|c| {
                let v = c.fetch16(bus);
                c.push16(bus, v);
            }),
            0xD4 => self.native_stack(|c| {
                let p = c.d.wrapping_add(c.fetch(bus) as u16);
                let v = Self::read16_bank0(bus, p);
                c.push16(bus, v);
            }),
            0x62 => self.native_stack(|c| {
                let off = c.fetch16(bus);
                let v = c.pc.wrapping_add(off);
                c.push16(bus, v);
            }),
            // flags
            0x18 => self.p &= !C,
            0x38 => self.p |= C,
            0x58 => self.p &= !I,
            0x78 => self.p |= I,
            0xB8 => self.p &= !V,
            0xD8 => self.p &= !D,
            0xF8 => self.p |= D,
            0xC2 => {
                let v = self.fetch(bus);
                self.p &= !v;
                self.fix_p();
            }
            0xE2 => {
                let v = self.fetch(bus);
                self.p |= v;
                self.fix_p();
            }
            0xFB => {
                let carry = self.p & C != 0;
                self.set(C, self.e);
                self.e = carry;
                if self.e {
                    self.p |= M | X;
                    self.s = 0x0100 | (self.s & 0xFF);
                }
                self.fix_p();
            }
            // transfers
            0xAA => {
                self.x = self.set_index(self.a);
                self.nz(self.x, x16);
            }
            0xA8 => {
                self.y = self.set_index(self.a);
                self.nz(self.y, x16);
            }
            0x8A => {
                self.set_acc(self.x);
                self.nz(self.acc(), m16);
            }
            0x98 => {
                self.set_acc(self.y);
                self.nz(self.acc(), m16);
            }
            0xBA => {
                self.x = self.set_index(self.s);
                self.nz(self.x, x16);
            }
            0x9A => {
                self.s = if self.e { 0x0100 | (self.x & 0xFF) } else { self.x };
            }
            0x9B => {
                self.y = self.x;
                self.nz(self.y, x16);
            }
            0xBB => {
                self.x = self.y;
                self.nz(self.x, x16);
            }
            0x5B => {
                self.d = self.a;
                self.nz16(self.d);
            }
            0x7B => {
                self.a = self.d;
                self.nz16(self.a);
            }
            0x1B => {
                self.s = if self.e { 0x0100 | (self.a & 0xFF) } else { self.a };
            }
            0x3B => {
                self.a = self.s;
                self.nz16(self.a);
            }
            0xEB => {
                self.a = self.a.rotate_left(8);
                self.nz8(self.a as u8);
            }
            // block moves, run to the end in one step
            0x54 | 0x44 => {
                let dst = self.fetch(bus);
                let src = self.fetch(bus);
                self.dbr = dst;
                let step = |r: u16| if op == 0x54 { r.wrapping_add(1) } else { r.wrapping_sub(1) };
                let mut moved = 0u32;
                loop {
                    let v = bus.read((src as u32) << 16 | self.x as u32);
                    bus.write((dst as u32) << 16 | self.y as u32, v);
                    self.x = self.set_index(step(self.x));
                    self.y = self.set_index(step(self.y));
                    self.a = self.a.wrapping_sub(1);
                    moved += 1;
                    if self.a == 0xFFFF {
                        break;
                    }
                }
                return 7 * moved;
            }
            0xEA | 0x42 => {
                if op == 0x42 {
                    self.fetch(bus);
                }
            }
            0xCB => self.waiting = true,
            0xDB => self.stopped = true,
            _ => unreachable!("opcode {op:#04x} is covered by the ALU group"),
        }
        3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 64 KB of RAM in bank 0, the rest mirrored.
    struct Ram(Vec<u8>);

    impl Bus for Ram {
        fn read(&mut self, addr: u32) -> u8 {
            self.0[addr as usize & 0xFFFF]
        }
        fn write(&mut self, addr: u32, value: u8) {
            self.0[addr as usize & 0xFFFF] = value;
        }
    }

    fn run(code: &[u8], steps: usize) -> (Cpu, Ram) {
        let mut ram = Ram(vec![0; 0x10000]);
        ram.0[0x8000..0x8000 + code.len()].copy_from_slice(code);
        ram.0[0xFFFC] = 0x00;
        ram.0[0xFFFD] = 0x80;
        let mut cpu = Cpu::reset(&mut ram);
        for _ in 0..steps {
            cpu.step(&mut ram);
        }
        (cpu, ram)
    }

    #[test]
    fn native_mode_sixteen_bit_arithmetic() {
        // CLC; XCE; REP #$30; LDA #$1234; CLC; ADC #$0F0F; STA $0010; LDX #$0003; DEX; BNE -3
        let code = [0x18, 0xFB, 0xC2, 0x30, 0xA9, 0x34, 0x12, 0x18, 0x69, 0x0F, 0x0F, 0x8D, 0x10, 0x00, 0xA2, 0x03, 0x00, 0xCA, 0xD0, 0xFD];
        let (cpu, ram) = run(&code, 7 + 6);
        assert!(!cpu.e);
        assert_eq!(cpu.a, 0x2143);
        assert_eq!(&ram.0[0x10..0x12], &[0x43, 0x21]);
        assert_eq!(cpu.x, 0);
        assert!(cpu.p & Z != 0);
    }

    #[test]
    fn decimal_mode_adds_in_bcd() {
        // SED; CLC; LDA #$19; ADC #$28
        let (cpu, _) = run(&[0xF8, 0x18, 0xA9, 0x19, 0x69, 0x28], 4);
        assert_eq!(cpu.a & 0xFF, 0x47);
        // SED; SEC; LDA #$40; SBC #$01
        let (cpu, _) = run(&[0xF8, 0x38, 0xA9, 0x40, 0xE9, 0x01], 4);
        assert_eq!(cpu.a & 0xFF, 0x39);
    }

    #[test]
    fn calls_return_and_block_moves_copy() {
        // CLC XCE REP #$30 ; JSR $8010 ; BRA -2 ... at $8010: LDA #$0002 ; LDX #$0100 ; LDY #$0200 ; MVN 0,0 ; RTS
        let mut code = vec![0x18, 0xFB, 0xC2, 0x30, 0x20, 0x10, 0x80, 0x80, 0xFE];
        code.resize(0x10, 0xEA);
        code.extend([0xA9, 0x02, 0x00, 0xA2, 0x00, 0x01, 0xA0, 0x00, 0x02, 0x54, 0x00, 0x00, 0x60]);
        let mut ram = Ram(vec![0; 0x10000]);
        ram.0[0x8000..0x8000 + code.len()].copy_from_slice(&code);
        ram.0[0x0100..0x0103].copy_from_slice(&[7, 8, 9]);
        ram.0[0xFFFC] = 0x00;
        ram.0[0xFFFD] = 0x80;
        let mut cpu = Cpu::reset(&mut ram);
        for _ in 0..20 {
            cpu.step(&mut ram);
        }
        assert_eq!(&ram.0[0x0200..0x0203], &[7, 8, 9]);
        assert_eq!(cpu.pc, 0x8007);
    }

    /// Runs the public SingleStepTests 65816 suite (one JSON file per opcode and mode, from
    /// github.com/SingleStepTests/65816) when `W65816_TESTS` names a folder holding its files.
    /// Registers, memory and cycle counts are compared.
    #[test]
    fn single_step_suite() {
        let Ok(dir) = std::env::var("W65816_TESTS") else { return };
        struct Flat(std::collections::HashMap<u32, u8>);
        impl Bus for Flat {
            fn read(&mut self, addr: u32) -> u8 {
                *self.0.get(&addr).unwrap_or(&0)
            }
            fn write(&mut self, addr: u32, value: u8) {
                self.0.insert(addr, value);
            }
        }
        let limit: usize = std::env::var("W65816_TESTS_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
        let mut paths: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
        paths.sort();
        let mut bad_files = Vec::new();
        for path in paths {
            let text = std::fs::read_to_string(&path).unwrap();
            let tests: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
            let mut failed = 0;
            let mut first = None;
            for t in tests.iter().take(limit) {
                let (i, f) = (&t["initial"], &t["final"]);
                let n = |v: &serde_json::Value, k: &str| v[k].as_u64().unwrap();
                let mut bus = Flat(i["ram"].as_array().unwrap().iter().map(|c| (c[0].as_u64().unwrap() as u32, c[1].as_u64().unwrap() as u8)).collect());
                let mut cpu = Cpu {
                    a: n(i, "a") as u16, x: n(i, "x") as u16, y: n(i, "y") as u16, s: n(i, "s") as u16, d: n(i, "d") as u16,
                    dbr: n(i, "dbr") as u8, pbr: n(i, "pbr") as u8, pc: n(i, "pc") as u16, p: n(i, "p") as u8, e: n(i, "e") != 0,
                    waiting: false, stopped: false, page_crossed: false, branch_taken: 0,
                };
                // The suite's starting states leave bits a real CPU cannot hold in them.
                if cpu.e {
                    cpu.s = 0x0100 | (cpu.s & 0xFF);
                }
                cpu.fix_p();
                let cycles = cpu.step(&mut bus) as usize;
                let want_cycles = t["cycles"].as_array().unwrap().len();
                let regs = [
                    ("a", cpu.a as u64), ("x", cpu.x as u64), ("y", cpu.y as u64), ("s", cpu.s as u64), ("d", cpu.d as u64),
                    ("dbr", cpu.dbr as u64), ("pbr", cpu.pbr as u64), ("pc", cpu.pc as u64), ("p", cpu.p as u64), ("e", cpu.e as u64),
                ];
                let mut wrong: Vec<String> = regs.iter().filter(|(k, v)| n(f, k) != *v).map(|(k, v)| format!("{k}={v:#x} want {:#x}", n(f, k))).collect();
                for c in f["ram"].as_array().unwrap() {
                    let (addr, want) = (c[0].as_u64().unwrap() as u32, c[1].as_u64().unwrap() as u8);
                    let got = bus.read(addr);
                    if got != want {
                        wrong.push(format!("[{addr:#08x}]={got:#04x} want {want:#04x}"));
                    }
                }
                if cycles != want_cycles {
                    wrong.push(format!("cycles={cycles} want {want_cycles}"));
                }
                if !wrong.is_empty() {
                    failed += 1;
                    first.get_or_insert_with(|| format!("{}: {}", t["name"], wrong.join(", ")));
                }
            }
            if failed > 0 {
                bad_files.push(format!("{}: {failed} failed, e.g. {}", path.file_name().unwrap().to_string_lossy(), first.unwrap()));
            }
        }
        assert!(bad_files.is_empty(), "{} files with failures:\n{}", bad_files.len(), bad_files.join("\n"));
    }
}
