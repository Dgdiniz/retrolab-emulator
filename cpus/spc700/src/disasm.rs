//! SPC700 disassembler: the opcode table in [`crate::decode`] rendered
//! as text, in the trace format.
//!
//! The format is fixed, a trace is compared line by line with
//! reference logs, so every byte of it is a behaviour, and
//! `tests/disasm_oracle.rs` pins it over every opcode and a sample of the
//! operand pairs.
//! Its rules:
//!
//! - a five-column mnemonic field (`mov  `, `or   `, `tcall`) with the
//!   operand text written straight after it, so a first operand stands
//!   three columns past a three-letter mnemonic (`mov   x, #$EF`);
//! - operands separated by a comma and a space, except in the handful
//!   of forms the format writes with a bare comma (`$0023,a`, `$010,ya`,
//!   `$0EF,$1234` for the bit branches) and `a,(x)`, which has no
//!   leading space either;
//! - a direct-page address printed with `PSW.P` folded in as a third
//!   hex digit (`$0EF` or `$1EF`; [`DisasmState::p`]), an absolute
//!   address with four, an immediate as `#$EF`, a bit operand as
//!   `$ADDR:BIT` (`!$ADDR:BIT` for an inverted bit) with the bit in
//!   decimal, `tcall`'s vector in decimal and `pcall`'s as `$FFnn`;
//! - a branch target as the instruction's address plus its length plus
//!   the signed offset, printed with at least four hex digits, a sum
//!   that leaves the address space is printed as it is, which is what
//!   the reference logs hold;
//! - the bit-indexed families carry the bit in the name (`set0`...`set7`,
//!   `clr0`..., `bbs0`..., `bbc0`...);
//! - one quirk of the reference format kept on purpose: opcode `$D2`
//!   (`clr6 $dp`) prints its operand as a branch target.
//!
//! Hex digits are uppercase here; the trace formatter lowercases the
//! whole line.

use crate::decode::{Mnemonic, OPCODE_TABLE};
use retro_core::cpu::Addressable;

/// What the disassembler needs beyond the bytes: `PSW.P`, which
/// selects the direct page (`$00xx` or `$01xx`) and is folded into
/// every printed direct-page address.
#[derive(Debug, Clone, Copy, Default)]
pub struct DisasmState {
    /// `PSW.P` (bit 5): `false` -> `$0EF`, `true` -> `$1EF`.
    pub p: bool,
}

/// One instruction, taken apart.
#[derive(Debug, Clone)]
pub struct DisasmResult {
    /// The mnemonic, lowercase, without the field's padding (`mov`,
    /// `or1`, `set0`, `tcall`).
    pub mnemonic: String,
    /// The operand text without its leading space (`a, $0EF`, `(x), a`,
    /// `a,(x)`); empty when the instruction has none.
    pub operands: String,
    /// For a branch, the raw signed offset as `(+n)` / `(-n)`.
    pub comment: Option<String>,
    /// The instruction's bytes, opcode first.
    pub bytes: Vec<u8>,
    /// How many, the same as `bytes.len()`.
    pub bytes_consumed: u8,
}

