use crate::z80::addressing::Reg8;
use crate::z80::{InstructionResult, Z80};
use retro_core::cpu::Addressable;

impl Z80 {
    // ========================
    // Logical
    // ========================

    /// AND r - Logical AND register with A
    /// Opcodes: 0xA0-0xA7
    /// T-states: 4
    pub(crate) fn and_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        self.and_a(value);
        InstructionResult::new(4)
    }

    /// AND n - Logical AND immediate with A
    /// Opcode: 0xE6
    /// T-states: 7
    pub(crate) fn and_n(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let value = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        self.and_a(value);
        InstructionResult::new(7)
    }

    /// OR r - Logical OR register with A
    /// Opcodes: 0xB0-0xB7
    /// T-states: 4
    pub(crate) fn or_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        self.or_a(value);
        InstructionResult::new(4)
    }

    /// OR n - Logical OR immediate with A
    /// Opcode: 0xF6
    /// T-states: 7
    pub(crate) fn or_n(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let value = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        self.or_a(value);
        InstructionResult::new(7)
    }

    /// XOR r - Logical XOR register with A
    /// Opcodes: 0xA8-0xAF
    /// T-states: 4
    pub(crate) fn xor_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        self.xor_a(value);
        InstructionResult::new(4)
    }

    /// XOR n - Logical XOR immediate with A
    /// Opcode: 0xEE
    /// T-states: 7
    pub(crate) fn xor_n(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let value = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        self.xor_a(value);
        InstructionResult::new(7)
    }

    // ========================
    // Logical Operations with (HL)
    // ========================

    /// AND (HL) - Logical AND memory value with A
    /// Opcode: 0xA6
    /// T-states: 7
    pub(crate) fn and_hl(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let hl = self.read_reg16(crate::z80::addressing::Reg16::HL);
        let value = bus.read(hl as u32);
        self.and_a(value);
        InstructionResult::new(7)
    }

    /// XOR (HL) - Logical XOR memory value with A
    /// Opcode: 0xAE
    /// T-states: 7
    pub(crate) fn xor_hl(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let hl = self.read_reg16(crate::z80::addressing::Reg16::HL);
        let value = bus.read(hl as u32);
        self.xor_a(value);
        InstructionResult::new(7)
    }

    /// OR (HL) - Logical OR memory value with A
    /// Opcode: 0xB6
    /// T-states: 7
    pub(crate) fn or_hl(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let hl = self.read_reg16(crate::z80::addressing::Reg16::HL);
        let value = bus.read(hl as u32);
        self.or_a(value);
        InstructionResult::new(7)
    }

    // ========================
    // Helper functions
    // ========================

    /// AND helper - performs logical AND and sets flags
    pub(crate) fn and_a(&mut self, value: u8) {
        self.a &= value;
        let result = self.a;

        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_h(true);
        self.set_flag_x(result & 0x08 != 0);
        self.set_flag_pv(self.parity(result));
        self.set_flag_n(false);
        self.set_flag_c(false);
    }

    /// OR helper - performs logical OR and sets flags
    pub(crate) fn or_a(&mut self, value: u8) {
        self.a |= value;
        let result = self.a;

        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_h(false);
        self.set_flag_x(result & 0x08 != 0);
        self.set_flag_pv(self.parity(result));
        self.set_flag_n(false);
        self.set_flag_c(false);
    }

    /// XOR helper - performs logical XOR and sets flags
    pub(crate) fn xor_a(&mut self, value: u8) {
        self.a ^= value;
        let result = self.a;

        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_h(false);
        self.set_flag_x(result & 0x08 != 0);
        self.set_flag_pv(self.parity(result));
        self.set_flag_n(false);
        self.set_flag_c(false);
    }

    /// Calculate parity (even = true, odd = false)
    fn parity(&self, value: u8) -> bool {
        value.count_ones().is_multiple_of(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_and_a_b() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0b11110000;
        cpu.b = 0b10101010;
        ram.write(0x0000, 0xA0); // AND B

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0b10100000);
        assert!(cpu.flag_s()); // Bit 7 set
        assert!(!cpu.flag_z());
        assert!(cpu.flag_h()); // Always set for AND
        assert!(!cpu.flag_n());
        assert!(!cpu.flag_c());
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_and_n_zero() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0b11110000;
        ram.write(0x0000, 0xE6); // AND n
        ram.write(0x0001, 0b00001111);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0);
        assert!(cpu.flag_z()); // Zero
        assert!(cpu.flag_pv()); // Even parity (0 bits set)
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_or_a_b() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0b11110000;
        cpu.b = 0b00001111;
        ram.write(0x0000, 0xB0); // OR B

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0b11111111);
        assert!(cpu.flag_s());
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_h());
        assert!(cpu.flag_pv()); // Even parity (8 bits set)
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_or_n() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x00;
        ram.write(0x0000, 0xF6); // OR n
        ram.write(0x0001, 0x42);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x42);
        assert!(!cpu.flag_z());
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_xor_a_a() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0xFF;
        ram.write(0x0000, 0xAF); // XOR A (clear A)

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0);
        assert!(cpu.flag_z()); // Zero
        assert!(cpu.flag_pv()); // Even parity
        assert!(!cpu.flag_s());
        assert!(!cpu.flag_h());
        assert!(!cpu.flag_n());
        assert!(!cpu.flag_c());
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_xor_n() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0b11110000;
        ram.write(0x0000, 0xEE); // XOR n
        ram.write(0x0001, 0b10101010);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0b01011010);
        assert!(!cpu.flag_s());
        assert!(!cpu.flag_z());
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_parity() {
        let cpu = Z80::new();

        // Even parity (even number of 1s)
        assert!(cpu.parity(0b00000000)); // 0 bits
        assert!(cpu.parity(0b00000011)); // 2 bits
        assert!(cpu.parity(0b11110000)); // 4 bits
        assert!(cpu.parity(0b11111111)); // 8 bits

        // Odd parity (odd number of 1s)
        assert!(!cpu.parity(0b00000001)); // 1 bit
        assert!(!cpu.parity(0b00000111)); // 3 bits
        assert!(!cpu.parity(0b01111111)); // 7 bits
    }
}
