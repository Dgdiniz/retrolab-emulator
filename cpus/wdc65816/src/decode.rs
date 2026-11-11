//! 65C816 opcode decode table.
//!
//! All 256 opcodes are defined (no illegal opcodes). Cycle counts are
//! base values; conditional penalties are tracked via flag fields.

use crate::addressing::AddressingMode;

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
    BRA,
    BRK,
    BRL,
    BVC,
    BVS,
    CLC,
    CLD,
    CLI,
    CLV,
    CMP,
    COP,
    CPX,
    CPY,
    DEC,
    DEX,
    DEY,
    EOR,
    INC,
    INX,
    INY,
    JML,
    JMP,
    JSL,
    JSR,
    LDA,
    LDX,
    LDY,
    LSR,
    MVN,
    MVP,
    NOP,
    ORA,
    PEA,
    PEI,
    PER,
    PHA,
    PHB,
    PHD,
    PHK,
    PHP,
    PHX,
    PHY,
    PLA,
    PLB,
    PLD,
    PLP,
    PLX,
    PLY,
    REP,
    ROL,
    ROR,
    RTI,
    RTL,
    RTS,
    SBC,
    SEC,
    SED,
    SEI,
    SEP,
    STA,
    STP,
    STX,
    STY,
    STZ,
    TAX,
    TAY,
    TCD,
    TCS,
    TDC,
    TRB,
    TSB,
    TSC,
    TSX,
    TXA,
    TXS,
    TXY,
    TYA,
    TYX,
    WAI,
    WDM,
    XBA,
    XCE,
    ILL,
}

