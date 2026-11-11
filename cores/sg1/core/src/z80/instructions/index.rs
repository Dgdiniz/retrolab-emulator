use crate::z80::{InstructionResult, Z80};
use retro_core::cpu::Addressable;

// ============================================================================
// Index Register Operations (IX/IY)
// ============================================================================
//
// An `is_iy` flag selects IX or IY, so the two share one implementation: the DD and FD
// opcodes are identical and differ only in the register they affect.
//
// Basic instructions (no displacement)
//   - LD IX/IY, nn
//   - LD (nn), IX/IY
//   - LD IX/IY, (nn)
//   - ADD IX/IY, rr
//   - INC/DEC IX/IY
//   - PUSH/POP IX/IY
//   - EX (SP), IX/IY
//   - LD SP, IX/IY
//   - JP (IX/IY) (in jumps.rs)
//
// Instructions with a displacement (IX+d)/(IY+d)
//   - LD r, (IX+d)
//   - LD (IX+d), r
//   - LD (IX+d), n
//   - INC/DEC (IX+d)
//   - Arithmetic with (IX+d): ADD/ADC/SUB/SBC/AND/XOR/OR/CP
//   - CB prefix: BIT/SET/RES with (IX+d) (in mod.rs)

impl Z80 {
    // ========================================================================
    // Basic Instructions (no displacement)
    // ========================================================================

    /// LD IX/IY, nn - Load 16-bit immediate
    /// DD/FD 21 nn nn
    /// Cycles: 14
    pub(crate) fn ld_idx_nn(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
    ) -> InstructionResult {
        let low = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        let high = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        let nn = u16::from_le_bytes([low, high]);

        if is_iy {
            self.iy = nn;
        } else {
            self.ix = nn;
        }

        InstructionResult::new(14)
    }

    /// LD (nn), IX/IY - Store IX/IY to memory
    /// DD/FD 22 nn nn
    /// Cycles: 20
    pub(crate) fn ld_nn_idx(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
    ) -> InstructionResult {
        let addr_low = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        let addr_high = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        let addr = u16::from_le_bytes([addr_low, addr_high]);

        let value = if is_iy { self.iy } else { self.ix };

        bus.write(addr as u32, (value & 0xFF) as u8);
        bus.write(addr.wrapping_add(1) as u32, (value >> 8) as u8);
        self.wz = addr.wrapping_add(1);

        InstructionResult::new(20)
    }

    /// LD IX/IY, (nn) - Load IX/IY from memory
    /// DD/FD 2A nn nn
    /// Cycles: 20
    pub(crate) fn ld_idx_nn_ind(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
    ) -> InstructionResult {
        let addr_low = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        let addr_high = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        let addr = u16::from_le_bytes([addr_low, addr_high]);

        let low = bus.read(addr as u32);
        let high = bus.read(addr.wrapping_add(1) as u32);
        let value = u16::from_le_bytes([low, high]);

        if is_iy {
            self.iy = value;
        } else {
            self.ix = value;
        }
        self.wz = addr.wrapping_add(1);

        InstructionResult::new(20)
    }

    /// ADD IX/IY, rr - Add 16-bit register to IX/IY
    /// DD/FD 09/19/29/39
    /// Cycles: 15
    pub(crate) fn add_idx_rr(&mut self, is_iy: bool, rr_code: u8) -> InstructionResult {
        let idx = if is_iy { self.iy } else { self.ix };
        self.wz = idx.wrapping_add(1);

        let rr = match rr_code {
            0 => self.bc(),
            1 => self.de(),
            2 => {
                if is_iy {
                    self.iy
                } else {
                    self.ix
                }
            } // ADD IX, IX or ADD IY, IY
            3 => self.sp,
            _ => unreachable!(),
        };

        let result = idx.wrapping_add(rr);

        // Flags: S, Z, P/V not affected; H, N, C affected
        self.set_flag_h((idx & 0x0FFF) + (rr & 0x0FFF) > 0x0FFF); // Half carry bit 11
        self.set_flag_n(false);
        self.set_flag_c(idx as u32 + rr as u32 > 0xFFFF);
        let result_high = (result >> 8) as u8;
        self.set_flag_y(result_high & 0x20 != 0);
        self.set_flag_x(result_high & 0x08 != 0);

        if is_iy {
            self.iy = result;
        } else {
            self.ix = result;
        }

        InstructionResult::new(15)
    }

