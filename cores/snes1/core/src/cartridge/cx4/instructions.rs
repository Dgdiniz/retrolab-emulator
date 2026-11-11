//! The HG51B169's instruction set: what each 16-bit word names, and how the
//! disassembler writes it.
//!
//! An instruction's high byte names its family and its low byte is an
//! operand, an 8-bit immediate, a register-file index (see
//! [`super::registers`]) or a jump target within a page. Some bits of the
//! high byte do not change the family: bit 2 chooses the low byte itself as
//! the operand instead of the register it names, bits 1-0 say how far an
//! arithmetic or logic operation shifts A first, and a jump's bit 5 makes it a
//! call while its bit 1 takes the target page from P. [`FAMILIES`] lists every
//! family by its high byte with those bits clear and the bits it ignores; the
//! decode table is built from that list when the crate compiles.
//!
//! No two families share a high byte, so the order of the list does not
//! matter. The one family that looks at the low byte is RDROM, which is
//! `$7000` exactly. Two families, ST (`$E0`) and WRBUS (`$E1`), sit side by
//! side; WRBUS stores the bus data latch where ST stores A. (bsnes-plus
//! compares the WRBUS family against `0xe1000`, five hex digits, so it never
//! decodes; this core decodes it, as a test pins.)

use super::registers::{self, Operand, RegisterRef};

/// What a jump tests before it is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Condition {
    /// JMP and JSR: always taken.
    Always,
    /// Taken if the zero flag is set.
    Zero,
    /// Taken if the carry flag is set.
    Carry,
    /// Taken if the negative flag is set.
    Negative,
    /// Taken if the overflow flag is set.
    Overflow,
}

/// The flag a skip tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flag {
    /// The overflow flag (high byte `$24`).
    Overflow,
    /// The carry flag (`$25`).
    Carry,
    /// The zero flag (`$26`).
    Zero,
    /// The negative flag (`$27`).
    Negative,
}

/// One byte of a 24-bit word, as RDRAM and WRRAM move it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lane {
    /// Bits 0-7.
    Low,
    /// Bits 8-15.
    Middle,
    /// Bits 16-23.
    High,
}

impl Lane {
    /// How far the byte sits from bit 0.
    pub const fn shift(self) -> u32 {
        match self {
            Lane::Low => 0,
            Lane::Middle => 8,
            Lane::High => 16,
        }
    }
}

/// An operation between A, shifted first, and an operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AluOp {
    /// ADD: A shifted plus the operand.
    Add,
    /// SUB: A shifted minus the operand.
    Subtract,
    /// SUBR: the operand minus A shifted.
    SubtractFrom,
    /// CMP: the flags of a SUB, A unchanged.
    Compare,
    /// CMPR: the flags of a SUBR, A unchanged.
    CompareFrom,
    /// AND.
    And,
    /// OR.
    Or,
    /// XOR.
    Xor,
}

/// A shift of A by the operand's count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShiftOp {
    /// SHR: logical, zeros in from the top.
    Right,
    /// ASR: arithmetic, the sign in from the top.
    ArithmeticRight,
    /// ROR: a rotation of the 24 bits.
    RotateRight,
    /// SHL.
    Left,
}

/// A decoded instruction. The operand fields stay in the instruction word;
/// the core and the disassembler read them from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    /// NOP.
    Nop,
    /// JMP/JSR and the conditional jumps (bit 13 makes it a call).
    Jump(Condition),
    /// Skip the next instruction when `flag` is `when_set` (low bit 0).
    Skip {
        /// The flag tested.
        flag: Flag,
        /// The flag's value that skips.
        when_set: bool,
    },
    /// RET: return from a call.
    Return,
    /// LOOP: wait for the cart-bus transfer in flight to land.
    WaitForBus,
    /// INC: step the cart-bus address.
    IncrementBusAddress,
    /// An arithmetic or logic operation (see [`AluOp`]).
    Alu(AluOp),
    /// MUL: A times the operand, both signed, into the product registers.
    Multiply,
    /// SXB: the operand's low byte, sign-extended, into A.
    SignExtendByte,
    /// SXW: the operand's low 16 bits, sign-extended, into A.
    SignExtendWord,
    /// LD A: the operand into A.
    Load,
    /// LD P: the operand into P.
    LoadPage,
    /// LD PL: the immediate into P's low byte.
    LoadPageLow,
    /// LD PH: the immediate into P's high byte.
    LoadPageHigh,
    /// RDBUS: read the register the low byte names (only for its effect,
    /// on `$2E`/`$2F` it starts a cart-bus read).
    ReadBus,
    /// RDRAM: one byte of data RAM into the RAM data latch.
    ReadRam(Lane),
    /// WRRAM: one byte of the RAM data latch into data RAM.
    WriteRam(Lane),
    /// RDROM: the data ROM word A selects into the ROM data latch.
    ReadRom,
    /// A shift of A (see [`ShiftOp`]).
    Shift(ShiftOp),
    /// ST: A into the register the low byte names.
    Store,
    /// WRBUS: the bus data latch into the register the low byte names.
    StoreBusData,
    /// SWAP: A and the register the low byte names exchange.
    Swap,
    /// CLEAR: A, P, the RAM data latch and the RAM base back to 0.
    Clear,
    /// HALT.
    Halt,
    /// A word no family claims; the core halts on it.
    Unknown,
}

