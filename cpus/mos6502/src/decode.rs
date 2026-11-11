//! 6502 opcode decode table.
//!
//! A 256-entry lookup table mapping each opcode byte to its instruction,
//! addressing mode, base cycle count, and page-crossing penalty flag.

use crate::addressing::AddressingMode;

/// Instruction mnemonic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mnemonic {
    ADC,
    AND,
    ASL,
    BCC,
    BCS,
    BEQ,
    BIT,
    BMI,
    BNE,
    BPL,
    BRK,
    BVC,
    BVS,
    CLC,
    CLD,
    CLI,
    CLV,
    CMP,
    CPX,
    CPY,
    DEC,
    DEX,
    DEY,
    EOR,
    INC,
    INX,
    INY,
    JMP,
    JSR,
    LDA,
    LDX,
    LDY,
    LSR,
    NOP,
    ORA,
    PHA,
    PHP,
    PLA,
    PLP,
    ROL,
    ROR,
    RTI,
    RTS,
    SBC,
    SEC,
    SED,
    SEI,
    STA,
    STX,
    STY,
    TAX,
    TAY,
    TSX,
    TXA,
    TXS,
    TYA,
    /// Illegal / undocumented opcode
    ILL,
}

/// Decoded opcode information.
#[derive(Debug, Clone, Copy)]
pub struct OpcodeInfo {
    pub mnemonic: Mnemonic,
    pub mode: AddressingMode,
    pub cycles: u8,
    /// Whether a page-crossing adds +1 cycle (read instructions only)
    pub page_penalty: bool,
}

impl OpcodeInfo {
    const fn new(mnemonic: Mnemonic, mode: AddressingMode, cycles: u8, page_penalty: bool) -> Self {
        Self {
            mnemonic,
            mode,
            cycles,
            page_penalty,
        }
    }
}

const fn op(m: Mnemonic, am: AddressingMode, c: u8, pp: bool) -> OpcodeInfo {
    OpcodeInfo::new(m, am, c, pp)
}

use AddressingMode::*;
use Mnemonic::*;

