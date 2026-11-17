//! SPC7110 arithmetic unit (`$4820-$482F`).
//!
//! A byte-faithful port of the math unit of the SPC7110 emulator by byuu
//! and neviksti (its notice is at the end of `LICENSE`), read in the copy
//! bsnes-plus carries. The unit does two operations, each
//! triggered by the write that completes the last operand byte:
//!
//! - **Multiply** (`$4825` write): 16x16 -> 32-bit. Operands are
//!   `$4824|$4825<<8` and `$4820|$4821<<8`; the 32-bit product lands in
//!   `$4828-$482B` (little-endian).
//! - **Divide** (`$4827` write): 32/16 -> 32-bit quotient + 16-bit remainder.
//!   The dividend is `$4820-$4823` (little-endian), the divisor
//!   `$4826|$4827<<8`; the quotient lands in `$4828-$482B`, the remainder in
//!   `$482C-$482D`. Division by zero yields quotient `0` and remainder = the
//!   low 16 bits of the dividend (the documented "illegal" result).
//!
//! `$482E bit0` selects signed vs unsigned arithmetic; a write to `$482E` also
//! **resets** the operand + result + remainder latches (but not the status
//! flag). `$482F bit7` is set when an operation completes and cleared when the
//! CPU reads `$482F` (a debugger read must not clear it).
//!
//! bsnes computes in C `int`/`unsigned` with two's-complement wrap; the Rust
//! port uses `wrapping_*` so the pathological `i32::MIN / -1` case matches the
//! hardware/bsnes result instead of panicking.

use crate::savestate::Spc7110AluState;

/// The SPC7110 ALU register file and computed results.
///
/// Owns only the `$4820-$482F` window; the hosting [`Spc7110Chip`] routes that
/// range here and keeps the other units' registers itself.
#[derive(Clone, Default)]
pub(super) struct Spc7110Alu {
    /// `$4820-$4827` operand latches (multiplicand/dividend + multiplier /
    /// divisor), indexed by `reg - 0x4820`.
    operands: [u8; 8],
    /// `$4828-$482B` result (product or quotient), little-endian.
    result: [u8; 4],
    /// `$482C-$482D` remainder (division only), little-endian.
    remainder: [u8; 2],
    /// `$482E` mode latch, bit 0 selects signed arithmetic.
    mode: u8,
    /// `$482F` status, bit 7 set on completion, cleared on a (non-debugger)
    /// read.
    status: u8,
}

impl Spc7110Alu {
    /// Read a register without side effects (the debugger path, and the value
    /// half of the CPU read). `$482F` returns the status byte *without*
    /// clearing bit 7, use [`Self::read_port`] for the CPU read.
    pub(super) fn read(&self, reg: u16) -> u8 {
        match reg {
            0x4820..=0x4827 => self.operands[(reg - 0x4820) as usize],
            0x4828..=0x482b => self.result[(reg - 0x4828) as usize],
            0x482c..=0x482d => self.remainder[(reg - 0x482c) as usize],
            0x482e => self.mode,
            0x482f => self.status,
            _ => 0,
        }
    }

    /// CPU read of a register. Identical to [`Self::read`] except a read of the
    /// status port `$482F` clears bit 7 (the hardware's read-to-clear).
    pub(super) fn read_port(&mut self, reg: u16) -> u8 {
        let value = self.read(reg);
        if reg == 0x482f {
            self.status &= 0x7f;
        }
        value
    }

    /// CPU write of a register. Storing the final operand byte (`$4825` /
    /// `$4827`) triggers the multiply / divide; a `$482E` write resets the
    /// unit. The result / remainder / status ports are read-only.
    pub(super) fn write(&mut self, reg: u16, value: u8) {
        match reg {
            0x4820..=0x4824 | 0x4826 => self.operands[(reg - 0x4820) as usize] = value,
            0x4825 => {
                self.operands[5] = value;
                self.multiply();
            }
            0x4827 => {
                self.operands[7] = value;
                self.divide();
            }
            0x482e => self.reset(value),
            // $4828-$482D (result/remainder) and $482F (status) are read-only.
            _ => {}
        }
    }

    /// 32-bit result latch (`$4828-$482B`), for the debug view.
    pub(super) fn result(&self) -> u32 {
        u32::from_le_bytes(self.result)
    }

    /// 16-bit remainder latch (`$482C-$482D`), for the debug view.
    pub(super) fn remainder(&self) -> u16 {
        u16::from_le_bytes(self.remainder)
    }

