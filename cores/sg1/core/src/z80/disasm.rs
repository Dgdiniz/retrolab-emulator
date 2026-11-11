//! Z80 Disassembler
//!
//! Complete Z80 disassembly including undocumented instructions.

use retro_core::cpu::Addressable;

/// Helper: Get register name from 3-bit encoding
pub fn reg8_name(code: u8) -> &'static str {
    match code & 0x07 {
        0 => "B",
        1 => "C",
        2 => "D",
        3 => "E",
        4 => "H",
        5 => "L",
        6 => "(HL)",
        7 => "A",
        _ => unreachable!(),
    }
}

/// Helper: Get 16-bit register pair name
pub fn reg16_name(code: u8, use_af: bool) -> &'static str {
    match code & 0x03 {
        0 => "BC",
        1 => "DE",
        2 => "HL",
        3 => {
            if use_af {
                "AF"
            } else {
                "SP"
            }
        }
        _ => unreachable!(),
    }
}

/// IXH/IXL register names for DD-prefix undocumented ops
fn ix_reg8_name(code: u8) -> &'static str {
    match code & 0x07 {
        0 => "B",
        1 => "C",
        2 => "D",
        3 => "E",
        4 => "IXH",
        5 => "IXL",
        6 => "(HL)", // shouldn't be used for undocumented
        7 => "A",
        _ => unreachable!(),
    }
}

/// IYH/IYL register names for FD-prefix undocumented ops
fn iy_reg8_name(code: u8) -> &'static str {
    match code & 0x07 {
        0 => "B",
        1 => "C",
        2 => "D",
        3 => "E",
        4 => "IYH",
        5 => "IYL",
        6 => "(HL)", // shouldn't be used for undocumented
        7 => "A",
        _ => unreachable!(),
    }
}

