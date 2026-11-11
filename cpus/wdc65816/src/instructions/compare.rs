//! Compare instructions: CMP, CPX, CPY.

use crate::addressing::AddressingMode;
use crate::decode::OpcodeInfo;
use crate::instructions::{FLAG_C, FLAG_N, FLAG_Z};
use crate::Wdc65816;
use retro_core::cpu::Addressable;

impl Wdc65816 {
    fn compare_8(&mut self, reg: u8, val: u8) {
        let result = (reg as u16).wrapping_sub(val as u16);
        self.set_flag(FLAG_C, reg >= val);
        self.set_flag(FLAG_Z, result as u8 == 0);
        self.set_flag(FLAG_N, result & 0x80 != 0);
    }

    fn compare_16(&mut self, reg: u16, val: u16) {
        let result = (reg as u32).wrapping_sub(val as u32);
        self.set_flag(FLAG_C, reg >= val);
        self.set_flag(FLAG_Z, result as u16 == 0);
        self.set_flag(FLAG_N, result & 0x8000 != 0);
    }

    fn read_operand_16(bus: &mut dyn Addressable, addr: u32, mode: AddressingMode) -> u16 {
        let in_bank_zero = (addr & 0xFF_0000) == 0;
        let is_immediate = matches!(mode, AddressingMode::Immediate | AddressingMode::ImmediateX);
        if in_bank_zero || is_immediate {
            Self::read_word_dp(bus, addr)
        } else {
            let lo = bus.read(addr) as u16;
            let hi = bus.read(addr.wrapping_add(1)) as u16;
            (hi << 8) | lo
        }
    }

    pub(crate) fn cmp(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        let extra = if info.penalty_page && resolved.page_crossed {
            1
        } else {
            0
        };
        if self.a_is_8bit() {
            let val = bus.read(resolved.addr);
            self.compare_8(self.a_lo(), val);
        } else {
            let val = Self::read_operand_16(bus, resolved.addr, info.mode);
            self.compare_16(self.c, val);
        }
        extra
    }

    pub(crate) fn cpx(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        let extra = if info.penalty_page && resolved.page_crossed {
            1
        } else {
            0
        };
        if self.xy_is_8bit() {
            let val = bus.read(resolved.addr);
            self.compare_8(self.x as u8, val);
        } else {
            let val = Self::read_operand_16(bus, resolved.addr, info.mode);
            self.compare_16(self.x, val);
        }
        extra
    }

    pub(crate) fn cpy(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        let extra = if info.penalty_page && resolved.page_crossed {
            1
        } else {
            0
        };
        if self.xy_is_8bit() {
            let val = bus.read(resolved.addr);
            self.compare_8(self.y as u8, val);
        } else {
            let val = Self::read_operand_16(bus, resolved.addr, info.mode);
            self.compare_16(self.y, val);
        }
        extra
    }
}
