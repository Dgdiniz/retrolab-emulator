//! Arithmetic instructions: ADC, SBC, INC, DEC, INX, INY, DEX, DEY.

use crate::addressing;
use crate::decode::OpcodeInfo;
use crate::instructions::*;
use crate::Mos6502;
use retro_core::cpu::Addressable;

impl Mos6502 {
    pub(crate) fn adc(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        let val = bus.read(res.addr as u32);
        self.adc_internal(val);
        if info.page_penalty && res.page_crossed {
            1
        } else {
            0
        }
    }

    pub(crate) fn sbc(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        let val = bus.read(res.addr as u32);
        self.sbc_internal(val);
        if info.page_penalty && res.page_crossed {
            1
        } else {
            0
        }
    }

    pub(crate) fn adc_internal(&mut self, val: u8) {
        let a = self.a as u16;
        let v = val as u16;
        let c = if self.get_flag(FLAG_C) { 1u16 } else { 0 };

        if !self.bcd_disabled && self.get_flag(FLAG_D) {
            // BCD mode
            let mut lo = (a & 0x0F) + (v & 0x0F) + c;
            if lo > 9 {
                lo += 6;
            }
            let mut hi = (a >> 4) + (v >> 4) + if lo > 0x0F { 1 } else { 0 };

            // Set N and V before BCD correction of high nibble
            let result_pre = ((hi << 4) | (lo & 0x0F)) as u8;
            self.set_flag(FLAG_Z, (a + v + c) as u8 == 0);
            self.set_flag(FLAG_N, result_pre & 0x80 != 0);

            let signed_sum = (self.a as i8 as i16) + (val as i8 as i16) + c as i16;
            self.set_flag(FLAG_V, !(-128..=127).contains(&signed_sum));

            if hi > 9 {
                hi += 6;
            }
            self.set_flag(FLAG_C, hi > 0x0F);
            self.a = ((hi << 4) | (lo & 0x0F)) as u8;
        } else {
            // Binary mode
            let sum = a + v + c;
            let result = sum as u8;
            self.set_flag(FLAG_C, sum > 0xFF);
            self.set_flag(FLAG_V, ((a ^ sum) & (v ^ sum) & 0x80) != 0);
            self.a = result;
            self.set_nz(self.a);
        }
    }

    pub(crate) fn sbc_internal(&mut self, val: u8) {
        if !self.bcd_disabled && self.get_flag(FLAG_D) {
            // BCD mode
            let a = self.a as u16;
            let v = val as u16;
            let c = if self.get_flag(FLAG_C) { 0u16 } else { 1 };

            let mut lo = (a & 0x0F).wrapping_sub(v & 0x0F).wrapping_sub(c);
            let borrow_lo = if lo & 0x10 != 0 { 1u16 } else { 0 };
            if lo & 0x10 != 0 {
                lo = lo.wrapping_sub(6) & 0x0F;
            }

            let mut hi = (a >> 4).wrapping_sub(v >> 4).wrapping_sub(borrow_lo);
            if hi & 0x10 != 0 {
                hi = hi.wrapping_sub(6);
            }

            let binary = a.wrapping_sub(v).wrapping_sub(c);
            self.set_flag(FLAG_C, binary < 0x100);
            let signed_sub = (self.a as i8 as i16) - (val as i8 as i16) - c as i16;
            self.set_flag(FLAG_V, !(-128..=127).contains(&signed_sub));
            self.set_nz(binary as u8);
            self.a = ((hi << 4) | (lo & 0x0F)) as u8;
        } else {
            // Binary SBC is identical to ADC with complemented operand
            self.adc_internal(!val);
        }
    }

    pub(crate) fn inc(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        let val = bus.read(res.addr as u32).wrapping_add(1);
        bus.write(res.addr as u32, val);
        self.set_nz(val);
        0
    }

    pub(crate) fn dec(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        let val = bus.read(res.addr as u32).wrapping_sub(1);
        bus.write(res.addr as u32, val);
        self.set_nz(val);
        0
    }

    pub(crate) fn inx(&mut self) {
        self.x = self.x.wrapping_add(1);
        self.set_nz(self.x);
    }

    pub(crate) fn iny(&mut self) {
        self.y = self.y.wrapping_add(1);
        self.set_nz(self.y);
    }

    pub(crate) fn dex(&mut self) {
        self.x = self.x.wrapping_sub(1);
        self.set_nz(self.x);
    }

    pub(crate) fn dey(&mut self) {
        self.y = self.y.wrapping_sub(1);
        self.set_nz(self.y);
    }
}