/// Full 256-entry opcode table for the MOS 6502.
/// Illegal/undocumented opcodes map to ILL with NOP-like timing.
pub static OPCODE_TABLE: [OpcodeInfo; 256] = [
    // 0x00-0x0F
    op(BRK, Implied, 7, false),     // 00
    op(ORA, IndirectX, 6, false),   // 01
    op(ILL, Implied, 2, false),     // 02
    op(ILL, Implied, 2, false),     // 03
    op(ILL, ZeroPage, 3, false),    // 04 *NOP zp
    op(ORA, ZeroPage, 3, false),    // 05
    op(ASL, ZeroPage, 5, false),    // 06
    op(ILL, Implied, 2, false),     // 07
    op(PHP, Implied, 3, false),     // 08
    op(ORA, Immediate, 2, false),   // 09
    op(ASL, Accumulator, 2, false), // 0A
    op(ILL, Implied, 2, false),     // 0B
    op(ILL, Absolute, 4, false),    // 0C *NOP abs
    op(ORA, Absolute, 4, false),    // 0D
    op(ASL, Absolute, 6, false),    // 0E
    op(ILL, Implied, 2, false),     // 0F
    // 0x10-0x1F
    op(BPL, Relative, 2, false),  // 10
    op(ORA, IndirectY, 5, true),  // 11
    op(ILL, Implied, 2, false),   // 12
    op(ILL, Implied, 2, false),   // 13
    op(ILL, ZeroPageX, 4, false), // 14 *NOP zp,x
    op(ORA, ZeroPageX, 4, false), // 15
    op(ASL, ZeroPageX, 6, false), // 16
    op(ILL, Implied, 2, false),   // 17
    op(CLC, Implied, 2, false),   // 18
    op(ORA, AbsoluteY, 4, true),  // 19
    op(ILL, Implied, 2, false),   // 1A *NOP
    op(ILL, Implied, 2, false),   // 1B
    op(ILL, AbsoluteX, 4, true),  // 1C *NOP abs,x
    op(ORA, AbsoluteX, 4, true),  // 1D
    op(ASL, AbsoluteX, 7, false), // 1E
    op(ILL, Implied, 2, false),   // 1F
    // 0x20-0x2F
    op(JSR, Absolute, 6, false),    // 20
    op(AND, IndirectX, 6, false),   // 21
    op(ILL, Implied, 2, false),     // 22
    op(ILL, Implied, 2, false),     // 23
    op(BIT, ZeroPage, 3, false),    // 24
    op(AND, ZeroPage, 3, false),    // 25
    op(ROL, ZeroPage, 5, false),    // 26
    op(ILL, Implied, 2, false),     // 27
    op(PLP, Implied, 4, false),     // 28
    op(AND, Immediate, 2, false),   // 29
    op(ROL, Accumulator, 2, false), // 2A
    op(ILL, Implied, 2, false),     // 2B
    op(BIT, Absolute, 4, false),    // 2C
    op(AND, Absolute, 4, false),    // 2D
    op(ROL, Absolute, 6, false),    // 2E
    op(ILL, Implied, 2, false),     // 2F
    // 0x30-0x3F
    op(BMI, Relative, 2, false),  // 30
    op(AND, IndirectY, 5, true),  // 31
    op(ILL, Implied, 2, false),   // 32
    op(ILL, Implied, 2, false),   // 33
    op(ILL, ZeroPageX, 4, false), // 34 *NOP zp,x
    op(AND, ZeroPageX, 4, false), // 35
    op(ROL, ZeroPageX, 6, false), // 36
    op(ILL, Implied, 2, false),   // 37
    op(SEC, Implied, 2, false),   // 38
    op(AND, AbsoluteY, 4, true),  // 39
    op(ILL, Implied, 2, false),   // 3A *NOP
    op(ILL, Implied, 2, false),   // 3B
    op(ILL, AbsoluteX, 4, true),  // 3C *NOP abs,x
    op(AND, AbsoluteX, 4, true),  // 3D
    op(ROL, AbsoluteX, 7, false), // 3E
    op(ILL, Implied, 2, false),   // 3F
    // 0x40-0x4F
    op(RTI, Implied, 6, false),     // 40
    op(EOR, IndirectX, 6, false),   // 41
    op(ILL, Implied, 2, false),     // 42
    op(ILL, Implied, 2, false),     // 43
    op(ILL, ZeroPage, 3, false),    // 44 *NOP zp
    op(EOR, ZeroPage, 3, false),    // 45
    op(LSR, ZeroPage, 5, false),    // 46
    op(ILL, Implied, 2, false),     // 47
    op(PHA, Implied, 3, false),     // 48
    op(EOR, Immediate, 2, false),   // 49
    op(LSR, Accumulator, 2, false), // 4A
    op(ILL, Implied, 2, false),     // 4B
    op(JMP, Absolute, 3, false),    // 4C
    op(EOR, Absolute, 4, false),    // 4D
    op(LSR, Absolute, 6, false),    // 4E
    op(ILL, Implied, 2, false),     // 4F
    // 0x50-0x5F
    op(BVC, Relative, 2, false),  // 50
    op(EOR, IndirectY, 5, true),  // 51
    op(ILL, Implied, 2, false),   // 52
    op(ILL, Implied, 2, false),   // 53
    op(ILL, ZeroPageX, 4, false), // 54 *NOP zp,x
    op(EOR, ZeroPageX, 4, false), // 55
    op(LSR, ZeroPageX, 6, false), // 56
    op(ILL, Implied, 2, false),   // 57
    op(CLI, Implied, 2, false),   // 58
    op(EOR, AbsoluteY, 4, true),  // 59
    op(ILL, Implied, 2, false),   // 5A *NOP
    op(ILL, Implied, 2, false),   // 5B
    op(ILL, AbsoluteX, 4, true),  // 5C *NOP abs,x
    op(EOR, AbsoluteX, 4, true),  // 5D
    op(LSR, AbsoluteX, 7, false), // 5E
    op(ILL, Implied, 2, false),   // 5F
    // 0x60-0x6F
    op(RTS, Implied, 6, false),     // 60
    op(ADC, IndirectX, 6, false),   // 61
    op(ILL, Implied, 2, false),     // 62
    op(ILL, Implied, 2, false),     // 63
    op(ILL, ZeroPage, 3, false),    // 64 *NOP zp
    op(ADC, ZeroPage, 3, false),    // 65
    op(ROR, ZeroPage, 5, false),    // 66
    op(ILL, Implied, 2, false),     // 67
    op(PLA, Implied, 4, false),     // 68
    op(ADC, Immediate, 2, false),   // 69
    op(ROR, Accumulator, 2, false), // 6A
    op(ILL, Implied, 2, false),     // 6B
    op(JMP, Indirect, 5, false),    // 6C
    op(ADC, Absolute, 4, false),    // 6D
    op(ROR, Absolute, 6, false),    // 6E
    op(ILL, Implied, 2, false),     // 6F
    // 0x70-0x7F
    op(BVS, Relative, 2, false),  // 70
    op(ADC, IndirectY, 5, true),  // 71
    op(ILL, Implied, 2, false),   // 72
    op(ILL, Implied, 2, false),   // 73
    op(ILL, ZeroPageX, 4, false), // 74 *NOP zp,x
    op(ADC, ZeroPageX, 4, false), // 75
    op(ROR, ZeroPageX, 6, false), // 76
    op(ILL, Implied, 2, false),   // 77
    op(SEI, Implied, 2, false),   // 78
    op(ADC, AbsoluteY, 4, true),  // 79
    op(ILL, Implied, 2, false),   // 7A *NOP
    op(ILL, Implied, 2, false),   // 7B
    op(ILL, AbsoluteX, 4, true),  // 7C *NOP abs,x
    op(ADC, AbsoluteX, 4, true),  // 7D
    op(ROR, AbsoluteX, 7, false), // 7E
    op(ILL, Implied, 2, false),   // 7F
    // 0x80-0x8F
    op(ILL, Immediate, 2, false), // 80 *NOP #imm
    op(STA, IndirectX, 6, false), // 81
    op(ILL, Immediate, 2, false), // 82 *NOP #imm
    op(ILL, Implied, 2, false),   // 83
    op(STY, ZeroPage, 3, false),  // 84
    op(STA, ZeroPage, 3, false),  // 85
    op(STX, ZeroPage, 3, false),  // 86
    op(ILL, Implied, 2, false),   // 87
    op(DEY, Implied, 2, false),   // 88
    op(ILL, Immediate, 2, false), // 89 *NOP #imm
    op(TXA, Implied, 2, false),   // 8A
    op(ILL, Implied, 2, false),   // 8B
    op(STY, Absolute, 4, false),  // 8C
    op(STA, Absolute, 4, false),  // 8D
    op(STX, Absolute, 4, false),  // 8E
    op(ILL, Implied, 2, false),   // 8F
    // 0x90-0x9F
    op(BCC, Relative, 2, false),  // 90
    op(STA, IndirectY, 6, false), // 91
    op(ILL, Implied, 2, false),   // 92
    op(ILL, Implied, 2, false),   // 93
    op(STY, ZeroPageX, 4, false), // 94
    op(STA, ZeroPageX, 4, false), // 95
    op(STX, ZeroPageY, 4, false), // 96
    op(ILL, Implied, 2, false),   // 97
    op(TYA, Implied, 2, false),   // 98
    op(STA, AbsoluteY, 5, false), // 99
    op(TXS, Implied, 2, false),   // 9A
    op(ILL, Implied, 2, false),   // 9B
    op(ILL, Implied, 2, false),   // 9C
    op(STA, AbsoluteX, 5, false), // 9D
    op(ILL, Implied, 2, false),   // 9E
    op(ILL, Implied, 2, false),   // 9F
    // 0xA0-0xAF
    op(LDY, Immediate, 2, false), // A0
    op(LDA, IndirectX, 6, false), // A1
    op(LDX, Immediate, 2, false), // A2
    op(ILL, Implied, 2, false),   // A3
    op(LDY, ZeroPage, 3, false),  // A4
    op(LDA, ZeroPage, 3, false),  // A5
    op(LDX, ZeroPage, 3, false),  // A6
    op(ILL, Implied, 2, false),   // A7
    op(TAY, Implied, 2, false),   // A8
    op(LDA, Immediate, 2, false), // A9
    op(TAX, Implied, 2, false),   // AA
    op(ILL, Implied, 2, false),   // AB
    op(LDY, Absolute, 4, false),  // AC
    op(LDA, Absolute, 4, false),  // AD
    op(LDX, Absolute, 4, false),  // AE
    op(ILL, Implied, 2, false),   // AF
    // 0xB0-0xBF
    op(BCS, Relative, 2, false),  // B0
    op(LDA, IndirectY, 5, true),  // B1
    op(ILL, Implied, 2, false),   // B2
    op(ILL, Implied, 2, false),   // B3
    op(LDY, ZeroPageX, 4, false), // B4
    op(LDA, ZeroPageX, 4, false), // B5
    op(LDX, ZeroPageY, 4, false), // B6
    op(ILL, Implied, 2, false),   // B7
    op(CLV, Implied, 2, false),   // B8
    op(LDA, AbsoluteY, 4, true),  // B9
    op(TSX, Implied, 2, false),   // BA
    op(ILL, Implied, 2, false),   // BB
    op(LDY, AbsoluteX, 4, true),  // BC
    op(LDA, AbsoluteX, 4, true),  // BD
    op(LDX, AbsoluteY, 4, true),  // BE
    op(ILL, Implied, 2, false),   // BF
    // 0xC0-0xCF
    op(CPY, Immediate, 2, false), // C0
    op(CMP, IndirectX, 6, false), // C1
    op(ILL, Immediate, 2, false), // C2 *NOP #imm
    op(ILL, Implied, 2, false),   // C3
    op(CPY, ZeroPage, 3, false),  // C4
    op(CMP, ZeroPage, 3, false),  // C5
    op(DEC, ZeroPage, 5, false),  // C6
    op(ILL, Implied, 2, false),   // C7
    op(INY, Implied, 2, false),   // C8
    op(CMP, Immediate, 2, false), // C9
    op(DEX, Implied, 2, false),   // CA
    op(ILL, Implied, 2, false),   // CB
    op(CPY, Absolute, 4, false),  // CC
    op(CMP, Absolute, 4, false),  // CD
    op(DEC, Absolute, 6, false),  // CE
    op(ILL, Implied, 2, false),   // CF
    // 0xD0-0xDF
    op(BNE, Relative, 2, false),  // D0
    op(CMP, IndirectY, 5, true),  // D1
    op(ILL, Implied, 2, false),   // D2
    op(ILL, Implied, 2, false),   // D3
    op(ILL, ZeroPageX, 4, false), // D4 *NOP zp,x
    op(CMP, ZeroPageX, 4, false), // D5
    op(DEC, ZeroPageX, 6, false), // D6
    op(ILL, Implied, 2, false),   // D7
    op(CLD, Implied, 2, false),   // D8
    op(CMP, AbsoluteY, 4, true),  // D9
    op(ILL, Implied, 2, false),   // DA *NOP
    op(ILL, Implied, 2, false),   // DB
    op(ILL, AbsoluteX, 4, true),  // DC *NOP abs,x
    op(CMP, AbsoluteX, 4, true),  // DD
    op(DEC, AbsoluteX, 7, false), // DE
    op(ILL, Implied, 2, false),   // DF
    // 0xE0-0xEF
    op(CPX, Immediate, 2, false), // E0
    op(SBC, IndirectX, 6, false), // E1
    op(ILL, Immediate, 2, false), // E2 *NOP #imm
    op(ILL, Implied, 2, false),   // E3
    op(CPX, ZeroPage, 3, false),  // E4
    op(SBC, ZeroPage, 3, false),  // E5
    op(INC, ZeroPage, 5, false),  // E6
    op(ILL, Implied, 2, false),   // E7
    op(INX, Implied, 2, false),   // E8
    op(SBC, Immediate, 2, false), // E9
    op(NOP, Implied, 2, false),   // EA
    op(ILL, Immediate, 2, false), // EB *SBC #imm (unofficial mirror)
    op(CPX, Absolute, 4, false),  // EC
    op(SBC, Absolute, 4, false),  // ED
    op(INC, Absolute, 6, false),  // EE
    op(ILL, Implied, 2, false),   // EF
    // 0xF0-0xFF
    op(BEQ, Relative, 2, false),  // F0
    op(SBC, IndirectY, 5, true),  // F1
    op(ILL, Implied, 2, false),   // F2
    op(ILL, Implied, 2, false),   // F3
    op(ILL, ZeroPageX, 4, false), // F4 *NOP zp,x
    op(SBC, ZeroPageX, 4, false), // F5
    op(INC, ZeroPageX, 6, false), // F6
    op(ILL, Implied, 2, false),   // F7
    op(SED, Implied, 2, false),   // F8
    op(SBC, AbsoluteY, 4, true),  // F9
    op(ILL, Implied, 2, false),   // FA *NOP
    op(ILL, Implied, 2, false),   // FB
    op(ILL, AbsoluteX, 4, true),  // FC *NOP abs,x
    op(SBC, AbsoluteX, 4, true),  // FD
    op(INC, AbsoluteX, 7, false), // FE
    op(ILL, Implied, 2, false),   // FF
];
