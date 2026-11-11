use crate::z80::{InstructionResult, Z80};
use retro_core::cpu::Addressable;

impl Z80 {
    // ========================
    // Jumps and Calls
    // ========================

    /// JP nn - Unconditional jump to absolute address
    /// Opcode: 0xC3
    /// T-states: 10
    pub(crate) fn jp_nn(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let nn_low = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let nn_high = bus.read(self.pc as u32) as u16;
        let nn = (nn_high << 8) | nn_low;

        self.pc = nn;
        self.wz = nn;
        InstructionResult::new(10)
    }

    /// JP cc, nn - Conditional jump
    /// Opcodes: 0xC2 (NZ), 0xCA (Z), 0xD2 (NC), 0xDA (C),
    /// 0xE2 (PO), 0xEA (PE), 0xF2 (P), 0xFA (M)
    /// T-states: 10
    pub(crate) fn jp_cc_nn(
        &mut self,
        bus: &mut dyn Addressable,
        condition: bool,
    ) -> InstructionResult {
        let nn_low = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let nn_high = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let nn = (nn_high << 8) | nn_low;

        self.wz = nn;
        if condition {
            self.pc = nn;
        }
        InstructionResult::new(10)
    }

    /// DJNZ e - Decrement B and Jump if Not Zero
    /// Opcode: 0x10
    /// T-states: 13 (taken) / 8 (not taken)
    pub(crate) fn djnz(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let e = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        self.b = self.b.wrapping_sub(1);

        if self.b != 0 {
            self.pc = self.pc.wrapping_add(e as u16);
            self.wz = self.pc; // WZ = destination address
            InstructionResult::new(13)
        } else {
            InstructionResult::new(8)
        }
    }

    /// JR e - Relative jump
    /// Opcode: 0x18
    /// T-states: 12
    pub(crate) fn jr_e(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let e = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        self.pc = self.pc.wrapping_add(e as u16);
        self.wz = self.pc; // WZ = destination address
        InstructionResult::new(12)
    }

    /// JR NZ, e - Relative jump if not zero
    /// Opcode: 0x20
    /// T-states: 12 (taken) / 7 (not taken)
    pub(crate) fn jr_nz_e(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let e = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        if !self.flag_z() {
            self.pc = self.pc.wrapping_add(e as u16);
            self.wz = self.pc;
            InstructionResult::new(12)
        } else {
            InstructionResult::new(7)
        }
    }

    /// JR Z, e - Relative jump if zero
    /// Opcode: 0x28
    /// T-states: 12 (taken) / 7 (not taken)
    pub(crate) fn jr_z_e(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let e = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        if self.flag_z() {
            self.pc = self.pc.wrapping_add(e as u16);
            self.wz = self.pc;
            InstructionResult::new(12)
        } else {
            InstructionResult::new(7)
        }
    }

    /// JR NC, e - Relative jump if not carry
    /// Opcode: 0x30
    /// T-states: 12 (taken) / 7 (not taken)
    pub(crate) fn jr_nc_e(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let e = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        if !self.flag_c() {
            self.pc = self.pc.wrapping_add(e as u16);
            self.wz = self.pc;
            InstructionResult::new(12)
        } else {
            InstructionResult::new(7)
        }
    }

    /// JR C, e - Relative jump if carry
    /// Opcode: 0x38
    /// T-states: 12 (taken) / 7 (not taken)
    pub(crate) fn jr_c_e(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let e = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        if self.flag_c() {
            self.pc = self.pc.wrapping_add(e as u16);
            self.wz = self.pc;
            InstructionResult::new(12)
        } else {
            InstructionResult::new(7)
        }
    }