    /// INC IX/IY - Increment IX/IY
    /// DD/FD 23
    /// Cycles: 10
    pub(crate) fn inc_idx(&mut self, is_iy: bool) -> InstructionResult {
        if is_iy {
            self.iy = self.iy.wrapping_add(1);
        } else {
            self.ix = self.ix.wrapping_add(1);
        }
        // Flags not affected
        InstructionResult::new(10)
    }

    /// DEC IX/IY - Decrement IX/IY
    /// DD/FD 2B
    /// Cycles: 10
    pub(crate) fn dec_idx(&mut self, is_iy: bool) -> InstructionResult {
        if is_iy {
            self.iy = self.iy.wrapping_sub(1);
        } else {
            self.ix = self.ix.wrapping_sub(1);
        }
        // Flags not affected
        InstructionResult::new(10)
    }

    /// PUSH IX/IY - Push IX/IY onto stack
    /// DD/FD E5
    /// Cycles: 15
    pub(crate) fn push_idx(&mut self, bus: &mut dyn Addressable, is_iy: bool) -> InstructionResult {
        let value = if is_iy { self.iy } else { self.ix };

        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp as u32, (value >> 8) as u8); // High byte
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp as u32, (value & 0xFF) as u8); // Low byte

        InstructionResult::new(15)
    }

    /// POP IX/IY - Pop IX/IY from stack
    /// DD/FD E1
    /// Cycles: 14
    pub(crate) fn pop_idx(&mut self, bus: &mut dyn Addressable, is_iy: bool) -> InstructionResult {
        let low = bus.read(self.sp as u32);
        self.sp = self.sp.wrapping_add(1);
        let high = bus.read(self.sp as u32);
        self.sp = self.sp.wrapping_add(1);

        let value = u16::from_le_bytes([low, high]);

        if is_iy {
            self.iy = value;
        } else {
            self.ix = value;
        }

        InstructionResult::new(14)
    }

    /// EX (SP), IX/IY - Exchange IX/IY with top of stack
    /// DD/FD E3
    /// Cycles: 23
    pub(crate) fn ex_sp_idx(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
    ) -> InstructionResult {
        let idx = if is_iy { self.iy } else { self.ix };

        // Read from stack
        let low = bus.read(self.sp as u32);
        let high = bus.read(self.sp.wrapping_add(1) as u32);
        let sp_value = u16::from_le_bytes([low, high]);

        // Write IX/IY to stack
        bus.write(self.sp as u32, (idx & 0xFF) as u8);
        bus.write(self.sp.wrapping_add(1) as u32, (idx >> 8) as u8);

        // Update IX/IY
        if is_iy {
            self.iy = sp_value;
        } else {
            self.ix = sp_value;
        }
        self.wz = sp_value;

        InstructionResult::new(23)
    }

    /// LD SP, IX/IY - Load SP from IX/IY
    /// DD/FD F9
    /// Cycles: 10
    pub(crate) fn ld_sp_idx(&mut self, is_iy: bool) -> InstructionResult {
        self.sp = if is_iy { self.iy } else { self.ix };
        InstructionResult::new(10)
    }

    // ========================================================================
    // IXH/IXL/IYH/IYL Undocumented Instructions
    // ========================================================================

    /// Helper: get high byte of IX or IY
    #[inline]
    fn idx_high(&self, is_iy: bool) -> u8 {
        if is_iy {
            (self.iy >> 8) as u8
        } else {
            (self.ix >> 8) as u8
        }
    }

    /// Helper: get low byte of IX or IY
    #[inline]
    fn idx_low(&self, is_iy: bool) -> u8 {
        if is_iy {
            (self.iy & 0xFF) as u8
        } else {
            (self.ix & 0xFF) as u8
        }
    }

    /// Helper: set high byte of IX or IY
    #[inline]
    fn set_idx_high(&mut self, is_iy: bool, value: u8) {
        if is_iy {
            self.iy = (self.iy & 0x00FF) | ((value as u16) << 8);
        } else {
            self.ix = (self.ix & 0x00FF) | ((value as u16) << 8);
        }
    }

    /// Helper: set low byte of IX or IY
    #[inline]
    fn set_idx_low(&mut self, is_iy: bool, value: u8) {
        if is_iy {
            self.iy = (self.iy & 0xFF00) | (value as u16);
        } else {
            self.ix = (self.ix & 0xFF00) | (value as u16);
        }
    }

    /// INC IXH/IYH - Increment high byte of IX/IY (undocumented)
    /// DD/FD 24 - Cycles: 8
    pub(crate) fn inc_idx_high(&mut self, is_iy: bool) -> InstructionResult {
        let value = self.idx_high(is_iy);
        let result = value.wrapping_add(1);
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_h((value & 0x0F) + 1 > 0x0F);
        self.set_flag_x(result & 0x08 != 0);
        self.set_flag_pv(value == 0x7F);
        self.set_flag_n(false);
        self.set_idx_high(is_iy, result);
        InstructionResult::new(8)
    }

    /// DEC IXH/IYH - Decrement high byte of IX/IY (undocumented)
    /// DD/FD 25 - Cycles: 8
    pub(crate) fn dec_idx_high(&mut self, is_iy: bool) -> InstructionResult {
        let value = self.idx_high(is_iy);
        let result = value.wrapping_sub(1);
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_h((value & 0x0F) == 0);
        self.set_flag_x(result & 0x08 != 0);
        self.set_flag_pv(value == 0x80);
        self.set_flag_n(true);
        self.set_idx_high(is_iy, result);
        InstructionResult::new(8)
    }

    /// LD IXH/IYH, n - Load immediate to high byte (undocumented)
    /// DD/FD 26 n - Cycles: 11
    pub(crate) fn ld_idx_high_n(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
    ) -> InstructionResult {
        let n = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        self.set_idx_high(is_iy, n);
        InstructionResult::new(11)
    }

    /// INC IXL/IYL - Increment low byte of IX/IY (undocumented)
    /// DD/FD 2C - Cycles: 8
    pub(crate) fn inc_idx_low(&mut self, is_iy: bool) -> InstructionResult {
        let value = self.idx_low(is_iy);
        let result = value.wrapping_add(1);
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_h((value & 0x0F) + 1 > 0x0F);
        self.set_flag_x(result & 0x08 != 0);
        self.set_flag_pv(value == 0x7F);
        self.set_flag_n(false);
        self.set_idx_low(is_iy, result);
        InstructionResult::new(8)
    }

    /// DEC IXL/IYL - Decrement low byte of IX/IY (undocumented)
    /// DD/FD 2D - Cycles: 8
    pub(crate) fn dec_idx_low(&mut self, is_iy: bool) -> InstructionResult {
        let value = self.idx_low(is_iy);
        let result = value.wrapping_sub(1);
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_h((value & 0x0F) == 0);
        self.set_flag_x(result & 0x08 != 0);
        self.set_flag_pv(value == 0x80);
        self.set_flag_n(true);
        self.set_idx_low(is_iy, result);
        InstructionResult::new(8)
    }

    /// LD IXL/IYL, n - Load immediate to low byte (undocumented)
    /// DD/FD 2E n - Cycles: 11
    pub(crate) fn ld_idx_low_n(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
    ) -> InstructionResult {
        let n = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        self.set_idx_low(is_iy, n);
        InstructionResult::new(11)
    }

    /// LD r, IXH/IXL/IYH/IYL (undocumented) - Cycles: 8
    /// reg_code: 0=B, 1=C, 2=D, 3=E, 7=A
    /// is_high: true = IXH/IYH, false = IXL/IYL
    pub(crate) fn ld_r_idx_half(
        &mut self,
        is_iy: bool,
        is_high: bool,
        reg_code: u8,
    ) -> InstructionResult {
        let value = if is_high {
            self.idx_high(is_iy)
        } else {
            self.idx_low(is_iy)
        };
        match reg_code {
            0 => self.b = value,
            1 => self.c = value,
            2 => self.d = value,
            3 => self.e = value,
            7 => self.a = value,
            _ => unreachable!(),
        }
        InstructionResult::new(8)
    }

    /// LD IXH/IYH, r (undocumented) - Cycles: 8
    pub(crate) fn ld_idx_high_r(&mut self, is_iy: bool, reg_code: u8) -> InstructionResult {
        let value = match reg_code {
            0 => self.b,
            1 => self.c,
            2 => self.d,
            3 => self.e,
            4 => self.idx_high(is_iy), // IXH/IYH = IXH/IYH (NOP)
            5 => self.idx_low(is_iy),  // IXH/IYH = IXL/IYL
            7 => self.a,
            _ => unreachable!(),
        };
        self.set_idx_high(is_iy, value);
        InstructionResult::new(8)
    }

    /// LD IXL/IYL, r (undocumented) - Cycles: 8
    pub(crate) fn ld_idx_low_r(&mut self, is_iy: bool, reg_code: u8) -> InstructionResult {
        let value = match reg_code {
            0 => self.b,
            1 => self.c,
            2 => self.d,
            3 => self.e,
            4 => self.idx_high(is_iy), // IXL/IYL = IXH/IYH
            5 => self.idx_low(is_iy),  // IXL/IYL = IXL/IYL (NOP)
            7 => self.a,
            _ => unreachable!(),
        };
        self.set_idx_low(is_iy, value);
        InstructionResult::new(8)
    }

    /// Arithmetic/logical with IXH/IYH/IXL/IYL (undocumented) - Cycles: 8
    /// op: 0=ADD, 1=ADC, 2=SUB, 3=SBC, 4=AND, 5=XOR, 6=OR, 7=CP
    pub(crate) fn alu_idx_half(&mut self, is_iy: bool, is_high: bool, op: u8) -> InstructionResult {
        let value = if is_high {
            self.idx_high(is_iy)
        } else {
            self.idx_low(is_iy)
        };
        match op {
            0 => self.add_a(value),
            1 => self.adc_a(value),
            2 => self.sub_a(value),
            3 => self.sbc_a(value),
            4 => self.and_a(value),
            5 => self.xor_a(value),
            6 => self.or_a(value),
            7 => self.cp_a(value),
            _ => unreachable!(),
        }
        InstructionResult::new(8)
    }

    // ========================================================================
    // Instructions with Displacement (IX+d)/(IY+d)
    // ========================================================================

    /// Helper: Calculate effective address with displacement
    #[inline]
    pub(crate) fn calc_indexed_addr(&self, is_iy: bool, displacement: i8) -> u16 {
        let base = if is_iy { self.iy } else { self.ix };
        base.wrapping_add(displacement as i16 as u16)
    }

    /// LD r, (IX+d) - Load register from indexed memory
    /// DD/FD 46/4E/56/5E/66/6E/7E d
    /// Cycles: 19
    pub(crate) fn ld_r_idx_d(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
        reg: u8,
    ) -> InstructionResult {
        let displacement = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        let addr = self.calc_indexed_addr(is_iy, displacement);
        self.wz = addr;
        let value = bus.read(addr as u32);

        self.write_reg8(crate::z80::addressing::Reg8::from_code(reg), value);

        InstructionResult::new(19)
    }

    /// LD (IX+d), r - Store register to indexed memory
    /// DD/FD 70/71/72/73/74/75/77 d
    /// Cycles: 19
    pub(crate) fn ld_idx_d_r(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
        reg: u8,
    ) -> InstructionResult {
        let displacement = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        let addr = self.calc_indexed_addr(is_iy, displacement);
        self.wz = addr;
        let value = self.read_reg8(crate::z80::addressing::Reg8::from_code(reg));

        bus.write(addr as u32, value);

        InstructionResult::new(19)
    }

    /// LD (IX+d), n - Store immediate to indexed memory
    /// DD/FD 36 d n
    /// Cycles: 19
    pub(crate) fn ld_idx_d_n(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
    ) -> InstructionResult {
        let displacement = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);
        let immediate = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);

        let addr = self.calc_indexed_addr(is_iy, displacement);
        self.wz = addr;
        bus.write(addr as u32, immediate);

        InstructionResult::new(19)
    }

    /// INC (IX+d) - Increment indexed memory
    /// DD/FD 34 d
    /// Cycles: 23
    pub(crate) fn inc_idx_d(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
    ) -> InstructionResult {
        let displacement = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        let addr = self.calc_indexed_addr(is_iy, displacement);
        self.wz = addr;
        let value = bus.read(addr as u32);
        let result = value.wrapping_add(1);

        bus.write(addr as u32, result);

        // Flags (same as INC r)
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_h((value & 0x0F) + 1 > 0x0F);
        self.set_flag_pv(value == 0x7F); // Overflow from 0x7F to 0x80
        self.set_flag_n(false);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_x(result & 0x08 != 0);
        // Carry not affected

        InstructionResult::new(23)
    }

    /// DEC (IX+d) - Decrement indexed memory
    /// DD/FD 35 d
    /// Cycles: 23
    pub(crate) fn dec_idx_d(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
    ) -> InstructionResult {
        let displacement = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        let addr = self.calc_indexed_addr(is_iy, displacement);
        self.wz = addr;
        let value = bus.read(addr as u32);
        let result = value.wrapping_sub(1);

        bus.write(addr as u32, result);

        // Flags (same as DEC r)
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_h((value & 0x0F) == 0); // Borrow from bit 4
        self.set_flag_pv(value == 0x80); // Overflow from 0x80 to 0x7F
        self.set_flag_n(true);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_x(result & 0x08 != 0);
        // Carry not affected

        InstructionResult::new(23)
    }

    /// ADD A, (IX+d) - Add indexed memory to A
    /// DD/FD 86 d
    /// Cycles: 19
    pub(crate) fn add_a_idx_d(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
    ) -> InstructionResult {
        let displacement = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        let addr = self.calc_indexed_addr(is_iy, displacement);
        self.wz = addr;
        let value = bus.read(addr as u32);
        let a = self.a;
        let result = a.wrapping_add(value);

        self.a = result;

        // Flags
        let overflow = ((a ^ result) & (value ^ result) & 0x80) != 0;
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_h((a & 0x0F) + (value & 0x0F) > 0x0F);
        self.set_flag_pv(overflow);
        self.set_flag_n(false);
        self.set_flag_c(a as u16 + value as u16 > 0xFF);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_x(result & 0x08 != 0);

        InstructionResult::new(19)
    }

    /// ADC A, (IX+d) - Add indexed memory to A with carry
    /// DD/FD 8E d
    /// Cycles: 19
    pub(crate) fn adc_a_idx_d(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
    ) -> InstructionResult {
        let displacement = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        let addr = self.calc_indexed_addr(is_iy, displacement);
        self.wz = addr;
        let value = bus.read(addr as u32);
        let a = self.a;
        let carry = if self.flag_c() { 1 } else { 0 };
        let result = a.wrapping_add(value).wrapping_add(carry);

        self.a = result;

        // Flags
        let overflow = ((a ^ result) & (value ^ result) & 0x80) != 0;
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_h((a & 0x0F) + (value & 0x0F) + carry > 0x0F);
        self.set_flag_pv(overflow);
        self.set_flag_n(false);
        self.set_flag_c(a as u16 + value as u16 + carry as u16 > 0xFF);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_x(result & 0x08 != 0);

        InstructionResult::new(19)
    }

    /// SUB (IX+d) - Subtract indexed memory from A
    /// DD/FD 96 d
    /// Cycles: 19
    pub(crate) fn sub_idx_d(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
    ) -> InstructionResult {
        let displacement = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        let addr = self.calc_indexed_addr(is_iy, displacement);
        self.wz = addr;
        let value = bus.read(addr as u32);
        let a = self.a;
        let result = a.wrapping_sub(value);

        self.a = result;

        // Flags
        let overflow = ((a ^ value) & (a ^ result) & 0x80) != 0;
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_h((a & 0x0F) < (value & 0x0F)); // Borrow
        self.set_flag_pv(overflow);
        self.set_flag_n(true);
        self.set_flag_c(a < value);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_x(result & 0x08 != 0);

        InstructionResult::new(19)
    }

    /// SBC A, (IX+d) - Subtract indexed memory from A with borrow
    /// DD/FD 9E d
    /// Cycles: 19
    pub(crate) fn sbc_a_idx_d(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
    ) -> InstructionResult {
        let displacement = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        let addr = self.calc_indexed_addr(is_iy, displacement);
        self.wz = addr;
        let value = bus.read(addr as u32);
        let a = self.a;
        let carry = if self.flag_c() { 1 } else { 0 };
        let result = a.wrapping_sub(value).wrapping_sub(carry);

        self.a = result;

        // Flags
        let overflow = ((a ^ value) & (a ^ result) & 0x80) != 0;
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_h((a & 0x0F) < (value & 0x0F) + carry);
        self.set_flag_pv(overflow);
        self.set_flag_n(true);
        self.set_flag_c((a as u16) < (value as u16 + carry as u16));
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_x(result & 0x08 != 0);

        InstructionResult::new(19)
    }

    /// AND (IX+d) - Logical AND with indexed memory
    /// DD/FD A6 d
    /// Cycles: 19
    pub(crate) fn and_idx_d(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
    ) -> InstructionResult {
        let displacement = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        let addr = self.calc_indexed_addr(is_iy, displacement);
        self.wz = addr;
        let value = bus.read(addr as u32);
        let result = self.a & value;

        self.a = result;

        // Flags
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_h(true); // Always set for AND
        self.set_flag_pv(result.count_ones().is_multiple_of(2)); // Parity
        self.set_flag_n(false);
        self.set_flag_c(false);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_x(result & 0x08 != 0);

        InstructionResult::new(19)
    }

    /// XOR (IX+d) - Logical XOR with indexed memory
    /// DD/FD AE d
    /// Cycles: 19
    pub(crate) fn xor_idx_d(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
    ) -> InstructionResult {
        let displacement = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        let addr = self.calc_indexed_addr(is_iy, displacement);
        self.wz = addr;
        let value = bus.read(addr as u32);
        let result = self.a ^ value;

        self.a = result;

        // Flags
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_h(false);
        self.set_flag_pv(result.count_ones().is_multiple_of(2)); // Parity
        self.set_flag_n(false);
        self.set_flag_c(false);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_x(result & 0x08 != 0);

        InstructionResult::new(19)
    }

    /// OR (IX+d) - Logical OR with indexed memory
    /// DD/FD B6 d
    /// Cycles: 19
    pub(crate) fn or_idx_d(&mut self, bus: &mut dyn Addressable, is_iy: bool) -> InstructionResult {
        let displacement = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        let addr = self.calc_indexed_addr(is_iy, displacement);
        self.wz = addr;
        let value = bus.read(addr as u32);
        let result = self.a | value;

        self.a = result;

        // Flags
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_h(false);
        self.set_flag_pv(result.count_ones().is_multiple_of(2)); // Parity
        self.set_flag_n(false);
        self.set_flag_c(false);
        self.set_flag_y(result & 0x20 != 0);
        self.set_flag_x(result & 0x08 != 0);

        InstructionResult::new(19)
    }

    /// CP (IX+d) - Compare A with indexed memory
    /// DD/FD BE d
    /// Cycles: 19
    pub(crate) fn cp_idx_d(&mut self, bus: &mut dyn Addressable, is_iy: bool) -> InstructionResult {
        let displacement = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);

        let addr = self.calc_indexed_addr(is_iy, displacement);
        self.wz = addr;
        let value = bus.read(addr as u32);
        let a = self.a;
        let result = a.wrapping_sub(value);

        // Flags (A is not modified, only flags)
        let overflow = ((a ^ value) & (a ^ result) & 0x80) != 0;
        self.set_flag_s(result & 0x80 != 0);
        self.set_flag_z(result == 0);
        self.set_flag_h((a & 0x0F) < (value & 0x0F));
        self.set_flag_pv(overflow);
        self.set_flag_n(true);
        self.set_flag_c(a < value);
        self.set_flag_y(value & 0x20 != 0);
        self.set_flag_x(value & 0x08 != 0);

        InstructionResult::new(19)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_ld_ix_nn() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x0000;
        ram.write(0x0000, 0x34); // Low byte
        ram.write(0x0001, 0x12); // High byte

        cpu.ld_idx_nn(&mut ram, false); // IX

        assert_eq!(cpu.ix, 0x1234);
        assert_eq!(cpu.pc, 0x0002);
    }

    #[test]
    fn test_ld_iy_nn() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x0000;
        ram.write(0x0000, 0xCD); // Low byte
        ram.write(0x0001, 0xAB); // High byte

        cpu.ld_idx_nn(&mut ram, true); // IY

        assert_eq!(cpu.iy, 0xABCD);
        assert_eq!(cpu.pc, 0x0002);
    }

    #[test]
    fn test_ld_nn_ix() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.ix = 0x5678;
        cpu.pc = 0x0000;
        ram.write(0x0000, 0x00); // Address low
        ram.write(0x0001, 0x30); // Address high = 0x3000

        cpu.ld_nn_idx(&mut ram, false);

        assert_eq!(ram.read(0x3000), 0x78); // Low byte
        assert_eq!(ram.read(0x3001), 0x56); // High byte
    }

    #[test]
    fn test_ld_ix_nn_ind() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x0000;
        ram.write(0x0000, 0x00); // Address low
        ram.write(0x0001, 0x40); // Address high = 0x4000
        ram.write(0x4000, 0xEF); // Low byte
        ram.write(0x4001, 0xBE); // High byte

        cpu.ld_idx_nn_ind(&mut ram, false);

        assert_eq!(cpu.ix, 0xBEEF);
    }

    #[test]
    fn test_add_ix_bc() {
        let mut cpu = Z80::new();

        cpu.ix = 0x1000;
        cpu.set_bc(0x0234);

        cpu.add_idx_rr(false, 0); // ADD IX, BC

        assert_eq!(cpu.ix, 0x1234);
        assert!(!cpu.flag_n());
    }

    #[test]
    fn test_add_ix_bc_carry() {
        let mut cpu = Z80::new();

        cpu.ix = 0xFFFF;
        cpu.set_bc(0x0002);

        cpu.add_idx_rr(false, 0);

        assert_eq!(cpu.ix, 0x0001);
        assert!(cpu.flag_c()); // Carry set
    }

    #[test]
    fn test_add_iy_iy() {
        let mut cpu = Z80::new();

        cpu.iy = 0x1234;

        cpu.add_idx_rr(true, 2); // ADD IY, IY (doubles it)

        assert_eq!(cpu.iy, 0x2468);
    }

    #[test]
    fn test_inc_ix() {
        let mut cpu = Z80::new();

        cpu.ix = 0x1234;

        cpu.inc_idx(false);

        assert_eq!(cpu.ix, 0x1235);
    }

    #[test]
    fn test_dec_iy() {
        let mut cpu = Z80::new();

        cpu.iy = 0x5000;

        cpu.dec_idx(true);

        assert_eq!(cpu.iy, 0x4FFF);
    }

    #[test]
    fn test_push_pop_ix() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0x2000;
        cpu.ix = 0xABCD;

        cpu.push_idx(&mut ram, false);

        assert_eq!(cpu.sp, 0x1FFE);
        assert_eq!(ram.read(0x1FFE), 0xCD); // Low
        assert_eq!(ram.read(0x1FFF), 0xAB); // High

        cpu.ix = 0x0000; // Clear

        cpu.pop_idx(&mut ram, false);

        assert_eq!(cpu.ix, 0xABCD);
        assert_eq!(cpu.sp, 0x2000);
    }

    #[test]
    fn test_ex_sp_ix() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0x2000;
        cpu.ix = 0x1234;

        ram.write(0x2000, 0xCD); // Low
        ram.write(0x2001, 0xAB); // High = 0xABCD

        cpu.ex_sp_idx(&mut ram, false);

        assert_eq!(cpu.ix, 0xABCD); // IX got value from stack
        assert_eq!(ram.read(0x2000), 0x34); // Stack got IX low
        assert_eq!(ram.read(0x2001), 0x12); // Stack got IX high
    }

    #[test]
    fn test_ld_sp_ix() {
        let mut cpu = Z80::new();

        cpu.ix = 0xFFFE;
        cpu.sp = 0x0000;

        cpu.ld_sp_idx(false);

        assert_eq!(cpu.sp, 0xFFFE);
    }

    #[test]
    fn test_ld_sp_iy() {
        let mut cpu = Z80::new();

        cpu.iy = 0x8000;
        cpu.sp = 0x0000;

        cpu.ld_sp_idx(true);

        assert_eq!(cpu.sp, 0x8000);
    }

    // ========================================================================
    // Tests with Displacement
    // ========================================================================

    #[test]
    fn test_ld_r_idx_d() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.ix = 0x1000;
        cpu.pc = 0x0000;
        ram.write(0x0000, 0x05); // Displacement +5
        ram.write(0x1005, 0x42); // Value at IX+5

        cpu.ld_r_idx_d(&mut ram, false, 0); // LD B, (IX+5)

        assert_eq!(cpu.b, 0x42);
        assert_eq!(cpu.pc, 0x0001);
    }

    #[test]
    fn test_ld_r_idx_d_negative() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.iy = 0x2000;
        cpu.pc = 0x0000;
        ram.write(0x0000, 0xFC_u8); // Displacement -4 (0xFC = -4 in i8)
        ram.write(0x1FFC, 0xAB); // Value at IY-4

        cpu.ld_r_idx_d(&mut ram, true, 7); // LD A, (IY-4)

        assert_eq!(cpu.a, 0xAB);
    }

    #[test]
    fn test_ld_idx_d_r() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.ix = 0x3000;
        cpu.c = 0x99;
        cpu.pc = 0x0000;
        ram.write(0x0000, 0x0A); // Displacement +10

        cpu.ld_idx_d_r(&mut ram, false, 1); // LD (IX+10), C

        assert_eq!(ram.read(0x300A), 0x99);
    }

    #[test]
    fn test_ld_idx_d_n() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.iy = 0x4000;
        cpu.pc = 0x0000;
        ram.write(0x0000, 0x02); // Displacement +2
        ram.write(0x0001, 0x55); // Immediate value

        cpu.ld_idx_d_n(&mut ram, true); // LD (IY+2), 0x55

        assert_eq!(ram.read(0x4002), 0x55);
        assert_eq!(cpu.pc, 0x0002);
    }

    #[test]
    fn test_inc_idx_d() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.ix = 0x5000;
        cpu.pc = 0x0000;
        ram.write(0x0000, 0x08); // Displacement +8
        ram.write(0x5008, 0x7F); // Value that will overflow

        cpu.inc_idx_d(&mut ram, false); // INC (IX+8)

        assert_eq!(ram.read(0x5008), 0x80);
        assert!(cpu.flag_pv()); // Overflow
        assert!(cpu.flag_s()); // Sign
        assert!(!cpu.flag_z());
    }

    #[test]
    fn test_dec_idx_d() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.iy = 0x6000;
        cpu.pc = 0x0000;
        ram.write(0x0000, 0x80_u8); // Displacement -128
        ram.write(0x5F80, 0x01); // Value

        cpu.dec_idx_d(&mut ram, true); // DEC (IY-128)

        assert_eq!(ram.read(0x5F80), 0x00);
        assert!(cpu.flag_z()); // Zero
    }

    #[test]
    fn test_add_a_idx_d() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.ix = 0x7000;
        cpu.a = 0x10;
        cpu.pc = 0x0000;
        ram.write(0x0000, 0x03); // Displacement +3
        ram.write(0x7003, 0x20);

        cpu.add_a_idx_d(&mut ram, false); // ADD A, (IX+3)

        assert_eq!(cpu.a, 0x30);
        assert!(!cpu.flag_n());
    }

    #[test]
    fn test_sub_idx_d() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.iy = 0x8000;
        cpu.a = 0x50;
        cpu.pc = 0x0000;
        ram.write(0x0000, 0x01); // Displacement +1
        ram.write(0x8001, 0x20);

        cpu.sub_idx_d(&mut ram, true); // SUB (IY+1)

        assert_eq!(cpu.a, 0x30);
        assert!(cpu.flag_n());
    }

    #[test]
    fn test_and_idx_d() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.ix = 0x9000;
        cpu.a = 0xFF;
        cpu.pc = 0x0000;
        ram.write(0x0000, 0x00); // Displacement 0
        ram.write(0x9000, 0x0F);

        cpu.and_idx_d(&mut ram, false); // AND (IX+0)

        assert_eq!(cpu.a, 0x0F);
        assert!(cpu.flag_h()); // Always set for AND
    }

    #[test]
    fn test_xor_idx_d() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.iy = 0xA000;
        cpu.a = 0xAA;
        cpu.pc = 0x0000;
        ram.write(0x0000, 0x7F); // Displacement +127
        ram.write(0xA07F, 0x55);

        cpu.xor_idx_d(&mut ram, true); // XOR (IY+127)

        assert_eq!(cpu.a, 0xFF);
    }

    #[test]
    fn test_cp_idx_d() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.ix = 0xB000;
        cpu.a = 0x30;
        cpu.pc = 0x0000;
        ram.write(0x0000, 0x02); // Displacement +2
        ram.write(0xB002, 0x30);

        cpu.cp_idx_d(&mut ram, false); // CP (IX+2)

        assert_eq!(cpu.a, 0x30); // A unchanged
        assert!(cpu.flag_z()); // Equal
        assert!(cpu.flag_n());
    }
}
