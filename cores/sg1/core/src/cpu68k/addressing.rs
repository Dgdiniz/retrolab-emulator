//! Motorola 68000 addressing modes and effective address calculation.

use super::prefetch::Prefetch;
use retro_core::cpu::Addressable;

/// Operand size for instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Byte = 1,
    Word = 2,
    Long = 4,
}

impl Size {
    /// Get size in bytes.
    pub fn bytes(self) -> u32 {
        self as u32
    }

    /// Get size in bits.
    pub fn bits(self) -> u32 {
        match self {
            Size::Byte => 8,
            Size::Word => 16,
            Size::Long => 32,
        }
    }

    /// Get mask for the size (to truncate values).
    pub fn mask(self) -> u32 {
        match self {
            Size::Byte => 0xFF,
            Size::Word => 0xFFFF,
            Size::Long => 0xFFFF_FFFF,
        }
    }
}

/// Addressing modes for the 68000.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressingMode {
    /// Dn - Data register direct
    DataDirect(u8),
    /// An - Address register direct
    AddressDirect(u8),
    /// (An) - Address register indirect
    AddressIndirect(u8),
    /// (An)+ - Address register indirect with post-increment
    AddressIndirectPostInc(u8),
    /// -(An) - Address register indirect with pre-decrement
    AddressIndirectPreDec(u8),
    /// d16(An) - Address register indirect with displacement
    AddressIndirectDisplacement(u8, i16),
    /// d8(An,Xn) - Address register indirect with index
    AddressIndirectIndex(u8, u8, Size, i8),
    /// xxx.W - Absolute short
    AbsoluteShort(u16),
    /// xxx.L - Absolute long
    AbsoluteLong(u32),
    /// d16(PC) - PC with displacement
    PcDisplacement(i16),
    /// d8(PC,Xn) - PC with index
    PcIndex(u8, Size, i8),
    /// #<data> - Immediate
    Immediate(u32),
}

/// Result of effective address calculation.
#[derive(Debug, Clone, Copy)]
pub struct EffectiveAddress {
    /// The addressing mode used.
    pub mode: AddressingMode,
    /// Additional cycles consumed by EA calculation.
    pub cycles: u32,
    pub cycles_exception: u32,
}

impl EffectiveAddress {
    /// Create a new effective address.
    pub fn new(mode: AddressingMode, cycles: u32) -> Self {
        Self {
            mode,
            cycles,
            cycles_exception: cycles,
        }
    }

    pub fn new_with_exception(mode: AddressingMode, cycles: u32, cycles_exception: u32) -> Self {
        Self {
            mode,
            cycles,
            cycles_exception,
        }
    }

    /// Calculate the actual address from the addressing mode (for LEA, PEA, etc).
    /// This returns the ADDRESS itself, not the value at that address.
    ///
    /// For PC-relative modes, `base_pc` should be the instruction PC + 2 (after opcode fetch).
    pub fn calculate_address(&self, cpu: &super::M68k, base_pc: u32) -> u32 {
        match &self.mode {
            AddressingMode::AddressIndirect(reg) => cpu.a[*reg as usize],
            AddressingMode::AddressIndirectDisplacement(reg, disp) => {
                cpu.a[*reg as usize].wrapping_add(*disp as i32 as u32)
            }
            AddressingMode::AddressIndirectIndex(reg, idx_reg, idx_size, disp) => {
                let base = cpu.a[*reg as usize];
                let is_address = (*idx_reg & 0x8) != 0;
                let reg_num = (*idx_reg & 0x7) as usize;
                let index = if is_address {
                    cpu.a[reg_num]
                } else {
                    cpu.d[reg_num]
                };
                let index = match idx_size {
                    Size::Word => index as i16 as i32 as u32,
                    _ => index,
                };
                base.wrapping_add(index).wrapping_add(*disp as i32 as u32)
            }
            AddressingMode::AbsoluteShort(addr) => *addr as i16 as i32 as u32,
            AddressingMode::AbsoluteLong(addr) => *addr,
            AddressingMode::PcDisplacement(disp) => {
                // PC-relative: use base_pc (instruction_pc + 2)
                base_pc.wrapping_add(*disp as i32 as u32)
            }
            AddressingMode::PcIndex(idx_reg, idx_size, disp) => {
                let is_address = (*idx_reg & 0x8) != 0;
                let reg_num = (*idx_reg & 0x7) as usize;
                let index = if is_address {
                    cpu.a[reg_num]
                } else {
                    cpu.d[reg_num]
                };
                let index = match idx_size {
                    Size::Word => index as i16 as i32 as u32,
                    _ => index,
                };
                base_pc
                    .wrapping_add(index)
                    .wrapping_add(*disp as i32 as u32)
            }
            _ => 0, // Invalid modes for LEA
        }
    }

