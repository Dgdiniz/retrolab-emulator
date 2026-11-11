//! Compare and bit-test instructions: CMP, CPX, CPY, BIT.

use crate::addressing;
use crate::decode::OpcodeInfo;
use crate::instructions::*;
use crate::Mos6502;
use retro_core::cpu::Addressable;

impl Mos6502 {
    fn compare(&mut self, reg: u8, val: u8) {
        let result = reg.wrapping_sub(val);
        self.set_flag(FLAG_C, reg >= val);
        self.set_nz(result);
    }

    pub(crate) fn cmp(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        let val = bus.read(res.addr as u32);
        self.compare(self.a, val);
        if info.page_penalty && res.page_crossed {
            1
        } else {
            0
        }
    }

    pub(crate) fn cpx(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        let val = bus.read(res.addr as u32);
        self.compare(self.x, val);
        if info.page_penalty && res.page_crossed {
            1
        } else {
            0
        }
    }

    pub(crate) fn cpy(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        let val = bus.read(res.addr as u32);
        self.compare(self.y, val);
        if info.page_penalty && res.page_crossed {
            1
        } else {
            0
        }
    }

    pub(crate) fn bit(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        let val = bus.read(res.addr as u32);
        self.set_flag(FLAG_Z, self.a & val == 0);
        self.set_flag(FLAG_N, val & 0x80 != 0);
        self.set_flag(FLAG_V, val & 0x40 != 0);
        0
    }
}
