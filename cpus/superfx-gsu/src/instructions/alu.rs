//! Arithmetic, logic, shifts and the byte operations.
//!
//! An operation reads the source register (FROM, R0 by default) and writes
//! the destination (TO, R0 by default). In the four arithmetic families
//! the second operand is the register the opcode's low nibble names, or
//! under ALT2 the nibble itself.

use crate::registers::status;
use crate::Gsu;

impl Gsu {
    /// `$50-$5F` ADD, and ADC under ALT1, which adds the carry in.
    pub(crate) fn add(&mut self, n: usize) {
        let a = u32::from(self.source_value());
        let b = u32::from(self.register_or_immediate(n));
        let carry_in = u32::from(self.alt1() && self.flag(status::CARRY));
        let sum = a + b + carry_in;
        let result = sum as u16;
        // Overflow: both operands had one sign and the sum has the other.
        self.set_flag(status::OVERFLOW, !(a ^ b) & (b ^ sum) & 0x8000 != 0);
        self.set_flag(status::SIGN, sum & 0x8000 != 0);
        self.set_flag(status::CARRY, sum >= 0x1_0000);
        self.set_flag(status::ZERO, result == 0);
        self.write_destination(result);
        self.clear_prefixes();
    }

    /// `$60-$6F` SUB; SBC under ALT1, which also subtracts the borrow (no
    /// carry); CMP under ALT3, which sets the flags and writes nothing.
    /// ALT2 alone subtracts the opcode's nibble.
    pub(crate) fn subtract(&mut self, n: usize) {
        let compare = self.alt1() && self.alt2();
        let a = self.source_value();
        let b = if self.alt2() && !compare {
            n as u16
        } else {
            self.registers[n]
        };
        let borrow = i32::from(self.alt1() && !self.alt2() && !self.flag(status::CARRY));
        let difference = i32::from(a) - i32::from(b) - borrow;
        let result = difference as u16;
        // Overflow: the operands had different signs and the result has the
        // sign of the one subtracted.
        self.set_flag(status::OVERFLOW, (a ^ b) & (a ^ result) & 0x8000 != 0);
        self.set_flag(status::SIGN, difference & 0x8000 != 0);
        self.set_flag(status::CARRY, difference >= 0);
        self.set_flag(status::ZERO, result == 0);
        if !compare {
            self.write_destination(result);
        }
        self.clear_prefixes();
    }

    /// `$71-$7F` AND, and BIC under ALT1, which clears the operand's bits.
    pub(crate) fn and(&mut self, n: usize) {
        let operand = self.register_or_immediate(n);
        let mask = if self.alt1() { !operand } else { operand };
        let result = self.source_value() & mask;
        self.set_sign_and_zero(result);
        self.write_destination(result);
        self.clear_prefixes();
    }

    /// `$C1-$CF` OR, and XOR under ALT1.
    pub(crate) fn or(&mut self, n: usize) {
        let operand = self.register_or_immediate(n);
        let source = self.source_value();
        let result = if self.alt1() {
            source ^ operand
        } else {
            source | operand
        };
        self.set_sign_and_zero(result);
        self.write_destination(result);
        self.clear_prefixes();
    }

    /// `$4F` NOT: the source's complement.
    pub(crate) fn not(&mut self) {
        let result = !self.source_value();
        self.set_sign_and_zero(result);
        self.write_destination(result);
        self.clear_prefixes();
    }

    /// `$03` LSR: shift right, bit 0 into the carry.
    pub(crate) fn shift_right(&mut self) {
        let source = self.source_value();
        self.set_flag(status::CARRY, source & 1 != 0);
        let result = source >> 1;
        self.set_sign_and_zero(result);
        self.write_destination(result);
        self.clear_prefixes();
    }

    /// `$96` ASR: shift right keeping the sign. Under ALT1, DIV2, which is
    /// the same shift but for one value.
    pub(crate) fn shift_right_signed(&mut self) {
        let value = self.source_value() as i16;
        self.set_flag(status::CARRY, value & 1 != 0);
        let result = match (self.alt1(), value) {
            // DIV2 halves -1 to 0, where the shift leaves -1.
            (true, -1) => 0,
            _ => (value >> 1) as u16,
        };
        self.set_sign_and_zero(result);
        self.write_destination(result);
        self.clear_prefixes();
    }

    /// `$04` ROL: rotate left through the carry.
    pub(crate) fn rotate_left(&mut self) {
        let source = self.source_value();
        let result = (source << 1) | u16::from(self.flag(status::CARRY));
        self.set_flag(status::CARRY, source & 0x8000 != 0);
        self.set_sign_and_zero(result);
        self.write_destination(result);
        self.clear_prefixes();
    }

    /// `$97` ROR: rotate right through the carry.
    pub(crate) fn rotate_right(&mut self) {
        let source = self.source_value();
        let result = (u16::from(self.flag(status::CARRY)) << 15) | (source >> 1);
        self.set_flag(status::CARRY, source & 1 != 0);
        self.set_sign_and_zero(result);
        self.write_destination(result);
        self.clear_prefixes();
    }

    /// `$D0-$DE` INC: add one to the register the nibble names.
    pub(crate) fn increment(&mut self, n: usize) {
        let result = self.registers[n].wrapping_add(1);
        self.write_register(n, result);
        self.set_sign_and_zero(result);
        self.clear_prefixes();
    }

    /// `$E0-$EE` DEC: take one from the register the nibble names.
    pub(crate) fn decrement(&mut self, n: usize) {
        let result = self.registers[n].wrapping_sub(1);
        self.write_register(n, result);
        self.set_sign_and_zero(result);
        self.clear_prefixes();
    }

    /// `$95` SEX: the source's low byte, its sign carried into the high
    /// one.
    pub(crate) fn sign_extend(&mut self) {
        let result = self.source_value() as u8 as i8 as u16;
        self.set_sign_and_zero(result);
        self.write_destination(result);
        self.clear_prefixes();
    }

    /// `$4D` SWAP: the source's two bytes exchanged.
    pub(crate) fn swap_bytes(&mut self) {
        let result = self.source_value().swap_bytes();
        self.set_sign_and_zero(result);
        self.write_destination(result);
        self.clear_prefixes();
    }

    /// `$C0` HIB: the source's high byte, as the low one.
    pub(crate) fn high_byte(&mut self) {
        let result = self.source_value() >> 8;
        self.set_byte_sign_and_zero(result);
        self.write_destination(result);
        self.clear_prefixes();
    }

    /// `$9E` LOB: the source's low byte.
    pub(crate) fn low_byte(&mut self) {
        let result = self.source_value() & 0x00ff;
        self.set_byte_sign_and_zero(result);
        self.write_destination(result);
        self.clear_prefixes();
    }

    /// `$70` MERGE: R7's high byte over R8's. Each flag tests some of the
    /// two bytes' top bits together, which texture code uses to tell
    /// whether coordinates are in range; Z is set when any of the top four
    /// bits is.
    pub(crate) fn merge(&mut self) {
        let result = (self.registers[7] & 0xff00) | (self.registers[8] >> 8);
        self.set_flag(status::OVERFLOW, result & 0xc0c0 != 0);
        self.set_flag(status::SIGN, result & 0x8080 != 0);
        self.set_flag(status::CARRY, result & 0xe0e0 != 0);
        self.set_flag(status::ZERO, result & 0xf0f0 != 0);
        self.write_destination(result);
        self.clear_prefixes();
    }
}