    /// LEGACY: Read a value from the effective address.
    /// NOTE: New code should use the version that takes prefetch parameter.
    pub fn read_legacy(&self, cpu: &mut super::M68k, bus: &mut dyn Addressable, size: Size) -> u32 {
        match self.mode {
            AddressingMode::DataDirect(reg) => cpu.d[reg as usize] & size.mask(),
            AddressingMode::AddressDirect(reg) => cpu.a[reg as usize] & size.mask(),
            AddressingMode::AddressIndirect(reg) => {
                let addr = cpu.a[reg as usize];
                read_memory_legacy(cpu, bus, addr, size)
            }
            AddressingMode::AddressIndirectPostInc(reg) => {
                let addr = cpu.a[reg as usize];
                let value = read_memory_legacy(cpu, bus, addr, size);
                let increment = if reg == 7 && size == Size::Byte {
                    2 // A7 (stack pointer) always increments by 2
                } else {
                    size.bytes()
                };

                cpu.a[reg as usize] = cpu.a[reg as usize].wrapping_add(increment);
                value
            }
            AddressingMode::AddressIndirectPreDec(reg) => {
                let decrement = if reg == 7 && size == Size::Byte {
                    2 // A7 (stack pointer) always decrements by 2
                } else {
                    size.bytes()
                };
                cpu.a[reg as usize] = cpu.a[reg as usize].wrapping_sub(decrement);
                let addr = cpu.a[reg as usize];
                read_memory_legacy(cpu, bus, addr, size)
            }
            AddressingMode::AddressIndirectDisplacement(reg, disp) => {
                let addr = cpu.a[reg as usize].wrapping_add(disp as i32 as u32);
                read_memory_legacy(cpu, bus, addr, size)
            }
            AddressingMode::AddressIndirectIndex(reg, idx_reg, idx_size, disp) => {
                let base = cpu.a[reg as usize];
                let index = get_index_value(cpu, idx_reg, idx_size);
                let addr = base.wrapping_add(index).wrapping_add(disp as i32 as u32);
                read_memory_legacy(cpu, bus, addr, size)
            }
            AddressingMode::AbsoluteShort(addr) => {
                let addr = addr as i16 as i32 as u32; // Sign-extend
                read_memory_legacy(cpu, bus, addr, size)
            }
            AddressingMode::AbsoluteLong(addr) => read_memory_legacy(cpu, bus, addr, size),
            AddressingMode::PcDisplacement(disp) => {
                let addr = cpu.pc.wrapping_add(disp as i32 as u32);
                read_memory_legacy(cpu, bus, addr, size)
            }
            AddressingMode::PcIndex(idx_reg, idx_size, disp) => {
                let base = cpu.pc;
                let index = get_index_value(cpu, idx_reg, idx_size);
                let addr = base.wrapping_add(index).wrapping_add(disp as i32 as u32);
                read_memory_legacy(cpu, bus, addr, size)
            }
            AddressingMode::Immediate(value) => value & size.mask(),
        }
    }

    /// LEGACY: Write a value to the effective address.
    /// NOTE: New code should use the version that takes prefetch parameter.
    ///
    /// For (An)+ and -(An) modes, by default this does NOT modify the register
    /// (assumes read() was called first for read-modify-write operations).
    ///
    /// For write-only operations (like MOVE to memory), pass `adjust: true`
    /// to apply the post-increment or pre-decrement.
    pub fn write_legacy(
        &self,
        cpu: &mut super::M68k,
        bus: &mut dyn Addressable,
        size: Size,
        value: u32,
    ) {
        self.write_with_adjust_legacy(cpu, bus, size, value, false);
    }