impl Op {
    /// The mnemonic as the disassembler writes it. A jump with the call bit
    /// set reads as its JSR form.
    pub fn name(self, opcode: u16) -> &'static str {
        let call = opcode & 0x2000 != 0;
        match self {
            Op::Nop => "nop",
            Op::Jump(condition) => match (condition, call) {
                (Condition::Always, false) => "jump",
                (Condition::Always, true) => "jsr",
                (Condition::Zero, false) => "jumpeq",
                (Condition::Zero, true) => "jsreq",
                (Condition::Carry, false) => "jumpge",
                (Condition::Carry, true) => "jsrge",
                (Condition::Negative, false) => "jumpmi",
                (Condition::Negative, true) => "jsrmi",
                (Condition::Overflow, false) => "jumpvs",
                (Condition::Overflow, true) => "jsrvs",
            },
            Op::Skip { flag, when_set } => match (flag, when_set) {
                (Flag::Overflow, true) => "skipvs",
                (Flag::Overflow, false) => "skipvc",
                (Flag::Carry, true) => "skipge",
                (Flag::Carry, false) => "skiplt",
                (Flag::Zero, true) => "skipeq",
                (Flag::Zero, false) => "skipne",
                (Flag::Negative, true) => "skipmi",
                (Flag::Negative, false) => "skippl",
            },
            Op::Return => "ret",
            Op::WaitForBus => "loop",
            Op::IncrementBusAddress => "inc",
            Op::Alu(op) => match op {
                AluOp::Add => "add",
                AluOp::Subtract => "sub",
                AluOp::SubtractFrom => "subr",
                AluOp::Compare => "cmp",
                AluOp::CompareFrom => "cmpr",
                AluOp::And => "and",
                AluOp::Or => "or",
                AluOp::Xor => "xor",
            },
            Op::Multiply => "mul",
            Op::SignExtendByte => "sxb",
            Op::SignExtendWord => "sxw",
            Op::Load | Op::LoadPage | Op::LoadPageLow | Op::LoadPageHigh => "ld",
            Op::ReadBus => "rdbus",
            Op::ReadRam(lane) => match lane {
                Lane::Low => "rdraml",
                Lane::Middle => "rdramh",
                Lane::High => "rdramb",
            },
            Op::WriteRam(lane) => match lane {
                Lane::Low => "wrraml",
                Lane::Middle => "wrramh",
                Lane::High => "wrramb",
            },
            Op::ReadRom => "rdrom",
            Op::Shift(op) => match op {
                ShiftOp::Right => "shr",
                ShiftOp::ArithmeticRight => "asr",
                ShiftOp::RotateRight => "ror",
                ShiftOp::Left => "shl",
            },
            Op::Store => "st",
            Op::StoreBusData => "wrbus",
            Op::Swap => "swap",
            Op::Clear => "clear",
            Op::Halt => "halt",
            Op::Unknown => "unknown",
        }
    }
}

/// What a high byte decodes to before the low byte is looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    /// The instruction, whatever the low byte.
    Op(Op),
    /// A skip on this flag; the low byte's bit 0 says on which value.
    Skip(Flag),
    /// RDROM when the low byte is zero, nothing otherwise.
    ReadRom,
    /// No family.
    Unknown,
}

