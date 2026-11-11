//! Arithmetic instructions: ADC, SBC, INC, DEC, INX, INY, DEX, DEY.

use crate::decode::OpcodeInfo;
use crate::instructions::{FLAG_C, FLAG_D, FLAG_V};
use crate::Wdc65816;
use retro_core::cpu::Addressable;

impl Wdc65816 {
    pub(crate) fn adc(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        let extra = if info.penalty_page && resolved.page_crossed {
            1
        } else {
            0
        };
        let carry = if self.get_flag(FLAG_C) { 1u16 } else { 0 };

        if self.a_is_8bit() {
            let a = self.a_lo() as u16;
            let val = bus.read(resolved.addr) as u16;

            if self.get_flag(FLAG_D) {
                let mut lo = (a & 0x0F) + (val & 0x0F) + carry;
                if lo > 9 {
                    lo += 6;
                }
                let mut hi = (a >> 4) + (val >> 4) + if lo > 0x0F { 1 } else { 0 };
                self.set_flag(FLAG_V, (!(a ^ val) & (a ^ (hi << 4))) & 0x80 != 0);
                if hi > 9 {
                    hi += 6;
                }
                self.set_flag(FLAG_C, hi > 0x0F);
                let result = ((hi & 0x0F) << 4) | (lo & 0x0F);
                self.set_a_lo(result as u8);
                self.set_nz_8(result as u8);
            } else {
                let result = a + val + carry;
                self.set_flag(FLAG_C, result > 0xFF);
                self.set_flag(FLAG_V, (!(a ^ val) & (a ^ result)) & 0x80 != 0);
                self.set_a_lo(result as u8);
                self.set_nz_8(result as u8);
            }
        } else {
            let a = self.c as u32;
            let lo = bus.read(resolved.addr) as u32;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u32;
            let val = (hi << 8) | lo;

            if self.get_flag(FLAG_D) {
                let carry32 = carry as u32;
                // Per-nibble BCD addition. The carry into the next
                // nibble is computed from the *pre-adjust* sum (`> 9`)
                // exactly as the silicon does, adjusting in-place and
                // reading bit 4 of the post-adjust value drops the
                // carry that the `+6` itself generated.
                let n0 = (a & 0xF) + (val & 0xF) + carry32;
                let c0 = (n0 > 9) as u32;
                let n0_adj = if n0 > 9 { n0 + 6 } else { n0 };

                let n1 = ((a >> 4) & 0xF) + ((val >> 4) & 0xF) + c0;
                let c1 = (n1 > 9) as u32;
                let n1_adj = if n1 > 9 { n1 + 6 } else { n1 };

                let n2 = ((a >> 8) & 0xF) + ((val >> 8) & 0xF) + c1;
                let c2 = (n2 > 9) as u32;
                let n2_adj = if n2 > 9 { n2 + 6 } else { n2 };

                let n3 = ((a >> 12) & 0xF) + ((val >> 12) & 0xF) + c2;
                // V flag uses the binary (pre-BCD-adjust) high-nibble
                // sum so it reflects signed-overflow on the MSB.
                let signed_bits = ((n3 & 0xF) << 12)
                    | ((n2_adj & 0xF) << 8)
                    | ((n1_adj & 0xF) << 4)
                    | (n0_adj & 0xF);
                self.set_flag(FLAG_V, (!(a ^ val) & (a ^ signed_bits)) & 0x8000 != 0);
                let c3 = (n3 > 9) as u32;
                let n3_adj = if n3 > 9 { n3 + 6 } else { n3 };

                self.set_flag(FLAG_C, c3 != 0);
                let result = ((n3_adj & 0xF) << 12)
                    | ((n2_adj & 0xF) << 8)
                    | ((n1_adj & 0xF) << 4)
                    | (n0_adj & 0xF);
                self.c = result as u16;
                self.set_nz_16(result as u16);
            } else {
                let carry32 = carry as u32;
                let result = a + val + carry32;
                self.set_flag(FLAG_C, result > 0xFFFF);
                self.set_flag(FLAG_V, (!(a ^ val) & (a ^ result)) & 0x8000 != 0);
                self.c = result as u16;
                self.set_nz_16(result as u16);
            }
        }
        extra
    }

