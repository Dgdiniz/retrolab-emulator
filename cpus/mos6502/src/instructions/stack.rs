//! Stack instructions: PHA, PHP, PLA, PLP.

use crate::instructions::*;
use crate::Mos6502;
use retro_core::cpu::Addressable;

impl Mos6502 {
    pub(crate) fn pha(&mut self, bus: &mut dyn Addressable) {
        self.push_byte(bus, self.a);
    }

    pub(crate) fn php(&mut self, bus: &mut dyn Addressable) {
        // PHP always pushes with B and U bits set
        self.push_byte(bus, self.p | FLAG_B | FLAG_U);
    }

    pub(crate) fn pla(&mut self, bus: &mut dyn Addressable) {
        self.a = self.pull_byte(bus);
        self.set_nz(self.a);
    }

    pub(crate) fn plp(&mut self, bus: &mut dyn Addressable) {
        // PLP always sets bit 5 (unused) and clears bit 4 (B)
        self.p = (self.pull_byte(bus) | FLAG_U) & !FLAG_B;
    }
}