/// High-byte bit 2: the operand is the low byte itself.
const IMMEDIATE: u8 = 0x04;
/// High-byte bits 1-0: how far an arithmetic or logic operation shifts A.
const SHIFT: u8 = 0x03;
/// A jump's high-byte bit 5: push the return address (a call).
const CALL: u8 = 0x20;
/// A jump's high-byte bit 1: the target page is P, not the current one.
const LONG: u8 = 0x02;

/// One instruction family: its high byte with the bits it ignores clear,
/// those bits, and what it decodes to.
struct Family {
    high: u8,
    ignores: u8,
    entry: Entry,
}

const fn family(high: u8, ignores: u8, entry: Entry) -> Family {
    Family {
        high,
        ignores,
        entry,
    }
}

const fn op(op: Op) -> Entry {
    Entry::Op(op)
}

/// Every family of the instruction set.
const FAMILIES: [Family; 45] = [
    family(0x00, SHIFT, op(Op::Nop)),
    family(0x08, CALL | LONG, op(Op::Jump(Condition::Always))),
    family(0x0C, CALL | LONG, op(Op::Jump(Condition::Zero))),
    family(0x10, CALL | LONG, op(Op::Jump(Condition::Carry))),
    family(0x14, CALL | LONG, op(Op::Jump(Condition::Negative))),
    family(0x18, CALL | LONG, op(Op::Jump(Condition::Overflow))),
    family(0x1C, SHIFT, op(Op::WaitForBus)),
    family(0x24, 0, Entry::Skip(Flag::Overflow)),
    family(0x25, 0, Entry::Skip(Flag::Carry)),
    family(0x26, 0, Entry::Skip(Flag::Zero)),
    family(0x27, 0, Entry::Skip(Flag::Negative)),
    family(0x3C, SHIFT, op(Op::Return)),
    family(0x40, SHIFT, op(Op::IncrementBusAddress)),
    family(0x48, IMMEDIATE | SHIFT, op(Op::Alu(AluOp::CompareFrom))),
    family(0x50, IMMEDIATE | SHIFT, op(Op::Alu(AluOp::Compare))),
    family(0x59, IMMEDIATE, op(Op::SignExtendByte)),
    family(0x5A, IMMEDIATE, op(Op::SignExtendWord)),
    family(0x60, IMMEDIATE, op(Op::Load)),
    family(0x61, IMMEDIATE, op(Op::ReadBus)),
    family(0x63, IMMEDIATE, op(Op::LoadPage)),
    family(0x68, IMMEDIATE, op(Op::ReadRam(Lane::Low))),
    family(0x69, IMMEDIATE, op(Op::ReadRam(Lane::Middle))),
    family(0x6A, IMMEDIATE, op(Op::ReadRam(Lane::High))),
    family(0x70, 0, Entry::ReadRom),
    family(0x7C, 0, op(Op::LoadPageLow)),
    family(0x7D, 0, op(Op::LoadPageHigh)),
    family(0x80, IMMEDIATE | SHIFT, op(Op::Alu(AluOp::Add))),
    family(0x88, IMMEDIATE | SHIFT, op(Op::Alu(AluOp::SubtractFrom))),
    family(0x90, IMMEDIATE | SHIFT, op(Op::Alu(AluOp::Subtract))),
    family(0x98, IMMEDIATE, op(Op::Multiply)),
    family(0xA8, IMMEDIATE | SHIFT, op(Op::Alu(AluOp::Xor))),
    family(0xB0, IMMEDIATE | SHIFT, op(Op::Alu(AluOp::And))),
    family(0xB8, IMMEDIATE | SHIFT, op(Op::Alu(AluOp::Or))),
    family(0xC0, IMMEDIATE, op(Op::Shift(ShiftOp::Right))),
    family(0xC8, IMMEDIATE, op(Op::Shift(ShiftOp::ArithmeticRight))),
    family(0xD0, IMMEDIATE, op(Op::Shift(ShiftOp::RotateRight))),
    family(0xD8, IMMEDIATE, op(Op::Shift(ShiftOp::Left))),
    family(0xE0, 0, op(Op::Store)),
    family(0xE1, 0, op(Op::StoreBusData)),
    family(0xE8, IMMEDIATE, op(Op::WriteRam(Lane::Low))),
    family(0xE9, IMMEDIATE, op(Op::WriteRam(Lane::Middle))),
    family(0xEA, IMMEDIATE, op(Op::WriteRam(Lane::High))),
    family(0xF0, SHIFT, op(Op::Swap)),
    family(0xF8, SHIFT, op(Op::Clear)),
    family(0xFC, SHIFT, op(Op::Halt)),
];