/// Disassemble a single Z80 instruction at the given address
/// Returns (mnemonic, bytes consumed)
pub fn disassemble_at(bus: &mut dyn Addressable, addr: u16) -> (String, u16) {
    let opcode = bus.read(addr as u32);

    match opcode {
        // Prefixes
        0xCB => disassemble_cb_prefix(bus, addr),
        0xDD => disassemble_dd_prefix(bus, addr),
        0xED => disassemble_ed_prefix(bus, addr),
        0xFD => disassemble_fd_prefix(bus, addr),

        // NOP
        0x00 => ("NOP".to_string(), 1),

        // 0x01-0x0F
        0x01 => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("LD BC, ${:04X}", nn), 3)
        }
        0x02 => ("LD (BC), A".to_string(), 1),
        0x03 => ("INC BC".to_string(), 1),
        0x04 => ("INC B".to_string(), 1),
        0x05 => ("DEC B".to_string(), 1),
        0x06 => {
            let n = bus.read(addr.wrapping_add(1) as u32);
            (format!("LD B, ${:02X}", n), 2)
        }
        0x07 => ("RLCA".to_string(), 1),
        0x08 => ("EX AF, AF'".to_string(), 1),
        0x09 => ("ADD HL, BC".to_string(), 1),
        0x0A => ("LD A, (BC)".to_string(), 1),
        0x0B => ("DEC BC".to_string(), 1),
        0x0C => ("INC C".to_string(), 1),
        0x0D => ("DEC C".to_string(), 1),
        0x0E => {
            let n = bus.read(addr.wrapping_add(1) as u32);
            (format!("LD C, ${:02X}", n), 2)
        }
        0x0F => ("RRCA".to_string(), 1),

        // 0x10-0x1F
        0x10 => {
            let e = bus.read(addr.wrapping_add(1) as u32) as i8;
            let target = addr.wrapping_add(2).wrapping_add(e as u16);
            (format!("DJNZ ${:04X}", target), 2)
        }
        0x11 => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("LD DE, ${:04X}", nn), 3)
        }
        0x12 => ("LD (DE), A".to_string(), 1),
        0x13 => ("INC DE".to_string(), 1),
        0x14 => ("INC D".to_string(), 1),
        0x15 => ("DEC D".to_string(), 1),
        0x16 => {
            let n = bus.read(addr.wrapping_add(1) as u32);
            (format!("LD D, ${:02X}", n), 2)
        }
        0x17 => ("RLA".to_string(), 1),
        0x18 => {
            let e = bus.read(addr.wrapping_add(1) as u32) as i8;
            let target = addr.wrapping_add(2).wrapping_add(e as u16);
            (format!("JR ${:04X}", target), 2)
        }
        0x19 => ("ADD HL, DE".to_string(), 1),
        0x1A => ("LD A, (DE)".to_string(), 1),
        0x1B => ("DEC DE".to_string(), 1),
        0x1C => ("INC E".to_string(), 1),
        0x1D => ("DEC E".to_string(), 1),
        0x1E => {
            let n = bus.read(addr.wrapping_add(1) as u32);
            (format!("LD E, ${:02X}", n), 2)
        }
        0x1F => ("RRA".to_string(), 1),

        // 0x20-0x2F
        0x20 => {
            let e = bus.read(addr.wrapping_add(1) as u32) as i8;
            let target = addr.wrapping_add(2).wrapping_add(e as u16);
            (format!("JR NZ, ${:04X}", target), 2)
        }
        0x21 => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("LD HL, ${:04X}", nn), 3)
        }
        0x22 => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("LD (${:04X}), HL", nn), 3)
        }
        0x23 => ("INC HL".to_string(), 1),
        0x24 => ("INC H".to_string(), 1),
        0x25 => ("DEC H".to_string(), 1),
        0x26 => {
            let n = bus.read(addr.wrapping_add(1) as u32);
            (format!("LD H, ${:02X}", n), 2)
        }
        0x27 => ("DAA".to_string(), 1),
        0x28 => {
            let e = bus.read(addr.wrapping_add(1) as u32) as i8;
            let target = addr.wrapping_add(2).wrapping_add(e as u16);
            (format!("JR Z, ${:04X}", target), 2)
        }
        0x29 => ("ADD HL, HL".to_string(), 1),
        0x2A => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("LD HL, (${:04X})", nn), 3)
        }
        0x2B => ("DEC HL".to_string(), 1),
        0x2C => ("INC L".to_string(), 1),
        0x2D => ("DEC L".to_string(), 1),
        0x2E => {
            let n = bus.read(addr.wrapping_add(1) as u32);
            (format!("LD L, ${:02X}", n), 2)
        }
        0x2F => ("CPL".to_string(), 1),

        // 0x30-0x3F
        0x30 => {
            let e = bus.read(addr.wrapping_add(1) as u32) as i8;
            let target = addr.wrapping_add(2).wrapping_add(e as u16);
            (format!("JR NC, ${:04X}", target), 2)
        }
        0x31 => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("LD SP, ${:04X}", nn), 3)
        }
        0x32 => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("LD (${:04X}), A", nn), 3)
        }
        0x33 => ("INC SP".to_string(), 1),
        0x34 => ("INC (HL)".to_string(), 1),
        0x35 => ("DEC (HL)".to_string(), 1),
        0x36 => {
            let n = bus.read(addr.wrapping_add(1) as u32);
            (format!("LD (HL), ${:02X}", n), 2)
        }
        0x37 => ("SCF".to_string(), 1),
        0x38 => {
            let e = bus.read(addr.wrapping_add(1) as u32) as i8;
            let target = addr.wrapping_add(2).wrapping_add(e as u16);
            (format!("JR C, ${:04X}", target), 2)
        }
        0x39 => ("ADD HL, SP".to_string(), 1),
        0x3A => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("LD A, (${:04X})", nn), 3)
        }
        0x3B => ("DEC SP".to_string(), 1),
        0x3C => ("INC A".to_string(), 1),
        0x3D => ("DEC A".to_string(), 1),
        0x3E => {
            let n = bus.read(addr.wrapping_add(1) as u32);
            (format!("LD A, ${:02X}", n), 2)
        }
        0x3F => ("CCF".to_string(), 1),

        // 0x40-0x7F: LD r, r' block (including (HL) variants)
        0x76 => ("HALT".to_string(), 1),
        0x40..=0x7F => {
            let dst = (opcode >> 3) & 0x07;
            let src = opcode & 0x07;
            (format!("LD {}, {}", reg8_name(dst), reg8_name(src)), 1)
        }

        // 0x80-0x87: ADD A, r
        0x80..=0x87 => {
            let src = opcode & 0x07;
            (format!("ADD A, {}", reg8_name(src)), 1)
        }
        // 0x88-0x8F: ADC A, r
        0x88..=0x8F => {
            let src = opcode & 0x07;
            (format!("ADC A, {}", reg8_name(src)), 1)
        }
        // 0x90-0x97: SUB r
        0x90..=0x97 => {
            let src = opcode & 0x07;
            (format!("SUB {}", reg8_name(src)), 1)
        }
        // 0x98-0x9F: SBC A, r
        0x98..=0x9F => {
            let src = opcode & 0x07;
            (format!("SBC A, {}", reg8_name(src)), 1)
        }
        // 0xA0-0xA7: AND r
        0xA0..=0xA7 => {
            let src = opcode & 0x07;
            (format!("AND {}", reg8_name(src)), 1)
        }
        // 0xA8-0xAF: XOR r
        0xA8..=0xAF => {
            let src = opcode & 0x07;
            (format!("XOR {}", reg8_name(src)), 1)
        }
        // 0xB0-0xB7: OR r
        0xB0..=0xB7 => {
            let src = opcode & 0x07;
            (format!("OR {}", reg8_name(src)), 1)
        }
        // 0xB8-0xBF: CP r
        0xB8..=0xBF => {
            let src = opcode & 0x07;
            (format!("CP {}", reg8_name(src)), 1)
        }

        // 0xC0-0xFF
        0xC0 => ("RET NZ".to_string(), 1),
        0xC1 => ("POP BC".to_string(), 1),
        0xC2 => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("JP NZ, ${:04X}", nn), 3)
        }
        0xC3 => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("JP ${:04X}", nn), 3)
        }
        0xC4 => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("CALL NZ, ${:04X}", nn), 3)
        }
        0xC5 => ("PUSH BC".to_string(), 1),
        0xC6 => {
            let n = bus.read(addr.wrapping_add(1) as u32);
            (format!("ADD A, ${:02X}", n), 2)
        }
        0xC7 => ("RST $00".to_string(), 1),
        0xC8 => ("RET Z".to_string(), 1),
        0xC9 => ("RET".to_string(), 1),
        0xCA => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("JP Z, ${:04X}", nn), 3)
        }
        // 0xCB handled above
        0xCC => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("CALL Z, ${:04X}", nn), 3)
        }
        0xCD => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("CALL ${:04X}", nn), 3)
        }
        0xCE => {
            let n = bus.read(addr.wrapping_add(1) as u32);
            (format!("ADC A, ${:02X}", n), 2)
        }
        0xCF => ("RST $08".to_string(), 1),

        0xD0 => ("RET NC".to_string(), 1),
        0xD1 => ("POP DE".to_string(), 1),
        0xD2 => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("JP NC, ${:04X}", nn), 3)
        }
        0xD3 => {
            let port = bus.read(addr.wrapping_add(1) as u32);
            (format!("OUT (${:02X}), A", port), 2)
        }
        0xD4 => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("CALL NC, ${:04X}", nn), 3)
        }
        0xD5 => ("PUSH DE".to_string(), 1),
        0xD6 => {
            let n = bus.read(addr.wrapping_add(1) as u32);
            (format!("SUB ${:02X}", n), 2)
        }
        0xD7 => ("RST $10".to_string(), 1),
        0xD8 => ("RET C".to_string(), 1),
        0xD9 => ("EXX".to_string(), 1),
        0xDA => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("JP C, ${:04X}", nn), 3)
        }
        0xDB => {
            let port = bus.read(addr.wrapping_add(1) as u32);
            (format!("IN A, (${:02X})", port), 2)
        }
        0xDC => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("CALL C, ${:04X}", nn), 3)
        }
        // 0xDD handled above
        0xDE => {
            let n = bus.read(addr.wrapping_add(1) as u32);
            (format!("SBC A, ${:02X}", n), 2)
        }
        0xDF => ("RST $18".to_string(), 1),

        0xE0 => ("RET PO".to_string(), 1),
        0xE1 => ("POP HL".to_string(), 1),
        0xE2 => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("JP PO, ${:04X}", nn), 3)
        }
        0xE3 => ("EX (SP), HL".to_string(), 1),
        0xE4 => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("CALL PO, ${:04X}", nn), 3)
        }
        0xE5 => ("PUSH HL".to_string(), 1),
        0xE6 => {
            let n = bus.read(addr.wrapping_add(1) as u32);
            (format!("AND ${:02X}", n), 2)
        }
        0xE7 => ("RST $20".to_string(), 1),
        0xE8 => ("RET PE".to_string(), 1),
        0xE9 => ("JP (HL)".to_string(), 1),
        0xEA => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("JP PE, ${:04X}", nn), 3)
        }
        0xEB => ("EX DE, HL".to_string(), 1),
        0xEC => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("CALL PE, ${:04X}", nn), 3)
        }
        // 0xED handled above
        0xEE => {
            let n = bus.read(addr.wrapping_add(1) as u32);
            (format!("XOR ${:02X}", n), 2)
        }
        0xEF => ("RST $28".to_string(), 1),

        0xF0 => ("RET P".to_string(), 1),
        0xF1 => ("POP AF".to_string(), 1),
        0xF2 => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("JP P, ${:04X}", nn), 3)
        }
        0xF3 => ("DI".to_string(), 1),
        0xF4 => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("CALL P, ${:04X}", nn), 3)
        }
        0xF5 => ("PUSH AF".to_string(), 1),
        0xF6 => {
            let n = bus.read(addr.wrapping_add(1) as u32);
            (format!("OR ${:02X}", n), 2)
        }
        0xF7 => ("RST $30".to_string(), 1),
        0xF8 => ("RET M".to_string(), 1),
        0xF9 => ("LD SP, HL".to_string(), 1),
        0xFA => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("JP M, ${:04X}", nn), 3)
        }
        0xFB => ("EI".to_string(), 1),
        0xFC => {
            let nn = read_u16(bus, addr.wrapping_add(1));
            (format!("CALL M, ${:04X}", nn), 3)
        }
        // 0xFD handled above
        0xFE => {
            let n = bus.read(addr.wrapping_add(1) as u32);
            (format!("CP ${:02X}", n), 2)
        }
        0xFF => ("RST $38".to_string(), 1),
    }
}

