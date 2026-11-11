//! 68000 instruction implementations.

#![cfg_attr(test, allow(unused_variables))]

pub mod arithmetic;
pub mod bcd;
pub mod branches;
pub mod control;
pub mod data_movement;
pub mod logical;
pub mod shifts;
pub mod system;

use super::{addressing::Size, M68k};

/// Result of instruction execution.
#[derive(Debug, Clone, Copy)]
pub struct InstructionResult {
    /// Number of cycles consumed.
    pub cycles: u32,
    /// PC increment (in bytes, usually handled by caller).
    pub pc_increment: u32,
    /// Whether this instruction uses the prefetch queue.
    pub uses_prefetch: bool,
    /// Whether the PC was already set by the instruction (e.g., branch taken, JMP, RTS).
    pub pc_set: bool,
}

impl InstructionResult {
    pub fn new(cycles: u32, pc_increment: u32) -> Self {
        Self {
            cycles,
            pc_increment,
            uses_prefetch: false, // Legacy by default
            pc_set: false,
        }
    }

    /// Create a result for an instruction that uses the prefetch queue.
    pub fn prefetch(cycles: u32) -> Self {
        Self {
            cycles,
            pc_increment: 0,
            uses_prefetch: true,
            pc_set: false,
        }
    }

    /// Create a result for an instruction that directly set the PC (branch taken, JMP, etc.).
    pub fn branch_taken(cycles: u32) -> Self {
        Self {
            cycles,
            pc_increment: 0,
            uses_prefetch: false,
            pc_set: true,
        }
    }
}

/// Status register (SR) flag manipulation helpers.
impl M68k {
    /// Get a condition code flag.
    pub fn get_flag(&self, flag: Flag) -> bool {
        (self.sr & (flag as u16)) != 0
    }

    /// Set a condition code flag.
    pub fn set_flag(&mut self, flag: Flag, value: bool) {
        if value {
            self.sr |= flag as u16;
        } else {
            self.sr &= !(flag as u16);
        }
    }

    /// Update flags based on a result value.
    /// - Z: Zero flag (result == 0)
    /// - N: Negative flag (MSB set)
    /// - V and C are NOT modified (caller must set explicitly)
    pub fn update_flags_nz(&mut self, result: u32, size: Size) {
        let mask = size.mask();
        let result = result & mask;

        // Zero flag
        self.set_flag(Flag::Z, result == 0);

        // Negative flag (check MSB based on size)
        let msb = match size {
            Size::Byte => result & 0x80,
            Size::Word => result & 0x8000,
            Size::Long => result & 0x8000_0000,
        };
        self.set_flag(Flag::N, msb != 0);
    }

    /// Update all arithmetic flags (N, Z, V, C) and X based on a result.
    /// This is typically used for ADD/SUB instructions.
    /// X is set to the same value as C.
    pub fn update_flags_nzvxc(
        &mut self,
        result: u32,
        operand1: u32,
        operand2: u32,
        size: Size,
        is_subtraction: bool,
    ) {
        self.update_flags_nzvc(result, operand1, operand2, size, is_subtraction);
        // X flag is always set to the same value as C for arithmetic
        self.set_flag(Flag::X, self.get_flag(Flag::C));
    }

    /// Update arithmetic flags (N, Z, V, C) based on a result, but NOT X.
    /// This is used for CMP instructions which do not affect the X flag.
    pub fn update_flags_nzvc(
        &mut self,
        result: u32,
        operand1: u32,
        operand2: u32,
        size: Size,
        is_subtraction: bool,
    ) {
        let mask = size.mask();
        let result = result & mask;
        let op1 = operand1 & mask;
        let op2 = operand2 & mask;

        // Zero and Negative
        self.update_flags_nz(result, size);

        // Carry and Overflow depend on operation type
        if is_subtraction {
            // For subtraction: result = op1 - op2
            // Carry: set if borrow occurred
            self.set_flag(Flag::C, op1 < op2);

            // Overflow: set if sign of result is incorrect
            // V = (src ^ dst) & (src ^ result)
            let msb_bit = match size {
                Size::Byte => 7,
                Size::Word => 15,
                Size::Long => 31,
            };
            let v = ((op1 ^ op2) & (op1 ^ result) & (1 << msb_bit)) != 0;
            self.set_flag(Flag::V, v);
        } else {
            // For addition: result = op1 + op2
            // Carry: set if carry occurred
            let full_result = (op1 as u64) + (op2 as u64);
            self.set_flag(Flag::C, full_result > mask as u64);

            // Overflow: set if sign of result is incorrect
            // V = ~(src ^ dst) & (src ^ result)
            // Overflow occurs when both operands have the same sign but result has different sign
            let msb_bit = match size {
                Size::Byte => 7,
                Size::Word => 15,
                Size::Long => 31,
            };
            let v = ((!(op1 ^ op2)) & (op1 ^ result) & (1 << msb_bit)) != 0;
            self.set_flag(Flag::V, v);
        }
    }