/// The operand text of every opcode, in the trace format: literal text
/// with the fields in braces. `{dp}` is the first operand byte as a
/// direct-page address and `{dp1}` the second; `{imm}` the first byte as
/// an immediate; `{abs}` both as an absolute address; `{bit}` both as a
/// bit operand; `{rel}` the first byte as a branch offset (two-byte
/// instructions) and `{rel3}` the second (three-byte ones); `{n}` the
/// `tcall` vector from the opcode; `{pcall}` the first byte as a
/// `$FFnn` address.
#[rustfmt::skip]
const OPERANDS: [&str; 256] = [
    // 0x00 - 0x0F
    "", " {n}", " ${dp}", " ${dp},${rel3}",
    " a, ${dp}", " a, ${abs}", "a,(x)", " a, (${dp}+x)",
    " a, #${imm}", " ${dp1}, ${dp}", " c, ${bit}", " ${dp}",
    " ${abs}", " p", " ${abs},a", "",
    // 0x10 - 0x1F
    " ${rel}", " {n}", " ${dp}", " ${dp},${rel3}",
    " a, ${dp}+x", " a, ${abs}+x", " a, ${abs}+y", " a, (${dp})+y",
    " ${dp1}, #${imm}", " (x), (y)", " ${dp}", " ${dp}+x",
    " a", " x", " x, ${abs}", " (${abs}+x)",
    // 0x20 - 0x2F
    "", " {n}", " ${dp}", " ${dp},${rel3}",
    " a, ${dp}", " a, ${abs}", "a,(x)", " a, (${dp}+x)",
    " a, #${imm}", " ${dp1}, ${dp}", " c, !${bit}", " ${dp}",
    " ${abs}", " a", " ${dp},${rel3}", " ${rel}",
    // 0x30 - 0x3F
    " ${rel}", " {n}", " ${dp}", " ${dp},${rel3}",
    " a, ${dp}+x", " a, ${abs}+x", " a, ${abs}+y", " a, (${dp})+y",
    " ${dp1}, #${imm}", " (x), (y)", " ${dp}", " ${dp}+x",
    " a", " x", " x, ${dp}", " ${abs}",
    // 0x40 - 0x4F
    "", " {n}", " ${dp}", " ${dp},${rel3}",
    " a, ${dp}", " a, ${abs}", "a,(x)", " a, (${dp}+x)",
    " a, #${imm}", " ${dp1}, ${dp}", " c, ${bit}", " ${dp}",
    " ${abs}", " x", " ${abs},a", " ${pcall}",
    // 0x50 - 0x5F
    " ${rel}", " {n}", " ${dp}", " ${dp},${rel3}",
    " a, ${dp}+x", " a, ${abs}+x", " a, ${abs}+y", " a, (${dp})+y",
    " ${dp1}, #${imm}", " (x), (y)", " ya, ${dp}", " ${dp}+x",
    " a", " x, a", " y, ${abs}", " ${abs}",
    // 0x60 - 0x6F
    "", " {n}", " ${dp}", " ${dp},${rel3}",
    " a, ${dp}", " a, ${abs}", "a,(x)", " a, (${dp}+x)",
    " a, #${imm}", " ${dp1}, ${dp}", " c, !${bit}", " ${dp}",
    " ${abs}", " y", " ${dp},${rel3}", "",
    // 0x70 - 0x7F
    " ${rel}", " {n}", " ${dp}", " ${dp},${rel3}",
    " a, ${dp}+x", " a, ${abs}+x", " a, ${abs}+y", " a, (${dp})+y",
    " ${dp1}, #${imm}", " (x), (y)", " ya, ${dp}", " ${dp}+x",
    " a", " a, x", " y, ${dp}", "",
    // 0x80 - 0x8F
    "", " {n}", " ${dp}", " ${dp},${rel3}",
    " a, ${dp}", " a, ${abs}", "a,(x)", " a, (${dp}+x)",
    " a, #${imm}", " ${dp1}, ${dp}", " c, ${bit}", " ${dp}",
    " ${abs}", " y, #${imm}", " p", " ${dp1}, #${imm}",
    // 0x90 - 0x9F
    " ${rel}", " {n}", " ${dp}", " ${dp},${rel3}",
    " a, ${dp}+x", " a, ${abs}+x", " a, ${abs}+y", " a, (${dp})+y",
    " ${dp1}, #${imm}", " (x), (y)", " ya, ${dp}", " ${dp}+x",
    " a", " x, sp", " ya, x", " a",
    // 0xA0 - 0xAF
    "", " {n}", " ${dp}", " ${dp},${rel3}",
    " a, ${dp}", " a, ${abs}", "a,(x)", " a, (${dp}+x)",
    " a, #${imm}", " ${dp1}, ${dp}", " c, ${bit}", " ${dp}",
    " ${abs}", " y, #${imm}", " a", " (x)+, a",
    // 0xB0 - 0xBF
    " ${rel}", " {n}", " ${dp}", " ${dp},${rel3}",
    " a, ${dp}+x", " a, ${abs}+x", " a, ${abs}+y", " a, (${dp})+y",
    " ${dp1}, #${imm}", " (x), (y)", " ya, ${dp}", " ${dp}+x",
    " a", " sp, x", " a", " a, (x)+",
    // 0xC0 - 0xCF
    "", " {n}", " ${dp}", " ${dp},${rel3}",
    " ${dp}, a", " ${abs},a", " (x), a", " (${dp}+x), a",
    " x, #${imm}", " ${abs}, x", " ${bit}, c", " ${dp}, y",
    " ${abs}, y", " x, #${imm}", " x", " ya",
    // 0xD0 - 0xDF
    " ${rel}", " {n}", " ${rel}", " ${dp},${rel3}",
    " ${dp}+x, a", " ${abs}+x, a", " ${abs}+y, a", " (${dp})+y, a",
    " ${dp}, x", " ${dp}+y, x", " ${dp},ya", " ${dp}+x, y",
    " y", " a, y", " ${dp}, ${rel3}", " a",
    // 0xE0 - 0xEF
    "", " {n}", " ${dp}", " ${dp},${rel3}",
    " a, ${dp}", " a, ${abs}", "a,(x)", " a, (${dp}+x)",
    " a, #${imm}", " x, ${abs}", " c, ${bit}", " y, ${dp}",
    " y, ${abs}", "", " y", "",
    // 0xF0 - 0xFF
    " ${rel}", " {n}", " ${dp}", " ${dp},${rel3}",
    " a, ${dp}+x", " a, ${abs}+x", " a, ${abs}+y", " a, (${dp})+y",
    " x, ${dp}", " x, ${dp}+y", " ${dp1}, ${dp}", " y, ${dp}+x",
    " y", " y, a", " y, ${rel}", "",
];

/// The mnemonic as the format names it: the bit-indexed families carry
/// their bit.
fn name(opcode: u8) -> String {
    let mnemonic = OPCODE_TABLE[usize::from(opcode)].mnemonic;
    let family = match mnemonic {
        Mnemonic::SET1 => "set",
        Mnemonic::CLR1 => "clr",
        Mnemonic::BBS => "bbs",
        Mnemonic::BBC => "bbc",
        other => return other.name().to_string(),
    };
    format!("{family}{}", (opcode >> 5) & 7)
}

/// A branch target: the instruction's address plus its length plus the
/// offset, printed with at least four hex digits.
fn branch_target(pc: u16, length: i64, offset: u8) -> String {
    let target = i64::from(pc) + length + i64::from(offset as i8);
    format!("{:04X}", target as u64)
}

