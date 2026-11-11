//! Miscellaneous Z80 instructions (NOP, HALT, etc.)

use super::*;
use crate::z80::addressing::Reg16;
use retro_core::cpu::Addressable;

impl Z80 {
    // ========================
    // Misc / System Control
    // ========================

    /// NOP - No operation
    /// Opcode: 0x00
    /// T-states: 4
    pub(crate) fn nop(&mut self) -> InstructionResult {
        InstructionResult::new(4)
    }

    /// HALT - Halt CPU until interrupt or reset
    /// Opcode: 0x76
    /// T-states: 4
    pub(crate) fn halt(&mut self) -> InstructionResult {
        self.halted = true;
        InstructionResult::new(4)
    }

    /// DI - Disable Interrupts
    /// Opcode: 0xF3
    /// T-states: 4
    pub(crate) fn di(&mut self) -> InstructionResult {
        self.iff1 = false;
        self.iff2 = false;
        InstructionResult::new(4)
    }

    /// EI - Enable Interrupts
    /// Opcode: 0xFB
    /// T-states: 4
    pub(crate) fn ei(&mut self) -> InstructionResult {
        self.iff1 = true;
        self.iff2 = true;
        self.ei = 1; // Signal that EI was just executed (delays interrupt by one instruction)
        InstructionResult::new(4)
    }

    /// SCF - Set Carry Flag
    /// Opcode: 0x37
    /// T-states: 4
    pub(crate) fn scf(&mut self) -> InstructionResult {
        // Undocumented X/Y behavior depends on Q from previous instruction:
        // If prev_q != 0: X/Y from A
        // If prev_q == 0: X/Y from A OR F
        let xy_source = if self.prev_q != 0 {
            self.a
        } else {
            self.a | self.f
        };
        self.set_flag_y(xy_source & 0x20 != 0);
        self.set_flag_h(false);
        self.set_flag_x(xy_source & 0x08 != 0);
        self.set_flag_n(false);
        self.set_flag_c(true);
        InstructionResult::new(4)
    }

    /// CCF - Complement Carry Flag
    /// Opcode: 0x3F
    /// T-states: 4
    pub(crate) fn ccf(&mut self) -> InstructionResult {
        let carry = self.flag_c();
        let xy_source = if self.prev_q != 0 {
            self.a
        } else {
            self.a | self.f
        };
        self.set_flag_y(xy_source & 0x20 != 0);
        self.set_flag_h(carry); // H = previous C
        self.set_flag_x(xy_source & 0x08 != 0);
        self.set_flag_n(false);
        self.set_flag_c(!carry);
        InstructionResult::new(4)
    }

    /// CPL - Complement A (bitwise NOT)
    /// Opcode: 0x2F
    /// T-states: 4
    pub(crate) fn cpl(&mut self) -> InstructionResult {
        self.a = !self.a;
        // X/Y flags come from A after complement
        self.set_flag_y(self.a & 0x20 != 0);
        self.set_flag_h(true);
        self.set_flag_x(self.a & 0x08 != 0);
        self.set_flag_n(true);
        InstructionResult::new(4)
    }

    /// NEG - Negate A (two's complement)
    /// Opcode: 0xED 0x44
    /// T-states: 8
    pub(crate) fn neg(&mut self) -> InstructionResult {
        let a = self.a;
        let result = 0u8.wrapping_sub(a);

        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_h((a & 0x0F) != 0);
        self.set_flag_x(result & 0x08 != 0);
        self.set_flag_pv(a == 0x80);
        self.set_flag_n(true);
        self.set_flag_c(a != 0);

        self.a = result;
        InstructionResult::new(8)
    }