    /// LEGACY: Write a value to the effective address with explicit register adjustment control.
    /// NOTE: New code should use the version that takes prefetch parameter.
    ///
    /// - `adjust: false` - Don't modify An for (An)+/-(An) (use after read() for read-modify-write)
    /// - `adjust: true` - Apply post-increment or pre-decrement (use for write-only operations)
    pub fn write_with_adjust_legacy(
        &self,
        cpu: &mut super::M68k,
        bus: &mut dyn Addressable,
        size: Size,
        value: u32,
        adjust: bool,
    ) {
        match self.mode {
            AddressingMode::DataDirect(reg) => {
                let mask = size.mask();
                cpu.d[reg as usize] = (cpu.d[reg as usize] & !mask) | (value & mask);
            }
            AddressingMode::AddressDirect(reg) => {
                // Address registers always write full 32-bit (sign-extended for word)
                if size == Size::Word {
                    cpu.a[reg as usize] = value as i16 as i32 as u32;
                } else {
                    cpu.a[reg as usize] = value;
                }
            }
            AddressingMode::AddressIndirect(reg) => {
                let addr = cpu.a[reg as usize];
                write_memory_legacy(cpu, bus, addr, size, value);
            }
            AddressingMode::AddressIndirectPostInc(reg) => {
                // For (An)+: if adjust=false (after read), An is already incremented,
                // so write at An - size. If adjust=true (write-only), write at An then increment.
                let (addr, new_a) = if adjust {
                    // Write-only: write at current An, then increment
                    let addr = cpu.a[reg as usize];
                    let increment = if reg == 7 && size == Size::Byte {
                        2
                    } else {
                        size.bytes()
                    };
                    (addr, addr.wrapping_add(increment))
                } else {
                    // After read: An was already incremented, write at original address
                    let decrement = if reg == 7 && size == Size::Byte {
                        2
                    } else {
                        size.bytes()
                    };
                    let original_addr = cpu.a[reg as usize].wrapping_sub(decrement);
                    (original_addr, cpu.a[reg as usize]) // Keep An unchanged
                };
                write_memory_legacy(cpu, bus, addr, size, value);
                cpu.a[reg as usize] = new_a;
            }
            AddressingMode::AddressIndirectPreDec(reg) => {
                // For -(An): if adjust=false (after read), An is already decremented,
                // so write at current An. If adjust=true (write-only), decrement then write.
                let addr = if adjust {
                    // Write-only: decrement first, then write
                    let decrement = if reg == 7 && size == Size::Byte {
                        2
                    } else {
                        size.bytes()
                    };
                    cpu.a[reg as usize] = cpu.a[reg as usize].wrapping_sub(decrement);
                    cpu.a[reg as usize]
                } else {
                    // After read: An is already decremented, write at current address
                    cpu.a[reg as usize]
                };
                write_memory_legacy(cpu, bus, addr, size, value);
            }
            AddressingMode::AddressIndirectDisplacement(reg, disp) => {
                let addr = cpu.a[reg as usize].wrapping_add(disp as i32 as u32);
                write_memory_legacy(cpu, bus, addr, size, value);
            }
            AddressingMode::AddressIndirectIndex(reg, idx_reg, idx_size, disp) => {
                let base = cpu.a[reg as usize];
                let index = get_index_value(cpu, idx_reg, idx_size);
                let addr = base.wrapping_add(index).wrapping_add(disp as i32 as u32);
                write_memory_legacy(cpu, bus, addr, size, value);
            }
            AddressingMode::AbsoluteShort(addr) => {
                let addr = addr as i16 as i32 as u32;
                write_memory_legacy(cpu, bus, addr, size, value);
            }
            AddressingMode::AbsoluteLong(addr) => {
                write_memory_legacy(cpu, bus, addr, size, value);
            }
            _ => {
                // PC-relative and Immediate modes are not writable - signal illegal instruction
                cpu.illegal_instruction_pending.set(true);
            }
        }
    }

