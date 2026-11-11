//! Shift and rotate instructions: ASL, LSR, ROL, ROR.

use crate::decode::OpcodeInfo;
use crate::instructions::FLAG_C;
use crate::Wdc65816;
use retro_core::cpu::Addressable;

impl Wdc65816 {
    // ---- ASL ----
    pub(crate) fn asl_a(&mut self) {
        if self.a_is_8bit() {
            let val = self.a_lo();
            self.set_flag(FLAG_C, val & 0x80 != 0);
            let result = val << 1;
            self.set_a_lo(result);
            self.set_nz_8(result);
        } else {
            self.set_flag(FLAG_C, self.c & 0x8000 != 0);
            self.c <<= 1;
            self.set_nz_16(self.c);
        }
    }

    pub(crate) fn asl_mem(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        if self.a_is_8bit() {
            let val = bus.read(resolved.addr);
            self.set_flag(FLAG_C, val & 0x80 != 0);
            let result = val << 1;
            bus.write(resolved.addr, result);
            self.set_nz_8(result);
        } else {
            let lo = bus.read(resolved.addr) as u16;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u16;
            let val = (hi << 8) | lo;
            self.set_flag(FLAG_C, val & 0x8000 != 0);
            let result = val << 1;
            bus.write(resolved.addr, result as u8);
            bus.write(resolved.addr.wrapping_add(1), (result >> 8) as u8);
            self.set_nz_16(result);
        }
        0
    }

    // ---- LSR ----
    pub(crate) fn lsr_a(&mut self) {
        if self.a_is_8bit() {
            let val = self.a_lo();
            self.set_flag(FLAG_C, val & 1 != 0);
            let result = val >> 1;
            self.set_a_lo(result);
            self.set_nz_8(result);
        } else {
            self.set_flag(FLAG_C, self.c & 1 != 0);
            self.c >>= 1;
            self.set_nz_16(self.c);
        }
    }

    pub(crate) fn lsr_mem(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        if self.a_is_8bit() {
            let val = bus.read(resolved.addr);
            self.set_flag(FLAG_C, val & 1 != 0);
            let result = val >> 1;
            bus.write(resolved.addr, result);
            self.set_nz_8(result);
        } else {
            let lo = bus.read(resolved.addr) as u16;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u16;
            let val = (hi << 8) | lo;
            self.set_flag(FLAG_C, val & 1 != 0);
            let result = val >> 1;
            bus.write(resolved.addr, result as u8);
            bus.write(resolved.addr.wrapping_add(1), (result >> 8) as u8);
            self.set_nz_16(result);
        }
        0
    }

    // ---- ROL ----
    pub(crate) fn rol_a(&mut self) {
        let old_carry = if self.get_flag(FLAG_C) { 1 } else { 0 };
        if self.a_is_8bit() {
            let val = self.a_lo();
            self.set_flag(FLAG_C, val & 0x80 != 0);
            let result = (val << 1) | old_carry;
            self.set_a_lo(result);
            self.set_nz_8(result);
        } else {
            self.set_flag(FLAG_C, self.c & 0x8000 != 0);
            self.c = (self.c << 1) | old_carry as u16;
            self.set_nz_16(self.c);
        }
    }

    pub(crate) fn rol_mem(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let old_carry = if self.get_flag(FLAG_C) { 1 } else { 0 };
        let resolved = self.resolve_address(bus, info.mode);
        if self.a_is_8bit() {
            let val = bus.read(resolved.addr);
            self.set_flag(FLAG_C, val & 0x80 != 0);
            let result = (val << 1) | old_carry;
            bus.write(resolved.addr, result);
            self.set_nz_8(result);
        } else {
            let lo = bus.read(resolved.addr) as u16;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u16;
            let val = (hi << 8) | lo;
            self.set_flag(FLAG_C, val & 0x8000 != 0);
            let result = (val << 1) | old_carry as u16;
            bus.write(resolved.addr, result as u8);
            bus.write(resolved.addr.wrapping_add(1), (result >> 8) as u8);
            self.set_nz_16(result);
        }
        0
    }

    // ---- ROR ----
    pub(crate) fn ror_a(&mut self) {
        let old_carry = if self.get_flag(FLAG_C) { 1 } else { 0 };
        if self.a_is_8bit() {
            let val = self.a_lo();
            self.set_flag(FLAG_C, val & 1 != 0);
            let result = (val >> 1) | (old_carry << 7);
            self.set_a_lo(result);
            self.set_nz_8(result);
        } else {
            self.set_flag(FLAG_C, self.c & 1 != 0);
            self.c = (self.c >> 1) | ((old_carry as u16) << 15);
            self.set_nz_16(self.c);
        }
    }

    pub(crate) fn ror_mem(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let old_carry = if self.get_flag(FLAG_C) { 1 } else { 0 };
        let resolved = self.resolve_address(bus, info.mode);
        if self.a_is_8bit() {
            let val = bus.read(resolved.addr);
            self.set_flag(FLAG_C, val & 1 != 0);
            let result = (val >> 1) | (old_carry << 7);
            bus.write(resolved.addr, result);
            self.set_nz_8(result);
        } else {
            let lo = bus.read(resolved.addr) as u16;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u16;
            let val = (hi << 8) | lo;
            self.set_flag(FLAG_C, val & 1 != 0);
            let result = (val >> 1) | ((old_carry as u16) << 15);
            bus.write(resolved.addr, result as u8);
            bus.write(resolved.addr.wrapping_add(1), (result >> 8) as u8);
            self.set_nz_16(result);
        }
        0
    }
}