/// Read little-endian u16 from bus
fn read_u16(bus: &mut dyn Addressable, addr: u16) -> u16 {
    let lo = bus.read(addr as u32) as u16;
    let hi = bus.read(addr.wrapping_add(1) as u32) as u16;
    (hi << 8) | lo
}

/// Convert disassembly text to the standard Z80 trace format.
///
/// Uses lowercase mnemonics/registers, no space after commas,
/// and uppercase hex values. For example:
///   Internal format:  "LD BC, $1FD9"
///   Trace format:     "ld   bc,$1FD9"
pub fn to_trace_format(text: &str) -> String {
    // Lowercase everything
    let lower = text.to_lowercase();
    // Remove space after comma ("bc, $1fd9" -> "bc,$1fd9")
    let no_comma_space = lower.replace(", ", ",");
    // Re-uppercase hex values (characters following '$')
    let mut result = String::with_capacity(no_comma_space.len());
    let mut in_hex = false;
    for ch in no_comma_space.chars() {
        if ch == '$' {
            in_hex = true;
            result.push(ch);
        } else if in_hex && ch.is_ascii_hexdigit() {
            result.push(ch.to_ascii_uppercase());
        } else {
            in_hex = false;
            result.push(ch);
        }
    }
    result
}

/// Format disassembly with address prefix and alignment.
///
/// Trace format:  "000C: ldir"  or  "0001: ld   bc,$1FD9"
/// The mnemonic is padded to 5 characters when followed by operands.
pub fn format_trace_disasm(addr: u16, disasm_text: &str) -> String {
    let formatted = to_trace_format(disasm_text);
    if let Some(pos) = formatted.find(' ') {
        let mnemonic = &formatted[..pos];
        let operands = formatted[pos + 1..].trim();
        format!("{:04X}: {:<5}{}", addr, mnemonic, operands)
    } else {
        format!("{:04X}: {}", addr, formatted)
    }
}

// ============================================================================
// Disassembly from raw bytes (for trace entries with stored instruction bytes)
// ============================================================================

/// A minimal bus adapter that reads from a fixed 4-byte slice.
/// Used to disassemble Z80 instructions from stored raw bytes in trace entries,
/// avoiding the need to access the (potentially changed) live Z80 RAM.
struct SliceBus {
    base_addr: u16,
    bytes: [u8; 4],
}

