//! 6502 interrupt handling: NMI, IRQ, RESET, BRK.

use crate::instructions::{FLAG_B, FLAG_I, FLAG_U};
use crate::Mos6502;
use retro_core::cpu::Addressable;

pub const NMI_VECTOR: u16 = 0xFFFA;
pub const RESET_VECTOR: u16 = 0xFFFC;
pub const IRQ_VECTOR: u16 = 0xFFFE;

impl Mos6502 {
    /// Check and service pending interrupts.
    /// Returns Some(cycles) if an interrupt was taken, None otherwise.
    pub(crate) fn poll_interrupts(&mut self, bus: &mut dyn Addressable) -> Option<u32> {
        // NMI has highest priority and is edge-triggered
        if self.nmi_pending {
            self.nmi_pending = false;
            self.nmi_edge_detected = false;
            self.service_interrupt(bus, NMI_VECTOR, false);
            return Some(7);
        }

        // IRQ is level-triggered, masked by I flag
        if self.irq_pending && !self.get_flag(FLAG_I) {
            self.service_interrupt(bus, IRQ_VECTOR, false);
            return Some(7);
        }

        None
    }

    /// Common interrupt sequence: push PC and P, then jump to vector.
    /// `is_brk` controls whether the B flag is set in the pushed status byte.
    pub(crate) fn service_interrupt(
        &mut self,
        bus: &mut dyn Addressable,
        vector: u16,
        is_brk: bool,
    ) {
        self.push_word(bus, self.pc);

        let mut status = self.p | FLAG_U;
        if is_brk {
            status |= FLAG_B;
        } else {
            status &= !FLAG_B;
        }
        self.push_byte(bus, status);

        self.set_flag(FLAG_I, true);

        let lo = bus.read(vector as u32) as u16;
        let hi = bus.read(vector.wrapping_add(1) as u32) as u16;
        self.pc = (hi << 8) | lo;
    }
}
