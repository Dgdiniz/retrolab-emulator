//! Arithmetic instructions.
//!
//! 44 opcodes split into:
//! - **Binary ALU** (12 + 12): `ADC` / `SBC` across the standard
//!   12-mode matrix (`$84-$89`, `$94-$99` for ADC; `$A4-$A9`, `$B4-$B9`
//!   for SBC). Operand-fetch handled by [`super::binary_ops`].
//! - **8-bit RMW** (6 + 6): `INC` / `DEC` across `A`, `X`, `Y`, `$dp`,
//!   `!abs`, `$dp+X`. Updates **only N/Z** (never C/V/H).
//! - **16-bit DP RMW** (1 + 1): `INCW` / `DECW` `$dp`, the low byte
//!   is read and written back before the high byte is read. Updates N
//!   (from bit 15) and Z (from full 16-bit).
//! - **16-bit YA ALU** (1 + 1): `ADDW YA,$dp` / `SUBW YA,$dp`, the
//!   adder at word width, with no carry or borrow in. `H` is the carry
//!   out of bit 11; `N`, `V` and `C` are bit 15's; `Z` is the word's.
//! - **Special** (4): `MUL YA` (`$CF`), `DIV YA,X` (`$9E`), `DAA`
//!   (`$DF`), `DAS` (`$BE`).
//!
//! Everything that adds or subtracts goes through one adder (`add`):
//! the processor subtracts by adding a complement, so the flags of
//! `SBC` and `SUBW` are the adder's for that sum. The semantics are the
//! ones fullsnes and Anomie's SPC700 notes describe; every opcode
//! passes the SingleStepTests sweep, bus accesses included, and the
//! results were checked against bsnes-plus's formulas over every
//! operand and every `PSW`.

use retro_core::cpu::Addressable;

use crate::addressing::{
    dp_addr, dp_addr_x, fetch_pc_byte, fetch_pc_word, read_dp_word, AddressingMode,
};
use crate::decode::OpcodeInfo;
use crate::instructions::binary_ops::{
    is_mem_target, read_a_target_operand, read_mem_target_operands,
};
use crate::instructions::{FLAG_C, FLAG_H, FLAG_V};
use crate::Spc700;

// -- The adder ---------------------------------------------------------------

/// How wide the adder's operands are.
#[derive(Clone, Copy)]
enum Width {
    Byte = 8,
    /// The `YA` pair.
    Word = 16,
}

/// What one pass through the adder leaves.
struct Sum {
    value: u16,
    /// A carry out of the top bit.
    carry: bool,
    /// A carry out of the bit below the top hexadecimal digit: bit 3 of
    /// a byte, bit 11 of a word.
    half_carry: bool,
    /// The operands, read as signed, have a sum that does not fit.
    overflow: bool,
}

/// `left + right + carry_in`.
fn add(left: u16, right: u16, carry_in: bool, width: Width) -> Sum {
    let bits = width as u32;
    let all = (1u32 << bits) - 1;
    let below_top_digit = all >> 4;
    let sign = 1u32 << (bits - 1);
    let (left, right, carry_in) = (u32::from(left), u32::from(right), u32::from(carry_in));

    let wide = left + right + carry_in;
    let value = wide & all;
    Sum {
        value: value as u16,
        carry: wide > all,
        half_carry: (left & below_top_digit) + (right & below_top_digit) + carry_in
            > below_top_digit,
        // Two operands of one sign whose sum has the other.
        overflow: (left ^ right) & sign == 0 && (left ^ value) & sign != 0,
    }
}

/// Leaves the adder's three flags and the result's `N` and `Z`.
fn keep_flags(cpu: &mut Spc700, sum: &Sum, width: Width) {
    cpu.set_flag(FLAG_C, sum.carry);
    cpu.set_flag(FLAG_H, sum.half_carry);
    cpu.set_flag(FLAG_V, sum.overflow);
    match width {
        Width::Byte => cpu.update_nz_u8(sum.value as u8),
        Width::Word => cpu.update_nz_u16(sum.value),
    }
}

/// `ADC`: `left + right + C`.
pub(super) fn add_with_carry(cpu: &mut Spc700, left: u8, right: u8) -> u8 {
    let sum = add(left.into(), right.into(), cpu.get_flag(FLAG_C), Width::Byte);
    keep_flags(cpu, &sum, Width::Byte);
    sum.value as u8
}

