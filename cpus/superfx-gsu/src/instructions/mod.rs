//! The instruction set: [`Gsu::execute`] names an opcode's family from the
//! opcode alone, and each family's handler reads the ALT prefixes itself.
//!
//! An instruction ends by clearing the prefixes and going back to R0 as
//! source and destination, which is what confines ALT1, ALT2 and WITH to
//! the opcode after them; the prefixes, TO/FROM/WITH and the branches are
//! the instructions that do not.
//!
//! The handlers follow ares (`component/processor/gsu/instructions.cpp`),
//! whose GSU is the same code as bsnes-plus's; ISC, the notice is at the
//! end of `LICENSE`.

mod alu;
mod control;
mod memory;
mod multiply;
mod plot;

use retro_core::cpu::Addressable;

use crate::registers::status;
use crate::Gsu;

impl Gsu {
    /// Run the instruction `opcode`, whose operand bytes the pipeline
    /// supplies.
    pub(crate) fn execute(&mut self, bus: &mut dyn Addressable, opcode: u8) {
        let n = usize::from(opcode & 0x0f);
        match opcode {
            0x00 => self.stop(),
            0x01 => self.clear_prefixes(),
            0x02 => self.start_cache_here(),
            0x03 => self.shift_right(),
            0x04 => self.rotate_left(),
            0x05 => self.branch(bus, true),
            0x06 => self.branch(bus, self.flag(status::SIGN) == self.flag(status::OVERFLOW)),
            0x07 => self.branch(bus, self.flag(status::SIGN) != self.flag(status::OVERFLOW)),
            0x08 => self.branch(bus, !self.flag(status::ZERO)),
            0x09 => self.branch(bus, self.flag(status::ZERO)),
            0x0a => self.branch(bus, !self.flag(status::SIGN)),
            0x0b => self.branch(bus, self.flag(status::SIGN)),
            0x0c => self.branch(bus, !self.flag(status::CARRY)),
            0x0d => self.branch(bus, self.flag(status::CARRY)),
            0x0e => self.branch(bus, !self.flag(status::OVERFLOW)),
            0x0f => self.branch(bus, self.flag(status::OVERFLOW)),
            0x10..=0x1f => self.destination_or_move(n),
            0x20..=0x2f => self.with(n),
            0x30..=0x3b => self.store(bus, n),
            0x3c => self.count_down_and_loop(),
            0x3d => self.set_prefix(true, false),
            0x3e => self.set_prefix(false, true),
            0x3f => self.set_prefix(true, true),
            0x40..=0x4b => self.load(bus, n),
            0x4c => self.plot_or_read_pixel(bus),
            0x4d => self.swap_bytes(),
            0x4e => self.color_or_options(),
            0x4f => self.not(),
            0x50..=0x5f => self.add(n),
            0x60..=0x6f => self.subtract(n),
            0x70 => self.merge(),
            0x71..=0x7f => self.and(n),
            0x80..=0x8f => self.multiply(bus, n),
            0x90 => self.store_back(bus),
            0x91..=0x94 => self.link(n),
            0x95 => self.sign_extend(),
            0x96 => self.shift_right_signed(),
            0x97 => self.rotate_right(),
            0x98..=0x9d => self.jump(n),
            0x9e => self.low_byte(),
            0x9f => self.multiply_words(bus),
            0xa0..=0xaf => self.byte_immediate_or_short_address(bus, n),
            0xb0..=0xbf => self.source_or_move_with_flags(n),
            0xc0 => self.high_byte(),
            0xc1..=0xcf => self.or(n),
            0xd0..=0xde => self.increment(n),
            0xdf => self.color_or_bank(bus),
            0xe0..=0xee => self.decrement(n),
            0xef => self.get_byte(bus),
            0xf0..=0xff => self.word_immediate_or_address(bus, n),
        }
    }
}
