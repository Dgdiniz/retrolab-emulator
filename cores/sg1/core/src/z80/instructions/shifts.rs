// Z80 Rotate and Shift Instructions
// RLCA/RRCA/RLA/RRA + CB prefix (64 instructions) + RLD/RRD

use crate::z80::{InstructionResult, Reg16, Reg8, Z80};

impl Z80 {
    /// Helper: set standard shift/rotate flags (S, Z, Y, H=0, X, PV=parity, N=0, C)
    fn set_shift_flags(&mut self, result: u8, carry: bool) {
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_h(false);
        self.set_flag_x(result & 0x08 != 0);
        self.set_flag_pv(result.count_ones().is_multiple_of(2));
        self.set_flag_n(false);
        self.set_flag_c(carry);
    }

    // ========================================================================
    // ACCUMULATOR ROTATES (non-prefixed)
    // ========================================================================

    /// RLCA - Rotate Left Circular Accumulator (0x07)
    /// Rotates A left 1 bit, bit 7 goes to carry and bit 0
    pub(crate) fn rlca(&mut self) -> InstructionResult {
        let a = self.a;
        let bit7 = (a & 0x80) != 0;

        self.a = (a << 1) | (if bit7 { 1 } else { 0 });

        // Flags: S, Z, PV unchanged; H=0, N=0, C=bit7; X/Y from A
        self.set_flag_y(self.a & 0x20 != 0);
        self.set_flag_h(false);
        self.set_flag_x(self.a & 0x08 != 0);
        self.set_flag_n(false);
        self.set_flag_c(bit7);

        InstructionResult::new(4)
    }

    /// RRCA - Rotate Right Circular Accumulator (0x0F)
    /// Rotates A right 1 bit, bit 0 goes to carry and bit 7
    pub(crate) fn rrca(&mut self) -> InstructionResult {
        let a = self.a;
        let bit0 = (a & 0x01) != 0;

        self.a = (a >> 1) | (if bit0 { 0x80 } else { 0 });

        self.set_flag_y(self.a & 0x20 != 0);
        self.set_flag_h(false);
        self.set_flag_x(self.a & 0x08 != 0);
        self.set_flag_n(false);
        self.set_flag_c(bit0);

        InstructionResult::new(4)
    }

    /// RLA - Rotate Left Accumulator through Carry (0x17)
    /// Rotates A left 1 bit through carry
    pub(crate) fn rla(&mut self) -> InstructionResult {
        let a = self.a;
        let old_carry = if self.flag_c() { 1 } else { 0 };
        let bit7 = (a & 0x80) != 0;

        self.a = (a << 1) | old_carry;

        self.set_flag_y(self.a & 0x20 != 0);
        self.set_flag_h(false);
        self.set_flag_x(self.a & 0x08 != 0);
        self.set_flag_n(false);
        self.set_flag_c(bit7);

        InstructionResult::new(4)
    }

    /// RRA - Rotate Right Accumulator through Carry (0x1F)
    /// Rotates A right 1 bit through carry
    pub(crate) fn rra(&mut self) -> InstructionResult {
        let a = self.a;
        let old_carry = if self.flag_c() { 0x80 } else { 0 };
        let bit0 = (a & 0x01) != 0;

        self.a = (a >> 1) | old_carry;

        self.set_flag_y(self.a & 0x20 != 0);
        self.set_flag_h(false);
        self.set_flag_x(self.a & 0x08 != 0);
        self.set_flag_n(false);
        self.set_flag_c(bit0);

        InstructionResult::new(4)
    }

    // ========================================================================
    // CB PREFIX ROTATES/SHIFTS
    // ========================================================================

    /// RLC r - Rotate Left Circular (CB 00-07)
    pub(crate) fn rlc_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        let bit7 = (value & 0x80) != 0;
        let result = (value << 1) | (if bit7 { 1 } else { 0 });

        self.write_reg8(reg, result);

        self.set_shift_flags(result, bit7);

