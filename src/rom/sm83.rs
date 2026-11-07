//! A Sharp SM83 interpreter: the Game Boy CPU, instruction by instruction.
//!
//! It runs a game's own code to drive the sound chip (see [`super::gb_music`]). Cycle counts
//! are per instruction, in machine cycles (4 clocks each). Like [`super::m6502`], it keeps a
//! shadow of the calls in progress, so the music search can tell which routine wrote to the
//! sound chip.

/// What the CPU is attached to.
pub trait Bus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, value: u8);
}

const ZF: u8 = 0x80;
const NF: u8 = 0x40;
const HF: u8 = 0x20;
const CF: u8 = 0x10;

/// How deep the shadow of calls in progress goes.
const MAX_CALLS: usize = 32;

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
    pub sp: u16,
    pub pc: u16,
    pub ime: bool,
    /// EI takes effect after the next instruction.
    ime_pending: bool,
    pub halted: bool,
    /// STOP ran (a speed switch on the Game Boy Color, else a stop until a button).
    pub stopped: bool,
    /// The entry points of the subroutines in progress, outermost first, with the stack
    /// pointer each returns to.
    pub calls: Vec<(u16, u16)>,
}

impl Cpu {
    /// The registers as the boot ROM leaves them (a Game Boy Color when `cgb`).
    pub fn after_boot(cgb: bool) -> Self {
        let mut c = Self {
            sp: 0xFFFE,
            pc: 0x0100,
            ..Default::default()
        };
        if cgb {
            (c.a, c.f, c.b, c.c, c.d, c.e, c.h, c.l) =
                (0x11, 0x80, 0x00, 0x00, 0xFF, 0x56, 0x00, 0x0D);
        } else {
            (c.a, c.f, c.b, c.c, c.d, c.e, c.h, c.l) =
                (0x01, 0xB0, 0x00, 0x13, 0x00, 0xD8, 0x01, 0x4D);
        }
        c
    }

