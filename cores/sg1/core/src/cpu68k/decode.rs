//! Instruction decoding for the Motorola 68000.

use super::addressing::{AddressingMode, EffectiveAddress, Size};
use super::prefetch::Prefetch;
use super::M68k;
use retro_core::cpu::Addressable;

/// Decode the size field from an instruction word.
/// Bits 7-6: 00=Byte, 01=Word, 10=Long
pub fn decode_size(size_bits: u8) -> Option<Size> {
    match size_bits & 0x3 {
        0b00 => Some(Size::Byte),
        0b01 => Some(Size::Word),
        0b10 => Some(Size::Long),
        _ => None, // Invalid
    }
}

/// Decode an effective address using an InstructionStream.
///
/// This version reads extension words from the stream, which automatically
/// tracks the position for Address Error exception handling.
/// Decode an effective address using the prefetch queue.
///
/// Reads extension words from the prefetch queue and advances its cursor.
/// This is the official decode_ea function - all new code should use this.
pub fn decode_ea(
    cpu: &M68k,
    bus: &mut dyn Addressable,
    prefetch: &mut Prefetch,
    mode: u8,
    reg: u8,
    size: Size,
) -> EffectiveAddress {
    let mode = mode & 0x7;
    let reg = reg & 0x7;

    match mode {
        0 => EffectiveAddress::new(AddressingMode::DataDirect(reg), 0),
        1 => EffectiveAddress::new(AddressingMode::AddressDirect(reg), 0),
        2 => {
            let cycles = match size {
                Size::Byte | Size::Word => 4,
                Size::Long => 8,
            };
            EffectiveAddress::new(AddressingMode::AddressIndirect(reg), cycles)
        }
        3 => {
            let cycles = match size {
                Size::Byte | Size::Word => 4,
                Size::Long => 8,
            };
            EffectiveAddress::new(AddressingMode::AddressIndirectPostInc(reg), cycles)
        }
        4 => {
            let cycles = match size {
                Size::Byte | Size::Word => 4,
                Size::Long => 8,
            };
            EffectiveAddress::new(AddressingMode::AddressIndirectPreDec(reg), cycles)
        }
        5 => {
            let disp = prefetch.read_word(bus) as i16;
            let cycles = match size {
                Size::Byte | Size::Word => 8,
                Size::Long => 12,
            };
            EffectiveAddress::new(
                AddressingMode::AddressIndirectDisplacement(reg, disp),
                cycles,
            )
        }
        6 => {
            let ext_word = prefetch.read_word(bus);
            let idx_reg = (ext_word >> 12) as u8;
            let idx_size = if (ext_word & 0x800) != 0 {
                Size::Long
            } else {
                Size::Word
            };
            let disp = (ext_word & 0xFF) as i8;
            let cycles = match size {
                Size::Byte | Size::Word => 10,
                Size::Long => 14,
            };
            EffectiveAddress::new(
                AddressingMode::AddressIndirectIndex(reg, idx_reg, idx_size, disp),
                cycles,
            )
        }
        7 => match reg {
            0 => {
                let addr = prefetch.read_word(bus);
                let cycles = match size {
                    Size::Byte | Size::Word => 8,
                    Size::Long => 12,
                };
                EffectiveAddress::new(AddressingMode::AbsoluteShort(addr), cycles)
            }
            1 => {
                let addr = prefetch.read_long(bus);
                let cycles = match size {
                    Size::Byte | Size::Word => 12,
                    Size::Long => 16,
                };
                EffectiveAddress::new(AddressingMode::AbsoluteLong(addr), cycles)
            }
            2 => {
                let disp = prefetch.read_word(bus) as i16;
                let cycles = match size {
                    Size::Byte | Size::Word => 8,
                    Size::Long => 12,
                };
                EffectiveAddress::new(AddressingMode::PcDisplacement(disp), cycles)
            }
            3 => {
                let ext_word = prefetch.read_word(bus);
                let idx_reg = (ext_word >> 12) as u8;
                let idx_size = if (ext_word & 0x800) != 0 {
                    Size::Long
                } else {
                    Size::Word
                };
                let disp = (ext_word & 0xFF) as i8;
                let cycles = match size {
                    Size::Byte | Size::Word => 10,
                    Size::Long => 14,
                };
                EffectiveAddress::new(AddressingMode::PcIndex(idx_reg, idx_size, disp), cycles)
            }
            4 => {
                let value = match size {
                    Size::Byte | Size::Word => prefetch.read_word(bus) as u32,
                    Size::Long => prefetch.read_long(bus),
                };
                let cycles = match size {
                    Size::Byte | Size::Word => 4,
                    Size::Long => 8,
                };
                EffectiveAddress::new(AddressingMode::Immediate(value), cycles)
            }
            _ => {
                cpu.illegal_instruction_pending.set(true);
                EffectiveAddress::new(AddressingMode::DataDirect(0), 0)
            }
        },
        _ => {
            cpu.illegal_instruction_pending.set(true);
            EffectiveAddress::new(AddressingMode::DataDirect(0), 0)
        }
    }
}