    /// Test a condition code for branch instructions.
    pub fn test_condition(&self, condition: u8) -> bool {
        match condition {
            0x0 => true,                                               // T (always true)
            0x1 => false,                                              // F (always false)
            0x2 => !self.get_flag(Flag::C) && !self.get_flag(Flag::Z), // HI (high)
            0x3 => self.get_flag(Flag::C) || self.get_flag(Flag::Z),   // LS (low or same)
            0x4 => !self.get_flag(Flag::C),                            // CC/HS (carry clear)
            0x5 => self.get_flag(Flag::C),                             // CS/LO (carry set)
            0x6 => !self.get_flag(Flag::Z),                            // NE (not equal)
            0x7 => self.get_flag(Flag::Z),                             // EQ (equal)
            0x8 => !self.get_flag(Flag::V),                            // VC (overflow clear)
            0x9 => self.get_flag(Flag::V),                             // VS (overflow set)
            0xA => !self.get_flag(Flag::N),                            // PL (plus)
            0xB => self.get_flag(Flag::N),                             // MI (minus)
            0xC => {
                // GE (greater or equal): N == V
                let n = self.get_flag(Flag::N);
                let v = self.get_flag(Flag::V);
                n == v
            }
            0xD => {
                // LT (less than): N != V
                let n = self.get_flag(Flag::N);
                let v = self.get_flag(Flag::V);
                n != v
            }
            0xE => {
                // GT (greater than): !Z && (N == V)
                let n = self.get_flag(Flag::N);
                let v = self.get_flag(Flag::V);
                let z = self.get_flag(Flag::Z);
                !z && n == v
            }
            0xF => {
                // LE (less or equal)
                let z = self.get_flag(Flag::Z);
                let n = self.get_flag(Flag::N);
                let v = self.get_flag(Flag::V);
                z || (n && !v) || (!n && v)
            }
            _ => false,
        }
    }
}

/// Status Register flags.
#[derive(Debug, Clone, Copy)]
#[repr(u16)]
pub enum Flag {
    C = 0x0001, // Carry
    V = 0x0002, // Overflow
    Z = 0x0004, // Zero
    N = 0x0008, // Negative
    X = 0x0010, // Extend
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flags() {
        let mut cpu = M68k::new();

        cpu.set_flag(Flag::Z, true);
        assert!(cpu.get_flag(Flag::Z));
        assert!(!cpu.get_flag(Flag::N));

        cpu.set_flag(Flag::N, true);
        assert!(cpu.get_flag(Flag::Z));
        assert!(cpu.get_flag(Flag::N));

        cpu.set_flag(Flag::Z, false);
        assert!(!cpu.get_flag(Flag::Z));
        assert!(cpu.get_flag(Flag::N));
    }

    #[test]
    fn test_update_flags_nz() {
        let mut cpu = M68k::new();

        // Zero result
        cpu.update_flags_nz(0, Size::Word);
        assert!(cpu.get_flag(Flag::Z));
        assert!(!cpu.get_flag(Flag::N));

        // Negative result (word)
        cpu.update_flags_nz(0x8000, Size::Word);
        assert!(!cpu.get_flag(Flag::Z));
        assert!(cpu.get_flag(Flag::N));

        // Positive result
        cpu.update_flags_nz(0x1234, Size::Word);
        assert!(!cpu.get_flag(Flag::Z));
        assert!(!cpu.get_flag(Flag::N));
    }

    #[test]
    fn test_condition_codes() {
        let mut cpu = M68k::new();

        // Test EQ condition (Z=1)
        cpu.set_flag(Flag::Z, true);
        assert!(cpu.test_condition(0x7)); // EQ

        // Test NE condition (Z=0)
        cpu.set_flag(Flag::Z, false);
        assert!(cpu.test_condition(0x6)); // NE

        // Test CC condition (C=0)
        cpu.set_flag(Flag::C, false);
        assert!(cpu.test_condition(0x4)); // CC
    }
}
