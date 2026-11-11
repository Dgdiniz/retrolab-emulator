//! Z80 Load instructions (8-bit and 16-bit loads)

use super::*;
use retro_core::cpu::Addressable;

impl Z80 {
    /// LD r, n - Load 8-bit immediate into register
    /// Opcodes: 0x06, 0x0E, 0x16, 0x1E, 0x26, 0x2E, 0x3E
    /// T-states: 7
    pub(crate) fn ld_r_n(&mut self, bus: &mut dyn Addressable, reg: Reg8) -> InstructionResult {
        let value = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);

        self.write_reg8(reg, value);

        InstructionResult::new(7)
    }

    /// LD r, r' - Load register to register
    /// Opcodes: 0x40-0x7F (excluding 0x76 which is HALT)
    /// T-states: 4
    pub(crate) fn ld_r_r(&mut self, dst: Reg8, src: Reg8) -> InstructionResult {
        let value = self.read_reg8(src);
        self.write_reg8(dst, value);

        InstructionResult::new(4)
    }

    /// LD A, (BC) - Load A from address in BC
    /// Opcode: 0x0A
    /// T-states: 7
    pub(crate) fn ld_a_bc_indirect(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let addr = self.bc();
        self.a = bus.read(addr as u32);
        self.wz = addr.wrapping_add(1);
        InstructionResult::new(7)
    }

    /// LD A, (DE) - Load A from address in DE
    /// Opcode: 0x1A
    /// T-states: 7
    pub(crate) fn ld_a_de_indirect(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let addr = self.de();
        self.a = bus.read(addr as u32);
        self.wz = addr.wrapping_add(1);
        InstructionResult::new(7)
    }

    /// LD A, (nn) - Load A from absolute address
    /// Opcode: 0x3A
    /// T-states: 13
    pub(crate) fn ld_a_nn_indirect(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let addr_low = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let addr_high = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let addr = (addr_high << 8) | addr_low;

        self.a = bus.read(addr as u32);
        self.wz = addr.wrapping_add(1);
        InstructionResult::new(13)
    }

    /// LD (BC), A - Store A to address in BC
    /// Opcode: 0x02
    /// T-states: 7
    pub(crate) fn ld_bc_indirect_a(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let addr = self.bc();
        bus.write(addr as u32, self.a);
        // WZ_low = (addr + 1) & 0xFF, WZ_high = A
        self.wz = ((self.a as u16) << 8) | (addr.wrapping_add(1) & 0xFF);
        InstructionResult::new(7)
    }

    /// LD (DE), A - Store A to address in DE
    /// Opcode: 0x12
    /// T-states: 7
    pub(crate) fn ld_de_indirect_a(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let addr = self.de();
        bus.write(addr as u32, self.a);
        self.wz = ((self.a as u16) << 8) | (addr.wrapping_add(1) & 0xFF);
        InstructionResult::new(7)
    }

    /// LD (nn), A - Store A to absolute address
    /// Opcode: 0x32
    /// T-states: 13
    pub(crate) fn ld_nn_indirect_a(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let addr_low = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let addr_high = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let addr = (addr_high << 8) | addr_low;

        bus.write(addr as u32, self.a);
        self.wz = ((self.a as u16) << 8) | (addr.wrapping_add(1) & 0xFF);
        InstructionResult::new(13)
    }

    // ========================
    // Load 16-bit
    // ========================

    /// LD dd, nn - Load 16-bit immediate into register pair
    /// Opcodes: 0x01 (BC), 0x11 (DE), 0x21 (HL), 0x31 (SP)
    /// T-states: 10
    pub(crate) fn ld_dd_nn(&mut self, bus: &mut dyn Addressable, reg: Reg16) -> InstructionResult {
        let nn_low = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let nn_high = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let nn = (nn_high << 8) | nn_low;

        // Write to 16-bit register pair
        match reg {
            Reg16::BC => self.set_bc(nn),
            Reg16::DE => self.set_de(nn),
            Reg16::HL => self.set_hl(nn),
            Reg16::SP => self.sp = nn,
            Reg16::AF => {
                self.a = (nn >> 8) as u8;
                self.f = (nn & 0xFF) as u8;
            }
            Reg16::IX => self.ix = nn,
            Reg16::IY => self.iy = nn,
        }
        InstructionResult::new(10)
    }

    /// LD HL, (nn) - Load HL from memory address
    /// Opcode: 0x2A
    /// T-states: 16
    pub(crate) fn ld_hl_nn_indirect(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let addr_low = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let addr_high = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let addr = (addr_high << 8) | addr_low;

        let l = bus.read(addr as u32);
        let h = bus.read(addr.wrapping_add(1) as u32);
        self.set_hl((h as u16) << 8 | l as u16);
        self.wz = addr.wrapping_add(1);
        InstructionResult::new(16)
    }

    /// LD (nn), HL - Store HL to memory address
    /// Opcode: 0x22
    /// T-states: 16
    pub(crate) fn ld_nn_indirect_hl(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let addr_low = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let addr_high = bus.read(self.pc as u32) as u16;
        self.pc = self.pc.wrapping_add(1);
        let addr = (addr_high << 8) | addr_low;

        let hl = self.hl();
        bus.write(addr as u32, (hl & 0xFF) as u8);
        bus.write(addr.wrapping_add(1) as u32, (hl >> 8) as u8);
        self.wz = addr.wrapping_add(1);
        InstructionResult::new(16)
    }

    /// LD SP, HL - Copy HL to SP
    /// Opcode: 0xF9
    /// T-states: 6
    pub(crate) fn ld_sp_hl(&mut self) -> InstructionResult {
        self.sp = self.hl();
        InstructionResult::new(6)
    }

    // ========================
    // LD r, (HL) / LD (HL), r
    // ========================

    /// LD r, (HL) - Load register from memory at HL
    /// Opcodes: 0x46 (B), 0x4E (C), 0x56 (D), 0x5E (E), 0x66 (H), 0x6E (L), 0x7E (A)
    /// T-states: 7
    pub(crate) fn ld_r_hl_indirect(
        &mut self,
        bus: &mut dyn Addressable,
        reg: Reg8,
    ) -> InstructionResult {
        let hl = self.hl();
        let value = bus.read(hl as u32);
        self.write_reg8(reg, value);
        InstructionResult::new(7)
    }

    /// LD (HL), r - Store register to memory at HL
    /// Opcodes: 0x70 (B), 0x71 (C), 0x72 (D), 0x73 (E), 0x74 (H), 0x75 (L), 0x77 (A)
    /// T-states: 7
    pub(crate) fn ld_hl_indirect_r(
        &mut self,
        bus: &mut dyn Addressable,
        reg: Reg8,
    ) -> InstructionResult {
        let hl = self.hl();
        let value = self.read_reg8(reg);
        bus.write(hl as u32, value);
        InstructionResult::new(7)
    }

    /// LD (HL), n - Store immediate byte to memory at HL
    /// Opcode: 0x36
    /// T-states: 10
    pub(crate) fn ld_hl_indirect_n(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let n = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        let hl = self.hl();
        bus.write(hl as u32, n);
        InstructionResult::new(10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_ld_b_n() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        // LD B, $42
        ram.write(0x0000, 0x06);
        ram.write(0x0001, 0x42);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.b, 0x42);
        assert_eq!(cpu.pc, 0x0002);
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_ld_c_n() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        // LD C, $FF
        ram.write(0x0000, 0x0E);
        ram.write(0x0001, 0xFF);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.c, 0xFF);
        assert_eq!(cpu.pc, 0x0002);
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_ld_a_n() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        // LD A, $7F
        ram.write(0x0000, 0x3E);
        ram.write(0x0001, 0x7F);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x7F);
        assert_eq!(cpu.pc, 0x0002);
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_ld_b_c() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.c = 0x12;
        ram.write(0x0000, 0x41); // LD B, C

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.b, 0x12);
        assert_eq!(cpu.c, 0x12); // Source unchanged
        assert_eq!(cpu.pc, 0x0001);
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_ld_a_b() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.b = 0x99;
        ram.write(0x0000, 0x78); // LD A, B

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x99);
        assert_eq!(cpu.b, 0x99);
        assert_eq!(cpu.pc, 0x0001);
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_ld_a_a() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x55;
        ram.write(0x0000, 0x7F); // LD A, A

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x55); // Unchanged
        assert_eq!(cpu.pc, 0x0001);
        assert_eq!(cycles, 4);
    }

    #[test]
    fn test_ld_a_bc_indirect() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_bc(0x1234);
        ram.write(0x1234, 0x42);
        ram.write(0x0000, 0x0A); // LD A, (BC)

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x42);
        assert_eq!(cpu.pc, 0x0001);
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_ld_a_de_indirect() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_de(0x5678);
        ram.write(0x5678, 0x99);
        ram.write(0x0000, 0x1A); // LD A, (DE)

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0x99);
        assert_eq!(cpu.pc, 0x0001);
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_ld_a_nn_indirect() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        ram.write(0x2000, 0xAB);
        ram.write(0x0000, 0x3A); // LD A, (nn)
        ram.write(0x0001, 0x00); // nn low
        ram.write(0x0002, 0x20); // nn high = 0x2000

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0xAB);
        assert_eq!(cpu.pc, 0x0003);
        assert_eq!(cycles, 13);
    }

    #[test]
    fn test_ld_bc_indirect_a() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x77;
        cpu.set_bc(0x3000);
        ram.write(0x0000, 0x02); // LD (BC), A

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(ram.read(0x3000), 0x77);
        assert_eq!(cpu.pc, 0x0001);
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_ld_de_indirect_a() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x88;
        cpu.set_de(0x4000);
        ram.write(0x0000, 0x12); // LD (DE), A

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(ram.read(0x4000), 0x88);
        assert_eq!(cpu.pc, 0x0001);
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_ld_nn_indirect_a() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0xCD;
        ram.write(0x0000, 0x32); // LD (nn), A
        ram.write(0x0001, 0x00); // nn low
        ram.write(0x0002, 0x50); // nn high = 0x5000

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(ram.read(0x5000), 0xCD);
        assert_eq!(cpu.pc, 0x0003);
        assert_eq!(cycles, 13);
    }

    // ========================
    // Load 16-bit
    // ========================

    #[test]
    fn test_ld_bc_nn() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        ram.write(0x0000, 0x01); // LD BC, nn
        ram.write(0x0001, 0x34); // low
        ram.write(0x0002, 0x12); // high = 0x1234

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.bc(), 0x1234);
        assert_eq!(cpu.pc, 0x0003);
        assert_eq!(cycles, 10);
    }

    #[test]
    fn test_ld_de_nn() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        ram.write(0x0000, 0x11); // LD DE, nn
        ram.write(0x0001, 0x78);
        ram.write(0x0002, 0x56);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.de(), 0x5678);
        assert_eq!(cpu.pc, 0x0003);
        assert_eq!(cycles, 10);
    }

    #[test]
    fn test_ld_hl_nn() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        ram.write(0x0000, 0x21); // LD HL, nn
        ram.write(0x0001, 0xCD);
        ram.write(0x0002, 0xAB);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.hl(), 0xABCD);
        assert_eq!(cpu.pc, 0x0003);
        assert_eq!(cycles, 10);
    }

    #[test]
    fn test_ld_sp_nn() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        ram.write(0x0000, 0x31); // LD SP, nn
        ram.write(0x0001, 0x00);
        ram.write(0x0002, 0xFF);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.sp, 0xFF00);
        assert_eq!(cpu.pc, 0x0003);
        assert_eq!(cycles, 10);
    }

    #[test]
    fn test_ld_hl_nn_indirect() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        ram.write(0x2000, 0x34); // low byte of value
        ram.write(0x2001, 0x12); // high byte = 0x1234
        ram.write(0x0000, 0x2A); // LD HL, (nn)
        ram.write(0x0001, 0x00); // addr low
        ram.write(0x0002, 0x20); // addr high = 0x2000

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.hl(), 0x1234);
        assert_eq!(cpu.pc, 0x0003);
        assert_eq!(cycles, 16);
    }

    #[test]
    fn test_ld_nn_indirect_hl() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_hl(0x5678);
        ram.write(0x0000, 0x22); // LD (nn), HL
        ram.write(0x0001, 0x00);
        ram.write(0x0002, 0x30); // addr = 0x3000

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(ram.read(0x3000), 0x78); // low byte
        assert_eq!(ram.read(0x3001), 0x56); // high byte
        assert_eq!(cpu.pc, 0x0003);
        assert_eq!(cycles, 16);
    }

    #[test]
    fn test_ld_sp_hl() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_hl(0xABCD);
        ram.write(0x0000, 0xF9); // LD SP, HL

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.sp, 0xABCD);
        assert_eq!(cpu.pc, 0x0001);
        assert_eq!(cycles, 6);
    }

    #[test]
    fn test_ld_b_hl_indirect() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_hl(0x2000);
        ram.write(0x2000, 0x42);
        ram.write(0x0000, 0x46); // LD B, (HL)

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.b, 0x42);
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_ld_a_hl_indirect() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_hl(0x3000);
        ram.write(0x3000, 0xFF);
        ram.write(0x0000, 0x7E); // LD A, (HL)

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(cpu.a, 0xFF);
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_ld_hl_indirect_b() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_hl(0x4000);
        cpu.b = 0x99;
        ram.write(0x0000, 0x70); // LD (HL), B

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(ram.read(0x4000), 0x99);
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_ld_hl_indirect_a() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_hl(0x5000);
        cpu.a = 0x55;
        ram.write(0x0000, 0x77); // LD (HL), A

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(ram.read(0x5000), 0x55);
        assert_eq!(cycles, 7);
    }

    #[test]
    fn test_ld_hl_indirect_n() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_hl(0x6000);
        ram.write(0x0000, 0x36); // LD (HL), n
        ram.write(0x0001, 0xAA);

        cpu.pc = 0x0000;
        let cycles = cpu.step(&mut ram);

        assert_eq!(ram.read(0x6000), 0xAA);
        assert_eq!(cpu.pc, 0x0002);
        assert_eq!(cycles, 10);
    }
}
