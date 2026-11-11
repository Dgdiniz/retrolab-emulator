//! What an instruction's low byte names, and the call stack.
//!
//! An operand that is not an immediate is a 7-bit index into the
//! HG51B169's register file, which holds more than the sixteen general
//! registers: the accumulator, the two halves of the product, the three data
//! latches, the cart-bus address, the RAM base, P, sixteen constants, and two
//! indices (`$2E`, `$2F`) whose access starts a transfer on the chip's cart
//! bus. [`decode_register`] says what an index names; reading and writing it
//! is the core's work ([`super::hg51b::Hg51b::read_register`]), since an
//! access has side effects.

/// The sixteen constants at register-file indices `$50-$5F`, masks and
/// fixed-point values the chip's arithmetic uses (hardware data).
pub const CONSTANT_TABLE: [u32; 16] = [
    0x00_0000, // $50
    0xFF_FFFF, // $51
    0x00_FF00, // $52
    0xFF_0000, // $53
    0x00_FFFF, // $54
    0xFF_FF00, // $55
    0x80_0000, // $56
    0x7F_FFFF, // $57
    0x00_8000, // $58
    0x00_7FFF, // $59
    0xFF_7FFF, // $5A
    0xFF_FF7F, // $5B
    0x01_0000, // $5C
    0xFE_FFFF, // $5D
    0x00_0100, // $5E
    0x00_FEFF, // $5F
];

/// What a register-file index names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterRef {
    /// `$00`: A, the 24-bit accumulator.
    Accumulator,
    /// `$01`: the high 24 bits of the last product (ACCH).
    ProductHigh,
    /// `$02`: the low 24 bits of the last product (ACCL).
    ProductLow,
    /// `$03`: the cart-bus data latch (BUSDATA).
    BusData,
    /// `$08`: the data ROM latch (ROMDATA).
    RomData,
    /// `$0C`: the data RAM latch (RAMDATA).
    RamData,
    /// `$13`: the cart-bus address (BUSADDR).
    BusAddress,
    /// `$1C`: the base RDRAM/WRRAM add to an immediate (RAMADDR).
    RamBase,
    /// `$20`: the program counter. Read, it gives P, as `$28` does (the
    /// reading bsnes-plus has, which this core keeps).
    ProgramCounter,
    /// `$28`: P, the page register.
    Page,
    /// `$2E`/`$2F`: start a cart-bus transfer, waiting the ROM's (`$2E`) or
    /// the RAM's (`$2F`) wait states.
    BusTransfer,
    /// `$50-$5F`: a constant of [`CONSTANT_TABLE`].
    Constant(u32),
    /// `$60-$6F`, and the same registers again at `$70-$7F`: R0-R15.
    General(u8),
    /// An index that names nothing (it reads as 0).
    Unmapped,
}

/// What register-file `index` names (the top bit is not part of an index).
pub fn decode_register(index: u8) -> RegisterRef {
    let index = index & 0x7F;
    match index {
        0x00 => RegisterRef::Accumulator,
        0x01 => RegisterRef::ProductHigh,
        0x02 => RegisterRef::ProductLow,
        0x03 => RegisterRef::BusData,
        0x08 => RegisterRef::RomData,
        0x0C => RegisterRef::RamData,
        0x13 => RegisterRef::BusAddress,
        0x1C => RegisterRef::RamBase,
        0x20 => RegisterRef::ProgramCounter,
        0x28 => RegisterRef::Page,
        0x2E | 0x2F => RegisterRef::BusTransfer,
        0x50..=0x5F => RegisterRef::Constant(CONSTANT_TABLE[usize::from(index - 0x50)]),
        0x60..=0x7F => RegisterRef::General(index & 0x0F),
        _ => RegisterRef::Unmapped,
    }
}

/// The operand an instruction's low byte gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operand {
    /// High-byte bit 2 set: the low byte itself.
    Immediate(u8),
    /// High-byte bit 2 clear: the register-file index the low byte holds.
    Register(u8),
}

/// The operand of `opcode`.
pub fn operand(opcode: u16) -> Operand {
    let low = opcode as u8;
    if opcode & 0x0400 != 0 {
        Operand::Immediate(low)
    } else {
        Operand::Register(low)
    }
}

/// How far an arithmetic or logic operation shifts A before it runs, from
/// the high byte's bits 1-0.
pub fn shift_amount(opcode: u16) -> u32 {
    const AMOUNTS: [u32; 4] = [0, 1, 8, 16];
    AMOUNTS[usize::from((opcode >> 8) & 3)]
}

/// A shifted as `opcode` asks, in 32 bits: a shift by 16 leaves A's top
/// byte out, and a shift by 8 or 16 keeps bits above 24.
pub fn shifted(opcode: u16, accumulator: u32) -> u32 {
    accumulator.wrapping_shl(shift_amount(opcode))
}

/// Whether a jump takes its target page from P (high-byte bit 1).
pub fn is_long_jump(opcode: u16) -> bool {
    opcode & 0x0200 != 0
}

/// Where a jump goes: the low byte within P's page for a long jump, within
/// the current page for a near one.
pub fn jump_target(opcode: u16, pc: u32, page: u16) -> u32 {
    let position = u32::from(opcode as u8);
    let page = if is_long_jump(opcode) {
        u32::from(page)
    } else {
        (pc >> 8) & 0xFFFF
    };
    (page << 8) | position
}

