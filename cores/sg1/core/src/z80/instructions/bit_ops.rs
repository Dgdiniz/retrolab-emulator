// Z80 Bit Operations Instructions
// BIT/SET/RES (the 192 CB-prefixed instructions)

use crate::z80::{InstructionResult, Reg8, Z80};

impl Z80 {
    // ========================================================================
    // BIT b, r - Test bit (CB 40-7F)
    // ========================================================================

    /// BIT b, r - Test bit b in register r
    /// Sets Z flag if bit b is 0, resets Z if bit b is 1
    /// Affects: Z (set if bit is 0), H (set), N (reset), S/P/V (varies)
    pub(crate) fn bit_b_r(&mut self, bit: u8, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        let bit_value = (value >> bit) & 1;

        // S flag: set only when testing bit 7 and it is set
        self.set_flag_s(bit == 7 && bit_value != 0);
        // Z flag: set if bit is 0
        self.set_flag_z(bit_value == 0);
        // X/Y flags come from the register being tested
        self.set_flag_y(value & 0x20 != 0);
        self.set_flag_h(true);
        self.set_flag_x(value & 0x08 != 0);
        // P/V flag: same as Z flag for BIT instruction
        self.set_flag_pv(bit_value == 0);
        self.set_flag_n(false);

        InstructionResult::new(8)
    }

    /// BIT b, (HL) - Test bit b in memory at (HL)
    pub(crate) fn bit_b_hl(
        &mut self,
        bit: u8,
        bus: &mut dyn retro_core::cpu::Addressable,
    ) -> InstructionResult {
        let hl = self.read_reg16(crate::z80::Reg16::HL);
        let value = bus.read(hl as u32);
        let bit_value = (value >> bit) & 1;
        let wz_high = (self.wz >> 8) as u8;

        self.set_flag_s(bit == 7 && bit_value != 0);
        self.set_flag_z(bit_value == 0);
        // X/Y come from high byte of WZ for BIT b,(HL)
        self.set_flag_y(wz_high & 0x20 != 0);
        self.set_flag_h(true);
        self.set_flag_x(wz_high & 0x08 != 0);
        self.set_flag_pv(bit_value == 0);
        self.set_flag_n(false);

        InstructionResult::new(12)
    }

    // ========================================================================
    // SET b, r - Set bit (CB C0-FF)
    // ========================================================================

    /// SET b, r - Set bit b in register r
    /// Sets bit b to 1
    /// Affects: None
    pub(crate) fn set_b_r(&mut self, bit: u8, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        let result = value | (1 << bit);
        self.write_reg8(reg, result);

        InstructionResult::new(8)
    }

    /// SET b, (HL) - Set bit b in memory at (HL)
    pub(crate) fn set_b_hl(
        &mut self,
        bit: u8,
        bus: &mut dyn retro_core::cpu::Addressable,
    ) -> InstructionResult {
        let hl = self.read_reg16(crate::z80::Reg16::HL);
        let value = bus.read(hl as u32);
        let result = value | (1 << bit);
        bus.write(hl as u32, result);

        InstructionResult::new(15)
    }

    // ========================================================================
    // RES b, r - Reset bit (CB 80-BF)
    // ========================================================================

    /// RES b, r - Reset bit b in register r
    /// Sets bit b to 0
    /// Affects: None
    pub(crate) fn res_b_r(&mut self, bit: u8, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        let result = value & !(1 << bit);
        self.write_reg8(reg, result);

        InstructionResult::new(8)
    }

    /// RES b, (HL) - Reset bit b in memory at (HL)
    pub(crate) fn res_b_hl(
        &mut self,
        bit: u8,
        bus: &mut dyn retro_core::cpu::Addressable,
    ) -> InstructionResult {
        let hl = self.read_reg16(crate::z80::Reg16::HL);
        let value = bus.read(hl as u32);
        let result = value & !(1 << bit);
        bus.write(hl as u32, result);

        InstructionResult::new(15)
    }
}