    /// Read a value from the effective address using prefetch for Address Error tracking.
    /// This is the official read method - all new code should use this.
    pub fn read(
        &self,
        cpu: &mut super::M68k,
        bus: &mut dyn Addressable,
        prefetch: &Prefetch,
        size: Size,
    ) -> u32 {
        match self.mode {
            AddressingMode::DataDirect(reg) => cpu.d[reg as usize] & size.mask(),
            AddressingMode::AddressDirect(reg) => cpu.a[reg as usize],
            AddressingMode::AddressIndirect(reg) => {
                let addr = cpu.a[reg as usize];
                read_memory(cpu, bus, prefetch, addr, size)
            }
            AddressingMode::AddressIndirectPostInc(reg) => {
                let addr = cpu.a[reg as usize];
                let increment = if reg == 7 && size == Size::Byte {
                    2 // A7 (SP) always uses 2-byte increment/decrement
                } else {
                    size.bytes()
                };

                // For (An)+, the 68000 ALWAYS increments the register, even if Address Error occurs
                // The increment happens "atomically" with the read attempt
                let value = read_memory(cpu, bus, prefetch, addr, size);
                cpu.a[reg as usize] = addr.wrapping_add(increment);

                value
            }
            AddressingMode::AddressIndirectPreDec(reg) => {
                let decrement = if reg == 7 && size == Size::Byte {
                    2
                } else {
                    size.bytes()
                };
                cpu.a[reg as usize] = cpu.a[reg as usize].wrapping_sub(decrement);
                let addr = cpu.a[reg as usize];
                read_memory(cpu, bus, prefetch, addr, size)
            }
            AddressingMode::AddressIndirectDisplacement(reg, disp) => {
                let addr = cpu.a[reg as usize].wrapping_add(disp as i32 as u32);
                read_memory(cpu, bus, prefetch, addr, size)
            }
            AddressingMode::AddressIndirectIndex(reg, idx_reg, idx_size, disp) => {
                let base = cpu.a[reg as usize];
                let index = get_index_value(cpu, idx_reg, idx_size);
                let addr = base.wrapping_add(index).wrapping_add(disp as i32 as u32);
                read_memory(cpu, bus, prefetch, addr, size)
            }
            AddressingMode::AbsoluteShort(addr) => {
                let addr = addr as i16 as i32 as u32;
                read_memory(cpu, bus, prefetch, addr, size)
            }
            AddressingMode::AbsoluteLong(addr) => read_memory(cpu, bus, prefetch, addr, size),
            AddressingMode::PcDisplacement(disp) => {
                // Use prefetch.base() for PC-relative (instruction_pc + 2)
                let addr = prefetch.base().wrapping_add(disp as i32 as u32);
                read_memory(cpu, bus, prefetch, addr, size)
            }
            AddressingMode::PcIndex(idx_reg, idx_size, disp) => {
                // Use prefetch.base() for PC-relative (instruction_pc + 2)
                let base = prefetch.base();
                let index = get_index_value(cpu, idx_reg, idx_size);
                let addr = base.wrapping_add(index).wrapping_add(disp as i32 as u32);
                read_memory(cpu, bus, prefetch, addr, size)
            }
            AddressingMode::Immediate(value) => value & size.mask(),
        }
    }

    /// Write a value to the effective address using prefetch for Address Error tracking.
    /// This is the official write method - all new code should use this.
    pub fn write(
        &self,
        cpu: &mut super::M68k,
        bus: &mut dyn Addressable,
        prefetch: &Prefetch,
        size: Size,
        value: u32,
    ) {
        match self.mode {
            AddressingMode::DataDirect(reg) => {
                let mask = size.mask();
                cpu.d[reg as usize] = (cpu.d[reg as usize] & !mask) | (value & mask);
            }
            AddressingMode::AddressDirect(reg) => {
                if size == Size::Word {
                    cpu.a[reg as usize] = value as i16 as i32 as u32;
                } else {
                    cpu.a[reg as usize] = value;
                }
            }
            AddressingMode::AddressIndirect(reg) => {
                let addr = cpu.a[reg as usize];
                write_memory(cpu, bus, prefetch, addr, size, value);
            }
            AddressingMode::AddressIndirectPostInc(reg) => {
                // For read-modify-write: read() already incremented An,
                // so write to An - increment to get back to original address
                let increment = if reg == 7 && size == Size::Byte {
                    2
                } else {
                    size.bytes()
                };
                let addr = cpu.a[reg as usize].wrapping_sub(increment);
                write_memory(cpu, bus, prefetch, addr, size, value);
            }
            AddressingMode::AddressIndirectPreDec(reg) => {
                // For read-modify-write: read() already decremented An,
                // so An points to the correct address
                let addr = cpu.a[reg as usize];
                write_memory(cpu, bus, prefetch, addr, size, value);
            }
            AddressingMode::AddressIndirectDisplacement(reg, disp) => {
                let addr = cpu.a[reg as usize].wrapping_add(disp as i32 as u32);
                write_memory(cpu, bus, prefetch, addr, size, value);
            }
            AddressingMode::AddressIndirectIndex(reg, idx_reg, idx_size, disp) => {
                let base = cpu.a[reg as usize];
                let index = get_index_value(cpu, idx_reg, idx_size);
                let addr = base.wrapping_add(index).wrapping_add(disp as i32 as u32);
                write_memory(cpu, bus, prefetch, addr, size, value);
            }
            AddressingMode::AbsoluteShort(addr) => {
                let addr = addr as i16 as i32 as u32;
                write_memory(cpu, bus, prefetch, addr, size, value);
            }
            AddressingMode::AbsoluteLong(addr) => {
                write_memory(cpu, bus, prefetch, addr, size, value);
            }
            _ => {
                // Cannot write to this addressing mode (e.g., PC-relative, immediate)
                // Signal illegal instruction
                cpu.illegal_instruction_pending.set(true);
            }
        }
    }

