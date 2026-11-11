//! The multiplier: bytes into a word (MULT, UMULT) and words into a long
//! (FMULT, LMULT). CFGR's fast-multiply bit shortens both.

use retro_core::cpu::Addressable;

use crate::registers::{config, status};
use crate::Gsu;

impl Gsu {
    /// `$80-$8F` MULT: the source's low byte times the operand's, both
    /// signed. Under ALT1, UMULT: unsigned. Unless the fast multiplier is
    /// on, it takes one clock tick more.
    pub(crate) fn multiply(&mut self, bus: &mut dyn Addressable, n: usize) {
        let a = self.source_value() as u8;
        let b = self.register_or_immediate(n) as u8;
        let product = if self.alt1() {
            u16::from(a) * u16::from(b)
        } else {
            (i16::from(a as i8) * i16::from(b as i8)) as u16
        };
        self.set_sign_and_zero(product);
        self.write_destination(product);
        self.clear_prefixes();
        if self.config & config::FAST_MULTIPLY == 0 {
            let clocks = u32::from(self.clock_divider);
            self.spend(bus, clocks);
        }
    }

    /// `$9F` FMULT: the source times R6, both signed; the destination takes
    /// the product's high word, and the carry bit 15 of its low word. Under
    /// ALT1, LMULT: R4 takes the low word as well. Seven clock ticks, three
    /// with the fast multiplier.
    pub(crate) fn multiply_words(&mut self, bus: &mut dyn Addressable) {
        let product = i32::from(self.source_value() as i16) * i32::from(self.registers[6] as i16);
        if self.alt1() {
            self.write_register(4, product as u16);
        }
        let high = (product >> 16) as u16;
        self.set_flag(status::SIGN, high & 0x8000 != 0);
        self.set_flag(status::CARRY, product & 0x8000 != 0);
        self.set_flag(status::ZERO, high == 0);
        self.write_destination(high);
        self.clear_prefixes();
        let ticks = if self.config & config::FAST_MULTIPLY != 0 {
            3
        } else {
            7
        };
        let clocks = ticks * u32::from(self.clock_divider);
        self.spend(bus, clocks);
    }
}