/// `SBC`: `left - right - !C`. The processor subtracts by adding the
/// operand's complement, so the flags are the adder's: `C` and `H` are
/// set when there was *no* borrow out of the byte and out of its low
/// digit.
pub(super) fn subtract_with_borrow(cpu: &mut Spc700, left: u8, right: u8) -> u8 {
    add_with_carry(cpu, left, !right)
}

/// `ADDW YA,$dp`: a 16-bit sum. The `C` the instruction finds is not
/// taken in.
pub(super) fn add_words(cpu: &mut Spc700, left: u16, right: u16) -> u16 {
    let sum = add(left, right, false, Width::Word);
    keep_flags(cpu, &sum, Width::Word);
    sum.value
}

/// `SUBW YA,$dp`: a 16-bit difference with no borrow in, the
/// operand's complement and a carry in of one, through the same adder.
pub(super) fn subtract_words(cpu: &mut Spc700, left: u16, right: u16) -> u16 {
    let sum = add(left, !right, true, Width::Word);
    keep_flags(cpu, &sum, Width::Word);
    sum.value
}

/// 8-bit INC. Updates ONLY `N` and `Z`; never touches `C/V/H`.
pub(super) fn incremented(cpu: &mut Spc700, value: u8) -> u8 {
    let result = value.wrapping_add(1);
    cpu.update_nz_u8(result);
    result
}

/// 8-bit DEC. Updates ONLY `N` and `Z`; never touches `C/V/H`.
pub(super) fn decremented(cpu: &mut Spc700, value: u8) -> u8 {
    let result = value.wrapping_sub(1);
    cpu.update_nz_u8(result);
    result
}

// -- Handlers: ADC / SBC -----------------------------------------------------

pub fn adc(cpu: &mut Spc700, bus: &mut dyn Addressable, _opcode: u8, info: &OpcodeInfo) -> u32 {
    if is_mem_target(info.mode) {
        let (dst_addr, dst_val, src_val) = read_mem_target_operands(cpu, bus, info.mode);
        let r = add_with_carry(cpu, dst_val, src_val);
        bus.write(dst_addr as u32, r);
    } else {
        let src = read_a_target_operand(cpu, bus, info.mode);
        cpu.a = add_with_carry(cpu, cpu.a, src);
    }
    info.base_cycles as u32
}

pub fn sbc(cpu: &mut Spc700, bus: &mut dyn Addressable, _opcode: u8, info: &OpcodeInfo) -> u32 {
    if is_mem_target(info.mode) {
        let (dst_addr, dst_val, src_val) = read_mem_target_operands(cpu, bus, info.mode);
        let r = subtract_with_borrow(cpu, dst_val, src_val);
        bus.write(dst_addr as u32, r);
    } else {
        let src = read_a_target_operand(cpu, bus, info.mode);
        cpu.a = subtract_with_borrow(cpu, cpu.a, src);
    }
    info.base_cycles as u32
}

// -- Handlers: ADDW / SUBW ---------------------------------------------------

pub fn addw(cpu: &mut Spc700, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    // 0x7A: `ADDW YA, $dp`.
    let dp = fetch_pc_byte(cpu, bus);
    let operand = read_dp_word(cpu, bus, dp);
    let r = add_words(cpu, cpu.ya(), operand);
    cpu.set_ya(r);
    info.base_cycles as u32
}

pub fn subw(cpu: &mut Spc700, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    // 0x9A: `SUBW YA, $dp`.
    let dp = fetch_pc_byte(cpu, bus);
    let operand = read_dp_word(cpu, bus, dp);
    let r = subtract_words(cpu, cpu.ya(), operand);
    cpu.set_ya(r);
    info.base_cycles as u32
}

// -- Handlers: INC / DEC (8-bit) ---------------------------------------------

pub fn inc(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    match opcode {
        0x3D => cpu.x = incremented(cpu, cpu.x),
        0xFC => cpu.y = incremented(cpu, cpu.y),
        0xBC => cpu.a = incremented(cpu, cpu.a),
        0xAB => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = dp_addr(cpu, dp);
            let val = bus.read(addr as u32);
            let r = incremented(cpu, val);
            bus.write(addr as u32, r);
        }
        0xAC => {
            let addr = fetch_pc_word(cpu, bus);
            let val = bus.read(addr as u32);
            let r = incremented(cpu, val);
            bus.write(addr as u32, r);
        }
        0xBB => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = dp_addr_x(cpu, dp);
            let val = bus.read(addr as u32);
            let r = incremented(cpu, val);
            bus.write(addr as u32, r);
        }
        _ => unreachable!("inc dispatched with non-INC opcode {:02X}", opcode),
    }
    info.base_cycles as u32
}