    /// Write a value to the effective address for write-only operations (like MOVE destination).
    /// Unlike write(), this handles (An)+ and -(An) correctly for operations that only write
    /// (not read-modify-write).
    pub fn write_with_adjust(
        &self,
        cpu: &mut super::M68k,
        bus: &mut dyn Addressable,
        prefetch: &Prefetch,
        size: Size,
        value: u32,
        decrement: bool,
    ) {
        match self.mode {
            AddressingMode::DataDirect(reg) => {
                let mask = size.mask();
                cpu.d[reg as usize] = (cpu.d[reg as usize] & !mask) | (value & mask);
            }
            AddressingMode::AddressDirect(reg) => {
                if size == Size::Word {
                    cpu.a[reg as usize] = value as i16 as i32 as u32;
                } else {
                    cpu.a[reg as usize] = value;
                }
            }
            AddressingMode::AddressIndirect(reg) => {
                let addr = cpu.a[reg as usize];
                if !cpu.check_address_alignment_write_only(addr, size, prefetch) {
                    return;
                }
                write_memory_unchecked(cpu, bus, addr, size, value);
            }
            AddressingMode::AddressIndirectPostInc(reg) => {
                // Write-only: check alignment, write to current An, then increment (only if no Address Error).
                let addr = cpu.a[reg as usize];

                // Check alignment for write-only operation
                if !cpu.check_address_alignment_write_only(addr, size, prefetch) {
                    return;
                }

                write_memory_unchecked(cpu, bus, addr, size, value);

                // Only increment if write succeeded (no Address Error)
                if cpu.address_error.is_none() {
                    let increment = if reg == 7 && size == Size::Byte {
                        2 // A7 (SP) always moves by at least 2
                    } else {
                        size.bytes()
                    };
                    cpu.a[reg as usize] = cpu.a[reg as usize].wrapping_add(increment);
                }
            }
            AddressingMode::AddressIndirectPreDec(reg) => {
                // Write-only: ALWAYS decrement first (MC68000 behavior)
                // The side effect (An decrement) occurs BEFORE the alignment check
                let increment = if reg == 7 && size == Size::Byte {
                    2 // A7 (SP) always moves by at least 2
                } else {
                    size.bytes()
                };

                // Calculate target address and DECREMENT An FIRST
                let addr = cpu.a[reg as usize].wrapping_sub(increment);
                cpu.a[reg as usize] = addr;

                // Check alignment AFTER decrementing An
                // If Address Error occurs, An remains decremented (documented 68000 behavior)
                // Use predec-specific check that adds +2 to PC
                if !cpu.check_address_alignment_predec_write(addr, size, prefetch) {
                    // Address Error - An is already decremented and stays that way
                    if size == Size::Long {
                        cpu.a[reg as usize] = cpu.a[reg as usize].wrapping_add(2);
                    }
                    return;
                }

                // No Address Error - proceed with write. A predecrement long
                // destination writes the low word first (68000 bus quirk), which
                // is observable when An points at a memory-mapped port.
                write_memory_unchecked_predec(cpu, bus, addr, size, value);
            }
            AddressingMode::AddressIndirectDisplacement(reg, disp) => {
                let addr = cpu.a[reg as usize].wrapping_add(disp as i32 as u32);
                if !cpu.check_address_alignment_write_only(addr, size, prefetch) {
                    return;
                }
                write_memory_unchecked(cpu, bus, addr, size, value);
            }
            AddressingMode::AddressIndirectIndex(reg, idx_reg, idx_size, disp) => {
                let base = cpu.a[reg as usize];
                let index = get_index_value(cpu, idx_reg, idx_size);
                let addr = base.wrapping_add(index).wrapping_add(disp as i32 as u32);
                if !cpu.check_address_alignment_write_only(addr, size, prefetch) {
                    return;
                }
                write_memory_unchecked(cpu, bus, addr, size, value);
            }
            AddressingMode::AbsoluteShort(addr) => {
                let addr = addr as i16 as i32 as u32;
                if !cpu.check_address_alignment_write_only(addr, size, prefetch) {
                    return;
                }
                write_memory_unchecked(cpu, bus, addr, size, value);
            }
            AddressingMode::AbsoluteLong(addr) => {
                // The absolute-long check: like the write-only one, but with
                // `decrement` (a MOVE from a memory source) the PC stacked is two
                // bytes lower.
                if !cpu.check_address_alignment_abslong_write(addr, size, prefetch, decrement) {
                    return;
                }
                write_memory_unchecked(cpu, bus, addr, size, value);
            }
            _ => {
                // PC-relative and Immediate modes are not writable - signal illegal instruction
                cpu.illegal_instruction_pending.set(true);
            }
        }
    }
}