    /// DAA - Decimal Adjust Accumulator (BCD)
    /// Opcode: 0x27
    /// T-states: 4
    pub(crate) fn daa(&mut self) -> InstructionResult {
        let mut a = self.a;
        let mut adjust = 0u8;
        let mut carry = self.flag_c();
        let n = self.flag_n();
        let h = self.flag_h();

        if h || (a & 0x0F) > 9 {
            adjust |= 0x06;
        }

        if carry || a > 0x99 {
            adjust |= 0x60;
            carry = true;
        }

        if n {
            a = a.wrapping_sub(adjust);
            // H flag after subtraction: set if low nibble borrow occurred
            self.set_flag_h(h && (self.a & 0x0F) < 6);
        } else {
            // H flag after addition: set if low nibble carry occurred
            self.set_flag_h((self.a & 0x0F) > 9);
            a = a.wrapping_add(adjust);
        }

        self.a = a;
        self.set_flag_s(a & 0x80 != 0);
        self.set_flag_z(a == 0);
        self.set_flag_y(a & 0x20 != 0);
        self.set_flag_x(a & 0x08 != 0);
        self.set_flag_pv(a.count_ones().is_multiple_of(2));
        self.set_flag_c(carry);

        InstructionResult::new(4)
    }

    // ========================
    // Exchange Instructions
    // ========================

    /// EX DE, HL - Exchange DE and HL
    /// Opcode: 0xEB
    /// T-states: 4
    pub(crate) fn ex_de_hl(&mut self) -> InstructionResult {
        let temp = self.read_reg16(Reg16::DE);
        self.write_reg16(Reg16::DE, self.read_reg16(Reg16::HL));
        self.write_reg16(Reg16::HL, temp);
        InstructionResult::new(4)
    }

    /// EX AF, AF' - Exchange AF and AF'
    /// Opcode: 0x08
    /// T-states: 4
    pub(crate) fn ex_af_af_prime(&mut self) -> InstructionResult {
        std::mem::swap(&mut self.a, &mut self.a_);
        std::mem::swap(&mut self.f, &mut self.f_);
        InstructionResult::new(4)
    }

    /// EX (SP), HL - Exchange (SP) and HL
    /// Opcode: 0xE3
    /// T-states: 19
    pub(crate) fn ex_sp_hl(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let sp = self.sp;
        let hl = self.read_reg16(Reg16::HL);

        let sp_low = bus.read(sp as u32);
        let sp_high = bus.read((sp.wrapping_add(1)) as u32);
        let sp_value = u16::from_le_bytes([sp_low, sp_high]);

        bus.write(sp as u32, (hl & 0xFF) as u8);
        bus.write((sp.wrapping_add(1)) as u32, (hl >> 8) as u8);

        self.write_reg16(Reg16::HL, sp_value);
        self.wz = sp_value;

        InstructionResult::new(19)
    }

    /// EXX - Exchange BC, DE, HL with shadow registers
    /// Opcode: 0xD9
    /// T-states: 4
    pub(crate) fn exx(&mut self) -> InstructionResult {
        std::mem::swap(&mut self.b, &mut self.b_);
        std::mem::swap(&mut self.c, &mut self.c_);
        std::mem::swap(&mut self.d, &mut self.d_);
        std::mem::swap(&mut self.e, &mut self.e_);
        std::mem::swap(&mut self.h, &mut self.h_);
        std::mem::swap(&mut self.l, &mut self.l_);
        InstructionResult::new(4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::cpu::Addressable;
    use retro_core::memory::Ram;

    #[test]
    fn test_nop() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        ram.write(0x0000, 0x00); // NOP

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x0001);
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_halt() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        ram.write(0x0000, 0x76); // HALT

        cpu.pc = 0x0000;
        assert!(!cpu.halted);

        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x0001);
        assert!(cpu.halted);
        assert_eq!(cycles, 4);