pub fn dec(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    match opcode {
        0x1D => cpu.x = decremented(cpu, cpu.x),
        0xDC => cpu.y = decremented(cpu, cpu.y),
        0x9C => cpu.a = decremented(cpu, cpu.a),
        0x8B => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = dp_addr(cpu, dp);
            let val = bus.read(addr as u32);
            let r = decremented(cpu, val);
            bus.write(addr as u32, r);
        }
        0x8C => {
            let addr = fetch_pc_word(cpu, bus);
            let val = bus.read(addr as u32);
            let r = decremented(cpu, val);
            bus.write(addr as u32, r);
        }
        0x9B => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = dp_addr_x(cpu, dp);
            let val = bus.read(addr as u32);
            let r = decremented(cpu, val);
            bus.write(addr as u32, r);
        }
        _ => unreachable!("dec dispatched with non-DEC opcode {:02X}", opcode),
    }
    info.base_cycles as u32
}

// -- Handlers: INCW / DECW (16-bit DP RMW) -----------------------------------

/// `INCW $dp` (`$3A`) and `DECW $dp` (`$1A`): the word at a direct-page
/// address goes up or down by one. The processor works a byte at a
/// time and the bus shows it, as the SingleStepTests record: the low
/// byte is read and written back changed before the high byte is read,
/// and the high byte is written back whether or not the low one
/// wrapped into it. Sets `N` from bit 15 and `Z` from the whole word;
/// leaves `C/V/H` alone.
fn step_word(cpu: &mut Spc700, bus: &mut dyn Addressable, dp: u8, up: bool) {
    let low_at = u32::from(dp_addr(cpu, dp));
    let high_at = u32::from(dp_addr(cpu, dp.wrapping_add(1)));

    let low = bus.read(low_at);
    let (new_low, wrapped) = if up {
        low.overflowing_add(1)
    } else {
        low.overflowing_sub(1)
    };
    bus.write(low_at, new_low);

    let high = bus.read(high_at);
    let new_high = match (wrapped, up) {
        (false, _) => high,
        (true, true) => high.wrapping_add(1),
        (true, false) => high.wrapping_sub(1),
    };
    bus.write(high_at, new_high);

    cpu.update_nz_u16(u16::from_le_bytes([new_low, new_high]));
}

pub fn incw(cpu: &mut Spc700, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    let dp = fetch_pc_byte(cpu, bus);
    step_word(cpu, bus, dp, true);
    info.base_cycles as u32
}

pub fn decw(cpu: &mut Spc700, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    let dp = fetch_pc_byte(cpu, bus);
    step_word(cpu, bus, dp, false);
    info.base_cycles as u32
}

// -- Handlers: MUL / DIV / DAA / DAS -----------------------------------------

/// `MUL YA` (`$CF`): `YA = Y * A` (16-bit unsigned product). `Y` holds
/// the high byte, `A` the low byte. Flags: `N` and `Z` are derived
/// from `Y` (the high byte) ONLY, `V/H/C` are left untouched.
pub fn mul(cpu: &mut Spc700, _bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    cpu.set_ya(u16::from(cpu.y) * u16::from(cpu.a));
    cpu.update_nz_u8(cpu.y);
    info.base_cycles as u32
}

/// `DIV YA, X` (`$9E`): divides the 16-bit dividend `YA` by the 8-bit
/// divisor `X`. The result is what nine steps on a 17-bit register
/// leave. `YA` goes in, and at each step the register is turned one
/// place to the left (the bit that leaves the top comes back at the
/// bottom), its lowest bit is flipped when the register holds at least
/// the divisor, which sits against bits 9 and up, and the divisor is
/// taken away when that bit is then set.
///
/// What is left is the quotient in the low nine bits, the ninth being
/// `V`, and the remainder above them. While the quotient fits in those
/// nine bits (`Y < 2X`) this is ordinary division: `A = YA / X`,
/// `Y = YA % X`. Past that it is whatever the nine steps leave, which
/// is what the chip leaves; a division by zero is such a case.
///
/// `H` compares the low digits of `Y` and `X` as they were before the
/// division; `N` and `Z` are the quotient byte's (`A`). `C` is never
/// touched.
pub fn div(cpu: &mut Spc700, _bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    const STEPS: u32 = 9;
    const REGISTER_BITS: u32 = 17;
    const REGISTER: u32 = (1 << REGISTER_BITS) - 1;

    let divisor = u32::from(cpu.x) << STEPS;
    let mut register = u32::from(cpu.ya());
    for _ in 0..STEPS {
        register = ((register << 1) | (register >> (REGISTER_BITS - 1))) & REGISTER;
        if register >= divisor {
            register ^= 1;
        }
        if register & 1 != 0 {
            register = register.wrapping_sub(divisor) & REGISTER;
        }
    }

    cpu.set_flag(FLAG_H, (cpu.y & 0x0F) >= (cpu.x & 0x0F));
    cpu.set_flag(FLAG_V, register & (1 << (STEPS - 1)) != 0);
    cpu.a = register as u8;
    cpu.y = (register >> STEPS) as u8;
    cpu.update_nz_u8(cpu.a);
    info.base_cycles as u32
}