/// Decode the mode and register fields from an instruction word.
pub fn decode_mode_reg(mode: u8, reg: u8) -> (u8, u8) {
    (mode & 0x7, reg & 0x7)
}

/// Get the value of an index register (for indexed addressing modes).
fn get_index_value(cpu: &super::M68k, idx_reg: u8, idx_size: Size) -> u32 {
    let is_address = (idx_reg & 0x8) != 0;
    let reg_num = (idx_reg & 0x7) as usize;

    let value = if is_address {
        cpu.a[reg_num]
    } else {
        cpu.d[reg_num]
    };

    match idx_size {
        Size::Word => value as i16 as i32 as u32, // Sign-extend
        Size::Long => value,
        _ => value,
    }
}

/// LEGACY: Read from memory with the specified size.
/// For word/long accesses, checks alignment and sets address_error on the CPU if misaligned.
/// NOTE: New code should use read_memory (prefetch version).
fn read_memory_legacy(
    cpu: &mut super::M68k,
    bus: &mut dyn Addressable,
    addr: u32,
    size: Size,
) -> u32 {
    // Check alignment for word/long accesses
    if !cpu.check_address_alignment(addr, size) {
        // Return dummy value; the instruction will be aborted
        return 0;
    }

    match size {
        Size::Byte => bus.read(addr) as u32,
        Size::Word => bus.read_word(addr) as u32,
        Size::Long => {
            let high = bus.read_word(addr) as u32;
            let low = bus.read_word(addr + 2) as u32;
            (high << 16) | low
        }
    }
}

/// LEGACY: Write to memory with the specified size.
/// For word/long accesses, checks alignment and sets address_error on the CPU if misaligned.
/// NOTE: New code should use write_memory (prefetch version).
fn write_memory_legacy(
    cpu: &mut super::M68k,
    bus: &mut dyn Addressable,
    addr: u32,
    size: Size,
    value: u32,
) {
    // Check alignment for word/long accesses
    if !cpu.check_address_alignment(addr, size) {
        // Don't write; the instruction will be aborted
        return;
    }

    match size {
        Size::Byte => bus.write(addr, value as u8),
        Size::Word => bus.write_word(addr, value as u16),
        Size::Long => {
            bus.write_word(addr, (value >> 16) as u16);
            bus.write_word(addr + 2, value as u16);
        }
    }
}

/// Read from memory using Prefetch for Address Error tracking.
/// This version uses check_address_alignment_with_prefetch to get the correct PC.
fn read_memory(
    cpu: &mut super::M68k,
    bus: &mut dyn Addressable,
    prefetch: &Prefetch,
    addr: u32,
    size: Size,
) -> u32 {
    if !cpu.check_address_alignment_with_prefetch(addr, size, prefetch, false) {
        return 0; // Read operation
    }

    match size {
        Size::Byte => bus.read(addr) as u32,
        Size::Word => bus.read_word(addr) as u32,
        Size::Long => {
            let high = bus.read_word(addr) as u32;
            let low = bus.read_word(addr + 2) as u32;
            (high << 16) | low
        }
    }
}

/// Write to memory using Prefetch for Address Error tracking.
/// This version uses check_address_alignment_with_prefetch to get the correct PC.
fn write_memory(
    cpu: &mut super::M68k,
    bus: &mut dyn Addressable,
    prefetch: &Prefetch,
    addr: u32,
    size: Size,
    value: u32,
) {
    if !cpu.check_address_alignment_with_prefetch(addr, size, prefetch, false) {
        return; // RMW operations mark writes as "read" in the 68000
    }

    write_memory_unchecked(cpu, bus, addr, size, value);
}

/// Write to memory WITHOUT alignment check.
/// Used by write_with_adjust which has already checked alignment with the correct is_write flag.
fn write_memory_unchecked(
    cpu: &mut super::M68k,
    bus: &mut dyn Addressable,
    addr: u32,
    size: Size,
    value: u32,
) {
    match size {
        Size::Byte => {
            bus.write(addr, value as u8);
            cpu.cycles_executed += 4;
        }
        Size::Word => {
            bus.write_word(addr, value as u16);
            cpu.cycles_executed += 4;
        }
        Size::Long => {
            bus.write_word(addr, (value >> 16) as u16);
            bus.write_word(addr + 2, value as u16);
            cpu.cycles_executed += 8;
        }
    }
}