        // Next step should also return 4 cycles (halted loop)
        let cycles2 = cpu.step(&mut ram);
        assert_eq!(cycles2, 4);
        assert_eq!(cpu.pc, 0x0001); // PC doesn't advance when halted
    }

    #[test]
    fn test_di() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.iff1 = true;
        cpu.iff2 = true;
        ram.write(0x0000, 0xF3); // DI

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert!(!cpu.iff1);
        assert!(!cpu.iff2);
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_ei() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.iff1 = false;
        cpu.iff2 = false;
        ram.write(0x0000, 0xFB); // EI

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert!(cpu.iff1);
        assert!(cpu.iff2);
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_scf() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_flag_c(false);
        ram.write(0x0000, 0x37); // SCF

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert!(cpu.flag_c());
        assert!(!cpu.flag_h());
        assert!(!cpu.flag_n());
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_ccf() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_flag_c(true);
        ram.write(0x0000, 0x3F); // CCF

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert!(!cpu.flag_c()); // Complemented
        assert!(cpu.flag_h()); // H = previous C
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_cpl() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0b10101010;
        ram.write(0x0000, 0x2F); // CPL

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0b01010101);
        assert!(cpu.flag_h());
        assert!(cpu.flag_n());
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_neg() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x10;
        ram.write(0x0000, 0xED); // NEG prefix
        ram.write(0x0001, 0x44);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0xF0); // 0 - 0x10 = -16 = 0xF0
        assert!(cpu.flag_s()); // Negative
        assert!(cpu.flag_n());
        assert!(cpu.flag_c()); // Borrow occurred
        assert_eq!(cycles, 8);
    }

    #[test]
    fn test_daa() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        // Add 0x15 + 0x27 in BCD = 0x42 (but as binary it's 0x3C)
        cpu.a = 0x3C; // Result of 0x15 + 0x27
        cpu.set_flag_n(false); // Addition
        cpu.set_flag_h(false);
        cpu.set_flag_c(false);
        ram.write(0x0000, 0x27); // DAA

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x42); // Adjusted to BCD
        assert_eq!(cycles, 4);
    }

    // ========================
    // Exchange Tests
    // ========================

    #[test]
    fn test_ex_de_hl() {
        let mut cpu = Z80::new();
        cpu.d = 0x12;
        cpu.e = 0x34;
        cpu.h = 0x56;
        cpu.l = 0x78;

        cpu.ex_de_hl();

        assert_eq!(cpu.d, 0x56);
        assert_eq!(cpu.e, 0x78);
        assert_eq!(cpu.h, 0x12);
        assert_eq!(cpu.l, 0x34);
    }

    #[test]
    fn test_ex_af_af_prime() {
        let mut cpu = Z80::new();
        cpu.a = 0xAA;
        cpu.f = 0x55;
        cpu.a_ = 0x11;
        cpu.f_ = 0x22;

        cpu.ex_af_af_prime();

        assert_eq!(cpu.a, 0x11);
        assert_eq!(cpu.f, 0x22);
        assert_eq!(cpu.a_, 0xAA);
        assert_eq!(cpu.f_, 0x55);
    }

    #[test]
    fn test_ex_sp_hl() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0x1000;
        cpu.h = 0x12;
        cpu.l = 0x34;

        // Stack contains 0x5678
        ram.write(0x1000, 0x78);
        ram.write(0x1001, 0x56);

        cpu.ex_sp_hl(&mut ram);

        assert_eq!(cpu.h, 0x56);
        assert_eq!(cpu.l, 0x78);
        assert_eq!(ram.read(0x1000), 0x34);
        assert_eq!(ram.read(0x1001), 0x12);
    }

    #[test]
    fn test_exx() {
        let mut cpu = Z80::new();
        cpu.b = 0x11;
        cpu.c = 0x22;
        cpu.d = 0x33;
        cpu.e = 0x44;
        cpu.h = 0x55;
        cpu.l = 0x66;

        cpu.b_ = 0xAA;
        cpu.c_ = 0xBB;
        cpu.d_ = 0xCC;
        cpu.e_ = 0xDD;
        cpu.h_ = 0xEE;
        cpu.l_ = 0xFF;

        cpu.exx();

        assert_eq!(cpu.b, 0xAA);
        assert_eq!(cpu.c, 0xBB);
        assert_eq!(cpu.d, 0xCC);
        assert_eq!(cpu.e, 0xDD);
        assert_eq!(cpu.h, 0xEE);
        assert_eq!(cpu.l, 0xFF);

        assert_eq!(cpu.b_, 0x11);
        assert_eq!(cpu.c_, 0x22);
        assert_eq!(cpu.d_, 0x33);
        assert_eq!(cpu.e_, 0x44);
        assert_eq!(cpu.h_, 0x55);
        assert_eq!(cpu.l_, 0x66);
    }
}