    /// CALL nn - Call subroutine
    /// Opcode: 0xCD
    /// T-states: 17
    pub(crate) fn call_nn(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let nn_low = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let nn_high = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let nn = (nn_high << 8) | nn_low;

        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp as u32, (self.pc >> 8) as u8);
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp as u32, (self.pc & 0xFF) as u8);

        self.pc = nn;
        self.wz = nn;
        InstructionResult::new(17)
    }

    /// CALL cc, nn - Conditional call
    /// Opcodes: 0xC4 (NZ), 0xCC (Z), 0xD4 (NC), 0xDC (C),
    /// 0xE4 (PO), 0xEC (PE), 0xF4 (P), 0xFC (M)
    /// T-states: 17 (taken), 10 (not taken)
    pub(crate) fn call_cc_nn(
        &mut self,
        bus: &mut dyn Addressable,
        condition: bool,
    ) -> InstructionResult {
        let nn_low = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let nn_high = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let nn = (nn_high << 8) | nn_low;

        self.wz = nn;
        if condition {
            self.sp = self.sp.wrapping_sub(1);
            bus.write(self.sp as u32, (self.pc >> 8) as u8);
            self.sp = self.sp.wrapping_sub(1);
            bus.write(self.sp as u32, (self.pc & 0xFF) as u8);

            self.pc = nn;
            InstructionResult::new(17)
        } else {
            InstructionResult::new(10)
        }
    }

    /// RET - Return from subroutine
    /// Opcode: 0xC9
    /// T-states: 10
    pub(crate) fn ret(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let low = bus.read(self.sp as u32) as u16;
        self.sp = self.sp.wrapping_add(1);
        let high = bus.read(self.sp as u32) as u16;
        self.sp = self.sp.wrapping_add(1);

        self.pc = (high << 8) | low;
        self.wz = self.pc;
        InstructionResult::new(10)
    }

    /// RET cc - Conditional return
    /// Opcodes: 0xC0 (NZ), 0xC8 (Z), 0xD0 (NC), 0xD8 (C),
    /// 0xE0 (PO), 0xE8 (PE), 0xF0 (P), 0xF8 (M)
    /// T-states: 11 (taken), 5 (not taken)
    pub(crate) fn ret_cc(
        &mut self,
        bus: &mut dyn Addressable,
        condition: bool,
    ) -> InstructionResult {
        if condition {
            let low = bus.read(self.sp as u32) as u16;
            self.sp = self.sp.wrapping_add(1);
            let high = bus.read(self.sp as u32) as u16;
            self.sp = self.sp.wrapping_add(1);

            self.pc = (high << 8) | low;
            self.wz = self.pc;
            InstructionResult::new(11)
        } else {
            InstructionResult::new(5)
        }
    }

    /// JP (HL) - Jump to address in HL
    /// Opcode: 0xE9
    /// T-states: 4
    pub(crate) fn jp_hl(&mut self) -> InstructionResult {
        let hl = ((self.h as u16) << 8) | (self.l as u16);
        self.pc = hl;
        InstructionResult::new(4)
    }

    /// JP (IX) - Jump to address in IX
    /// Opcode: DD E9
    /// T-states: 8
    pub(crate) fn jp_ix(&mut self) -> InstructionResult {
        self.pc = self.ix;
        InstructionResult::new(8)
    }

    /// JP (IY) - Jump to address in IY
    /// Opcode: FD E9
    /// T-states: 8
    pub(crate) fn jp_iy(&mut self) -> InstructionResult {
        self.pc = self.iy;
        InstructionResult::new(8)
    }

    /// RST p - Restart to address p (00h, 08h, 10h, 18h, 20h, 28h, 30h, 38h)
    /// Opcodes: 0xC7, 0xCF, 0xD7, 0xDF, 0xE7, 0xEF, 0xF7, 0xFF
    /// T-states: 11
    pub(crate) fn rst(&mut self, bus: &mut dyn Addressable, addr: u16) -> InstructionResult {
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp as u32, (self.pc >> 8) as u8);
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp as u32, self.pc as u8);

        self.pc = addr;
        self.wz = addr;
        InstructionResult::new(11)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_jp_nn() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        ram.write(0x0000, 0xC3); // JP nn
        ram.write(0x0001, 0x34); // low
        ram.write(0x0002, 0x12); // high = 0x1234

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x1234);
        assert_eq!(cycles, 10);
    }

    #[test]
    fn test_jr_e_forward() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        ram.write(0x1000, 0x18); // JR e
        ram.write(0x1001, 0x10); // +16

        cpu.pc = 0x1000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x1012); // 0x1002 + 0x10
        assert_eq!(cycles, 12);
    }

    #[test]
    fn test_jr_e_backward() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        ram.write(0x1000, 0x18); // JR e
        ram.write(0x1001, 0xFE); // -2 (signed)

        cpu.pc = 0x1000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x1000); // 0x1002 + (-2) = 0x1000
        assert_eq!(cycles, 12);
    }

    #[test]
    fn test_jr_nz_e_taken() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_flag_z(false); // Not zero
        ram.write(0x1000, 0x20); // JR NZ, e
        ram.write(0x1001, 0x05);

        cpu.pc = 0x1000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x1007); // Jump taken
        assert_eq!(cycles, 12);
    }

    #[test]
    fn test_jr_nz_e_not_taken() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_flag_z(true); // Zero
        ram.write(0x1000, 0x20); // JR NZ, e
        ram.write(0x1001, 0x05);

        cpu.pc = 0x1000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x1002); // Jump not taken
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_jr_z_e_taken() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_flag_z(true); // Zero
        ram.write(0x1000, 0x28); // JR Z, e
        ram.write(0x1001, 0x03);

        cpu.pc = 0x1000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x1005); // Jump taken
        assert_eq!(cycles, 12);
    }

    #[test]
    fn test_call_ret() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0xFFFE; // Stack pointer

        // CALL 0x2000
        ram.write(0x1000, 0xCD); // CALL nn
        ram.write(0x1001, 0x00);
        ram.write(0x1002, 0x20); // 0x2000

        // RET at 0x2000
        ram.write(0x2000, 0xC9); // RET

        cpu.pc = 0x1000;
        let cycles_call = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x2000); // Jumped to subroutine
        assert_eq!(cpu.sp, 0xFFFC); // SP decremented by 2
        assert_eq!(cycles_call, 17);

        // Check return address on stack
        assert_eq!(ram.read(0xFFFC), 0x03); // Low byte of 0x1003
        assert_eq!(ram.read(0xFFFD), 0x10); // High byte of 0x1003

        let cycles_ret = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x1003); // Returned
        assert_eq!(cpu.sp, 0xFFFE); // SP restored
        assert_eq!(cycles_ret, 10);
    }

    #[test]
    fn test_jr_c_taken() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_flag_c(true); // Carry set
        ram.write(0x1000, 0x38); // JR C, e
        ram.write(0x1001, 0x08);

        cpu.pc = 0x1000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x100A); // Jump taken
        assert_eq!(cycles, 12);
    }

    #[test]
    fn test_jr_nc_taken() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_flag_c(false); // Carry clear
        ram.write(0x1000, 0x30); // JR NC, e
        ram.write(0x1001, 0x0A);

        cpu.pc = 0x1000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x100C); // Jump taken
        assert_eq!(cycles, 12);
    }

    #[test]
    fn test_jp_z_taken() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_flag_z(true);
        ram.write(0x0000, 0xCA); // JP Z, nn
        ram.write(0x0001, 0x00);
        ram.write(0x0002, 0x20);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x2000);
        assert_eq!(cycles, 10);
    }

    #[test]
    fn test_jp_nz_not_taken() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_flag_z(true); // Z is set, NZ condition is false
        ram.write(0x0000, 0xC2); // JP NZ, nn
        ram.write(0x0001, 0x00);
        ram.write(0x0002, 0x30);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x0003); // Didn't jump
        assert_eq!(cycles, 10);
    }

    #[test]
    fn test_call_z_taken() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0xFFFE;
        cpu.set_flag_z(true);
        ram.write(0x0000, 0xCC); // CALL Z, nn
        ram.write(0x0001, 0x00);
        ram.write(0x0002, 0x40);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x4000);
        assert_eq!(cpu.sp, 0xFFFC);
        // Check return address was pushed
        assert_eq!(ram.read(0xFFFC), 0x03); // Low byte
        assert_eq!(ram.read(0xFFFD), 0x00); // High byte
        assert_eq!(cycles, 17);
    }

    #[test]
    fn test_call_nz_not_taken() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0xFFFE;
        cpu.set_flag_z(true); // Z is set, NZ condition is false
        ram.write(0x0000, 0xC4); // CALL NZ, nn
        ram.write(0x0001, 0x00);
        ram.write(0x0002, 0x50);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x0003); // Didn't call
        assert_eq!(cpu.sp, 0xFFFE); // Stack unchanged
        assert_eq!(cycles, 10);
    }

    // ========================================================================
    // JP (HL)/(IX)/(IY), RST and RET cc Tests
    // ========================================================================

    #[test]
    fn test_jp_hl() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.h = 0x50;
        cpu.l = 0x34;
        ram.write(0x0000, 0xE9); // JP (HL)

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x5034);
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_jp_ix() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.ix = 0x1234;
        ram.write(0x0000, 0xDD); // DD prefix
        ram.write(0x0001, 0xE9); // JP (IX)

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x1234);
        assert_eq!(cycles, 8);
    }

    #[test]
    fn test_jp_iy() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.iy = 0xABCD;
        ram.write(0x0000, 0xFD); // FD prefix
        ram.write(0x0001, 0xE9); // JP (IY)

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0xABCD);
        assert_eq!(cycles, 8);
    }

    #[test]
    fn test_rst_00() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0xFFFE;
        ram.write(0x1000, 0xC7); // RST 00h

        cpu.pc = 0x1000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x0000);
        assert_eq!(cpu.sp, 0xFFFC);
        assert_eq!(ram.read(0xFFFC), 0x01); // Low byte of return address (0x1001)
        assert_eq!(ram.read(0xFFFD), 0x10); // High byte
        assert_eq!(cycles, 11);
    }

    #[test]
    fn test_rst_38() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0xFFFE;
        ram.write(0x2000, 0xFF); // RST 38h

        cpu.pc = 0x2000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x0038);
        assert_eq!(cpu.sp, 0xFFFC);
        assert_eq!(ram.read(0xFFFC), 0x01); // Low byte of return address (0x2001)
        assert_eq!(ram.read(0xFFFD), 0x20); // High byte
        assert_eq!(cycles, 11);
    }

    #[test]
    fn test_rst_all_addresses() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        let rst_ops = [
            (0xC7, 0x00),
            (0xCF, 0x08),
            (0xD7, 0x10),
            (0xDF, 0x18),
            (0xE7, 0x20),
            (0xEF, 0x28),
            (0xF7, 0x30),
            (0xFF, 0x38),
        ];

        for (opcode, target) in rst_ops.iter() {
            cpu.pc = 0x1000;
            cpu.sp = 0xFFFE;
            ram.write(0x1000, *opcode);

            cpu.step(&mut ram);

            assert_eq!(cpu.pc, *target as u16, "RST ${:02X}", opcode);
        }
    }

    #[test]
    fn test_ret_z_taken() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0xFFFC;
        cpu.set_flag_z(true);
        ram.write(0xFFFC, 0x00); // Return address low
        ram.write(0xFFFD, 0x60); // Return address high
        ram.write(0x0000, 0xC8); // RET Z

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x6000);
        assert_eq!(cpu.sp, 0xFFFE);
        assert_eq!(cycles, 11);
    }

    #[test]
    fn test_ret_nz_not_taken() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0xFFFC;
        cpu.set_flag_z(true); // Z is set, NZ condition is false
        ram.write(0x0000, 0xC0); // RET NZ

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.pc, 0x0001); // Didn't return
        assert_eq!(cpu.sp, 0xFFFC); // Stack unchanged
        assert_eq!(cycles, 5);
    }

    // ========================================
    // DJNZ Tests
    // ========================================

    #[test]
    fn test_djnz_jump() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.b = 0x05;
        ram.write(0x0000, 0x10); // DJNZ
        ram.write(0x0001, 0xFE); // -2 (jump back)

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.b, 0x04); // B decremented
        assert_eq!(cpu.pc, 0x0000); // 0x0002 + (-2) = 0x0000
        assert_eq!(cycles, 13); // Taken
    }

    #[test]
    fn test_djnz_no_jump() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.b = 0x01;
        ram.write(0x0000, 0x10); // DJNZ
        ram.write(0x0001, 0xFE); // -2

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.b, 0x00); // B decremented to 0
        assert_eq!(cpu.pc, 0x0002); // No jump, continue
        assert_eq!(cycles, 8); // Not taken
    }

    #[test]
    fn test_djnz_forward_jump() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.b = 0x10;
        ram.write(0x0100, 0x10); // DJNZ
        ram.write(0x0101, 0x10); // +16

        cpu.pc = 0x0100;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.b, 0x0F); // B decremented
        assert_eq!(cpu.pc, 0x0112); // 0x0102 + 0x10
        assert_eq!(cycles, 13);
    }
}
