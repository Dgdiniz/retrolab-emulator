//! Branch instructions: BCC, BCS, BEQ, BMI, BNE, BPL, BVC, BVS, BRA, BRL.

use crate::instructions::{FLAG_C, FLAG_N, FLAG_V, FLAG_Z};
use crate::Wdc65816;
use retro_core::cpu::Addressable;

impl Wdc65816 {
    /// Generic 8-bit relative branch. Returns extra cycles.
    fn branch_if(&mut self, bus: &mut dyn Addressable, condition: bool) -> u32 {
        let offset = self.fetch_byte(bus) as i8;
        if condition {
            let old_pc = self.pc;
            self.pc = self.pc.wrapping_add(offset as u16);
            // +1 for taken branch, +1 more if page crossed in emulation mode
            if self.emulation && (old_pc & 0xFF00) != (self.pc & 0xFF00) {
                2
            } else {
                1
            }
        } else {
            0
        }
    }

    pub(crate) fn bcc(&mut self, bus: &mut dyn Addressable) -> u32 {
        let cond = !self.get_flag(FLAG_C);
        self.branch_if(bus, cond)
    }

    pub(crate) fn bcs(&mut self, bus: &mut dyn Addressable) -> u32 {
        let cond = self.get_flag(FLAG_C);
        self.branch_if(bus, cond)
    }

    pub(crate) fn beq(&mut self, bus: &mut dyn Addressable) -> u32 {
        let cond = self.get_flag(FLAG_Z);
        self.branch_if(bus, cond)
    }

    pub(crate) fn bmi(&mut self, bus: &mut dyn Addressable) -> u32 {
        let cond = self.get_flag(FLAG_N);
        self.branch_if(bus, cond)
    }

    pub(crate) fn bne(&mut self, bus: &mut dyn Addressable) -> u32 {
        let cond = !self.get_flag(FLAG_Z);
        self.branch_if(bus, cond)
    }

    pub(crate) fn bpl(&mut self, bus: &mut dyn Addressable) -> u32 {
        let cond = !self.get_flag(FLAG_N);
        self.branch_if(bus, cond)
    }

    pub(crate) fn bvc(&mut self, bus: &mut dyn Addressable) -> u32 {
        let cond = !self.get_flag(FLAG_V);
        self.branch_if(bus, cond)
    }

    pub(crate) fn bvs(&mut self, bus: &mut dyn Addressable) -> u32 {
        let cond = self.get_flag(FLAG_V);
        self.branch_if(bus, cond)
    }

    /// BRA: unconditional 8-bit relative branch.
    ///
    /// Base cycle count (3) already covers the "taken" branch; only
    /// the page-cross penalty in emulation mode is added on top.
    pub(crate) fn bra(&mut self, bus: &mut dyn Addressable) -> u32 {
        let offset = self.fetch_byte(bus) as i8;
        let old_pc = self.pc;
        self.pc = self.pc.wrapping_add(offset as u16);
        if self.emulation && (old_pc & 0xFF00) != (self.pc & 0xFF00) {
            1
        } else {
            0
        }
    }

    /// BRL: unconditional 16-bit relative branch.
    pub(crate) fn brl(&mut self, bus: &mut dyn Addressable) -> u32 {
        let offset = self.fetch_word(bus) as i16;
        self.pc = self.pc.wrapping_add(offset as u16);
        0
    }
}
