//! A 6502 interpreter: the NES CPU (a 2A03, which has no decimal mode), instruction by
//! instruction, with the undocumented opcodes games are known to use.
//!
//! It runs a game's own code to drive the sound chip (see [`super::nes_music`]): no graphics,
//! cycle counts per instruction (page crossings and taken branches included), not per bus
//! access. It also keeps a shadow of the subroutine calls in progress, which is how the music
//! search tells which routine wrote to the sound chip.

/// What the CPU is attached to.
pub trait Bus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, value: u8);
}

const C: u8 = 0x01;
const Z: u8 = 0x02;
const I: u8 = 0x04;
const D: u8 = 0x08;
const B: u8 = 0x10;
const U: u8 = 0x20;
const V: u8 = 0x40;
const N: u8 = 0x80;

/// How an instruction finds its operand.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Imp,
    Acc,
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
    Rel,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Adc,
    And,
    Asl,
    Bcc,
    Bcs,
    Beq,
    Bit,
    Bmi,
    Bne,
    Bpl,
    Brk,
    Bvc,
    Bvs,
    Clc,
    Cld,
    Cli,
    Clv,
    Cmp,
    Cpx,
    Cpy,
    Dec,
    Dex,
    Dey,
    Eor,
    Inc,
    Inx,
    Iny,
    Jmp,
    Jsr,
    Lda,
    Ldx,
    Ldy,
    Lsr,
    Nop,
    Ora,
    Pha,
    Php,
    Pla,
    Plp,
    Rol,
    Ror,
    Rti,
    Rts,
    Sbc,
    Sec,
    Sed,
    Sei,
    Sta,
    Stx,
    Sty,
    Tax,
    Tay,
    Tsx,
    Txa,
    Txs,
    Tya,
    // Undocumented.
    Lax,
    Sax,
    Dcp,
    Isc,
    Slo,
    Rla,
    Sre,
    Rra,
    Anc,
    Alr,
    Arr,
    Sbx,
    Las,
    Ane,
    Lxa,
    Sha,
    Shx,
    Shy,
    Tas,
    Jam,
}

