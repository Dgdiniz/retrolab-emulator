//! 65C816 disassembler.
//!
//! State-aware: instruction size depends on M/X flag states because
//! immediate operands can be 1 or 2 bytes.
//!
//! The output follows the format bsnes-plus's debugger writes, so a
//! trace produced here can be diffed against a bsnes-plus trace with
//! minimal noise. Conventions:
//! - Lowercase mnemonics and lowercase index/stack suffixes (`,x` / `,y`
//!   / `,s`); see [`crate::decode::Mnemonic::name`].
//! - Uppercase hex digits (`$1234`, `$7E2000`), as that debugger's UI
//!   shows them; its trace log is lowercase, which the trace formatter
//!   gets by lowercasing the line.
//! - Block-move operands are printed in `src, dst` order with a space
//!   after the comma (`mvn $7F, $7E`); the operand bytes themselves are
//!   stored in memory as `dst_bank, src_bank` after the opcode.

use crate::addressing::AddressingMode;
use crate::decode::OPCODE_TABLE;
use retro_core::cpu::Addressable;

/// CPU state needed by the disassembler to determine operand sizes.
#[derive(Debug, Clone, Copy)]
pub struct DisasmState {
    pub emulation: bool,
    pub flag_m: bool,
    pub flag_x: bool,
}

impl DisasmState {
    fn m8(&self) -> bool {
        self.emulation || self.flag_m
    }
    fn x8(&self) -> bool {
        self.emulation || self.flag_x
    }
}

#[derive(Debug, Clone)]
pub struct DisasmResult {
    pub mnemonic: String,
    pub operands: String,
    pub comment: Option<String>,
    pub bytes: Vec<u8>,
    pub bytes_consumed: u8,
}

/// Disassemble a single instruction at `addr` (24-bit).
/// Returns a formatted string and total byte size.
pub fn disassemble_at(bus: &mut dyn Addressable, addr: u32, state: &DisasmState) -> (String, u8) {
    let result = disassemble_one(bus, addr, state);
    let text = if result.operands.is_empty() {
        result.mnemonic.clone()
    } else {
        format!("{} {}", result.mnemonic, result.operands)
    };
    (text, result.bytes_consumed)
}

/// Disassemble a single instruction at `addr` with full detail.
pub fn disassemble_one(bus: &mut dyn Addressable, addr: u32, state: &DisasmState) -> DisasmResult {
    let opcode = bus.read(addr);
    let info = &OPCODE_TABLE[opcode as usize];

    let op_size = info.mode.operand_size(state.m8(), state.x8());
    let size = 1 + op_size;
    let mut bytes = Vec::with_capacity(size as usize);
    bytes.push(opcode);
    for i in 1..size {
        bytes.push(bus.read(addr.wrapping_add(i as u32)));
    }

    let mnemonic = info.mnemonic.name();
    let operand_addr = addr.wrapping_add(1);
    let operands = format_operands(bus, info.mode, operand_addr, state);
    let comment = format_comment(info.mode, bus, operand_addr);

    DisasmResult {
        mnemonic: mnemonic.to_string(),
        operands,
        comment,
        bytes,
        bytes_consumed: size,
    }
}

fn read_byte(bus: &mut dyn Addressable, addr: u32) -> u8 {
    bus.read(addr)
}

fn read_word(bus: &mut dyn Addressable, addr: u32) -> u16 {
    let lo = bus.read(addr) as u16;
    let hi = bus.read(addr.wrapping_add(1)) as u16;
    (hi << 8) | lo
}

fn read_long_val(bus: &mut dyn Addressable, addr: u32) -> u32 {
    let lo = bus.read(addr) as u32;
    let mid = bus.read(addr.wrapping_add(1)) as u32;
    let hi = bus.read(addr.wrapping_add(2)) as u32;
    (hi << 16) | (mid << 8) | lo
}