/// The operand text for `opcode` with its operand bytes, at `pc`.
fn operands(opcode: u8, op0: u8, op1: u8, pc: u16, p: bool) -> String {
    let pattern = OPERANDS[usize::from(opcode)];
    let direct = |byte: u8| format!("{:03X}", (u16::from(p) << 8) | u16::from(byte));
    let absolute = u16::from_le_bytes([op0, op1]);
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = rest[open..]
            .find('}')
            .expect("a field in the operand table");
        let field = &rest[open + 1..open + close];
        match field {
            "dp" => out.push_str(&direct(op0)),
            "dp1" => out.push_str(&direct(op1)),
            "imm" => out.push_str(&format!("{op0:02X}")),
            "abs" => out.push_str(&format!("{absolute:04X}")),
            "bit" => out.push_str(&format!("{:04X}:{}", absolute & 0x1fff, absolute >> 13)),
            "rel" => out.push_str(&branch_target(pc, 2, op0)),
            "rel3" => out.push_str(&branch_target(pc, 3, op1)),
            "n" => out.push_str(&format!("{}", opcode >> 4)),
            "pcall" => out.push_str(&format!("FF{op0:02X}")),
            other => panic!("unknown field {{{other}}} in the operand table"),
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

/// For a branch, the offset byte as a signed number.
fn branch_comment(opcode: u8, op0: u8, op1: u8) -> Option<String> {
    let pattern = OPERANDS[usize::from(opcode)];
    if pattern.contains("{rel3}") {
        Some(format!("({:+})", op1 as i8))
    } else if pattern.contains("{rel}") {
        Some(format!("({:+})", op0 as i8))
    } else {
        None
    }
}

/// The instruction at `addr` as one trace line (`name operands`, with
/// the five-column name) and its length in bytes. The trace formatter
/// calls this for every instruction, so it allocates one string.
pub fn disassemble_at(bus: &mut dyn Addressable, addr: u32, state: &DisasmState) -> (String, u8) {
    let opcode = bus.read(addr);
    let length = OPCODE_TABLE[usize::from(opcode)].mode.operand_size();
    let op0 = bus.read(addr.wrapping_add(1));
    let op1 = bus.read(addr.wrapping_add(2));
    let text = format!(
        "{:<5}{}",
        name(opcode),
        operands(opcode, op0, op1, addr as u16, state.p)
    );
    (text, length)
}

/// The instruction at `addr`, taken apart.
pub fn disassemble_one(bus: &mut dyn Addressable, addr: u32, state: &DisasmState) -> DisasmResult {
    let opcode = bus.read(addr);
    let length = OPCODE_TABLE[usize::from(opcode)].mode.operand_size();
    let bytes: Vec<u8> = (0..u32::from(length))
        .map(|i| bus.read(addr.wrapping_add(i)))
        .collect();
    let op0 = bytes.get(1).copied().unwrap_or(0);
    let op1 = bytes.get(2).copied().unwrap_or(0);
    DisasmResult {
        mnemonic: name(opcode),
        operands: operands(opcode, op0, op1, addr as u16, state.p)
            .trim_start()
            .to_string(),
        comment: branch_comment(opcode, op0, op1),
        bytes,
        bytes_consumed: length,
    }
}

#[cfg(test)]
mod tests {
    //! Every expected string is the trace format exactly (uppercase
    //! hex; the trace formatter lowercases). The line keeps the
    //! five-column mnemonic field, so a register operand starts three
    //! columns past a three-letter mnemonic (`mov   x, #$EF`).

    use super::*;
    use retro_core::cpu::Addressable;

    struct TinyBus {
        ram: [u8; 0x10000],
    }
    impl TinyBus {
        fn new() -> Self {
            Self { ram: [0; 0x10000] }
        }
        fn load(&mut self, addr: u16, bytes: &[u8]) {
            for (i, b) in bytes.iter().enumerate() {
                self.ram[addr.wrapping_add(i as u16) as usize] = *b;
            }
        }
    }
    impl Addressable for TinyBus {
        fn read(&mut self, addr: u32) -> u8 {
            self.ram[(addr & 0xFFFF) as usize]
        }
        fn write(&mut self, addr: u32, value: u8) {
            self.ram[(addr & 0xFFFF) as usize] = value;
        }
    }

    /// Disassemble the instruction stored at address 0 of a fresh bus
    /// with `P=0`.
    fn disasm(stream: &[u8]) -> (String, u8) {
        let mut bus = TinyBus::new();
        bus.load(0, stream);
        disassemble_at(&mut bus, 0, &DisasmState::default())
    }

    /// Disassemble the instruction stored at `addr` of a fresh bus with
    /// `P=0`.
    fn disasm_at(addr: u16, stream: &[u8]) -> (String, u8) {
        let mut bus = TinyBus::new();
        bus.load(addr, stream);
        disassemble_at(&mut bus, addr as u32, &DisasmState::default())
    }

    /// Disassemble at address 0 with `P=1` (direct-page base `$01xx`).
    fn disasm_p(stream: &[u8]) -> (String, u8) {
        let mut bus = TinyBus::new();
        bus.load(0, stream);
        disassemble_at(&mut bus, 0, &DisasmState { p: true })
    }

    // -- Mnemonic table, every opcode 0x00..0xFF -----------------------------

    #[test]
    fn test_disasm_all_256_opcodes_mnemonic() {
        // The bit-indexed families carry the bit in the name
        // (`set0`..`bbc7`); `tcall` keeps its mnemonic intact.
        #[rustfmt::skip]
        let expected: [&str; 256] = [
            // 0x00..0x0F
            "nop", "tcall", "set0", "bbs0", "or", "or", "or", "or",
            "or", "or", "or1", "asl", "asl", "push", "tset", "brk",
            // 0x10..0x1F
            "bpl", "tcall", "clr0", "bbc0", "or", "or", "or", "or",
            "or", "or", "decw", "asl", "asl", "dec", "cmp", "jmp",
            // 0x20..0x2F
            "clrp", "tcall", "set1", "bbs1", "and", "and", "and", "and",
            "and", "and", "or1", "rol", "rol", "push", "cbne", "bra",
            // 0x30..0x3F
            "bmi", "tcall", "clr1", "bbc1", "and", "and", "and", "and",
            "and", "and", "incw", "rol", "rol", "inc", "cmp", "call",
            // 0x40..0x4F
            "setp", "tcall", "set2", "bbs2", "eor", "eor", "eor", "eor",
            "eor", "eor", "and1", "lsr", "lsr", "push", "tclr", "pcall",
            // 0x50..0x5F
            "bvc", "tcall", "clr2", "bbc2", "eor", "eor", "eor", "eor",
            "eor", "eor", "cmpw", "lsr", "lsr", "mov", "cmp", "jmp",
            // 0x60..0x6F
            "clrc", "tcall", "set3", "bbs3", "cmp", "cmp", "cmp", "cmp",
            "cmp", "cmp", "and1", "ror", "ror", "push", "dbnz", "ret",
            // 0x70..0x7F
            "bvs", "tcall", "clr3", "bbc3", "cmp", "cmp", "cmp", "cmp",
            "cmp", "cmp", "addw", "ror", "ror", "mov", "cmp", "reti",
            // 0x80..0x8F
            "setc", "tcall", "set4", "bbs4", "adc", "adc", "adc", "adc",
            "adc", "adc", "eor1", "dec", "dec", "mov", "pop", "mov",
            // 0x90..0x9F
            "bcc", "tcall", "clr4", "bbc4", "adc", "adc", "adc", "adc",
            "adc", "adc", "subw", "dec", "dec", "mov", "div", "xcn",
            // 0xA0..0xAF
            "ei", "tcall", "set5", "bbs5", "sbc", "sbc", "sbc", "sbc",
            "sbc", "sbc", "mov1", "inc", "inc", "cmp", "pop", "mov",
            // 0xB0..0xBF
            "bcs", "tcall", "clr5", "bbc5", "sbc", "sbc", "sbc", "sbc",
            "sbc", "sbc", "movw", "inc", "inc", "mov", "das", "mov",
            // 0xC0..0xCF
            "di", "tcall", "set6", "bbs6", "mov", "mov", "mov", "mov",
            "cmp", "mov", "mov1", "mov", "mov", "mov", "pop", "mul",
            // 0xD0..0xDF
            "bne", "tcall", "clr6", "bbc6", "mov", "mov", "mov", "mov",
            "mov", "mov", "movw", "mov", "dec", "mov", "cbne", "daa",
            // 0xE0..0xEF
            "clrv", "tcall", "set7", "bbs7", "mov", "mov", "mov", "mov",
            "mov", "mov", "not1", "mov", "mov", "notc", "pop", "sleep",
            // 0xF0..0xFF
            "beq", "tcall", "clr7", "bbc7", "mov", "mov", "mov", "mov",
            "mov", "mov", "mov", "mov", "inc", "mov", "dbnz", "stop",
        ];
        for op in 0u16..256 {
            assert_eq!(
                name(op as u8),
                expected[op as usize],
                "opcode {:02X} mnemonic mismatch",
                op
            );
        }
    }

    // -- Implied-mode MOV reg-to-reg directions -------------------------------

    #[test]
    fn test_disasm_implied_mov_directions() {
        let cases: &[(u8, &str)] = &[
            (0x7D, "mov   a, x"),
            (0xDD, "mov   a, y"),
            (0x5D, "mov   x, a"),
            (0xFD, "mov   y, a"),
            (0x9D, "mov   x, sp"),
            (0xBD, "mov   sp, x"),
        ];
        for &(op, want) in cases {
            let (text, size) = disasm(&[op]);
            assert_eq!(text, want, "opcode {:02X}", op);
            assert_eq!(size, 1, "opcode {:02X}", op);
        }
    }

    // -- Immediate directions (a/x/y for MOV/CMP, always `a` for ALU) ---------

    #[test]
    fn test_disasm_immediate_directions() {
        let cases: &[(u8, u8, &str)] = &[
            (0xE8, 0x11, "mov   a, #$11"),
            (0xCD, 0x22, "mov   x, #$22"),
            (0x8D, 0x33, "mov   y, #$33"),
            (0x68, 0x44, "cmp   a, #$44"),
            (0xC8, 0x55, "cmp   x, #$55"),
            (0xAD, 0x66, "cmp   y, #$66"),
            (0x88, 0x77, "adc   a, #$77"),
            (0xA8, 0x88, "sbc   a, #$88"),
            (0x28, 0x99, "and   a, #$99"),
            (0x08, 0xAA, "or    a, #$AA"),
            (0x48, 0xBB, "eor   a, #$BB"),
        ];
        for &(op, imm, want) in cases {
            let (text, size) = disasm(&[op, imm]);
            assert_eq!(text, want, "opcode {:02X}", op);
            assert_eq!(size, 2, "opcode {:02X}", op);
        }
    }

    // -- Direct-page direction (P-folded 3-hex; every register side) ----------

    #[test]
    fn test_disasm_dp_directions() {
        let cases: &[(u8, u8, &str)] = &[
            (0xE4, 0x20, "mov   a, $020"),
            (0xC4, 0x20, "mov   $020, a"),
            (0xEB, 0x30, "mov   y, $030"),
            (0xCB, 0x30, "mov   $030, y"),
            (0xF8, 0x40, "mov   x, $040"),
            (0xD8, 0x40, "mov   $040, x"),
            (0x3E, 0x10, "cmp   x, $010"),
            (0x7E, 0x11, "cmp   y, $011"),
            (0x64, 0x12, "cmp   a, $012"),
            (0x84, 0x13, "adc   a, $013"),
            (0xA4, 0x14, "sbc   a, $014"),
            (0x24, 0x15, "and   a, $015"),
            (0x04, 0x16, "or    a, $016"),
            (0x44, 0x17, "eor   a, $017"),
        ];
        for &(op, dp, want) in cases {
            let (text, size) = disasm(&[op, dp]);
            assert_eq!(text, want, "opcode {:02X}", op);
            assert_eq!(size, 2, "opcode {:02X}", op);
        }
    }

    // -- PSW.P folds the direct-page base into the printed operand ------------

    #[test]
    fn test_disasm_dp_folds_psw_p() {
        // P=0 -> $0EF, P=1 -> $1EF (mov a,$dp = 0xE4).
        assert_eq!(disasm(&[0xE4, 0xEF]).0, "mov   a, $0EF");
        assert_eq!(disasm_p(&[0xE4, 0xEF]).0, "mov   a, $1EF");
    }

    // -- DirectPage,DirectPage destination-first byte order -------------------

    #[test]
    fn test_disasm_dp_dp_source_first() {
        // 0xFA mov $dst,$src. Bytes: opcode, src=$22 (first), dst=$11
        // (second); the format prints the destination first.
        let (text, size) = disasm(&[0xFA, 0x22, 0x11]);
        assert_eq!(text, "mov   $011, $022");
        assert_eq!(size, 3);
    }

    // -- DirectPage,#imm operand byte layout ----------------------------------

    #[test]
    fn test_disasm_dp_imm_byte_layout() {
        // 0x8F mov $dp,#imm. Bytes: opcode, imm=$77 (first), dp=$20
        // (second); the format prints the direct page, then the immediate.
        let (text, size) = disasm(&[0x8F, 0x77, 0x20]);
        assert_eq!(text, "mov   $020, #$77");
        assert_eq!(size, 3);
    }

    // -- Relative branch target resolution ------------------------------------

    #[test]
    fn test_disasm_relative_target_positive() {
        // 0xD0 bne; offset +5 at $1000 -> target = $1000 + 2 + 5 = $1007.
        let (text, size) = disasm_at(0x1000, &[0xD0, 0x05]);
        assert_eq!(text, "bne   $1007");
        assert_eq!(size, 2);
    }

    #[test]
    fn test_disasm_relative_target_negative() {
        // 0xD0 bne; offset -3 (0xFD) at $1000 -> target = $1002 - 3 = $0FFF.
        let (text, size) = disasm_at(0x1000, &[0xD0, 0xFD]);
        assert_eq!(text, "bne   $0FFF");
        assert_eq!(size, 2);
    }

    #[test]
    fn test_disasm_dp_relative_target() {
        // 0x2E cbne $dp,rel, a bare comma in this form. At $1000,
        // bytes [opcode, $42, +5]. target = $1000 + 3 + 5 = $1008.
        let (text, size) = disasm_at(0x1000, &[0x2E, 0x42, 0x05]);
        assert_eq!(text, "cbne  $042,$1008");
        assert_eq!(size, 3);
    }

    #[test]
    fn test_disasm_y_relative_target() {
        // 0xFE dbnz y,rel. At $1000, bytes [opcode, +10]. target =
        // $1000 + 2 + 10 = $100C.
        let (text, size) = disasm_at(0x1000, &[0xFE, 0x0A]);
        assert_eq!(text, "dbnz  y, $100C");
        assert_eq!(size, 2);
    }

    #[test]
    fn test_disasm_dpx_relative_target() {
        // 0xDE cbne $dp+x,rel, comma and space in this form.
        // At $2000, bytes [opcode, $33, -2]. target = $2003 - 2 = $2001.
        let (text, size) = disasm_at(0x2000, &[0xDE, 0x33, 0xFE]);
        assert_eq!(text, "cbne  $033, $2001");
        assert_eq!(size, 3);
    }

    // -- Bit-indexed families keep the bit in the name ------------------------

    #[test]
    fn test_disasm_bit_merged_set_clr() {
        for bit in 0u8..8 {
            let set_op = (bit << 5) | 0x02;
            let (text, size) = disasm(&[set_op, 0x20]);
            assert_eq!(
                text,
                format!("set{}  $020", bit),
                "set opcode {:02X}",
                set_op
            );
            assert_eq!(size, 2);

            let clr_op = (bit << 5) | 0x12;
            let (text, size) = disasm(&[clr_op, 0x20]);
            // 0xD2 (clr6) is the format's one quirk; its own test covers it.
            if clr_op == 0xD2 {
                continue;
            }
            assert_eq!(
                text,
                format!("clr{}  $020", bit),
                "clr opcode {:02X}",
                clr_op
            );
            assert_eq!(size, 2);
        }
    }

    #[test]
    fn test_disasm_bit_merged_bbs_bbc() {
        for bit in 0u8..8 {
            let bbs_op = (bit << 5) | 0x03;
            let (text, size) = disasm_at(0x1000, &[bbs_op, 0x20, 0x00]);
            // target = $1000 + 3 + 0 = $1003.
            assert_eq!(
                text,
                format!("bbs{}  $020,$1003", bit),
                "bbs opcode {:02X}",
                bbs_op
            );
            assert_eq!(size, 3);

            let bbc_op = (bit << 5) | 0x13;
            let (text, size) = disasm_at(0x1000, &[bbc_op, 0x20, 0x00]);
            assert_eq!(
                text,
                format!("bbc{}  $020,$1003", bit),
                "bbc opcode {:02X}",
                bbc_op
            );
            assert_eq!(size, 3);
        }
    }

    // -- 0xD2 clr6: the format's one quirk, kept ------------------------------

    #[test]
    fn test_disasm_clr6_keeps_the_formats_quirk() {
        // The reference logs print 0xD2's operand as a branch target: at
        // $0861 with the byte $1D the line reads `clr6  $0880`.
        let (text, size) = disasm_at(0x0861, &[0xD2, 0x1D]);
        assert_eq!(text, "clr6  $0880");
        assert_eq!(size, 2);
    }

    // -- TCALL vector is a DECIMAL index, no `$` ------------------------------

    #[test]
    fn test_disasm_tcall_decimal_index() {
        for n in 0u8..16 {
            let op = (n << 4) | 0x01;
            let (text, size) = disasm(&[op]);
            assert_eq!(text, format!("tcall {}", n), "opcode {:02X}", op);
            assert_eq!(size, 1);
        }
    }

    // -- PCALL resolves to page $FF -------------------------------------------

    #[test]
    fn test_disasm_pcall_page_ff() {
        let (text, size) = disasm(&[0x4F, 0x42]);
        assert_eq!(text, "pcall $FF42");
        assert_eq!(size, 2);
    }

    // -- JMP shapes (bare `$abs`, `($abs+x)` indirect) ------------------------

    #[test]
    fn test_disasm_jmp_modes() {
        let (text, size) = disasm(&[0x5F, 0x34, 0x12]);
        assert_eq!(text, "jmp   $1234");
        assert_eq!(size, 3);

        let (text, size) = disasm(&[0x1F, 0x34, 0x12]);
        assert_eq!(text, "jmp   ($1234+x)");
        assert_eq!(size, 3);
    }

    // -- YA,DP family, both MOVW directions, ADDW/SUBW/CMPW, INCW/DECW ------

    #[test]
    fn test_disasm_yadp() {
        let cases: &[(u8, &str)] = &[
            (0xBA, "movw  ya, $010"),
            (0xDA, "movw  $010,ya"),
            (0x5A, "cmpw  ya, $010"),
            (0x7A, "addw  ya, $010"),
            (0x9A, "subw  ya, $010"),
        ];
        for &(op, want) in cases {
            let (text, size) = disasm(&[op, 0x10]);
            assert_eq!(text, want, "opcode {:02X}", op);
            assert_eq!(size, 2);
        }
        // INCW/DECW render a bare direct page.
        let (text, _) = disasm(&[0x3A, 0x10]);
        assert_eq!(text, "incw  $010");
        let (text, _) = disasm(&[0x1A, 0x10]);
        assert_eq!(text, "decw  $010");
    }

    // -- TSET/TCLR write-back into the implicit A operand (`,a` no space) -----

    #[test]
    fn test_disasm_tset_tclr() {
        let (text, size) = disasm(&[0x0E, 0x34, 0x12]);
        assert_eq!(text, "tset  $1234,a");
        assert_eq!(size, 3);

        let (text, _) = disasm(&[0x4E, 0x34, 0x12]);
        assert_eq!(text, "tclr  $1234,a");
    }

    // -- Indirect (X) / (X)+ / (X),(Y) directions ----------------------------

    #[test]
    fn test_disasm_indirect_directions() {
        // `a,(x)` is the one operand with no leading space.
        let (text, size) = disasm(&[0xE6]);
        assert_eq!(text, "mov  a,(x)");
        assert_eq!(size, 1);

        // `(x), a` has the leading space and the comma and space.
        let (text, _) = disasm(&[0xC6]);
        assert_eq!(text, "mov   (x), a");

        let (text, _) = disasm(&[0xBF]);
        assert_eq!(text, "mov   a, (x)+");

        let (text, _) = disasm(&[0xAF]);
        assert_eq!(text, "mov   (x)+, a");

        let (text, _) = disasm(&[0x79]);
        assert_eq!(text, "cmp   (x), (y)");
    }

    // -- IndirectIndirect{X,Y} (one opcode per direction) ---------------------

    #[test]
    fn test_disasm_ii_x_y_directions() {
        // ($dp+x), MOV directions.
        let (text, _) = disasm(&[0xE7, 0x20]);
        assert_eq!(text, "mov   a, ($020+x)");
        let (text, _) = disasm(&[0xC7, 0x20]);
        assert_eq!(text, "mov   ($020+x), a");

        // ($dp)+y, MOV directions.
        let (text, _) = disasm(&[0xF7, 0x20]);
        assert_eq!(text, "mov   a, ($020)+y");
        let (text, _) = disasm(&[0xD7, 0x20]);
        assert_eq!(text, "mov   ($020)+y, a");

        // ALU defaults to `a, (...)`.
        let (text, _) = disasm(&[0x67, 0x10]);
        assert_eq!(text, "cmp   a, ($010+x)");
        let (text, _) = disasm(&[0x77, 0x10]);
        assert_eq!(text, "cmp   a, ($010)+y");
    }

    // -- MemBit rendering: `c, $ADDR:BIT` with DECIMAL bit -------------------

    #[test]
    fn test_disasm_membit_split() {
        // raw = 0xE123 -> bit = 7, addr = $0123.
        let (text, size) = disasm(&[0xAA, 0x23, 0xE1]);
        assert_eq!(text, "mov1  c, $0123:7");
        assert_eq!(size, 3);

        // 0xCA mov1 $abs:bit,c.
        let (text, _) = disasm(&[0xCA, 0x23, 0xE1]);
        assert_eq!(text, "mov1  $0123:7, c");

        // 0xEA not1 keeps the `c,` form.
        let (text, _) = disasm(&[0xEA, 0x23, 0xE1]);
        assert_eq!(text, "not1  c, $0123:7");

        let (text, _) = disasm(&[0x0A, 0x23, 0xE1]);
        assert_eq!(text, "or1   c, $0123:7");

        let (text, _) = disasm(&[0x4A, 0x23, 0xE1]);
        assert_eq!(text, "and1  c, $0123:7");

        let (text, _) = disasm(&[0x8A, 0x23, 0xE1]);
        assert_eq!(text, "eor1  c, $0123:7");
    }

    #[test]
    fn test_disasm_membit_inverted() {
        // raw = 0x6042 -> bit = 3, addr = $0042; the inverted bit is `!$`.
        let (text, size) = disasm(&[0x2A, 0x42, 0x60]);
        assert_eq!(text, "or1   c, !$0042:3");
        assert_eq!(size, 3);

        let (text, _) = disasm(&[0x6A, 0x42, 0x60]);
        assert_eq!(text, "and1  c, !$0042:3");
    }

    // -- Instruction byte length per opcode (from decode::OPCODE_TABLE) -------

    #[test]
    fn test_disasm_instruction_size_per_mode() {
        let cases: &[(u8, &[u8], u8)] = &[
            (0x00, &[], 1),           // Implied (nop)
            (0x1C, &[], 1),           // asl a
            (0xE8, &[0x42], 2),       // Immediate
            (0xD0, &[0x05], 2),       // Relative
            (0xE4, &[0x20], 2),       // DirectPage
            (0xF4, &[0x20], 2),       // DirectPageX
            (0xF9, &[0x20], 2),       // DirectPageY
            (0xE5, &[0x34, 0x12], 3), // Absolute
            (0xF5, &[0x34, 0x12], 3), // AbsoluteX
            (0xF6, &[0x34, 0x12], 3), // AbsoluteY
            (0xE6, &[], 1),           // IndirectX
            (0xBF, &[], 1),           // IndirectXAuto
            (0xE7, &[0x20], 2),       // IndirectIndirectX
            (0xF7, &[0x20], 2),       // IndirectIndirectY
            (0x79, &[], 1),           // IxIy
            (0x1F, &[0x34, 0x12], 3), // AbsoluteIndirectX
            (0x8F, &[0x77, 0x20], 3), // DirectPageImm
            (0xFA, &[0x22, 0x11], 3), // DirectPageDirectPage
            (0x2E, &[0x42, 0x05], 3), // DirectPageRelative
            (0xDE, &[0x42, 0x05], 3), // DirectPageXRelative
            (0xFE, &[0x05], 2),       // YRelative
            (0xBA, &[0x20], 2),       // YaDirectPage
            (0xAA, &[0x42, 0x60], 3), // MemBit
            (0x2A, &[0x42, 0x60], 3), // MemBitNot
            (0x4F, &[0x42], 2),       // PCallVector
            (0x01, &[], 1),           // TCallVector
        ];
        for &(op, ops, want_size) in cases {
            let mut bytes = vec![op];
            bytes.extend_from_slice(ops);
            let (_, size) = disasm(&bytes);
            assert_eq!(size, want_size, "opcode {:02X}", op);
        }
    }

    // -- Accumulator / implied single-operand rendering ----------------------

    #[test]
    fn test_disasm_accumulator_modes() {
        // The format prints the explicit `a` operand for the
        // shift/rotate/inc/dec accumulator forms.
        assert_eq!(disasm(&[0x1C]).0, "asl   a");
        assert_eq!(disasm(&[0x5C]).0, "lsr   a");
        assert_eq!(disasm(&[0x3C]).0, "rol   a");
        assert_eq!(disasm(&[0x7C]).0, "ror   a");
        assert_eq!(disasm(&[0xBC]).0, "inc   a");
        assert_eq!(disasm(&[0x9C]).0, "dec   a");
        assert_eq!(disasm(&[0x9F]).0, "xcn   a");
        assert_eq!(disasm(&[0xDF]).0, "daa   a");
        assert_eq!(disasm(&[0xBE]).0, "das   a");
        assert_eq!(disasm(&[0x2D]).0, "push  a");
        assert_eq!(disasm(&[0xAE]).0, "pop   a");
    }

    // -- Relative branch comment carries the signed raw offset ----------------

    #[test]
    fn test_disasm_relative_comment_offset() {
        let mut bus = TinyBus::new();
        bus.load(0x1000, &[0xD0, 0x05]);
        let res = disassemble_one(&mut bus, 0x1000, &DisasmState::default());
        assert_eq!(res.comment, Some("(+5)".to_string()));

        bus.load(0x1000, &[0xD0, 0xFD]);
        let res = disassemble_one(&mut bus, 0x1000, &DisasmState::default());
        assert_eq!(res.comment, Some("(-3)".to_string()));
    }

    // -- PC wrap at the top of the 16-bit address space ----------------------

    #[test]
    fn test_disasm_pc_wraps_at_top_of_address_space() {
        // 3-byte JMP at $FFFE consumes $FFFE, $FFFF, $0000.
        let mut bus = TinyBus::new();
        bus.write(0xFFFE, 0x5F);
        bus.write(0xFFFF, 0x34);
        bus.write(0x0000, 0x12);
        let (text, size) = disassemble_at(&mut bus, 0xFFFE, &DisasmState::default());
        assert_eq!(text, "jmp   $1234");
        assert_eq!(size, 3);
    }

    // -- PUSH/POP with explicit registers -------------------------------------

    #[test]
    fn test_disasm_push_pop_registers() {
        // The format writes the status word as `p`.
        let cases: &[(u8, &str)] = &[
            (0x0D, "push  p"),
            (0x4D, "push  x"),
            (0x6D, "push  y"),
            (0x8E, "pop   p"),
            (0xCE, "pop   x"),
            (0xEE, "pop   y"),
        ];
        for &(op, want) in cases {
            let (text, _) = disasm(&[op]);
            assert_eq!(text, want, "opcode {:02X}", op);
        }
    }

    // -- INC/DEC X/Y ----------------------------------------------------------

    #[test]
    fn test_disasm_inc_dec_xy() {
        assert_eq!(disasm(&[0x3D]).0, "inc   x");
        assert_eq!(disasm(&[0x1D]).0, "dec   x");
        assert_eq!(disasm(&[0xFC]).0, "inc   y");
        assert_eq!(disasm(&[0xDC]).0, "dec   y");
    }

    // -- MUL/DIV operand pairs ------------------------------------------------

    #[test]
    fn test_disasm_mul_div_pairs() {
        assert_eq!(disasm(&[0xCF]).0, "mul   ya");
        assert_eq!(disasm(&[0x9E]).0, "div   ya, x");
    }

    // -- True no-operand mnemonics (sanity) -----------------------------------

    #[test]
    fn test_disasm_no_operand_mnemonics() {
        let cases: &[(u8, &str)] = &[
            (0x00, "nop"),
            (0x0F, "brk"),
            (0x6F, "ret"),
            (0x7F, "reti"),
            (0xEF, "sleep"),
            (0xFF, "stop"),
            (0xA0, "ei"),
            (0xC0, "di"),
            (0x60, "clrc"),
            (0x80, "setc"),
            (0xED, "notc"),
            (0x20, "clrp"),
            (0x40, "setp"),
            (0xE0, "clrv"),
        ];
        for &(op, want) in cases {
            // Implied mode yields just the (trimmed) name.
            assert_eq!(disasm(&[op]).0.trim_end(), want, "opcode {:02X}", op);
        }
    }

    // -- DirectPageX / DirectPageY MOV directions -----------------------------

    #[test]
    fn test_disasm_dp_x_y_mov_directions() {
        let cases_x: &[(u8, &str)] = &[
            (0xD4, "mov   $020+x, a"),
            (0xDB, "mov   $020+x, y"),
            (0xF4, "mov   a, $020+x"),
            (0xFB, "mov   y, $020+x"),
        ];
        for &(op, want) in cases_x {
            let (text, _) = disasm(&[op, 0x20]);
            assert_eq!(text, want, "opcode {:02X}", op);
        }
        // DirectPageY: 2 MOV opcodes.
        let (text, _) = disasm(&[0xD9, 0x40]);
        assert_eq!(text, "mov   $040+y, x");
        let (text, _) = disasm(&[0xF9, 0x40]);
        assert_eq!(text, "mov   x, $040+y");
    }

    // -- Absolute MOV directions (a/x/y on either side) ----------------------

    #[test]
    fn test_disasm_absolute_mov_directions() {
        let cases: &[(u8, &str)] = &[
            (0xC5, "mov   $1234,a"),
            (0xC9, "mov   $1234, x"),
            (0xCC, "mov   $1234, y"),
            (0xE5, "mov   a, $1234"),
            (0xE9, "mov   x, $1234"),
            (0xEC, "mov   y, $1234"),
        ];
        for &(op, want) in cases {
            let (text, size) = disasm(&[op, 0x34, 0x12]);
            assert_eq!(text, want, "opcode {:02X}", op);
            assert_eq!(size, 3);
        }
    }

    // -- AbsoluteX/Y MOV directions ------------------------------------------

    #[test]
    fn test_disasm_absolute_x_y_mov_directions() {
        let (text, _) = disasm(&[0xD5, 0x00, 0x20]);
        assert_eq!(text, "mov   $2000+x, a");
        let (text, _) = disasm(&[0xF5, 0x00, 0x20]);
        assert_eq!(text, "mov   a, $2000+x");
        let (text, _) = disasm(&[0xD6, 0x00, 0x20]);
        assert_eq!(text, "mov   $2000+y, a");
        let (text, _) = disasm(&[0xF6, 0x00, 0x20]);
        assert_eq!(text, "mov   a, $2000+y");
    }

    // -- DisasmResult split (mnemonic / operands / bytes) ---------------------

    #[test]
    fn test_disassemble_one_populates_bytes_and_size() {
        let mut bus = TinyBus::new();
        bus.load(0x4000, &[0x8F, 0x77, 0x20]);
        let res = disassemble_one(&mut bus, 0x4000, &DisasmState::default());
        assert_eq!(res.mnemonic, "mov");
        assert_eq!(res.operands, "$020, #$77");
        assert_eq!(res.bytes, vec![0x8F, 0x77, 0x20]);
        assert_eq!(res.bytes_consumed, 3);
        assert!(res.comment.is_none());
    }

    // -- Trace-shaped reference lines (IPL boot) match exactly ---------------

    #[test]
    fn test_disasm_matches_ipl_reference_disasm() {
        // The disassembly column of the first six lines of a reference
        // trace (the IPL boot, which every game runs first), lowercased
        // as the trace formatter would.
        let cases: &[(u16, &[u8], &str)] = &[
            (0xFFC0, &[0xCD, 0xEF], "mov   x, #$ef"),
            (0xFFC2, &[0xBD], "mov   sp, x"),
            (0xFFC3, &[0xE8, 0x00], "mov   a, #$00"),
            (0xFFC5, &[0xC6], "mov   (x), a"),
            (0xFFC6, &[0x1D], "dec   x"),
            (0xFFC7, &[0xD0, 0xFC], "bne   $ffc5"),
        ];
        for &(addr, stream, want) in cases {
            let (text, _) = disasm_at(addr, stream);
            assert_eq!(text.to_lowercase(), want, "addr {:04X}", addr);
        }
    }
}
