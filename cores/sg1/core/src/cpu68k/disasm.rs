//! 68000 Disassembler
//!
//! Converts raw opcodes back into human-readable assembly instructions.

use super::addressing::Size;
use super::decode;
use retro_core::cpu::Addressable;

/// Minimal instruction stream cursor for disassembly.
///
/// Mirrors the core's notion of "prefetch after opcode": base starts at `addr + 2`
/// and `consumed()` tracks how many bytes of extension/immediate data were read.
struct DisasmCursor<'a> {
    bus: &'a mut dyn Addressable,
    base: u32,
    cursor: u32,
    bytes: Vec<u8>,
    ea_comment: Option<String>,
}

impl<'a> DisasmCursor<'a> {
    #[inline]
    fn new(bus: &'a mut dyn Addressable, instruction_addr: u32, opcode: u16) -> Self {
        Self {
            bus,
            base: instruction_addr.wrapping_add(2),
            cursor: 0,
            bytes: vec![(opcode >> 8) as u8, opcode as u8],
            ea_comment: None,
        }
    }

    #[inline]
    fn addr(&self) -> u32 {
        self.base.wrapping_add(self.cursor)
    }

    #[inline]
    fn read_word(&mut self) -> u16 {
        let w = self.bus.read_word(self.addr());
        self.bytes.push((w >> 8) as u8);
        self.bytes.push(w as u8);
        self.cursor = self.cursor.wrapping_add(2);
        w
    }

    #[inline]
    fn read_long(&mut self) -> u32 {
        let hi = self.read_word() as u32;
        let lo = self.read_word() as u32;
        (hi << 16) | lo
    }

    #[inline]
    fn consumed(&self) -> u32 {
        self.cursor
    }

    #[inline]
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[inline]
    fn ea_comment(&self) -> Option<&str> {
        self.ea_comment.as_deref()
    }
}

/// Structured disassembly output for debugger UIs.
#[derive(Debug, Clone)]
pub struct DisasmParts {
    pub mnemonic: String,
    pub operands: String,
    pub comment: Option<String>,
    pub bytes: Vec<u8>,
    pub bytes_consumed: u32,
}

/// Disassemble a single instruction at the given address.
///
/// Returns (instruction_string, bytes_consumed).
pub fn disassemble_one(bus: &mut dyn Addressable, addr: u32) -> (String, u32) {
    let opcode = bus.read_word(addr);

    // Stream cursor after opcode (addr + 2). All extension/immediate reads should go through this.
    let mut cursor = DisasmCursor::new(bus, addr, opcode);

    disassemble_one_decode(opcode, addr, &mut cursor)
}