/// Decode effective address with accurate cycle accounting.
///
/// This function returns the internal CPU cycles (ALU operations, register updates)
/// separate from bus cycles. Bus cycles (memory reads/writes) are tracked automatically
/// by the bus via the CyclesTrackable trait.
///
/// **Cycle Accounting Model:**
/// - `ea.cycles`: Internal CPU operations (register arithmetic, ALU, etc.)
/// - `bus.cycles()`: Bus accesses (4 cycles per word read/write, tracked automatically)
/// - Total instruction cycles = ea.cycles + bus.cycles()
///
/// **Parameters:**
/// - `to_ea`: true if EA is destination (RMW operation), false if EA is source
///
/// **Timing varies by:**
/// - Addressing mode complexity
/// - Operation size (Byte/Word/Long)
/// - Direction (source vs destination for RMW)
///
pub fn decode_ea_with_cycles(
    cpu: &M68k,
    bus: &mut dyn Addressable,
    prefetch: &mut Prefetch,
    mode: u8,
    reg: u8,
    size: Size,
) -> EffectiveAddress {
    let mode = mode & 0x7;
    let reg = reg & 0x7;

    match mode {
        // Mode 0: Dn - Data Register Direct
        0 => {
            // Register access is fast, but Long operations need extra ALU cycles
            let cycles = match size {
                Size::Byte | Size::Word => 0, // Register access happens in parallel
                Size::Long => 0,              // 32-bit operation requires extra ALU time
            };
            EffectiveAddress::new(AddressingMode::DataDirect(reg), cycles)
        }
        // Mode 1: An - Address Register Direct
        1 => {
            let cycles = match size {
                Size::Byte | Size::Word => 0, // Register access in parallel
                Size::Long => 0,              // 32-bit operation
            };
            EffectiveAddress::new(AddressingMode::AddressDirect(reg), cycles)
        }
        // Mode 2: (An) - Address Register Indirect
        2 => {
            // Memory access via register pointer
            // Bus tracks read/write cycles automatically
            let cycles = match size {
                Size::Byte | Size::Word => 4, // No extra internal cycles
                Size::Long => 8,
            };
            EffectiveAddress::new(AddressingMode::AddressIndirect(reg), cycles)
        }
        // Mode 3: (An)+ - Address Register Indirect with Post-Increment
        3 => {
            // After access, increment An by operand size
            let cycles = match size {
                Size::Byte | Size::Word => 4, // Increment happens in parallel
                Size::Long => 8,
            };
            EffectiveAddress::new(AddressingMode::AddressIndirectPostInc(reg), cycles)
        }
        // Mode 4: -(An) - Address Register Indirect with Pre-Decrement
        4 => {
            // Decrement An BEFORE access
            // Pre-decrement requires extra cycles to update register first
            let cycles = match size {
                Size::Byte | Size::Word => 6, // Register update overhead
                Size::Long => 10,
            };
            EffectiveAddress::new(AddressingMode::AddressIndirectPreDec(reg), cycles)
        }
        // Mode 5: (d16,An) - Address Register Indirect with Displacement
        5 => {
            // Extension word (displacement) read by prefetch (bus tracks cycles)
            // Effective address = An + sign_extended(d16)
            let disp = prefetch.read_word(bus) as i16;
            let cycles = match size {
                Size::Byte | Size::Word => 8, // Address calculation in parallel
                Size::Long => 12,
            };
            EffectiveAddress::new(
                AddressingMode::AddressIndirectDisplacement(reg, disp),
                cycles,
            )
        }
        // Mode 6: (d8,An,Xn) - Address Register Indirect with Index
        6 => {
            // Extension word: index register, size, displacement
            // EA = An + Xn + sign_extended(d8)
            // More complex addressing = more cycles
            let ext_word = prefetch.read_word(bus);
            let idx_reg = (ext_word >> 12) as u8;
            let idx_size = if (ext_word & 0x800) != 0 {
                Size::Long
            } else {
                Size::Word
            };
            let disp = (ext_word & 0xFF) as i8;
            let cycles = match size {
                Size::Byte | Size::Word => 10, // Index calculation overhead
                Size::Long => 14,
            };
            EffectiveAddress::new(
                AddressingMode::AddressIndirectIndex(reg, idx_reg, idx_size, disp),
                cycles,
            )
        }
        // Mode 7: Special modes (determined by reg)
        7 => match reg {
            // Mode 7, reg 0: (xxx).w - Absolute Short
            0 => {
                // 16-bit address in extension word (sign-extended to 24 bits)
                let addr = prefetch.read_word(bus);
                let cycles = match size {
                    Size::Byte | Size::Word => 8, // Address from extension word
                    Size::Long => 12,
                };
                EffectiveAddress::new(AddressingMode::AbsoluteShort(addr), cycles)
            }
            // Mode 7, reg 1: (xxx).l - Absolute Long
            1 => {
                // 32-bit address in two extension words
                let addr = prefetch.read_long(bus);
                let cycles = match size {
                    Size::Byte | Size::Word => 12, // Address from extension words
                    Size::Long => 16,
                };
                EffectiveAddress::new(AddressingMode::AbsoluteLong(addr), cycles)
            }
            // Mode 7, reg 2: (d16,PC) - PC with Displacement
            2 => {
                // Extension word (displacement) relative to PC
                // EA = PC + sign_extended(d16)
                let disp = prefetch.read_word(bus) as i16;
                let cycles = match size {
                    Size::Byte | Size::Word => 8, // PC-relative calculation
                    Size::Long => 12,
                };
                EffectiveAddress::new(AddressingMode::PcDisplacement(disp), cycles)
            }
            // Mode 7, reg 3: (d8,PC,Xn) - PC with Index
            3 => {
                // Extension word: index register, size, displacement
                // EA = PC + Xn + sign_extended(d8)
                let ext_word = prefetch.read_word(bus);
                let idx_reg = (ext_word >> 12) as u8;
                let idx_size = if (ext_word & 0x800) != 0 {
                    Size::Long
                } else {
                    Size::Word
                };
                let disp = (ext_word & 0xFF) as i8;
                let cycles = match size {
                    Size::Byte | Size::Word => 10, // PC + index calculation
                    Size::Long => 14,              // More complex for Long
                };
                EffectiveAddress::new(AddressingMode::PcIndex(idx_reg, idx_size, disp), cycles)
            }
            // Mode 7, reg 4: #imm - Immediate
            4 => {
                // Immediate value in extension word(s)
                let value = match size {
                    Size::Byte | Size::Word => prefetch.read_word(bus) as u32,
                    Size::Long => prefetch.read_long(bus),
                };
                let cycles = match size {
                    Size::Byte | Size::Word => 4, // Immediate from extension word
                    Size::Long => 8,              // Two extension words = more time
                };
                EffectiveAddress::new(AddressingMode::Immediate(value), cycles)
            }
            _ => {
                cpu.illegal_instruction_pending.set(true);
                EffectiveAddress::new(AddressingMode::DataDirect(0), 0)
            }
        },
        _ => {
            cpu.illegal_instruction_pending.set(true);
            EffectiveAddress::new(AddressingMode::DataDirect(0), 0)
        }
    }
}

