//! Flag manipulation: CLC, CLD, CLI, CLV, SEC, SED, SEI, REP, SEP.
//!
//! CLC-SEI are handled inline in the dispatch (they just flip single bits).
//! REP and SEP need side-effect handling.

use crate::instructions::{FLAG_M, FLAG_X};
use crate::Wdc65816;
use retro_core::cpu::Addressable;

impl Wdc65816 {
    /// REP: Reset Processor status bits. Clears bits specified by the immediate operand.
    pub(crate) fn rep(&mut self, bus: &mut dyn Addressable) {
        let mask = self.fetch_byte(bus);
        let old_x = self.p & FLAG_X;
        self.p &= !mask;

        if self.emulation {
            // In emulation mode, M and X bits are always forced to 1
            self.p |= FLAG_M | FLAG_X;
        }

        // No transition to 8-bit; REP only clears bits
        let _ = old_x;
    }

    /// SEP: Set Processor status bits. Sets bits specified by the immediate operand.
    pub(crate) fn sep(&mut self, bus: &mut dyn Addressable) {
        let mask = self.fetch_byte(bus);
        let old_x = self.p & FLAG_X;
        self.p |= mask;

        if self.emulation {
            self.p |= FLAG_M | FLAG_X;
        }

        // If X flag just became 1 (index registers narrowing to 8-bit), zero high bytes
        if (self.p & FLAG_X) != 0 && old_x == 0 {
            self.on_index_to_8bit();
        }
    }
}
