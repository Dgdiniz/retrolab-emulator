//! Branch instructions: BCC, BCS, BEQ, BMI, BNE, BPL, BVC, BVS.

use crate::instructions::*;
use crate::Mos6502;
use retro_core::cpu::Addressable;

impl Mos6502 {
    /// Generic conditional branch. Returns extra cycles:
    /// +1 if branch taken, +2 if taken and crosses a page boundary.
    fn branch_if(&mut self, bus: &mut dyn Addressable, condition: bool) -> u32 {
        let offset = self.fetch_byte(bus) as i8;
        if condition {
            let old_pc = self.pc;
            self.pc = self.pc.wrapping_add(offset as u16);
            if (old_pc & 0xFF00) != (self.pc & 0xFF00) {
                2 // taken + page crossed
            } else {
                1 // taken, same page
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
}
