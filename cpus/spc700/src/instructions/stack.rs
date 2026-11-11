//! Stack operations: `PUSH A/X/Y/PSW`, `POP A/X/Y/PSW`.
//!
//! Opcode encoding (target register identified by the opcode byte):
//!
//! | Opcode | Mnemonic |
//! |--------|----------|
//! | `$2D`  | PUSH A   |
//! | `$4D`  | PUSH X   |
//! | `$6D`  | PUSH Y   |
//! | `$0D`  | PUSH PSW |
//! | `$AE`  | POP A    |
//! | `$CE`  | POP X    |
//! | `$EE`  | POP Y    |
//! | `$8E`  | POP PSW  |
//!
//! POP does NOT update N/Z flags from the popped register (the
//! SingleStepTests show it; checked against snes9x).

use retro_core::cpu::Addressable;

use crate::decode::OpcodeInfo;
use crate::Spc700;

pub fn push(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    let val = match opcode {
        0x2D => cpu.a,
        0x4D => cpu.x,
        0x6D => cpu.y,
        0x0D => cpu.psw,
        _ => panic!("spc700: PUSH dispatch on unexpected opcode {:02X}", opcode),
    };
    cpu.push_byte(bus, val);
    info.base_cycles as u32
}

pub fn pop(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    let v = cpu.pull_byte(bus);
    match opcode {
        0xAE => cpu.a = v,
        0xCE => cpu.x = v,
        0xEE => cpu.y = v,
        0x8E => cpu.psw = v,
        _ => panic!("spc700: POP dispatch on unexpected opcode {:02X}", opcode),
    }
    info.base_cycles as u32
}