    pub(crate) fn sbc(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        let extra = if info.penalty_page && resolved.page_crossed {
            1
        } else {
            0
        };
        let borrow = if self.get_flag(FLAG_C) { 0u16 } else { 1 };

        if self.a_is_8bit() {
            let a = self.a_lo() as u16;
            let val = bus.read(resolved.addr) as u16;

            if self.get_flag(FLAG_D) {
                // Per-nibble BCD subtraction with borrow propagation.
                // `wrapping_sub` underflows into the `0xFFF0..=0xFFFF`
                // band, so we detect borrow via an unsigned compare
                // against `0x0F` (a signed compare would mis-classify
                // the high-bit-set u16 as "less than" and skip the
                // borrow into the high nibble).
                let mut lo = (a & 0x0F).wrapping_sub(val & 0x0F).wrapping_sub(borrow);
                let lo_borrow = if lo > 0x0F { 1u16 } else { 0 };
                if lo & 0x10 != 0 {
                    lo = lo.wrapping_sub(6) & 0x0F;
                }
                let mut hi = (a >> 4).wrapping_sub(val >> 4).wrapping_sub(lo_borrow);
                let raw = a.wrapping_sub(val).wrapping_sub(borrow);
                self.set_flag(FLAG_V, ((a ^ val) & (a ^ raw)) & 0x80 != 0);
                if hi & 0x10 != 0 {
                    hi = hi.wrapping_sub(6) & 0x0F;
                }
                self.set_flag(FLAG_C, raw < 0x100);
                let result = (hi << 4) | (lo & 0x0F);
                self.set_a_lo(result as u8);
                self.set_nz_8(result as u8);
            } else {
                let result = a.wrapping_sub(val).wrapping_sub(borrow);
                self.set_flag(FLAG_C, result < 0x100);
                self.set_flag(FLAG_V, ((a ^ val) & (a ^ result)) & 0x80 != 0);
                self.set_a_lo(result as u8);
                self.set_nz_8(result as u8);
            }
        } else {
            let a = self.c as u32;
            let lo = bus.read(resolved.addr) as u32;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u32;
            let val = (hi << 8) | lo;
            let borrow32 = borrow as u32;

            if self.get_flag(FLAG_D) {
                // V and C flags use the pure binary subtraction (input
                // borrow is `1 - C`, so `borrow32 == 0` means carry-in).
                let result_bin = a.wrapping_sub(val).wrapping_sub(borrow32);
                self.set_flag(FLAG_V, ((a ^ val) & (a ^ result_bin)) & 0x8000 != 0);
                self.set_flag(FLAG_C, result_bin < 0x10000);

                // Per-nibble BCD subtraction. The borrow into the next
                // nibble is taken from the *pre-adjust* sign bit (`> 9`
                // after `wrapping_sub` means bit 4 / 0x10 is set), then
                // the `-6` correction is applied to produce the result
                // digit. Reading the borrow off the adjusted value would
                // lose information when consecutive borrows propagate.
                let n0 = (a & 0xF).wrapping_sub(val & 0xF).wrapping_sub(borrow32);
                let b0 = (n0 >> 4) & 1;
                let n0_adj = if n0 & 0x10 != 0 {
                    n0.wrapping_sub(6)
                } else {
                    n0
                };

                let n1 = ((a >> 4) & 0xF)
                    .wrapping_sub((val >> 4) & 0xF)
                    .wrapping_sub(b0);
                let b1 = (n1 >> 4) & 1;
                let n1_adj = if n1 & 0x10 != 0 {
                    n1.wrapping_sub(6)
                } else {
                    n1
                };

                let n2 = ((a >> 8) & 0xF)
                    .wrapping_sub((val >> 8) & 0xF)
                    .wrapping_sub(b1);
                let b2 = (n2 >> 4) & 1;
                let n2_adj = if n2 & 0x10 != 0 {
                    n2.wrapping_sub(6)
                } else {
                    n2
                };

                let n3 = ((a >> 12) & 0xF)
                    .wrapping_sub((val >> 12) & 0xF)
                    .wrapping_sub(b2);
                let n3_adj = if n3 & 0x10 != 0 {
                    n3.wrapping_sub(6)
                } else {
                    n3
                };

                let result = ((n3_adj & 0xF) << 12)
                    | ((n2_adj & 0xF) << 8)
                    | ((n1_adj & 0xF) << 4)
                    | (n0_adj & 0xF);
                self.c = result as u16;
                self.set_nz_16(result as u16);
            } else {
                let result = a.wrapping_sub(val).wrapping_sub(borrow32);
                self.set_flag(FLAG_C, result < 0x10000);
                self.set_flag(FLAG_V, ((a ^ val) & (a ^ result)) & 0x8000 != 0);
                self.c = result as u16;
                self.set_nz_16(result as u16);
            }
        }
        extra
    }