/// Opcode → (operation, mode, base cycles, +1 when an index crosses a page).
const fn table() -> [(Op, Mode, u8, bool); 256] {
    use Mode::*;
    use Op::*;
    let mut t = [(Jam, Imp, 2, false); 256];
    // The eight ALU groups share their addressing modes by column.
    let alu = [Ora, And, Eor, Adc, Sta, Lda, Cmp, Sbc];
    let cols: [(u8, Mode, u8, bool); 8] = [
        (0x01, IndX, 6, false),
        (0x05, Zp, 3, false),
        (0x09, Imm, 2, false),
        (0x0D, Abs, 4, false),
        (0x11, IndY, 5, true),
        (0x15, ZpX, 4, false),
        (0x19, AbsY, 4, true),
        (0x1D, AbsX, 4, true),
    ];
    let mut g = 0;
    while g < 8 {
        let mut c = 0;
        while c < 8 {
            let (low, mode, cycles, page) = cols[c];
            t[g * 0x20 + low as usize] = (alu[g], mode, cycles, page);
            c += 1;
        }
        g += 1;
    }
    // STA: no immediate; indexed stores always take the extra cycle.
    t[0x89] = (Nop, Imm, 2, false);
    t[0x91] = (Sta, IndY, 6, false);
    t[0x99] = (Sta, AbsY, 5, false);
    t[0x9D] = (Sta, AbsX, 5, false);
    // Shifts and INC/DEC: zp, acc, abs, zp,x, abs,x.
    let rmw = [Asl, Rol, Lsr, Ror];
    let mut i = 0;
    while i < 4 {
        let b = i * 0x20;
        t[b + 0x06] = (rmw[i], Zp, 5, false);
        t[b + 0x0A] = (rmw[i], Acc, 2, false);
        t[b + 0x0E] = (rmw[i], Abs, 6, false);
        t[b + 0x16] = (rmw[i], ZpX, 6, false);
        t[b + 0x1E] = (rmw[i], AbsX, 7, false);
        i += 1;
    }
    t[0xC6] = (Dec, Zp, 5, false);
    t[0xCE] = (Dec, Abs, 6, false);
    t[0xD6] = (Dec, ZpX, 6, false);
    t[0xDE] = (Dec, AbsX, 7, false);
    t[0xE6] = (Inc, Zp, 5, false);
    t[0xEE] = (Inc, Abs, 6, false);
    t[0xF6] = (Inc, ZpX, 6, false);
    t[0xFE] = (Inc, AbsX, 7, false);
    // Undocumented read-modify-write groups (SLO RLA SRE RRA, then DCP ISC).
    let ill = [Slo, Rla, Sre, Rra];
    let icols: [(u8, Mode, u8); 7] = [
        (0x03, IndX, 8),
        (0x07, Zp, 5),
        (0x0F, Abs, 6),
        (0x13, IndY, 8),
        (0x17, ZpX, 6),
        (0x1B, AbsY, 7),
        (0x1F, AbsX, 7),
    ];
    let mut g = 0;
    while g < 4 {
        let mut c = 0;
        while c < 7 {
            let (low, mode, cycles) = icols[c];
            t[g * 0x20 + low as usize] = (ill[g], mode, cycles, false);
            c += 1;
        }
        g += 1;
    }
    let mut c = 0;
    while c < 7 {
        let (low, mode, cycles) = icols[c];
        t[0xC0 + low as usize] = (Dcp, mode, cycles, false);
        t[0xE0 + low as usize] = (Isc, mode, cycles, false);
        c += 1;
    }
    // LAX and SAX.
    t[0xA3] = (Lax, IndX, 6, false);
    t[0xA7] = (Lax, Zp, 3, false);
    t[0xAB] = (Lxa, Imm, 2, false);
    t[0xAF] = (Lax, Abs, 4, false);
    t[0xB3] = (Lax, IndY, 5, true);
    t[0xB7] = (Lax, ZpY, 4, false);
    t[0xBF] = (Lax, AbsY, 4, true);
    t[0x83] = (Sax, IndX, 6, false);
    t[0x87] = (Sax, Zp, 3, false);
    t[0x8F] = (Sax, Abs, 4, false);
    t[0x97] = (Sax, ZpY, 4, false);
    // The rest of the undocumented ones.
    t[0x0B] = (Anc, Imm, 2, false);
    t[0x2B] = (Anc, Imm, 2, false);
    t[0x4B] = (Alr, Imm, 2, false);
    t[0x6B] = (Arr, Imm, 2, false);
    t[0xCB] = (Sbx, Imm, 2, false);
    t[0xEB] = (Sbc, Imm, 2, false);
    t[0x8B] = (Ane, Imm, 2, false);
    t[0xBB] = (Las, AbsY, 4, true);
    t[0x93] = (Sha, IndY, 6, false);
    t[0x9F] = (Sha, AbsY, 5, false);
    t[0x9E] = (Shx, AbsY, 5, false);
    t[0x9C] = (Shy, AbsX, 5, false);
    t[0x9B] = (Tas, AbsY, 5, false);
    // NOPs of every width.
    let nops1 = [0x1A, 0x3A, 0x5A, 0x7A, 0xDA, 0xFA, 0xEA];
    let mut i = 0;
    while i < nops1.len() {
        t[nops1[i]] = (Nop, Imp, 2, false);
        i += 1;
    }
    let mut i = 0;
    let imm = [0x80, 0x82, 0xC2, 0xE2];
    while i < imm.len() {
        t[imm[i]] = (Nop, Imm, 2, false);
        i += 1;
    }
    t[0x04] = (Nop, Zp, 3, false);
    t[0x44] = (Nop, Zp, 3, false);
    t[0x64] = (Nop, Zp, 3, false);
    let mut i = 0;
    let zpx = [0x14, 0x34, 0x54, 0x74, 0xD4, 0xF4];
    while i < zpx.len() {
        t[zpx[i]] = (Nop, ZpX, 4, false);
        t[zpx[i] + 8] = (Nop, AbsX, 4, true);
        i += 1;
    }
    t[0x0C] = (Nop, Abs, 4, false);
    // Branches.
    let br = [Bpl, Bmi, Bvc, Bvs, Bcc, Bcs, Bne, Beq];
    let mut i = 0;
    while i < 8 {
        t[i * 0x20 + 0x10] = (br[i], Rel, 2, false);
        i += 1;
    }
    // Everything else, one by one.
    t[0x00] = (Brk, Imp, 7, false);
    t[0x20] = (Jsr, Abs, 6, false);
    t[0x40] = (Rti, Imp, 6, false);
    t[0x60] = (Rts, Imp, 6, false);
    t[0x4C] = (Jmp, Abs, 3, false);
    t[0x6C] = (Jmp, Ind, 5, false);
    t[0x08] = (Php, Imp, 3, false);
    t[0x28] = (Plp, Imp, 4, false);
    t[0x48] = (Pha, Imp, 3, false);
    t[0x68] = (Pla, Imp, 4, false);
    t[0x18] = (Clc, Imp, 2, false);
    t[0x38] = (Sec, Imp, 2, false);
    t[0x58] = (Cli, Imp, 2, false);
    t[0x78] = (Sei, Imp, 2, false);
    t[0xB8] = (Clv, Imp, 2, false);
    t[0xD8] = (Cld, Imp, 2, false);
    t[0xF8] = (Sed, Imp, 2, false);
    t[0x24] = (Bit, Zp, 3, false);
    t[0x2C] = (Bit, Abs, 4, false);
    t[0x84] = (Sty, Zp, 3, false);
    t[0x8C] = (Sty, Abs, 4, false);
    t[0x94] = (Sty, ZpX, 4, false);
    t[0x86] = (Stx, Zp, 3, false);
    t[0x8E] = (Stx, Abs, 4, false);
    t[0x96] = (Stx, ZpY, 4, false);
    t[0x88] = (Dey, Imp, 2, false);
    t[0xCA] = (Dex, Imp, 2, false);
    t[0xC8] = (Iny, Imp, 2, false);
    t[0xE8] = (Inx, Imp, 2, false);
    t[0x8A] = (Txa, Imp, 2, false);
    t[0x98] = (Tya, Imp, 2, false);
    t[0x9A] = (Txs, Imp, 2, false);
    t[0xA8] = (Tay, Imp, 2, false);
    t[0xAA] = (Tax, Imp, 2, false);
    t[0xBA] = (Tsx, Imp, 2, false);
    t[0xA0] = (Ldy, Imm, 2, false);
    t[0xA4] = (Ldy, Zp, 3, false);
    t[0xAC] = (Ldy, Abs, 4, false);
    t[0xB4] = (Ldy, ZpX, 4, false);
    t[0xBC] = (Ldy, AbsX, 4, true);
    t[0xA2] = (Ldx, Imm, 2, false);
    t[0xA6] = (Ldx, Zp, 3, false);
    t[0xAE] = (Ldx, Abs, 4, false);
    t[0xB6] = (Ldx, ZpY, 4, false);
    t[0xBE] = (Ldx, AbsY, 4, true);
    t[0xC0] = (Cpy, Imm, 2, false);
    t[0xC4] = (Cpy, Zp, 3, false);
    t[0xCC] = (Cpy, Abs, 4, false);
    t[0xE0] = (Cpx, Imm, 2, false);
    t[0xE4] = (Cpx, Zp, 3, false);
    t[0xEC] = (Cpx, Abs, 4, false);
    t
}