impl Addressable for SliceBus {
    fn read(&mut self, addr: u32) -> u8 {
        let offset = (addr as u16).wrapping_sub(self.base_addr) as usize;
        if offset < 4 {
            self.bytes[offset]
        } else {
            0
        }
    }
    fn write(&mut self, _addr: u32, _value: u8) {
        // No-op: SliceBus is read-only
    }
}

/// Disassemble a Z80 instruction from stored raw bytes (up to 4 bytes).
/// Returns (mnemonic, bytes consumed), same as `disassemble_at`.
///
/// This is used when converting binary trace entries to text, where the
/// original Z80 RAM may have been overwritten by the 68000 since capture.
pub fn disassemble_from_bytes(addr: u16, raw_bytes: &[u8; 4]) -> (String, u16) {
    let mut bus = SliceBus {
        base_addr: addr,
        bytes: *raw_bytes,
    };
    disassemble_at(&mut bus, addr)
}

// ============================================================================
// CB prefix: Bit operations
// ============================================================================
fn disassemble_cb_prefix(bus: &mut dyn Addressable, addr: u16) -> (String, u16) {
    let opcode = bus.read(addr.wrapping_add(1) as u32);
    let reg_idx = opcode & 0x07;
    let reg_name = reg8_name(reg_idx);

    let mnemonic = match opcode {
        0x00..=0x07 => format!("RLC {}", reg_name),
        0x08..=0x0F => format!("RRC {}", reg_name),
        0x10..=0x17 => format!("RL {}", reg_name),
        0x18..=0x1F => format!("RR {}", reg_name),
        0x20..=0x27 => format!("SLA {}", reg_name),
        0x28..=0x2F => format!("SRA {}", reg_name),
        0x30..=0x37 => format!("SLL {}", reg_name), // undocumented
        0x38..=0x3F => format!("SRL {}", reg_name),
        0x40..=0x7F => {
            let bit = (opcode >> 3) & 0x07;
            format!("BIT {}, {}", bit, reg_name)
        }
        0x80..=0xBF => {
            let bit = (opcode >> 3) & 0x07;
            format!("RES {}, {}", bit, reg_name)
        }
        0xC0..=0xFF => {
            let bit = (opcode >> 3) & 0x07;
            format!("SET {}, {}", bit, reg_name)
        }
    };

    (mnemonic, 2)
}

// ============================================================================
// DDCB / FDCB prefix: Indexed bit operations (including undocumented store variants)
// ============================================================================
fn disassemble_cb_indexed_prefix(
    bus: &mut dyn Addressable,
    addr: u16,
    idx_name: &str,
) -> (String, u16) {
    let d = bus.read(addr.wrapping_add(2) as u32) as i8;
    let opcode = bus.read(addr.wrapping_add(3) as u32);
    let reg_idx = opcode & 0x07;

    // For (IX+d)/(IY+d) bit ops, reg_idx=6 is the "normal" form,
    // others are undocumented store-to-register variants
    let mem_op = format!("({}{})", idx_name, format_disp(d));

    let mnemonic = match opcode & 0xF8 {
        // Rotate/shift block
        0x00 => format_idx_shift("RLC", reg_idx, &mem_op),
        0x08 => format_idx_shift("RRC", reg_idx, &mem_op),
        0x10 => format_idx_shift("RL", reg_idx, &mem_op),
        0x18 => format_idx_shift("RR", reg_idx, &mem_op),
        0x20 => format_idx_shift("SLA", reg_idx, &mem_op),
        0x28 => format_idx_shift("SRA", reg_idx, &mem_op),
        0x30 => format_idx_shift("SLL", reg_idx, &mem_op),
        0x38 => format_idx_shift("SRL", reg_idx, &mem_op),
        // BIT block (no store variants - all act on memory only)
        0x40 | 0x48 | 0x50 | 0x58 | 0x60 | 0x68 | 0x70 | 0x78 => {
            let bit = (opcode >> 3) & 0x07;
            format!("BIT {}, {}", bit, mem_op)
        }
        // RES block
        0x80 | 0x88 | 0x90 | 0x98 | 0xA0 | 0xA8 | 0xB0 | 0xB8 => {
            let bit = (opcode >> 3) & 0x07;
            if reg_idx == 6 {
                format!("RES {}, {}", bit, mem_op)
            } else {
                format!("RES {}, {}, {}", bit, mem_op, reg8_name(reg_idx))
            }
        }
        // SET block
        0xC0 | 0xC8 | 0xD0 | 0xD8 | 0xE0 | 0xE8 | 0xF0 | 0xF8 => {
            let bit = (opcode >> 3) & 0x07;
            if reg_idx == 6 {
                format!("SET {}, {}", bit, mem_op)
            } else {
                format!("SET {}, {}, {}", bit, mem_op, reg8_name(reg_idx))
            }
        }
        _ => unreachable!(),
    };

    (mnemonic, 4)
}

/// Format indexed shift/rotate instruction
/// reg_idx=6 is normal (memory-only), others are undocumented store variants
fn format_idx_shift(op: &str, reg_idx: u8, mem_op: &str) -> String {
    if reg_idx == 6 {
        format!("{} {}", op, mem_op)
    } else {
        format!("{} {}, {}", op, mem_op, reg8_name(reg_idx))
    }
}

/// Format displacement as signed offset string
fn format_disp(d: i8) -> String {
    if d >= 0 {
        format!("+{}", d)
    } else {
        format!("{}", d)
    }
}