/// Push a return address onto the eight-deep call stack: every entry moves
/// one deeper, the deepest is lost.
pub fn push(stack: &mut [u32; 8], pc: u32) {
    stack.copy_within(0..7, 1);
    stack[0] = pc;
}

/// Pop the top of the call stack: every entry moves one up and the deepest
/// becomes 0.
pub fn pull(stack: &mut [u32; 8]) -> u32 {
    let top = stack[0];
    stack.copy_within(1..8, 0);
    stack[7] = 0;
    top
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_constants_are_the_sixteen_values_at_50_5f() {
        assert_eq!(
            CONSTANT_TABLE,
            [
                0x00_0000, 0xFF_FFFF, 0x00_FF00, 0xFF_0000, 0x00_FFFF, 0xFF_FF00, 0x80_0000,
                0x7F_FFFF, 0x00_8000, 0x00_7FFF, 0xFF_7FFF, 0xFF_FF7F, 0x01_0000, 0xFE_FFFF,
                0x00_0100, 0x00_FEFF,
            ]
        );
        assert_eq!(decode_register(0x50), RegisterRef::Constant(0x00_0000));
        assert_eq!(decode_register(0x56), RegisterRef::Constant(0x80_0000));
        assert_eq!(decode_register(0x5F), RegisterRef::Constant(0x00_FEFF));
    }

    #[test]
    fn named_registers() {
        assert_eq!(decode_register(0x00), RegisterRef::Accumulator);
        assert_eq!(decode_register(0x01), RegisterRef::ProductHigh);
        assert_eq!(decode_register(0x02), RegisterRef::ProductLow);
        assert_eq!(decode_register(0x03), RegisterRef::BusData);
        assert_eq!(decode_register(0x08), RegisterRef::RomData);
        assert_eq!(decode_register(0x0C), RegisterRef::RamData);
        assert_eq!(decode_register(0x13), RegisterRef::BusAddress);
        assert_eq!(decode_register(0x1C), RegisterRef::RamBase);
        assert_eq!(decode_register(0x20), RegisterRef::ProgramCounter);
        assert_eq!(decode_register(0x28), RegisterRef::Page);
        assert_eq!(decode_register(0x2E), RegisterRef::BusTransfer);
        assert_eq!(decode_register(0x2F), RegisterRef::BusTransfer);
    }

    #[test]
    fn general_registers_appear_twice() {
        assert_eq!(decode_register(0x60), RegisterRef::General(0));
        assert_eq!(decode_register(0x6F), RegisterRef::General(15));
        assert_eq!(decode_register(0x70), RegisterRef::General(0));
        assert_eq!(decode_register(0x7F), RegisterRef::General(15));
        // The top bit of a byte is not part of the index.
        assert_eq!(decode_register(0xE0), RegisterRef::General(0));
    }

    #[test]
    fn indices_that_name_nothing() {
        for index in [0x04u8, 0x07, 0x09, 0x12, 0x1D, 0x27, 0x2D, 0x30, 0x4F] {
            assert_eq!(
                decode_register(index),
                RegisterRef::Unmapped,
                "{index:#04x}"
            );
        }
    }

    #[test]
    fn the_shift_of_a() {
        assert_eq!(shift_amount(0x0000), 0);
        assert_eq!(shift_amount(0x0100), 1);
        assert_eq!(shift_amount(0x0200), 8);
        assert_eq!(shift_amount(0x0300), 16);
        assert_eq!(shift_amount(0x8155), 1);
        // A shift by 16 keeps 32 bits, so A's top byte is lost.
        assert_eq!(shifted(0x0300, 0x12_3456), 0x3456_0000);
        assert_eq!(shifted(0x0000, 0x12_3456), 0x12_3456);
    }

    #[test]
    fn the_operand_is_the_byte_or_the_register_it_names() {
        assert_eq!(operand(0x0455), Operand::Immediate(0x55));
        assert_eq!(operand(0x0060), Operand::Register(0x60));
    }

    #[test]
    fn jump_targets() {
        assert!(!is_long_jump(0x0812));
        assert_eq!(jump_target(0x0812, 0x01_23AB, 0x0055), 0x01_2312);
        assert!(is_long_jump(0x0A34));
        assert_eq!(jump_target(0x0A34, 0x01_23AB, 0x0055), 0x00_5534);
    }

    #[test]
    fn a_push_drops_the_deepest_entry() {
        let mut stack = [0u32; 8];
        for pc in 1..=9u32 {
            push(&mut stack, pc);
        }
        assert_eq!(stack, [9, 8, 7, 6, 5, 4, 3, 2]);
    }

    #[test]
    fn a_pull_clears_the_deepest_entry() {
        let mut stack = [10, 20, 30, 40, 50, 60, 70, 80];
        assert_eq!(pull(&mut stack), 10);
        assert_eq!(stack, [20, 30, 40, 50, 60, 70, 80, 0]);
        assert_eq!(pull(&mut stack), 20);
        assert_eq!(stack, [30, 40, 50, 60, 70, 80, 0, 0]);
    }

    #[test]
    fn push_then_pull_round_trips() {
        let mut stack = [0u32; 8];
        push(&mut stack, 0x12_3456);
        push(&mut stack, 0x00_00AB);
        assert_eq!(pull(&mut stack), 0x00_00AB);
        assert_eq!(pull(&mut stack), 0x12_3456);
        assert_eq!(pull(&mut stack), 0);
    }
}