fn disassemble_one_decode(opcode: u16, addr: u32, cursor: &mut DisasmCursor) -> (String, u32) {
    // Extract common bit fields
    let bits_15_12 = (opcode >> 12) & 0xF;
    let bits_11_9 = (opcode >> 9) & 0x7;
    let bits_8_6 = (opcode >> 6) & 0x7;
    let bits_7_6 = (opcode >> 6) & 0x3;
    let bits_5_3 = (opcode >> 3) & 0x7;
    let bits_2_0 = opcode & 0x7;

    match bits_15_12 {
        // Data movement and immediate operations
        0x0 => {
            // Special SR/CCR operations
            if opcode == 0x003C {
                let imm = cursor.read_word();
                return (
                    format!("ORI     #${:02X}, CCR", imm & 0xFF),
                    2 + cursor.consumed(),
                );
            }
            if opcode == 0x007C {
                let imm = cursor.read_word();
                return (format!("ORI     #${:04X}, SR", imm), 2 + cursor.consumed());
            }
            if opcode == 0x023C {
                let imm = cursor.read_word();
                return (
                    format!("ANDI    #${:02X}, CCR", imm & 0xFF),
                    2 + cursor.consumed(),
                );
            }
            if opcode == 0x027C {
                let imm = cursor.read_word();
                return (format!("ANDI    #${:04X}, SR", imm), 2 + cursor.consumed());
            }
            if opcode == 0x0A3C {
                let imm = cursor.read_word();
                return (
                    format!("EORI    #${:02X}, CCR", imm & 0xFF),
                    2 + cursor.consumed(),
                );
            }
            if opcode == 0x0A7C {
                let imm = cursor.read_word();
                return (format!("EORI    #${:04X}, SR", imm), 2 + cursor.consumed());
            }

            // MOVEP (MUST be before bit operations!)
            if (opcode & 0xF138) == 0x0108 {
                let data_reg = bits_11_9 as u8;
                let addr_reg = bits_2_0 as u8;
                let to_memory = (opcode & 0x0080) != 0;
                // Bit 7 is the direction and bit 6 the size: 0 = Word, 1 = Long
                let size = if (opcode & 0x0040) != 0 {
                    Size::Long
                } else {
                    Size::Word
                };
                let disp = cursor.read_word() as i16;
                if to_memory {
                    return (
                        format!(
                            "MOVEP.{} D{}, ${:04X}(A{})",
                            size_suffix(size),
                            data_reg,
                            disp as u16,
                            addr_reg
                        ),
                        2 + cursor.consumed(),
                    );
                } else {
                    return (
                        format!(
                            "MOVEP.{} ${:04X}(A{}), D{}",
                            size_suffix(size),
                            disp as u16,
                            addr_reg,
                            data_reg
                        ),
                        2 + cursor.consumed(),
                    );
                }
            }

            // Bit operations (dynamic - register)
            if (opcode & 0xF100) == 0x0100 {
                let bit_reg = bits_11_9 as u8;
                let op = bits_7_6 as u8;
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Byte);
                let op_name = match op {
                    0 => "BTST",
                    1 => "BCHG",
                    2 => "BCLR",
                    3 => "BSET",
                    _ => "???",
                };
                return (
                    format!("{}    D{}, {}", op_name, bit_reg, ea_str),
                    2 + cursor.consumed(),
                );
            }

            // Bit operations (static - immediate)
            if (opcode & 0xFF00) == 0x0800 {
                let op = bits_7_6 as u8;
                let bit_num = cursor.read_word();
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Byte);
                let op_name = match op {
                    0 => "BTST",
                    1 => "BCHG",
                    2 => "BCLR",
                    3 => "BSET",
                    _ => "???",
                };
                return (
                    format!("{}    #${}, {}", op_name, bit_num & 0xFF, ea_str),
                    2 + cursor.consumed(),
                );
            }

            // Immediate operations (ORI, ANDI, SUBI, ADDI, EORI, CMPI)
            let size = decode::decode_size(bits_7_6 as u8).unwrap_or(Size::Word);
            let imm_str = match size {
                Size::Byte => format!("#${:02X}", cursor.read_word() & 0xFF),
                Size::Word => format!("#${:04X}", cursor.read_word()),
                Size::Long => format!("#${:08X}", cursor.read_long()),
            };
            let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, size);

            let op_name = match bits_11_9 {
                0x0 => "ORI",
                0x1 => "ANDI",
                0x2 => "SUBI",
                0x3 => "ADDI",
                0x5 => "EORI",
                0x6 => "CMPI",
                _ => "???",
            };

            (
                format!("{}.{} {}, {}", op_name, size_suffix(size), imm_str, ea_str),
                2 + cursor.consumed(),
            )
        }

        // MOVE.B
        0x1 => {
            let (dest_mode, dest_reg, src_mode, src_reg) = extract_move_operands(opcode);
            let src_str = format_ea(cursor, src_mode, src_reg, Size::Byte);
            let dest_str = format_ea(cursor, dest_mode, dest_reg, Size::Byte);
            (
                format!("MOVE.B  {}, {}", src_str, dest_str),
                2 + cursor.consumed(),
            )
        }

        // MOVE.L
        0x2 => {
            let (dest_mode, dest_reg, src_mode, src_reg) = extract_move_operands(opcode);

            // Check if destination is address register (MOVEA.L)
            if dest_mode == 1 {
                let src_str = format_ea(cursor, src_mode, src_reg, Size::Long);
                return (
                    format!("MOVEA.L {}, A{}", src_str, dest_reg),
                    2 + cursor.consumed(),
                );
            }

            let src_str = format_ea(cursor, src_mode, src_reg, Size::Long);
            let dest_str = format_ea(cursor, dest_mode, dest_reg, Size::Long);
            (
                format!("MOVE.L  {}, {}", src_str, dest_str),
                2 + cursor.consumed(),
            )
        }

        // MOVE.W
        0x3 => {
            let (dest_mode, dest_reg, src_mode, src_reg) = extract_move_operands(opcode);

            // Check if destination is address register (MOVEA.W)
            if dest_mode == 1 {
                let src_str = format_ea(cursor, src_mode, src_reg, Size::Word);
                return (
                    format!("MOVEA.W {}, A{}", src_str, dest_reg),
                    2 + cursor.consumed(),
                );
            }

            let src_str = format_ea(cursor, src_mode, src_reg, Size::Word);
            let dest_str = format_ea(cursor, dest_mode, dest_reg, Size::Word);
            (
                format!("MOVE.W  {}, {}", src_str, dest_str),
                2 + cursor.consumed(),
            )
        }

        // Miscellaneous
        0x4 => {
            // System Control Instructions
            if opcode == 0x4E71 {
                return ("NOP".to_string(), 2);
            }
            if opcode == 0x4E70 {
                return ("RESET".to_string(), 2);
            }
            if opcode == 0x4E72 {
                let imm = cursor.read_word();
                return (format!("STOP    #${:04X}", imm), 2 + cursor.consumed());
            }
            if opcode == 0x4E73 {
                return ("RTE".to_string(), 2);
            }
            if opcode == 0x4E75 {
                return ("RTS".to_string(), 2);
            }
            if opcode == 0x4E76 {
                return ("TRAPV".to_string(), 2);
            }
            if opcode == 0x4E77 {
                return ("RTR".to_string(), 2);
            }
            if opcode == 0x4AFC {
                return ("ILLEGAL".to_string(), 2);
            }
            if (opcode & 0xFFF0) == 0x4E40 {
                let vector = (opcode & 0xF) as u8;
                return (format!("TRAP    #{}", vector), 2);
            }

            // LINK
            if (opcode & 0xFFF8) == 0x4E50 {
                let reg = (opcode & 0x7) as u8;
                let disp = cursor.read_word() as i16;
                return (
                    format!("LINK    A{}, #${:04X}", reg, disp as u16),
                    2 + cursor.consumed(),
                );
            }

            // UNLK
            if (opcode & 0xFFF8) == 0x4E58 {
                let reg = (opcode & 0x7) as u8;
                return (format!("UNLK    A{}", reg), 2);
            }

            // MOVE USP
            if (opcode & 0xFFF0) == 0x4E60 {
                let reg = (opcode & 0x7) as u8;
                let dir = (opcode & 0x0008) != 0;
                if dir {
                    return (format!("MOVE    A{}, USP", reg), 2);
                } else {
                    return (format!("MOVE    USP, A{}", reg), 2);
                }
            }

            // JSR
            if (opcode & 0xFFC0) == 0x4E80 {
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Long);
                return (format!("JSR     {}", ea_str), 2 + cursor.consumed());
            }

            // JMP
            if (opcode & 0xFFC0) == 0x4EC0 {
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Long);
                return (format!("JMP     {}", ea_str), 2 + cursor.consumed());
            }

            // TAS
            if (opcode & 0xFFC0) == 0x4AC0 {
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Byte);
                return (format!("TAS     {}", ea_str), 2 + cursor.consumed());
            }

            // MOVE from SR (MUST be before NEGX!)
            if (opcode & 0xFFC0) == 0x40C0 {
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Word);
                return (format!("MOVE    SR, {}", ea_str), 2 + cursor.consumed());
            }

            // MOVE from CCR (MUST be before CLR!)
            if (opcode & 0xFFC0) == 0x42C0 {
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Word);
                return (format!("MOVE    CCR, {}", ea_str), 2 + cursor.consumed());
            }

            // MOVE to CCR (MUST be before NEG!)
            if (opcode & 0xFFC0) == 0x44C0 {
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Word);
                return (format!("MOVE    {}, CCR", ea_str), 2 + cursor.consumed());
            }

            // MOVE to SR (MUST be before NOT!)
            if (opcode & 0xFFC0) == 0x46C0 {
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Word);
                return (format!("MOVE    {}, SR", ea_str), 2 + cursor.consumed());
            }

            // TST
            if (opcode & 0xFF00) == 0x4A00 {
                let size = decode::decode_size(bits_8_6 as u8).unwrap_or(Size::Word);
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, size);
                return (
                    format!("TST.{}  {}", size_suffix(size), ea_str),
                    2 + cursor.consumed(),
                );
            }

            // NEG
            if (opcode & 0xFF00) == 0x4400 {
                let size = decode::decode_size(bits_8_6 as u8).unwrap_or(Size::Word);
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, size);
                return (
                    format!("NEG.{}  {}", size_suffix(size), ea_str),
                    2 + cursor.consumed(),
                );
            }

            // NEGX
            if (opcode & 0xFF00) == 0x4000 {
                let size = decode::decode_size(bits_8_6 as u8).unwrap_or(Size::Word);
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, size);
                return (
                    format!("NEGX.{} {}", size_suffix(size), ea_str),
                    2 + cursor.consumed(),
                );
            }

            // CLR
            if (opcode & 0xFF00) == 0x4200 {
                let size = decode::decode_size(bits_8_6 as u8).unwrap_or(Size::Word);
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, size);
                return (
                    format!("CLR.{}  {}", size_suffix(size), ea_str),
                    2 + cursor.consumed(),
                );
            }

            // NOT
            if (opcode & 0xFF00) == 0x4600 {
                let size = decode::decode_size(bits_8_6 as u8).unwrap_or(Size::Word);
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, size);
                return (
                    format!("NOT.{}  {}", size_suffix(size), ea_str),
                    2 + cursor.consumed(),
                );
            }

            // NBCD
            if (opcode & 0xFFC0) == 0x4800 {
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Byte);
                return (format!("NBCD    {}", ea_str), 2 + cursor.consumed());
            }

            // SWAP
            if (opcode & 0xFFF8) == 0x4840 {
                let reg = (opcode & 0x7) as u8;
                return (format!("SWAP    D{}", reg), 2);
            }

            // PEA
            if (opcode & 0xFFC0) == 0x4840 {
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Long);
                return (format!("PEA     {}", ea_str), 2 + cursor.consumed());
            }

            // EXT
            if (opcode & 0xFFF8) == 0x4880 {
                let reg = (opcode & 0x7) as u8;
                return (format!("EXT.W   D{}", reg), 2);
            }
            if (opcode & 0xFFF8) == 0x48C0 {
                let reg = (opcode & 0x7) as u8;
                return (format!("EXT.L   D{}", reg), 2);
            }

            // MOVEM
            if (opcode & 0xFB80) == 0x4880 {
                let dir = (opcode & 0x0400) != 0;
                let size = if (opcode & 0x0040) != 0 {
                    Size::Long
                } else {
                    Size::Word
                };
                let _mask = cursor.read_word(); // Register mask (not decoded in simple disasm)
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, size);

                if dir {
                    // Memory to registers
                    return (
                        format!("MOVEM.{} {}, <list>", size_suffix(size), ea_str),
                        2 + cursor.consumed(),
                    );
                } else {
                    // Registers to memory
                    return (
                        format!("MOVEM.{} <list>, {}", size_suffix(size), ea_str),
                        2 + cursor.consumed(),
                    );
                }
            }

            // LEA
            if (opcode & 0xF1C0) == 0x41C0 {
                let reg = ((opcode >> 9) & 0x7) as u8;
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Long);
                return (
                    format!("LEA     {}, A{}", ea_str, reg),
                    2 + cursor.consumed(),
                );
            }

            // CHK
            if (opcode & 0xF1C0) == 0x4180 {
                let reg = ((opcode >> 9) & 0x7) as u8;
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Word);
                return (
                    format!("CHK     {}, D{}", ea_str, reg),
                    2 + cursor.consumed(),
                );
            }

            (format!("DC.W    ${:04X}", opcode), 2)
        }

        // ADDQ/SUBQ / DBcc / Scc
        0x5 => {
            // DBcc
            if (opcode & 0xF0F8) == 0x50C8 {
                let cond = ((opcode >> 8) & 0xF) as u8;
                let reg = bits_2_0 as u8;
                let disp = cursor.read_word() as i16;
                // Displacement is relative to the address of the displacement word (addr + 2)
                let target = addr.wrapping_add(2).wrapping_add(disp as i32 as u32);
                let cond_name = condition_name(cond);
                return (
                    format!("DB{}    D{}, ${:08X}", cond_name, reg, target),
                    2 + cursor.consumed(),
                );
            }

            // Scc
            if (opcode & 0xF0C0) == 0x50C0 {
                let cond = ((opcode >> 8) & 0xF) as u8;
                let cond_name = condition_name(cond);
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Byte);
                return (
                    format!("S{}     {}", cond_name, ea_str),
                    2 + cursor.consumed(),
                );
            }

            // ADDQ/SUBQ
            let data = bits_11_9;
            let data_val = if data == 0 { 8 } else { data };
            let size = decode::decode_size(bits_8_6 as u8).unwrap_or(Size::Word);
            let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, size);

            if (opcode & 0x0100) == 0 {
                (
                    format!("ADDQ.{} #{}, {}", size_suffix(size), data_val, ea_str),
                    2 + cursor.consumed(),
                )
            } else {
                (
                    format!("SUBQ.{} #{}, {}", size_suffix(size), data_val, ea_str),
                    2 + cursor.consumed(),
                )
            }
        }

        // Bcc / BSR
        0x6 => {
            let cond = (opcode >> 8) & 0xF;
            let disp8_u8 = (opcode & 0xFF) as u8;

            // 68000: 0 means 16-bit displacement follows.
            let (target, bytes) = if disp8_u8 == 0x00 {
                let disp16 = cursor.read_word() as i16;
                // Displacement is relative to the address of the displacement word (addr + 2)
                let target = addr.wrapping_add(2).wrapping_add(disp16 as i32 as u32);
                (target, 2 + cursor.consumed())
            } else {
                let disp8 = disp8_u8 as i8;
                let target = addr.wrapping_add(2).wrapping_add(disp8 as i32 as u32);
                (target, 2 + cursor.consumed())
            };

            if cond == 0 {
                (format!("BRA     ${:08X}", target), bytes)
            } else if cond == 1 {
                (format!("BSR     ${:08X}", target), bytes)
            } else {
                let cond_name = condition_name(cond as u8);
                (format!("B{}     ${:08X}", cond_name, target), bytes)
            }
        }

        // MOVEQ
        0x7 => {
            let reg = bits_11_9;
            let data = (opcode & 0xFF) as i8;
            (format!("MOVEQ   #${:02X}, D{}", data as u8, reg), 2)
        }

        // OR / DIVU / DIVS / SBCD
        0x8 => {
            // DIVU
            if (opcode & 0xF1C0) == 0x80C0 {
                let reg = bits_11_9 as u8;
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Word);
                return (
                    format!("DIVU.W  {}, D{}", ea_str, reg),
                    2 + cursor.consumed(),
                );
            }

            // DIVS
            if (opcode & 0xF1C0) == 0x81C0 {
                let reg = bits_11_9 as u8;
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Word);
                return (
                    format!("DIVS.W  {}, D{}", ea_str, reg),
                    2 + cursor.consumed(),
                );
            }

            // SBCD
            if (opcode & 0xF1F8) == 0x8100 {
                let rx = bits_11_9 as u8;
                let ry = bits_2_0 as u8;
                if (opcode & 0x0008) == 0 {
                    return (format!("SBCD    D{}, D{}", ry, rx), 2);
                } else {
                    return (format!("SBCD    -(A{}), -(A{})", ry, rx), 2);
                }
            }

            // OR
            let reg = bits_11_9;
            let size = decode::decode_size(bits_8_6 as u8).unwrap_or(Size::Word);
            let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, size);

            if (opcode & 0x0100) == 0 {
                // EA | Dn -> Dn
                (
                    format!("OR.{}   {}, D{}", size_suffix(size), ea_str, reg),
                    2 + cursor.consumed(),
                )
            } else {
                // Dn | EA -> EA
                (
                    format!("OR.{}   D{}, {}", size_suffix(size), reg, ea_str),
                    2 + cursor.consumed(),
                )
            }
        }

        // SUB / SUBA / SUBX
        0x9 => {
            // SUBA
            if (opcode & 0xF0C0) == 0x90C0 {
                let reg = bits_11_9 as u8;
                let size = if (opcode & 0x0100) != 0 {
                    Size::Long
                } else {
                    Size::Word
                };
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, size);
                return (
                    format!("SUBA.{} {}, A{}", size_suffix(size), ea_str, reg),
                    2 + cursor.consumed(),
                );
            }

            // SUBX
            if (opcode & 0xF130) == 0x9100 {
                let rx = bits_11_9 as u8;
                let ry = bits_2_0 as u8;
                let size = decode::decode_size(bits_8_6 as u8).unwrap_or(Size::Word);
                if (opcode & 0x0008) == 0 {
                    return (format!("SUBX.{} D{}, D{}", size_suffix(size), ry, rx), 2);
                } else {
                    return (
                        format!("SUBX.{} -(A{}), -(A{})", size_suffix(size), ry, rx),
                        2,
                    );
                }
            }

            // SUB
            let reg = bits_11_9;
            let size = decode::decode_size(bits_8_6 as u8).unwrap_or(Size::Word);
            let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, size);

            if (opcode & 0x0100) == 0 {
                // <ea> - Dn -> Dn
                (
                    format!("SUB.{}  {}, D{}", size_suffix(size), ea_str, reg),
                    2 + cursor.consumed(),
                )
            } else {
                // Dn - <ea> -> <ea>
                (
                    format!("SUB.{}  D{}, {}", size_suffix(size), reg, ea_str),
                    2 + cursor.consumed(),
                )
            }
        }

        // CMP / EOR / CMPA / CMPM
        0xB => {
            // CMPA
            if (opcode & 0xF0C0) == 0xB0C0 {
                let reg = bits_11_9 as u8;
                let size = if (opcode & 0x0100) != 0 {
                    Size::Long
                } else {
                    Size::Word
                };
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, size);
                return (
                    format!("CMPA.{} {}, A{}", size_suffix(size), ea_str, reg),
                    2 + cursor.consumed(),
                );
            }

            // CMPM
            if (opcode & 0xF138) == 0xB108 {
                let rx = bits_11_9 as u8;
                let ry = bits_2_0 as u8;
                let size = decode::decode_size(bits_8_6 as u8).unwrap_or(Size::Word);
                return (
                    format!("CMPM.{} (A{})+, (A{})+", size_suffix(size), ry, rx),
                    2,
                );
            }

            // CMP / EOR
            let reg = bits_11_9;
            let size = decode::decode_size(bits_8_6 as u8).unwrap_or(Size::Word);
            let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, size);

            if (opcode & 0x0100) == 0 {
                // CMP
                (
                    format!("CMP.{}  {}, D{}", size_suffix(size), ea_str, reg),
                    2 + cursor.consumed(),
                )
            } else {
                // EOR
                (
                    format!("EOR.{}  D{}, {}", size_suffix(size), reg, ea_str),
                    2 + cursor.consumed(),
                )
            }
        }

        // AND / MUL / MULU / MULS / ABCD / EXG
        0xC => {
            // MULU
            if (opcode & 0xF1C0) == 0xC0C0 {
                let reg = bits_11_9 as u8;
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Word);
                return (
                    format!("MULU.W  {}, D{}", ea_str, reg),
                    2 + cursor.consumed(),
                );
            }

            // MULS
            if (opcode & 0xF1C0) == 0xC1C0 {
                let reg = bits_11_9 as u8;
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, Size::Word);
                return (
                    format!("MULS.W  {}, D{}", ea_str, reg),
                    2 + cursor.consumed(),
                );
            }

            // ABCD
            if (opcode & 0xF1F8) == 0xC100 {
                let rx = bits_11_9 as u8;
                let ry = bits_2_0 as u8;
                if (opcode & 0x0008) == 0 {
                    return (format!("ABCD    D{}, D{}", ry, rx), 2);
                } else {
                    return (format!("ABCD    -(A{}), -(A{})", ry, rx), 2);
                }
            }

            // EXG
            if (opcode & 0xF130) == 0xC100 {
                let rx = bits_11_9 as u8;
                let ry = bits_2_0 as u8;
                let opmode = ((opcode >> 3) & 0x1F) as u8;
                match opmode {
                    0x08 => return (format!("EXG     D{}, D{}", rx, ry), 2),
                    0x09 => return (format!("EXG     A{}, A{}", rx, ry), 2),
                    0x11 => return (format!("EXG     D{}, A{}", rx, ry), 2),
                    _ => {}
                }
            }

            // AND
            let reg = bits_11_9;
            let size = decode::decode_size(bits_8_6 as u8).unwrap_or(Size::Word);
            let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, size);

            if (opcode & 0x0100) == 0 {
                // EA & Dn -> Dn
                (
                    format!("AND.{}  {}, D{}", size_suffix(size), ea_str, reg),
                    2 + cursor.consumed(),
                )
            } else {
                // Dn & EA -> EA
                (
                    format!("AND.{}  D{}, {}", size_suffix(size), reg, ea_str),
                    2 + cursor.consumed(),
                )
            }
        }

        // ADD / ADDA / ADDX
        0xD => {
            // ADDA
            if (opcode & 0xF0C0) == 0xD0C0 {
                let reg = bits_11_9 as u8;
                let size = if (opcode & 0x0100) != 0 {
                    Size::Long
                } else {
                    Size::Word
                };
                let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, size);
                return (
                    format!("ADDA.{} {}, A{}", size_suffix(size), ea_str, reg),
                    2 + cursor.consumed(),
                );
            }

            // ADDX
            if (opcode & 0xF130) == 0xD100 {
                let rx = bits_11_9 as u8;
                let ry = bits_2_0 as u8;
                let size = decode::decode_size(bits_8_6 as u8).unwrap_or(Size::Word);
                if (opcode & 0x0008) == 0 {
                    return (format!("ADDX.{} D{}, D{}", size_suffix(size), ry, rx), 2);
                } else {
                    return (
                        format!("ADDX.{} -(A{}), -(A{})", size_suffix(size), ry, rx),
                        2,
                    );
                }
            }

            // ADD
            let reg = bits_11_9;
            let size = decode::decode_size(bits_8_6 as u8).unwrap_or(Size::Word);
            let ea_str = format_ea(cursor, bits_5_3 as u8, bits_2_0 as u8, size);

            if (opcode & 0x0100) == 0 {
                // <ea> + Dn -> Dn
                (
                    format!("ADD.{}  {}, D{}", size_suffix(size), ea_str, reg),
                    2 + cursor.consumed(),
                )
            } else {
                // Dn + <ea> -> <ea>
                (
                    format!("ADD.{}  D{}, {}", size_suffix(size), reg, ea_str),
                    2 + cursor.consumed(),
                )
            }
        }

        // Shifts
        0xE => {
            let count_reg = bits_11_9;
            let dir = (opcode >> 8) & 1; // 0=right, 1=left
            let size = decode::decode_size(bits_8_6 as u8).unwrap_or(Size::Word);
            let ir = (opcode >> 5) & 1; // 0=immediate, 1=register
            let reg = bits_2_0;

            let shift_type = match (opcode >> 3) & 0x3 {
                0 => "AS",
                1 => "LS",
                2 => "ROX",
                _ => "RO",
            };

            let dir_str = if dir == 0 { "R" } else { "L" };
            let count_str = if ir == 0 {
                format!("#{}", if count_reg == 0 { 8 } else { count_reg })
            } else {
                format!("D{}", count_reg)
            };

            (
                format!(
                    "{}{}.{} {}, D{}",
                    shift_type,
                    dir_str,
                    size_suffix(size),
                    count_str,
                    reg
                ),
                2,
            )
        }

        // Line-A (emulator) - generates Line-A exception on 68000
        0xA => (format!("LINEA   ${:04X}", opcode), 2),

        // Line-F (emulator) - generates Line-F exception on 68000
        0xF => (format!("LINEF   ${:04X}", opcode), 2),

        _ => (format!("DC.W    ${:04X}", opcode), 2),
    }
}

