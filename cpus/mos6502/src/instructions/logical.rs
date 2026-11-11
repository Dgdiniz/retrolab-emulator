//! Logical instructions: AND, ORA, EOR.

use crate::addressing;
use crate::decode::OpcodeInfo;
use crate::Mos6502;
use retro_core::cpu::Addressable;

impl Mos6502 {
    pub(crate) fn and(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        self.a &= bus.read(res.addr as u32);
        self.set_nz(self.a);
        if info.page_penalty && res.page_crossed {
            1
        } else {
            0
        }
    }

    pub(crate) fn ora(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        self.a |= bus.read(res.addr as u32);
        self.set_nz(self.a);
        if info.page_penalty && res.page_crossed {
            1
        } else {
            0
        }
    }

    pub(crate) fn eor(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        self.a ^= bus.read(res.addr as u32);
        self.set_nz(self.a);
        if info.page_penalty && res.page_crossed {
            1
        } else {
            0
        }
    }
}
