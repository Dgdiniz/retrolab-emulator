//! Load, store, and register transfer instructions.

use crate::decode::OpcodeInfo;
use crate::Wdc65816;
use retro_core::cpu::Addressable;

impl Wdc65816 {
    pub(crate) fn lda(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        let extra = if info.penalty_page && resolved.page_crossed {
            1
        } else {
            0
        };
        if self.a_is_8bit() {
            let val = bus.read(resolved.addr);
            self.set_a_lo(val);
            self.set_nz_8(val);
        } else {
            let lo = bus.read(resolved.addr) as u16;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u16;
            let val = (hi << 8) | lo;
            self.c = val;
            self.set_nz_16(val);
        }
        extra
    }

    pub(crate) fn ldx(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        let extra = if info.penalty_page && resolved.page_crossed {
            1
        } else {
            0
        };
        if self.xy_is_8bit() {
            let val = bus.read(resolved.addr);
            self.x = val as u16;
            self.set_nz_8(val);
        } else {
            let lo = bus.read(resolved.addr) as u16;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u16;
            let val = (hi << 8) | lo;
            self.x = val;
            self.set_nz_16(val);
        }
        extra
    }

    pub(crate) fn ldy(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        let extra = if info.penalty_page && resolved.page_crossed {
            1
        } else {
            0
        };
        if self.xy_is_8bit() {
            let val = bus.read(resolved.addr);
            self.y = val as u16;
            self.set_nz_8(val);
        } else {
            let lo = bus.read(resolved.addr) as u16;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u16;
            let val = (hi << 8) | lo;
            self.y = val;
            self.set_nz_16(val);
        }
        extra
    }

    pub(crate) fn sta(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        if self.a_is_8bit() {
            bus.write(resolved.addr, self.a_lo());
        } else {
            bus.write(resolved.addr, self.c as u8);
            bus.write(resolved.addr.wrapping_add(1), (self.c >> 8) as u8);
        }
        0
    }

    pub(crate) fn stx(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        if self.xy_is_8bit() {
            bus.write(resolved.addr, self.x as u8);
        } else {
            bus.write(resolved.addr, self.x as u8);
            bus.write(resolved.addr.wrapping_add(1), (self.x >> 8) as u8);
        }
        0
    }

    pub(crate) fn sty(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        if self.xy_is_8bit() {
            bus.write(resolved.addr, self.y as u8);
        } else {
            bus.write(resolved.addr, self.y as u8);
            bus.write(resolved.addr.wrapping_add(1), (self.y >> 8) as u8);
        }
        0
    }

    pub(crate) fn stz(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        if self.a_is_8bit() {
            bus.write(resolved.addr, 0);
        } else {
            bus.write(resolved.addr, 0);
            bus.write(resolved.addr.wrapping_add(1), 0);
        }
        0
    }

    // ---- Register transfers ----

    pub(crate) fn tax(&mut self) {
        if self.xy_is_8bit() {
            let val = self.a_lo();
            self.x = val as u16;
            self.set_nz_8(val);
        } else {
            self.x = self.c;
            self.set_nz_16(self.c);
        }
    }

    pub(crate) fn tay(&mut self) {
        if self.xy_is_8bit() {
            let val = self.a_lo();
            self.y = val as u16;
            self.set_nz_8(val);
        } else {
            self.y = self.c;
            self.set_nz_16(self.c);
        }
    }

    pub(crate) fn txa(&mut self) {
        if self.a_is_8bit() {
            let val = self.x as u8;
            self.set_a_lo(val);
            self.set_nz_8(val);
        } else {
            self.c = self.x;
            self.set_nz_16(self.x);
        }
    }

    pub(crate) fn tya(&mut self) {
        if self.a_is_8bit() {
            let val = self.y as u8;
            self.set_a_lo(val);
            self.set_nz_8(val);
        } else {
            self.c = self.y;
            self.set_nz_16(self.y);
        }
    }

    pub(crate) fn tsx(&mut self) {
        if self.xy_is_8bit() {
            let val = self.sp as u8;
            self.x = val as u16;
            self.set_nz_8(val);
        } else {
            self.x = self.sp;
            self.set_nz_16(self.sp);
        }
    }

    pub(crate) fn txs(&mut self) {
        if self.emulation {
            self.sp = 0x0100 | (self.x & 0xFF);
        } else {
            self.sp = self.x;
        }
    }

    /// Transfer 16-bit accumulator C to Direct Page register D.
    pub(crate) fn tcd(&mut self) {
        self.d = self.c;
        self.set_nz_16(self.c);
    }

    /// Transfer 16-bit accumulator C to Stack Pointer.
    pub(crate) fn tcs(&mut self) {
        if self.emulation {
            self.sp = 0x0100 | (self.c & 0xFF);
        } else {
            self.sp = self.c;
        }
    }

    /// Transfer Direct Page register to 16-bit accumulator C.
    pub(crate) fn tdc(&mut self) {
        self.c = self.d;
        self.set_nz_16(self.d);
    }

    /// Transfer Stack Pointer to 16-bit accumulator C.
    pub(crate) fn tsc(&mut self) {
        self.c = self.sp;
        self.set_nz_16(self.sp);
    }

    pub(crate) fn txy(&mut self) {
        if self.xy_is_8bit() {
            let val = self.x as u8;
            self.y = val as u16;
            self.set_nz_8(val);
        } else {
            self.y = self.x;
            self.set_nz_16(self.x);
        }
    }

    pub(crate) fn tyx(&mut self) {
        if self.xy_is_8bit() {
            let val = self.y as u8;
            self.x = val as u16;
            self.set_nz_8(val);
        } else {
            self.x = self.y;
            self.set_nz_16(self.y);
        }
    }
}
