//! SPC700 opcode decode table.
//!
//! The 256-entry [`OPCODE_TABLE`] maps each opcode byte to its
//! [`Mnemonic`], operand-shape ([`AddressingMode`]), and the **minimum**
//! cycle count observed across the SingleStepTests dataset (i.e. the
//! "branch not taken / condition not met" baseline). Instruction
//! handlers add penalty cycles on top of that baseline when branches
//! actually fire.
//!
//! Mnemonics are uppercase Rust identifiers; lowercase rendering (for
//! the disassembler) comes from [`Mnemonic::name`]. The bit-indexed
//! families (`set0..=set7`, `clr0..=clr7`, `bbs0..=bbs7`,
//! `bbc0..=bbc7`) are one mnemonic each here, the bit staying in the
//! opcode byte: the disassembler recovers it from `(opcode >> 5) & 7`,
//! and TCALL recovers its target from `(opcode >> 4) & 0xF`.

use crate::addressing::AddressingMode;

/// Every distinct mnemonic exposed by the SPC700 instruction set
/// (66 in total). `ILL` is reserved for future use; the SPC700 has no
/// strictly illegal opcodes, all 256 bytes are defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mnemonic {
    ADC,
    ADDW,
    AND,
    AND1,
    ASL,
    BBC,
    BBS,
    BCC,
    BCS,
    BEQ,
    BMI,
    BNE,
    BPL,
    BRA,
    BRK,
    BVC,
    BVS,
    CALL,
    CBNE,
    CLR1,
    CLRC,
    CLRP,
    CLRV,
    CMP,
    CMPW,
    DAA,
    DAS,
    DBNZ,
    DEC,
    DECW,
    DI,
    DIV,
    EI,
    EOR,
    EOR1,
    INC,
    INCW,
    JMP,
    LSR,
    MOV,
    MOV1,
    MOVW,
    MUL,
    NOP,
    NOT1,
    NOTC,
    OR,
    OR1,
    PCALL,
    POP,
    PUSH,
    RET,
    RETI,
    ROL,
    ROR,
    SBC,
    SET1,
    SETC,
    SETP,
    SLEEP,
    STOP,
    SUBW,
    TCALL,
    TCLR,
    TSET,
    XCN,
    /// Reserved placeholder. Never present in [`OPCODE_TABLE`] today
    /// (every SPC700 byte decodes to a real instruction); kept so the
    /// disassembler can render unknown bytes gracefully if the table
    /// ever needs to grow.
    ILL,
}

impl Mnemonic {
    /// Lowercase rendering, alphabetic only, the trace format's
    /// spelling. `ILL` renders as `"???"`, as in the 65C816
    /// disassembler.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ADC => "adc",
            Self::ADDW => "addw",
            Self::AND => "and",
            Self::AND1 => "and1",
            Self::ASL => "asl",
            Self::BBC => "bbc",
            Self::BBS => "bbs",
            Self::BCC => "bcc",
            Self::BCS => "bcs",
            Self::BEQ => "beq",
            Self::BMI => "bmi",
            Self::BNE => "bne",
            Self::BPL => "bpl",
            Self::BRA => "bra",
            Self::BRK => "brk",
            Self::BVC => "bvc",
            Self::BVS => "bvs",
            Self::CALL => "call",
            Self::CBNE => "cbne",
            Self::CLR1 => "clr1",
            Self::CLRC => "clrc",
            Self::CLRP => "clrp",
            Self::CLRV => "clrv",
            Self::CMP => "cmp",
            Self::CMPW => "cmpw",
            Self::DAA => "daa",
            Self::DAS => "das",
            Self::DBNZ => "dbnz",
            Self::DEC => "dec",
            Self::DECW => "decw",
            Self::DI => "di",
            Self::DIV => "div",
            Self::EI => "ei",
            Self::EOR => "eor",
            Self::EOR1 => "eor1",
            Self::INC => "inc",
            Self::INCW => "incw",
            Self::JMP => "jmp",
            Self::LSR => "lsr",
            Self::MOV => "mov",
            Self::MOV1 => "mov1",
            Self::MOVW => "movw",
            Self::MUL => "mul",
            Self::NOP => "nop",
            Self::NOT1 => "not1",
            Self::NOTC => "notc",
            Self::OR => "or",
            Self::OR1 => "or1",
            Self::PCALL => "pcall",
            Self::POP => "pop",
            Self::PUSH => "push",
            Self::RET => "ret",
            Self::RETI => "reti",
            Self::ROL => "rol",
            Self::ROR => "ror",
            Self::SBC => "sbc",
            Self::SET1 => "set1",
            Self::SETC => "setc",
            Self::SETP => "setp",
            Self::SLEEP => "sleep",
            Self::STOP => "stop",
            Self::SUBW => "subw",
            Self::TCALL => "tcall",
            Self::TCLR => "tclr",
            Self::TSET => "tset",
            Self::XCN => "xcn",
            Self::ILL => "???",
        }
    }
}

/// One row of the dispatch table: mnemonic + operand shape + baseline
/// cycle count. The cycle count is the *minimum* observed across the
/// 10 000 SingleStepTests cases per opcode (i.e. the "branch not
/// taken / condition not met" path). Handlers add penalty cycles on
/// top when a conditional path actually fires.
#[derive(Debug, Clone, Copy)]
pub struct OpcodeInfo {
    pub mnemonic: Mnemonic,
    pub mode: AddressingMode,
    pub base_cycles: u8,
}

/// Decode a byte into its [`OpcodeInfo`] entry.
#[inline]
pub fn decode(opcode: u8) -> &'static OpcodeInfo {
    &OPCODE_TABLE[opcode as usize]
}

