//! 6502 disassembler.
//!
//! Mirrors the decode table structure, producing human-readable assembly
//! in standard 6502 syntax.

use crate::addressing::AddressingMode;
use crate::decode::{Mnemonic, OPCODE_TABLE};
use retro_core::cpu::Addressable;

/// Structured disassembly result.
#[derive(Debug, Clone)]
pub struct DisasmResult {
    pub mnemonic: String,
    pub operands: String,
    pub comment: Option<String>,
    pub bytes: Vec<u8>,
    pub bytes_consumed: u8,
}

/// Disassemble a single instruction at `addr`.
/// Returns a formatted string and the total instruction size in bytes.
pub fn disassemble_at(bus: &mut dyn Addressable, addr: u16) -> (String, u8) {
    let result = disassemble_one(bus, addr);
    let text = if result.operands.is_empty() {
        result.mnemonic.clone()
    } else {
        format!("{} {}", result.mnemonic, result.operands)
    };
    (text, result.bytes_consumed)
}

/// Disassemble a single instruction at `addr` with full detail.
pub fn disassemble_one(bus: &mut dyn Addressable, addr: u16) -> DisasmResult {
    let opcode = bus.read(addr as u32);
    let info = &OPCODE_TABLE[opcode as usize];

    let size = 1 + info.mode.operand_size();
    let mut bytes = Vec::with_capacity(size as usize);
    bytes.push(opcode);
    for i in 1..size {
        bytes.push(bus.read(addr.wrapping_add(i as u16) as u32));
    }

    let mnemonic = mnemonic_str(info.mnemonic, opcode);
    let operand_addr = addr.wrapping_add(1);
    let operands = format_operands(bus, info.mode, operand_addr);
    let comment = format_comment(info.mnemonic, info.mode, bus, operand_addr);

    DisasmResult {
        mnemonic: mnemonic.to_string(),
        operands,
        comment,
        bytes,
        bytes_consumed: size,
    }
}

fn mnemonic_str(m: Mnemonic, _opcode: u8) -> &'static str {
    match m {
        Mnemonic::ADC => "ADC",
        Mnemonic::AND => "AND",
        Mnemonic::ASL => "ASL",
        Mnemonic::BCC => "BCC",
        Mnemonic::BCS => "BCS",
        Mnemonic::BEQ => "BEQ",
        Mnemonic::BMI => "BMI",
        Mnemonic::BNE => "BNE",
        Mnemonic::BPL => "BPL",
        Mnemonic::BIT => "BIT",
        Mnemonic::BRK => "BRK",
        Mnemonic::BVC => "BVC",
        Mnemonic::BVS => "BVS",
        Mnemonic::CLC => "CLC",
        Mnemonic::CLD => "CLD",
        Mnemonic::CLI => "CLI",
        Mnemonic::CLV => "CLV",
        Mnemonic::CMP => "CMP",
        Mnemonic::CPX => "CPX",
        Mnemonic::CPY => "CPY",
        Mnemonic::DEC => "DEC",
        Mnemonic::DEX => "DEX",
        Mnemonic::DEY => "DEY",
        Mnemonic::EOR => "EOR",
        Mnemonic::INC => "INC",
        Mnemonic::INX => "INX",
        Mnemonic::INY => "INY",
        Mnemonic::JMP => "JMP",
        Mnemonic::JSR => "JSR",
        Mnemonic::LDA => "LDA",
        Mnemonic::LDX => "LDX",
        Mnemonic::LDY => "LDY",
        Mnemonic::LSR => "LSR",
        Mnemonic::NOP => "NOP",
        Mnemonic::ORA => "ORA",
        Mnemonic::PHA => "PHA",
        Mnemonic::PHP => "PHP",
        Mnemonic::PLA => "PLA",
        Mnemonic::PLP => "PLP",
        Mnemonic::ROL => "ROL",
        Mnemonic::ROR => "ROR",
        Mnemonic::RTI => "RTI",
        Mnemonic::RTS => "RTS",
        Mnemonic::SBC => "SBC",
        Mnemonic::SEC => "SEC",
        Mnemonic::SED => "SED",
        Mnemonic::SEI => "SEI",
        Mnemonic::STA => "STA",
        Mnemonic::STX => "STX",
        Mnemonic::STY => "STY",
        Mnemonic::TAX => "TAX",
        Mnemonic::TAY => "TAY",
        Mnemonic::TSX => "TSX",
        Mnemonic::TXA => "TXA",
        Mnemonic::TXS => "TXS",
        Mnemonic::TYA => "TYA",
        Mnemonic::ILL => {
            // An illegal opcode shows as `???`
            "???"
        }
    }
}

fn format_operands(bus: &mut dyn Addressable, mode: AddressingMode, operand_addr: u16) -> String {
    match mode {
        AddressingMode::Implied => String::new(),
        AddressingMode::Accumulator => "A".to_string(),
        AddressingMode::Immediate => {
            let val = bus.read(operand_addr as u32);
            format!("#${:02X}", val)
        }
        AddressingMode::ZeroPage => {
            let addr = bus.read(operand_addr as u32);
            format!("${:02X}", addr)
        }
        AddressingMode::ZeroPageX => {
            let addr = bus.read(operand_addr as u32);
            format!("${:02X},X", addr)
        }
        AddressingMode::ZeroPageY => {
            let addr = bus.read(operand_addr as u32);
            format!("${:02X},Y", addr)
        }
        AddressingMode::Absolute => {
            let lo = bus.read(operand_addr as u32) as u16;
            let hi = bus.read(operand_addr.wrapping_add(1) as u32) as u16;
            format!("${:04X}", (hi << 8) | lo)
        }
        AddressingMode::AbsoluteX => {
            let lo = bus.read(operand_addr as u32) as u16;
            let hi = bus.read(operand_addr.wrapping_add(1) as u32) as u16;
            format!("${:04X},X", (hi << 8) | lo)
        }
        AddressingMode::AbsoluteY => {
            let lo = bus.read(operand_addr as u32) as u16;
            let hi = bus.read(operand_addr.wrapping_add(1) as u32) as u16;
            format!("${:04X},Y", (hi << 8) | lo)
        }
        AddressingMode::Indirect => {
            let lo = bus.read(operand_addr as u32) as u16;
            let hi = bus.read(operand_addr.wrapping_add(1) as u32) as u16;
            format!("(${:04X})", (hi << 8) | lo)
        }
        AddressingMode::IndirectX => {
            let zp = bus.read(operand_addr as u32);
            format!("(${:02X},X)", zp)
        }
        AddressingMode::IndirectY => {
            let zp = bus.read(operand_addr as u32);
            format!("(${:02X}),Y", zp)
        }
        AddressingMode::Relative => {
            let offset = bus.read(operand_addr as u32) as i8;
            let target = operand_addr.wrapping_add(1).wrapping_add(offset as u16);
            format!("${:04X}", target)
        }
    }
}

fn format_comment(
    _mnemonic: Mnemonic,
    mode: AddressingMode,
    bus: &mut dyn Addressable,
    operand_addr: u16,
) -> Option<String> {
    match mode {
        AddressingMode::Relative => {
            let offset = bus.read(operand_addr as u32) as i8;
            Some(format!("({:+})", offset))
        }
        _ => None,
    }
}
