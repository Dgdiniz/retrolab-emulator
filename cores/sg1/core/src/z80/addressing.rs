//! Z80 Addressing modes and register definitions

/// 8-bit registers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reg8 {
    A,
    B,
    C,
    D,
    E,
    H,
    L,
    // Index register high/low bytes (for DD/FD prefixes)
    IXH,
    IXL,
    IYH,
    IYL,
}

impl Reg8 {
    /// Decode 3-bit register code (used in Z80 opcodes)
    /// Code 6 is (HL) indirect and not handled here
    pub fn from_code(code: u8) -> Self {
        match code & 0x07 {
            0 => Reg8::B,
            1 => Reg8::C,
            2 => Reg8::D,
            3 => Reg8::E,
            4 => Reg8::H,
            5 => Reg8::L,
            7 => Reg8::A,
            _ => panic!(
                "Invalid register code: {} (6 is (HL), not a register)",
                code
            ),
        }
    }
}

/// 16-bit register pairs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reg16 {
    BC,
    DE,
    HL,
    SP,
    AF,
    IX,
    IY,
}

/// Z80 addressing modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressingMode {
    /// Implied (no operands)
    Implied,

    /// 8-bit register direct
    Register8(Reg8),

    /// 16-bit register direct
    Register16(Reg16),

    /// 8-bit immediate
    Immediate8,

    /// 16-bit immediate
    Immediate16,

    /// Indirect via register pair: (HL), (BC), (DE)
    Indirect(Reg16),

    /// Indexed with displacement: (IX+d), (IY+d)
    IndexedIX(i8),
    IndexedIY(i8),

    /// Extended/Absolute: (nn)
    Extended,

    /// Relative (for JR, DJNZ)
    Relative,
}