// ============================================================================
// DD prefix: IX operations (including undocumented IXH/IXL)
// ============================================================================
fn disassemble_dd_prefix(bus: &mut dyn Addressable, addr: u16) -> (String, u16) {
    let opcode = bus.read(addr.wrapping_add(1) as u32);

    match opcode {
        0xCB => disassemble_cb_indexed_prefix(bus, addr, "IX"),

        // 16-bit operations
        0x09 => ("ADD IX, BC".to_string(), 2),
        0x19 => ("ADD IX, DE".to_string(), 2),
        0x21 => {
            let nn = read_u16(bus, addr.wrapping_add(2));
            (format!("LD IX, ${:04X}", nn), 4)
        }
        0x22 => {
            let nn = read_u16(bus, addr.wrapping_add(2));
            (format!("LD (${:04X}), IX", nn), 4)
        }
        0x23 => ("INC IX".to_string(), 2),
        0x29 => ("ADD IX, IX".to_string(), 2),
        0x2A => {
            let nn = read_u16(bus, addr.wrapping_add(2));
            (format!("LD IX, (${:04X})", nn), 4)
        }
        0x2B => ("DEC IX".to_string(), 2),
        0x39 => ("ADD IX, SP".to_string(), 2),

        // Undocumented IXH/IXL operations
        0x24 => ("INC IXH".to_string(), 2),
        0x25 => ("DEC IXH".to_string(), 2),
        0x26 => {
            let n = bus.read(addr.wrapping_add(2) as u32);
            (format!("LD IXH, ${:02X}", n), 3)
        }
        0x2C => ("INC IXL".to_string(), 2),
        0x2D => ("DEC IXL".to_string(), 2),
        0x2E => {
            let n = bus.read(addr.wrapping_add(2) as u32);
            (format!("LD IXL, ${:02X}", n), 3)
        }

        // Indexed memory operations (IX+d)
        0x34 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("INC (IX{})", format_disp(d)), 3)
        }
        0x35 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("DEC (IX{})", format_disp(d)), 3)
        }
        0x36 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            let n = bus.read(addr.wrapping_add(3) as u32);
            (format!("LD (IX{}), ${:02X}", format_disp(d), n), 4)
        }

        // LD r, (IX+d)
        0x46 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD B, (IX{})", format_disp(d)), 3)
        }
        0x4E => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD C, (IX{})", format_disp(d)), 3)
        }
        0x56 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD D, (IX{})", format_disp(d)), 3)
        }
        0x5E => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD E, (IX{})", format_disp(d)), 3)
        }
        0x66 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD H, (IX{})", format_disp(d)), 3)
        }
        0x6E => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD L, (IX{})", format_disp(d)), 3)
        }
        0x7E => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD A, (IX{})", format_disp(d)), 3)
        }

        // LD (IX+d), r
        0x70 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD (IX{}), B", format_disp(d)), 3)
        }
        0x71 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD (IX{}), C", format_disp(d)), 3)
        }
        0x72 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD (IX{}), D", format_disp(d)), 3)
        }
        0x73 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD (IX{}), E", format_disp(d)), 3)
        }
        0x74 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD (IX{}), H", format_disp(d)), 3)
        }
        0x75 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD (IX{}), L", format_disp(d)), 3)
        }
        0x77 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD (IX{}), A", format_disp(d)), 3)
        }

        // Arithmetic with (IX+d)
        0x86 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("ADD A, (IX{})", format_disp(d)), 3)
        }
        0x8E => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("ADC A, (IX{})", format_disp(d)), 3)
        }
        0x96 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("SUB (IX{})", format_disp(d)), 3)
        }
        0x9E => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("SBC A, (IX{})", format_disp(d)), 3)
        }
        0xA6 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("AND (IX{})", format_disp(d)), 3)
        }
        0xAE => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("XOR (IX{})", format_disp(d)), 3)
        }
        0xB6 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("OR (IX{})", format_disp(d)), 3)
        }
        0xBE => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("CP (IX{})", format_disp(d)), 3)
        }

        // Stack/control
        0xE1 => ("POP IX".to_string(), 2),
        0xE3 => ("EX (SP), IX".to_string(), 2),
        0xE5 => ("PUSH IX".to_string(), 2),
        0xE9 => ("JP (IX)".to_string(), 2),
        0xF9 => ("LD SP, IX".to_string(), 2),

        // Undocumented LD r, IXH/IXL and LD IXH/IXL, r
        // 0x44 LD B, IXH .. 0x45 LD B, IXL .. etc
        0x40..=0x6F if (opcode & 0x07) != 6 && ((opcode >> 3) & 0x07) != 6 => {
            let dst = (opcode >> 3) & 0x07;
            let src = opcode & 0x07;
            (
                format!("LD {}, {}", ix_reg8_name(dst), ix_reg8_name(src)),
                2,
            )
        }

        // Undocumented arithmetic with IXH/IXL
        0x84 => ("ADD A, IXH".to_string(), 2),
        0x85 => ("ADD A, IXL".to_string(), 2),
        0x8C => ("ADC A, IXH".to_string(), 2),
        0x8D => ("ADC A, IXL".to_string(), 2),
        0x94 => ("SUB IXH".to_string(), 2),
        0x95 => ("SUB IXL".to_string(), 2),
        0x9C => ("SBC A, IXH".to_string(), 2),
        0x9D => ("SBC A, IXL".to_string(), 2),
        0xA4 => ("AND IXH".to_string(), 2),
        0xA5 => ("AND IXL".to_string(), 2),
        0xAC => ("XOR IXH".to_string(), 2),
        0xAD => ("XOR IXL".to_string(), 2),
        0xB4 => ("OR IXH".to_string(), 2),
        0xB5 => ("OR IXL".to_string(), 2),
        0xBC => ("CP IXH".to_string(), 2),
        0xBD => ("CP IXL".to_string(), 2),

        _ => (format!("DB $DD, ${:02X}", opcode), 2),
    }
}