fn format_operands(
    bus: &mut dyn Addressable,
    mode: AddressingMode,
    op_addr: u32,
    state: &DisasmState,
) -> String {
    match mode {
        // The format prints no operand for the accumulator forms (`asl` /
        // `lsr` / `rol` / `ror` / `inc` / `dec` on A), although the WDC
        // mnemonic reads "ASL A", so traces line up with the reference
        // logs without bespoke normalisation.
        AddressingMode::Implied | AddressingMode::Accumulator => String::new(),

        AddressingMode::Immediate => {
            if state.m8() {
                format!("#${:02X}", read_byte(bus, op_addr))
            } else {
                format!("#${:04X}", read_word(bus, op_addr))
            }
        }
        AddressingMode::ImmediateX => {
            if state.x8() {
                format!("#${:02X}", read_byte(bus, op_addr))
            } else {
                format!("#${:04X}", read_word(bus, op_addr))
            }
        }
        // REP/SEP flag mask: always a single byte, never widened by M/X.
        AddressingMode::ImmediateByte => format!("#${:02X}", read_byte(bus, op_addr)),

        AddressingMode::Direct => format!("${:02X}", read_byte(bus, op_addr)),
        AddressingMode::DirectX => format!("${:02X},x", read_byte(bus, op_addr)),
        AddressingMode::DirectY => format!("${:02X},y", read_byte(bus, op_addr)),
        AddressingMode::DirectIndirect => format!("(${:02X})", read_byte(bus, op_addr)),
        AddressingMode::DirectIndirectLong => format!("[${:02X}]", read_byte(bus, op_addr)),
        AddressingMode::DirectXIndirect => format!("(${:02X},x)", read_byte(bus, op_addr)),
        AddressingMode::DirectIndirectY => format!("(${:02X}),y", read_byte(bus, op_addr)),
        AddressingMode::DirectIndirectLongY => format!("[${:02X}],y", read_byte(bus, op_addr)),

        AddressingMode::Absolute => format!("${:04X}", read_word(bus, op_addr)),
        AddressingMode::AbsoluteX => format!("${:04X},x", read_word(bus, op_addr)),
        AddressingMode::AbsoluteY => format!("${:04X},y", read_word(bus, op_addr)),
        AddressingMode::AbsoluteIndirect => format!("(${:04X})", read_word(bus, op_addr)),
        AddressingMode::AbsoluteXIndirect => format!("(${:04X},x)", read_word(bus, op_addr)),
        AddressingMode::AbsoluteIndirectLong => format!("[${:04X}]", read_word(bus, op_addr)),

        AddressingMode::AbsoluteLong => format!("${:06X}", read_long_val(bus, op_addr)),
        AddressingMode::AbsoluteLongX => format!("${:06X},x", read_long_val(bus, op_addr)),

        AddressingMode::StackRelative => format!("${:02X},s", read_byte(bus, op_addr)),
        AddressingMode::StackRelativeIndirectY => {
            format!("(${:02X},s),y", read_byte(bus, op_addr))
        }

        AddressingMode::Relative => {
            let offset = read_byte(bus, op_addr) as i8;
            // target = PC after fetching operand (op_addr + 1)
            let pc_after = (op_addr.wrapping_add(1)) as u16;
            let target = pc_after.wrapping_add(offset as u16);
            format!("${:04X}", target)
        }
        AddressingMode::RelativeLong => {
            let offset = read_word(bus, op_addr) as i16;
            let pc_after = (op_addr.wrapping_add(2)) as u16;
            let target = pc_after.wrapping_add(offset as u16);
            format!("${:04X}", target)
        }

        AddressingMode::BlockMove => {
            // Operand bytes are stored dst-bank then src-bank; the format
            // prints them in src, dst order (the WDC mnemonic's syntax).
            let dst_bank = read_byte(bus, op_addr);
            let src_bank = read_byte(bus, op_addr.wrapping_add(1));
            format!("${:02X}, ${:02X}", src_bank, dst_bank)
        }
    }
}

fn format_comment(mode: AddressingMode, bus: &mut dyn Addressable, op_addr: u32) -> Option<String> {
    match mode {
        AddressingMode::Relative => {
            let offset = read_byte(bus, op_addr) as i8;
            Some(format!("({:+})", offset))
        }
        AddressingMode::RelativeLong => {
            let offset = read_word(bus, op_addr) as i16;
            Some(format!("({:+})", offset))
        }
        _ => None,
    }
}
