//! SPC700 addressing mode resolution.
//!
//! The SPC700 inherits the 6502-style addressing repertoire
//! (immediate, direct page, indexed, indirect) and adds a handful of
//! bit-addressed modes (`AABB.b` for `MOV1`/`SET1`/`CLR1`) plus the
//! `[d]+Y` and `[d+X]` indirect forms used by `MOVW`. This module
//! owns the operand-shape enum and the helpers that translate it into
//! effective 16-bit addresses on a given [`Addressable`] bus.
//!
//! ## Operand shapes vs. source/destination direction
//!
//! [`AddressingMode`] models only the operand *shape*: `a,$dp` and
//! `$dp,a` are the same mode. Direction (which register receives or
//! supplies the byte) is recovered from the opcode inside each
//! instruction handler. This keeps the enum small (~25 variants) and
//! the per-mode helpers reusable across mnemonics.

use retro_core::cpu::Addressable;

use crate::Spc700;

/// Operand shape for a decoded SPC700 instruction.
///
/// Each variant describes the *bytes that follow the opcode* and the
/// arithmetic the CPU performs to turn them into an effective address.
/// "Implied" / "Accumulator" never reach the bus through this enum,
/// they just keep the opcode 1 byte long.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressingMode {
    /// No operand. 1 byte. Used by NOP, RET, RETI, BRK, EI, DI,
    /// CLRC/SETC/NOTC, CLRP/SETP, CLRV, SLEEP, STOP, MOV reg-to-reg,
    /// PUSH/POP, MUL, DIV, INC/DEC X/Y.
    Implied,
    /// Implicit `A` operand. 1 byte. Used by ASL A / LSR A / ROL A /
    /// ROR A / INC A / DEC A / XCN A / DAA / DAS / PUSH A / POP A.
    /// Treated as `Implied` for the bus but distinguished here so the
    /// disassembler can tell it apart (the trace format prints nothing
    /// for it).
    Accumulator,
    /// `#imm`: 8-bit immediate. 2 bytes total.
    Immediate,
    /// `+/-$rr`: 8-bit signed relative offset (branches). 2 bytes total.
    Relative,
    /// `$dp`: 8-bit DP offset, added to `dp_base()`. 2 bytes total.
    DirectPage,
    /// `$dp+X`: DP offset with `X` added before lookup. 2 bytes total.
    DirectPageX,
    /// `$dp+Y`: DP offset with `Y` added before lookup. 2 bytes total.
    DirectPageY,
    /// `!abs`: 16-bit absolute address. 3 bytes total.
    Absolute,
    /// `!abs+X`: 16-bit absolute address with `X` added. 3 bytes total.
    AbsoluteX,
    /// `!abs+Y`: 16-bit absolute address with `Y` added. 3 bytes total.
    AbsoluteY,
    /// `(X)`: DP-relative indirect through register `X`. 1 byte total.
    /// Used by `MOV (X),A`, `MOV A,(X)`, `OR/AND/EOR/CMP/ADC/SBC A,(X)`.
    IndirectX,
    /// `(X)+`: same as [`IndirectX`] but post-increments `X` (with wrap).
    /// 1 byte total. Used by `MOV A,(X)+` (0xBF) and `MOV (X)+,A` (0xAF).
    ///
    /// [`IndirectX`]: AddressingMode::IndirectX
    IndirectXAuto,
    /// `($dp+X)`: DP[+X] holds a 16-bit absolute pointer. 2 bytes total.
    IndirectIndirectX,
    /// `($dp)+Y`: DP holds a 16-bit base; `Y` added at use. 2 bytes total.
    IndirectIndirectY,
    /// `(X),(Y)`: both operands are DP-relative indirects (the rare
    /// `OR (X),(Y)` family at 0x19/0x39/0x59/0x79/0x99/0xB9). 1 byte
    /// total. Source = `(Y)`, destination = `(X)`.
    IxIy,
    /// `[!abs+X]`: 16-bit pointer at `!abs+X` (for `JMP [!abs+X]`).
    /// 3 bytes total.
    AbsoluteIndirectX,
    /// `$dp,#imm`: 3 bytes total. Source = immediate, destination = `$dp`
    /// (or read-modify-write target, depending on the instruction).
    DirectPageImm,
    /// `$dp(src),$dp(dst)`: the instruction stores the SOURCE byte
    /// first and the DESTINATION second. 3 bytes total.
    DirectPageDirectPage,
    /// `$dp,+/-$rr`: 3 bytes total. Used by BBS/BBC/CBNE/DBNZ ($dp).
    DirectPageRelative,
    /// `$dp+X,+/-$rr`: 3 bytes total. Used by `CBNE $dp+X,rel` (0xDE).
    DirectPageXRelative,
    /// `Y,+/-$rr`: 2 bytes total. Used only by `DBNZ Y,rel` (0xFE).
    YRelative,
    /// `ya,$dp` / `$dp,ya`: DP-addressed 16-bit operand for `MOVW`,
    /// `ADDW`, `SUBW`, `CMPW`. 2 bytes total.
    YaDirectPage,
    /// `m.b`: 16-bit operand where the top 3 bits are the bit index
    /// and the bottom 13 are the absolute address. 3 bytes total.
    /// Used by `OR1 C,m.b`, `AND1 C,m.b`, `EOR1 C,m.b`, `MOV1 C,m.b`,
    /// `MOV1 m.b,C`, `NOT1 m.b`.
    MemBit,
    /// `/m.b`: same encoding as [`MemBit`] but with the bit inverted
    /// before the operation (`OR1 C,/m.b`, `AND1 C,/m.b`). 3 bytes.
    ///
    /// [`MemBit`]: AddressingMode::MemBit
    MemBitNot,
    /// `!u`: 8-bit operand `u`; target address is `$FF00 | u`.
    /// 2 bytes total. Used by `PCALL u`.
    PCallVector,
    /// `tcall n`: `n` is encoded in the upper 4 bits of the opcode.
    /// 1 byte total. Vector address is `$FFC0 + 2*(15-n)`.
    TCallVector,
}

