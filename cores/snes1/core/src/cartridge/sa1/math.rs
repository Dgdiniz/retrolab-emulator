//! The SA-1's arithmetic unit (`$2250-$2254` written, `$2306-$230B` read).
//!
//! Three operations, chosen by MCNT (`$2250`) and started by the write of
//! MB's high byte (`$2254`):
//!
//! - **Multiply** (MCNT bits 1 and 0 clear): MA x MB, both signed, a 32-bit
//!   signed product. MB is cleared.
//! - **Divide** (bit 0 set): MA, signed, divided by MB, unsigned. The
//!   remainder is never negative (the quotient rounds toward minus
//!   infinity), so a 16-bit quotient and a 16-bit remainder fill the low 32
//!   bits of the result. Dividing by zero gives zero. MA and MB are cleared.
//! - **Multiply and accumulate** (bit 1 set): MA x MB, signed, added to a
//!   40-bit sum. Writing MCNT with bit 1 set starts the sum again at zero.
//!   MB is cleared.
//!
//! The result reads at `$2306-$230A`, lowest byte first; `$230B` bit 7 says
//! whether the last addition to the sum overflowed 40 bits, a negative
//! product taking a sum of zero below zero included (the sum is unsigned and
//! wraps). Only an addition sets or clears it.
//!
//! Behaviour checked against bsnes-plus and ares, which agree.

use super::registers::{flag, pack, Bytes};
use crate::savestate::Sa1MathState;

/// The sum and the result are 40 bits wide.
const RESULT_MASK: u64 = (1 << 40) - 1;

/// The arithmetic unit's registers and its result.
#[derive(Clone, Default)]
pub(super) struct Sa1Math {
    /// MCNT (`$2250`) bit 1: add each product to the sum.
    accumulate: bool,
    /// MCNT bit 0: divide (when bit 1 is clear).
    divide: bool,
    /// MA (`$2251-$2252`): the multiplicand, or the dividend.
    operand_a: u16,
    /// MB (`$2253-$2254`): the multiplier, or the divisor.
    operand_b: u16,
    /// MR (`$2306-$230A`): the 40-bit result, product, quotient and
    /// remainder, or sum.
    result: u64,
    /// `$230B` bit 7: the last addition to the sum overflowed 40 bits.
    overflow: bool,
}

impl Sa1Math {
    /// A read of `$2306-$230B`; it changes nothing. The registers the program
    /// writes (`$2250-$2254`) read back as zero.
    pub(super) fn read(&self, reg: u16) -> u8 {
        match reg {
            0x2306..=0x230A => self.result.byte(usize::from(reg - 0x2306)),
            0x230B => u8::from(self.overflow) << 7,
            _ => 0,
        }
    }

    /// A write of `$2250-$2254`. The high byte of MB starts the operation.
    pub(super) fn write(&mut self, reg: u16, value: u8) {
        match reg {
            0x2250 => {
                self.accumulate = flag(value, 1);
                self.divide = flag(value, 0);
                if self.accumulate {
                    self.result = 0;
                }
            }
            0x2251 | 0x2252 => {
                self.operand_a = self.operand_a.with_byte(usize::from(reg - 0x2251), value)
            }
            0x2253 => self.operand_b = self.operand_b.with_byte(0, value),
            0x2254 => {
                self.operand_b = self.operand_b.with_byte(1, value);
                self.start();
            }
            _ => {}
        }
    }

    /// Everything back to zero, as at power-on.
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    /// MCNT as written (bits 1 and 0), for the debugger.
    pub(super) fn control_byte(&self) -> u8 {
        pack([(self.accumulate, 1), (self.divide, 0)], 0)
    }

    /// MA, for the debugger.
    pub(super) fn operand_a(&self) -> u16 {
        self.operand_a
    }

    /// MB, for the debugger.
    pub(super) fn operand_b(&self) -> u16 {
        self.operand_b
    }

    /// The 40-bit result, for the debugger.
    pub(super) fn result(&self) -> u64 {
        self.result
    }

    /// The overflow bit, for the debugger.
    pub(super) fn overflow(&self) -> bool {
        self.overflow
    }

