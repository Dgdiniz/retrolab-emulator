//! Block move instructions: MVN (Move Next) and MVP (Move Previous).
//!
//! On the 65C816 these opcodes move one byte per machine instruction
//! and re-fetch the opcode until the counter (`C`) wraps to `$FFFF`.
//! Concretely:
//!
//! - PC advances past `opcode + dst_bank + src_bank` while the operands
//!   are fetched.
//! - One byte is transferred from `[src_bank:X]` to `[dst_bank:Y]`.
//! - X / Y are post-incremented (MVN) or post-decremented (MVP), C is
//!   decremented unconditionally.
//! - If the count did NOT wrap, PC is rewound by 3 so the next CPU step
//!   refetches MVN/MVP and continues. When `C == 0xFFFF` post-decrement,
//!   PC stays at `instruction + 3` and execution moves on.
//!
//! This matches the silicon's "interruptible block move", important so
//! the H-event scheduler can preempt long block transfers without
//! desynchronising the PPU / APU.
//!
//! With `X = 1` (8-bit index registers) the increments/decrements wrap
//! inside the low byte of X/Y; the high byte stays zero.

use crate::Wdc65816;
use retro_core::cpu::Addressable;

impl Wdc65816 {
    /// MVN: Move block Next (ascending). One byte per call; PC re-fetches
    /// the opcode until `C` wraps from `0x0000` to `0xFFFF`.
    pub(crate) fn mvn(&mut self, bus: &mut dyn Addressable) -> u32 {
        let dst_bank = self.fetch_byte(bus);
        let src_bank = self.fetch_byte(bus);
        self.dbr = dst_bank;

        let src_addr = (src_bank as u32) << 16 | self.x as u32;
        let dst_addr = (dst_bank as u32) << 16 | self.y as u32;
        let val = bus.read(src_addr);
        bus.write(dst_addr, val);

        if self.xy_is_8bit() {
            self.x = (self.x & 0xFF00) | (self.x.wrapping_add(1) & 0x00FF);
            self.y = (self.y & 0xFF00) | (self.y.wrapping_add(1) & 0x00FF);
        } else {
            self.x = self.x.wrapping_add(1);
            self.y = self.y.wrapping_add(1);
        }
        self.c = self.c.wrapping_sub(1);

        if self.c != 0xFFFF {
            self.pc = self.pc.wrapping_sub(3);
        }
        0
    }

    /// MVP: Move block Previous (descending). Same as `mvn` but X / Y are
    /// post-decremented instead of incremented.
    pub(crate) fn mvp(&mut self, bus: &mut dyn Addressable) -> u32 {
        let dst_bank = self.fetch_byte(bus);
        let src_bank = self.fetch_byte(bus);
        self.dbr = dst_bank;

        let src_addr = (src_bank as u32) << 16 | self.x as u32;
        let dst_addr = (dst_bank as u32) << 16 | self.y as u32;
        let val = bus.read(src_addr);
        bus.write(dst_addr, val);

        if self.xy_is_8bit() {
            self.x = (self.x & 0xFF00) | (self.x.wrapping_sub(1) & 0x00FF);
            self.y = (self.y & 0xFF00) | (self.y.wrapping_sub(1) & 0x00FF);
        } else {
            self.x = self.x.wrapping_sub(1);
            self.y = self.y.wrapping_sub(1);
        }
        self.c = self.c.wrapping_sub(1);

        if self.c != 0xFFFF {
            self.pc = self.pc.wrapping_sub(3);
        }
        0
    }
}