/// The full SPC700 opcode dispatch table. Each entry maps a byte
/// opcode to its mnemonic, operand shape, and baseline cycle count.
/// The cycle count is the minimum observed across the SingleStepTests
/// dataset (the not-taken / no-extra path for conditional opcodes).
/// Handlers add penalty cycles when branches or conditions fire.
#[rustfmt::skip]
pub static OPCODE_TABLE: [OpcodeInfo; 256] = [
    OpcodeInfo { mnemonic: Mnemonic::NOP,   mode: AddressingMode::Implied,             base_cycles: 2 },  // 0x00 nop
    OpcodeInfo { mnemonic: Mnemonic::TCALL, mode: AddressingMode::TCallVector,         base_cycles: 8 },  // 0x01 tcall n=0
    OpcodeInfo { mnemonic: Mnemonic::SET1,  mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0x02 set0 bit=0
    OpcodeInfo { mnemonic: Mnemonic::BBS,   mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0x03 bbs0 bit=0
    OpcodeInfo { mnemonic: Mnemonic::OR,    mode: AddressingMode::DirectPage,          base_cycles: 3 },  // 0x04 or a,$dp
    OpcodeInfo { mnemonic: Mnemonic::OR,    mode: AddressingMode::Absolute,            base_cycles: 4 },  // 0x05 or a,!abs
    OpcodeInfo { mnemonic: Mnemonic::OR,    mode: AddressingMode::IndirectX,           base_cycles: 3 },  // 0x06 or a,(x)
    OpcodeInfo { mnemonic: Mnemonic::OR,    mode: AddressingMode::IndirectIndirectX,   base_cycles: 6 },  // 0x07 or a,($dp+x)
    OpcodeInfo { mnemonic: Mnemonic::OR,    mode: AddressingMode::Immediate,           base_cycles: 2 },  // 0x08 or a,#imm
    OpcodeInfo { mnemonic: Mnemonic::OR,    mode: AddressingMode::DirectPageDirectPage,base_cycles: 6 },  // 0x09 or $dp,$dp
    OpcodeInfo { mnemonic: Mnemonic::OR1,   mode: AddressingMode::MemBit,              base_cycles: 5 },  // 0x0A or1 c,m.b
    OpcodeInfo { mnemonic: Mnemonic::ASL,   mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0x0B asl $dp
    OpcodeInfo { mnemonic: Mnemonic::ASL,   mode: AddressingMode::Absolute,            base_cycles: 5 },  // 0x0C asl !abs
    OpcodeInfo { mnemonic: Mnemonic::PUSH,  mode: AddressingMode::Implied,             base_cycles: 4 },  // 0x0D push psw
    OpcodeInfo { mnemonic: Mnemonic::TSET,  mode: AddressingMode::Absolute,            base_cycles: 6 },  // 0x0E tset !abs,a
    OpcodeInfo { mnemonic: Mnemonic::BRK,   mode: AddressingMode::Implied,             base_cycles: 8 },  // 0x0F brk

    OpcodeInfo { mnemonic: Mnemonic::BPL,   mode: AddressingMode::Relative,            base_cycles: 2 },  // 0x10 bpl
    OpcodeInfo { mnemonic: Mnemonic::TCALL, mode: AddressingMode::TCallVector,         base_cycles: 8 },  // 0x11 tcall n=1
    OpcodeInfo { mnemonic: Mnemonic::CLR1,  mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0x12 clr0 bit=0
    OpcodeInfo { mnemonic: Mnemonic::BBC,   mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0x13 bbc0 bit=0
    OpcodeInfo { mnemonic: Mnemonic::OR,    mode: AddressingMode::DirectPageX,         base_cycles: 4 },  // 0x14 or a,$dp+x
    OpcodeInfo { mnemonic: Mnemonic::OR,    mode: AddressingMode::AbsoluteX,           base_cycles: 5 },  // 0x15 or a,!abs+x
    OpcodeInfo { mnemonic: Mnemonic::OR,    mode: AddressingMode::AbsoluteY,           base_cycles: 5 },  // 0x16 or a,!abs+y
    OpcodeInfo { mnemonic: Mnemonic::OR,    mode: AddressingMode::IndirectIndirectY,   base_cycles: 6 },  // 0x17 or a,($dp)+y
    OpcodeInfo { mnemonic: Mnemonic::OR,    mode: AddressingMode::DirectPageImm,       base_cycles: 5 },  // 0x18 or $dp,#imm
    OpcodeInfo { mnemonic: Mnemonic::OR,    mode: AddressingMode::IxIy,                base_cycles: 5 },  // 0x19 or (x),(y)
    OpcodeInfo { mnemonic: Mnemonic::DECW,  mode: AddressingMode::DirectPage,          base_cycles: 6 },  // 0x1A decw $dp
    OpcodeInfo { mnemonic: Mnemonic::ASL,   mode: AddressingMode::DirectPageX,         base_cycles: 5 },  // 0x1B asl $dp+x
    OpcodeInfo { mnemonic: Mnemonic::ASL,   mode: AddressingMode::Accumulator,         base_cycles: 2 },  // 0x1C asl a
    OpcodeInfo { mnemonic: Mnemonic::DEC,   mode: AddressingMode::Implied,             base_cycles: 2 },  // 0x1D dec x
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::Absolute,            base_cycles: 4 },  // 0x1E cmp x,!abs
    OpcodeInfo { mnemonic: Mnemonic::JMP,   mode: AddressingMode::AbsoluteIndirectX,   base_cycles: 6 },  // 0x1F jmp [!abs+x]

    OpcodeInfo { mnemonic: Mnemonic::CLRP,  mode: AddressingMode::Implied,             base_cycles: 2 },  // 0x20 clrp
    OpcodeInfo { mnemonic: Mnemonic::TCALL, mode: AddressingMode::TCallVector,         base_cycles: 8 },  // 0x21 tcall n=2
    OpcodeInfo { mnemonic: Mnemonic::SET1,  mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0x22 set1 bit=1
    OpcodeInfo { mnemonic: Mnemonic::BBS,   mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0x23 bbs1 bit=1
    OpcodeInfo { mnemonic: Mnemonic::AND,   mode: AddressingMode::DirectPage,          base_cycles: 3 },  // 0x24 and a,$dp
    OpcodeInfo { mnemonic: Mnemonic::AND,   mode: AddressingMode::Absolute,            base_cycles: 4 },  // 0x25 and a,!abs
    OpcodeInfo { mnemonic: Mnemonic::AND,   mode: AddressingMode::IndirectX,           base_cycles: 3 },  // 0x26 and a,(x)
    OpcodeInfo { mnemonic: Mnemonic::AND,   mode: AddressingMode::IndirectIndirectX,   base_cycles: 6 },  // 0x27 and a,($dp+x)
    OpcodeInfo { mnemonic: Mnemonic::AND,   mode: AddressingMode::Immediate,           base_cycles: 2 },  // 0x28 and a,#imm
    OpcodeInfo { mnemonic: Mnemonic::AND,   mode: AddressingMode::DirectPageDirectPage,base_cycles: 6 },  // 0x29 and $dp,$dp
    OpcodeInfo { mnemonic: Mnemonic::OR1,   mode: AddressingMode::MemBitNot,           base_cycles: 5 },  // 0x2A or1 c,/m.b
    OpcodeInfo { mnemonic: Mnemonic::ROL,   mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0x2B rol $dp
    OpcodeInfo { mnemonic: Mnemonic::ROL,   mode: AddressingMode::Absolute,            base_cycles: 5 },  // 0x2C rol !abs
    OpcodeInfo { mnemonic: Mnemonic::PUSH,  mode: AddressingMode::Accumulator,         base_cycles: 4 },  // 0x2D push a
    OpcodeInfo { mnemonic: Mnemonic::CBNE,  mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0x2E cbne $dp,rel
    OpcodeInfo { mnemonic: Mnemonic::BRA,   mode: AddressingMode::Relative,            base_cycles: 4 },  // 0x2F bra

    OpcodeInfo { mnemonic: Mnemonic::BMI,   mode: AddressingMode::Relative,            base_cycles: 2 },  // 0x30 bmi
    OpcodeInfo { mnemonic: Mnemonic::TCALL, mode: AddressingMode::TCallVector,         base_cycles: 8 },  // 0x31 tcall n=3
    OpcodeInfo { mnemonic: Mnemonic::CLR1,  mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0x32 clr1 bit=1
    OpcodeInfo { mnemonic: Mnemonic::BBC,   mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0x33 bbc1 bit=1
    OpcodeInfo { mnemonic: Mnemonic::AND,   mode: AddressingMode::DirectPageX,         base_cycles: 4 },  // 0x34 and a,$dp+x
    OpcodeInfo { mnemonic: Mnemonic::AND,   mode: AddressingMode::AbsoluteX,           base_cycles: 5 },  // 0x35 and a,!abs+x
    OpcodeInfo { mnemonic: Mnemonic::AND,   mode: AddressingMode::AbsoluteY,           base_cycles: 5 },  // 0x36 and a,!abs+y
    OpcodeInfo { mnemonic: Mnemonic::AND,   mode: AddressingMode::IndirectIndirectY,   base_cycles: 6 },  // 0x37 and a,($dp)+y
    OpcodeInfo { mnemonic: Mnemonic::AND,   mode: AddressingMode::DirectPageImm,       base_cycles: 5 },  // 0x38 and $dp,#imm
    OpcodeInfo { mnemonic: Mnemonic::AND,   mode: AddressingMode::IxIy,                base_cycles: 5 },  // 0x39 and (x),(y)
    OpcodeInfo { mnemonic: Mnemonic::INCW,  mode: AddressingMode::DirectPage,          base_cycles: 6 },  // 0x3A incw $dp
    OpcodeInfo { mnemonic: Mnemonic::ROL,   mode: AddressingMode::DirectPageX,         base_cycles: 5 },  // 0x3B rol $dp+x
    OpcodeInfo { mnemonic: Mnemonic::ROL,   mode: AddressingMode::Accumulator,         base_cycles: 2 },  // 0x3C rol a
    OpcodeInfo { mnemonic: Mnemonic::INC,   mode: AddressingMode::Implied,             base_cycles: 2 },  // 0x3D inc x
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::DirectPage,          base_cycles: 3 },  // 0x3E cmp x,$dp
    OpcodeInfo { mnemonic: Mnemonic::CALL,  mode: AddressingMode::Absolute,            base_cycles: 8 },  // 0x3F call !abs

    OpcodeInfo { mnemonic: Mnemonic::SETP,  mode: AddressingMode::Implied,             base_cycles: 2 },  // 0x40 setp
    OpcodeInfo { mnemonic: Mnemonic::TCALL, mode: AddressingMode::TCallVector,         base_cycles: 8 },  // 0x41 tcall n=4
    OpcodeInfo { mnemonic: Mnemonic::SET1,  mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0x42 set2 bit=2
    OpcodeInfo { mnemonic: Mnemonic::BBS,   mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0x43 bbs2 bit=2
    OpcodeInfo { mnemonic: Mnemonic::EOR,   mode: AddressingMode::DirectPage,          base_cycles: 3 },  // 0x44 eor a,$dp
    OpcodeInfo { mnemonic: Mnemonic::EOR,   mode: AddressingMode::Absolute,            base_cycles: 4 },  // 0x45 eor a,!abs
    OpcodeInfo { mnemonic: Mnemonic::EOR,   mode: AddressingMode::IndirectX,           base_cycles: 3 },  // 0x46 eor a,(x)
    OpcodeInfo { mnemonic: Mnemonic::EOR,   mode: AddressingMode::IndirectIndirectX,   base_cycles: 6 },  // 0x47 eor a,($dp+x)
    OpcodeInfo { mnemonic: Mnemonic::EOR,   mode: AddressingMode::Immediate,           base_cycles: 2 },  // 0x48 eor a,#imm
    OpcodeInfo { mnemonic: Mnemonic::EOR,   mode: AddressingMode::DirectPageDirectPage,base_cycles: 6 },  // 0x49 eor $dp,$dp
    OpcodeInfo { mnemonic: Mnemonic::AND1,  mode: AddressingMode::MemBit,              base_cycles: 4 },  // 0x4A and1 c,m.b
    OpcodeInfo { mnemonic: Mnemonic::LSR,   mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0x4B lsr $dp
    OpcodeInfo { mnemonic: Mnemonic::LSR,   mode: AddressingMode::Absolute,            base_cycles: 5 },  // 0x4C lsr !abs
    OpcodeInfo { mnemonic: Mnemonic::PUSH,  mode: AddressingMode::Implied,             base_cycles: 4 },  // 0x4D push x
    OpcodeInfo { mnemonic: Mnemonic::TCLR,  mode: AddressingMode::Absolute,            base_cycles: 6 },  // 0x4E tclr !abs,a
    OpcodeInfo { mnemonic: Mnemonic::PCALL, mode: AddressingMode::PCallVector,         base_cycles: 6 },  // 0x4F pcall

    OpcodeInfo { mnemonic: Mnemonic::BVC,   mode: AddressingMode::Relative,            base_cycles: 2 },  // 0x50 bvc
    OpcodeInfo { mnemonic: Mnemonic::TCALL, mode: AddressingMode::TCallVector,         base_cycles: 8 },  // 0x51 tcall n=5
    OpcodeInfo { mnemonic: Mnemonic::CLR1,  mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0x52 clr2 bit=2
    OpcodeInfo { mnemonic: Mnemonic::BBC,   mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0x53 bbc2 bit=2
    OpcodeInfo { mnemonic: Mnemonic::EOR,   mode: AddressingMode::DirectPageX,         base_cycles: 4 },  // 0x54 eor a,$dp+x
    OpcodeInfo { mnemonic: Mnemonic::EOR,   mode: AddressingMode::AbsoluteX,           base_cycles: 5 },  // 0x55 eor a,!abs+x
    OpcodeInfo { mnemonic: Mnemonic::EOR,   mode: AddressingMode::AbsoluteY,           base_cycles: 5 },  // 0x56 eor a,!abs+y
    OpcodeInfo { mnemonic: Mnemonic::EOR,   mode: AddressingMode::IndirectIndirectY,   base_cycles: 6 },  // 0x57 eor a,($dp)+y
    OpcodeInfo { mnemonic: Mnemonic::EOR,   mode: AddressingMode::DirectPageImm,       base_cycles: 5 },  // 0x58 eor $dp,#imm
    OpcodeInfo { mnemonic: Mnemonic::EOR,   mode: AddressingMode::IxIy,                base_cycles: 5 },  // 0x59 eor (x),(y)
    OpcodeInfo { mnemonic: Mnemonic::CMPW,  mode: AddressingMode::YaDirectPage,        base_cycles: 4 },  // 0x5A cmpw ya,$dp
    OpcodeInfo { mnemonic: Mnemonic::LSR,   mode: AddressingMode::DirectPageX,         base_cycles: 5 },  // 0x5B lsr $dp+x
    OpcodeInfo { mnemonic: Mnemonic::LSR,   mode: AddressingMode::Accumulator,         base_cycles: 2 },  // 0x5C lsr a
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::Implied,             base_cycles: 2 },  // 0x5D mov x,a
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::Absolute,            base_cycles: 4 },  // 0x5E cmp y,!abs
    OpcodeInfo { mnemonic: Mnemonic::JMP,   mode: AddressingMode::Absolute,            base_cycles: 3 },  // 0x5F jmp !abs

    OpcodeInfo { mnemonic: Mnemonic::CLRC,  mode: AddressingMode::Implied,             base_cycles: 2 },  // 0x60 clrc
    OpcodeInfo { mnemonic: Mnemonic::TCALL, mode: AddressingMode::TCallVector,         base_cycles: 8 },  // 0x61 tcall n=6
    OpcodeInfo { mnemonic: Mnemonic::SET1,  mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0x62 set3 bit=3
    OpcodeInfo { mnemonic: Mnemonic::BBS,   mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0x63 bbs3 bit=3
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::DirectPage,          base_cycles: 3 },  // 0x64 cmp a,$dp
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::Absolute,            base_cycles: 4 },  // 0x65 cmp a,!abs
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::IndirectX,           base_cycles: 3 },  // 0x66 cmp a,(x)
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::IndirectIndirectX,   base_cycles: 6 },  // 0x67 cmp a,($dp+x)
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::Immediate,           base_cycles: 2 },  // 0x68 cmp a,#imm
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::DirectPageDirectPage,base_cycles: 6 },  // 0x69 cmp $dp,$dp
    OpcodeInfo { mnemonic: Mnemonic::AND1,  mode: AddressingMode::MemBitNot,           base_cycles: 4 },  // 0x6A and1 c,/m.b
    OpcodeInfo { mnemonic: Mnemonic::ROR,   mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0x6B ror $dp
    OpcodeInfo { mnemonic: Mnemonic::ROR,   mode: AddressingMode::Absolute,            base_cycles: 5 },  // 0x6C ror !abs
    OpcodeInfo { mnemonic: Mnemonic::PUSH,  mode: AddressingMode::Implied,             base_cycles: 4 },  // 0x6D push y
    OpcodeInfo { mnemonic: Mnemonic::DBNZ,  mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0x6E dbnz $dp,rel
    OpcodeInfo { mnemonic: Mnemonic::RET,   mode: AddressingMode::Implied,             base_cycles: 5 },  // 0x6F ret

    OpcodeInfo { mnemonic: Mnemonic::BVS,   mode: AddressingMode::Relative,            base_cycles: 2 },  // 0x70 bvs
    OpcodeInfo { mnemonic: Mnemonic::TCALL, mode: AddressingMode::TCallVector,         base_cycles: 8 },  // 0x71 tcall n=7
    OpcodeInfo { mnemonic: Mnemonic::CLR1,  mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0x72 clr3 bit=3
    OpcodeInfo { mnemonic: Mnemonic::BBC,   mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0x73 bbc3 bit=3
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::DirectPageX,         base_cycles: 4 },  // 0x74 cmp a,$dp+x
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::AbsoluteX,           base_cycles: 5 },  // 0x75 cmp a,!abs+x
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::AbsoluteY,           base_cycles: 5 },  // 0x76 cmp a,!abs+y
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::IndirectIndirectY,   base_cycles: 6 },  // 0x77 cmp a,($dp)+y
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::DirectPageImm,       base_cycles: 5 },  // 0x78 cmp $dp,#imm
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::IxIy,                base_cycles: 5 },  // 0x79 cmp (x),(y)
    OpcodeInfo { mnemonic: Mnemonic::ADDW,  mode: AddressingMode::YaDirectPage,        base_cycles: 5 },  // 0x7A addw ya,$dp
    OpcodeInfo { mnemonic: Mnemonic::ROR,   mode: AddressingMode::DirectPageX,         base_cycles: 5 },  // 0x7B ror $dp+x
    OpcodeInfo { mnemonic: Mnemonic::ROR,   mode: AddressingMode::Accumulator,         base_cycles: 2 },  // 0x7C ror a
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::Implied,             base_cycles: 2 },  // 0x7D mov a,x
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::DirectPage,          base_cycles: 3 },  // 0x7E cmp y,$dp
    OpcodeInfo { mnemonic: Mnemonic::RETI,  mode: AddressingMode::Implied,             base_cycles: 6 },  // 0x7F reti

    OpcodeInfo { mnemonic: Mnemonic::SETC,  mode: AddressingMode::Implied,             base_cycles: 2 },  // 0x80 setc
    OpcodeInfo { mnemonic: Mnemonic::TCALL, mode: AddressingMode::TCallVector,         base_cycles: 8 },  // 0x81 tcall n=8
    OpcodeInfo { mnemonic: Mnemonic::SET1,  mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0x82 set4 bit=4
    OpcodeInfo { mnemonic: Mnemonic::BBS,   mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0x83 bbs4 bit=4
    OpcodeInfo { mnemonic: Mnemonic::ADC,   mode: AddressingMode::DirectPage,          base_cycles: 3 },  // 0x84 adc a,$dp
    OpcodeInfo { mnemonic: Mnemonic::ADC,   mode: AddressingMode::Absolute,            base_cycles: 4 },  // 0x85 adc a,!abs
    OpcodeInfo { mnemonic: Mnemonic::ADC,   mode: AddressingMode::IndirectX,           base_cycles: 3 },  // 0x86 adc a,(x)
    OpcodeInfo { mnemonic: Mnemonic::ADC,   mode: AddressingMode::IndirectIndirectX,   base_cycles: 6 },  // 0x87 adc a,($dp+x)
    OpcodeInfo { mnemonic: Mnemonic::ADC,   mode: AddressingMode::Immediate,           base_cycles: 2 },  // 0x88 adc a,#imm
    OpcodeInfo { mnemonic: Mnemonic::ADC,   mode: AddressingMode::DirectPageDirectPage,base_cycles: 6 },  // 0x89 adc $dp,$dp
    OpcodeInfo { mnemonic: Mnemonic::EOR1,  mode: AddressingMode::MemBit,              base_cycles: 5 },  // 0x8A eor1 c,m.b
    OpcodeInfo { mnemonic: Mnemonic::DEC,   mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0x8B dec $dp
    OpcodeInfo { mnemonic: Mnemonic::DEC,   mode: AddressingMode::Absolute,            base_cycles: 5 },  // 0x8C dec !abs
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::Immediate,           base_cycles: 2 },  // 0x8D mov y,#imm
    OpcodeInfo { mnemonic: Mnemonic::POP,   mode: AddressingMode::Implied,             base_cycles: 4 },  // 0x8E pop psw
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::DirectPageImm,       base_cycles: 5 },  // 0x8F mov $dp,#imm

    OpcodeInfo { mnemonic: Mnemonic::BCC,   mode: AddressingMode::Relative,            base_cycles: 2 },  // 0x90 bcc
    OpcodeInfo { mnemonic: Mnemonic::TCALL, mode: AddressingMode::TCallVector,         base_cycles: 8 },  // 0x91 tcall n=9
    OpcodeInfo { mnemonic: Mnemonic::CLR1,  mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0x92 clr4 bit=4
    OpcodeInfo { mnemonic: Mnemonic::BBC,   mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0x93 bbc4 bit=4
    OpcodeInfo { mnemonic: Mnemonic::ADC,   mode: AddressingMode::DirectPageX,         base_cycles: 4 },  // 0x94 adc a,$dp+x
    OpcodeInfo { mnemonic: Mnemonic::ADC,   mode: AddressingMode::AbsoluteX,           base_cycles: 5 },  // 0x95 adc a,!abs+x
    OpcodeInfo { mnemonic: Mnemonic::ADC,   mode: AddressingMode::AbsoluteY,           base_cycles: 5 },  // 0x96 adc a,!abs+y
    OpcodeInfo { mnemonic: Mnemonic::ADC,   mode: AddressingMode::IndirectIndirectY,   base_cycles: 6 },  // 0x97 adc a,($dp)+y
    OpcodeInfo { mnemonic: Mnemonic::ADC,   mode: AddressingMode::DirectPageImm,       base_cycles: 5 },  // 0x98 adc $dp,#imm
    OpcodeInfo { mnemonic: Mnemonic::ADC,   mode: AddressingMode::IxIy,                base_cycles: 5 },  // 0x99 adc (x),(y)
    OpcodeInfo { mnemonic: Mnemonic::SUBW,  mode: AddressingMode::YaDirectPage,        base_cycles: 5 },  // 0x9A subw ya,$dp
    OpcodeInfo { mnemonic: Mnemonic::DEC,   mode: AddressingMode::DirectPageX,         base_cycles: 5 },  // 0x9B dec $dp+x
    OpcodeInfo { mnemonic: Mnemonic::DEC,   mode: AddressingMode::Accumulator,         base_cycles: 2 },  // 0x9C dec a
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::Implied,             base_cycles: 2 },  // 0x9D mov x,sp
    OpcodeInfo { mnemonic: Mnemonic::DIV,   mode: AddressingMode::Implied,             base_cycles: 12 }, // 0x9E div ya,x
    OpcodeInfo { mnemonic: Mnemonic::XCN,   mode: AddressingMode::Accumulator,         base_cycles: 5 },  // 0x9F xcn a

    OpcodeInfo { mnemonic: Mnemonic::EI,    mode: AddressingMode::Implied,             base_cycles: 3 },  // 0xA0 ei
    OpcodeInfo { mnemonic: Mnemonic::TCALL, mode: AddressingMode::TCallVector,         base_cycles: 8 },  // 0xA1 tcall n=10
    OpcodeInfo { mnemonic: Mnemonic::SET1,  mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0xA2 set5 bit=5
    OpcodeInfo { mnemonic: Mnemonic::BBS,   mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0xA3 bbs5 bit=5
    OpcodeInfo { mnemonic: Mnemonic::SBC,   mode: AddressingMode::DirectPage,          base_cycles: 3 },  // 0xA4 sbc a,$dp
    OpcodeInfo { mnemonic: Mnemonic::SBC,   mode: AddressingMode::Absolute,            base_cycles: 4 },  // 0xA5 sbc a,!abs
    OpcodeInfo { mnemonic: Mnemonic::SBC,   mode: AddressingMode::IndirectX,           base_cycles: 3 },  // 0xA6 sbc a,(x)
    OpcodeInfo { mnemonic: Mnemonic::SBC,   mode: AddressingMode::IndirectIndirectX,   base_cycles: 6 },  // 0xA7 sbc a,($dp+x)
    OpcodeInfo { mnemonic: Mnemonic::SBC,   mode: AddressingMode::Immediate,           base_cycles: 2 },  // 0xA8 sbc a,#imm
    OpcodeInfo { mnemonic: Mnemonic::SBC,   mode: AddressingMode::DirectPageDirectPage,base_cycles: 6 },  // 0xA9 sbc $dp,$dp
    OpcodeInfo { mnemonic: Mnemonic::MOV1,  mode: AddressingMode::MemBit,              base_cycles: 4 },  // 0xAA mov1 c,m.b
    OpcodeInfo { mnemonic: Mnemonic::INC,   mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0xAB inc $dp
    OpcodeInfo { mnemonic: Mnemonic::INC,   mode: AddressingMode::Absolute,            base_cycles: 5 },  // 0xAC inc !abs
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::Immediate,           base_cycles: 2 },  // 0xAD cmp y,#imm
    OpcodeInfo { mnemonic: Mnemonic::POP,   mode: AddressingMode::Accumulator,         base_cycles: 4 },  // 0xAE pop a
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::IndirectXAuto,       base_cycles: 4 },  // 0xAF mov (x)+,a

    OpcodeInfo { mnemonic: Mnemonic::BCS,   mode: AddressingMode::Relative,            base_cycles: 2 },  // 0xB0 bcs
    OpcodeInfo { mnemonic: Mnemonic::TCALL, mode: AddressingMode::TCallVector,         base_cycles: 8 },  // 0xB1 tcall n=11
    OpcodeInfo { mnemonic: Mnemonic::CLR1,  mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0xB2 clr5 bit=5
    OpcodeInfo { mnemonic: Mnemonic::BBC,   mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0xB3 bbc5 bit=5
    OpcodeInfo { mnemonic: Mnemonic::SBC,   mode: AddressingMode::DirectPageX,         base_cycles: 4 },  // 0xB4 sbc a,$dp+x
    OpcodeInfo { mnemonic: Mnemonic::SBC,   mode: AddressingMode::AbsoluteX,           base_cycles: 5 },  // 0xB5 sbc a,!abs+x
    OpcodeInfo { mnemonic: Mnemonic::SBC,   mode: AddressingMode::AbsoluteY,           base_cycles: 5 },  // 0xB6 sbc a,!abs+y
    OpcodeInfo { mnemonic: Mnemonic::SBC,   mode: AddressingMode::IndirectIndirectY,   base_cycles: 6 },  // 0xB7 sbc a,($dp)+y
    OpcodeInfo { mnemonic: Mnemonic::SBC,   mode: AddressingMode::DirectPageImm,       base_cycles: 5 },  // 0xB8 sbc $dp,#imm
    OpcodeInfo { mnemonic: Mnemonic::SBC,   mode: AddressingMode::IxIy,                base_cycles: 5 },  // 0xB9 sbc (x),(y)
    OpcodeInfo { mnemonic: Mnemonic::MOVW,  mode: AddressingMode::YaDirectPage,        base_cycles: 5 },  // 0xBA movw ya,$dp
    OpcodeInfo { mnemonic: Mnemonic::INC,   mode: AddressingMode::DirectPageX,         base_cycles: 5 },  // 0xBB inc $dp+x
    OpcodeInfo { mnemonic: Mnemonic::INC,   mode: AddressingMode::Accumulator,         base_cycles: 2 },  // 0xBC inc a
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::Implied,             base_cycles: 2 },  // 0xBD mov sp,x
    OpcodeInfo { mnemonic: Mnemonic::DAS,   mode: AddressingMode::Accumulator,         base_cycles: 3 },  // 0xBE das a
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::IndirectXAuto,       base_cycles: 4 },  // 0xBF mov a,(x)+

    OpcodeInfo { mnemonic: Mnemonic::DI,    mode: AddressingMode::Implied,             base_cycles: 3 },  // 0xC0 di
    OpcodeInfo { mnemonic: Mnemonic::TCALL, mode: AddressingMode::TCallVector,         base_cycles: 8 },  // 0xC1 tcall n=12
    OpcodeInfo { mnemonic: Mnemonic::SET1,  mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0xC2 set6 bit=6
    OpcodeInfo { mnemonic: Mnemonic::BBS,   mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0xC3 bbs6 bit=6
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0xC4 mov $dp,a
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::Absolute,            base_cycles: 5 },  // 0xC5 mov !abs,a
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::IndirectX,           base_cycles: 4 },  // 0xC6 mov (x),a
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::IndirectIndirectX,   base_cycles: 7 },  // 0xC7 mov ($dp+x),a
    OpcodeInfo { mnemonic: Mnemonic::CMP,   mode: AddressingMode::Immediate,           base_cycles: 2 },  // 0xC8 cmp x,#imm
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::Absolute,            base_cycles: 5 },  // 0xC9 mov !abs,x
    OpcodeInfo { mnemonic: Mnemonic::MOV1,  mode: AddressingMode::MemBit,              base_cycles: 6 },  // 0xCA mov1 m.b,c
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0xCB mov $dp,y
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::Absolute,            base_cycles: 5 },  // 0xCC mov !abs,y
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::Immediate,           base_cycles: 2 },  // 0xCD mov x,#imm
    OpcodeInfo { mnemonic: Mnemonic::POP,   mode: AddressingMode::Implied,             base_cycles: 4 },  // 0xCE pop x
    OpcodeInfo { mnemonic: Mnemonic::MUL,   mode: AddressingMode::Implied,             base_cycles: 9 },  // 0xCF mul ya

    OpcodeInfo { mnemonic: Mnemonic::BNE,   mode: AddressingMode::Relative,            base_cycles: 2 },  // 0xD0 bne
    OpcodeInfo { mnemonic: Mnemonic::TCALL, mode: AddressingMode::TCallVector,         base_cycles: 8 },  // 0xD1 tcall n=13
    OpcodeInfo { mnemonic: Mnemonic::CLR1,  mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0xD2 clr6 bit=6 (the trace format prints its operand as a branch target)
    OpcodeInfo { mnemonic: Mnemonic::BBC,   mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0xD3 bbc6 bit=6
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::DirectPageX,         base_cycles: 5 },  // 0xD4 mov $dp+x,a
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::AbsoluteX,           base_cycles: 6 },  // 0xD5 mov !abs+x,a
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::AbsoluteY,           base_cycles: 6 },  // 0xD6 mov !abs+y,a
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::IndirectIndirectY,   base_cycles: 7 },  // 0xD7 mov ($dp)+y,a
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0xD8 mov $dp,x
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::DirectPageY,         base_cycles: 5 },  // 0xD9 mov $dp+y,x
    OpcodeInfo { mnemonic: Mnemonic::MOVW,  mode: AddressingMode::YaDirectPage,        base_cycles: 5 },  // 0xDA movw $dp,ya
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::DirectPageX,         base_cycles: 5 },  // 0xDB mov $dp+x,y
    OpcodeInfo { mnemonic: Mnemonic::DEC,   mode: AddressingMode::Implied,             base_cycles: 2 },  // 0xDC dec y
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::Implied,             base_cycles: 2 },  // 0xDD mov a,y
    OpcodeInfo { mnemonic: Mnemonic::CBNE,  mode: AddressingMode::DirectPageXRelative, base_cycles: 6 },  // 0xDE cbne $dp+x,rel
    OpcodeInfo { mnemonic: Mnemonic::DAA,   mode: AddressingMode::Accumulator,         base_cycles: 3 },  // 0xDF daa a

    OpcodeInfo { mnemonic: Mnemonic::CLRV,  mode: AddressingMode::Implied,             base_cycles: 2 },  // 0xE0 clrv
    OpcodeInfo { mnemonic: Mnemonic::TCALL, mode: AddressingMode::TCallVector,         base_cycles: 8 },  // 0xE1 tcall n=14
    OpcodeInfo { mnemonic: Mnemonic::SET1,  mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0xE2 set7 bit=7
    OpcodeInfo { mnemonic: Mnemonic::BBS,   mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0xE3 bbs7 bit=7
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::DirectPage,          base_cycles: 3 },  // 0xE4 mov a,$dp
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::Absolute,            base_cycles: 4 },  // 0xE5 mov a,!abs
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::IndirectX,           base_cycles: 3 },  // 0xE6 mov a,(x)
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::IndirectIndirectX,   base_cycles: 6 },  // 0xE7 mov a,($dp+x)
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::Immediate,           base_cycles: 2 },  // 0xE8 mov a,#imm
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::Absolute,            base_cycles: 4 },  // 0xE9 mov x,!abs
    OpcodeInfo { mnemonic: Mnemonic::NOT1,  mode: AddressingMode::MemBit,              base_cycles: 5 },  // 0xEA not1 m.b
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::DirectPage,          base_cycles: 3 },  // 0xEB mov y,$dp
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::Absolute,            base_cycles: 4 },  // 0xEC mov y,!abs
    OpcodeInfo { mnemonic: Mnemonic::NOTC,  mode: AddressingMode::Implied,             base_cycles: 3 },  // 0xED notc
    OpcodeInfo { mnemonic: Mnemonic::POP,   mode: AddressingMode::Implied,             base_cycles: 4 },  // 0xEE pop y
    OpcodeInfo { mnemonic: Mnemonic::SLEEP, mode: AddressingMode::Implied,             base_cycles: 7 },  // 0xEF sleep

    OpcodeInfo { mnemonic: Mnemonic::BEQ,   mode: AddressingMode::Relative,            base_cycles: 2 },  // 0xF0 beq
    OpcodeInfo { mnemonic: Mnemonic::TCALL, mode: AddressingMode::TCallVector,         base_cycles: 8 },  // 0xF1 tcall n=15
    OpcodeInfo { mnemonic: Mnemonic::CLR1,  mode: AddressingMode::DirectPage,          base_cycles: 4 },  // 0xF2 clr7 bit=7
    OpcodeInfo { mnemonic: Mnemonic::BBC,   mode: AddressingMode::DirectPageRelative,  base_cycles: 5 },  // 0xF3 bbc7 bit=7
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::DirectPageX,         base_cycles: 4 },  // 0xF4 mov a,$dp+x
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::AbsoluteX,           base_cycles: 5 },  // 0xF5 mov a,!abs+x
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::AbsoluteY,           base_cycles: 5 },  // 0xF6 mov a,!abs+y
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::IndirectIndirectY,   base_cycles: 6 },  // 0xF7 mov a,($dp)+y
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::DirectPage,          base_cycles: 3 },  // 0xF8 mov x,$dp
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::DirectPageY,         base_cycles: 4 },  // 0xF9 mov x,$dp+y
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::DirectPageDirectPage,base_cycles: 5 },  // 0xFA mov $dp(dst),$dp(src)
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::DirectPageX,         base_cycles: 4 },  // 0xFB mov y,$dp+x
    OpcodeInfo { mnemonic: Mnemonic::INC,   mode: AddressingMode::Implied,             base_cycles: 2 },  // 0xFC inc y
    OpcodeInfo { mnemonic: Mnemonic::MOV,   mode: AddressingMode::Implied,             base_cycles: 2 },  // 0xFD mov y,a
    OpcodeInfo { mnemonic: Mnemonic::DBNZ,  mode: AddressingMode::YRelative,           base_cycles: 4 },  // 0xFE dbnz y,rel
    OpcodeInfo { mnemonic: Mnemonic::STOP,  mode: AddressingMode::Implied,             base_cycles: 7 },  // 0xFF stop
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_all_256_have_known_mnemonic() {
        // The SPC700 has no truly illegal opcodes; every byte must
        // decode to a real instruction. This guard catches future
        // table-editing mistakes that would leave a hole behind.
        for op in 0u16..256 {
            let info = decode(op as u8);
            assert_ne!(
                info.mnemonic,
                Mnemonic::ILL,
                "opcode {:02X} decoded to Mnemonic::ILL - every SPC700 byte must be defined",
                op
            );
        }
    }

    #[test]
    fn test_table_length_is_256() {
        assert_eq!(OPCODE_TABLE.len(), 256);
    }

    #[test]
    fn test_decode_indexing_matches_array() {
        for op in 0u16..256 {
            let by_helper = decode(op as u8);
            let by_index = &OPCODE_TABLE[op as usize];
            assert_eq!(by_helper.mnemonic, by_index.mnemonic);
            assert_eq!(by_helper.mode, by_index.mode);
            assert_eq!(by_helper.base_cycles, by_index.base_cycles);
        }
    }

    #[test]
    fn test_mnemonic_names_are_lowercase_and_short() {
        for &mnem in &[
            Mnemonic::NOP,
            Mnemonic::MOV,
            Mnemonic::MOVW,
            Mnemonic::ADC,
            Mnemonic::SBC,
            Mnemonic::AND,
            Mnemonic::OR,
            Mnemonic::EOR,
            Mnemonic::CMP,
            Mnemonic::BBS,
            Mnemonic::BBC,
            Mnemonic::TCALL,
            Mnemonic::PCALL,
            Mnemonic::ILL,
        ] {
            let name = mnem.name();
            assert!(
                !name.is_empty() && name.len() <= 5,
                "mnemonic {:?} -> {:?} should be 1..=5 chars",
                mnem,
                name
            );
            assert!(
                name.chars().all(|c| !c.is_ascii_uppercase()),
                "mnemonic {:?} -> {:?} should be lowercase only",
                mnem,
                name
            );
        }
    }

    #[test]
    fn test_known_opcodes_decode_correctly() {
        // Sample a handful of opcodes representing all major families,
        // pinning their mnemonic + mode.
        let expectations: &[(u8, Mnemonic, AddressingMode)] = &[
            (0x00, Mnemonic::NOP, AddressingMode::Implied),
            (0x0F, Mnemonic::BRK, AddressingMode::Implied),
            (0x10, Mnemonic::BPL, AddressingMode::Relative),
            (0x2F, Mnemonic::BRA, AddressingMode::Relative),
            (0x5F, Mnemonic::JMP, AddressingMode::Absolute),
            (0x6F, Mnemonic::RET, AddressingMode::Implied),
            (0x7F, Mnemonic::RETI, AddressingMode::Implied),
            (0x8D, Mnemonic::MOV, AddressingMode::Immediate),
            (0xC4, Mnemonic::MOV, AddressingMode::DirectPage),
            (0xCD, Mnemonic::MOV, AddressingMode::Immediate),
            (0xE8, Mnemonic::MOV, AddressingMode::Immediate),
            (0xBA, Mnemonic::MOVW, AddressingMode::YaDirectPage),
            (0xDA, Mnemonic::MOVW, AddressingMode::YaDirectPage),
            (0xCF, Mnemonic::MUL, AddressingMode::Implied),
            (0x9E, Mnemonic::DIV, AddressingMode::Implied),
            (0xEF, Mnemonic::SLEEP, AddressingMode::Implied),
            (0xFF, Mnemonic::STOP, AddressingMode::Implied),
        ];

        for &(op, mn, mode) in expectations {
            let info = decode(op);
            assert_eq!(info.mnemonic, mn, "opcode {:02X} mnemonic mismatch", op);
            assert_eq!(info.mode, mode, "opcode {:02X} mode mismatch", op);
        }
    }

    #[test]
    fn test_all_tcalls_use_tcall_vector_mode() {
        for n in 0..16u8 {
            let op = (n << 4) | 0x01;
            let info = decode(op);
            assert_eq!(info.mnemonic, Mnemonic::TCALL);
            assert_eq!(info.mode, AddressingMode::TCallVector);
            assert_eq!(info.base_cycles, 8);
        }
    }

    #[test]
    fn test_set1_clr1_bit_family_pattern() {
        for n in 0..8u8 {
            let set_op = (n << 5) | 0x02;
            let clr_op = (n << 5) | 0x12;

            let set_info = decode(set_op);
            assert_eq!(
                set_info.mnemonic,
                Mnemonic::SET1,
                "0x{:02X} should be SET1",
                set_op
            );
            assert_eq!(set_info.mode, AddressingMode::DirectPage);

            let clr_info = decode(clr_op);
            assert_eq!(
                clr_info.mnemonic,
                Mnemonic::CLR1,
                "0x{:02X} should be CLR1",
                clr_op
            );
            assert_eq!(clr_info.mode, AddressingMode::DirectPage);
        }
    }

    #[test]
    fn test_bbs_bbc_branch_bit_family_pattern() {
        for n in 0..8u8 {
            let bbs_op = (n << 5) | 0x03;
            let bbc_op = (n << 5) | 0x13;

            let bbs_info = decode(bbs_op);
            assert_eq!(
                bbs_info.mnemonic,
                Mnemonic::BBS,
                "0x{:02X} should be BBS",
                bbs_op
            );
            assert_eq!(bbs_info.mode, AddressingMode::DirectPageRelative);

            let bbc_info = decode(bbc_op);
            assert_eq!(
                bbc_info.mnemonic,
                Mnemonic::BBC,
                "0x{:02X} should be BBC",
                bbc_op
            );
            assert_eq!(bbc_info.mode, AddressingMode::DirectPageRelative);
        }
    }
}