const OPS: [(Op, Mode, u8, bool); 256] = table();

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
    /// A JAM opcode stopped the CPU; only a reset starts it again.
    pub jammed: bool,
    /// The entry points of the subroutines in progress, outermost first: JSR targets, and the
    /// handler address for an interrupt. RTS and RTI pop them when the stack pointer says the
    /// call returned, so a routine that drops its own return address does not confuse it for
    /// long.
    pub calls: Vec<(u16, u8)>,
    page_crossed: bool,
}

impl Cpu {
    /// The CPU after a reset: interrupts off, the reset vector's address.
    pub fn reset<B: Bus>(bus: &mut B) -> Self {
        let pc = u16::from_le_bytes([bus.read(0xFFFC), bus.read(0xFFFD)]);
        Self {
            a: 0,
            x: 0,
            y: 0,
            s: 0xFD,
            p: I | U,
            pc,
            jammed: false,
            calls: Vec::new(),
            page_crossed: false,
        }
    }

    /// A CPU with the given registers and nothing in progress.
    pub fn with(pc: u16, s: u8) -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            s,
            p: I | U,
            pc,
            jammed: false,
            calls: Vec::new(),
            page_crossed: false,
        }
    }

    fn push<B: Bus>(&mut self, bus: &mut B, v: u8) {
        bus.write(0x0100 | self.s as u16, v);
        self.s = self.s.wrapping_sub(1);
    }

    fn pull<B: Bus>(&mut self, bus: &mut B) -> u8 {
        self.s = self.s.wrapping_add(1);
        bus.read(0x0100 | self.s as u16)
    }

    /// A return: pops every tracked call the stack pointer has now left.
    fn leave(&mut self) {
        // After RTS/RTI the stack pointer is back where it was before the call pushed.
        while let Some(&(_, s)) = self.calls.last() {
            if self.s.wrapping_sub(s) as i8 >= 0 {
                self.calls.pop();
            } else {
                break;
            }
        }
    }

    /// An interrupt: NMI (vector $FFFA) or IRQ ($FFFE). Returns the cycles taken.
    pub fn interrupt<B: Bus>(&mut self, bus: &mut B, nmi: bool) -> u32 {
        if !nmi && self.p & I != 0 {
            return 0;
        }
        self.jammed = false;
        let pc = self.pc;
        self.push(bus, (pc >> 8) as u8);
        self.push(bus, pc as u8);
        let p = (self.p | U) & !B;
        self.push(bus, p);
        self.p |= I;
        let vector = if nmi { 0xFFFA } else { 0xFFFE };
        self.pc = u16::from_le_bytes([bus.read(vector), bus.read(vector + 1)]);
        // The stack pointer before the three pushes.
        let s = self.s.wrapping_add(3);
        if self.calls.len() < MAX_CALLS {
            self.calls.push((self.pc, s));
        }
        7
    }

    fn set_zn(&mut self, v: u8) {
        self.p = (self.p & !(Z | N)) | if v == 0 { Z } else { 0 } | (v & N);
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

    /// The operand's address (not for Imp, Acc, Imm or Rel).
    fn address<B: Bus>(&mut self, bus: &mut B, mode: Mode) -> u16 {
        self.page_crossed = false;
        match mode {
            Mode::Zp => self.fetch(bus) as u16,
            Mode::ZpX => self.fetch(bus).wrapping_add(self.x) as u16,
            Mode::ZpY => self.fetch(bus).wrapping_add(self.y) as u16,
            Mode::Abs => self.fetch16(bus),
            Mode::AbsX | Mode::AbsY => {
                let base = self.fetch16(bus);
                let index = if mode == Mode::AbsX { self.x } else { self.y };
                let addr = base.wrapping_add(index as u16);
                self.page_crossed = (base ^ addr) & 0xFF00 != 0;
                addr
            }
            Mode::Ind => {
                // The page wrap bug: the high byte comes from the same page.
                let ptr = self.fetch16(bus);
                let lo = bus.read(ptr);
                let hi = bus.read((ptr & 0xFF00) | (ptr.wrapping_add(1) & 0x00FF));
                u16::from_le_bytes([lo, hi])
            }
            Mode::IndX => {
                let zp = self.fetch(bus).wrapping_add(self.x);
                u16::from_le_bytes([bus.read(zp as u16), bus.read(zp.wrapping_add(1) as u16)])
            }
            Mode::IndY => {
                let zp = self.fetch(bus);
                let base =
                    u16::from_le_bytes([bus.read(zp as u16), bus.read(zp.wrapping_add(1) as u16)]);
                let addr = base.wrapping_add(self.y as u16);
                self.page_crossed = (base ^ addr) & 0xFF00 != 0;
                addr
            }
            Mode::Imp | Mode::Acc | Mode::Imm | Mode::Rel => unreachable!(),
        }
    }

    fn adc(&mut self, v: u8) {
        let sum = self.a as u16 + v as u16 + (self.p & C) as u16;
        let result = sum as u8;
        self.p = (self.p & !(C | V))
            | if sum > 0xFF { C } else { 0 }
            | if (self.a ^ result) & (v ^ result) & 0x80 != 0 {
                V
            } else {
                0
            };
        self.a = result;
        self.set_zn(result);
    }

    fn compare(&mut self, reg: u8, v: u8) {
        let r = reg.wrapping_sub(v);
        self.p = (self.p & !C) | if reg >= v { C } else { 0 };
        self.set_zn(r);
    }

    fn asl(&mut self, v: u8) -> u8 {
        self.p = (self.p & !C) | (v >> 7);
        let r = v << 1;
        self.set_zn(r);
        r
    }

    fn lsr(&mut self, v: u8) -> u8 {
        self.p = (self.p & !C) | (v & 1);
        let r = v >> 1;
        self.set_zn(r);
        r
    }

    fn rol(&mut self, v: u8) -> u8 {
        let r = (v << 1) | (self.p & C);
        self.p = (self.p & !C) | (v >> 7);
        self.set_zn(r);
        r
    }

    fn ror(&mut self, v: u8) -> u8 {
        let r = (v >> 1) | ((self.p & C) << 7);
        self.p = (self.p & !C) | (v & 1);
        self.set_zn(r);
        r
    }

    /// The store-high-byte family (SHA SHX SHY TAS): value & (high byte of the base + 1); when
    /// the index crossed a page, the high byte of the address becomes that value too.
    fn store_high<B: Bus>(&mut self, bus: &mut B, addr: u16, index: u8, value: u8) {
        let base = addr.wrapping_sub(index as u16);
        let hi = ((base >> 8) as u8).wrapping_add(1);
        let v = value & hi;
        let addr = if (base ^ addr) & 0xFF00 != 0 {
            ((v as u16) << 8) | (addr & 0xFF)
        } else {
            addr
        };
        bus.write(addr, v);
    }

    /// Runs one instruction; returns the cycles it took.
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> u32 {
        if self.jammed {
            return 2;
        }
        let opcode = self.fetch(bus);
        let (op, mode, base, page) = OPS[opcode as usize];
        let mut cycles = base as u32;
        // The operand: an address, or the value itself for Imm/Acc.
        let addr = match mode {
            Mode::Imp | Mode::Acc => 0,
            Mode::Imm => {
                let at = self.pc;
                self.pc = self.pc.wrapping_add(1);
                at
            }
            Mode::Rel => {
                let off = self.fetch(bus) as i8;
                self.pc.wrapping_add(off as u16)
            }
            _ => self.address(bus, mode),
        };
        if page && self.page_crossed {
            cycles += 1;
        }
        use Op::*;
        match op {
            Lda => {
                self.a = bus.read(addr);
                self.set_zn(self.a);
            }
            Ldx => {
                self.x = bus.read(addr);
                self.set_zn(self.x);
            }
            Ldy => {
                self.y = bus.read(addr);
                self.set_zn(self.y);
            }
            Lax => {
                let v = bus.read(addr);
                self.a = v;
                self.x = v;
                self.set_zn(v);
            }
            Sta => bus.write(addr, self.a),
            Stx => bus.write(addr, self.x),
            Sty => bus.write(addr, self.y),
            Sax => bus.write(addr, self.a & self.x),
            Ora => {
                self.a |= bus.read(addr);
                self.set_zn(self.a);
            }
            And => {
                self.a &= bus.read(addr);
                self.set_zn(self.a);
            }
            Eor => {
                self.a ^= bus.read(addr);
                self.set_zn(self.a);
            }
            Adc => {
                let v = bus.read(addr);
                self.adc(v);
            }
            Sbc => {
                let v = bus.read(addr);
                self.adc(!v);
            }
            Cmp => {
                let v = bus.read(addr);
                self.compare(self.a, v);
            }
            Cpx => {
                let v = bus.read(addr);
                self.compare(self.x, v);
            }
            Cpy => {
                let v = bus.read(addr);
                self.compare(self.y, v);
            }
            Bit => {
                let v = bus.read(addr);
                self.p =
                    (self.p & !(Z | V | N)) | if self.a & v == 0 { Z } else { 0 } | (v & (V | N));
            }
            Asl | Lsr | Rol | Ror if mode == Mode::Acc => {
                self.a = match op {
                    Asl => self.asl(self.a),
                    Lsr => self.lsr(self.a),
                    Rol => self.rol(self.a),
                    _ => self.ror(self.a),
                };
            }
            Asl | Lsr | Rol | Ror | Inc | Dec | Slo | Rla | Sre | Rra | Dcp | Isc => {
                let v = bus.read(addr);
                // The dummy write of the old value: mappers see it.
                bus.write(addr, v);
                let r = match op {
                    Asl => self.asl(v),
                    Lsr => self.lsr(v),
                    Rol => self.rol(v),
                    Ror => self.ror(v),
                    Inc => {
                        let r = v.wrapping_add(1);
                        self.set_zn(r);
                        r
                    }
                    Dec => {
                        let r = v.wrapping_sub(1);
                        self.set_zn(r);
                        r
                    }
                    Slo => {
                        let r = self.asl(v);
                        self.a |= r;
                        self.set_zn(self.a);
                        r
                    }
                    Rla => {
                        let r = self.rol(v);
                        self.a &= r;
                        self.set_zn(self.a);
                        r
                    }
                    Sre => {
                        let r = self.lsr(v);
                        self.a ^= r;
                        self.set_zn(self.a);
                        r
                    }
                    Rra => {
                        let r = self.ror(v);
                        self.adc(r);
                        r
                    }
                    Dcp => {
                        let r = v.wrapping_sub(1);
                        self.compare(self.a, r);
                        r
                    }
                    _ => {
                        let r = v.wrapping_add(1);
                        self.adc(!r);
                        r
                    }
                };
                bus.write(addr, r);
            }
            Inx => {
                self.x = self.x.wrapping_add(1);
                self.set_zn(self.x);
            }
            Iny => {
                self.y = self.y.wrapping_add(1);
                self.set_zn(self.y);
            }
            Dex => {
                self.x = self.x.wrapping_sub(1);
                self.set_zn(self.x);
            }
            Dey => {
                self.y = self.y.wrapping_sub(1);
                self.set_zn(self.y);
            }
            Tax => {
                self.x = self.a;
                self.set_zn(self.x);
            }
            Tay => {
                self.y = self.a;
                self.set_zn(self.y);
            }
            Txa => {
                self.a = self.x;
                self.set_zn(self.a);
            }
            Tya => {
                self.a = self.y;
                self.set_zn(self.a);
            }
            Tsx => {
                self.x = self.s;
                self.set_zn(self.x);
            }
            Txs => self.s = self.x,
            Pha => self.push(bus, self.a),
            Php => self.push(bus, self.p | B | U),
            Pla => {
                self.a = self.pull(bus);
                self.set_zn(self.a);
            }
            Plp => self.p = (self.pull(bus) & !B) | U,
            Clc => self.p &= !C,
            Sec => self.p |= C,
            Cli => self.p &= !I,
            Sei => self.p |= I,
            Clv => self.p &= !V,
            Cld => self.p &= !D,
            Sed => self.p |= D,
            Bpl | Bmi | Bvc | Bvs | Bcc | Bcs | Bne | Beq => {
                let taken = match op {
                    Bpl => self.p & N == 0,
                    Bmi => self.p & N != 0,
                    Bvc => self.p & V == 0,
                    Bvs => self.p & V != 0,
                    Bcc => self.p & C == 0,
                    Bcs => self.p & C != 0,
                    Bne => self.p & Z == 0,
                    _ => self.p & Z != 0,
                };
                if taken {
                    cycles += 1;
                    if (addr ^ self.pc) & 0xFF00 != 0 {
                        cycles += 1;
                    }
                    self.pc = addr;
                }
            }
            Jmp => self.pc = addr,
            Jsr => {
                let ret = self.pc.wrapping_sub(1);
                let s = self.s;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = addr;
                if self.calls.len() < MAX_CALLS {
                    self.calls.push((addr, s));
                }
            }
            Rts => {
                let lo = self.pull(bus);
                let hi = self.pull(bus);
                self.pc = u16::from_le_bytes([lo, hi]).wrapping_add(1);
                self.leave();
            }
            Rti => {
                self.p = (self.pull(bus) & !B) | U;
                let lo = self.pull(bus);
                let hi = self.pull(bus);
                self.pc = u16::from_le_bytes([lo, hi]);
                self.leave();
            }
            Brk => {
                let ret = self.pc.wrapping_add(1);
                let s = self.s;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                let p = self.p | B | U;
                self.push(bus, p);
                self.p |= I;
                self.pc = u16::from_le_bytes([bus.read(0xFFFE), bus.read(0xFFFF)]);
                if self.calls.len() < MAX_CALLS {
                    self.calls.push((self.pc, s));
                }
            }
            Nop => {
                if !matches!(mode, Mode::Imp | Mode::Imm) {
                    bus.read(addr);
                }
            }
            Anc => {
                self.a &= bus.read(addr);
                self.set_zn(self.a);
                self.p = (self.p & !C) | (self.a >> 7);
            }
            Alr => {
                self.a &= bus.read(addr);
                self.a = self.lsr(self.a);
            }
            Arr => {
                self.a &= bus.read(addr);
                self.a = (self.a >> 1) | ((self.p & C) << 7);
                self.set_zn(self.a);
                let c = (self.a >> 6) & 1;
                let v = ((self.a >> 6) ^ (self.a >> 5)) & 1;
                self.p = (self.p & !(C | V)) | c | (v << 6);
            }
            Sbx => {
                let v = bus.read(addr);
                let ax = self.a & self.x;
                self.p = (self.p & !C) | if ax >= v { C } else { 0 };
                self.x = ax.wrapping_sub(v);
                self.set_zn(self.x);
            }
            Las => {
                let v = bus.read(addr) & self.s;
                self.a = v;
                self.x = v;
                self.s = v;
                self.set_zn(v);
            }
            Ane => {
                // Unstable: the common magic constant $EE.
                self.a = (self.a | 0xEE) & self.x & bus.read(addr);
                self.set_zn(self.a);
            }
            Lxa => {
                self.a = (self.a | 0xEE) & bus.read(addr);
                self.x = self.a;
                self.set_zn(self.a);
            }
            Sha => self.store_high(bus, addr, self.y, self.a & self.x),
            Shx => self.store_high(bus, addr, self.y, self.x),
            Shy => self.store_high(bus, addr, self.x, self.y),
            Tas => {
                self.s = self.a & self.x;
                self.store_high(bus, addr, self.y, self.a & self.x);
            }
            Jam => {
                self.jammed = true;
                self.pc = self.pc.wrapping_sub(1);
            }
        }
        cycles
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ram(Vec<u8>);
    impl Bus for Ram {
        fn read(&mut self, addr: u16) -> u8 {
            self.0[addr as usize]
        }
        fn write(&mut self, addr: u16, value: u8) {
            self.0[addr as usize] = value;
        }
    }

    #[test]
    fn a_loop_counts_and_calls_are_tracked() {
        // LDX #5; loop: JSR sub; DEX; BNE loop; JAM.  sub: INC $10; RTS
        let mut ram = Ram(vec![0; 0x10000]);
        let code = [0xA2, 0x05, 0x20, 0x00, 0x90, 0xCA, 0xD0, 0xFA, 0x02];
        ram.0[0x8000..0x8000 + code.len()].copy_from_slice(&code);
        ram.0[0x9000..0x9003].copy_from_slice(&[0xE6, 0x10, 0x60]);
        ram.0[0xFFFC] = 0x00;
        ram.0[0xFFFD] = 0x80;
        let mut cpu = Cpu::reset(&mut ram);
        let mut deepest = 0;
        while !cpu.jammed {
            cpu.step(&mut ram);
            deepest = deepest.max(cpu.calls.len());
            if cpu.pc == 0x9002 {
                assert_eq!(cpu.calls.last().map(|c| c.0), Some(0x9000));
            }
        }
        assert_eq!(ram.0[0x10], 5);
        assert_eq!(deepest, 1);
        assert!(cpu.calls.is_empty());
    }

    /// Runs the public SingleStepTests NES 6502 suite (github.com/SingleStepTests/65x02,
    /// folder nes6502/v1, one JSON file per opcode) when `M6502_TESTS` names a folder holding
    /// its files. Registers, memory and cycle counts are compared.
    #[test]
    fn single_step_suite() {
        let Ok(dir) = std::env::var("M6502_TESTS") else {
            return;
        };
        struct Flat(std::collections::HashMap<u16, u8>);
        impl Bus for Flat {
            fn read(&mut self, addr: u16) -> u8 {
                *self.0.get(&addr).unwrap_or(&0)
            }
            fn write(&mut self, addr: u16, value: u8) {
                self.0.insert(addr, value);
            }
        }
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        // The JAM opcodes ($02, $12, ... $F2): the suite models the bus lock-up cycle by
        // cycle; here the CPU simply stops, which is all a game that hits one gets.
        const JAMS: [&str; 12] = [
            "02", "12", "22", "32", "42", "52", "62", "72", "92", "b2", "d2", "f2",
        ];
        paths.retain(|p| {
            !JAMS
                .iter()
                .any(|j| p.file_stem().is_some_and(|s| s.eq_ignore_ascii_case(j)))
        });
        let mut bad = Vec::new();
        for path in paths {
            let tests: Vec<serde_json::Value> =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let (mut failed, mut first) = (0, None);
            for t in &tests {
                let (i, f) = (&t["initial"], &t["final"]);
                let n = |v: &serde_json::Value, k: &str| v[k].as_u64().unwrap();
                let mut bus = Flat(
                    i["ram"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|c| (c[0].as_u64().unwrap() as u16, c[1].as_u64().unwrap() as u8))
                        .collect(),
                );
                let mut cpu = Cpu {
                    a: n(i, "a") as u8,
                    x: n(i, "x") as u8,
                    y: n(i, "y") as u8,
                    s: n(i, "s") as u8,
                    p: n(i, "p") as u8,
                    pc: n(i, "pc") as u16,
                    jammed: false,
                    calls: Vec::new(),
                    page_crossed: false,
                };
                let cycles = cpu.step(&mut bus) as usize;
                let want = t["cycles"].as_array().unwrap().len();
                let regs = [
                    ("a", cpu.a as u64),
                    ("x", cpu.x as u64),
                    ("y", cpu.y as u64),
                    ("s", cpu.s as u64),
                    ("p", cpu.p as u64),
                    ("pc", cpu.pc as u64),
                ];
                let mut wrong: Vec<String> = regs
                    .iter()
                    .filter(|(k, v)| n(f, k) != *v)
                    .map(|(k, v)| format!("{k}={v:#x} want {:#x}", n(f, k)))
                    .collect();
                for c in f["ram"].as_array().unwrap() {
                    let (addr, want) =
                        (c[0].as_u64().unwrap() as u16, c[1].as_u64().unwrap() as u8);
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
}