/// Disassemble a single instruction and return structured fields suitable for UI rendering.
///
/// This is layered on top of `disassemble_one()` for now: it keeps `bytes_consumed` consistent
/// with the legacy logic while allowing the debugger UI to show separate columns.
pub fn disassemble_one_parts(bus: &mut dyn Addressable, addr: u32) -> DisasmParts {
    let opcode = bus.read_word(addr);
    let mut cursor = DisasmCursor::new(bus, addr, opcode);
    let (line, bytes_consumed) = disassemble_one_decode(opcode, addr, &mut cursor);
    let mut bytes = cursor.bytes().to_vec();
    bytes.truncate(bytes_consumed as usize);

    // Split "MNEMONIC  operands" into (mnemonic, operands).
    let trimmed = line.trim();
    let (mnemonic, operands) = match trimmed.char_indices().find(|&(_, c)| c.is_whitespace()) {
        Some((idx, _)) => {
            let (m, rest) = trimmed.split_at(idx);
            (m.trim().to_string(), rest.trim().to_string())
        }
        None => (trimmed.to_string(), String::new()),
    };

    // Optional richer info for UI (show what we can).
    // For branches, provide the resolved target in `comment` as well.
    let branch_comment = if (opcode & 0xF000) == 0x6000 && bytes.len() >= 2 {
        let disp8_u8 = (opcode & 0x00FF) as u8;
        // Both 8-bit and 16-bit displacements are relative to addr + 2
        let target = if disp8_u8 == 0x00 && bytes.len() >= 4 {
            let disp16 = (((bytes[2] as u16) << 8) | (bytes[3] as u16)) as i16;
            addr.wrapping_add(2).wrapping_add(disp16 as i32 as u32)
        } else {
            let disp8 = disp8_u8 as i8;
            addr.wrapping_add(2).wrapping_add(disp8 as i32 as u32)
        } & 0x00FF_FFFF;
        Some(format!("target=${:06X}", target))
    } else {
        None
    };

    let dbcc_comment = if (opcode & 0xF0F8) == 0x50C8 && bytes.len() >= 4 {
        let disp16 = (((bytes[2] as u16) << 8) | (bytes[3] as u16)) as i16;
        // DBcc displacement is relative to the displacement word address (addr + 2)
        let target = addr.wrapping_add(2).wrapping_add(disp16 as i32 as u32) & 0x00FF_FFFF;
        Some(format!("target=${:06X}", target))
    } else {
        None
    };

    // Prefer explicit control-flow targets; else try PC-relative EA comment.
    let comment = branch_comment
        .or(dbcc_comment)
        .or_else(|| cursor.ea_comment().map(|s| s.to_string()));

    DisasmParts {
        mnemonic,
        operands,
        comment,
        bytes,
        bytes_consumed,
    }
}