pub fn decode_ea_with_cycles_for_move(
    cpu: &M68k,
    bus: &mut dyn Addressable,
    prefetch: &mut Prefetch,
    mode: u8,
    reg: u8,
    size: Size,
    to_ea: bool,
) -> EffectiveAddress {
    let mode = mode & 0x7;
    let reg = reg & 0x7;

    match mode {
        // Mode 0: Dn - Data Register Direct
        0 => {
            // Register access is fast, but Long operations need extra ALU cycles
            let cycles = match size {
                Size::Byte | Size::Word => 0, // Register access happens in parallel
                Size::Long => 0,              // 32-bit operation requires extra ALU time
            };
            EffectiveAddress::new(AddressingMode::DataDirect(reg), cycles)
        }
        // Mode 1: An - Address Register Direct
        1 => {
            let cycles = match size {
                Size::Byte | Size::Word => 0, // Register access in parallel
                Size::Long => 0,              // 32-bit operation
            };
            EffectiveAddress::new(AddressingMode::AddressDirect(reg), cycles)
        }
        // Mode 2: (An) - Address Register Indirect
        2 => {
            // Memory access via register pointer
            // Bus tracks read/write cycles automatically
            let cycles = match size {
                Size::Byte | Size::Word => 4, // No extra internal cycles
                Size::Long => 0,
            };

            let cycles_exception = match size {
                Size::Byte | Size::Word => 8, // Address calculation in parallel
                Size::Long => 4,
            };

            EffectiveAddress::new_with_exception(
                AddressingMode::AddressIndirect(reg),
                cycles,
                cycles_exception,
            )
        }
        // Mode 3: (An)+ - Address Register Indirect with Post-Increment
        3 => {
            // After access, increment An by operand size
            let cycles = match size {
                Size::Byte | Size::Word => 2, // Increment happens in parallel
                Size::Long => 2,
            };

            let cycles_exception = match size {
                Size::Byte | Size::Word => 8, // Address calculation in parallel
                Size::Long => 4,
            };

            EffectiveAddress::new_with_exception(
                AddressingMode::AddressIndirectPostInc(reg),
                cycles,
                cycles_exception,
            )
        }
        // Mode 4: -(An) - Address Register Indirect with Pre-Decrement
        4 => {
            // Decrement An BEFORE access
            // Pre-decrement requires extra cycles to update register first
            let cycles = match size {
                Size::Byte | Size::Word => 2, // Register update overhead
                Size::Long => 2,
            };

            let cycles_exception = match size {
                Size::Byte | Size::Word => 8, // Address calculation in parallel
                Size::Long => 6,
            };

            EffectiveAddress::new_with_exception(
                AddressingMode::AddressIndirectPreDec(reg),
                cycles,
                cycles_exception,
            )
        }
        // Mode 5: (d16,An) - Address Register Indirect with Displacement
        5 => {
            // Extension word (displacement) read by prefetch (bus tracks cycles)
            // Effective address = An + sign_extended(d16)
            let disp = prefetch.read_word(bus) as i16;
            let cycles_exception = match size {
                Size::Byte | Size::Word => 8, // Address calculation in parallel
                Size::Long => {
                    if to_ea {
                        12
                    } else {
                        10
                    }
                }
            };

            let cycles = match size {
                Size::Byte | Size::Word => 8, // Address calculation in parallel
                Size::Long => {
                    if to_ea {
                        10
                    } else {
                        8
                    }
                }
            };

            EffectiveAddress::new_with_exception(
                AddressingMode::AddressIndirectDisplacement(reg, disp),
                cycles,
                cycles_exception,
            )
        }
        // Mode 6: (d8,An,Xn) - Address Register Indirect with Index
        6 => {
            // Extension word: index register, size, displacement
            // EA = An + Xn + sign_extended(d8)
            // More complex addressing = more cycles
            let ext_word = prefetch.read_word(bus);
            let idx_reg = (ext_word >> 12) as u8;
            let idx_size = if (ext_word & 0x800) != 0 {
                Size::Long
            } else {
                Size::Word
            };
            let disp = (ext_word & 0xFF) as i8;

            let cycles_exception = match size {
                Size::Byte | Size::Word => 8, // Address calculation in parallel
                Size::Long => {
                    if to_ea {
                        8
                    } else {
                        2
                    }
                }
            };

            let cycles = match size {
                Size::Byte | Size::Word => 10, // Index calculation overhead
                Size::Long => 2,
            };
            EffectiveAddress::new_with_exception(
                AddressingMode::AddressIndirectIndex(reg, idx_reg, idx_size, disp),
                cycles,
                cycles_exception,
            )
        }
        // Mode 7: Special modes (determined by reg)
        7 => match reg {
            // Mode 7, reg 0: (xxx).w - Absolute Short
            0 => {
                // 16-bit address in extension word (sign-extended to 24 bits)
                let addr = prefetch.read_word(bus);
                let cycles = match size {
                    Size::Byte | Size::Word => 8, // Address from extension word
                    Size::Long => {
                        if to_ea {
                            8
                        } else {
                            6
                        }
                    }
                };
                EffectiveAddress::new(AddressingMode::AbsoluteShort(addr), cycles)
            }
            // Mode 7, reg 1: (xxx).l - Absolute Long
            1 => {
                // 32-bit address in two extension words
                let addr = prefetch.read_long(bus);
                let cycles = match size {
                    Size::Byte | Size::Word => 12, // Address from extension words
                    Size::Long => {
                        if to_ea {
                            10
                        } else {
                            8
                        }
                    }
                };

                let cycles_exception = match size {
                    Size::Byte | Size::Word => 8, // Address calculation in parallel
                    Size::Long => 4,
                };

                EffectiveAddress::new_with_exception(
                    AddressingMode::AbsoluteLong(addr),
                    cycles,
                    cycles_exception,
                )
            }
            // Mode 7, reg 2: (d16,PC) - PC with Displacement
            2 => {
                // Extension word (displacement) relative to PC
                // EA = PC + sign_extended(d16)
                let disp = prefetch.read_word(bus) as i16;
                let cycles = match size {
                    Size::Byte | Size::Word => 8, // PC-relative calculation
                    Size::Long => 12,
                };
                EffectiveAddress::new(AddressingMode::PcDisplacement(disp), cycles)
            }
            // Mode 7, reg 3: (d8,PC,Xn) - PC with Index
            3 => {
                // Extension word: index register, size, displacement
                // EA = PC + Xn + sign_extended(d8)
                let ext_word = prefetch.read_word(bus);
                let idx_reg = (ext_word >> 12) as u8;
                let idx_size = if (ext_word & 0x800) != 0 {
                    Size::Long
                } else {
                    Size::Word
                };
                let disp = (ext_word & 0xFF) as i8;

                let cycles_exception = match size {
                    Size::Byte | Size::Word => 8, // Address calculation in parallel
                    Size::Long => 2,
                };

                let cycles = match size {
                    Size::Byte | Size::Word => 8, // Address calculation in parallel
                    Size::Long => 2,
                };

                EffectiveAddress::new_with_exception(
                    AddressingMode::PcIndex(idx_reg, idx_size, disp),
                    cycles,
                    cycles_exception,
                )
            }
            // Mode 7, reg 4: #imm - Immediate
            4 => {
                // Immediate value in extension word(s)
                let value = match size {
                    Size::Byte | Size::Word => prefetch.read_word(bus) as u32,
                    Size::Long => prefetch.read_long(bus),
                };
                let cycles = match size {
                    Size::Byte | Size::Word => 4, // Immediate from extension word
                    Size::Long => 8,              // Two extension words = more time
                };
                EffectiveAddress::new(AddressingMode::Immediate(value), cycles)
            }
            _ => {
                cpu.illegal_instruction_pending.set(true);
                EffectiveAddress::new(AddressingMode::DataDirect(0), 0)
            }
        },
        _ => {
            cpu.illegal_instruction_pending.set(true);
            EffectiveAddress::new(AddressingMode::DataDirect(0), 0)
        }
    }
}

