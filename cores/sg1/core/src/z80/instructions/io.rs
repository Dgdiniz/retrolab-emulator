//! Z80 I/O Instructions
//!
//! Input/Output operations to I/O ports.
//! I/O operations use the bus's io_read/io_write methods, which provide
//! the full 16-bit address bus value during I/O cycles.

use crate::z80::addressing::Reg8;
use crate::z80::InstructionResult;
use crate::z80::Z80;
use retro_core::cpu::Addressable;

impl Z80 {
    // ========================================================================
    // Basic I/O
    // ========================================================================

    /// IN A, (n) - Input from port n to A
    /// Opcode: DB n
    /// Cycles: 11
    /// Flags: None affected
    ///
    /// The 16-bit port address is: A in high byte, n in low byte.
    pub(crate) fn in_a_n(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let n = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);

        let port = ((self.a as u16) << 8) | (n as u16);
        // WZ = port + 1 (must use original A, before read)
        self.wz = port.wrapping_add(1);
        self.a = bus.io_read(port);

        InstructionResult::new(11)
    }

    /// OUT (n), A - Output A to port n
    /// Opcode: D3 n
    /// Cycles: 11
    /// Flags: None affected
    ///
    /// The 16-bit port address is: A in high byte, n in low byte.
    pub(crate) fn out_n_a(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let n = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);

        let port = ((self.a as u16) << 8) | (n as u16);
        bus.io_write(port, self.a);

        // WZ low = (n + 1) & 0xFF, WZ high = A
        self.wz = ((self.a as u16) << 8) | (n.wrapping_add(1) as u16);

        InstructionResult::new(11)
    }

    // ========================================================================
    // Extended I/O (ED prefix)
    // ========================================================================

    /// IN r, (C) - Input from port (C) to register r
    /// Opcode: ED 40/48/50/58/60/68/78
    /// Cycles: 12
    /// Flags: S, Z, H=0, P/V=parity, N=0, C unchanged
    ///
    /// The 16-bit port address is: B in high byte, C in low byte.
    pub(crate) fn in_r_c(&mut self, bus: &mut dyn Addressable, reg: Reg8) -> InstructionResult {
        let port = ((self.b as u16) << 8) | (self.c as u16);
        let value = bus.io_read(port);

        self.write_reg8(reg, value);

        // WZ = BC + 1
        self.wz = port.wrapping_add(1);

        // Flags
        self.set_flag_s(value & 0x80 != 0);
        self.set_flag_z(value == 0);
        self.set_flag_h(false);
        self.set_flag_pv(value.count_ones().is_multiple_of(2)); // Parity
        self.set_flag_n(false);
        // Undocumented flags Y (bit 5) and X (bit 3) from result
        self.set_flag_y(value & 0x20 != 0);
        self.set_flag_x(value & 0x08 != 0);
        // Carry unchanged

        InstructionResult::new(12)
    }

    /// IN (C) - Input from port (C), flags only (undocumented)
    /// Opcode: ED 70
    /// Cycles: 12
    /// Flags: S, Z, H=0, P/V=parity, N=0, C unchanged
    pub(crate) fn in_c_flags_only(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let port = ((self.b as u16) << 8) | (self.c as u16);
        let value = bus.io_read(port);

        // Value is not stored anywhere, only flags are affected
        // WZ = BC + 1
        self.wz = port.wrapping_add(1);

        // Flags
        self.set_flag_s(value & 0x80 != 0);
        self.set_flag_z(value == 0);
        self.set_flag_h(false);
        self.set_flag_pv(value.count_ones().is_multiple_of(2)); // Parity
        self.set_flag_n(false);
        self.set_flag_y(value & 0x20 != 0);
        self.set_flag_x(value & 0x08 != 0);
        // Carry unchanged

        InstructionResult::new(12)
    }

    /// OUT (C), r - Output register r to port (C)
    /// Opcode: ED 41/49/51/59/61/69/79
    /// Cycles: 12
    /// Flags: None affected
    pub(crate) fn out_c_r(&mut self, bus: &mut dyn Addressable, reg: Reg8) -> InstructionResult {
        let port = ((self.b as u16) << 8) | (self.c as u16);
        let value = self.read_reg8(reg);

        bus.io_write(port, value);

        // WZ = BC + 1
        self.wz = port.wrapping_add(1);

        InstructionResult::new(12)
    }

    /// OUT (C), 0 - Output 0 to port (C) (undocumented)
    /// Opcode: ED 71
    /// Cycles: 12
    /// Flags: None affected
    pub(crate) fn out_c_zero(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let port = ((self.b as u16) << 8) | (self.c as u16);

        bus.io_write(port, 0);

        // WZ = BC + 1
        self.wz = port.wrapping_add(1);

        InstructionResult::new(12)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_in_a_n() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x0000;
        ram.write(0x0000, 0x42); // Port 0x42

        cpu.in_a_n(&mut ram);

        assert_eq!(cpu.a, 0xFF); // Default floating bus
        assert_eq!(cpu.pc, 0x0001);
    }

    #[test]
    fn test_out_n_a() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x0000;
        cpu.a = 0x55;
        ram.write(0x0000, 0x42); // Port 0x42

        cpu.out_n_a(&mut ram);

        assert_eq!(cpu.pc, 0x0001);
    }

    #[test]
    fn test_in_r_c() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.c = 0x80; // Port 0x80
        cpu.b = 0x00;

        cpu.in_r_c(&mut ram, Reg8::B);

        assert_eq!(cpu.b, 0xFF); // Default floating bus
        assert!(cpu.flag_s()); // Sign flag (0xFF is negative)
        assert!(!cpu.flag_z()); // Not zero
        assert!(!cpu.flag_h()); // H cleared
        assert!(cpu.flag_pv()); // Parity (0xFF has even parity)
        assert!(!cpu.flag_n()); // N cleared
    }

    #[test]
    fn test_in_c_flags_only() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.c = 0x42;
        cpu.b = 0x11; // Should not be modified
        cpu.set_flag_z(false);

        cpu.in_c_flags_only(&mut ram);

        // Flags are still set
        assert!(cpu.flag_s());
        // No register is modified
        assert_eq!(cpu.b, 0x11);
    }

    #[test]
    fn test_out_c_r() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.c = 0x80; // Port 0x80
        cpu.a = 0x42;

        cpu.out_c_r(&mut ram, Reg8::A);

        assert_eq!(cpu.a, 0x42); // Register unchanged
    }

    #[test]
    fn test_out_c_zero() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.c = 0x80;

        cpu.out_c_zero(&mut ram);
        // No visible side effects with default bus
    }

    #[test]
    fn test_in_preserves_carry() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.c = 0x00;
        cpu.set_flag_c(true);

        cpu.in_r_c(&mut ram, Reg8::A);

        assert!(cpu.flag_c()); // Carry preserved
    }
}