// ============================================================================
// ED prefix: Extended instructions
// ============================================================================
fn disassemble_ed_prefix(bus: &mut dyn Addressable, addr: u16) -> (String, u16) {
    let opcode = bus.read(addr.wrapping_add(1) as u32);

    match opcode {
        // I/O operations
        0x40 => ("IN B, (C)".to_string(), 2),
        0x41 => ("OUT (C), B".to_string(), 2),
        0x48 => ("IN C, (C)".to_string(), 2),
        0x49 => ("OUT (C), C".to_string(), 2),
        0x50 => ("IN D, (C)".to_string(), 2),
        0x51 => ("OUT (C), D".to_string(), 2),
        0x58 => ("IN E, (C)".to_string(), 2),
        0x59 => ("OUT (C), E".to_string(), 2),
        0x60 => ("IN H, (C)".to_string(), 2),
        0x61 => ("OUT (C), H".to_string(), 2),
        0x68 => ("IN L, (C)".to_string(), 2),
        0x69 => ("OUT (C), L".to_string(), 2),
        0x70 => ("IN (C)".to_string(), 2), // Undocumented - flags only
        0x71 => ("OUT (C), 0".to_string(), 2), // Undocumented
        0x78 => ("IN A, (C)".to_string(), 2),
        0x79 => ("OUT (C), A".to_string(), 2),

        // 16-bit arithmetic
        0x42 => ("SBC HL, BC".to_string(), 2),
        0x4A => ("ADC HL, BC".to_string(), 2),
        0x52 => ("SBC HL, DE".to_string(), 2),
        0x5A => ("ADC HL, DE".to_string(), 2),
        0x62 => ("SBC HL, HL".to_string(), 2),
        0x6A => ("ADC HL, HL".to_string(), 2),
        0x72 => ("SBC HL, SP".to_string(), 2),
        0x7A => ("ADC HL, SP".to_string(), 2),

        // 16-bit load indirect
        0x43 => {
            let nn = read_u16(bus, addr.wrapping_add(2));
            (format!("LD (${:04X}), BC", nn), 4)
        }
        0x4B => {
            let nn = read_u16(bus, addr.wrapping_add(2));
            (format!("LD BC, (${:04X})", nn), 4)
        }
        0x53 => {
            let nn = read_u16(bus, addr.wrapping_add(2));
            (format!("LD (${:04X}), DE", nn), 4)
        }
        0x5B => {
            let nn = read_u16(bus, addr.wrapping_add(2));
            (format!("LD DE, (${:04X})", nn), 4)
        }
        0x63 => {
            let nn = read_u16(bus, addr.wrapping_add(2));
            (format!("LD (${:04X}), HL", nn), 4)
        }
        0x6B => {
            let nn = read_u16(bus, addr.wrapping_add(2));
            (format!("LD HL, (${:04X})", nn), 4)
        }
        0x73 => {
            let nn = read_u16(bus, addr.wrapping_add(2));
            (format!("LD (${:04X}), SP", nn), 4)
        }
        0x7B => {
            let nn = read_u16(bus, addr.wrapping_add(2));
            (format!("LD SP, (${:04X})", nn), 4)
        }

        // Interrupt/special
        0x44 => ("NEG".to_string(), 2),
        0x45 => ("RETN".to_string(), 2),
        0x46 => ("IM 0".to_string(), 2),
        0x47 => ("LD I, A".to_string(), 2),
        0x4D => ("RETI".to_string(), 2),
        0x4F => ("LD R, A".to_string(), 2),
        0x56 => ("IM 1".to_string(), 2),
        0x57 => ("LD A, I".to_string(), 2),
        0x5E => ("IM 2".to_string(), 2),
        0x5F => ("LD A, R".to_string(), 2),

        // Rotate digits
        0x67 => ("RRD".to_string(), 2),
        0x6F => ("RLD".to_string(), 2),

        // Block transfer/search
        0xA0 => ("LDI".to_string(), 2),
        0xA1 => ("CPI".to_string(), 2),
        0xA2 => ("INI".to_string(), 2),
        0xA3 => ("OUTI".to_string(), 2),
        0xA8 => ("LDD".to_string(), 2),
        0xA9 => ("CPD".to_string(), 2),
        0xAA => ("IND".to_string(), 2),
        0xAB => ("OUTD".to_string(), 2),
        0xB0 => ("LDIR".to_string(), 2),
        0xB1 => ("CPIR".to_string(), 2),
        0xB2 => ("INIR".to_string(), 2),
        0xB3 => ("OTIR".to_string(), 2),
        0xB8 => ("LDDR".to_string(), 2),
        0xB9 => ("CPDR".to_string(), 2),
        0xBA => ("INDR".to_string(), 2),
        0xBB => ("OTDR".to_string(), 2),

        _ => (format!("DB $ED, ${:02X}", opcode), 2),
    }
}