impl AddressingMode {
    /// Byte size of the encoded instruction (opcode + operand bytes).
    pub const fn operand_size(self) -> u8 {
        match self {
            Self::Implied
            | Self::Accumulator
            | Self::IndirectX
            | Self::IndirectXAuto
            | Self::IxIy
            | Self::TCallVector => 1,
            Self::Immediate
            | Self::Relative
            | Self::DirectPage
            | Self::DirectPageX
            | Self::DirectPageY
            | Self::IndirectIndirectX
            | Self::IndirectIndirectY
            | Self::YRelative
            | Self::YaDirectPage
            | Self::PCallVector => 2,
            Self::Absolute
            | Self::AbsoluteX
            | Self::AbsoluteY
            | Self::AbsoluteIndirectX
            | Self::DirectPageImm
            | Self::DirectPageDirectPage
            | Self::DirectPageRelative
            | Self::DirectPageXRelative
            | Self::MemBit
            | Self::MemBitNot => 3,
        }
    }
}

// -- Operand fetch / address resolution helpers ------------------------------
//
// These all read the *operand* bytes; the opcode itself is already past
// `PC` by the time we enter any of them. Each helper advances `PC` over
// the bytes it consumes and performs the bus reads the instruction
// makes, in the order the SingleStepTests record them.

/// Fetch the next byte under `PC` and advance `PC`.
pub fn fetch_pc_byte(cpu: &mut Spc700, bus: &mut dyn Addressable) -> u8 {
    let b = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    b
}

/// Fetch the next 16-bit little-endian word under `PC` and advance `PC`
/// by 2.
pub fn fetch_pc_word(cpu: &mut Spc700, bus: &mut dyn Addressable) -> u16 {
    let lo = fetch_pc_byte(cpu, bus) as u16;
    let hi = fetch_pc_byte(cpu, bus) as u16;
    (hi << 8) | lo
}

/// Read the effective direct-page address for an 8-bit `dp` offset:
/// `dp_base() | dp`. DP addressing is always confined to one page.
pub fn dp_addr(cpu: &Spc700, dp: u8) -> u16 {
    cpu.dp_base() | (dp as u16)
}

/// `dp_addr(cpu, dp + X)` with 8-bit wrap inside the DP page.
pub fn dp_addr_x(cpu: &Spc700, dp: u8) -> u16 {
    cpu.dp_base() | (dp.wrapping_add(cpu.x) as u16)
}