/// The decode table: one entry per high byte, built from [`FAMILIES`].
const DECODE: [Entry; 256] = build_decode_table();

const fn build_decode_table() -> [Entry; 256] {
    let mut table = [Entry::Unknown; 256];
    let mut high = 0;
    while high < 256 {
        let mut i = 0;
        while i < FAMILIES.len() {
            let f = &FAMILIES[i];
            if high as u8 & !f.ignores == f.high {
                table[high] = f.entry;
            }
            i += 1;
        }
        high += 1;
    }
    table
}

/// Decode one instruction word.
pub fn decode(opcode: u16) -> Op {
    match DECODE[usize::from(opcode >> 8)] {
        Entry::Op(op) => op,
        Entry::Skip(flag) => Op::Skip {
            flag,
            when_set: opcode & 1 != 0,
        },
        Entry::ReadRom if opcode & 0xFF == 0 => Op::ReadRom,
        Entry::ReadRom | Entry::Unknown => Op::Unknown,
    }
}

/// A disassembled instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisasmResult {
    /// The mnemonic (a call reads as its JSR form).
    pub mnemonic: String,
    /// The operand text, empty for an instruction without one.
    pub operands: String,
    /// The instruction word.
    pub opcode: u16,
}

/// Disassemble one instruction word.
pub fn disassemble(opcode: u16) -> DisasmResult {
    let op = decode(opcode);
    let low = opcode as u8;
    let operands = match op {
        Op::Jump(_) => {
            if registers::is_long_jump(opcode) {
                format!("p:${low:02X}")
            } else {
                format!("${low:02X}")
            }
        }
        Op::Alu(_) => format!(
            "{},{}",
            shifted_accumulator_text(opcode),
            operand_text(opcode)
        ),
        Op::Load | Op::SignExtendByte | Op::SignExtendWord | Op::Multiply | Op::Shift(_) => {
            format!("a,{}", operand_text(opcode))
        }
        Op::LoadPage => format!("p,{}", operand_text(opcode)),
        Op::LoadPageLow => format!("pl,#${low:02X}"),
        Op::LoadPageHigh => format!("ph,#${low:02X}"),
        Op::ReadBus | Op::StoreBusData => register_text(low),
        Op::Store => format!("{},a", register_text(low)),
        Op::Swap => format!("a,{}", register_text(low)),
        Op::ReadRam(_) | Op::WriteRam(_) => ram_address_text(opcode),
        Op::Unknown => format!("${opcode:04X}"),
        Op::Nop
        | Op::Skip { .. }
        | Op::Return
        | Op::WaitForBus
        | Op::IncrementBusAddress
        | Op::ReadRom
        | Op::Clear
        | Op::Halt => String::new(),
    };
    DisasmResult {
        mnemonic: op.name(opcode).to_string(),
        operands,
        opcode,
    }
}

/// Disassemble into one `mnemonic operands` line.
pub fn disassemble_line(opcode: u16) -> String {
    let d = disassemble(opcode);
    if d.operands.is_empty() {
        d.mnemonic
    } else {
        format!("{} {}", d.mnemonic, d.operands)
    }
}

/// A register-file index as the disassembler names it.
fn register_text(index: u8) -> String {
    match registers::decode_register(index) {
        RegisterRef::Accumulator => "a".to_string(),
        RegisterRef::ProductHigh => "acch".to_string(),
        RegisterRef::ProductLow => "accl".to_string(),
        RegisterRef::BusData => "busdata".to_string(),
        RegisterRef::RomData => "romdata".to_string(),
        RegisterRef::RamData => "ramdata".to_string(),
        RegisterRef::BusAddress => "busaddr".to_string(),
        RegisterRef::RamBase => "ramaddr".to_string(),
        RegisterRef::ProgramCounter => "pc".to_string(),
        RegisterRef::Page => "p".to_string(),
        RegisterRef::BusTransfer => "bus".to_string(),
        RegisterRef::Constant(value) => format!("#${value:06X}"),
        RegisterRef::General(n) => format!("r{n}"),
        RegisterRef::Unmapped => format!("reg:${:02X}", index & 0x7F),
    }
}

/// The operand an instruction's low byte gives: the immediate, or the
/// register it names.
fn operand_text(opcode: u16) -> String {
    match registers::operand(opcode) {
        Operand::Immediate(value) => format!("#${value:02X}"),
        Operand::Register(index) => register_text(index),
    }
}