/// LEGACY: Decode an effective address from mode and register fields.
///
/// Returns the EffectiveAddress and the number of extension words consumed (in bytes).
/// The caller is responsible for advancing PC by the returned extension word count.
///
/// Reads extension words from `cpu.pc`.
/// Also increments `cpu.extension_bytes_read` for Address Error tracking.
///
/// NOTE: This is the legacy version. New code should use `decode_ea`.
pub fn decode_ea_legacy(
    cpu: &mut M68k,
    bus: &mut dyn Addressable,
    mode: u8,
    reg: u8,
    size: Size,
) -> (EffectiveAddress, u32) {
    decode_ea_at_legacy(cpu, bus, cpu.pc, mode, reg, size)
}

/// LEGACY: Decode an effective address, reading extension words from a specific address.
///
/// This is useful when the instruction has other data (like immediates) between
/// the opcode and the EA extension words.
/// Also increments `cpu.extension_bytes_read` for Address Error tracking.
///
/// NOTE: This is the legacy version. New code should use prefetch system.
pub fn decode_ea_at_legacy(
    cpu: &mut M68k,
    bus: &mut dyn Addressable,
    ext_addr: u32,
    mode: u8,
    reg: u8,
    size: Size,
) -> (EffectiveAddress, u32) {
    let mode = mode & 0x7;
    let reg = reg & 0x7;

    match mode {
        0 => {
            // Dn - Data register direct
            (EffectiveAddress::new(AddressingMode::DataDirect(reg), 0), 0)
        }
        1 => {
            // An - Address register direct
            (
                EffectiveAddress::new(AddressingMode::AddressDirect(reg), 0),
                0,
            )
        }
        2 => {
            // (An) - Address register indirect
            let cycles = match size {
                Size::Byte | Size::Word => 4,
                Size::Long => 8,
            };
            (
                EffectiveAddress::new(AddressingMode::AddressIndirect(reg), cycles),
                0,
            )
        }
        3 => {
            // (An)+ - Address register indirect with post-increment
            let cycles = match size {
                Size::Byte | Size::Word => 4,
                Size::Long => 8,
            };
            (
                EffectiveAddress::new(AddressingMode::AddressIndirectPostInc(reg), cycles),
                0,
            )
        }
        4 => {
            // -(An) - Address register indirect with pre-decrement
            let cycles = match size {
                Size::Byte | Size::Word => 6,
                Size::Long => 10,
            };
            (
                EffectiveAddress::new(AddressingMode::AddressIndirectPreDec(reg), cycles),
                0,
            )
        }
        5 => {
            // d16(An) - Address register indirect with displacement
            let disp = bus.read_word(ext_addr) as i16;
            cpu.extension_bytes_read += 2;
            let cycles = match size {
                Size::Byte | Size::Word => 8,
                Size::Long => 12,
            };
            (
                EffectiveAddress::new(
                    AddressingMode::AddressIndirectDisplacement(reg, disp),
                    cycles,
                ),
                2,
            )
        }
        6 => {
            // d8(An,Xn) - Address register indirect with index
            let ext_word = bus.read_word(ext_addr);
            cpu.extension_bytes_read += 2;
            let idx_reg = (ext_word >> 12) as u8;
            let idx_size = if (ext_word & 0x800) != 0 {
                Size::Long
            } else {
                Size::Word
            };
            let disp = (ext_word & 0xFF) as i8;
            let cycles = match size {
                Size::Byte | Size::Word => 10,
                Size::Long => 14,
            };
            (
                EffectiveAddress::new(
                    AddressingMode::AddressIndirectIndex(reg, idx_reg, idx_size, disp),
                    cycles,
                ),
                2,
            )
        }
        7 => {
            // Special modes based on reg field
            match reg {
                0 => {
                    // xxx.W - Absolute short
                    let addr = bus.read_word(ext_addr);
                    cpu.extension_bytes_read += 2;
                    let cycles = match size {
                        Size::Byte | Size::Word => 8,
                        Size::Long => 12,
                    };
                    (
                        EffectiveAddress::new(AddressingMode::AbsoluteShort(addr), cycles),
                        2,
                    )
                }
                1 => {
                    // xxx.L - Absolute long
                    let addr_high = bus.read_word(ext_addr) as u32;
                    let addr_low = bus.read_word(ext_addr + 2) as u32;
                    cpu.extension_bytes_read += 4;
                    let addr = (addr_high << 16) | addr_low;
                    let cycles = match size {
                        Size::Byte | Size::Word => 12,
                        Size::Long => 16,
                    };
                    (
                        EffectiveAddress::new(AddressingMode::AbsoluteLong(addr), cycles),
                        4,
                    )
                }
                2 => {
                    // d16(PC) - PC with displacement
                    let disp = bus.read_word(ext_addr) as i16;
                    cpu.extension_bytes_read += 2;
                    let cycles = match size {
                        Size::Byte | Size::Word => 8,
                        Size::Long => 12,
                    };
                    (
                        EffectiveAddress::new(AddressingMode::PcDisplacement(disp), cycles),
                        2,
                    )
                }
                3 => {
                    // d8(PC,Xn) - PC with index
                    let ext_word = bus.read_word(ext_addr);
                    cpu.extension_bytes_read += 2;
                    let idx_reg = (ext_word >> 12) as u8;
                    let idx_size = if (ext_word & 0x800) != 0 {
                        Size::Long
                    } else {
                        Size::Word
                    };
                    let disp = (ext_word & 0xFF) as i8;
                    let cycles = match size {
                        Size::Byte | Size::Word => 10,
                        Size::Long => 14,
                    };
                    (
                        EffectiveAddress::new(
                            AddressingMode::PcIndex(idx_reg, idx_size, disp),
                            cycles,
                        ),
                        2,
                    )
                }
                4 => {
                    // #<data> - Immediate
                    let (value, ext_words) = match size {
                        Size::Byte | Size::Word => {
                            let v = bus.read_word(ext_addr) as u32;
                            cpu.extension_bytes_read += 2;
                            (v, 2)
                        }
                        Size::Long => {
                            let high = bus.read_word(ext_addr) as u32;
                            let low = bus.read_word(ext_addr + 2) as u32;
                            cpu.extension_bytes_read += 4;
                            ((high << 16) | low, 4)
                        }
                    };
                    let cycles = match size {
                        Size::Byte | Size::Word => 4,
                        Size::Long => 8,
                    };
                    (
                        EffectiveAddress::new(AddressingMode::Immediate(value), cycles),
                        ext_words,
                    )
                }
                _ => {
                    // Invalid addressing mode
                    cpu.illegal_instruction_pending.set(true);
                    (EffectiveAddress::new(AddressingMode::DataDirect(0), 0), 0)
                }
            }
        }
        _ => {
            cpu.illegal_instruction_pending.set(true);
            (EffectiveAddress::new(AddressingMode::DataDirect(0), 0), 0)
        }
    }
}

