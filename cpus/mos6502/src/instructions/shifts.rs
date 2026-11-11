//! Shift and rotate instructions: ASL, LSR, ROL, ROR.

use crate::addressing::{self, AddressingMode};
use crate::decode::OpcodeInfo;
use crate::instructions::*;
use crate::Mos6502;
use retro_core::cpu::Addressable;

impl Mos6502 {
    pub(crate) fn asl(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        if info.mode == AddressingMode::Accumulator {
            self.set_flag(FLAG_C, self.a & 0x80 != 0);
            self.a <<= 1;
            self.set_nz(self.a);
        } else {
            let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
            self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
            let mut val = bus.read(res.addr as u32);
            self.set_flag(FLAG_C, val & 0x80 != 0);
            val <<= 1;
            bus.write(res.addr as u32, val);
            self.set_nz(val);
        }
        0
    }

    pub(crate) fn lsr(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        if info.mode == AddressingMode::Accumulator {
            self.set_flag(FLAG_C, self.a & 0x01 != 0);
            self.a >>= 1;
            self.set_nz(self.a);
        } else {
            let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
            self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
            let mut val = bus.read(res.addr as u32);
            self.set_flag(FLAG_C, val & 0x01 != 0);
            val >>= 1;
            bus.write(res.addr as u32, val);
            self.set_nz(val);
        }
        0
    }

    pub(crate) fn rol(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let old_carry = if self.get_flag(FLAG_C) { 1u8 } else { 0 };
        if info.mode == AddressingMode::Accumulator {
            self.set_flag(FLAG_C, self.a & 0x80 != 0);
            self.a = (self.a << 1) | old_carry;
            self.set_nz(self.a);
        } else {
            let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
            self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
            let mut val = bus.read(res.addr as u32);
            self.set_flag(FLAG_C, val & 0x80 != 0);
            val = (val << 1) | old_carry;
            bus.write(res.addr as u32, val);
            self.set_nz(val);
        }
        0
    }

    pub(crate) fn ror(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let old_carry = if self.get_flag(FLAG_C) { 0x80u8 } else { 0 };
        if info.mode == AddressingMode::Accumulator {
            self.set_flag(FLAG_C, self.a & 0x01 != 0);
            self.a = (self.a >> 1) | old_carry;
            self.set_nz(self.a);
        } else {
            let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
            self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
            let mut val = bus.read(res.addr as u32);
            self.set_flag(FLAG_C, val & 0x01 != 0);
            val = (val >> 1) | old_carry;
            bus.write(res.addr as u32, val);
            self.set_nz(val);
        }
        0
    }
}
