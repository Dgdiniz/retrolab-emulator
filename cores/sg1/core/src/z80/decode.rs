//! Z80 instruction decoding utilities

/// Extract bits from opcode (helper functions)
/// Get bits 7-6 (register pair for some instructions)
#[inline]
pub fn bits_7_6(opcode: u8) -> u8 {
    (opcode >> 6) & 0x03
}

/// Get bits 5-3 (destination register in many instructions)
#[inline]
pub fn bits_5_3(opcode: u8) -> u8 {
    (opcode >> 3) & 0x07
}

/// Get bits 2-0 (source register in many instructions)
#[inline]
pub fn bits_2_0(opcode: u8) -> u8 {
    opcode & 0x07
}

/// Get bit 4 (used in some instructions)
#[inline]
pub fn bit_4(opcode: u8) -> bool {
    (opcode & 0x10) != 0
}

/// Get bit 3 (used in some instructions)
#[inline]
pub fn bit_3(opcode: u8) -> bool {
    (opcode & 0x08) != 0
}

/// Decode register from 3-bit encoding
/// Used in many instructions for r and r' fields
pub fn decode_reg8(code: u8) -> crate::z80::addressing::Reg8 {
    use crate::z80::addressing::Reg8;
    match code & 0x07 {
        0 => Reg8::B,
        1 => Reg8::C,
        2 => Reg8::D,
        3 => Reg8::E,
        4 => Reg8::H,
        5 => Reg8::L,
        6 => panic!("(HL) addressing not a register"),
        7 => Reg8::A,
        _ => unreachable!(),
    }
}

/// Decode 16-bit register pair from 2-bit encoding
/// 00=BC, 01=DE, 10=HL, 11=SP (or AF for PUSH/POP)
pub fn decode_reg16(code: u8, use_af: bool) -> crate::z80::addressing::Reg16 {
    use crate::z80::addressing::Reg16;
    match code & 0x03 {
        0 => Reg16::BC,
        1 => Reg16::DE,
        2 => Reg16::HL,
        3 => {
            if use_af {
                Reg16::AF
            } else {
                Reg16::SP
            }
        }
        _ => unreachable!(),
    }
}