    fn bc(&self) -> u16 {
        u16::from_be_bytes([self.b, self.c])
    }
    fn de(&self) -> u16 {
        u16::from_be_bytes([self.d, self.e])
    }
    fn hl(&self) -> u16 {
        u16::from_be_bytes([self.h, self.l])
    }
    fn set_bc(&mut self, v: u16) {
        [self.b, self.c] = v.to_be_bytes();
    }
    fn set_de(&mut self, v: u16) {
        [self.d, self.e] = v.to_be_bytes();
    }
    fn set_hl(&mut self, v: u16) {
        [self.h, self.l] = v.to_be_bytes();
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

    fn push<B: Bus>(&mut self, bus: &mut B, v: u16) {
        let [hi, lo] = v.to_be_bytes();
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp, hi);
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp, lo);
    }

    fn pop<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let lo = bus.read(self.sp);
        self.sp = self.sp.wrapping_add(1);
        let hi = bus.read(self.sp);
        self.sp = self.sp.wrapping_add(1);
        u16::from_le_bytes([lo, hi])
    }

    /// A call to `target`: pushes the return address and records the call.
    fn call<B: Bus>(&mut self, bus: &mut B, target: u16) {
        let sp = self.sp;
        let pc = self.pc;
        self.push(bus, pc);
        self.pc = target;
        if self.calls.len() < MAX_CALLS {
            self.calls.push((target, sp));
        }
    }

    fn ret<B: Bus>(&mut self, bus: &mut B) {
        self.pc = self.pop(bus);
        while let Some(&(_, sp)) = self.calls.last() {
            // Returned once the stack is back at or above where the call started.
            if self.sp.wrapping_sub(sp) as i16 >= 0 {
                self.calls.pop();
            } else {
                break;
            }
        }
    }

    /// Takes interrupt `bit` (0 VBlank, 1 STAT, 2 timer, 3 serial, 4 joypad) if enabled.
    /// The caller has checked IE & IF; this clears the IF bit through `ack`. Returns the
    /// machine cycles taken.
    pub fn interrupt<B: Bus>(&mut self, bus: &mut B, bit: u8) -> u32 {
        self.halted = false;
        if !self.ime {
            return 0;
        }
        self.ime = false;
        self.ime_pending = false;
        let flags = bus.read(0xFF0F);
        bus.write(0xFF0F, flags & !(1 << bit));
        self.call(bus, 0x40 + 8 * bit as u16);
        5
    }

    fn get_r<B: Bus>(&mut self, bus: &mut B, r: u8) -> u8 {
        match r {
            0 => self.b,
            1 => self.c,
            2 => self.d,
            3 => self.e,
            4 => self.h,
            5 => self.l,
            6 => bus.read(self.hl()),
            _ => self.a,
        }
    }

    fn set_r<B: Bus>(&mut self, bus: &mut B, r: u8, v: u8) {
        match r {
            0 => self.b = v,
            1 => self.c = v,
            2 => self.d = v,
            3 => self.e = v,
            4 => self.h = v,
            5 => self.l = v,
            6 => bus.write(self.hl(), v),
            _ => self.a = v,
        }
    }

    fn get_rp(&self, p: u8) -> u16 {
        match p {
            0 => self.bc(),
            1 => self.de(),
            2 => self.hl(),
            _ => self.sp,
        }
    }

    fn set_rp(&mut self, p: u8, v: u16) {
        match p {
            0 => self.set_bc(v),
            1 => self.set_de(v),
            2 => self.set_hl(v),
            _ => self.sp = v,
        }
    }

    fn cond(&self, y: u8) -> bool {
        match y & 3 {
            0 => self.f & ZF == 0,
            1 => self.f & ZF != 0,
            2 => self.f & CF == 0,
            _ => self.f & CF != 0,
        }
    }

    fn alu(&mut self, op: u8, v: u8) {
        let a = self.a;
        match op {
            0 | 1 => {
                let c = if op == 1 { (self.f & CF != 0) as u8 } else { 0 };
                let r = a as u16 + v as u16 + c as u16;
                self.f = if r as u8 == 0 { ZF } else { 0 }
                    | if (a & 0xF) + (v & 0xF) + c > 0xF {
                        HF
                    } else {
                        0
                    }
                    | if r > 0xFF { CF } else { 0 };
                self.a = r as u8;
            }
            2 | 3 | 7 => {
                let c = if op == 3 { (self.f & CF != 0) as u8 } else { 0 };
                let r = (a as i16) - (v as i16) - c as i16;
                self.f = NF
                    | if r as u8 == 0 { ZF } else { 0 }
                    | if ((a & 0xF) as i16) - ((v & 0xF) as i16) - (c as i16) < 0 {
                        HF
                    } else {
                        0
                    }
                    | if r < 0 { CF } else { 0 };
                if op != 7 {
                    self.a = r as u8;
                }
            }
            4 => {
                self.a &= v;
                self.f = if self.a == 0 { ZF } else { 0 } | HF;
            }
            5 => {
                self.a ^= v;
                self.f = if self.a == 0 { ZF } else { 0 };
            }
            _ => {
                self.a |= v;
                self.f = if self.a == 0 { ZF } else { 0 };
            }
        }
    }

    fn add_sp(&mut self, bus_offset: u8) -> u16 {
        let off = bus_offset as i8 as i16 as u16;
        let sp = self.sp;
        self.f = if (sp & 0xF) + (off & 0xF) > 0xF {
            HF
        } else {
            0
        } | if (sp & 0xFF) + (off & 0xFF) > 0xFF {
            CF
        } else {
            0
        };
        sp.wrapping_add(off)
    }

    fn cb<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let op = self.fetch(bus);
        let (x, y, z) = (op >> 6, (op >> 3) & 7, op & 7);
        let v = self.get_r(bus, z);
        let mem = z == 6;
        match x {
            0 => {
                let c = self.f & CF != 0;
                let (r, out) = match y {
                    0 => (v.rotate_left(1), v >> 7),
                    1 => (v.rotate_right(1), v & 1),
                    2 => ((v << 1) | c as u8, v >> 7),
                    3 => ((v >> 1) | ((c as u8) << 7), v & 1),
                    4 => (v << 1, v >> 7),
                    5 => ((v >> 1) | (v & 0x80), v & 1),
                    6 => (v.rotate_left(4), 0),
                    _ => (v >> 1, v & 1),
                };
                self.f = if r == 0 { ZF } else { 0 } | if out != 0 { CF } else { 0 };
                self.set_r(bus, z, r);
                if mem {
                    4
                } else {
                    2
                }
            }
            1 => {
                self.f = (self.f & CF) | HF | if v & (1 << y) == 0 { ZF } else { 0 };
                if mem {
                    3
                } else {
                    2
                }
            }
            2 => {
                self.set_r(bus, z, v & !(1 << y));
                if mem {
                    4
                } else {
                    2
                }
            }
            _ => {
                self.set_r(bus, z, v | (1 << y));
                if mem {
                    4
                } else {
                    2
                }
            }
        }
    }

    /// Runs one instruction; returns the machine cycles it took (1 while halted).
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> u32 {
        if self.halted || self.stopped {
            return 1;
        }
        if self.ime_pending {
            self.ime_pending = false;
            self.ime = true;
        }
        let op = self.fetch(bus);
        let (x, y, z) = (op >> 6, (op >> 3) & 7, op & 7);
        let (p, q) = (y >> 1, y & 1);
        match x {
            1 => {
                if op == 0x76 {
                    self.halted = true;
                    return 1;
                }
                let v = self.get_r(bus, z);
                self.set_r(bus, y, v);
                if z == 6 || y == 6 {
                    2
                } else {
                    1
                }
            }
            2 => {
                let v = self.get_r(bus, z);
                self.alu(y, v);
                if z == 6 {
                    2
                } else {
                    1
                }
            }
            0 => match z {
                0 => match y {
                    0 => 1,
                    1 => {
                        let a = self.fetch16(bus);
                        let [hi, lo] = self.sp.to_be_bytes();
                        bus.write(a, lo);
                        bus.write(a.wrapping_add(1), hi);
                        5
                    }
                    2 => {
                        self.fetch(bus);
                        self.stopped = true;
                        1
                    }
                    3 => {
                        let off = self.fetch(bus) as i8;
                        self.pc = self.pc.wrapping_add(off as u16);
                        3
                    }
                    _ => {
                        let off = self.fetch(bus) as i8;
                        if self.cond(y - 4) {
                            self.pc = self.pc.wrapping_add(off as u16);
                            3
                        } else {
                            2
                        }
                    }
                },
                1 => {
                    if q == 0 {
                        let v = self.fetch16(bus);
                        self.set_rp(p, v);
                        3
                    } else {
                        let hl = self.hl();
                        let v = self.get_rp(p);
                        let r = hl as u32 + v as u32;
                        self.f = (self.f & ZF)
                            | if (hl & 0xFFF) + (v & 0xFFF) > 0xFFF {
                                HF
                            } else {
                                0
                            }
                            | if r > 0xFFFF { CF } else { 0 };
                        self.set_hl(r as u16);
                        2
                    }
                }
                2 => {
                    let addr = match p {
                        0 => self.bc(),
                        1 => self.de(),
                        _ => self.hl(),
                    };
                    if q == 0 {
                        bus.write(addr, self.a);
                    } else {
                        self.a = bus.read(addr);
                    }
                    match p {
                        2 => self.set_hl(addr.wrapping_add(1)),
                        3 => self.set_hl(addr.wrapping_sub(1)),
                        _ => {}
                    }
                    2
                }
                3 => {
                    let v = self.get_rp(p);
                    self.set_rp(
                        p,
                        if q == 0 {
                            v.wrapping_add(1)
                        } else {
                            v.wrapping_sub(1)
                        },
                    );
                    2
                }
                4 | 5 => {
                    let v = self.get_r(bus, y);
                    let r = if z == 4 {
                        v.wrapping_add(1)
                    } else {
                        v.wrapping_sub(1)
                    };
                    let half = if z == 4 { v & 0xF == 0xF } else { v & 0xF == 0 };
                    self.f = (self.f & CF)
                        | if r == 0 { ZF } else { 0 }
                        | if half { HF } else { 0 }
                        | if z == 5 { NF } else { 0 };
                    self.set_r(bus, y, r);
                    if y == 6 {
                        3
                    } else {
                        1
                    }
                }
                6 => {
                    let v = self.fetch(bus);
                    self.set_r(bus, y, v);
                    if y == 6 {
                        3
                    } else {
                        2
                    }
                }
                _ => {
                    match y {
                        0 => {
                            let c = self.a >> 7;
                            self.a = self.a.rotate_left(1);
                            self.f = if c != 0 { CF } else { 0 };
                        }
                        1 => {
                            let c = self.a & 1;
                            self.a = self.a.rotate_right(1);
                            self.f = if c != 0 { CF } else { 0 };
                        }
                        2 => {
                            let c = self.a >> 7;
                            self.a = (self.a << 1) | ((self.f & CF != 0) as u8);
                            self.f = if c != 0 { CF } else { 0 };
                        }
                        3 => {
                            let c = self.a & 1;
                            self.a = (self.a >> 1) | (((self.f & CF != 0) as u8) << 7);
                            self.f = if c != 0 { CF } else { 0 };
                        }
                        4 => {
                            // DAA
                            let mut a = self.a;
                            let mut carry = self.f & CF != 0;
                            if self.f & NF == 0 {
                                if carry || a > 0x99 {
                                    a = a.wrapping_add(0x60);
                                    carry = true;
                                }
                                if self.f & HF != 0 || a & 0x0F > 9 {
                                    a = a.wrapping_add(6);
                                }
                            } else {
                                if carry {
                                    a = a.wrapping_sub(0x60);
                                }
                                if self.f & HF != 0 {
                                    a = a.wrapping_sub(6);
                                }
                            }
                            self.a = a;
                            self.f = (self.f & NF)
                                | if a == 0 { ZF } else { 0 }
                                | if carry { CF } else { 0 };
                        }
                        5 => {
                            self.a = !self.a;
                            self.f |= NF | HF;
                        }
                        6 => self.f = (self.f & ZF) | CF,
                        _ => self.f = (self.f & ZF) | if self.f & CF != 0 { 0 } else { CF },
                    }
                    1
                }
            },
            _ => match z {
                0 => match y {
                    0..=3 => {
                        if self.cond(y) {
                            self.ret(bus);
                            5
                        } else {
                            2
                        }
                    }
                    4 => {
                        let n = self.fetch(bus);
                        bus.write(0xFF00 | n as u16, self.a);
                        3
                    }
                    5 => {
                        let n = self.fetch(bus);
                        self.sp = self.add_sp(n);
                        4
                    }
                    6 => {
                        let n = self.fetch(bus);
                        self.a = bus.read(0xFF00 | n as u16);
                        3
                    }
                    _ => {
                        let n = self.fetch(bus);
                        let v = self.add_sp(n);
                        self.set_hl(v);
                        3
                    }
                },
                1 => {
                    if q == 0 {
                        let v = self.pop(bus);
                        match p {
                            0 => self.set_bc(v),
                            1 => self.set_de(v),
                            2 => self.set_hl(v),
                            _ => {
                                self.a = (v >> 8) as u8;
                                self.f = v as u8 & 0xF0;
                            }
                        }
                        3
                    } else {
                        match p {
                            0 => {
                                self.ret(bus);
                                4
                            }
                            1 => {
                                self.ret(bus);
                                self.ime = true;
                                4
                            }
                            2 => {
                                self.pc = self.hl();
                                // A jump table reached by RST pops its return address and
                                // jumps on: the call in progress is now to where it went.
                                if let Some(top) = self.calls.last_mut() {
                                    if self.sp.wrapping_sub(top.1) as i16 >= 0 {
                                        top.0 = self.pc;
                                    }
                                }
                                1
                            }
                            _ => {
                                self.sp = self.hl();
                                2
                            }
                        }
                    }
                }
                2 => match y {
                    0..=3 => {
                        let a = self.fetch16(bus);
                        if self.cond(y) {
                            self.pc = a;
                            4
                        } else {
                            3
                        }
                    }
                    4 => {
                        bus.write(0xFF00 | self.c as u16, self.a);
                        2
                    }
                    5 => {
                        let a = self.fetch16(bus);
                        bus.write(a, self.a);
                        4
                    }
                    6 => {
                        self.a = bus.read(0xFF00 | self.c as u16);
                        2
                    }
                    _ => {
                        let a = self.fetch16(bus);
                        self.a = bus.read(a);
                        4
                    }
                },
                3 => match y {
                    0 => {
                        self.pc = self.fetch16(bus);
                        4
                    }
                    1 => self.cb(bus),
                    6 => {
                        self.ime = false;
                        self.ime_pending = false;
                        1
                    }
                    7 => {
                        self.ime_pending = true;
                        1
                    }
                    // D3 DB DD E3 E4 EB EC ED F4 FC FD: no instruction; the CPU locks up.
                    _ => {
                        self.pc = self.pc.wrapping_sub(1);
                        self.halted = true;
                        self.ime = false;
                        1
                    }
                },
                4 => {
                    if y > 3 {
                        self.pc = self.pc.wrapping_sub(1);
                        self.halted = true;
                        self.ime = false;
                        return 1;
                    }
                    let a = self.fetch16(bus);
                    if self.cond(y) {
                        self.call(bus, a);
                        6
                    } else {
                        3
                    }
                }
                5 => {
                    if q == 0 {
                        let v = match p {
                            0 => self.bc(),
                            1 => self.de(),
                            2 => self.hl(),
                            _ => ((self.a as u16) << 8) | self.f as u16,
                        };
                        self.push(bus, v);
                        4
                    } else if p == 0 {
                        let a = self.fetch16(bus);
                        self.call(bus, a);
                        6
                    } else {
                        self.pc = self.pc.wrapping_sub(1);
                        self.halted = true;
                        self.ime = false;
                        1
                    }
                }
                6 => {
                    let v = self.fetch(bus);
                    self.alu(y, v);
                    2
                }
                _ => {
                    self.call(bus, y as u16 * 8);
                    4
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the public SingleStepTests SM83 suite (github.com/SingleStepTests/sm83, folder
    /// v1, one JSON file per opcode) when `SM83_TESTS` names a folder holding its files.
    #[test]
    fn single_step_suite() {
        let Ok(dir) = std::env::var("SM83_TESTS") else {
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
        // STOP and HALT are not single-step instructions in this model.
        paths.retain(|p| !matches!(p.file_stem().and_then(|s| s.to_str()), Some("10" | "76")));
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
                    f: n(i, "f") as u8,
                    b: n(i, "b") as u8,
                    c: n(i, "c") as u8,
                    d: n(i, "d") as u8,
                    e: n(i, "e") as u8,
                    h: n(i, "h") as u8,
                    l: n(i, "l") as u8,
                    sp: n(i, "sp") as u16,
                    pc: n(i, "pc") as u16,
                    ime: n(i, "ime") != 0,
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
                    ("sp", cpu.sp as u64),
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
