//! 6502 instruction implementations.

pub mod arithmetic;
pub mod branches;
pub mod compare;
pub mod flags;
pub mod load_store;
pub mod logical;
pub mod shifts;
pub mod stack;
pub mod system;

use crate::Mos6502;

// Status register flag bits
pub const FLAG_C: u8 = 0x01; // Carry
pub const FLAG_Z: u8 = 0x02; // Zero
pub const FLAG_I: u8 = 0x04; // IRQ disable
pub const FLAG_D: u8 = 0x08; // Decimal (BCD)
pub const FLAG_B: u8 = 0x10; // Break command
pub const FLAG_U: u8 = 0x20; // Unused (always 1)
pub const FLAG_V: u8 = 0x40; // Overflow
pub const FLAG_N: u8 = 0x80; // Negative

impl Mos6502 {
    #[inline]
    pub(crate) fn get_flag(&self, flag: u8) -> bool {
        self.p & flag != 0
    }

    #[inline]
    pub(crate) fn set_flag(&mut self, flag: u8, val: bool) {
        if val {
            self.p |= flag;
        } else {
            self.p &= !flag;
        }
    }

    /// Set N and Z flags based on a value.
    #[inline]
    pub(crate) fn set_nz(&mut self, val: u8) {
        self.set_flag(FLAG_Z, val == 0);
        self.set_flag(FLAG_N, val & 0x80 != 0);
    }
}