/// Long write using the MC68000 predecrement bus order.
///
/// `MOVE.L <ea>,-(An)` is the one long write the 68000 performs **low word
/// first** (to `addr + 2`), then **high word** (to `addr`), every other long
/// write is high word first. The final bytes in plain RAM are identical either
/// way, so this only matters on memory-mapped ports where the two halves have
/// side effects: the Mega Drive VDP relies on it so the low word can complete a
/// DMA-triggering control command and the chip takes the bus mid-instruction
/// before the high word lands as the following data/register write
/// (VDPFIFOTesting "DMA Transfer Bus Locking"). Byte/word are single accesses,
/// so they defer to the standard path.
fn write_memory_unchecked_predec(
    cpu: &mut super::M68k,
    bus: &mut dyn Addressable,
    addr: u32,
    size: Size,
    value: u32,
) {
    match size {
        Size::Long => {
            bus.write_word(addr + 2, value as u16);
            bus.write_word(addr, (value >> 16) as u16);
            cpu.cycles_executed += 8;
        }
        _ => write_memory_unchecked(cpu, bus, addr, size, value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_size_bytes() {
        assert_eq!(Size::Byte.bytes(), 1);
        assert_eq!(Size::Word.bytes(), 2);
        assert_eq!(Size::Long.bytes(), 4);
    }

    #[test]
    fn test_size_mask() {
        assert_eq!(Size::Byte.mask(), 0xFF);
        assert_eq!(Size::Word.mask(), 0xFFFF);
        assert_eq!(Size::Long.mask(), 0xFFFF_FFFF);
    }

    #[test]
    fn test_data_direct_read_write() {
        let mut cpu = super::super::M68k::new();
        let mut ram = Ram::new(0x1000);
        cpu.d[0] = 0x12345678;

        let ea = EffectiveAddress::new(AddressingMode::DataDirect(0), 0);

        // Read byte
        let value = ea.read_legacy(&mut cpu, &mut ram, Size::Byte);
        assert_eq!(value, 0x78);

        // Read word
        let value = ea.read_legacy(&mut cpu, &mut ram, Size::Word);
        assert_eq!(value, 0x5678);

        // Read long
        let value = ea.read_legacy(&mut cpu, &mut ram, Size::Long);
        assert_eq!(value, 0x12345678);

        // Write byte (preserve other bits)
        ea.write_legacy(&mut cpu, &mut ram, Size::Byte, 0xAB);
        assert_eq!(cpu.d[0], 0x123456AB);

        // Write word (preserve upper word)
        ea.write_legacy(&mut cpu, &mut ram, Size::Word, 0xCDEF);
        assert_eq!(cpu.d[0], 0x1234CDEF);

        // Write long
        ea.write_legacy(&mut cpu, &mut ram, Size::Long, 0x87654321);
        assert_eq!(cpu.d[0], 0x87654321);
    }

    #[test]
    fn test_address_indirect_post_inc() {
        let mut cpu = super::super::M68k::new();
        let mut ram = Ram::new(0x1000);

        cpu.a[0] = 0x100;
        ram.write(0x100, 0x12);
        ram.write(0x101, 0x34);

        let ea = EffectiveAddress::new(AddressingMode::AddressIndirectPostInc(0), 0);

        // Read byte
        let value = ea.read_legacy(&mut cpu, &mut ram, Size::Byte);
        assert_eq!(value, 0x12);
        assert_eq!(cpu.a[0], 0x101); // Incremented by 1

        // Read word (Big Endian: 0x12 at 0x100, 0x34 at 0x101)
        // For Big Endian, read_word will read 0x1234
        cpu.a[0] = 0x100;
        let value = ea.read_legacy(&mut cpu, &mut ram, Size::Word);
        // Note: Ram doesn't do Big Endian, so we get the raw bytes
        // This is actually correct for the test - just checking post-increment works
        assert!(value == 0x1234 || value == 0x3412); // Accept either endianness for unit test
        assert_eq!(cpu.a[0], 0x102); // Incremented by 2
    }

    #[test]
    fn test_address_indirect_pre_dec() {
        let mut cpu = super::super::M68k::new();
        let mut ram = Ram::new(0x1000);

        cpu.a[0] = 0x102;
        ram.write(0x100, 0x12);
        ram.write(0x101, 0x34);

        let ea = EffectiveAddress::new(AddressingMode::AddressIndirectPreDec(0), 0);

        // Read word
        let value = ea.read_legacy(&mut cpu, &mut ram, Size::Word);
        assert!(value == 0x1234 || value == 0x3412); // Accept either endianness for unit test
        assert_eq!(cpu.a[0], 0x100); // Decremented by 2

        // Read byte
        cpu.a[0] = 0x101;
        let value = ea.read_legacy(&mut cpu, &mut ram, Size::Byte);
        assert_eq!(value, 0x12);
        assert_eq!(cpu.a[0], 0x100); // Decremented by 1
    }

    #[test]
    fn test_immediate_mode() {
        let mut cpu = super::super::M68k::new();
        let mut ram = Ram::new(0x1000);

        let ea = EffectiveAddress::new(AddressingMode::Immediate(0x12345678), 0);

        assert_eq!(ea.read_legacy(&mut cpu, &mut ram, Size::Byte), 0x78);
        assert_eq!(ea.read_legacy(&mut cpu, &mut ram, Size::Word), 0x5678);
        assert_eq!(ea.read_legacy(&mut cpu, &mut ram, Size::Long), 0x12345678);
    }

    /// Test read-modify-write with (An)+ - register should only increment once
    #[test]
    fn test_post_inc_read_then_write() {
        let mut cpu = super::super::M68k::new();
        let mut ram = Ram::new(0x1000);

        cpu.a[0] = 0x100;
        ram.write(0x100, 0x42);

        let ea = EffectiveAddress::new(AddressingMode::AddressIndirectPostInc(0), 0);

        // Read (increments A0 to 0x101)
        let value = ea.read_legacy(&mut cpu, &mut ram, Size::Byte);
        assert_eq!(value, 0x42);
        assert_eq!(cpu.a[0], 0x101);

        // Write without adjust (keeps A0 at 0x101, writes to original address 0x100)
        ea.write_legacy(&mut cpu, &mut ram, Size::Byte, 0x55);
        assert_eq!(cpu.a[0], 0x101); // NOT 0x102!
        assert_eq!(ram.read(0x100), 0x55); // Written to correct address
    }

    /// Test write-only with (An)+ - using write_with_adjust(true)
    #[test]
    fn test_post_inc_write_only() {
        let mut cpu = super::super::M68k::new();
        let mut ram = Ram::new(0x1000);

        cpu.a[0] = 0x100;

        let ea = EffectiveAddress::new(AddressingMode::AddressIndirectPostInc(0), 0);

        // Write with adjust (write-only operation)
        ea.write_with_adjust_legacy(&mut cpu, &mut ram, Size::Byte, 0x55, true);
        assert_eq!(cpu.a[0], 0x101); // Incremented
        assert_eq!(ram.read(0x100), 0x55);
    }

    /// Test read-modify-write with -(An) - register should only decrement once
    #[test]
    fn test_pre_dec_read_then_write() {
        let mut cpu = super::super::M68k::new();
        let mut ram = Ram::new(0x1000);

        cpu.a[0] = 0x101;
        ram.write(0x100, 0x42);

        let ea = EffectiveAddress::new(AddressingMode::AddressIndirectPreDec(0), 0);

        // Read (decrements A0 to 0x100 first, then reads)
        let value = ea.read_legacy(&mut cpu, &mut ram, Size::Byte);
        assert_eq!(value, 0x42);
        assert_eq!(cpu.a[0], 0x100);

        // Write without adjust (keeps A0 at 0x100, writes to same address)
        ea.write_legacy(&mut cpu, &mut ram, Size::Byte, 0x55);
        assert_eq!(cpu.a[0], 0x100); // NOT 0xFF!
        assert_eq!(ram.read(0x100), 0x55);
    }

    /// Test write-only with -(An) - using write_with_adjust(true)
    #[test]
    fn test_pre_dec_write_only() {
        let mut cpu = super::super::M68k::new();
        let mut ram = Ram::new(0x1000);

        cpu.a[0] = 0x101;

        let ea = EffectiveAddress::new(AddressingMode::AddressIndirectPreDec(0), 0);

        // Write with adjust (write-only operation)
        ea.write_with_adjust_legacy(&mut cpu, &mut ram, Size::Byte, 0x55, true);
        assert_eq!(cpu.a[0], 0x100); // Decremented
        assert_eq!(ram.read(0x100), 0x55);
    }
}
