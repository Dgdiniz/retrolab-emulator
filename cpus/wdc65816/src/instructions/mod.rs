//! 65C816 instruction implementations.

pub mod arithmetic;
pub mod block_move;
pub mod branches;
pub mod compare;
pub mod flags;
pub mod load_store;
pub mod logical;
pub mod shifts;
pub mod stack;
pub mod system;

use crate::Wdc65816;

// Status register flag bits (native mode: NVMXDIZC)
pub const FLAG_C: u8 = 0x01; // Carry
pub const FLAG_Z: u8 = 0x02; // Zero
pub const FLAG_I: u8 = 0x04; // IRQ disable
pub const FLAG_D: u8 = 0x08; // Decimal (BCD)
pub const FLAG_X: u8 = 0x10; // Index register size (native: 0=16-bit, 1=8-bit)
pub const FLAG_M: u8 = 0x20; // Accumulator size  (native: 0=16-bit, 1=8-bit)
pub const FLAG_V: u8 = 0x40; // Overflow
pub const FLAG_N: u8 = 0x80; // Negative

// In emulation mode, bits 4/5 are B (break) and unused (always 1)
pub const FLAG_B: u8 = 0x10; // Break (emulation mode only, same bit as X)

impl Wdc65816 {
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

    #[inline]
    pub(crate) fn set_nz_8(&mut self, val: u8) {
        self.set_flag(FLAG_Z, val == 0);
        self.set_flag(FLAG_N, val & 0x80 != 0);
    }

    #[inline]
    pub(crate) fn set_nz_16(&mut self, val: u16) {
        self.set_flag(FLAG_Z, val == 0);
        self.set_flag(FLAG_N, val & 0x8000 != 0);
    }

    /// Apply side effects when X flag changes to 1 (8-bit index):
    /// high bytes of X and Y are zeroed.
    pub(crate) fn on_index_to_8bit(&mut self) {
        self.x &= 0x00FF;
        self.y &= 0x00FF;
    }
}