/// `dp_addr(cpu, dp + Y)` with 8-bit wrap inside the DP page.
pub fn dp_addr_y(cpu: &Spc700, dp: u8) -> u16 {
    cpu.dp_base() | (dp.wrapping_add(cpu.y) as u16)
}

/// `(X)`: current DP-relative indirect address through `X`.
pub fn x_indirect_addr(cpu: &Spc700) -> u16 {
    cpu.dp_base() | (cpu.x as u16)
}

/// `(Y)`: current DP-relative indirect address through `Y`.
pub fn y_indirect_addr(cpu: &Spc700) -> u16 {
    cpu.dp_base() | (cpu.y as u16)
}

/// Read a 16-bit pointer stored at consecutive DP bytes, wrapping the
/// low byte's offset within the DP page (so `$ff` reads from `$ff`
/// then `$00` of the same page).
pub fn read_dp_word(cpu: &Spc700, bus: &mut dyn Addressable, dp: u8) -> u16 {
    let lo = bus.read(dp_addr(cpu, dp) as u32) as u16;
    let hi = bus.read(dp_addr(cpu, dp.wrapping_add(1)) as u32) as u16;
    (hi << 8) | lo
}

/// Write a 16-bit value at consecutive DP bytes with low-byte wrap.
pub fn write_dp_word(cpu: &Spc700, bus: &mut dyn Addressable, dp: u8, val: u16) {
    bus.write(dp_addr(cpu, dp) as u32, val as u8);
    bus.write(dp_addr(cpu, dp.wrapping_add(1)) as u32, (val >> 8) as u8);
}

/// Resolve `($dp+X)` to the absolute 16-bit address it points to.
pub fn indirect_indirect_x(cpu: &Spc700, bus: &mut dyn Addressable, dp: u8) -> u16 {
    read_dp_word(cpu, bus, dp.wrapping_add(cpu.x))
}

/// Resolve `($dp)+Y` to the absolute 16-bit address it points to (base
/// in DP, `Y` added at use).
pub fn indirect_indirect_y(cpu: &Spc700, bus: &mut dyn Addressable, dp: u8) -> u16 {
    read_dp_word(cpu, bus, dp).wrapping_add(cpu.y as u16)
}

/// Fetch the 16-bit MemBit operand from `PC` and split it into
/// `(absolute_addr, bit_index)`. Address is the bottom 13 bits
/// (`0x0000..=0x1FFF`); bit index is the top 3 bits (`0..=7`).
/// Used by NOT1, OR1, AND1, EOR1, and the two MOV1 single-bit
/// variants: the operand is one little-endian word, read low byte
/// first.
pub fn fetch_mem_bit(cpu: &mut Spc700, bus: &mut dyn Addressable) -> (u16, u8) {
    let raw = fetch_pc_word(cpu, bus);
    let bit = (raw >> 13) as u8;
    let addr = raw & 0x1FFF;
    (addr, bit)
}

#[cfg(test)]
mod tests {
    //! Unit tests for the addressing helpers. Each test instantiates
    //! a fresh CPU + 64 KiB linear bus, primes the bytes the helper
    //! is about to read, and asserts the helper produced the expected
    //! effective address and PC advance.

    use super::*;
    use crate::instructions::FLAG_P;
    use crate::Spc700;

    struct TinyBus {
        ram: [u8; 0x10000],
    }
    impl TinyBus {
        fn new() -> Self {
            Self { ram: [0; 0x10000] }
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

    fn fresh(pc: u16) -> (Spc700, TinyBus) {
        let mut cpu = Spc700::new();
        cpu.pc = pc;
        (cpu, TinyBus::new())
    }

    // -- fetch_pc_byte / fetch_pc_word ------------------------------------

    #[test]
    fn fetch_pc_byte_advances_pc_by_one() {
        let (mut cpu, mut bus) = fresh(0x2000);
        bus.write(0x2000, 0xAB);
        let v = fetch_pc_byte(&mut cpu, &mut bus);
        assert_eq!(v, 0xAB);
        assert_eq!(cpu.pc, 0x2001);
    }

    #[test]
    fn fetch_pc_word_is_little_endian_and_advances_pc_by_two() {
        let (mut cpu, mut bus) = fresh(0x3000);
        bus.write(0x3000, 0x34); // low
        bus.write(0x3001, 0x12); // high
        let v = fetch_pc_word(&mut cpu, &mut bus);
        assert_eq!(v, 0x1234);
        assert_eq!(cpu.pc, 0x3002);
    }

    #[test]
    fn fetch_pc_byte_wraps_at_top_of_address_space() {
        let (mut cpu, mut bus) = fresh(0xFFFF);
        bus.write(0xFFFF, 0x77);
        let v = fetch_pc_byte(&mut cpu, &mut bus);
        assert_eq!(v, 0x77);
        assert_eq!(cpu.pc, 0x0000);
    }

    // -- dp_addr / dp_addr_x / dp_addr_y ----------------------------------

    #[test]
    fn dp_addr_respects_psw_p_zero() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        assert_eq!(dp_addr(&cpu, 0x42), 0x0042);
    }

