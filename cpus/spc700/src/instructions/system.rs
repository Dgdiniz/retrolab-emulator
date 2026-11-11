//! System-level opcodes: `NOP`, `SLEEP`, `STOP`, `BRK`.
//!
//! `SLEEP`/`STOP` are halts that the SingleStepTests still accept as
//! a single "step", the PC stays put while we burn the baseline
//! cycle count. `BRK` is a software interrupt that pushes PC/PSW and
//! jumps to the `$FFDE`/`$FFDF` vector with `I` cleared and `B` set.

use retro_core::cpu::Addressable;

use super::{FLAG_B, FLAG_I};
use crate::decode::OpcodeInfo;
use crate::interrupts::BRK_VECTOR;
use crate::Spc700;

pub fn nop(_cpu: &mut Spc700, _bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    info.base_cycles as u32
}

pub fn sleep(_cpu: &mut Spc700, _bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    // SLEEP halts execution until an interrupt; for SingleStepTests
    // (which never raises one) we just return the baseline.
    info.base_cycles as u32
}

pub fn stop(_cpu: &mut Spc700, _bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    info.base_cycles as u32
}

pub fn brk(cpu: &mut Spc700, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    let pc_hi = (cpu.pc >> 8) as u8;
    let pc_lo = (cpu.pc & 0xFF) as u8;
    cpu.push_byte(bus, pc_hi);
    cpu.push_byte(bus, pc_lo);
    cpu.push_byte(bus, cpu.psw);

    let lo = bus.read(BRK_VECTOR as u32) as u16;
    let hi = bus.read((BRK_VECTOR + 1) as u32) as u16;
    cpu.pc = (hi << 8) | lo;

    cpu.set_flag(FLAG_B, true);
    cpu.set_flag(FLAG_I, false);

    info.base_cycles as u32
}