// ============================================================================
// UNIT TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::cpu::Addressable;
    use retro_core::memory::Ram;

    // ========================================================================
    // BIT TESTS
    // ========================================================================

    #[test]
    fn test_bit_0_set() {
        let mut cpu = Z80::new();
        cpu.b = 0b00000001; // Bit 0 is set

        cpu.bit_b_r(0, Reg8::B);

        assert!(!cpu.flag_z()); // Z=0 because bit is set
        assert!(cpu.flag_h()); // H always set
        assert!(!cpu.flag_n()); // N always reset
        assert!(!cpu.flag_pv()); // P/V same as Z
    }

    #[test]
    fn test_bit_0_clear() {
        let mut cpu = Z80::new();
        cpu.c = 0b11111110; // Bit 0 is clear

        cpu.bit_b_r(0, Reg8::C);

        assert!(cpu.flag_z()); // Z=1 because bit is clear
        assert!(cpu.flag_h());
        assert!(!cpu.flag_n());
        assert!(cpu.flag_pv()); // P/V same as Z
    }

    #[test]
    fn test_bit_7_set() {
        let mut cpu = Z80::new();
        cpu.d = 0b10000000; // Bit 7 is set

        cpu.bit_b_r(7, Reg8::D);

        assert!(!cpu.flag_z());
        assert!(cpu.flag_s()); // S reflects bit 7 result
        assert!(cpu.flag_h());
        assert!(!cpu.flag_n());
    }

    #[test]
    fn test_bit_7_clear() {
        let mut cpu = Z80::new();
        cpu.e = 0b01111111; // Bit 7 is clear

        cpu.bit_b_r(7, Reg8::E);

        assert!(cpu.flag_z());
        assert!(!cpu.flag_s()); // S reflects bit 7 result
        assert!(cpu.flag_h());
        assert!(!cpu.flag_n());
    }

    #[test]
    fn test_bit_middle() {
        let mut cpu = Z80::new();
        cpu.h = 0b00010000; // Bit 4 is set

        cpu.bit_b_r(4, Reg8::H);

        assert!(!cpu.flag_z());
        assert!(cpu.flag_h());
        assert!(!cpu.flag_n());
    }

    #[test]
    fn test_bit_hl_indirect() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x10;
        cpu.l = 0x00;
        ram.write(0x1000, 0b00001000); // Bit 3 is set

        cpu.bit_b_hl(3, &mut ram);

        assert!(!cpu.flag_z());
        assert!(cpu.flag_h());
        assert!(!cpu.flag_n());
    }

    #[test]
    fn test_bit_all_bits() {
        let mut cpu = Z80::new();
        cpu.a = 0b10101010;

        // Test all 8 bits
        for bit in 0..8 {
            cpu.bit_b_r(bit, Reg8::A);
            let expected_z = ((cpu.a >> bit) & 1) == 0;
            assert_eq!(cpu.flag_z(), expected_z, "Bit {} failed", bit);
        }
    }

    // ========================================================================
    // SET TESTS
    // ========================================================================

    #[test]
    fn test_set_bit_0() {
        let mut cpu = Z80::new();
        cpu.b = 0b00000000;

        cpu.set_b_r(0, Reg8::B);

        assert_eq!(cpu.b, 0b00000001);
    }

    #[test]
    fn test_set_bit_7() {
        let mut cpu = Z80::new();
        cpu.c = 0b00000000;

        cpu.set_b_r(7, Reg8::C);

        assert_eq!(cpu.c, 0b10000000);
    }

    #[test]
    fn test_set_already_set() {
        let mut cpu = Z80::new();
        cpu.d = 0b00000100; // Bit 2 already set

        cpu.set_b_r(2, Reg8::D);

        assert_eq!(cpu.d, 0b00000100); // Still set
    }

    #[test]
    fn test_set_multiple_bits() {
        let mut cpu = Z80::new();
        cpu.e = 0b00000000;

        cpu.set_b_r(0, Reg8::E);
        cpu.set_b_r(2, Reg8::E);
        cpu.set_b_r(7, Reg8::E);

        assert_eq!(cpu.e, 0b10000101);
    }

    #[test]
    fn test_set_hl_indirect() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x10;
        cpu.l = 0x00;
        ram.write(0x1000, 0b00000000);

        cpu.set_b_hl(4, &mut ram);

        assert_eq!(ram.read(0x1000), 0b00010000);
    }

    #[test]
    fn test_set_all_bits() {
        let mut cpu = Z80::new();
        cpu.h = 0b00000000;

        for bit in 0..8 {
            cpu.set_b_r(bit, Reg8::H);
        }

        assert_eq!(cpu.h, 0b11111111);
    }

    // ========================================================================
    // RES TESTS
    // ========================================================================

    #[test]
    fn test_res_bit_0() {
        let mut cpu = Z80::new();
        cpu.b = 0b11111111;

        cpu.res_b_r(0, Reg8::B);

        assert_eq!(cpu.b, 0b11111110);
    }

    #[test]
    fn test_res_bit_7() {
        let mut cpu = Z80::new();
        cpu.c = 0b11111111;

        cpu.res_b_r(7, Reg8::C);

        assert_eq!(cpu.c, 0b01111111);
    }

    #[test]
    fn test_res_already_clear() {
        let mut cpu = Z80::new();
        cpu.d = 0b11111011; // Bit 2 already clear

        cpu.res_b_r(2, Reg8::D);

        assert_eq!(cpu.d, 0b11111011); // Still clear
    }

    #[test]
    fn test_res_multiple_bits() {
        let mut cpu = Z80::new();
        cpu.e = 0b11111111;

        cpu.res_b_r(0, Reg8::E);
        cpu.res_b_r(2, Reg8::E);
        cpu.res_b_r(7, Reg8::E);

        assert_eq!(cpu.e, 0b01111010);
    }

    #[test]
    fn test_res_hl_indirect() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x10;
        cpu.l = 0x00;
        ram.write(0x1000, 0b11111111);

        cpu.res_b_hl(4, &mut ram);

        assert_eq!(ram.read(0x1000), 0b11101111);
    }

    #[test]
    fn test_res_all_bits() {
        let mut cpu = Z80::new();
        cpu.l = 0b11111111;

        for bit in 0..8 {
            cpu.res_b_r(bit, Reg8::L);
        }

        assert_eq!(cpu.l, 0b00000000);
    }

    // ========================================================================
    // COMBINED TESTS
    // ========================================================================

    #[test]
    fn test_bit_set_res_sequence() {
        let mut cpu = Z80::new();
        cpu.a = 0b00000000;

        // SET bit 3
        cpu.set_b_r(3, Reg8::A);
        assert_eq!(cpu.a, 0b00001000);

        // BIT test bit 3 (should be set)
        cpu.bit_b_r(3, Reg8::A);
        assert!(!cpu.flag_z());

        // RES bit 3
        cpu.res_b_r(3, Reg8::A);
        assert_eq!(cpu.a, 0b00000000);

        // BIT test bit 3 (should be clear)
        cpu.bit_b_r(3, Reg8::A);
        assert!(cpu.flag_z());
    }

    #[test]
    fn test_flags_not_affected_by_set_res() {
        let mut cpu = Z80::new();
        cpu.b = 0x00;

        // Set some flags
        cpu.set_flag_z(true);
        cpu.set_flag_s(true);
        cpu.set_flag_c(true);

        // SET and RES should not affect flags
        cpu.set_b_r(5, Reg8::B);
        assert!(cpu.flag_z());
        assert!(cpu.flag_s());
        assert!(cpu.flag_c());

        cpu.res_b_r(5, Reg8::B);
        assert!(cpu.flag_z());
        assert!(cpu.flag_s());
        assert!(cpu.flag_c());
    }

    #[test]
    fn test_bit_pattern_manipulation() {
        let mut cpu = Z80::new();
        cpu.h = 0b10101010; // 0xAA

        // Clear even bits (0, 2, 4, 6)
        for bit in (0..8).step_by(2) {
            cpu.res_b_r(bit, Reg8::H);
        }
        assert_eq!(cpu.h, 0b10101010); // No change (already clear)

        // Set even bits
        for bit in (0..8).step_by(2) {
            cpu.set_b_r(bit, Reg8::H);
        }
        assert_eq!(cpu.h, 0b11111111);

        // Clear odd bits (1, 3, 5, 7)
        for bit in (1..8).step_by(2) {
            cpu.res_b_r(bit, Reg8::H);
        }
        assert_eq!(cpu.h, 0b01010101); // 0x55
    }
}
