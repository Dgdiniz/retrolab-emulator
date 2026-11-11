//! PSW manipulation: `CLRP`/`SETP`, `CLRC`/`SETC`/`NOTC`, `CLRV`,
//! `EI`/`DI`.
//!
//! These are all single-byte opcodes that mutate one PSW bit
//! (`CLRV` also clears the `H` half-carry flag, as the hardware
//! does). Cycles come straight from `OpcodeInfo`.

use retro_core::cpu::Addressable;

use super::{FLAG_C, FLAG_H, FLAG_I, FLAG_P, FLAG_V};
use crate::decode::OpcodeInfo;
use crate::Spc700;

pub fn clrp(cpu: &mut Spc700, _bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    cpu.set_flag(FLAG_P, false);
    info.base_cycles as u32
}

pub fn setp(cpu: &mut Spc700, _bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    cpu.set_flag(FLAG_P, true);
    info.base_cycles as u32
}

pub fn clrc(cpu: &mut Spc700, _bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    cpu.set_flag(FLAG_C, false);
    info.base_cycles as u32
}

pub fn setc(cpu: &mut Spc700, _bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    cpu.set_flag(FLAG_C, true);
    info.base_cycles as u32
}

pub fn notc(cpu: &mut Spc700, _bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    cpu.psw ^= FLAG_C;
    info.base_cycles as u32
}

pub fn ei(cpu: &mut Spc700, _bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    cpu.set_flag(FLAG_I, true);
    info.base_cycles as u32
}

pub fn di(cpu: &mut Spc700, _bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    cpu.set_flag(FLAG_I, false);
    info.base_cycles as u32
}

pub fn clrv(cpu: &mut Spc700, _bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    // `CLRV` clears both V and H (Anomie's SPC700 notes; checked
    // against bsnes-plus).
    cpu.set_flag(FLAG_V, false);
    cpu.set_flag(FLAG_H, false);
    info.base_cycles as u32
}
