//! Flow of control, the prefixes and the register moves.
//!
//! A jump lands one instruction late: the pipeline already holds the byte
//! after the jump, so that instruction runs first (the delay slot). The
//! prefixes (ALT1-3, WITH) and TO/FROM leave the prefixes in place for the
//! opcode after them, and so does a branch.

use retro_core::cpu::Addressable;

use crate::registers::{config, status};
use crate::Gsu;

impl Gsu {
    /// `$00` STOP: the GSU halts and, unless CFGR masks it, raises the
    /// interrupt for the S-CPU; the pipeline is left holding a NOP for the
    /// next start.
    pub(crate) fn stop(&mut self) {
        if self.config & config::IRQ_MASKED == 0 {
            self.set_flag(status::IRQ, true);
        }
        self.set_flag(status::GO, false);
        self.next_opcode = 0x01;
        self.clear_prefixes();
    }

    /// `$02` CACHE: the instruction cache moves to the 16-byte line that
    /// holds R15, and is emptied if that moves it.
    pub(crate) fn start_cache_here(&mut self) {
        let line = self.registers[15] & 0xfff0;
        if self.cache_base != line {
            self.cache_base = line;
            self.flush_cache();
        }
        self.clear_prefixes();
    }

    /// `$05-$0F` the branches: the displacement byte is always read; a
    /// taken branch adds it to R15, which then points past it.
    pub(crate) fn branch(&mut self, bus: &mut dyn Addressable, taken: bool) {
        let displacement = self.take_operand(bus) as i8;
        if taken {
            let target = self.registers[15].wrapping_add(displacement as u16);
            self.write_register(15, target);
        }
    }

    /// `$3C` LOOP: count R12 down and, unless it reached zero, jump to R13.
    pub(crate) fn count_down_and_loop(&mut self) {
        let count = self.registers[12].wrapping_sub(1);
        self.write_register(12, count);
        self.set_sign_and_zero(count);
        if count != 0 {
            let target = self.registers[13];
            self.write_register(15, target);
        }
        self.clear_prefixes();
    }

    /// `$98-$9D` JMP: jump to the register the nibble names. Under ALT1,
    /// LJMP: that register gives the program bank and the source register
    /// the address, and the cache starts again there.
    pub(crate) fn jump(&mut self, n: usize) {
        if self.alt1() {
            self.program_bank = (self.registers[n] & 0x7f) as u8;
            let target = self.source_value();
            self.write_register(15, target);
            self.cache_base = target & 0xfff0;
            self.flush_cache();
        } else {
            let target = self.registers[n];
            self.write_register(15, target);
        }
        self.clear_prefixes();
    }

    /// `$91-$94` LINK: R11 gets the address `n` bytes past R15, to return
    /// to after a jump.
    pub(crate) fn link(&mut self, n: usize) {
        let address = self.registers[15].wrapping_add(n as u16);
        self.write_register(11, address);
        self.clear_prefixes();
    }

    /// `$3D-$3F` ALT1, ALT2 and ALT3 (both): the prefixes for the next
    /// opcode. They end a WITH.
    pub(crate) fn set_prefix(&mut self, alt1: bool, alt2: bool) {
        self.set_flag(status::WITH, false);
        if alt1 {
            self.set_flag(status::ALT1, true);
        }
        if alt2 {
            self.set_flag(status::ALT2, true);
        }
    }

    /// `$20-$2F` WITH: the register the nibble names becomes both source
    /// and destination, and a TO or FROM that follows becomes a move.
    pub(crate) fn with(&mut self, n: usize) {
        self.source = n as u8;
        self.destination = n as u8;
        self.set_flag(status::WITH, true);
    }

    /// `$10-$1F` TO: the destination for the next instruction. After WITH,
    /// MOVE: the source is copied there at once.
    pub(crate) fn destination_or_move(&mut self, n: usize) {
        if self.flag(status::WITH) {
            let value = self.source_value();
            self.write_register(n, value);
            self.clear_prefixes();
        } else {
            self.destination = n as u8;
        }
    }

    /// `$B0-$BF` FROM: the source for the next instruction. After WITH,
    /// MOVES: the register is copied to the destination and sets the
    /// flags, OV from its bit 7.
    pub(crate) fn source_or_move_with_flags(&mut self, n: usize) {
        if self.flag(status::WITH) {
            let value = self.registers[n];
            self.write_destination(value);
            self.set_flag(status::OVERFLOW, value & 0x80 != 0);
            self.set_sign_and_zero(value);
            self.clear_prefixes();
        } else {
            self.source = n as u8;
        }
    }
}