/// Extract operands for MOVE instruction.
fn extract_move_operands(opcode: u16) -> (u8, u8, u8, u8) {
    let dest_reg = ((opcode >> 9) & 0x7) as u8;
    let dest_mode = ((opcode >> 6) & 0x7) as u8;
    let src_mode = ((opcode >> 3) & 0x7) as u8;
    let src_reg = (opcode & 0x7) as u8;
    (dest_mode, dest_reg, src_mode, src_reg)
}

/// Format effective address as string.
fn format_ea(cursor: &mut DisasmCursor, mode: u8, reg: u8, size: Size) -> String {
    match mode {
        0 => format!("D{}", reg),
        1 => format!("A{}", reg),
        2 => format!("(A{})", reg),
        3 => format!("(A{})+", reg),
        4 => format!("-(A{})", reg),
        5 => {
            let disp = cursor.read_word() as i16;
            format!("${:04X}(A{})", disp as u16, reg)
        }
        6 => {
            // d8(An,Xn)
            let ext = cursor.read_word();
            let idx_reg = (ext >> 12) as u8;
            let idx_is_a = (idx_reg & 0x8) != 0;
            let idx_num = idx_reg & 0x7;
            let idx_size = if (ext & 0x0800) != 0 { 'L' } else { 'W' };
            let disp = (ext & 0x00FF) as u8;
            let idx_name = if idx_is_a {
                format!("A{}", idx_num)
            } else {
                format!("D{}", idx_num)
            };
            format!("${:02X}(A{}, {}.{})", disp, reg, idx_name, idx_size)
        }
        7 => match reg {
            0 => {
                // (xxx).W
                let a = cursor.read_word();
                format!("${:04X}", a)
            }
            1 => {
                // (xxx).L
                let a = cursor.read_long();
                format!("${:08X}", a)
            }
            2 => {
                // d16(PC)
                let ext_addr = cursor.addr();
                let disp = cursor.read_word() as i16;
                // The 68000 adds the displacement to the extension word's
                // own address.
                let abs = ext_addr.wrapping_add(disp as i32 as u32) & 0x00FF_FFFF;
                cursor.ea_comment = Some(format!("ea=${:06X}", abs));
                format!("${:04X}(PC)", disp as u16)
            }
            3 => {
                // d8(PC,Xn)
                let ext_addr = cursor.addr();
                let ext = cursor.read_word();
                let idx_reg = (ext >> 12) as u8;
                let idx_is_a = (idx_reg & 0x8) != 0;
                let idx_num = idx_reg & 0x7;
                let idx_size = if (ext & 0x0800) != 0 { 'L' } else { 'W' };
                let disp_u8 = (ext & 0x00FF) as u8;
                let disp = disp_u8 as i8;
                // The base is the extension word's own address, as for d16(PC).
                // The index register's value is not known here, so only
                // base + displacement is given.
                let base = ext_addr.wrapping_add(disp as i32 as u32) & 0x00FF_FFFF;
                cursor.ea_comment = Some(format!("ea_base=${:06X}", base));
                let idx_name = if idx_is_a {
                    format!("A{}", idx_num)
                } else {
                    format!("D{}", idx_num)
                };
                format!("${:02X}(PC, {}.{})", disp_u8, idx_name, idx_size)
            }
            4 => {
                // #<data>
                let v = match size {
                    Size::Byte => cursor.read_word() as u32 & 0xFF,
                    Size::Word => cursor.read_word() as u32,
                    Size::Long => cursor.read_long(),
                };
                match size {
                    Size::Byte => format!("#${:02X}", v),
                    Size::Word => format!("#${:04X}", v as u16),
                    Size::Long => format!("#${:08X}", v),
                }
            }
            _ => "<?>".to_string(),
        },
        _ => format!("<mode{}>", mode),
    }
}

/// Get size suffix string.
fn size_suffix(size: Size) -> &'static str {
    match size {
        Size::Byte => "B",
        Size::Word => "W",
        Size::Long => "L",
    }
}

/// Get condition code name.
fn condition_name(cond: u8) -> &'static str {
    match cond {
        0x0 => "T",
        0x1 => "F",
        0x2 => "HI",
        0x3 => "LS",
        0x4 => "CC",
        0x5 => "CS",
        0x6 => "NE",
        0x7 => "EQ",
        0x8 => "VC",
        0x9 => "VS",
        0xA => "PL",
        0xB => "MI",
        0xC => "GE",
        0xD => "LT",
        0xE => "GT",
        0xF => "LE",
        _ => "??",
    }
}