/// Puts `A` back into two decimal digits after decimal numbers were
/// added or subtracted. A digit is corrected when it is out of range
/// or when the operation carried out of it, after a subtraction that
/// is a borrow, which the flags show as a *clear* `C` or `H`. The high
/// digit goes first, by `0x60`, and leaves `C` as such an operation
/// would; then the low digit, by 6. `H` is read and not written.
fn decimal_adjust(cpu: &mut Spc700, after_subtraction: bool) {
    let correct = |a: u8, by: u8| {
        if after_subtraction {
            a.wrapping_sub(by)
        } else {
            a.wrapping_add(by)
        }
    };
    let high_digit_carried = cpu.get_flag(FLAG_C) != after_subtraction;
    if high_digit_carried || cpu.a > 0x99 {
        cpu.a = correct(cpu.a, 0x60);
        cpu.set_flag(FLAG_C, !after_subtraction);
    }
    let low_digit_carried = cpu.get_flag(FLAG_H) != after_subtraction;
    if low_digit_carried || cpu.a & 0x0F > 0x09 {
        cpu.a = correct(cpu.a, 0x06);
    }
    cpu.update_nz_u8(cpu.a);
}

/// `DAA` (`$DF`): `decimal_adjust` after an addition.
pub fn daa(cpu: &mut Spc700, _bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    decimal_adjust(cpu, false);
    info.base_cycles as u32
}

/// `DAS` (`$BE`): `decimal_adjust` after a subtraction.
pub fn das(cpu: &mut Spc700, _bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    decimal_adjust(cpu, true);
    info.base_cycles as u32
}