    /// INC accumulator
    pub(crate) fn inc_a(&mut self) {
        if self.a_is_8bit() {
            let val = self.a_lo().wrapping_add(1);
            self.set_a_lo(val);
            self.set_nz_8(val);
        } else {
            self.c = self.c.wrapping_add(1);
            self.set_nz_16(self.c);
        }
    }

    /// INC memory
    pub(crate) fn inc_mem(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        if self.a_is_8bit() {
            let val = bus.read(resolved.addr).wrapping_add(1);
            bus.write(resolved.addr, val);
            self.set_nz_8(val);
        } else {
            let lo = bus.read(resolved.addr) as u16;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u16;
            let val = ((hi << 8) | lo).wrapping_add(1);
            bus.write(resolved.addr, val as u8);
            bus.write(resolved.addr.wrapping_add(1), (val >> 8) as u8);
            self.set_nz_16(val);
        }
        0
    }

    /// DEC accumulator
    pub(crate) fn dec_a(&mut self) {
        if self.a_is_8bit() {
            let val = self.a_lo().wrapping_sub(1);
            self.set_a_lo(val);
            self.set_nz_8(val);
        } else {
            self.c = self.c.wrapping_sub(1);
            self.set_nz_16(self.c);
        }
    }

    /// DEC memory
    pub(crate) fn dec_mem(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let resolved = self.resolve_address(bus, info.mode);
        if self.a_is_8bit() {
            let val = bus.read(resolved.addr).wrapping_sub(1);
            bus.write(resolved.addr, val);
            self.set_nz_8(val);
        } else {
            let lo = bus.read(resolved.addr) as u16;
            let hi = bus.read(resolved.addr.wrapping_add(1)) as u16;
            let val = ((hi << 8) | lo).wrapping_sub(1);
            bus.write(resolved.addr, val as u8);
            bus.write(resolved.addr.wrapping_add(1), (val >> 8) as u8);
            self.set_nz_16(val);
        }
        0
    }

    pub(crate) fn inx(&mut self) {
        if self.xy_is_8bit() {
            self.x = (self.x as u8).wrapping_add(1) as u16;
            self.set_nz_8(self.x as u8);
        } else {
            self.x = self.x.wrapping_add(1);
            self.set_nz_16(self.x);
        }
    }

    pub(crate) fn iny(&mut self) {
        if self.xy_is_8bit() {
            self.y = (self.y as u8).wrapping_add(1) as u16;
            self.set_nz_8(self.y as u8);
        } else {
            self.y = self.y.wrapping_add(1);
            self.set_nz_16(self.y);
        }
    }

    pub(crate) fn dex(&mut self) {
        if self.xy_is_8bit() {
            self.x = (self.x as u8).wrapping_sub(1) as u16;
            self.set_nz_8(self.x as u8);
        } else {
            self.x = self.x.wrapping_sub(1);
            self.set_nz_16(self.x);
        }
    }

    pub(crate) fn dey(&mut self) {
        if self.xy_is_8bit() {
            self.y = (self.y as u8).wrapping_sub(1) as u16;
            self.set_nz_8(self.y as u8);
        } else {
            self.y = self.y.wrapping_sub(1);
            self.set_nz_16(self.y);
        }
    }
}