/// Extract bit fields from an instruction word using common patterns.
pub mod bit_extract {
    /// Extract bits 15-12 (top nibble).
    pub fn bits_15_12(word: u16) -> u8 {
        ((word >> 12) & 0xF) as u8
    }

    /// Extract bits 11-9 (register field).
    pub fn bits_11_9(word: u16) -> u8 {
        ((word >> 9) & 0x7) as u8
    }

    /// Extract bits 11-8 (condition code for branches).
    pub fn bits_11_8(word: u16) -> u8 {
        ((word >> 8) & 0xF) as u8
    }

    /// Extract bits 8-6 (mode field or size field).
    pub fn bits_8_6(word: u16) -> u8 {
        ((word >> 6) & 0x7) as u8
    }

    /// Extract bits 7-6 (size field).
    pub fn bits_7_6(word: u16) -> u8 {
        ((word >> 6) & 0x3) as u8
    }

    /// Extract bits 5-3 (mode field).
    pub fn bits_5_3(word: u16) -> u8 {
        ((word >> 3) & 0x7) as u8
    }

    /// Extract bits 2-0 (register field).
    pub fn bits_2_0(word: u16) -> u8 {
        (word & 0x7) as u8
    }

    /// Extract bits 11-0 (12-bit immediate/displacement).
    pub fn bits_11_0(word: u16) -> u16 {
        word & 0xFFF
    }