impl Mnemonic {
    /// Lowercase canonical name of the mnemonic, as the trace format
    /// spells it (the format bsnes-plus's debugger writes).
    /// Illegal opcodes return `"???"` so disassembly of unknown bytes is
    /// still printable.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ADC => "adc",
            Self::AND => "and",
            Self::ASL => "asl",
            Self::BCC => "bcc",
            Self::BCS => "bcs",
            Self::BEQ => "beq",
            Self::BIT => "bit",
            Self::BMI => "bmi",
            Self::BNE => "bne",
            Self::BPL => "bpl",
            Self::BRA => "bra",
            Self::BRK => "brk",
            Self::BRL => "brl",
            Self::BVC => "bvc",
            Self::BVS => "bvs",
            Self::CLC => "clc",
            Self::CLD => "cld",
            Self::CLI => "cli",
            Self::CLV => "clv",
            Self::CMP => "cmp",
            Self::COP => "cop",
            Self::CPX => "cpx",
            Self::CPY => "cpy",
            Self::DEC => "dec",
            Self::DEX => "dex",
            Self::DEY => "dey",
            Self::EOR => "eor",
            Self::INC => "inc",
            Self::INX => "inx",
            Self::INY => "iny",
            Self::JML => "jml",
            Self::JMP => "jmp",
            Self::JSL => "jsl",
            Self::JSR => "jsr",
            Self::LDA => "lda",
            Self::LDX => "ldx",
            Self::LDY => "ldy",
            Self::LSR => "lsr",
            Self::MVN => "mvn",
            Self::MVP => "mvp",
            Self::NOP => "nop",
            Self::ORA => "ora",
            Self::PEA => "pea",
            Self::PEI => "pei",
            Self::PER => "per",
            Self::PHA => "pha",
            Self::PHB => "phb",
            Self::PHD => "phd",
            Self::PHK => "phk",
            Self::PHP => "php",
            Self::PHX => "phx",
            Self::PHY => "phy",
            Self::PLA => "pla",
            Self::PLB => "plb",
            Self::PLD => "pld",
            Self::PLP => "plp",
            Self::PLX => "plx",
            Self::PLY => "ply",
            Self::REP => "rep",
            Self::ROL => "rol",
            Self::ROR => "ror",
            Self::RTI => "rti",
            Self::RTL => "rtl",
            Self::RTS => "rts",
            Self::SBC => "sbc",
            Self::SEC => "sec",
            Self::SED => "sed",
            Self::SEI => "sei",
            Self::SEP => "sep",
            Self::STA => "sta",
            Self::STP => "stp",
            Self::STX => "stx",
            Self::STY => "sty",
            Self::STZ => "stz",
            Self::TAX => "tax",
            Self::TAY => "tay",
            Self::TCD => "tcd",
            Self::TCS => "tcs",
            Self::TDC => "tdc",
            Self::TRB => "trb",
            Self::TSB => "tsb",
            Self::TSC => "tsc",
            Self::TSX => "tsx",
            Self::TXA => "txa",
            Self::TXS => "txs",
            Self::TXY => "txy",
            Self::TYA => "tya",
            Self::TYX => "tyx",
            Self::WAI => "wai",
            Self::WDM => "wdm",
            Self::XBA => "xba",
            Self::XCE => "xce",
            Self::ILL => "???",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OpcodeInfo {
    pub mnemonic: Mnemonic,
    pub mode: AddressingMode,
    pub base_cycles: u8,
    pub penalty_m16: bool,
    pub penalty_x16: bool,
    pub penalty_dp: bool,
    pub penalty_page: bool,
    /// Extra cycle(s) added when running in native mode (E=0). Used by
    /// the software interrupt family (BRK/COP) and RTI, which require
    /// one additional clock to push/pull the program-bank register.
    pub penalty_native: u8,
    /// Extra `+1` cycle added when the accumulator is 16-bit (M=0) AND
    /// the instruction performs a read-modify-write on memory. Sits on
    /// top of `penalty_m16` so a 16-bit RMW costs `+2` total.
    pub penalty_rmw_m16: bool,
}

#[allow(clippy::too_many_arguments)]
const fn op(
    m: Mnemonic,
    am: AddressingMode,
    c: u8,
    m16: bool,
    x16: bool,
    dp: bool,
    pg: bool,
    pn: u8,
    rmw: bool,
) -> OpcodeInfo {
    OpcodeInfo {
        mnemonic: m,
        mode: am,
        base_cycles: c,
        penalty_m16: m16,
        penalty_x16: x16,
        penalty_dp: dp,
        penalty_page: pg,
        penalty_native: pn,
        penalty_rmw_m16: rmw,
    }
}

use AddressingMode::*;
use Mnemonic::*;

// Shorthand for common patterns
const fn s(m: Mnemonic, am: AddressingMode, c: u8) -> OpcodeInfo {
    op(m, am, c, false, false, false, false, 0, false)
}
const fn sm(m: Mnemonic, am: AddressingMode, c: u8) -> OpcodeInfo {
    op(m, am, c, true, false, false, false, 0, false)
}
const fn smd(m: Mnemonic, am: AddressingMode, c: u8) -> OpcodeInfo {
    op(m, am, c, true, false, true, false, 0, false)
}
const fn smdp(m: Mnemonic, am: AddressingMode, c: u8) -> OpcodeInfo {
    op(m, am, c, true, false, true, true, 0, false)
}
const fn smp(m: Mnemonic, am: AddressingMode, c: u8) -> OpcodeInfo {
    op(m, am, c, true, false, false, true, 0, false)
}
const fn sx(m: Mnemonic, am: AddressingMode, c: u8) -> OpcodeInfo {
    op(m, am, c, false, true, false, false, 0, false)
}
const fn sxd(m: Mnemonic, am: AddressingMode, c: u8) -> OpcodeInfo {
    op(m, am, c, false, true, true, false, 0, false)
}
const fn sxp(m: Mnemonic, am: AddressingMode, c: u8) -> OpcodeInfo {
    op(m, am, c, false, true, false, true, 0, false)
}
const fn sd(m: Mnemonic, am: AddressingMode, c: u8) -> OpcodeInfo {
    op(m, am, c, false, false, true, false, 0, false)
}

/// `+1` native cycle (BRK / COP / RTI). No other penalties.
const fn sn(m: Mnemonic, am: AddressingMode, c: u8) -> OpcodeInfo {
    op(m, am, c, false, false, false, false, 1, false)
}

/// RMW on absolute memory: `+1` if M=0 (m16) and another `+1` for the
/// extra read/write cycle (rmw_m16) = `+2` total in 16-bit.
const fn srm(m: Mnemonic, am: AddressingMode, c: u8) -> OpcodeInfo {
    op(m, am, c, true, false, false, false, 0, true)
}

/// RMW on direct-page memory: same as `srm` plus the `+1` DP penalty
/// when `D & 0xFF != 0`.
const fn srmd(m: Mnemonic, am: AddressingMode, c: u8) -> OpcodeInfo {
    op(m, am, c, true, false, true, false, 0, true)
}

pub static OPCODE_TABLE: [OpcodeInfo; 256] = [
    // 0x00
    sn(BRK, Implied, 7),             // 00  +1 native
    smd(ORA, DirectXIndirect, 6),    // 01
    sn(COP, Implied, 7),             // 02  +1 native
    sm(ORA, StackRelative, 4),       // 03
    srmd(TSB, Direct, 5),            // 04
    smd(ORA, Direct, 3),             // 05
    srmd(ASL, Direct, 5),            // 06
    smd(ORA, DirectIndirectLong, 6), // 07
    s(PHP, Implied, 3),              // 08
    sm(ORA, Immediate, 2),           // 09
    s(ASL, Accumulator, 2),          // 0A
    s(PHD, Implied, 4),              // 0B
    srm(TSB, Absolute, 6),           // 0C
    sm(ORA, Absolute, 4),            // 0D
    srm(ASL, Absolute, 6),           // 0E
    sm(ORA, AbsoluteLong, 5),        // 0F
    // 0x10
    s(BPL, Relative, 2),                // 10
    smdp(ORA, DirectIndirectY, 5),      // 11
    smd(ORA, DirectIndirect, 5),        // 12
    sm(ORA, StackRelativeIndirectY, 7), // 13
    srmd(TRB, Direct, 5),               // 14
    smd(ORA, DirectX, 4),               // 15
    srmd(ASL, DirectX, 6),              // 16
    smd(ORA, DirectIndirectLongY, 6),   // 17
    s(CLC, Implied, 2),                 // 18
    smp(ORA, AbsoluteY, 4),             // 19
    s(INC, Accumulator, 2),             // 1A
    s(TCS, Implied, 2),                 // 1B
    srm(TRB, Absolute, 6),              // 1C
    smp(ORA, AbsoluteX, 4),             // 1D
    srm(ASL, AbsoluteX, 7),             // 1E
    sm(ORA, AbsoluteLongX, 5),          // 1F
    // 0x20
    s(JSR, Absolute, 6),             // 20
    smd(AND, DirectXIndirect, 6),    // 21
    s(JSL, AbsoluteLong, 8),         // 22
    sm(AND, StackRelative, 4),       // 23
    smd(BIT, Direct, 3),             // 24
    smd(AND, Direct, 3),             // 25
    srmd(ROL, Direct, 5),            // 26
    smd(AND, DirectIndirectLong, 6), // 27
    s(PLP, Implied, 4),              // 28
    sm(AND, Immediate, 2),           // 29
    s(ROL, Accumulator, 2),          // 2A
    s(PLD, Implied, 5),              // 2B
    sm(BIT, Absolute, 4),            // 2C
    sm(AND, Absolute, 4),            // 2D
    srm(ROL, Absolute, 6),           // 2E
    sm(AND, AbsoluteLong, 5),        // 2F
    // 0x30
    s(BMI, Relative, 2),                // 30
    smdp(AND, DirectIndirectY, 5),      // 31
    smd(AND, DirectIndirect, 5),        // 32
    sm(AND, StackRelativeIndirectY, 7), // 33
    smd(BIT, DirectX, 4),               // 34
    smd(AND, DirectX, 4),               // 35
    srmd(ROL, DirectX, 6),              // 36
    smd(AND, DirectIndirectLongY, 6),   // 37
    s(SEC, Implied, 2),                 // 38
    smp(AND, AbsoluteY, 4),             // 39
    s(DEC, Accumulator, 2),             // 3A
    s(TSC, Implied, 2),                 // 3B
    smp(BIT, AbsoluteX, 4),             // 3C
    smp(AND, AbsoluteX, 4),             // 3D
    srm(ROL, AbsoluteX, 7),             // 3E
    sm(AND, AbsoluteLongX, 5),          // 3F
    // 0x40
    sn(RTI, Implied, 6),             // 40  +1 native
    smd(EOR, DirectXIndirect, 6),    // 41
    s(WDM, Implied, 2),              // 42
    sm(EOR, StackRelative, 4),       // 43
    s(MVP, BlockMove, 7),            // 44
    smd(EOR, Direct, 3),             // 45
    srmd(LSR, Direct, 5),            // 46
    smd(EOR, DirectIndirectLong, 6), // 47
    sm(PHA, Implied, 3),             // 48  +1 if M=0
    sm(EOR, Immediate, 2),           // 49
    s(LSR, Accumulator, 2),          // 4A
    s(PHK, Implied, 3),              // 4B
    s(JMP, Absolute, 3),             // 4C
    sm(EOR, Absolute, 4),            // 4D
    srm(LSR, Absolute, 6),           // 4E
    sm(EOR, AbsoluteLong, 5),        // 4F
    // 0x50
    s(BVC, Relative, 2),                // 50
    smdp(EOR, DirectIndirectY, 5),      // 51
    smd(EOR, DirectIndirect, 5),        // 52
    sm(EOR, StackRelativeIndirectY, 7), // 53
    s(MVN, BlockMove, 7),               // 54
    smd(EOR, DirectX, 4),               // 55
    srmd(LSR, DirectX, 6),              // 56
    smd(EOR, DirectIndirectLongY, 6),   // 57
    s(CLI, Implied, 2),                 // 58
    smp(EOR, AbsoluteY, 4),             // 59
    sx(PHY, Implied, 3),                // 5A
    s(TCD, Implied, 2),                 // 5B
    s(JML, AbsoluteLong, 4),            // 5C
    smp(EOR, AbsoluteX, 4),             // 5D
    srm(LSR, AbsoluteX, 7),             // 5E
    sm(EOR, AbsoluteLongX, 5),          // 5F
    // 0x60
    s(RTS, Implied, 6),              // 60
    smd(ADC, DirectXIndirect, 6),    // 61
    s(PER, RelativeLong, 6),         // 62
    sm(ADC, StackRelative, 4),       // 63
    smd(STZ, Direct, 3),             // 64
    smd(ADC, Direct, 3),             // 65
    srmd(ROR, Direct, 5),            // 66
    smd(ADC, DirectIndirectLong, 6), // 67
    sm(PLA, Implied, 4),             // 68  +1 if M=0
    sm(ADC, Immediate, 2),           // 69
    s(ROR, Accumulator, 2),          // 6A
    s(RTL, Implied, 6),              // 6B
    s(JMP, AbsoluteIndirect, 5),     // 6C
    sm(ADC, Absolute, 4),            // 6D
    srm(ROR, Absolute, 6),           // 6E
    sm(ADC, AbsoluteLong, 5),        // 6F
    // 0x70
    s(BVS, Relative, 2),                // 70
    smdp(ADC, DirectIndirectY, 5),      // 71
    smd(ADC, DirectIndirect, 5),        // 72
    sm(ADC, StackRelativeIndirectY, 7), // 73
    smd(STZ, DirectX, 4),               // 74
    smd(ADC, DirectX, 4),               // 75
    srmd(ROR, DirectX, 6),              // 76
    smd(ADC, DirectIndirectLongY, 6),   // 77
    s(SEI, Implied, 2),                 // 78
    smp(ADC, AbsoluteY, 4),             // 79
    sx(PLY, Implied, 4),                // 7A
    s(TDC, Implied, 2),                 // 7B
    s(JMP, AbsoluteXIndirect, 6),       // 7C
    smp(ADC, AbsoluteX, 4),             // 7D
    srm(ROR, AbsoluteX, 7),             // 7E
    sm(ADC, AbsoluteLongX, 5),          // 7F
    // 0x80
    s(BRA, Relative, 3),             // 80
    smd(STA, DirectXIndirect, 6),    // 81
    s(BRL, RelativeLong, 4),         // 82
    sm(STA, StackRelative, 4),       // 83
    sxd(STY, Direct, 3),             // 84
    smd(STA, Direct, 3),             // 85
    sxd(STX, Direct, 3),             // 86
    smd(STA, DirectIndirectLong, 6), // 87
    s(DEY, Implied, 2),              // 88
    sm(BIT, Immediate, 2),           // 89
    s(TXA, Implied, 2),              // 8A
    s(PHB, Implied, 3),              // 8B
    sx(STY, Absolute, 4),            // 8C
    sm(STA, Absolute, 4),            // 8D
    sx(STX, Absolute, 4),            // 8E
    sm(STA, AbsoluteLong, 5),        // 8F
    // 0x90
    s(BCC, Relative, 2),                // 90
    smd(STA, DirectIndirectY, 6),       // 91
    smd(STA, DirectIndirect, 5),        // 92
    sm(STA, StackRelativeIndirectY, 7), // 93
    sxd(STY, DirectX, 4),               // 94
    smd(STA, DirectX, 4),               // 95
    sxd(STX, DirectY, 4),               // 96
    smd(STA, DirectIndirectLongY, 6),   // 97
    s(TYA, Implied, 2),                 // 98
    sm(STA, AbsoluteY, 5),              // 99
    s(TXS, Implied, 2),                 // 9A
    s(TXY, Implied, 2),                 // 9B
    sm(STZ, Absolute, 4),               // 9C
    sm(STA, AbsoluteX, 5),              // 9D
    sm(STZ, AbsoluteX, 5),              // 9E
    sm(STA, AbsoluteLongX, 5),          // 9F
    // 0xA0
    sx(LDY, ImmediateX, 2),          // A0
    smd(LDA, DirectXIndirect, 6),    // A1
    sx(LDX, ImmediateX, 2),          // A2
    sm(LDA, StackRelative, 4),       // A3
    sxd(LDY, Direct, 3),             // A4
    smd(LDA, Direct, 3),             // A5
    sxd(LDX, Direct, 3),             // A6
    smd(LDA, DirectIndirectLong, 6), // A7
    s(TAY, Implied, 2),              // A8
    sm(LDA, Immediate, 2),           // A9
    s(TAX, Implied, 2),              // AA
    s(PLB, Implied, 4),              // AB
    sx(LDY, Absolute, 4),            // AC
    sm(LDA, Absolute, 4),            // AD
    sx(LDX, Absolute, 4),            // AE
    sm(LDA, AbsoluteLong, 5),        // AF
    // 0xB0
    s(BCS, Relative, 2),                // B0
    smdp(LDA, DirectIndirectY, 5),      // B1
    smd(LDA, DirectIndirect, 5),        // B2
    sm(LDA, StackRelativeIndirectY, 7), // B3
    sxd(LDY, DirectX, 4),               // B4
    smd(LDA, DirectX, 4),               // B5
    sxd(LDX, DirectY, 4),               // B6
    smd(LDA, DirectIndirectLongY, 6),   // B7
    s(CLV, Implied, 2),                 // B8
    smp(LDA, AbsoluteY, 4),             // B9
    s(TSX, Implied, 2),                 // BA
    s(TYX, Implied, 2),                 // BB
    sxp(LDY, AbsoluteX, 4),             // BC
    smp(LDA, AbsoluteX, 4),             // BD
    sxp(LDX, AbsoluteY, 4),             // BE
    sm(LDA, AbsoluteLongX, 5),          // BF
    // 0xC0
    sx(CPY, ImmediateX, 2),          // C0
    smd(CMP, DirectXIndirect, 6),    // C1
    s(REP, ImmediateByte, 3),        // C2
    sm(CMP, StackRelative, 4),       // C3
    sxd(CPY, Direct, 3),             // C4
    smd(CMP, Direct, 3),             // C5
    srmd(DEC, Direct, 5),            // C6
    smd(CMP, DirectIndirectLong, 6), // C7
    s(INY, Implied, 2),              // C8
    sm(CMP, Immediate, 2),           // C9
    s(DEX, Implied, 2),              // CA
    s(WAI, Implied, 4),              // CB
    sx(CPY, Absolute, 4),            // CC
    sm(CMP, Absolute, 4),            // CD
    srm(DEC, Absolute, 6),           // CE
    sm(CMP, AbsoluteLong, 5),        // CF
    // 0xD0
    s(BNE, Relative, 2),                // D0
    smdp(CMP, DirectIndirectY, 5),      // D1
    smd(CMP, DirectIndirect, 5),        // D2
    sm(CMP, StackRelativeIndirectY, 7), // D3
    sd(PEI, DirectIndirect, 6),         // D4
    smd(CMP, DirectX, 4),               // D5
    srmd(DEC, DirectX, 6),              // D6
    smd(CMP, DirectIndirectLongY, 6),   // D7
    s(CLD, Implied, 2),                 // D8
    smp(CMP, AbsoluteY, 4),             // D9
    sx(PHX, Implied, 3),                // DA
    s(STP, Implied, 4),                 // DB
    s(JML, AbsoluteIndirectLong, 6),    // DC
    smp(CMP, AbsoluteX, 4),             // DD
    srm(DEC, AbsoluteX, 7),             // DE
    sm(CMP, AbsoluteLongX, 5),          // DF
    // 0xE0
    sx(CPX, ImmediateX, 2),          // E0
    smd(SBC, DirectXIndirect, 6),    // E1
    s(SEP, ImmediateByte, 3),        // E2
    sm(SBC, StackRelative, 4),       // E3
    sxd(CPX, Direct, 3),             // E4
    smd(SBC, Direct, 3),             // E5
    srmd(INC, Direct, 5),            // E6
    smd(SBC, DirectIndirectLong, 6), // E7
    s(INX, Implied, 2),              // E8
    sm(SBC, Immediate, 2),           // E9
    s(NOP, Implied, 2),              // EA
    s(XBA, Implied, 3),              // EB
    sx(CPX, Absolute, 4),            // EC
    sm(SBC, Absolute, 4),            // ED
    srm(INC, Absolute, 6),           // EE
    sm(SBC, AbsoluteLong, 5),        // EF
    // 0xF0
    s(BEQ, Relative, 2),                // F0
    smdp(SBC, DirectIndirectY, 5),      // F1
    smd(SBC, DirectIndirect, 5),        // F2
    sm(SBC, StackRelativeIndirectY, 7), // F3
    s(PEA, Absolute, 5),                // F4
    smd(SBC, DirectX, 4),               // F5
    srmd(INC, DirectX, 6),              // F6
    smd(SBC, DirectIndirectLongY, 6),   // F7
    s(SED, Implied, 2),                 // F8
    smp(SBC, AbsoluteY, 4),             // F9
    sx(PLX, Implied, 4),                // FA
    s(XCE, Implied, 2),                 // FB
    s(JSR, AbsoluteXIndirect, 8),       // FC
    smp(SBC, AbsoluteX, 4),             // FD
    srm(INC, AbsoluteX, 7),             // FE
    sm(SBC, AbsoluteLongX, 5),          // FF
];