    /// The unit for a savestate.
    pub(super) fn snapshot(&self) -> Sa1MathState {
        Sa1MathState {
            acm: self.accumulate,
            md: self.divide,
            ma: self.operand_a,
            mb: self.operand_b,
            mr: self.result,
            overflow: self.overflow,
        }
    }

    /// The unit from a savestate; the result is cut to its 40 bits, so a
    /// damaged savestate cannot carry a bit above them into the next
    /// overflow test.
    pub(super) fn restore_state(&mut self, s: &Sa1MathState) {
        self.accumulate = s.acm;
        self.divide = s.md;
        self.operand_a = s.ma;
        self.operand_b = s.mb;
        self.result = s.mr & RESULT_MASK;
        self.overflow = s.overflow;
    }

    /// The operation MCNT chose, as the write of MB's high byte starts it.
    fn start(&mut self) {
        if self.accumulate {
            self.add_product();
        } else if self.divide {
            self.quotient_and_remainder();
        } else {
            self.multiply();
        }
    }

    /// The signed product of MA and MB.
    fn signed_product(&self) -> i32 {
        i32::from(self.operand_a as i16) * i32::from(self.operand_b as i16)
    }

    /// The signed product, its 32 bits in the low half of the result.
    fn multiply(&mut self) {
        self.result = u64::from(self.signed_product() as u32);
        self.operand_b = 0;
    }

    /// MA (signed) over MB (unsigned): the quotient in the low 16 bits, the
    /// remainder, between zero and the divisor, in the 16 above.
    fn quotient_and_remainder(&mut self) {
        self.result = match i32::from(self.operand_b) {
            0 => 0,
            divisor => {
                let dividend = i32::from(self.operand_a as i16);
                let quotient = dividend.div_euclid(divisor) as u16;
                let remainder = dividend.rem_euclid(divisor) as u16;
                u64::from(remainder) << 16 | u64::from(quotient)
            }
        };
        self.operand_a = 0;
        self.operand_b = 0;
    }