        InstructionResult::new(8)
    }

    /// RRC r - Rotate Right Circular (CB 08-0F)
    pub(crate) fn rrc_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        let bit0 = (value & 0x01) != 0;
        let result = (value >> 1) | (if bit0 { 0x80 } else { 0 });

        self.write_reg8(reg, result);

        self.set_shift_flags(result, bit0);

        InstructionResult::new(8)
    }

    /// RL r - Rotate Left through Carry (CB 10-17)
    pub(crate) fn rl_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        let old_carry = if self.flag_c() { 1 } else { 0 };
        let bit7 = (value & 0x80) != 0;
        let result = (value << 1) | old_carry;

        self.write_reg8(reg, result);

        self.set_shift_flags(result, bit7);

        InstructionResult::new(8)
    }

    /// RR r - Rotate Right through Carry (CB 18-1F)
    pub(crate) fn rr_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        let old_carry = if self.flag_c() { 0x80 } else { 0 };
        let bit0 = (value & 0x01) != 0;
        let result = (value >> 1) | old_carry;

        self.write_reg8(reg, result);

        self.set_shift_flags(result, bit0);

        InstructionResult::new(8)
    }

    /// SLA r - Shift Left Arithmetic (CB 20-27)
    /// Shifts left, bit 7 to carry, bit 0 = 0
    pub(crate) fn sla_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        let bit7 = (value & 0x80) != 0;
        let result = value << 1;

        self.write_reg8(reg, result);

        self.set_shift_flags(result, bit7);

        InstructionResult::new(8)
    }

    /// SRA r - Shift Right Arithmetic (CB 28-2F)
    /// Shifts right, bit 7 unchanged (sign extension), bit 0 to carry
    pub(crate) fn sra_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        let bit7 = value & 0x80;
        let bit0 = (value & 0x01) != 0;
        let result = (value >> 1) | bit7; // Preserve bit 7

        self.write_reg8(reg, result);

        self.set_shift_flags(result, bit0);

        InstructionResult::new(8)
    }

    /// SLL r - Shift Left Logical (CB 30-37) - UNDOCUMENTED
    /// Shifts left, bit 7 to carry, bit 0 = 1
    pub(crate) fn sll_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        let bit7 = (value & 0x80) != 0;
        let result = (value << 1) | 1; // Bit 0 set to 1

        self.write_reg8(reg, result);

        self.set_shift_flags(result, bit7);

        InstructionResult::new(8)
    }

    /// SRL r - Shift Right Logical (CB 38-3F)
    /// Shifts right, bit 0 to carry, bit 7 = 0
    pub(crate) fn srl_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        let bit0 = (value & 0x01) != 0;
        let result = value >> 1;

        self.write_reg8(reg, result);

        self.set_shift_flags(result, bit0);

        InstructionResult::new(8)
    }

    // ========================================================================
    // CB PREFIX OPERATIONS ON (HL) - Memory versions
    // ========================================================================

    /// Helper function for CB operations on (HL)
    pub(crate) fn rlc_hl(
        &mut self,
        bus: &mut dyn retro_core::cpu::Addressable,
    ) -> InstructionResult {
        let hl = self.read_reg16(Reg16::HL);
        let value = bus.read(hl as u32);
        let bit7 = (value & 0x80) != 0;
        let result = (value << 1) | (if bit7 { 1 } else { 0 });
        bus.write(hl as u32, result);

        self.set_shift_flags(result, bit7);
        InstructionResult::new(15)
    }

    pub(crate) fn rrc_hl(
        &mut self,
        bus: &mut dyn retro_core::cpu::Addressable,
    ) -> InstructionResult {
        let hl = self.read_reg16(Reg16::HL);
        let value = bus.read(hl as u32);
        let bit0 = (value & 0x01) != 0;
        let result = (value >> 1) | (if bit0 { 0x80 } else { 0 });
        bus.write(hl as u32, result);

        self.set_shift_flags(result, bit0);
        InstructionResult::new(15)
    }

    pub(crate) fn rl_hl(
        &mut self,
        bus: &mut dyn retro_core::cpu::Addressable,
    ) -> InstructionResult {
        let hl = self.read_reg16(Reg16::HL);
        let value = bus.read(hl as u32);
        let old_carry = if self.flag_c() { 1 } else { 0 };
        let bit7 = (value & 0x80) != 0;
        let result = (value << 1) | old_carry;
        bus.write(hl as u32, result);

        self.set_shift_flags(result, bit7);
        InstructionResult::new(15)
    }

    pub(crate) fn rr_hl(
        &mut self,
        bus: &mut dyn retro_core::cpu::Addressable,
    ) -> InstructionResult {
        let hl = self.read_reg16(crate::z80::Reg16::HL);
        let value = bus.read(hl as u32);
        let old_carry = if self.flag_c() { 0x80 } else { 0 };
        let bit0 = (value & 0x01) != 0;
        let result = (value >> 1) | old_carry;
        bus.write(hl as u32, result);

        self.set_shift_flags(result, bit0);
        InstructionResult::new(15)
    }

    pub(crate) fn sla_hl(
        &mut self,
        bus: &mut dyn retro_core::cpu::Addressable,
    ) -> InstructionResult {
        let hl = self.read_reg16(crate::z80::Reg16::HL);
        let value = bus.read(hl as u32);
        let bit7 = (value & 0x80) != 0;
        let result = value << 1;
        bus.write(hl as u32, result);

        self.set_shift_flags(result, bit7);
        InstructionResult::new(15)
    }

    pub(crate) fn sra_hl(
        &mut self,
        bus: &mut dyn retro_core::cpu::Addressable,
    ) -> InstructionResult {
        let hl = self.read_reg16(crate::z80::Reg16::HL);
        let value = bus.read(hl as u32);
        let bit7 = value & 0x80;
        let bit0 = (value & 0x01) != 0;
        let result = (value >> 1) | bit7;
        bus.write(hl as u32, result);

        self.set_shift_flags(result, bit0);
        InstructionResult::new(15)
    }

    pub(crate) fn sll_hl(
        &mut self,
        bus: &mut dyn retro_core::cpu::Addressable,
    ) -> InstructionResult {
        let hl = self.read_reg16(crate::z80::Reg16::HL);
        let value = bus.read(hl as u32);
        let bit7 = (value & 0x80) != 0;
        let result = (value << 1) | 1;
        bus.write(hl as u32, result);

        self.set_shift_flags(result, bit7);
        InstructionResult::new(15)
    }

    pub(crate) fn srl_hl(
        &mut self,
        bus: &mut dyn retro_core::cpu::Addressable,
    ) -> InstructionResult {
        let hl = self.read_reg16(crate::z80::Reg16::HL);
        let value = bus.read(hl as u32);
        let bit0 = (value & 0x01) != 0;
        let result = value >> 1;
        bus.write(hl as u32, result);

        self.set_shift_flags(result, bit0);
        InstructionResult::new(15)
    }

    // ========================================================================
    // ED PREFIX DIGIT ROTATES
    // ========================================================================

    /// RLD - Rotate Left Digit (ED 6F)
    /// Rotates nibbles: (HL) high nibble <- (HL) low nibble <- A low nibble <- (HL) high nibble
    pub(crate) fn rld(&mut self, bus: &mut dyn retro_core::cpu::Addressable) -> InstructionResult {
        let hl = self.read_reg16(crate::z80::Reg16::HL);
        let hl_value = bus.read(hl as u32);
        let a_low = self.a & 0x0F;
        let hl_high = (hl_value >> 4) & 0x0F;
        let hl_low = hl_value & 0x0F;

        // A = (A high nibble) | (HL high nibble)
        self.a = (self.a & 0xF0) | hl_high;

        // (HL) = (HL low nibble << 4) | (A low nibble)
        let new_hl_value = (hl_low << 4) | a_low;
        bus.write(hl as u32, new_hl_value);

        // Flags: S, Z, PV set based on A; H=0, N=0, C unchanged
        self.set_flag_s(self.a & 0x80 != 0);
        self.set_flag_z(self.a == 0);
        self.set_flag_y(self.a & 0x20 != 0);
        self.set_flag_h(false);
        self.set_flag_x(self.a & 0x08 != 0);
        self.set_flag_pv(self.a.count_ones().is_multiple_of(2));
        self.set_flag_n(false);

        self.wz = hl.wrapping_add(1);

        InstructionResult::new(18)
    }

    /// RRD - Rotate Right Digit (ED 67)
    /// Rotates nibbles: (HL) low nibble -> A low nibble -> (HL) high nibble -> (HL) low nibble
    pub(crate) fn rrd(&mut self, bus: &mut dyn retro_core::cpu::Addressable) -> InstructionResult {
        let hl = self.read_reg16(crate::z80::Reg16::HL);
        let hl_value = bus.read(hl as u32);
        let a_low = self.a & 0x0F;
        let hl_high = (hl_value >> 4) & 0x0F;
        let hl_low = hl_value & 0x0F;

        // A = (A high nibble) | (HL low nibble)
        self.a = (self.a & 0xF0) | hl_low;

        // (HL) = (A low nibble << 4) | (HL high nibble)
        let new_hl_value = (a_low << 4) | hl_high;
        bus.write(hl as u32, new_hl_value);

        // Flags: S, Z, PV set based on A; H=0, N=0, C unchanged
        self.set_flag_s(self.a & 0x80 != 0);
        self.set_flag_z(self.a == 0);
        self.set_flag_y(self.a & 0x20 != 0);
        self.set_flag_h(false);
        self.set_flag_x(self.a & 0x08 != 0);
        self.set_flag_pv(self.a.count_ones().is_multiple_of(2));
        self.set_flag_n(false);

        self.wz = hl.wrapping_add(1);

        InstructionResult::new(18)
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
    // ACCUMULATOR ROTATES
    // ========================================================================

    #[test]
    fn test_rlca() {
        let mut cpu = Z80::new();
        cpu.a = 0b10110101;
        cpu.set_flag_c(false);

        cpu.rlca();

        assert_eq!(cpu.a, 0b01101011); // Bit 7 rotated to bit 0
        assert!(cpu.flag_c()); // Bit 7 went to carry
        assert!(!cpu.flag_h());
        assert!(!cpu.flag_n());
    }

    #[test]
    fn test_rrca() {
        let mut cpu = Z80::new();
        cpu.a = 0b10110101;
        cpu.set_flag_c(false);

        cpu.rrca();

        assert_eq!(cpu.a, 0b11011010); // Bit 0 rotated to bit 7
        assert!(cpu.flag_c()); // Bit 0 went to carry
        assert!(!cpu.flag_h());
        assert!(!cpu.flag_n());
    }

    #[test]
    fn test_rla() {
        let mut cpu = Z80::new();
        cpu.a = 0b10110101;
        cpu.set_flag_c(true); // Old carry = 1

        cpu.rla();

        assert_eq!(cpu.a, 0b01101011); // Shifted left, carry into bit 0
        assert!(cpu.flag_c()); // Bit 7 went to carry
        assert!(!cpu.flag_h());
        assert!(!cpu.flag_n());
    }

    #[test]
    fn test_rra() {
        let mut cpu = Z80::new();
        cpu.a = 0b10110101;
        cpu.set_flag_c(true); // Old carry = 1

        cpu.rra();

        assert_eq!(cpu.a, 0b11011010); // Shifted right, carry into bit 7
        assert!(cpu.flag_c()); // Bit 0 went to carry
        assert!(!cpu.flag_h());
        assert!(!cpu.flag_n());
    }

    // ========================================================================
    // CB PREFIX ROTATES/SHIFTS
    // ========================================================================

    #[test]
    fn test_rlc_r() {
        let mut cpu = Z80::new();
        cpu.b = 0b10110101;

        cpu.rlc_r(Reg8::B);

        assert_eq!(cpu.b, 0b01101011);
        assert!(cpu.flag_c());
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_s()); // Bit 7 = 0, so not sign
        assert!(!cpu.flag_h());
        assert!(!cpu.flag_n());
    }

    #[test]
    fn test_rrc_r() {
        let mut cpu = Z80::new();
        cpu.c = 0b10110101;

        cpu.rrc_r(Reg8::C);

        assert_eq!(cpu.c, 0b11011010);
        assert!(cpu.flag_c());
        assert!(cpu.flag_s());
        assert!(!cpu.flag_z());
    }

    #[test]
    fn test_rl_r() {
        let mut cpu = Z80::new();
        cpu.d = 0b01110101;
        cpu.set_flag_c(true);

        cpu.rl_r(Reg8::D);

        assert_eq!(cpu.d, 0b11101011); // Carry in bit 0
        assert!(!cpu.flag_c()); // Bit 7 was 0
        assert!(cpu.flag_s()); // Result has bit 7 set
    }

    #[test]
    fn test_rr_r() {
        let mut cpu = Z80::new();
        cpu.e = 0b10110100;
        cpu.set_flag_c(true);

        cpu.rr_r(Reg8::E);

        assert_eq!(cpu.e, 0b11011010);
        assert!(!cpu.flag_c()); // Bit 0 was 0
        assert!(cpu.flag_s());
    }

    #[test]
    fn test_sla_r() {
        let mut cpu = Z80::new();
        cpu.h = 0b10110101;

        cpu.sla_r(Reg8::H);

        assert_eq!(cpu.h, 0b01101010); // Bit 0 = 0
        assert!(cpu.flag_c()); // Bit 7 to carry
        assert!(!cpu.flag_z());
    }

    #[test]
    fn test_sra_r() {
        let mut cpu = Z80::new();
        cpu.l = 0b10110101;

        cpu.sra_r(Reg8::L);

        assert_eq!(cpu.l, 0b11011010); // Bit 7 preserved (sign extension)
        assert!(cpu.flag_c()); // Bit 0 to carry
        assert!(cpu.flag_s());
    }

    #[test]
    fn test_sll_r() {
        let mut cpu = Z80::new();
        cpu.a = 0b10110100;

        cpu.sll_r(Reg8::A);

        assert_eq!(cpu.a, 0b01101001); // Bit 0 = 1
        assert!(cpu.flag_c()); // Bit 7 to carry
    }

    #[test]
    fn test_srl_r() {
        let mut cpu = Z80::new();
        cpu.b = 0b10110101;

        cpu.srl_r(Reg8::B);

        assert_eq!(cpu.b, 0b01011010); // Bit 7 = 0
        assert!(cpu.flag_c()); // Bit 0 to carry
        assert!(!cpu.flag_s());
    }

    #[test]
    fn test_sla_zero_result() {
        let mut cpu = Z80::new();
        cpu.a = 0x00;

        cpu.sla_r(Reg8::A);

        assert_eq!(cpu.a, 0x00);
        assert!(cpu.flag_z());
        assert!(!cpu.flag_c());
    }

    // ========================================================================
    // ED PREFIX DIGIT ROTATES
    // ========================================================================

    #[test]
    fn test_rld() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x50;
        cpu.l = 0x00;
        cpu.a = 0x7A; // A = 0x7A (high=7, low=A)
        ram.write(0x5000, 0x31); // (HL) = 0x31 (high=3, low=1)

        cpu.rld(&mut ram);

        // Expected: A = 0x73 (high=7, low=3), (HL) = 0x1A (high=1, low=A)
        assert_eq!(cpu.a, 0x73);
        assert_eq!(ram.read(0x5000), 0x1A);
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_h());
        assert!(!cpu.flag_n());
    }

    #[test]
    fn test_rrd() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x50;
        cpu.l = 0x00;
        cpu.a = 0x84; // A = 0x84 (high=8, low=4)
        ram.write(0x5000, 0x20); // (HL) = 0x20 (high=2, low=0)

        cpu.rrd(&mut ram);

        // Expected: A = 0x80 (high=8, low=0), (HL) = 0x42 (high=4, low=2)
        assert_eq!(cpu.a, 0x80);
        assert_eq!(ram.read(0x5000), 0x42);
        assert!(!cpu.flag_z());
        assert!(cpu.flag_s());
        assert!(!cpu.flag_h());
        assert!(!cpu.flag_n());
    }

    #[test]
    fn test_rld_zero_result() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x10;
        cpu.l = 0x00;
        cpu.a = 0x00;
        ram.write(0x1000, 0x00);

        cpu.rld(&mut ram);

        assert_eq!(cpu.a, 0x00);
        assert!(cpu.flag_z());
    }

    #[test]
    fn test_rrd_parity() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x10;
        cpu.l = 0x00;
        cpu.a = 0xF3; // High=F, Low=3
        ram.write(0x1000, 0x56); // High=5, Low=6

        cpu.rrd(&mut ram);

        // A = 0xF6 (6 bits set, even parity)
        assert_eq!(cpu.a, 0xF6);
        assert!(cpu.flag_pv()); // Even parity
    }
}