// ============================================================================
// FD prefix: IY operations (including undocumented IYH/IYL)
// ============================================================================
fn disassemble_fd_prefix(bus: &mut dyn Addressable, addr: u16) -> (String, u16) {
    let opcode = bus.read(addr.wrapping_add(1) as u32);

    match opcode {
        0xCB => disassemble_cb_indexed_prefix(bus, addr, "IY"),

        // 16-bit operations
        0x09 => ("ADD IY, BC".to_string(), 2),
        0x19 => ("ADD IY, DE".to_string(), 2),
        0x21 => {
            let nn = read_u16(bus, addr.wrapping_add(2));
            (format!("LD IY, ${:04X}", nn), 4)
        }
        0x22 => {
            let nn = read_u16(bus, addr.wrapping_add(2));
            (format!("LD (${:04X}), IY", nn), 4)
        }
        0x23 => ("INC IY".to_string(), 2),
        0x29 => ("ADD IY, IY".to_string(), 2),
        0x2A => {
            let nn = read_u16(bus, addr.wrapping_add(2));
            (format!("LD IY, (${:04X})", nn), 4)
        }
        0x2B => ("DEC IY".to_string(), 2),
        0x39 => ("ADD IY, SP".to_string(), 2),

        // Undocumented IYH/IYL operations
        0x24 => ("INC IYH".to_string(), 2),
        0x25 => ("DEC IYH".to_string(), 2),
        0x26 => {
            let n = bus.read(addr.wrapping_add(2) as u32);
            (format!("LD IYH, ${:02X}", n), 3)
        }
        0x2C => ("INC IYL".to_string(), 2),
        0x2D => ("DEC IYL".to_string(), 2),
        0x2E => {
            let n = bus.read(addr.wrapping_add(2) as u32);
            (format!("LD IYL, ${:02X}", n), 3)
        }

        // Indexed memory operations (IY+d)
        0x34 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("INC (IY{})", format_disp(d)), 3)
        }
        0x35 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("DEC (IY{})", format_disp(d)), 3)
        }
        0x36 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            let n = bus.read(addr.wrapping_add(3) as u32);
            (format!("LD (IY{}), ${:02X}", format_disp(d), n), 4)
        }

        // LD r, (IY+d)
        0x46 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD B, (IY{})", format_disp(d)), 3)
        }
        0x4E => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD C, (IY{})", format_disp(d)), 3)
        }
        0x56 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD D, (IY{})", format_disp(d)), 3)
        }
        0x5E => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD E, (IY{})", format_disp(d)), 3)
        }
        0x66 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD H, (IY{})", format_disp(d)), 3)
        }
        0x6E => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD L, (IY{})", format_disp(d)), 3)
        }
        0x7E => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD A, (IY{})", format_disp(d)), 3)
        }

        // LD (IY+d), r
        0x70 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD (IY{}), B", format_disp(d)), 3)
        }
        0x71 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD (IY{}), C", format_disp(d)), 3)
        }
        0x72 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD (IY{}), D", format_disp(d)), 3)
        }
        0x73 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD (IY{}), E", format_disp(d)), 3)
        }
        0x74 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD (IY{}), H", format_disp(d)), 3)
        }
        0x75 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD (IY{}), L", format_disp(d)), 3)
        }
        0x77 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("LD (IY{}), A", format_disp(d)), 3)
        }

        // Arithmetic with (IY+d)
        0x86 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("ADD A, (IY{})", format_disp(d)), 3)
        }
        0x8E => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("ADC A, (IY{})", format_disp(d)), 3)
        }
        0x96 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("SUB (IY{})", format_disp(d)), 3)
        }
        0x9E => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("SBC A, (IY{})", format_disp(d)), 3)
        }
        0xA6 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("AND (IY{})", format_disp(d)), 3)
        }
        0xAE => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("XOR (IY{})", format_disp(d)), 3)
        }
        0xB6 => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("OR (IY{})", format_disp(d)), 3)
        }
        0xBE => {
            let d = bus.read(addr.wrapping_add(2) as u32) as i8;
            (format!("CP (IY{})", format_disp(d)), 3)
        }

        // Stack/control
        0xE1 => ("POP IY".to_string(), 2),
        0xE3 => ("EX (SP), IY".to_string(), 2),
        0xE5 => ("PUSH IY".to_string(), 2),
        0xE9 => ("JP (IY)".to_string(), 2),
        0xF9 => ("LD SP, IY".to_string(), 2),

        // Undocumented LD r, IYH/IYL and LD IYH/IYL, r
        0x40..=0x6F if (opcode & 0x07) != 6 && ((opcode >> 3) & 0x07) != 6 => {
            let dst = (opcode >> 3) & 0x07;
            let src = opcode & 0x07;
            (
                format!("LD {}, {}", iy_reg8_name(dst), iy_reg8_name(src)),
                2,
            )
        }

        // Undocumented arithmetic with IYH/IYL
        0x84 => ("ADD A, IYH".to_string(), 2),
        0x85 => ("ADD A, IYL".to_string(), 2),
        0x8C => ("ADC A, IYH".to_string(), 2),
        0x8D => ("ADC A, IYL".to_string(), 2),
        0x94 => ("SUB IYH".to_string(), 2),
        0x95 => ("SUB IYL".to_string(), 2),
        0x9C => ("SBC A, IYH".to_string(), 2),
        0x9D => ("SBC A, IYL".to_string(), 2),
        0xA4 => ("AND IYH".to_string(), 2),
        0xA5 => ("AND IYL".to_string(), 2),
        0xAC => ("XOR IYH".to_string(), 2),
        0xAD => ("XOR IYL".to_string(), 2),
        0xB4 => ("OR IYH".to_string(), 2),
        0xB5 => ("OR IYL".to_string(), 2),
        0xBC => ("CP IYH".to_string(), 2),
        0xBD => ("CP IYL".to_string(), 2),

        _ => (format!("DB $FD, ${:02X}", opcode), 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_disasm_nop() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0x00);
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "NOP");
        assert_eq!(size, 1);
    }

    #[test]
    fn test_disasm_ld_b_n() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0x06);
        ram.write(0x0001, 0x42);
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "LD B, $42");
        assert_eq!(size, 2);
    }

    #[test]
    fn test_disasm_ld_a_b() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0x78);
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "LD A, B");
        assert_eq!(size, 1);
    }

    #[test]
    fn test_disasm_halt() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0x76);
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "HALT");
        assert_eq!(size, 1);
    }

    #[test]
    fn test_disasm_daa() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0x27);
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "DAA");
        assert_eq!(size, 1);
    }

    #[test]
    fn test_disasm_cpl() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0x2F);
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "CPL");
        assert_eq!(size, 1);
    }

    #[test]
    fn test_disasm_ld_b_hl_indirect() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0x46); // LD B, (HL)
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "LD B, (HL)");
        assert_eq!(size, 1);
    }

    #[test]
    fn test_disasm_ld_hl_b() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0x70); // LD (HL), B
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "LD (HL), B");
        assert_eq!(size, 1);
    }

    #[test]
    fn test_disasm_ld_c_d() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0x4A); // LD C, D
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "LD C, D");
        assert_eq!(size, 1);
    }

    #[test]
    fn test_disasm_ed_ini() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0xED);
        ram.write(0x0001, 0xA2); // INI
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "INI");
        assert_eq!(size, 2);
    }

    #[test]
    fn test_disasm_ed_otir() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0xED);
        ram.write(0x0001, 0xB3); // OTIR
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "OTIR");
        assert_eq!(size, 2);
    }

    #[test]
    fn test_disasm_ed_ld_r_a() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0xED);
        ram.write(0x0001, 0x4F); // LD R, A
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "LD R, A");
        assert_eq!(size, 2);
    }

    #[test]
    fn test_disasm_dd_inc_ixh() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0xDD);
        ram.write(0x0001, 0x24); // INC IXH
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "INC IXH");
        assert_eq!(size, 2);
    }

    #[test]
    fn test_disasm_fd_ld_iyl_n() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0xFD);
        ram.write(0x0001, 0x2E); // LD IYL, n
        ram.write(0x0002, 0x42);
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "LD IYL, $42");
        assert_eq!(size, 3);
    }

    #[test]
    fn test_disasm_ld_b_ix_d_positive() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0xDD);
        ram.write(0x0001, 0x46);
        ram.write(0x0002, 0x05); // d = +5
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "LD B, (IX+5)");
        assert_eq!(size, 3);
    }

    #[test]
    fn test_disasm_ld_ix_d_a_negative() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0xDD);
        ram.write(0x0001, 0x77);
        ram.write(0x0002, 0xFC); // d = -4
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "LD (IX-4), A");
        assert_eq!(size, 3);
    }

    #[test]
    fn test_disasm_add_a_iy_d() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0xFD);
        ram.write(0x0001, 0x86);
        ram.write(0x0002, 0x0A); // d = +10
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "ADD A, (IY+10)");
        assert_eq!(size, 3);
    }

    #[test]
    fn test_disasm_ld_iy_d_n() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0xFD);
        ram.write(0x0001, 0x36);
        ram.write(0x0002, 0x02); // d = +2
        ram.write(0x0003, 0x42); // n = 0x42
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "LD (IY+2), $42");
        assert_eq!(size, 4);
    }

    #[test]
    fn test_disasm_inc_ix_d() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0xDD);
        ram.write(0x0001, 0x34);
        ram.write(0x0002, 0x00); // d = 0
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "INC (IX+0)");
        assert_eq!(size, 3);
    }

    #[test]
    fn test_disasm_xor_iy_d_large_negative() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0xFD);
        ram.write(0x0001, 0xAE);
        ram.write(0x0002, 0x80); // d = -128
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "XOR (IY-128)");
        assert_eq!(size, 3);
    }

    #[test]
    fn test_disasm_ddcb_rlc_ix_d() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0xDD);
        ram.write(0x0001, 0xCB);
        ram.write(0x0002, 0x05); // d = +5
        ram.write(0x0003, 0x06); // RLC (IX+d)
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "RLC (IX+5)");
        assert_eq!(size, 4);
    }

    #[test]
    fn test_disasm_ddcb_rlc_ix_d_store_b() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0000, 0xDD);
        ram.write(0x0001, 0xCB);
        ram.write(0x0002, 0x03); // d = +3
        ram.write(0x0003, 0x00); // RLC (IX+d), B (undocumented)
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0000);
        assert_eq!(mnemonic, "RLC (IX+3), B");
        assert_eq!(size, 4);
    }

    #[test]
    fn test_disasm_jr_target_addr() {
        let mut ram = Ram::new(0x10000);
        ram.write(0x0100, 0x18); // JR
        ram.write(0x0101, 0x05); // offset +5 -> target = 0x0102 + 5 = 0x0107
        let (mnemonic, size) = disassemble_at(&mut ram, 0x0100);
        assert_eq!(mnemonic, "JR $0107");
        assert_eq!(size, 2);
    }

    #[test]
    fn test_disasm_all_base_opcodes_no_db() {
        // Verify all 256 base opcodes produce a valid disassembly (not "DB $xx")
        // Skip prefix bytes 0xCB, 0xDD, 0xED, 0xFD - they need a second opcode byte
        let mut ram = Ram::new(0x10000);
        for op in 0..=255u8 {
            let base = (op as u16) * 4;
            ram.write(base as u32, op);
            ram.write((base + 1) as u32, 0x00);
            ram.write((base + 2) as u32, 0x00);
            ram.write((base + 3) as u32, 0x00);
        }

        for op in 0..=255u8 {
            // Skip prefix bytes
            if op == 0xCB || op == 0xDD || op == 0xED || op == 0xFD {
                continue;
            }
            let base = (op as u16) * 4;
            let (mnemonic, _) = disassemble_at(&mut ram, base);
            assert!(
                !mnemonic.starts_with("DB "),
                "Opcode 0x{:02X} produced fallback: {}",
                op,
                mnemonic
            );
        }
    }
}
