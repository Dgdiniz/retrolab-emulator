//! Sony SPC700 CPU emulation (SNES audio CPU).
//!
//! 8-bit CPU with a 16-bit address space. Similar in spirit to a 6502
//! with extra addressing modes and a 16-bit `YA` register pair used
//! by multiply / divide. Drives the SNES S-DSP at a 1.024 MHz clock
//! (24 MHz oscillator / 24, divided again by 2 for each CPU cycle
//! depending on the instruction).
//!
//! This crate exposes the register file plus helper accessors, and a
//! handler for each of the 256 opcodes. An opcode no handler expects
//! `panic!`s, so SingleStepTests surface it as `panicked` rather than
//! silently passing.

pub mod addressing;
pub mod decode;
pub mod disasm;
pub mod instructions;
pub mod interrupts;

/// SingleStepTests runner, shared by the crate's integration tests and
/// `snes1-cli singlestep-spc700`. Gated behind the `singlestep` feature so the
/// default and WASM builds never compile in `serde`/`serde_json`.
#[cfg(feature = "singlestep")]
pub mod singlestep;

use instructions::{FLAG_N, FLAG_P, FLAG_Z};
use retro_core::cpu::{Addressable, Clockable, Resettable};

/// Sony SPC700 CPU.
///
/// Register layout matches the hardware: 8-bit `A`/`X`/`Y` (plus the
/// implicit 16-bit `YA` pair used by `MUL`/`DIV`), 8-bit stack
/// pointer (always in page 1, `0x100..=0x1FF`), 16-bit program
/// counter, and an 8-bit Program Status Word (`NVPBHIZC`).
pub struct Spc700 {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    /// Program Status Word: `N V P B H I Z C` (bit 7 -> bit 0).
    pub psw: u8,
    pub cycles_executed: u64,
}

impl Default for Spc700 {
    fn default() -> Self {
        Self::new()
    }
}

impl Spc700 {
    /// Build a fresh SPC700 in its post-reset state. The reset vector
    /// is left at `0xFFC0` (the canonical entry point for the SNES
    /// IPL ROM); callers that need to honour a bus-driven reset
    /// vector should invoke [`Self::reset_with_bus`] afterwards.
    pub fn new() -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xEF,
            pc: 0xFFC0,
            psw: 0x02,
            cycles_executed: 0,
        }
    }

    /// Hardware reset: fetches the 16-bit reset vector from
    /// `$FFFE`/`$FFFF` (little-endian) and re-initialises the
    /// architectural registers.
    pub fn reset_with_bus(&mut self, bus: &mut dyn Addressable) {
        self.a = 0;
        self.x = 0;
        self.y = 0;
        self.sp = 0xEF;
        self.psw = 0x02;
        self.cycles_executed = 0;

        let lo = bus.read(interrupts::RESET_VECTOR as u32) as u16;
        let hi = bus.read((interrupts::RESET_VECTOR + 1) as u32) as u16;
        self.pc = (hi << 8) | lo;
    }

    // -- Register pair helpers ------------------------------------------------

    /// `YA` 16-bit pseudo-register: `Y` is the high byte, `A` is the low byte.
    /// Used by `MUL YA`, `DIV YA,X`, `MOVW YA,$dp`, `MOVW $dp,YA`,
    /// `ADDW YA,$dp`, `SUBW YA,$dp`, `CMPW YA,$dp`.
    pub fn ya(&self) -> u16 {
        ((self.y as u16) << 8) | (self.a as u16)
    }

    /// Write the `YA` pair: `Y` gets the high byte, `A` gets the low byte.
    pub fn set_ya(&mut self, v: u16) {
        self.y = (v >> 8) as u8;
        self.a = (v & 0xFF) as u8;
    }

    /// Direct-page base address selected by `PSW.P`: `$0000` when `P=0`,
    /// `$0100` when `P=1`. Every direct-page addressing mode adds the
    /// 8-bit DP offset to this base.
    pub fn dp_base(&self) -> u16 {
        if self.psw & FLAG_P != 0 {
            0x0100
        } else {
            0x0000
        }
    }

    /// Effective stack address for the current `SP`: always lives in page 1
    /// (`$0100 | sp`), regardless of `PSW.P`.
    pub fn stack_addr(&self) -> u16 {
        0x0100 | self.sp as u16
    }

    // -- Bus stack helpers ----------------------------------------------------

    /// Write `v` to `$0100 | SP`, then decrement `SP` (post-decrement, 6502
    /// style). Wraps around within page 1.
    pub fn push_byte(&mut self, bus: &mut dyn Addressable, v: u8) {
        bus.write(self.stack_addr() as u32, v);
        self.sp = self.sp.wrapping_sub(1);
    }

    /// Increment `SP` first (pre-increment, 6502 style) and return the byte
    /// at the new `$0100 | SP`.
    pub fn pull_byte(&mut self, bus: &mut dyn Addressable) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        bus.read(self.stack_addr() as u32)
    }

    // -- Flag helpers ---------------------------------------------------------

    /// Read a single `PSW` flag.
    pub fn get_flag(&self, flag: u8) -> bool {
        (self.psw & flag) != 0
    }

    /// Set or clear a single `PSW` flag without touching the others.
    pub fn set_flag(&mut self, flag: u8, on: bool) {
        if on {
            self.psw |= flag;
        } else {
            self.psw &= !flag;
        }
    }

    /// Update the `N` and `Z` flags from an 8-bit value (typical ALU result).
    pub fn update_nz_u8(&mut self, v: u8) {
        self.set_flag(FLAG_Z, v == 0);
        self.set_flag(FLAG_N, (v & 0x80) != 0);
    }

    /// Update the `N` and `Z` flags from a 16-bit value (used by `MOVW`,
    /// `ADDW`, `SUBW`, `INCW`, `DECW`, `CMPW`).
    pub fn update_nz_u16(&mut self, v: u16) {
        self.set_flag(FLAG_Z, v == 0);
        self.set_flag(FLAG_N, (v & 0x8000) != 0);
    }

    // -- Execution ------------------------------------------------------------

    /// Execute a single instruction and return the number of master
    /// cycles consumed. Dispatches through [`decode::OPCODE_TABLE`]
    /// into [`instructions::dispatch`]; an opcode no handler expects
    /// `panic!`s, so that the SingleStepTests runner reports it as
    /// `panicked` rather than silently miscounting.
    pub fn step(&mut self, bus: &mut dyn Addressable) -> u32 {
        let opcode = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);

        let info = &decode::OPCODE_TABLE[opcode as usize];
        let cycles = instructions::dispatch(self, bus, opcode, info);

        self.cycles_executed = self.cycles_executed.saturating_add(cycles as u64);
        cycles
    }
}

impl Clockable for Spc700 {
    /// Sentinel implementation that mirrors `Wdc65816`/`Mos6502`: real
    /// execution happens through [`Self::step`] driven by the host
    /// scheduler, so this returns `0` consumed cycles for callers that
    /// only need the trait shape.
    fn clock(&mut self, _cycles: u32) -> u32 {
        0
    }
}

impl Resettable for Spc700 {
    /// Architectural reset without a bus: `PC` is left at `$FFC0` (the
    /// canonical IPL entry point). Callers that need to honour a
    /// bus-driven vector must use [`Self::reset_with_bus`] instead.
    fn reset(&mut self) {
        self.a = 0;
        self.x = 0;
        self.y = 0;
        self.sp = 0xEF;
        self.psw = 0x02;
        self.cycles_executed = 0;
        self.pc = 0xFFC0;
    }
}

#[cfg(test)]
mod tests;