/// A, with the shift an arithmetic or logic operation applies first.
fn shifted_accumulator_text(opcode: u16) -> String {
    match registers::shift_amount(opcode) {
        0 => "a".to_string(),
        shift => format!("a<<{shift}"),
    }
}

/// The data RAM address of RDRAM/WRRAM: an immediate added to the RAM base
/// (high-byte bit 2), or a register.
fn ram_address_text(opcode: u16) -> String {
    match registers::operand(opcode) {
        Operand::Immediate(offset) => format!("[#${offset:02X}+ramaddr]"),
        Operand::Register(index) => format!("[{}]", register_text(index)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One word of each family decodes to it (the low byte zero unless the
    /// family needs a bit set).
    #[test]
    fn decode_covers_every_family() {
        let cases: &[(u16, Op)] = &[
            (0x0000, Op::Nop),
            (0x0800, Op::Jump(Condition::Always)),
            (0x0C00, Op::Jump(Condition::Zero)),
            (0x1000, Op::Jump(Condition::Carry)),
            (0x1400, Op::Jump(Condition::Negative)),
            (0x1800, Op::Jump(Condition::Overflow)),
            (0x1C00, Op::WaitForBus),
            (0x3C00, Op::Return),
            (0x4000, Op::IncrementBusAddress),
            (0x4800, Op::Alu(AluOp::CompareFrom)),
            (0x5000, Op::Alu(AluOp::Compare)),
            (0x5900, Op::SignExtendByte),
            (0x5A00, Op::SignExtendWord),
            (0x6000, Op::Load),
            (0x6100, Op::ReadBus),
            (0x6300, Op::LoadPage),
            (0x6800, Op::ReadRam(Lane::Low)),
            (0x6900, Op::ReadRam(Lane::Middle)),
            (0x6A00, Op::ReadRam(Lane::High)),
            (0x7000, Op::ReadRom),
            (0x7C00, Op::LoadPageLow),
            (0x7D00, Op::LoadPageHigh),
            (0x8000, Op::Alu(AluOp::Add)),
            (0x8800, Op::Alu(AluOp::SubtractFrom)),
            (0x9000, Op::Alu(AluOp::Subtract)),
            (0x9800, Op::Multiply),
            (0xA800, Op::Alu(AluOp::Xor)),
            (0xB000, Op::Alu(AluOp::And)),
            (0xB800, Op::Alu(AluOp::Or)),
            (0xC000, Op::Shift(ShiftOp::Right)),
            (0xC800, Op::Shift(ShiftOp::ArithmeticRight)),
            (0xD000, Op::Shift(ShiftOp::RotateRight)),
            (0xD800, Op::Shift(ShiftOp::Left)),
            (0xE000, Op::Store),
            (0xE100, Op::StoreBusData),
            (0xE800, Op::WriteRam(Lane::Low)),
            (0xE900, Op::WriteRam(Lane::Middle)),
            (0xEA00, Op::WriteRam(Lane::High)),
            (0xF000, Op::Swap),
            (0xF800, Op::Clear),
            (0xFC00, Op::Halt),
        ];
        for &(opcode, expected) in cases {
            assert_eq!(decode(opcode), expected, "opcode {opcode:#06X}");
        }
    }

    /// The bits a family ignores do not change it: the immediate bit, the
    /// shift of A, and a jump's call and long bits.
    #[test]
    fn ignored_bits_keep_the_family() {
        assert_eq!(decode(0x8755), Op::Alu(AluOp::Add));
        assert_eq!(decode(0x5701), Op::Alu(AluOp::Compare));
        assert_eq!(decode(0x2A12), Op::Jump(Condition::Always));
        assert_eq!(decode(0x3612), Op::Jump(Condition::Negative));
        assert_eq!(decode(0x6D10), Op::ReadRam(Lane::Middle));
        assert_eq!(decode(0xF3FF), Op::Swap);
    }

    #[test]
    fn a_skip_takes_its_value_from_bit_0() {
        let skip = |flag, when_set| Op::Skip { flag, when_set };
        assert_eq!(decode(0x2400), skip(Flag::Overflow, false));
        assert_eq!(decode(0x2401), skip(Flag::Overflow, true));
        assert_eq!(decode(0x2500), skip(Flag::Carry, false));
        assert_eq!(decode(0x2501), skip(Flag::Carry, true));
        assert_eq!(decode(0x2600), skip(Flag::Zero, false));
        assert_eq!(decode(0x2601), skip(Flag::Zero, true));
        assert_eq!(decode(0x2700), skip(Flag::Negative, false));
        assert_eq!(decode(0x2701), skip(Flag::Negative, true));
        assert_eq!(disassemble_line(0x2400), "skipvc");
        assert_eq!(disassemble_line(0x2501), "skipge");
        assert_eq!(disassemble_line(0x2600), "skipne");
        assert_eq!(disassemble_line(0x2701), "skipmi");
    }

    /// `$E1xx` is WRBUS here; bsnes-plus never decodes it (its comparison
    /// has a fifth hex digit) and halts on it.
    #[test]
    fn wrbus_decodes_where_bsnes_plus_does_not() {
        assert_eq!(decode(0xE100), Op::StoreBusData);
        assert_eq!(decode(0xE1FF), Op::StoreBusData);
        assert_eq!(decode(0xE000), Op::Store);
        assert_eq!(disassemble_line(0xE100), "wrbus a");
    }

    /// Holes in the table, and `$70` with a non-zero low byte, decode to
    /// nothing. (`$2800` is not one: it is a call.)
    #[test]
    fn unknown_words_decode_to_unknown() {
        for opcode in [0x2000u16, 0x2100, 0x6200, 0x7001] {
            assert_eq!(decode(opcode), Op::Unknown, "opcode {opcode:#06X}");
        }
        assert_eq!(disassemble_line(0x2000), "unknown $2000");
    }

    #[test]
    fn jumps_write_their_call_and_long_forms() {
        assert_eq!(disassemble_line(0x0812), "jump $12");
        assert_eq!(disassemble_line(0x0A34), "jump p:$34");
        assert_eq!(disassemble_line(0x2812), "jsr $12");
        assert_eq!(disassemble_line(0x2C00), "jsreq $00");
        assert_eq!(disassemble_line(0x0C05), "jumpeq $05");
    }

    #[test]
    fn alu_operands_write_the_shift_of_a() {
        assert_eq!(disassemble_line(0x8060), "add a,r0");
        assert_eq!(disassemble_line(0x8555), "add a<<1,#$55");
        assert_eq!(disassemble_line(0x5301), "cmp a<<16,acch");
    }

    #[test]
    fn loads_and_register_operands_use_the_register_names() {
        assert_eq!(disassemble_line(0x6455), "ld a,#$55");
        assert_eq!(disassemble_line(0x6065), "ld a,r5");
        assert_eq!(disassemble_line(0x6328), "ld p,p");
        assert_eq!(disassemble_line(0x7C40), "ld pl,#$40");
        assert_eq!(disassemble_line(0x7D80), "ld ph,#$80");
        assert_eq!(disassemble_line(0xE063), "st r3,a");
        assert_eq!(disassemble_line(0xF06F), "swap a,r15");
        assert_eq!(disassemble_line(0x6103), "rdbus busdata");
        assert_eq!(disassemble_line(0x6056), "ld a,#$800000");
    }

    #[test]
    fn ram_addresses_write_the_base_or_the_register() {
        assert_eq!(disassemble_line(0x6800), "rdraml [a]");
        assert_eq!(disassemble_line(0x6C10), "rdraml [#$10+ramaddr]");
        assert_eq!(disassemble_line(0xE800), "wrraml [a]");
        assert_eq!(disassemble_line(0xEC10), "wrraml [#$10+ramaddr]");
    }

    #[test]
    fn instructions_without_operands_write_only_the_mnemonic() {
        assert_eq!(disassemble_line(0x0000), "nop");
        assert_eq!(disassemble_line(0x3C00), "ret");
        assert_eq!(disassemble_line(0x7000), "rdrom");
        assert_eq!(disassemble_line(0xF800), "clear");
        assert_eq!(disassemble_line(0xFC00), "halt");
        assert_eq!(disassemble_line(0x1C00), "loop");
    }

    #[test]
    fn every_mnemonic_is_lowercase() {
        for opcode in 0u32..=0xFFFF {
            let opcode = opcode as u16;
            let name = decode(opcode).name(opcode);
            assert!(!name.is_empty());
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase()),
                "mnemonic {name:?} must be lowercase"
            );
        }
    }
}