    /// Extract bits 7-0 (8-bit immediate/displacement).
    pub fn bits_7_0(word: u16) -> u8 {
        (word & 0xFF) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_decode_size() {
        assert_eq!(decode_size(0b00), Some(Size::Byte));
        assert_eq!(decode_size(0b01), Some(Size::Word));
        assert_eq!(decode_size(0b10), Some(Size::Long));
        assert_eq!(decode_size(0b11), None);
    }

    #[test]
    fn test_decode_data_direct() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x1000);

        let (ea, ext_words) = decode_ea_legacy(&mut cpu, &mut ram, 0, 3, Size::Word);
        assert_eq!(ext_words, 0);
        assert_eq!(ea.cycles, 0);
        match ea.mode {
            AddressingMode::DataDirect(reg) => assert_eq!(reg, 3),
            _ => panic!("Wrong mode"),
        }
    }

    #[test]
    fn test_decode_address_indirect() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x1000);

        let (ea, ext_words) = decode_ea_legacy(&mut cpu, &mut ram, 2, 5, Size::Long);
        assert_eq!(ext_words, 0);
        assert_eq!(ea.cycles, 8);
        match ea.mode {
            AddressingMode::AddressIndirect(reg) => assert_eq!(reg, 5),
            _ => panic!("Wrong mode"),
        }
    }

    #[test]
    fn test_decode_immediate() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x1000);

        cpu.pc = 0x100;
        ram.write_word(0x100, 0x1234);

        let (ea, ext_words) = decode_ea_legacy(&mut cpu, &mut ram, 7, 4, Size::Word);
        assert_eq!(ext_words, 2);
        assert_eq!(ea.cycles, 4);
        match ea.mode {
            AddressingMode::Immediate(value) => assert_eq!(value, 0x1234),
            _ => panic!("Wrong mode"),
        }
        // Note: decode_ea does NOT advance PC - caller is responsible
        // But it DOES track extension_bytes_read for Address Error handling
        assert_eq!(cpu.extension_bytes_read, 2);
    }

    #[test]
    fn test_bit_extract() {
        let word: u16 = 0b1010_0111_0010_1001;

        assert_eq!(bit_extract::bits_15_12(word), 0b1010);
        assert_eq!(bit_extract::bits_11_9(word), 0b011);
        assert_eq!(bit_extract::bits_8_6(word), 0b100);
        assert_eq!(bit_extract::bits_5_3(word), 0b101);
        assert_eq!(bit_extract::bits_2_0(word), 0b001);
    }
}
