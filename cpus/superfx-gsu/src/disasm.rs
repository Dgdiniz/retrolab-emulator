//! The GSU disassembler.
//!
//! Reads one instruction at a bus address and renders it as the other
//! disassemblers of this tree do: a lowercase mnemonic, uppercase hex, no
//! space after a comma. The GSU reads an opcode according to the ALT
//! prefixes set by the instructions before it, so the caller passes them
//! (SFR.ALT1 and SFR.ALT2).
//!
//! It only reads the bus. A caller whose bus has read side effects should
//! hand it a copy that has none.

use crate::decode::{decode, Operand};
use retro_core::cpu::Addressable;

/// One disassembled instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisasmResult {
    /// The mnemonic, lowercase (`add`, `iwt`, `plot`, `ljmp`).
    pub mnemonic: String,
    /// The operand (`r5`, `r0,#$1234`, `$0012`); empty when there is none.
    pub operands: String,
    /// The instruction's bytes, opcode first.
    pub bytes: Vec<u8>,
    /// How many bytes the instruction takes; `bytes.len()`.
    pub bytes_consumed: u8,
}

/// Disassemble the instruction at `addr`, read with the prefixes `alt1`
/// and `alt2`.
pub fn disassemble_one(
    bus: &mut dyn Addressable,
    addr: u32,
    alt1: bool,
    alt2: bool,
) -> DisasmResult {
    let fetched = [
        bus.read(addr),
        bus.read(addr.wrapping_add(1)),
        bus.read(addr.wrapping_add(2)),
    ];
    let decoded = decode(fetched[0], alt1, alt2);
    let length = decoded.operand.length();
    DisasmResult {
        mnemonic: decoded.mnemonic.to_string(),
        operands: operand_text(decoded.operand, addr, fetched),
        bytes: fetched[..usize::from(length)].to_vec(),
        bytes_consumed: length,
    }
}

/// Disassemble one instruction into a single `mnemonic operands` line,
/// with its length, the shape of the other CPU crates' `disassemble_at`.
pub fn disassemble_at(
    bus: &mut dyn Addressable,
    addr: u32,
    alt1: bool,
    alt2: bool,
) -> (String, u8) {
    let instruction = disassemble_one(bus, addr, alt1, alt2);
    let line = if instruction.operands.is_empty() {
        instruction.mnemonic
    } else {
        format!("{} {}", instruction.mnemonic, instruction.operands)
    };
    (line, instruction.bytes_consumed)
}

/// The operand text of an instruction at `addr` whose bytes are `fetched`.
fn operand_text(operand: Operand, addr: u32, fetched: [u8; 3]) -> String {
    let [opcode, first, second] = fetched;
    let register = opcode & 0x0f;
    let word = u16::from_le_bytes([first, second]);
    match operand {
        Operand::None => String::new(),
        Operand::Displacement => {
            // Relative to the instruction after the branch.
            let target = addr.wrapping_add(2).wrapping_add(first as i8 as u32) & 0xffff;
            format!("${target:04X}")
        }
        Operand::LinkOffset => {
            let target = addr.wrapping_add(1).wrapping_add(u32::from(register)) & 0xffff;
            format!("${target:04X}")
        }
        Operand::Register => format!("r{register}"),
        Operand::Indirect => format!("(r{register})"),
        Operand::RegisterByte => format!("r{register},#${first:02X}"),
        Operand::RegisterWord => format!("r{register},#${word:04X}"),
        Operand::RegisterShortAddress => format!("r{register},${:03X}", u16::from(first) << 1),
        Operand::RegisterAddress => format!("r{register},${word:04X}"),
        Operand::Nibble => format!("#${register:02X}"),
    }
}