    /// `$482E` mode latch (bit 0 = signed), for the debug view.
    pub(super) fn mode(&self) -> u8 {
        self.mode
    }

    /// `$482F` status (bit 7 = op complete), for the debug view.
    pub(super) fn status(&self) -> u8 {
        self.status
    }

    /// Capture the volatile register file for the savestate.
    pub(super) fn snapshot(&self) -> Spc7110AluState {
        Spc7110AluState {
            operands: self.operands,
            result: self.result,
            remainder: self.remainder,
            mode: self.mode,
            status: self.status,
        }
    }

    /// Restore the register file from a savestate.
    pub(super) fn restore_state(&mut self, s: &Spc7110AluState) {
        self.operands = s.operands;
        self.result = s.result;
        self.remainder = s.remainder;
        self.mode = s.mode;
        self.status = s.status;
    }

    /// 16x16 -> 32-bit multiply of the `$4824/$4825` and `$4820/$4821` operand
    /// pairs, honouring the `$482E bit0` sign mode.
    fn multiply(&mut self) {
        let a = u16::from(self.operands[4]) | (u16::from(self.operands[5]) << 8);
        let b = u16::from(self.operands[0]) | (u16::from(self.operands[1]) << 8);
        let result = if self.signed() {
            ((a as i16 as i32).wrapping_mul(b as i16 as i32)) as u32
        } else {
            u32::from(a).wrapping_mul(u32::from(b))
        };
        self.result = result.to_le_bytes();
        self.status = 0x80;
    }

    /// 32/16 -> 32-bit quotient + 16-bit remainder, honouring the sign mode and
    /// the division-by-zero rule (quotient `0`, remainder = low 16 bits of the
    /// dividend).
    fn divide(&mut self) {
        let dividend = u32::from_le_bytes([
            self.operands[0],
            self.operands[1],
            self.operands[2],
            self.operands[3],
        ]);
        let divisor = u16::from(self.operands[6]) | (u16::from(self.operands[7]) << 8);

        let (quotient, remainder): (u32, u16) = if self.signed() {
            let dividend = dividend as i32;
            let divisor = divisor as i16 as i32;
            if divisor != 0 {
                let q = dividend.wrapping_div(divisor) as u32;
                let r = (dividend.wrapping_rem(divisor) as u32 & 0xffff) as u16;
                (q, r)
            } else {
                (0, (dividend as u32 & 0xffff) as u16)
            }
        } else {
            let divisor = u32::from(divisor);
            match (dividend.checked_div(divisor), dividend.checked_rem(divisor)) {
                (Some(q), Some(r)) => (q, r as u16),
                _ => (0, (dividend & 0xffff) as u16),
            }
        };

        self.result = quotient.to_le_bytes();
        self.remainder = remainder.to_le_bytes();
        self.status = 0x80;
    }

    /// Reset the operand / result / remainder latches and store the new mode
    /// byte. The status flag is preserved (matching bsnes).
    fn reset(&mut self, mode: u8) {
        self.operands = [0; 8];
        self.result = [0; 4];
        self.remainder = [0; 2];
        self.mode = mode;
    }