    #[test]
    fn dp_addr_respects_psw_p_one() {
        let mut cpu = Spc700::new();
        cpu.set_flag(FLAG_P, true);
        assert_eq!(dp_addr(&cpu, 0x42), 0x0142);
    }

    #[test]
    fn dp_addr_x_wraps_inside_direct_page() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        cpu.x = 0x05;
        assert_eq!(
            dp_addr_x(&cpu, 0xFE),
            0x0003,
            "should wrap to $03 within DP"
        );
        cpu.set_flag(FLAG_P, true);
        assert_eq!(dp_addr_x(&cpu, 0xFE), 0x0103);
    }

    #[test]
    fn dp_addr_y_wraps_inside_direct_page() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        cpu.y = 0x10;
        assert_eq!(dp_addr_y(&cpu, 0xF8), 0x0008);
    }

    // -- x_indirect_addr / y_indirect_addr --------------------------------

    #[test]
    fn x_indirect_addr_combines_x_with_dp_base() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        cpu.x = 0x55;
        assert_eq!(x_indirect_addr(&cpu), 0x0055);
        cpu.set_flag(FLAG_P, true);
        assert_eq!(x_indirect_addr(&cpu), 0x0155);
    }

    #[test]
    fn y_indirect_addr_combines_y_with_dp_base() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        cpu.y = 0xAA;
        assert_eq!(y_indirect_addr(&cpu), 0x00AA);
    }

    // -- read_dp_word / write_dp_word -------------------------------------

    #[test]
    fn read_dp_word_is_little_endian_within_dp() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        let mut bus = TinyBus::new();
        bus.write(0x0040, 0xCD);
        bus.write(0x0041, 0xAB);
        let v = read_dp_word(&cpu, &mut bus, 0x40);
        assert_eq!(v, 0xABCD);
    }

    #[test]
    fn read_dp_word_low_byte_wraps_within_page() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        let mut bus = TinyBus::new();
        bus.write(0x00FF, 0x34);
        bus.write(0x0000, 0x12);
        let v = read_dp_word(&cpu, &mut bus, 0xFF);
        assert_eq!(
            v, 0x1234,
            "low byte at $FF, high byte must wrap to $00 (same page)"
        );
    }

    #[test]
    fn read_dp_word_low_byte_wraps_within_page_one() {
        let mut cpu = Spc700::new();
        cpu.psw = FLAG_P;
        let mut bus = TinyBus::new();
        bus.write(0x01FF, 0x99);
        bus.write(0x0100, 0xCC);
        let v = read_dp_word(&cpu, &mut bus, 0xFF);
        assert_eq!(v, 0xCC99);
    }

    #[test]
    fn write_dp_word_round_trips_through_read() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        let mut bus = TinyBus::new();
        write_dp_word(&cpu, &mut bus, 0x10, 0xBEEF);
        assert_eq!(bus.read(0x0010), 0xEF);
        assert_eq!(bus.read(0x0011), 0xBE);
        let v = read_dp_word(&cpu, &mut bus, 0x10);
        assert_eq!(v, 0xBEEF);
    }

    // -- indirect_indirect_x / indirect_indirect_y ------------------------

    #[test]
    fn indirect_indirect_x_dereferences_at_dp_plus_x() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        cpu.x = 0x05;
        let mut bus = TinyBus::new();
        bus.write(0x0015, 0x34);
        bus.write(0x0016, 0x12);
        let v = indirect_indirect_x(&cpu, &mut bus, 0x10);
        assert_eq!(v, 0x1234);
    }

    #[test]
    fn indirect_indirect_x_wraps_dp_pointer_within_page() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        cpu.x = 0xFF;
        let mut bus = TinyBus::new();
        bus.write(0x0000, 0x78);
        bus.write(0x0001, 0x56);
        let v = indirect_indirect_x(&cpu, &mut bus, 0x01);
        assert_eq!(v, 0x5678);
    }

    #[test]
    fn indirect_indirect_y_loads_base_then_adds_y() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        cpu.y = 0x10;
        let mut bus = TinyBus::new();
        bus.write(0x0040, 0x00);
        bus.write(0x0041, 0x20);
        let v = indirect_indirect_y(&cpu, &mut bus, 0x40);
        assert_eq!(v, 0x2010, "($40) = $2000, +Y(0x10) = $2010");
    }

    // -- fetch_mem_bit ----------------------------------------------------

    #[test]
    fn fetch_mem_bit_splits_bottom_13_bits_as_addr_and_top_3_as_bit() {
        let (mut cpu, mut bus) = fresh(0x4000);
        // Operand = 0xE123  -> bit = 0xE123 >> 13 = 7, addr = 0xE123 & 0x1FFF = 0x0123.
        bus.write(0x4000, 0x23); // low
        bus.write(0x4001, 0xE1); // high
        let (addr, bit) = fetch_mem_bit(&mut cpu, &mut bus);
        assert_eq!(addr, 0x0123);
        assert_eq!(bit, 7);
        assert_eq!(cpu.pc, 0x4002);
    }

    #[test]
    fn fetch_mem_bit_zero_bit_index_uses_full_13_bit_address() {
        let (mut cpu, mut bus) = fresh(0x5000);
        // Operand = 0x1FFF  -> bit = 0, addr = 0x1FFF (max 13-bit address).
        bus.write(0x5000, 0xFF);
        bus.write(0x5001, 0x1F);
        let (addr, bit) = fetch_mem_bit(&mut cpu, &mut bus);
        assert_eq!(addr, 0x1FFF);
        assert_eq!(bit, 0);
    }

    // -- AddressingMode::operand_size -------------------------------------

    #[test]
    fn operand_size_is_the_instruction_length() {
        assert_eq!(AddressingMode::Implied.operand_size(), 1);
        assert_eq!(AddressingMode::Accumulator.operand_size(), 1);
        assert_eq!(AddressingMode::IndirectX.operand_size(), 1);
        assert_eq!(AddressingMode::IndirectXAuto.operand_size(), 1);
        assert_eq!(AddressingMode::IxIy.operand_size(), 1);
        assert_eq!(AddressingMode::TCallVector.operand_size(), 1);

        assert_eq!(AddressingMode::Immediate.operand_size(), 2);
        assert_eq!(AddressingMode::Relative.operand_size(), 2);
        assert_eq!(AddressingMode::DirectPage.operand_size(), 2);
        assert_eq!(AddressingMode::DirectPageX.operand_size(), 2);
        assert_eq!(AddressingMode::DirectPageY.operand_size(), 2);
        assert_eq!(AddressingMode::IndirectIndirectX.operand_size(), 2);
        assert_eq!(AddressingMode::IndirectIndirectY.operand_size(), 2);
        assert_eq!(AddressingMode::YRelative.operand_size(), 2);
        assert_eq!(AddressingMode::YaDirectPage.operand_size(), 2);
        assert_eq!(AddressingMode::PCallVector.operand_size(), 2);

        assert_eq!(AddressingMode::Absolute.operand_size(), 3);
        assert_eq!(AddressingMode::AbsoluteX.operand_size(), 3);
        assert_eq!(AddressingMode::AbsoluteY.operand_size(), 3);
        assert_eq!(AddressingMode::AbsoluteIndirectX.operand_size(), 3);
        assert_eq!(AddressingMode::DirectPageImm.operand_size(), 3);
        assert_eq!(AddressingMode::DirectPageDirectPage.operand_size(), 3);
        assert_eq!(AddressingMode::DirectPageRelative.operand_size(), 3);
        assert_eq!(AddressingMode::DirectPageXRelative.operand_size(), 3);
        assert_eq!(AddressingMode::MemBit.operand_size(), 3);
        assert_eq!(AddressingMode::MemBitNot.operand_size(), 3);
    }
}
