//! Logical instructions: AND, ORA, EOR, BIT, TRB, TSB.

use crate::decode::OpcodeInfo;
use crate::instructions::{FLAG_N, FLAG_V, FLAG_Z};
use crate::Wdc65816;
use retro_core::cpu::Addressable;

impl Wdc65816 {
    pub(crate) fn and(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        let extra = if info.penalty_page && resolved.page_crossed {
            1
        } else {
            0
        };
        if self.a_is_8bit() {
            let val = bus.read(resolved.addr);
            let result = self.a_lo() & val;
            self.set_a_lo(result);
            self.set_nz_8(result);
        } else {
            let lo = bus.read(resolved.addr) as u16;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u16;
            let val = (hi << 8) | lo;
            self.c &= val;
            self.set_nz_16(self.c);
        }
        extra
    }

    pub(crate) fn ora(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        let extra = if info.penalty_page && resolved.page_crossed {
            1
        } else {
            0
        };
        if self.a_is_8bit() {
            let val = bus.read(resolved.addr);
            let result = self.a_lo() | val;
            self.set_a_lo(result);
            self.set_nz_8(result);
        } else {
            let lo = bus.read(resolved.addr) as u16;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u16;
            let val = (hi << 8) | lo;
            self.c |= val;
            self.set_nz_16(self.c);
        }
        extra
    }

    pub(crate) fn eor(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        let extra = if info.penalty_page && resolved.page_crossed {
            1
        } else {
            0
        };
        if self.a_is_8bit() {
            let val = bus.read(resolved.addr);
            let result = self.a_lo() ^ val;
            self.set_a_lo(result);
            self.set_nz_8(result);
        } else {
            let lo = bus.read(resolved.addr) as u16;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u16;
            let val = (hi << 8) | lo;
            self.c ^= val;
            self.set_nz_16(self.c);
        }
        extra
    }

    pub(crate) fn bit(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        use crate::addressing::AddressingMode;
        let resolved = self.resolve_address(bus, info.mode);
        let extra = if info.penalty_page && resolved.page_crossed {
            1
        } else {
            0
        };

        if self.a_is_8bit() {
            let val = bus.read(resolved.addr);
            let result = self.a_lo() & val;
            self.set_flag(FLAG_Z, result == 0);
            // Immediate mode: only Z flag affected
            if info.mode != AddressingMode::Immediate {
                self.set_flag(FLAG_N, val & 0x80 != 0);
                self.set_flag(FLAG_V, val & 0x40 != 0);
            }
        } else {
            let lo = bus.read(resolved.addr) as u16;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u16;
            let val = (hi << 8) | lo;
            let result = self.c & val;
            self.set_flag(FLAG_Z, result == 0);
            if info.mode != AddressingMode::Immediate {
                self.set_flag(FLAG_N, val & 0x8000 != 0);
                self.set_flag(FLAG_V, val & 0x4000 != 0);
            }
        }
        extra
    }

    /// TRB: Test and Reset Bits in memory
    pub(crate) fn trb(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        if self.a_is_8bit() {
            let val = bus.read(resolved.addr);
            self.set_flag(FLAG_Z, (self.a_lo() & val) == 0);
            bus.write(resolved.addr, val & !self.a_lo());
        } else {
            let lo = bus.read(resolved.addr) as u16;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u16;
            let val = (hi << 8) | lo;
            self.set_flag(FLAG_Z, (self.c & val) == 0);
            let result = val & !self.c;
            bus.write(resolved.addr, result as u8);
            bus.write(resolved.addr.wrapping_add(1), (result >> 8) as u8);
        }
        0
    }

    /// TSB: Test and Set Bits in memory
    pub(crate) fn tsb(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        if self.a_is_8bit() {
            let val = bus.read(resolved.addr);
            self.set_flag(FLAG_Z, (self.a_lo() & val) == 0);
            bus.write(resolved.addr, val | self.a_lo());
        } else {
            let lo = bus.read(resolved.addr) as u16;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u16;
            let val = (hi << 8) | lo;
            self.set_flag(FLAG_Z, (self.c & val) == 0);
            let result = val | self.c;
            bus.write(resolved.addr, result as u8);
            bus.write(resolved.addr.wrapping_add(1), (result >> 8) as u8);
        }
        0
    }
}