    /// `true` when `$482E bit0` selects signed arithmetic.
    fn signed(&self) -> bool {
        self.mode & 1 != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Load a 16-bit operand into a low/high register pair without triggering
    /// (the high byte of a trigger pair is written via the trigger helpers).
    fn set16(alu: &mut Spc7110Alu, lo: u16, value: u16) {
        alu.write(lo, value as u8);
        alu.write(lo + 1, (value >> 8) as u8);
    }

    /// Program both multiply operands, triggering on the `$4825` high byte.
    fn multiply(alu: &mut Spc7110Alu, a: u16, b: u16) -> u32 {
        // r1 = $4820/$4821, r0 = $4824/$4825 (trigger).
        alu.write(0x4820, b as u8);
        alu.write(0x4821, (b >> 8) as u8);
        alu.write(0x4824, a as u8);
        alu.write(0x4825, (a >> 8) as u8); // triggers
        alu.result()
    }

    /// Program a 32/16 divide, triggering on the `$4827` high byte.
    fn divide(alu: &mut Spc7110Alu, dividend: u32, divisor: u16) -> (u32, u16) {
        let d = dividend.to_le_bytes();
        alu.write(0x4820, d[0]);
        alu.write(0x4821, d[1]);
        alu.write(0x4822, d[2]);
        alu.write(0x4823, d[3]);
        alu.write(0x4826, divisor as u8);
        alu.write(0x4827, (divisor >> 8) as u8); // triggers
        (alu.result(), alu.remainder())
    }

    #[test]
    fn unsigned_multiply() {
        let mut alu = Spc7110Alu::default();
        assert_eq!(multiply(&mut alu, 0x1234, 0x0010), 0x1234 * 0x10);
        assert_eq!(multiply(&mut alu, 0xFFFF, 0xFFFF), 0xFFFE_0001);
    }

    #[test]
    fn signed_multiply() {
        let mut alu = Spc7110Alu::default();
        alu.write(0x482e, 0x01); // signed mode (also resets latches)
                                 // -2 * 3 = -6 -> 0xFFFFFFFA.
        assert_eq!(multiply(&mut alu, 0xFFFE, 0x0003), 0xFFFF_FFFA);
        // -32768 * -32768 = 0x40000000 (fits in i32).
        assert_eq!(multiply(&mut alu, 0x8000, 0x8000), 0x4000_0000);
    }

    #[test]
    fn unsigned_divide() {
        let mut alu = Spc7110Alu::default();
        assert_eq!(divide(&mut alu, 100, 7), (14, 2));
        assert_eq!(divide(&mut alu, 0xFFFF_FFFF, 0xFFFF), (0x0001_0001, 0));
    }

    #[test]
    fn signed_divide() {
        let mut alu = Spc7110Alu::default();
        alu.write(0x482e, 0x01); // signed
                                 // -100 / 7 = -14 rem -2 -> quotient 0xFFFFFFF2, remainder 0xFFFE.
        let (q, r) = divide(&mut alu, (-100_i32) as u32, 7);
        assert_eq!(q, (-14_i32) as u32);
        assert_eq!(r, (-2_i32 as u32 & 0xffff) as u16);
    }

    #[test]
    fn divide_by_zero_yields_zero_quotient_and_low_dividend_remainder() {
        let mut alu = Spc7110Alu::default();
        // Unsigned: quotient 0, remainder = low 16 bits of dividend.
        assert_eq!(divide(&mut alu, 0x1234_ABCD, 0), (0, 0xABCD));
        // Signed div-by-zero follows the same rule.
        alu.write(0x482e, 0x01);
        assert_eq!(divide(&mut alu, 0x1234_ABCD, 0), (0, 0xABCD));
    }

    #[test]
    fn signed_divide_min_by_neg_one_wraps_without_panicking() {
        let mut alu = Spc7110Alu::default();
        alu.write(0x482e, 0x01);
        // i32::MIN / -1 overflows; bsnes/C wrap to i32::MIN, remainder 0.
        let (q, r) = divide(&mut alu, i32::MIN as u32, 0xFFFF);
        assert_eq!(q, i32::MIN as u32);
        assert_eq!(r, 0);
    }

    #[test]
    fn status_sets_on_op_and_clears_on_read() {
        let mut alu = Spc7110Alu::default();
        assert_eq!(alu.read(0x482f), 0x00);
        multiply(&mut alu, 2, 3);
        assert_eq!(alu.read(0x482f), 0x80, "status set after multiply");
        // A non-clearing read leaves it set.
        assert_eq!(alu.read(0x482f), 0x80);
        // The CPU read-port clears bit 7.
        assert_eq!(alu.read_port(0x482f), 0x80);
        assert_eq!(alu.read_port(0x482f), 0x00, "bit 7 cleared on read");
    }

    #[test]
    fn reset_clears_operands_and_results_but_keeps_status() {
        let mut alu = Spc7110Alu::default();
        multiply(&mut alu, 0x00FF, 0x00FF);
        assert_ne!(alu.result(), 0);
        assert_eq!(alu.status(), 0x80);
        alu.write(0x482e, 0x00); // reset, unsigned
        assert_eq!(alu.result(), 0, "result cleared on reset");
        assert_eq!(alu.read(0x4820), 0, "operands cleared on reset");
        assert_eq!(alu.status(), 0x80, "status survives reset");
        assert_eq!(alu.mode(), 0x00);
    }

    #[test]
    fn operands_round_trip_before_trigger() {
        let mut alu = Spc7110Alu::default();
        // Writing $4820-$4824/$4826 stores without triggering.
        set16(&mut alu, 0x4820, 0xBEEF);
        assert_eq!(alu.read(0x4820), 0xEF);
        assert_eq!(alu.read(0x4821), 0xBE);
        alu.write(0x4826, 0x77);
        assert_eq!(alu.read(0x4826), 0x77);
        // Status still clear, no operation ran.
        assert_eq!(alu.status(), 0x00);
    }
}