    /// The signed product added to the 40-bit sum, which wraps as an
    /// unsigned number; whether the sum left the 40 bits is the overflow.
    fn add_product(&mut self) {
        let sum = self
            .result
            .wrapping_add(i64::from(self.signed_product()) as u64);
        self.overflow = sum > RESULT_MASK;
        self.result = sum & RESULT_MASK;
        self.operand_b = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Program MA (`$2251/$2252`) without triggering.
    fn set_ma(m: &mut Sa1Math, v: u16) {
        m.write(0x2251, v as u8);
        m.write(0x2252, (v >> 8) as u8);
    }

    /// Program MB (`$2253/$2254`); the `$2254` high byte triggers the op.
    fn set_mb_trigger(m: &mut Sa1Math, v: u16) {
        m.write(0x2253, v as u8);
        m.write(0x2254, (v >> 8) as u8);
    }

    /// Run a multiply (`acm=0, md=0`) and return the 40-bit result.
    fn multiply(m: &mut Sa1Math, a: u16, b: u16) -> u64 {
        m.write(0x2250, 0x00);
        set_ma(m, a);
        set_mb_trigger(m, b);
        m.result()
    }

    /// Run a divide (`acm=0, md=1`) and return `(quotient, remainder)`.
    fn divide(m: &mut Sa1Math, a: u16, b: u16) -> (u16, u16) {
        m.write(0x2250, 0x01);
        set_ma(m, a);
        set_mb_trigger(m, b);
        let mr = m.result();
        (mr as u16, (mr >> 16) as u16)
    }

    #[test]
    fn signed_multiply_matches_i16_product() {
        let mut m = Sa1Math::default();
        // Positive x positive.
        assert_eq!(multiply(&mut m, 0x1234, 0x0010), 0x1234u64 * 0x10);
        // -2 x 3 = -6 -> 0xFFFF_FFFA, zero-extended into the 40-bit mr.
        assert_eq!(multiply(&mut m, 0xFFFE, 0x0003), 0xFFFF_FFFA);
        // Oracle: recompute independently from the operands.
        let oracle = ((0xFFFEu16 as i16 as i64) * (0x0003i16 as i64)) as u32 as u64;
        assert_eq!(multiply(&mut m, 0xFFFE, 0x0003), oracle);
        // -32768 x -32768 = 0x4000_0000 (fits in i32, no top byte).
        assert_eq!(multiply(&mut m, 0x8000, 0x8000), 0x4000_0000);
    }

    #[test]
    fn multiply_top_byte_is_zero_and_resets_only_mb() {
        let mut m = Sa1Math::default();
        set_ma(&mut m, 0x00FF);
        set_mb_trigger(&mut m, 0x00FF);
        // Product fits in 32 bits -> mr bits 32-39 (read at $230A) are 0.
        assert_eq!(m.read(0x230A), 0);
        // Multiply resets MB but leaves MA intact.
        assert_eq!(m.operand_b(), 0, "multiply resets MB");
        assert_eq!(m.operand_a(), 0x00FF, "multiply leaves MA");
    }

    #[test]
    fn unsigned_divisor_signed_dividend_euclidean_remainder() {
        let mut m = Sa1Math::default();
        // 100 / 7 = 14 rem 2.
        assert_eq!(divide(&mut m, 100, 7), (14, 2));
        // -100 / 7: euclidean -> quotient -15 (0xFFF1), remainder 5 (>= 0).
        let (q, r) = divide(&mut m, (-100i16) as u16, 7);
        assert_eq!(q, (-15i16) as u16);
        assert_eq!(r, 5, "euclidean remainder is always >= 0");
        // Oracle check: q*divisor + r == dividend, 0 <= r < divisor.
        assert_eq!((q as i16 as i32) * 7 + i32::from(r), -100);
        assert!(r < 7);
    }

    #[test]
    fn divide_resets_ma_and_mb() {
        let mut m = Sa1Math::default();
        divide(&mut m, 0x0064, 7);
        assert_eq!(m.operand_a(), 0, "division resets MA");
        assert_eq!(m.operand_b(), 0, "division resets MB");
    }

    #[test]
    fn divide_by_zero_yields_zero() {
        let mut m = Sa1Math::default();
        assert_eq!(divide(&mut m, 0x1234, 0), (0, 0));
        // MA/MB still reset even on the div-by-zero path.
        assert_eq!(m.operand_a(), 0);
        assert_eq!(m.operand_b(), 0);
    }

    #[test]
    fn divide_min_dividend_by_one() {
        let mut m = Sa1Math::default();
        // -32768 / 1 = -32768 rem 0, the unsigned divisor keeps this safe.
        let (q, r) = divide(&mut m, 0x8000, 1);
        assert_eq!(q, 0x8000);
        assert_eq!(r, 0);
    }

    #[test]
    fn mcnt_with_acm_zeroes_the_accumulator() {
        let mut m = Sa1Math::default();
        multiply(&mut m, 0x00FF, 0x00FF); // leaves mr non-zero
        assert_ne!(m.result(), 0);
        m.write(0x2250, 0x02); // MCNT acm=1 -> clears mr
        assert_eq!(
            m.result(),
            0,
            "writing MCNT with acm=1 resets the accumulator"
        );
    }

    #[test]
    fn mac_accumulates_and_resets_only_mb() {
        let mut m = Sa1Math::default();
        m.write(0x2250, 0x02); // acm=1, mr=0
        set_ma(&mut m, 3);
        set_mb_trigger(&mut m, 4); // mr += 12
        assert_eq!(m.result(), 12);
        assert!(!m.overflow());
        assert_eq!(m.operand_b(), 0, "MAC resets MB");
        assert_eq!(m.operand_a(), 3, "MAC leaves MA");
        // Accumulate again without re-arming MCNT (mr keeps summing).
        set_mb_trigger(&mut m, 4); // mr += 12 -> 24
        assert_eq!(m.result(), 24);
    }

    #[test]
    fn mac_negative_product_wraps_and_flags_overflow() {
        let mut m = Sa1Math::default();
        m.write(0x2250, 0x02); // acm=1, mr=0
        set_ma(&mut m, 1);
        set_mb_trigger(&mut m, 0xFFFF); // product = 1 * -1 = -1
                                        // The uint64 wrap of 0 + (-1) is huge (>= 2^40) -> overflow, masked to 40 bits.
        assert_eq!(
            m.result(),
            RESULT_MASK,
            "0 - 1 wraps to 2^40 - 1 after the 40-bit mask"
        );
        assert!(m.overflow(), "the negative wrap trips the overflow flag");
        assert_eq!(m.read(0x230B), 0x80);
    }

    #[test]
    fn mac_positive_overflow_wraps_past_2_40() {
        let mut m = Sa1Math::default();
        m.write(0x2250, 0x02); // acm=1, mr=0
                               // Drive mr to 2^40 - 1 via the negative-wrap trick.
        set_ma(&mut m, 1);
        set_mb_trigger(&mut m, 0xFFFF);
        assert_eq!(m.result(), RESULT_MASK);
        // Add 2 -> un-masked sum 2^40 + 1 -> overflow, mr masks to 1.
        set_ma(&mut m, 1);
        set_mb_trigger(&mut m, 2);
        assert_eq!(m.result(), 1);
        assert!(m.overflow());
    }

    #[test]
    fn mac_overflow_is_recomputed_not_sticky() {
        let mut m = Sa1Math::default();
        m.write(0x2250, 0x02);
        set_ma(&mut m, 1);
        set_mb_trigger(&mut m, 0xFFFF); // overflow = true
        assert!(m.overflow());
        // A fresh accumulation whose sum stays under 2^40 clears the flag.
        m.write(0x2250, 0x02); // reset mr (acm stays 1); overflow untouched here
        assert!(m.overflow(), "MCNT write does not touch the overflow flag");
        set_ma(&mut m, 2);
        set_mb_trigger(&mut m, 3); // mr = 6 < 2^40
        assert!(!m.overflow(), "the next MAC recomputes overflow to false");
        assert_eq!(m.result(), 6);
    }

    #[test]
    fn result_reads_little_endian_across_five_bytes() {
        let mut m = Sa1Math::default();
        m.write(0x2250, 0x02);
        set_ma(&mut m, 1);
        set_mb_trigger(&mut m, 0xFFFF); // mr = 0xFF_FFFF_FFFF
        assert_eq!(m.read(0x2306), 0xFF);
        assert_eq!(m.read(0x2307), 0xFF);
        assert_eq!(m.read(0x2308), 0xFF);
        assert_eq!(m.read(0x2309), 0xFF);
        assert_eq!(m.read(0x230A), 0xFF);
        // Write-only operand ports read back 0.
        assert_eq!(m.read(0x2250), 0);
        assert_eq!(m.read(0x2254), 0);
    }

    #[test]
    fn reset_clears_all_state() {
        let mut m = Sa1Math::default();
        m.write(0x2250, 0x02);
        set_ma(&mut m, 1);
        set_mb_trigger(&mut m, 0xFFFF); // mr + overflow set
        m.reset();
        assert_eq!(m.result(), 0);
        assert_eq!(m.operand_a(), 0);
        assert_eq!(m.operand_b(), 0);
        assert_eq!(m.control_byte(), 0);
        assert!(!m.overflow());
    }

    #[test]
    fn snapshot_round_trips() {
        let mut m = Sa1Math::default();
        m.write(0x2250, 0x02); // acm=1
        set_ma(&mut m, 0x1234);
        set_mb_trigger(&mut m, 0x0002); // MAC -> mr = 0x2468, mb reset
        set_ma(&mut m, 0x00AB); // leave MA loaded (not triggered)
        let snap = m.snapshot();
        let mut restored = Sa1Math::default();
        restored.restore_state(&snap);
        assert_eq!(restored.control_byte(), m.control_byte());
        assert_eq!(restored.operand_a(), m.operand_a());
        assert_eq!(restored.operand_b(), m.operand_b());
        assert_eq!(restored.result(), m.result());
        assert_eq!(restored.overflow(), m.overflow());
    }

    #[test]
    fn control_byte_packs_acm_and_md() {
        let mut m = Sa1Math::default();
        m.write(0x2250, 0x03); // acm=1 (bit1), md=1 (bit0)
        assert_eq!(m.control_byte(), 0x03);
        m.write(0x2250, 0x01); // md only
        assert_eq!(m.control_byte(), 0x01);
    }
}
