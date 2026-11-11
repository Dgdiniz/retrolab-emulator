//! Stack instructions: PHA, PHP, PLA, PLP, PHX, PHY, PLX, PLY,
//! PHB, PLB, PHD, PLD, PHK, PEA, PEI, PER.

use crate::instructions::{FLAG_M, FLAG_X};
use crate::Wdc65816;
use retro_core::cpu::Addressable;

impl Wdc65816 {
    pub(crate) fn pha(&mut self, bus: &mut dyn Addressable) {
        if self.a_is_8bit() {
            self.push_byte(bus, self.a_lo());
        } else {
            self.push_word(bus, self.c);
        }
    }

    pub(crate) fn pla(&mut self, bus: &mut dyn Addressable) {
        if self.a_is_8bit() {
            let val = self.pull_byte(bus);
            self.set_a_lo(val);
            self.set_nz_8(val);
        } else {
            let val = self.pull_word(bus);
            self.c = val;
            self.set_nz_16(val);
        }
    }

    pub(crate) fn php(&mut self, bus: &mut dyn Addressable) {
        self.push_byte(bus, self.p);
    }

    pub(crate) fn plp(&mut self, bus: &mut dyn Addressable) {
        let val = self.pull_byte(bus);
        let old_x_flag = self.p & FLAG_X;
        self.p = val;

        if self.emulation {
            self.p |= FLAG_M | FLAG_X;
        }

        // If X flag transitioned to 1 (8-bit), zero high bytes
        if (self.p & FLAG_X) != 0 && old_x_flag == 0 {
            self.on_index_to_8bit();
        }
    }

    pub(crate) fn phx(&mut self, bus: &mut dyn Addressable) {
        if self.xy_is_8bit() {
            self.push_byte(bus, self.x as u8);
        } else {
            self.push_word(bus, self.x);
        }
    }

    pub(crate) fn phy(&mut self, bus: &mut dyn Addressable) {
        if self.xy_is_8bit() {
            self.push_byte(bus, self.y as u8);
        } else {
            self.push_word(bus, self.y);
        }
    }

    pub(crate) fn plx(&mut self, bus: &mut dyn Addressable) {
        if self.xy_is_8bit() {
            let val = self.pull_byte(bus);
            self.x = val as u16;
            self.set_nz_8(val);
        } else {
            let val = self.pull_word(bus);
            self.x = val;
            self.set_nz_16(val);
        }
    }

    pub(crate) fn ply(&mut self, bus: &mut dyn Addressable) {
        if self.xy_is_8bit() {
            let val = self.pull_byte(bus);
            self.y = val as u16;
            self.set_nz_8(val);
        } else {
            let val = self.pull_word(bus);
            self.y = val;
            self.set_nz_16(val);
        }
    }

    /// PHB: Push Data Bank Register.
    pub(crate) fn phb(&mut self, bus: &mut dyn Addressable) {
        self.push_byte(bus, self.dbr);
    }

    /// PLB: Pull Data Bank Register.
    ///
    /// PLB is a native-65816 instruction, so the pull reads at the
    /// next physical byte (`raw` SP++) and `finalize_emu_sp` clamps
    /// back into `$01xx` afterwards. Using the 6502-style clamped
    /// `pull_byte` would wrap `$01FF + 1` to `$0100`, but Tom Harte's
    /// reference (and the silicon) expect the read to come from
    /// `$0200` instead.
    pub(crate) fn plb(&mut self, bus: &mut dyn Addressable) {
        let val = self.pull_byte_raw(bus);
        self.finalize_emu_sp();
        self.dbr = val;
        self.set_nz_8(val);
    }

    /// PHD: Push Direct Page register (always 16-bit).
    pub(crate) fn phd(&mut self, bus: &mut dyn Addressable) {
        self.push_word(bus, self.d);
    }

    /// PLD: Pull Direct Page register (always 16-bit).
    pub(crate) fn pld(&mut self, bus: &mut dyn Addressable) {
        let val = self.pull_word(bus);
        self.d = val;
        self.set_nz_16(val);
    }

    /// PHK: Push Program Bank Register.
    pub(crate) fn phk(&mut self, bus: &mut dyn Addressable) {
        self.push_byte(bus, self.pbr);
    }

    /// PEA: Push Effective Absolute address (always 16-bit, despite the name).
    pub(crate) fn pea(&mut self, bus: &mut dyn Addressable) {
        let val = self.fetch_word(bus);
        self.push_word(bus, val);
    }

    /// PEI: Push Effective Indirect address. Reads a 16-bit pointer from DP.
    ///
    /// Uses `dp_offset` so the operand wraps inside the DP page in
    /// emulation+aligned mode, and `read_word_dp` so the high byte of
    /// the pointer wraps inside bank 0.
    pub(crate) fn pei(&mut self, bus: &mut dyn Addressable) {
        let operand = self.fetch_byte(bus);
        let ptr_addr = self.dp_offset(operand, 0) as u32;
        let val = Self::read_word_dp(bus, ptr_addr);
        self.push_word(bus, val);
    }

    /// PER: Push Effective Relative address. PC + 16-bit signed offset.
    pub(crate) fn per(&mut self, bus: &mut dyn Addressable) {
        let offset = self.fetch_word(bus) as i16;
        let target = self.pc.wrapping_add(offset as u16);
        self.push_word(bus, target);
    }
}