// Silence the "unused" warning for `AddressingMode`: it stays in the
// `use` list to document which mode constants the helpers in
// `binary_ops` consume on our behalf.
const _: Option<AddressingMode> = None;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::{FLAG_C, FLAG_H, FLAG_N, FLAG_V, FLAG_Z};
    use crate::Spc700;

    fn cpu_with_flags(c: bool, h: bool) -> Spc700 {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        cpu.set_flag(FLAG_C, c);
        cpu.set_flag(FLAG_H, h);
        cpu
    }

    // -- ADC --------------------------------------------------------------

    #[test]
    fn adc_carry_in_propagates() {
        let mut cpu = cpu_with_flags(true, false);
        let r = add_with_carry(&mut cpu, 0x01, 0x01);
        assert_eq!(r, 0x03, "1 + 1 + C(1) = 3");
        assert!(!cpu.get_flag(FLAG_C));
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(!cpu.get_flag(FLAG_N));
    }

    #[test]
    fn adc_half_carry_at_nibble_boundary() {
        let mut cpu = cpu_with_flags(false, false);
        let _ = add_with_carry(&mut cpu, 0x0F, 0x01);
        assert!(
            cpu.get_flag(FLAG_H),
            "0x0F + 0x01 must trigger H (carry into bit 4)"
        );
        let mut cpu = cpu_with_flags(false, false);
        let _ = add_with_carry(&mut cpu, 0x10, 0x01);
        assert!(!cpu.get_flag(FLAG_H), "0x10 + 0x01 has no nibble carry");
    }

    #[test]
    fn adc_overflow_signed_max_plus_one() {
        let mut cpu = cpu_with_flags(false, false);
        let r = add_with_carry(&mut cpu, 0x7F, 0x01);
        assert_eq!(r, 0x80);
        assert!(
            cpu.get_flag(FLAG_V),
            "0x7F + 0x01 = 0x80 is signed overflow"
        );
        assert!(cpu.get_flag(FLAG_N));
        assert!(!cpu.get_flag(FLAG_C));
    }

    #[test]
    fn adc_unsigned_carry_out() {
        let mut cpu = cpu_with_flags(false, false);
        let r = add_with_carry(&mut cpu, 0xFF, 0x01);
        assert_eq!(r, 0x00);
        assert!(cpu.get_flag(FLAG_C), "0xFF + 0x01 wraps and sets C");
        assert!(cpu.get_flag(FLAG_Z));
        assert!(
            !cpu.get_flag(FLAG_V),
            "two unsigned overflows is not signed overflow"
        );
    }

    // -- SBC --------------------------------------------------------------

    #[test]
    fn sbc_inverted_carry_borrow() {
        // C=1 means "no borrow in" (6502 convention).
        let mut cpu = cpu_with_flags(true, false);
        let r = subtract_with_borrow(&mut cpu, 0x10, 0x05);
        assert_eq!(r, 0x0B, "0x10 - 0x05 - !1 = 0x0B");
        assert!(cpu.get_flag(FLAG_C), "no borrow out");

        // C=0 means "borrow in" subtracts an extra 1.
        let mut cpu = cpu_with_flags(false, false);
        let r = subtract_with_borrow(&mut cpu, 0x10, 0x05);
        assert_eq!(r, 0x0A);
        assert!(cpu.get_flag(FLAG_C));
    }

    #[test]
    fn sbc_h_flag_inverted_vs_adc() {
        // After SBC, H is set ONLY when the low digit did NOT need to
        // borrow from bit 4. 0x18 - 0x05 = 0x13, low digit 8-5=3, no
        // borrow, so H=1.
        let mut cpu = cpu_with_flags(true, false);
        let _ = subtract_with_borrow(&mut cpu, 0x18, 0x05);
        assert!(cpu.get_flag(FLAG_H), "0x18 - 0x05 no nibble borrow -> H=1");

        // 0x10 - 0x01: low nibble 0-1 underflows -> nibble borrow -> H=0.
        let mut cpu = cpu_with_flags(true, false);
        let _ = subtract_with_borrow(&mut cpu, 0x10, 0x01);
        assert!(!cpu.get_flag(FLAG_H), "0x10 - 0x01 nibble borrow -> H=0");
    }

    #[test]
    fn sbc_borrow_out_clears_c() {
        let mut cpu = cpu_with_flags(true, false);
        let r = subtract_with_borrow(&mut cpu, 0x05, 0x10);
        assert_eq!(r, 0xF5, "0x05 - 0x10 - !1 wraps");
        assert!(!cpu.get_flag(FLAG_C), "borrow out -> C=0");
        assert!(cpu.get_flag(FLAG_N));
    }

    // -- ADDW / SUBW ------------------------------------------------------

    #[test]
    fn addw_does_not_take_the_carry_in() {
        let mut cpu = cpu_with_flags(true, false);
        // 0x0001 + 0x0001 should be 0x0002 even though C was 1 going in.
        let r = add_words(&mut cpu, 0x0001, 0x0001);
        assert_eq!(
            r, 0x0002,
            "ADDW must NOT honour pre-existing C on the low byte"
        );
    }

    #[test]
    fn addw_z_reflects_full_16_bit_result() {
        let mut cpu = cpu_with_flags(false, false);
        let r = add_words(&mut cpu, 0xFFFF, 0x0001);
        assert_eq!(r, 0x0000);
        assert!(cpu.get_flag(FLAG_Z), "full 16-bit zero sets Z");
        assert!(cpu.get_flag(FLAG_C), "carry out of high byte sets C");
    }

    #[test]
    fn subw_does_not_take_a_borrow_in() {
        let mut cpu = cpu_with_flags(false, false);
        // 0x0002 - 0x0001 should be 0x0001 (no borrow), even with C=0.
        let r = subtract_words(&mut cpu, 0x0002, 0x0001);
        assert_eq!(
            r, 0x0001,
            "SUBW must ignore pre-existing C and force borrow-in cleared"
        );
        assert!(cpu.get_flag(FLAG_C));
    }

    // -- INC / DEC --------------------------------------------------------

    #[test]
    fn inc_does_not_touch_carry() {
        let mut cpu = cpu_with_flags(false, true);
        cpu.set_flag(FLAG_V, true);
        let r = incremented(&mut cpu, 0xFF);
        assert_eq!(r, 0x00);
        assert!(cpu.get_flag(FLAG_Z));
        // C/V/H must be unchanged.
        assert!(!cpu.get_flag(FLAG_C));
        assert!(cpu.get_flag(FLAG_V));
        assert!(cpu.get_flag(FLAG_H));
    }

    #[test]
    fn dec_does_not_touch_carry() {
        let mut cpu = cpu_with_flags(true, false);
        cpu.set_flag(FLAG_V, true);
        let r = decremented(&mut cpu, 0x00);
        assert_eq!(r, 0xFF);
        assert!(cpu.get_flag(FLAG_N));
        assert!(cpu.get_flag(FLAG_C));
        assert!(cpu.get_flag(FLAG_V));
    }

    // -- MUL / DIV --------------------------------------------------------

    #[test]
    fn mul_ya_flags_from_high_byte_only() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        cpu.y = 0x05;
        cpu.a = 0x10;
        let _ = mul(&mut cpu, &mut DummyBus, &dummy_info(9));
        // 0x05 * 0x10 = 0x0050  -> Y=0x00, A=0x50
        assert_eq!(cpu.y, 0x00);
        assert_eq!(cpu.a, 0x50);
        assert!(cpu.get_flag(FLAG_Z), "high byte is 0 -> Z");
        assert!(!cpu.get_flag(FLAG_N));
    }

    #[test]
    fn mul_ya_negative_high_byte_sets_n() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        cpu.y = 0x80;
        cpu.a = 0x02;
        let _ = mul(&mut cpu, &mut DummyBus, &dummy_info(9));
        // 0x80 * 0x02 = 0x0100  -> Y=0x01, A=0x00, N from Y is 0.
        assert_eq!(cpu.y, 0x01);
        assert!(!cpu.get_flag(FLAG_N));

        // Force a high byte with bit 7 set.
        cpu.y = 0x10;
        cpu.a = 0xFF;
        let _ = mul(&mut cpu, &mut DummyBus, &dummy_info(9));
        // 0x10 * 0xFF = 0x0FF0 -> Y=0x0F still no N. Try 0x80 * 0xFF = 0x7F80 -> Y=0x7F.
        // To actually trigger N, need Y_high >= 0x80 of product.
        cpu.y = 0x80;
        cpu.a = 0xFF;
        let _ = mul(&mut cpu, &mut DummyBus, &dummy_info(9));
        // 0x80 * 0xFF = 0x7F80 -> Y=0x7F, A=0x80. N from Y(0x7F) = 0.
        // Use Y=0xFF, A=0xFF -> 0xFE01 -> Y=0xFE: N=1.
        cpu.y = 0xFF;
        cpu.a = 0xFF;
        let _ = mul(&mut cpu, &mut DummyBus, &dummy_info(9));
        assert_eq!(cpu.y, 0xFE);
        assert!(cpu.get_flag(FLAG_N));
    }

    #[test]
    fn div_ya_x_overflow_when_y_ge_x() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        // Y=0x10, A=0x00 (YA = 0x1000); X = 0x08. quotient = 0x200 -> overflow.
        cpu.y = 0x10;
        cpu.a = 0x00;
        cpu.x = 0x08;
        let _ = div(&mut cpu, &mut DummyBus, &dummy_info(12));
        assert!(cpu.get_flag(FLAG_V), "Y >= X must set V");
    }

    #[test]
    fn div_ya_x_h_flag_from_low_nibbles() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        // Y nibble (0x05) < X nibble (0x07) -> H=0
        cpu.y = 0x05;
        cpu.a = 0x00;
        cpu.x = 0x07;
        let _ = div(&mut cpu, &mut DummyBus, &dummy_info(12));
        assert!(!cpu.get_flag(FLAG_H));

        // Y nibble (0x08) >= X nibble (0x03) -> H=1, no overflow because Y(0x08) < X(0x13)*2=0x26.
        cpu.y = 0x08;
        cpu.a = 0x00;
        cpu.x = 0x13;
        let _ = div(&mut cpu, &mut DummyBus, &dummy_info(12));
        assert!(cpu.get_flag(FLAG_H));
    }

    #[test]
    fn div_ya_x_normal_path_quotient_and_remainder() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        // YA = 100, X = 7. quotient = 14, remainder = 2.
        cpu.y = 0;
        cpu.a = 100;
        cpu.x = 7;
        let _ = div(&mut cpu, &mut DummyBus, &dummy_info(12));
        assert_eq!(cpu.a, 14);
        assert_eq!(cpu.y, 2);
    }

    // -- DAA / DAS --------------------------------------------------------

    #[test]
    fn daa_high_nibble_correction_sets_carry() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        cpu.a = 0xA5; // > 0x99 triggers +0x60 and sets C
        let _ = daa(&mut cpu, &mut DummyBus, &dummy_info(3));
        assert_eq!(cpu.a, 0xA5u8.wrapping_add(0x60));
        assert!(cpu.get_flag(FLAG_C));
    }

    #[test]
    fn daa_low_nibble_correction_uses_h_flag() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        cpu.set_flag(FLAG_H, true);
        cpu.a = 0x40; // low nibble 0, but H is set -> +0x06
        let _ = daa(&mut cpu, &mut DummyBus, &dummy_info(3));
        assert_eq!(cpu.a, 0x46);
    }

    #[test]
    fn das_preserves_h_flag() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        cpu.set_flag(FLAG_C, true);
        cpu.set_flag(FLAG_H, true);
        cpu.a = 0x50;
        let _ = das(&mut cpu, &mut DummyBus, &dummy_info(3));
        assert!(cpu.get_flag(FLAG_H), "DAS reads H but must not clear it");
    }

    // -- The adder at word width ------------------------------------------

    #[test]
    fn addw_half_carry_is_the_carry_out_of_bit_11() {
        let mut cpu = cpu_with_flags(false, false);
        let _ = add_words(&mut cpu, 0x0FFF, 0x0001);
        assert!(cpu.get_flag(FLAG_H));

        let mut cpu = cpu_with_flags(false, true);
        let _ = add_words(&mut cpu, 0x00FF, 0x0001);
        assert!(
            !cpu.get_flag(FLAG_H),
            "a carry out of bit 7 is not the word's half carry"
        );
    }

    #[test]
    fn addw_overflow_and_sign_are_bit_15s() {
        let mut cpu = cpu_with_flags(false, false);
        let r = add_words(&mut cpu, 0x7FFF, 0x0001);
        assert_eq!(r, 0x8000);
        assert!(cpu.get_flag(FLAG_V));
        assert!(cpu.get_flag(FLAG_N));
        assert!(!cpu.get_flag(FLAG_C));
    }

    #[test]
    fn subw_flags_say_what_was_not_borrowed() {
        let mut cpu = cpu_with_flags(false, false);
        let r = subtract_words(&mut cpu, 0x1000, 0x0001);
        assert_eq!(r, 0x0FFF);
        assert!(cpu.get_flag(FLAG_C), "nothing borrowed out of the word");
        assert!(!cpu.get_flag(FLAG_H), "bit 12 lent to the bits below it");

        let mut cpu = cpu_with_flags(true, true);
        let r = subtract_words(&mut cpu, 0x0000, 0x0001);
        assert_eq!(r, 0xFFFF);
        assert!(!cpu.get_flag(FLAG_C));
        assert!(!cpu.get_flag(FLAG_H));
        assert!(cpu.get_flag(FLAG_N));

        let mut cpu = cpu_with_flags(false, false);
        let r = subtract_words(&mut cpu, 0x8000, 0x0001);
        assert_eq!(r, 0x7FFF);
        assert!(
            cpu.get_flag(FLAG_V),
            "the most negative word less one overflows"
        );
    }

    // -- INCW / DECW on the bus -------------------------------------------

    /// A page of memory that remembers what was done to it.
    struct RecordingBus {
        mem: [u8; 0x100],
        accesses: Vec<(char, u32, u8)>,
    }
    impl Addressable for RecordingBus {
        fn read(&mut self, addr: u32) -> u8 {
            let value = self.mem[addr as usize];
            self.accesses.push(('r', addr, value));
            value
        }
        fn write(&mut self, addr: u32, value: u8) {
            self.mem[addr as usize] = value;
            self.accesses.push(('w', addr, value));
        }
    }

    /// Runs `INCW $20` or `DECW $20` over the word given; the operand
    /// byte sits at `$80`.
    fn step_the_word_at_20(low: u8, high: u8, up: bool) -> (Spc700, RecordingBus) {
        let mut cpu = Spc700::new();
        cpu.psw = FLAG_C | FLAG_V | FLAG_H;
        cpu.pc = 0x0080;
        let mut bus = RecordingBus {
            mem: [0; 0x100],
            accesses: Vec::new(),
        };
        bus.mem[0x80] = 0x20;
        bus.mem[0x20] = low;
        bus.mem[0x21] = high;
        let _ = if up {
            incw(&mut cpu, &mut bus, &dummy_info(6))
        } else {
            decw(&mut cpu, &mut bus, &dummy_info(6))
        };
        (cpu, bus)
    }

    #[test]
    fn incw_writes_the_low_byte_back_before_it_reads_the_high_one() {
        let (cpu, bus) = step_the_word_at_20(0xFF, 0x01, true);
        assert_eq!(
            bus.accesses,
            [
                ('r', 0x80, 0x20),
                ('r', 0x20, 0xFF),
                ('w', 0x20, 0x00),
                ('r', 0x21, 0x01),
                ('w', 0x21, 0x02),
            ]
        );
        assert!(!cpu.get_flag(FLAG_N));
        assert!(!cpu.get_flag(FLAG_Z));
        assert_eq!(
            cpu.psw & (FLAG_C | FLAG_V | FLAG_H),
            FLAG_C | FLAG_V | FLAG_H,
            "C, V and H stay as they were"
        );
    }

    #[test]
    fn incw_writes_the_high_byte_back_when_the_low_one_did_not_wrap() {
        let (_, bus) = step_the_word_at_20(0x05, 0x40, true);
        assert_eq!(
            bus.accesses[1..],
            [
                ('r', 0x20, 0x05),
                ('w', 0x20, 0x06),
                ('r', 0x21, 0x40),
                ('w', 0x21, 0x40),
            ]
        );
    }

    #[test]
    fn decw_borrows_from_the_high_byte_and_flags_the_whole_word() {
        let (cpu, bus) = step_the_word_at_20(0x00, 0x00, false);
        assert_eq!(bus.mem[0x20], 0xFF);
        assert_eq!(bus.mem[0x21], 0xFF);
        assert!(cpu.get_flag(FLAG_N));
        assert!(!cpu.get_flag(FLAG_Z));

        let (cpu, bus) = step_the_word_at_20(0x01, 0x00, false);
        assert_eq!((bus.mem[0x20], bus.mem[0x21]), (0x00, 0x00));
        assert!(cpu.get_flag(FLAG_Z), "Z is the word's, not a byte's");

        let (cpu, _) = step_the_word_at_20(0xFF, 0xFF, true);
        assert!(cpu.get_flag(FLAG_Z), "0xFFFF + 1 wraps to zero");
    }

    // -- DIV past nine bits -----------------------------------------------

    #[test]
    fn div_ya_x_ninth_quotient_bit_is_v() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        // YA = 0x0300, X = 2: the quotient is 0x180, its low byte in A
        // and its ninth bit in V.
        cpu.y = 0x03;
        cpu.a = 0x00;
        cpu.x = 0x02;
        let _ = div(&mut cpu, &mut DummyBus, &dummy_info(12));
        assert_eq!(cpu.a, 0x80);
        assert_eq!(cpu.y, 0x00);
        assert!(cpu.get_flag(FLAG_V));
        assert!(cpu.get_flag(FLAG_N), "N is the quotient byte's");
    }

    #[test]
    fn div_ya_x_past_nine_bits_leaves_what_the_nine_steps_leave() {
        // (Y, A, X) and the (A, Y) the instruction leaves when the
        // quotient does not fit in nine bits, a division by zero is
        // one such case.
        for (y, a, x, a_after, y_after) in [
            (0x10u8, 0x00u8, 0x08u8, 0xFFu8, 0x08u8),
            (0xFF, 0xFF, 0x01, 0x01, 0xFE),
            (0x12, 0x34, 0x00, 0xED, 0x34),
        ] {
            let mut cpu = Spc700::new();
            cpu.psw = 0;
            (cpu.y, cpu.a, cpu.x) = (y, a, x);
            let _ = div(&mut cpu, &mut DummyBus, &dummy_info(12));
            assert_eq!(
                (cpu.a, cpu.y),
                (a_after, y_after),
                "YA={y:02X}{a:02X} X={x:02X}"
            );
            assert!(cpu.get_flag(FLAG_V), "YA={y:02X}{a:02X} X={x:02X}");
        }
    }

    #[test]
    fn das_takes_the_digits_back_down() {
        // 0x42 - 0x15 in decimal: SBC leaves 0x2D with H clear (the low
        // digit borrowed) and C set; DAS turns it into 27.
        let mut cpu = Spc700::new();
        cpu.psw = FLAG_C;
        cpu.a = 0x2D;
        let _ = das(&mut cpu, &mut DummyBus, &dummy_info(3));
        assert_eq!(cpu.a, 0x27);
        assert!(cpu.get_flag(FLAG_C), "no borrow out of the high digit");
    }

    // -- Test fixtures ----------------------------------------------------

    struct DummyBus;
    impl Addressable for DummyBus {
        fn read(&mut self, _: u32) -> u8 {
            0
        }
        fn write(&mut self, _: u32, _: u8) {}
    }

    fn dummy_info(base_cycles: u8) -> OpcodeInfo {
        OpcodeInfo {
            mnemonic: crate::decode::Mnemonic::NOP,
            mode: AddressingMode::Implied,
            base_cycles,
        }
    }
}
