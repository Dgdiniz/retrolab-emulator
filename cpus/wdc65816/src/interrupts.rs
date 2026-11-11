//! 65C816 interrupt handling: NMI, IRQ, BRK, COP, ABORT, RESET.
//!
//! Vector addresses differ between emulation and native mode.

use crate::instructions::{FLAG_B, FLAG_D, FLAG_I};
use crate::Wdc65816;
use retro_core::cpu::Addressable;

// Emulation mode vectors (bank $00)
pub const EMU_COP_VECTOR: u16 = 0xFFF4;
pub const EMU_ABORT_VECTOR: u16 = 0xFFF8;
pub const EMU_NMI_VECTOR: u16 = 0xFFFA;
pub const EMU_RESET_VECTOR: u16 = 0xFFFC;
pub const EMU_IRQ_VECTOR: u16 = 0xFFFE; // also BRK in emulation mode

// Native mode vectors (bank $00)
pub const NAT_COP_VECTOR: u16 = 0xFFE4;
pub const NAT_BRK_VECTOR: u16 = 0xFFE6;
pub const NAT_ABORT_VECTOR: u16 = 0xFFE8;
pub const NAT_NMI_VECTOR: u16 = 0xFFEA;
pub const NAT_IRQ_VECTOR: u16 = 0xFFEE;

impl Wdc65816 {
    pub(crate) fn poll_interrupts(&mut self, bus: &mut dyn Addressable) -> Option<u32> {
        if self.nmi_pending {
            self.nmi_pending = false;
            self.nmi_edge_detected = false;
            let vector = if self.emulation {
                EMU_NMI_VECTOR
            } else {
                NAT_NMI_VECTOR
            };
            self.service_interrupt(bus, vector, false);
            return Some(if self.emulation { 7 } else { 8 });
        }

        if self.irq_pending && !self.get_flag(FLAG_I) {
            let vector = if self.emulation {
                EMU_IRQ_VECTOR
            } else {
                NAT_IRQ_VECTOR
            };
            self.service_interrupt(bus, vector, false);
            return Some(if self.emulation { 7 } else { 8 });
        }

        None
    }

    /// Service an interrupt. In native mode pushes PBR, PC, P (4 bytes).
    /// In emulation mode pushes PC, P (3 bytes, like 6502).
    ///
    /// BRK / COP / IRQ / NMI are 6502-compatible operations: each byte
    /// is pushed with `push_byte`, which re-applies the `$01xx` clamp
    /// in emulation mode, so successive writes wrap inside page 1 of
    /// bank 0 (the silicon "stack stays in page 1" behaviour).
    pub(crate) fn service_interrupt(
        &mut self,
        bus: &mut dyn Addressable,
        vector: u16,
        is_software: bool,
    ) {
        let pc = self.pc;
        if self.emulation {
            self.push_byte(bus, (pc >> 8) as u8);
            self.push_byte(bus, pc as u8);
            let mut status = self.p;
            if is_software {
                status |= FLAG_B;
            } else {
                status &= !FLAG_B;
            }
            self.push_byte(bus, status);
        } else {
            self.push_byte(bus, self.pbr);
            self.push_byte(bus, (pc >> 8) as u8);
            self.push_byte(bus, pc as u8);
            self.push_byte(bus, self.p);
        }

        self.set_flag(FLAG_I, true);
        self.set_flag(FLAG_D, false);
        self.pbr = 0;

        let lo = bus.read(vector as u32) as u16;
        let hi = bus.read(vector.wrapping_add(1) as u32) as u16;
        self.pc = (hi << 8) | lo;
    }
}
