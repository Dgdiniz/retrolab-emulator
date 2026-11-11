use crate::z80::addressing::Reg16;
use crate::z80::{InstructionResult, Z80};
use retro_core::cpu::Addressable;

impl Z80 {
    // ========================
    // Stack Operations
    // ========================

    /// PUSH qq - Push register pair onto stack
    /// Opcodes: 0xC5 (BC), 0xD5 (DE), 0xE5 (HL), 0xF5 (AF)
    /// T-states: 11
    pub(crate) fn push_qq(&mut self, bus: &mut dyn Addressable, reg: Reg16) -> InstructionResult {
        let value = match reg {
            Reg16::BC => self.bc(),
            Reg16::DE => self.de(),
            Reg16::HL => self.hl(),
            Reg16::AF => ((self.a as u16) << 8) | (self.f as u16),
            Reg16::SP => self.sp,
            Reg16::IX => self.ix,
            Reg16::IY => self.iy,
        };

        // Push high byte first, so the pair lands little-endian (low byte at SP)
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp as u32, (value >> 8) as u8);
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp as u32, (value & 0xFF) as u8);

        InstructionResult::new(11)
    }

    /// POP qq - Pop register pair from stack
    /// Opcodes: 0xC1 (BC), 0xD1 (DE), 0xE1 (HL), 0xF1 (AF)
    /// T-states: 10
    pub(crate) fn pop_qq(&mut self, bus: &mut dyn Addressable, reg: Reg16) -> InstructionResult {
        let low = bus.read(self.sp as u32) as u16;
        self.sp = self.sp.wrapping_add(1);
        let high = bus.read(self.sp as u32) as u16;
        self.sp = self.sp.wrapping_add(1);

        let value = (high << 8) | low;

        match reg {
            Reg16::BC => self.set_bc(value),
            Reg16::DE => self.set_de(value),
            Reg16::HL => self.set_hl(value),
            Reg16::AF => {
                self.a = (value >> 8) as u8;
                self.f = (value & 0xFF) as u8;
            }
            Reg16::SP => self.sp = value,
            Reg16::IX => self.ix = value,
            Reg16::IY => self.iy = value,
        }

        InstructionResult::new(10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_push_bc() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0xFFFE;
        cpu.set_bc(0x1234);
        ram.write(0x0000, 0xC5); // PUSH BC

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.sp, 0xFFFC);
        assert_eq!(ram.read(0xFFFC), 0x34); // Low byte
        assert_eq!(ram.read(0xFFFD), 0x12); // High byte
        assert_eq!(cycles, 11);
    }

    #[test]
    fn test_pop_bc() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0xFFFC;
        ram.write(0xFFFC, 0x78); // Low
        ram.write(0xFFFD, 0x56); // High
        ram.write(0x0000, 0xC1); // POP BC

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.bc(), 0x5678);
        assert_eq!(cpu.sp, 0xFFFE);
        assert_eq!(cycles, 10);
    }

    #[test]
    fn test_push_pop_de() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0xFFF0;
        cpu.set_de(0xABCD);

        // PUSH DE
        ram.write(0x0000, 0xD5);
        cpu.pc = 0x0000;
        cpu.step(&mut ram);

        assert_eq!(cpu.sp, 0xFFEE);
        assert_eq!(ram.read(0xFFEE), 0xCD);
        assert_eq!(ram.read(0xFFEF), 0xAB);

        // Clear DE
        cpu.set_de(0x0000);

        // POP DE
        ram.write(0x0001, 0xD1);
        cpu.pc = 0x0001;
        cpu.step(&mut ram);

        assert_eq!(cpu.de(), 0xABCD);
        assert_eq!(cpu.sp, 0xFFF0);
    }

    #[test]
    fn test_push_pop_hl() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0xFFF0;
        cpu.set_hl(0x9876);

        // PUSH HL
        ram.write(0x0000, 0xE5);
        cpu.pc = 0x0000;
        cpu.step(&mut ram);

        assert_eq!(cpu.sp, 0xFFEE);

        // POP HL
        cpu.set_hl(0x0000);
        ram.write(0x0001, 0xE1);
        cpu.pc = 0x0001;
        cpu.step(&mut ram);

        assert_eq!(cpu.hl(), 0x9876);
        assert_eq!(cpu.sp, 0xFFF0);
    }

    #[test]
    fn test_push_pop_af() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0xFFF0;
        cpu.a = 0x42;
        cpu.f = 0x85; // Some flags

        // PUSH AF
        ram.write(0x0000, 0xF5);
        cpu.pc = 0x0000;
        cpu.step(&mut ram);

        assert_eq!(cpu.sp, 0xFFEE);
        assert_eq!(ram.read(0xFFEE), 0x85); // F (low byte)
        assert_eq!(ram.read(0xFFEF), 0x42); // A (high byte)

        // Clear AF
        cpu.a = 0x00;
        cpu.f = 0x00;

        // POP AF
        ram.write(0x0001, 0xF1);
        cpu.pc = 0x0001;
        cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x42);
        assert_eq!(cpu.f, 0x85);
        assert_eq!(cpu.sp, 0xFFF0);
    }

    #[test]
    fn test_stack_overflow() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0x0001;
        cpu.set_bc(0x1234);
        ram.write(0x0000, 0xC5); // PUSH BC

        cpu.pc = 0x0000;
        cpu.step(&mut ram);

        // Stack wraps around
        assert_eq!(cpu.sp, 0xFFFF);
    }
}
