use crate::z80::addressing::{Reg16, Reg8};
use crate::z80::{InstructionResult, Z80};
use retro_core::cpu::Addressable;

impl Z80 {
    // ========================
    // Arithmetic 8-bit
    // ========================

    /// ADD A, r - Add register to A
    /// Opcodes: 0x80-0x87
    /// T-states: 4
    pub(crate) fn add_a_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        self.add_a(value);
        InstructionResult::new(4)
    }

    /// ADD A, n - Add immediate to A
    /// Opcode: 0xC6
    /// T-states: 7
    pub(crate) fn add_a_n(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let value = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        self.add_a(value);
        InstructionResult::new(7)
    }

    /// SUB r - Subtract register from A
    /// Opcodes: 0x90-0x97
    /// T-states: 4
    pub(crate) fn sub_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        self.sub_a(value);
        InstructionResult::new(4)
    }

    /// SUB n - Subtract immediate from A
    /// Opcode: 0xD6
    /// T-states: 7
    pub(crate) fn sub_n(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let value = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        self.sub_a(value);
        InstructionResult::new(7)
    }

    /// INC r - Increment register
    /// Opcodes: 0x04, 0x0C, 0x14, 0x1C, 0x24, 0x2C, 0x3C
    /// T-states: 4
    pub(crate) fn inc_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        let result = value.wrapping_add(1);

        // Set flags
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0); // Undocumented: bit 5 of result
        self.set_flag_h((value & 0x0F) + 1 > 0x0F);
        self.set_flag_x(result & 0x08 != 0); // Undocumented: bit 3 of result
        self.set_flag_pv(value == 0x7F); // Overflow from 0x7F to 0x80
        self.set_flag_n(false);
        // Carry flag not affected

        self.write_reg8(reg, result);
        InstructionResult::new(4)
    }

    /// DEC r - Decrement register
    /// Opcodes: 0x05, 0x0D, 0x15, 0x1D, 0x25, 0x2D, 0x3D
    /// T-states: 4
    pub(crate) fn dec_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        let result = value.wrapping_sub(1);

        // Set flags
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0); // Undocumented: bit 5 of result
        self.set_flag_h((value & 0x0F) == 0); // Borrow from bit 4
        self.set_flag_x(result & 0x08 != 0); // Undocumented: bit 3 of result
        self.set_flag_pv(value == 0x80); // Overflow from 0x80 to 0x7F
        self.set_flag_n(true);
        // Carry flag not affected

        self.write_reg8(reg, result);
        InstructionResult::new(4)
    }

    /// INC (HL) - Increment byte at (HL)
    /// Opcode: 0x34
    /// T-states: 11
    pub(crate) fn inc_hl_indirect(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let hl = self.read_reg16(Reg16::HL);
        let value = bus.read(hl as u32);
        let result = value.wrapping_add(1);

        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_h((value & 0x0F) + 1 > 0x0F);
        self.set_flag_x(result & 0x08 != 0);
        self.set_flag_pv(value == 0x7F);
        self.set_flag_n(false);

        bus.write(hl as u32, result);
        InstructionResult::new(11)
    }

    /// DEC (HL) - Decrement byte at (HL)
    /// Opcode: 0x35
    /// T-states: 11
    pub(crate) fn dec_hl_indirect(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let hl = self.read_reg16(Reg16::HL);
        let value = bus.read(hl as u32);
        let result = value.wrapping_sub(1);

        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_h((value & 0x0F) == 0);
        self.set_flag_x(result & 0x08 != 0);
        self.set_flag_pv(value == 0x80);
        self.set_flag_n(true);

        bus.write(hl as u32, result);
        InstructionResult::new(11)
    }

    /// CP r - Compare register with A
    /// Opcodes: 0xB8-0xBF
    /// T-states: 4
    pub(crate) fn cp_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        self.cp_a(value);
        InstructionResult::new(4)
    }

    /// CP n - Compare immediate with A
    /// Opcode: 0xFE
    /// T-states: 7
    pub(crate) fn cp_n(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let value = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        self.cp_a(value);
        InstructionResult::new(7)
    }

    // ========================
    // Arithmetic 8-bit: (HL) operands, ADC and SBC
    // ========================

    /// ADD A, (HL) - Add memory value to A
    /// Opcode: 0x86
    /// T-states: 7
    pub(crate) fn add_a_hl(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let hl = self.read_reg16(Reg16::HL);
        let value = bus.read(hl as u32);
        self.add_a(value);
        InstructionResult::new(7)
    }

    /// ADC A, r - Add register to A with carry
    /// Opcodes: 0x88-0x8F (except 0x8E)
    /// T-states: 4
    pub(crate) fn adc_a_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        self.adc_a(value);
        InstructionResult::new(4)
    }

    /// ADC A, (HL) - Add memory value to A with carry
    /// Opcode: 0x8E
    /// T-states: 7
    pub(crate) fn adc_a_hl(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let hl = self.read_reg16(Reg16::HL);
        let value = bus.read(hl as u32);
        self.adc_a(value);
        InstructionResult::new(7)
    }

    /// ADC A, n - Add immediate to A with carry
    /// Opcode: 0xCE
    /// T-states: 7
    pub(crate) fn adc_a_n(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let value = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        self.adc_a(value);
        InstructionResult::new(7)
    }

    /// SUB (HL) - Subtract memory value from A
    /// Opcode: 0x96
    /// T-states: 7
    pub(crate) fn sub_hl(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let hl = self.read_reg16(Reg16::HL);
        let value = bus.read(hl as u32);
        self.sub_a(value);
        InstructionResult::new(7)
    }

    /// SBC A, r - Subtract register from A with carry
    /// Opcodes: 0x98-0x9F (except 0x9E)
    /// T-states: 4
    pub(crate) fn sbc_a_r(&mut self, reg: Reg8) -> InstructionResult {
        let value = self.read_reg8(reg);
        self.sbc_a(value);
        InstructionResult::new(4)
    }

    /// SBC A, (HL) - Subtract memory value from A with carry
    /// Opcode: 0x9E
    /// T-states: 7
    pub(crate) fn sbc_a_hl(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let hl = self.read_reg16(Reg16::HL);
        let value = bus.read(hl as u32);
        self.sbc_a(value);
        InstructionResult::new(7)
    }

    /// SBC A, n - Subtract immediate from A with carry
    /// Opcode: 0xDE
    /// T-states: 7
    pub(crate) fn sbc_a_n(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let value = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        self.sbc_a(value);
        InstructionResult::new(7)
    }

    /// CP (HL) - Compare memory value with A
    /// Opcode: 0xBE
    /// T-states: 7
    pub(crate) fn cp_hl(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let hl = self.read_reg16(Reg16::HL);
        let value = bus.read(hl as u32);
        self.cp_a(value);
        InstructionResult::new(7)
    }

    // ========================
    // Helper functions
    // ========================

    /// ADD helper - performs addition and sets flags
    pub(crate) fn add_a(&mut self, value: u8) {
        let a = self.a;
        let result = a.wrapping_add(value);

        let overflow = ((a ^ result) & (value ^ result) & 0x80) != 0;

        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_h((a & 0x0F) + (value & 0x0F) > 0x0F);
        self.set_flag_x(result & 0x08 != 0);
        self.set_flag_pv(overflow);
        self.set_flag_n(false);
        self.set_flag_c(a as u16 + value as u16 > 0xFF);

        self.a = result;
    }

    /// SUB helper - performs subtraction and sets flags
    pub(crate) fn sub_a(&mut self, value: u8) {
        let a = self.a;
        let result = a.wrapping_sub(value);

        let overflow = ((a ^ value) & (a ^ result) & 0x80) != 0;

        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_h((a & 0x0F) < (value & 0x0F));
        self.set_flag_x(result & 0x08 != 0);
        self.set_flag_pv(overflow);
        self.set_flag_n(true);
        self.set_flag_c(a < value);

        self.a = result;
    }

    /// CP helper - performs comparison (subtraction without storing result)
    /// Note: X/Y flags come from the OPERAND, not the result (Z80 peculiarity)
    pub(crate) fn cp_a(&mut self, value: u8) {
        let a = self.a;
        let result = a.wrapping_sub(value);

        let overflow = ((a ^ value) & (a ^ result) & 0x80) != 0;

        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(value & 0x20 != 0); // From operand, not result
        self.set_flag_h((a & 0x0F) < (value & 0x0F));
        self.set_flag_x(value & 0x08 != 0); // From operand, not result
        self.set_flag_pv(overflow);
        self.set_flag_n(true);
        self.set_flag_c(a < value);
    }

    /// ADC helper - performs addition with carry and sets flags
    pub(crate) fn adc_a(&mut self, value: u8) {
        let a = self.a;
        let carry = if self.flag_c() { 1 } else { 0 };
        let result = a.wrapping_add(value).wrapping_add(carry);

        let overflow = ((a ^ result) & (value ^ result) & 0x80) != 0;

        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_h((a & 0x0F) + (value & 0x0F) + carry > 0x0F);
        self.set_flag_x(result & 0x08 != 0);
        self.set_flag_pv(overflow);
        self.set_flag_n(false);
        self.set_flag_c((a as u16 + value as u16 + carry as u16) > 0xFF);

        self.a = result;
    }

    /// SBC helper - performs subtraction with carry and sets flags
    pub(crate) fn sbc_a(&mut self, value: u8) {
        let a = self.a;
        let carry = if self.flag_c() { 1 } else { 0 };
        let result = a.wrapping_sub(value).wrapping_sub(carry);

        let overflow = ((a ^ value) & (a ^ result) & 0x80) != 0;

        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_h((a & 0x0F) < ((value & 0x0F) + carry));
        self.set_flag_x(result & 0x08 != 0);
        self.set_flag_pv(overflow);
        self.set_flag_n(true);
        self.set_flag_c((a as u16) < (value as u16 + carry as u16));

        self.a = result;
    }

    // ========================
    // Arithmetic 16-bit
    // ========================

    /// INC rr - Increment 16-bit register pair
    /// Opcodes: 0x03 (BC), 0x13 (DE), 0x23 (HL), 0x33 (SP)
    /// T-states: 6
    /// Flags: None affected
    pub(crate) fn inc_rr(&mut self, reg: Reg16) -> InstructionResult {
        let value = self.read_reg16(reg);
        let result = value.wrapping_add(1);
        self.write_reg16(reg, result);
        InstructionResult::new(6)
    }

    /// DEC rr - Decrement 16-bit register pair
    /// Opcodes: 0x0B (BC), 0x1B (DE), 0x2B (HL), 0x3B (SP)
    /// T-states: 6
    /// Flags: None affected
    pub(crate) fn dec_rr(&mut self, reg: Reg16) -> InstructionResult {
        let value = self.read_reg16(reg);
        let result = value.wrapping_sub(1);
        self.write_reg16(reg, result);
        InstructionResult::new(6)
    }

    /// ADD HL, rr - Add 16-bit register pair to HL
    /// Opcodes: 0x09 (BC), 0x19 (DE), 0x29 (HL), 0x39 (SP)
    /// T-states: 11
    /// Flags: H, N (reset), C
    pub(crate) fn add_hl_rr(&mut self, reg: Reg16) -> InstructionResult {
        let hl = self.read_reg16(Reg16::HL);
        let value = self.read_reg16(reg);
        let result = hl.wrapping_add(value);
        let result_high = (result >> 8) as u8;

        // WZ = HL + 1 (before the addition)
        self.wz = hl.wrapping_add(1);

        // X/Y from high byte of result
        self.set_flag_y(result_high & 0x20 != 0);
        self.set_flag_h((hl & 0x0FFF) + (value & 0x0FFF) > 0x0FFF);
        self.set_flag_x(result_high & 0x08 != 0);
        self.set_flag_n(false);
        self.set_flag_c(hl as u32 + value as u32 > 0xFFFF);

        self.write_reg16(Reg16::HL, result);
        InstructionResult::new(11)
    }

    /// ADC HL, rr - Add 16-bit register pair to HL with carry
    /// Opcodes: ED 4A (BC), ED 5A (DE), ED 6A (HL), ED 7A (SP)
    /// T-states: 15
    /// Flags: S, Z, H, P/V (overflow), N (reset), C
    pub(crate) fn adc_hl_rr(&mut self, reg: Reg16) -> InstructionResult {
        let hl = self.read_reg16(Reg16::HL);
        let value = self.read_reg16(reg);
        let carry = if self.flag_c() { 1 } else { 0 };

        let result32 = hl as u32 + value as u32 + carry;
        let result = result32 as u16;

        // S flag: bit 15 of result
        self.set_flag_s(result & 0x8000 != 0);

        // Z flag: result is zero
        self.set_flag_z(result == 0);

        // H flag: carry from bit 11 to bit 12
        self.set_flag_h((hl & 0x0FFF) + (value & 0x0FFF) + carry as u16 > 0x0FFF);

        // P/V flag: overflow (operands same sign, result different sign)
        let overflow = ((hl ^ result) & (value ^ result) & 0x8000) != 0;
        self.set_flag_pv(overflow);

        // N flag: reset
        self.set_flag_n(false);

        // C flag: carry from bit 15
        self.set_flag_c(result32 > 0xFFFF);

        // WZ = HL + 1 (initial HL before addition)
        self.wz = hl.wrapping_add(1);

        // X/Y flags from high byte of 16-bit result
        let result_high = (result >> 8) as u8;
        self.set_flag_y(result_high & 0x20 != 0);
        self.set_flag_x(result_high & 0x08 != 0);

        self.write_reg16(Reg16::HL, result);
        InstructionResult::new(15)
    }

    /// SBC HL, rr - Subtract 16-bit register pair from HL with carry
    /// Opcodes: ED 42 (BC), ED 52 (DE), ED 62 (HL), ED 72 (SP)
    /// T-states: 15
    /// Flags: S, Z, H, P/V (overflow), N (set), C
    pub(crate) fn sbc_hl_rr(&mut self, reg: Reg16) -> InstructionResult {
        let hl = self.read_reg16(Reg16::HL);
        let value = self.read_reg16(reg);
        let carry = if self.flag_c() { 1 } else { 0 };

        let result32 = (hl as i32) - (value as i32) - carry;
        let result = result32 as u16;

        // S flag: bit 15 of result
        self.set_flag_s(result & 0x8000 != 0);

        // Z flag: result is zero
        self.set_flag_z(result == 0);

        // H flag: borrow from bit 12
        self.set_flag_h((hl & 0x0FFF) < ((value & 0x0FFF) + carry as u16));

        // P/V flag: overflow (operands different signs, result different sign from minuend)
        let overflow = ((hl ^ value) & (hl ^ result) & 0x8000) != 0;
        self.set_flag_pv(overflow);

        // N flag: set
        self.set_flag_n(true);

        // C flag: borrow
        self.set_flag_c(result32 < 0);

        // WZ = HL + 1 (initial HL before subtraction)
        self.wz = hl.wrapping_add(1);

        // X/Y flags from high byte of 16-bit result
        let result_high = (result >> 8) as u8;
        self.set_flag_y(result_high & 0x20 != 0);
        self.set_flag_x(result_high & 0x08 != 0);

        self.write_reg16(Reg16::HL, result);
        InstructionResult::new(15)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_add_a_b() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x10;
        cpu.b = 0x20;
        ram.write(0x0000, 0x80); // ADD A, B

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x30);
        assert!(!cpu.flag_s());
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_h());
        assert!(!cpu.flag_pv());
        assert!(!cpu.flag_n());
        assert!(!cpu.flag_c());
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_add_a_n() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x50;
        ram.write(0x0000, 0xC6); // ADD A, n
        ram.write(0x0001, 0x30);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x80);
        assert!(cpu.flag_s()); // Bit 7 set
        assert!(!cpu.flag_z());
        assert!(cpu.flag_pv()); // Overflow 0x50 + 0x30 = 0x80
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_sub_a_b() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x30;
        cpu.b = 0x10;
        ram.write(0x0000, 0x90); // SUB B

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x20);
        assert!(!cpu.flag_s());
        assert!(!cpu.flag_z());
        assert!(cpu.flag_n());
        assert!(!cpu.flag_c());
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_sub_n() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x10;
        ram.write(0x0000, 0xD6); // SUB n
        ram.write(0x0001, 0x20);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0xF0);
        assert!(cpu.flag_s()); // Negative result
        assert!(cpu.flag_c()); // Borrow
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_inc_b() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.b = 0x0F;
        ram.write(0x0000, 0x04); // INC B

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.b, 0x10);
        assert!(cpu.flag_h()); // Half-carry from bit 3 to 4
        assert!(!cpu.flag_n());
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_dec_c() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.c = 0x10;
        ram.write(0x0000, 0x0D); // DEC C

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.c, 0x0F);
        assert!(cpu.flag_n());
        assert!(cpu.flag_h()); // Half-borrow
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_cp_a_b() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x20;
        cpu.b = 0x20;
        ram.write(0x0000, 0xB8); // CP B

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x20); // A unchanged
        assert!(cpu.flag_z()); // Equal
        assert!(!cpu.flag_c());
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_cp_n() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x10;
        ram.write(0x0000, 0xFE); // CP n
        ram.write(0x0001, 0x20);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x10); // A unchanged
        assert!(cpu.flag_c()); // A < n
        assert_eq!(cycles, 7);
    }

    // ========================
    // Arithmetic 16-bit Tests
    // ========================

    #[test]
    fn test_inc_bc() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.b = 0x10;
        cpu.c = 0xFF;
        ram.write(0x0000, 0x03); // INC BC

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.b, 0x11);
        assert_eq!(cpu.c, 0x00); // Wrap around
        assert_eq!(cycles, 6);
    }

    #[test]
    fn test_dec_de() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.d = 0x10;
        cpu.e = 0x00;
        ram.write(0x0000, 0x1B); // DEC DE

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.d, 0x0F);
        assert_eq!(cpu.e, 0xFF); // Borrow
        assert_eq!(cycles, 6);
    }

    #[test]
    fn test_add_hl_bc() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x10;
        cpu.l = 0x00;
        cpu.b = 0x05;
        cpu.c = 0x50;
        ram.write(0x0000, 0x09); // ADD HL, BC

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.h, 0x15);
        assert_eq!(cpu.l, 0x50);
        assert!(!cpu.flag_n());
        assert!(!cpu.flag_c());
        assert_eq!(cycles, 11);
    }

    #[test]
    fn test_add_hl_hl_overflow() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x80;
        cpu.l = 0x00;
        ram.write(0x0000, 0x29); // ADD HL, HL

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.h, 0x00);
        assert_eq!(cpu.l, 0x00);
        assert!(cpu.flag_c()); // Carry
        assert_eq!(cycles, 11);
    }

    #[test]
    fn test_add_hl_sp() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x12;
        cpu.l = 0x34;
        cpu.sp = 0xABCD;
        ram.write(0x0000, 0x39); // ADD HL, SP

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        let result = 0x1234u16.wrapping_add(0xABCD);
        assert_eq!(((cpu.h as u16) << 8) | (cpu.l as u16), result);
        assert_eq!(cycles, 11);
    }

    #[test]
    fn test_adc_hl_bc_no_carry() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x10;
        cpu.l = 0x00;
        cpu.b = 0x05;
        cpu.c = 0x50;
        cpu.set_flag_c(false);
        ram.write(0x0000, 0xED); // ADC HL, BC
        ram.write(0x0001, 0x4A);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.h, 0x15);
        assert_eq!(cpu.l, 0x50);
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_c());
        assert_eq!(cycles, 15);
    }

    #[test]
    fn test_adc_hl_de_with_carry() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x10;
        cpu.l = 0x00;
        cpu.d = 0x05;
        cpu.e = 0x50;
        cpu.set_flag_c(true);
        ram.write(0x0000, 0xED); // ADC HL, DE
        ram.write(0x0001, 0x5A);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.h, 0x15);
        assert_eq!(cpu.l, 0x51); // +1 from carry
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_c());
        assert_eq!(cycles, 15);
    }

    #[test]
    fn test_adc_hl_hl_zero() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x00;
        cpu.l = 0x00;
        cpu.set_flag_c(false);
        ram.write(0x0000, 0xED); // ADC HL, HL
        ram.write(0x0001, 0x6A);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.h, 0x00);
        assert_eq!(cpu.l, 0x00);
        assert!(cpu.flag_z()); // Zero flag
        assert_eq!(cycles, 15);
    }

    #[test]
    fn test_sbc_hl_bc_no_borrow() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x20;
        cpu.l = 0x00;
        cpu.b = 0x10;
        cpu.c = 0x00;
        cpu.set_flag_c(false);
        ram.write(0x0000, 0xED); // SBC HL, BC
        ram.write(0x0001, 0x42);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.h, 0x10);
        assert_eq!(cpu.l, 0x00);
        assert!(cpu.flag_n()); // Subtract flag
        assert!(!cpu.flag_c());
        assert_eq!(cycles, 15);
    }

    #[test]
    fn test_sbc_hl_de_with_borrow() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x10;
        cpu.l = 0x00;
        cpu.d = 0x05;
        cpu.e = 0x50;
        cpu.set_flag_c(true);
        ram.write(0x0000, 0xED); // SBC HL, DE
        ram.write(0x0001, 0x52);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.h, 0x0A);
        assert_eq!(cpu.l, 0xAF); // -1 from borrow
        assert!(cpu.flag_n());
        assert!(!cpu.flag_c());
        assert_eq!(cycles, 15);
    }

    #[test]
    fn test_sbc_hl_hl_zero() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x12;
        cpu.l = 0x34;
        cpu.set_flag_c(false);
        ram.write(0x0000, 0xED); // SBC HL, HL
        ram.write(0x0001, 0x62);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.h, 0x00);
        assert_eq!(cpu.l, 0x00);
        assert!(cpu.flag_z()); // Zero flag
        assert!(cpu.flag_n());
        assert_eq!(cycles, 15);
    }

    #[test]
    fn test_inc_sp() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0xFFFF;
        ram.write(0x0000, 0x33); // INC SP

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.sp, 0x0000); // Wrap around
        assert_eq!(cycles, 6);
    }

    #[test]
    fn test_dec_sp() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0x0000;
        ram.write(0x0000, 0x3B); // DEC SP

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.sp, 0xFFFF); // Wrap around
        assert_eq!(cycles, 6);
    }

    // ========================================
    // Arithmetic 8-bit Tests: (HL) operands, ADC and SBC
    // ========================================

    #[test]
    fn test_add_a_hl() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x10;
        cpu.h = 0x80;
        cpu.l = 0x00;
        ram.write(0x8000, 0x25); // Value at (HL)

        cpu.pc = 0x0000;
        ram.write(0x0000, 0x86); // ADD A, (HL)

        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x35); // 0x10 + 0x25
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_n());
        assert!(!cpu.flag_c());
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_adc_a_r_no_carry() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x10;
        cpu.b = 0x20;
        cpu.set_flag_c(false);

        ram.write(0x0000, 0x88); // ADC A, B

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x30); // 0x10 + 0x20 + 0
        assert!(!cpu.flag_c());
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_adc_a_r_with_carry() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x10;
        cpu.b = 0x20;
        cpu.set_flag_c(true);

        ram.write(0x0000, 0x88); // ADC A, B

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x31); // 0x10 + 0x20 + 1
        assert!(!cpu.flag_c());
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_adc_a_hl() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0xFF;
        cpu.h = 0x80;
        cpu.l = 0x00;
        ram.write(0x8000, 0x01);
        cpu.set_flag_c(true);

        ram.write(0x0000, 0x8E); // ADC A, (HL)

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x01); // 0xFF + 0x01 + 1 = 0x101 (overflow)
        assert!(cpu.flag_c()); // Carry
        assert!(!cpu.flag_z()); // Result is 0x01 (not zero)
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_adc_a_n() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x50;
        cpu.set_flag_c(false);

        ram.write(0x0000, 0xCE); // ADC A, n
        ram.write(0x0001, 0x30);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x80); // 0x50 + 0x30
        assert!(!cpu.flag_c());
        assert!(cpu.flag_s()); // Sign flag (bit 7 set)
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_sub_hl() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x50;
        cpu.h = 0x80;
        cpu.l = 0x00;
        ram.write(0x8000, 0x30);

        ram.write(0x0000, 0x96); // SUB (HL)

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x20); // 0x50 - 0x30
        assert!(!cpu.flag_c());
        assert!(cpu.flag_n());
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_sbc_a_r_no_carry() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x50;
        cpu.c = 0x20;
        cpu.set_flag_c(false);

        ram.write(0x0000, 0x99); // SBC A, C

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x30); // 0x50 - 0x20 - 0
        assert!(!cpu.flag_c());
        assert!(cpu.flag_n());
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_sbc_a_r_with_carry() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x50;
        cpu.c = 0x20;
        cpu.set_flag_c(true);

        ram.write(0x0000, 0x99); // SBC A, C

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x2F); // 0x50 - 0x20 - 1
        assert!(!cpu.flag_c());
        assert!(cpu.flag_n());
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_sbc_a_hl() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x10;
        cpu.h = 0x80;
        cpu.l = 0x00;
        ram.write(0x8000, 0x20);
        cpu.set_flag_c(true);

        ram.write(0x0000, 0x9E); // SBC A, (HL)

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0xEF); // 0x10 - 0x20 - 1 (underflow)
        assert!(cpu.flag_c()); // Borrow
        assert!(cpu.flag_n());
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_sbc_a_n() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x80;
        cpu.set_flag_c(false);

        ram.write(0x0000, 0xDE); // SBC A, n
        ram.write(0x0001, 0x30);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x50); // 0x80 - 0x30
        assert!(!cpu.flag_c());
        assert!(cpu.flag_n());
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_cp_hl() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x50;
        cpu.h = 0x80;
        cpu.l = 0x00;
        ram.write(0x8000, 0x50);

        ram.write(0x0000, 0xBE); // CP (HL)

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x50); // A unchanged
        assert!(cpu.flag_z()); // Equal
        assert!(cpu.flag_n());
        assert!(!cpu.flag_c());
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_inc_hl_indirect() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x80;
        cpu.l = 0x00;
        ram.write(0x8000, 0x7F); // Value at (HL)

        ram.write(0x0000, 0x34); // INC (HL)

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(ram.read(0x8000), 0x80); // 0x7F + 1
        assert!(cpu.flag_pv()); // Overflow from 0x7F to 0x80
        assert!(!cpu.flag_n());
        assert_eq!(cycles, 11);
    }

    #[test]
    fn test_dec_hl_indirect() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x80;
        cpu.l = 0x00;
        ram.write(0x8000, 0x80); // Value at (HL)

        ram.write(0x0000, 0x35); // DEC (HL)

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(ram.read(0x8000), 0x7F); // 0x80 - 1
        assert!(cpu.flag_pv()); // Overflow from 0x80 to 0x7F
        assert!(cpu.flag_n());
        assert_eq!(cycles, 11);
    }

    #[test]
    fn test_inc_hl_indirect_zero() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x80;
        cpu.l = 0x00;
        ram.write(0x8000, 0xFF);

        ram.write(0x0000, 0x34); // INC (HL)

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(ram.read(0x8000), 0x00);
        assert!(cpu.flag_z()); // Zero flag
        assert!(!cpu.flag_n());
        assert_eq!(cycles, 11);
    }
}
