//! Z80 instruction implementations
//!
//! Organized by category (similar to 68000):
//! - load: LD (8-bit and 16-bit)
//! - stack: PUSH, POP
//! - arithmetic: ADD, ADC, SUB, SBC, INC, DEC
//! - logical: AND, OR, XOR, CP
//! - shifts: RLC, RL, SLA, SRA, etc.
//! - bit_ops: BIT, SET, RES
//! - jumps: JP, JR, CALL, RET, DJNZ
//! - io: IN, OUT
//! - misc: NOP, HALT, DI, EI, EXX, etc.
//! - extended: the ED-prefixed block transfers, block I/O, 16-bit loads,
//!   IM, RETI/RETN and LD I/R
//! - index: IX and IY (the DD and FD prefixes)

pub mod arithmetic;
pub mod bit_ops;
pub mod extended;
pub mod index;
pub mod io;
pub mod jumps;
pub mod load;
pub mod logical;
pub mod misc;
pub mod shifts;
pub mod stack;

// Re-export addressing for convenience
pub use crate::z80::addressing::{Reg16, Reg8};
pub use crate::z80::{InstructionResult, Z80};
