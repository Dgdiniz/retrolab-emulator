//! Data movement instructions: MOVE, MOVEA, MOVEQ, LEA.

use super::{InstructionResult, M68k};
use crate::cpu68k::{
    addressing::{AddressingMode, EffectiveAddress, Size},
    decode,
    prefetch::Prefetch,
};
use retro_core::cpu::Addressable;

/// Map addressing mode to cycle table index for MOVE instruction.
///
/// Table indices:
/// 0 = Dn          (Data Register Direct)
/// 1 = An          (Address Register Direct)
/// 2 = (An)        (Address Register Indirect)
/// 3 = (An)+       (Post-increment)
/// 4 = -(An)       (Pre-decrement)
/// 5 = d(An)       (Displacement)
/// 6 = d(An,ix)    (Index)
/// 7 = xxx.W       (Absolute Short)
/// 8 = xxx.L       (Absolute Long)
/// 9 = d(PC)       (PC Displacement)
/// 10 = d(PC,ix)   (PC Index)
/// 11 = #xxx       (Immediate)
fn ea_mode_to_cycle_index(mode: &AddressingMode) -> usize {
    match mode {
        AddressingMode::DataDirect(_) => 0,
        AddressingMode::AddressDirect(_) => 1,
        AddressingMode::AddressIndirect(_) => 2,
        AddressingMode::AddressIndirectPostInc(_) => 3,
        AddressingMode::AddressIndirectPreDec(_) => 4,
        AddressingMode::AddressIndirectDisplacement(_, _) => 5,
        AddressingMode::AddressIndirectIndex(_, _, _, _) => 6,
        AddressingMode::AbsoluteShort(_) => 7,
        AddressingMode::AbsoluteLong(_) => 8,
        AddressingMode::PcDisplacement(_) => 9,
        AddressingMode::PcIndex(_, _, _) => 10,
        AddressingMode::Immediate(_) => 11,
    }
}

/// Get MOVE instruction cycle count from lookup table.
///
/// **Cycle Tables** (from 68000 Programmer's Reference Manual):
/// - Format: total_cycles(reads/writes)
/// - Source = rows, Destination = columns
/// - Separate tables for Byte/Word vs Long
///
/// **Important**: These are TOTAL cycles including bus accesses, which is what
/// `step()` expects an instruction to return.
///
/// Returns the total cycle count for the MOVE operation.
fn get_move_cycles(src_ea: &EffectiveAddress, dst_ea: &EffectiveAddress, size: Size) -> u32 {
    let src_idx = ea_mode_to_cycle_index(&src_ea.mode);
    let dst_idx = ea_mode_to_cycle_index(&dst_ea.mode);

    // Cycle tables for MOVE Byte/Word
    // Rows = Source EA, Columns = Destination EA
    #[rustfmt::skip]
    const MOVE_BYTE_WORD_CYCLES: [[u32; 12]; 12] = [
        // Dn    An    (An)  (An)+ -(An) d(An) d(An,ix) xxx.W xxx.L d(PC) d(PC,ix) #xxx
        [   4,    4,    8,    8,    8,   12,    14,     12,   16,   12,    14,      8  ], // Dn
        [   4,    4,    8,    8,    8,   12,    14,     12,   16,   12,    14,      8  ], // An
        [   8,    8,   12,   12,   12,   16,    18,     16,   20,   16,    18,     12  ], // (An)
        [   8,    8,   12,   12,   12,   16,    18,     16,   20,   16,    18,     12  ], // (An)+
        [  10,   10,   14,   14,   14,   18,    20,     18,   22,   18,    20,     14  ], // -(An)
        [  12,   12,   16,   16,   16,   20,    22,     20,   24,   20,    22,     16  ], // d(An)
        [  14,   14,   18,   18,   18,   22,    24,     22,   26,   22,    24,     18  ], // d(An,ix)
        [  12,   12,   16,   16,   16,   20,    22,     20,   24,   20,    22,     16  ], // xxx.W
        [  16,   16,   20,   20,   20,   24,    26,     24,   28,   24,    26,     20  ], // xxx.L
        [  12,   12,   16,   16,   16,   20,    22,     20,   24,   20,    22,     16  ], // d(PC)
        [  14,   14,   18,   18,   18,   22,    24,     22,   26,   22,    24,     18  ], // d(PC,ix)
        [   8,    8,   12,   12,   12,   16,    18,     16,   20,   16,    18,     12  ], // #xxx
    ];

    // Cycle tables for MOVE Long
    #[rustfmt::skip]
    const MOVE_LONG_CYCLES: [[u32; 12]; 12] = [
        // Dn    An    (An)  (An)+ -(An) d(An) d(An,ix) xxx.W xxx.L d(PC) d(PC,ix) #xxx
        [   4,    4,   12,   12,   12,   16,    18,     16,   20,   16,    18,     12  ], // Dn
        [   4,    4,   12,   12,   12,   16,    18,     16,   20,   16,    18,     12  ], // An
        [  12,   12,   20,   20,   20,   24,    26,     24,   28,   24,    26,     20  ], // (An)
        [  12,   12,   20,   20,   20,   24,    26,     24,   28,   24,    26,     20  ], // (An)+
        [  14,   14,   22,   22,   22,   26,    28,     26,   30,   26,    28,     22  ], // -(An)
        [  16,   16,   24,   24,   24,   28,    30,     28,   32,   28,    30,     24  ], // d(An)
        [  18,   18,   26,   26,   26,   30,    32,     30,   34,   30,    32,     26  ], // d(An,ix)
        [  16,   16,   24,   24,   24,   28,    30,     28,   32,   28,    30,     24  ], // xxx.W
        [  20,   20,   28,   28,   28,   32,    34,     32,   36,   32,    34,     28  ], // xxx.L
        [  16,   16,   24,   24,   24,   28,    30,     28,   32,   28,    30,     24  ], // d(PC)
        [  18,   18,   26,   26,   26,   30,    32,     30,   34,   30,    32,     26  ], // d(PC,ix)
        [  12,   12,   20,   20,   20,   24,    26,     24,   28,   24,    26,     20  ], // #xxx
    ];

    let table = match size {
        Size::Byte | Size::Word => &MOVE_BYTE_WORD_CYCLES,
        Size::Long => &MOVE_LONG_CYCLES,
    };

    table[src_idx][dst_idx]
}

//TODO: the tables are equal, so only one is needed
fn get_move_exception_source_cycles(src_ea: &EffectiveAddress, size: Size) -> u32 {
    let src_idx = ea_mode_to_cycle_index(&src_ea.mode);

    // Cycle tables for MOVE Byte/Word
    // Rows = Source EA, Columns = Destination EA
    #[rustfmt::skip]
    const MOVE_BYTE_WORD_CYCLES: [u32; 12] = 
        // Dn    An    (An)  (An)+ -(An) d(An) d(An,ix) xxx.W xxx.L d(PC) d(PC,ix) #xxx
        [  10,   10,    10,   10,   12,   10,   12,     10,   10,   10,   12,      10  ];

    // Cycle tables for MOVE Long
    #[rustfmt::skip]
    const MOVE_LONG_CYCLES: [u32; 12] = 
        // Dn    An    (An)  (An)+ -(An) d(An) d(An,ix) xxx.W xxx.L d(PC) d(PC,ix) #xxx
        [  10,   10,    10,   10,   12,   10,   12,     10,   10,   10,   12,      10  ];

    let table = match size {
        Size::Byte | Size::Word => &MOVE_BYTE_WORD_CYCLES,
        Size::Long => &MOVE_LONG_CYCLES,
    };

    table[src_idx]
}

//TODO: the tables are equal, so only one is needed
fn get_move_exception_destination_cycles(
    src_ea: &EffectiveAddress,
    dst_ea: &EffectiveAddress,
    size: Size,
) -> u32 {
    let src_idx = ea_mode_to_cycle_index(&src_ea.mode);
    let dst_idx = ea_mode_to_cycle_index(&dst_ea.mode);

    // Cycle tables for MOVE Byte/Word
    // Rows = Source EA, Columns = Destination EA
    #[rustfmt::skip]
    const MOVE_BYTE_WORD_CYCLES: [[u32; 12]; 12] = [
        // Dn    An    (An)  (An)+ -(An) d(An) d(An,ix) xxx.W xxx.L d(PC) d(PC,ix) #xxx
        [  10,   10,   10,   10,   14,   10,    12,     10,   10,   10,    10,     10  ], // Dn
        [  10,   10,   10,   10,   14,   10,    12,     10,   10,   10,    10,     10  ], // An
        [  10,   10,   10,   10,   14,   10,    12,     10,    6,   10,    10,     10  ], // (An)
        [  10,   10,   10,   10,   14,   10,    12,     10,    6,   10,    10,     10  ], // (An)+
        [  10,   10,   12,   12,   16,   12,    14,     12,    8,   10,    10,     10  ], // -(An)
        [  10,   10,   10,   10,   14,   10,    12,     10,    6,   10,    10,     10  ], // d(An)
        [  10,   10,   12,   12,   16,   12,    14,     12,    8,   10,    10,     10  ], // d(An,ix)
        [  10,   10,   10,   10,   14,   10,    12,     10,    8,   10,    10,     10  ], // xxx.W
        [  10,   10,   10,   10,   14,   10,    12,     10,    6,   10,    10,     10  ], // xxx.L
        [  10,   10,   10,   10,   14,   10,    12,     10,    6,   10,    10,     10  ], // d(PC)
        [  10,   10,   12,   12,   16,   12,    14,     12,    8,   10,    10,     10  ], // d(PC,ix)
        [  10,   10,   10,   10,   14,   10,    12,     10,   10,   10,    10,     10  ], // #xxx
    ];

    // Cycle tables for MOVE Long
    #[rustfmt::skip]
    const MOVE_LONG_CYCLES: [[u32; 12]; 12] = [
        // Dn    An    (An)  (An)+ -(An) d(An) d(An,ix) xxx.W xxx.L d(PC) d(PC,ix) #xxx
        [  10,   10,   10,   10,   14,   10,    12,     10,   10,   10,    10,     10  ], // Dn
        [  10,   10,   10,   10,   14,   10,    12,     10,   10,   10,    10,     10  ], // An
        [  10,   10,   10,   10,   14,   10,    12,     10,    6,   10,    10,     10  ], // (An)
        [  10,   10,   10,   10,   14,   10,    12,     10,    6,   10,    10,     10  ], // (An)+
        [  10,   10,   12,   12,   16,   12,    14,     12,    8,   10,    10,     10  ], // -(An)
        [  10,   10,   10,   10,   14,   10,    12,     10,    6,   10,    10,     10  ], // d(An)
        [  10,   10,   12,   12,   16,   12,    14,     12,    8,   10,    10,     10  ], // d(An,ix)
        [  10,   10,   10,   10,   14,   10,    12,     10,    8,   10,    10,     10  ], // xxx.W
        [  10,   10,   10,   10,   14,   10,    12,     10,    6,   10,    10,     10  ], // xxx.L
        [  10,   10,   10,   10,   14,   10,    12,     10,    6,   10,    10,     10  ], // d(PC)
        [  10,   10,   12,   12,   16,   12,    14,     12,    8,   10,    10,     10  ], // d(PC,ix)
        [  10,   10,   10,   10,   14,   10,    12,     10,   10,   10,    10,     10  ], // #xxx
    ];

    let table = match size {
        Size::Byte | Size::Word => &MOVE_BYTE_WORD_CYCLES,
        Size::Long => &MOVE_LONG_CYCLES,
    };

    table[src_idx][dst_idx]
}

impl M68k {
    /// MOVE - Move data
    /// Opcode: 00ssddddddmmmrrr
    /// where ss = size (01=byte, 11=word, 10=long)
    #[allow(clippy::too_many_arguments)]
    pub fn move_instr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        dst_mode: u8,
        dst_reg: u8,
        src_mode: u8,
        src_reg: u8,
    ) -> InstructionResult {
        // Decode source EA (reads extension words from prefetch)
        let src_ea = decode::decode_ea_with_cycles_for_move(
            self, bus, prefetch, src_mode, src_reg, size, false,
        );

        // Read source value
        let value = src_ea.read(self, bus, prefetch, size);

        // Check for Address Error during source read
        if self.address_error.is_some() {
            // Return with only source EA cycles consumed
            let cycles = get_move_exception_source_cycles(&src_ea, size);
            return InstructionResult::prefetch(cycles);
        }

        // Decode destination EA (reads MORE extension words from prefetch)
        let dst_ea = decode::decode_ea_with_cycles_for_move(
            self, bus, prefetch, dst_mode, dst_reg, size, true,
        );

        // Update flags BEFORE write (so they persist if Address Error occurs during write)
        // Flags: N, Z, V=0, C=0 (based on source value)
        self.update_flags_nz(value, size);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

        let decrement = src_mode != 0 && src_mode != 1 && !(src_mode == 7 && src_reg == 4);
        // Write to destination (write-only, so use write_with_adjust for correct (An)+/-(An) behavior)
        dst_ea.write_with_adjust(self, bus, prefetch, size, value, decrement);

        // Check for Address Error during destination write
        if self.address_error.is_some() {
            // Return with the cycles spent up to the destination write
            let cycles = get_move_exception_destination_cycles(&src_ea, &dst_ea, size);
            return InstructionResult::prefetch(cycles);
        }

        // Use cycle lookup table for accurate MOVE timing
        // The table provides TOTAL cycles (bus + internal) for each EA mode combination
        // Since MOVE is purely data transfer with no ALU operations, most cycles are bus accesses
        let total_cycles = get_move_cycles(&src_ea, &dst_ea, size);

        InstructionResult::prefetch(total_cycles)
    }

    /// MOVEA - Move to address register
    /// Opcode: 00ssaaaddmmmrrr
    /// where ss = 11 (word) or 10 (long)
    pub fn movea(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        dst_reg: u8,
        src_mode: u8,
        src_reg: u8,
    ) -> InstructionResult {
        // Decode source EA (reads extension words from prefetch)
        let src_ea = decode::decode_ea_with_cycles(self, bus, prefetch, src_mode, src_reg, size);
        let dst_ea = decode::decode_ea_with_cycles(self, bus, prefetch, 1, dst_reg, size); // TODO: Optimize this

        // Read source value
        let value = src_ea.read(self, bus, prefetch, size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_move_exception_source_cycles(&src_ea, size));
        }

        // Write to address register (sign-extended if word)
        if size == Size::Word {
            self.a[dst_reg as usize] = value as i16 as i32 as u32;
        } else {
            self.a[dst_reg as usize] = value;
        }

        // MOVEA does NOT affect flags

        InstructionResult::prefetch(get_move_cycles(&src_ea, &dst_ea, size))
    }

    /// MOVEQ - Move quick (8-bit immediate to data register)
    /// Opcode: 0111rrr0dddddddd
    pub fn moveq(&mut self, dst_reg: u8, immediate: i8) -> InstructionResult {
        // Sign-extend 8-bit immediate to 32 bits
        let value = immediate as i32 as u32;

        // Write to data register
        self.d[dst_reg as usize] = value;

        // Update flags
        self.update_flags_nz(value, Size::Long);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

        InstructionResult::new(4, 2)
    }

    /// LEA - Load Effective Address
    /// Opcode: 0100aaa111mmmrrr
    pub fn lea(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        dst_reg: u8,
        src_mode: u8,
        src_reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, src_mode, src_reg, Size::Long);

        // For PC-relative: use instruction_pc (prefetch.base)
        // The displacement in the extension word is relative to the PC *after* the extension word
        // which is prefetch.base + 2, but decode already accounts for this
        let base_pc = prefetch.base();

        let address = ea.calculate_address(self, base_pc);

        self.a[dst_reg as usize] = address;

        // LEA does NOT affect flags

        let cycles = match &ea.mode {
            AddressingMode::AddressIndirect(_) => 4,
            AddressingMode::AddressIndirectDisplacement(_, _) => 8,
            AddressingMode::AddressIndirectIndex(_, _, _, _) => 12,
            AddressingMode::AbsoluteShort(_) => 8,
            AddressingMode::AbsoluteLong(_) => 12,
            AddressingMode::PcDisplacement(_) => 8,
            AddressingMode::PcIndex(_, _, _) => 12,
            _ => 4,
        };

        InstructionResult::prefetch(cycles)
    }

    /// PEA - Push Effective Address
    /// Opcode: 0100100001mmmrrr
    /// Computes EA and pushes it onto the stack
    pub fn pea(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, Size::Long);

        // For PC-relative: use instruction_pc (prefetch.base)
        // The displacement in the extension word is relative to the PC *after* the extension word
        // which is prefetch.base + 2, but decode already accounts for this
        let base_pc = prefetch.base();

        let address = ea.calculate_address(self, base_pc);

        // Push address onto stack (A7)
        self.a[7] = self.a[7].wrapping_sub(4);
        bus.write_word(self.a[7], (address >> 16) as u16);
        bus.write_word(self.a[7].wrapping_add(2), address as u16);

        let cycles = match &ea.mode {
            AddressingMode::AddressIndirect(_) => 12,
            AddressingMode::AddressIndirectDisplacement(_, _) => 16,
            AddressingMode::AddressIndirectIndex(_, _, _, _) => 20,
            AddressingMode::AbsoluteShort(_) => 16,
            AddressingMode::AbsoluteLong(_) => 20,
            AddressingMode::PcDisplacement(_) => 16,
            AddressingMode::PcIndex(_, _, _) => 20,
            _ => 12,
        };

        InstructionResult::prefetch(cycles)
    }

    /// CLR - Clear operand (set to zero)
    /// Opcode: 01000010ssmmmrrr
    /// where ss = size (00=byte, 01=word, 10=long)
    pub fn clr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);

        let _value = ea.read(self, bus, prefetch, size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_move_exception_source_cycles(&ea, size));
        }

        // Write zero to destination
        ea.write(self, bus, prefetch, size, 0);

        // Update flags: N=0, Z=1, V=0, C=0
        self.set_flag(super::Flag::N, false);
        self.set_flag(super::Flag::Z, true);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

        let cycles = if size == Size::Long {
            match ea.mode {
                AddressingMode::DataDirect(_) => 6,
                AddressingMode::AddressDirect(_) => 6,
                _ => 12,
            }
        } else {
            match ea.mode {
                AddressingMode::DataDirect(_) => 4,
                AddressingMode::AddressDirect(_) => 4,
                _ => 8,
            }
        };

        InstructionResult::prefetch(cycles + ea.cycles)
    }

    /// SWAP - Swap register halves
    /// Opcode: 0100100001000rrr
    /// Swaps upper and lower 16 bits of a data register
    pub fn swap(&mut self, reg: u8) -> InstructionResult {
        let value = self.d[reg as usize];
        let swapped = value.rotate_left(16);
        self.d[reg as usize] = swapped;

        // Update flags based on 32-bit result
        self.update_flags_nz(swapped, Size::Long);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

        InstructionResult::new(4, 2)
    }

    /// EXT - Sign Extend
    /// EXT.W Dn: sign-extend byte to word (bit 7 -> bits 15-8)
    /// EXT.L Dn: sign-extend word to long (bit 15 -> bits 31-16)
    pub fn ext(&mut self, reg: u8, size: Size) -> InstructionResult {
        let value = self.d[reg as usize];
        let result = match size {
            Size::Word => {
                // Sign-extend byte to word, keep upper 16 bits unchanged
                let extended = (value as i8) as i16 as u16;
                (value & 0xFFFF0000) | extended as u32
            }
            Size::Long => {
                // Sign-extend word to long
                (value as i16) as i32 as u32
            }
            _ => unreachable!("EXT only supports Word and Long"),
        };
        self.d[reg as usize] = result;

        // Update flags: N and Z based on result, V and C cleared
        self.update_flags_nz(result, size);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

        InstructionResult::new(4, 2)
    }

    /// EXG - Exchange registers
    /// Opcode: 1100rrr1oooooRRR
    /// where ooooo determines exchange mode:
    /// - 01000: Data registers (Dx <-> Dy)
    /// - 01001: Address registers (Ax <-> Ay)
    /// - 10001: Data and Address (Dx <-> Ay)
    pub fn exg(&mut self, rx: u8, ry: u8, opmode: u8) -> InstructionResult {
        match opmode {
            0b01000 => {
                // Exchange data registers
                self.d.swap(rx as usize, ry as usize);
            }
            0b01001 => {
                // Exchange address registers
                self.a.swap(rx as usize, ry as usize);
            }
            0b10001 => {
                // Exchange Dx with Ay
                std::mem::swap(&mut self.d[rx as usize], &mut self.a[ry as usize]);
            }
            _ => {
                // Invalid opmode - treat as NOP
            }
        }

        // EXG does not affect flags
        InstructionResult::new(6, 2)
    }

    /// LINK - Link and allocate
    /// Opcode: 0100111001010rrr
    /// Pushes An, copies SP to An, adds displacement to SP
    pub fn link(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        reg: u8,
    ) -> InstructionResult {
        // Read 16-bit displacement from prefetch (sign-extended)
        let displacement = prefetch.read_word(bus) as i16 as i32 as u32;

        // Push address register onto stack
        self.a[7] = self.a[7].wrapping_sub(4);
        let sp = self.a[7];

        bus.write_word(sp, (self.a[reg as usize] >> 16) as u16);
        bus.write_word(sp.wrapping_add(2), self.a[reg as usize] as u16);

        // Copy SP to An
        self.a[reg as usize] = self.a[7];

        // Add displacement to SP
        self.a[7] = self.a[7].wrapping_add(displacement);

        InstructionResult::prefetch(16)
    }

    /// UNLK - Unlink
    /// Opcode: 0100111001011rrr
    /// Copies An to SP, pops An from stack
    pub fn unlk(
        &mut self,
        bus: &mut dyn Addressable,
        _prefetch: &mut Prefetch,
        reg: u8,
    ) -> InstructionResult {
        // Copy An to SP
        let sp = self.a[reg as usize];
        self.a[7] = sp;

        // Pop An from stack (Big Endian: high word first)
        let high = bus.read_word(sp) as u32;
        let low = bus.read_word(sp.wrapping_add(2)) as u32;
        let value = (high << 16) | low;

        // Update An with popped value
        self.a[reg as usize] = value;

        // Increment SP by 4 - UNLESS reg is A7 itself
        // When reg==7, we just restored A7 from stack, don't add 4
        if reg != 7 {
            self.a[7] = sp.wrapping_add(4);
        }

        InstructionResult::prefetch(12)
    }

    /// MOVEM - Move Multiple Registers
    /// Opcode: 01001d001smmmrrr
    pub fn movem(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        mode: u8,
        reg: u8,
        to_memory: bool,
    ) -> InstructionResult {
        // MOVEM supports only Word and Long sizes on 68000.
        debug_assert!(matches!(size, Size::Word | Size::Long));

        // Register mask word comes from the instruction stream (prefetch).
        // Bits: 0..7 = D0..D7, 8..15 = A0..A7.
        let mask = prefetch.read_word(bus);

        // We don't use ea.read/ea.write because MOVEM performs multiple transfers.
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);

        // Compute base PC for address calculation.
        //
        // Important MOVEM detail:
        // There is an extra 16-bit register-mask word *before* the EA extension words.
        // So PC-relative modes (d16,PC)/(d8,PC,Xn) are effectively based on (instruction_pc + 4),
        // not (instruction_pc + 2).
        let base_pc = match ea.mode {
            AddressingMode::PcDisplacement(_) | AddressingMode::PcIndex(_, _, _) => {
                prefetch.base().wrapping_add(2)
            }
            _ => prefetch.base(),
        };

        // Determine starting address for transfers.
        let mut addr = match ea.mode {
            AddressingMode::AddressIndirect(r)
            | AddressingMode::AddressIndirectPostInc(r)
            | AddressingMode::AddressIndirectPreDec(r) => self.a[r as usize],
            _ => ea.calculate_address(self, base_pc),
        };

        let bytes_per = size.bytes();
        let regs_count = mask.count_ones();

        // Transfer loop.
        if to_memory {
            // Registers -> Memory
            if matches!(ea.mode, AddressingMode::AddressIndirectPreDec(_)) {
                // -(An): special mask mapping on 68000.
                //
                // For predecrement destination, the MOVEM mask bits map as:
                // - bits 0..7  => A7..A0
                // - bits 8..15 => D7..D0
                // For correct Address Error behavior (and Harte expectations), -(An) must be
                // performed *per transfer* (decrement, then access). The access address that
                // faults is the per-step predecremented address, not the final base.
                //
                // With this per-step approach, iterating mask bits from low->high (0..15)
                // results in the correct final memory layout (highest-numbered transfers end up
                // at the lowest addresses).
                let original_an = self.a[reg as usize];

                for bit in 0..16 {
                    if (mask >> bit) & 1 == 0 {
                        continue;
                    }

                    let value = if bit < 8 {
                        // A7..A0
                        let a_reg = 7 - bit;
                        self.a[a_reg as usize]
                    } else {
                        // D7..D0
                        let d_reg = 15 - bit;
                        self.d[d_reg as usize]
                    };

                    match size {
                        Size::Word => {
                            let next_addr = addr.wrapping_sub(2);
                            if !self.check_address_alignment_with_prefetch(
                                next_addr,
                                Size::Word,
                                prefetch,
                                true,
                            ) {
                                // If the access faults, do not commit the failing decrement.
                                self.a[reg as usize] = original_an;
                                return InstructionResult::prefetch(
                                    get_move_exception_source_cycles(&ea, size) - 2,
                                );
                            }
                            addr = next_addr;
                            bus.write_word(addr, value as u16);
                        }
                        Size::Long => {
                            // 68000 uses a 16-bit bus: long transfers are two word accesses.
                            // For -(An), it predecrements by 2 for each word access:
                            // - first word access at (An - 2) (LOW word)
                            // - second word access at (An - 4) (HIGH word)
                            //
                            // This matters for Address Error stack frames (Tom Harte):
                            // the faulting address for an odd An is (An - 2), not (An - 4).
                            let low = value as u16;
                            let high = (value >> 16) as u16;

                            // LOW word @ (An - 2)
                            let next_addr_low = addr.wrapping_sub(2);
                            if !self.check_address_alignment_with_prefetch(
                                next_addr_low,
                                Size::Word,
                                prefetch,
                                true,
                            ) {
                                self.a[reg as usize] = original_an;
                                return InstructionResult::prefetch(
                                    get_move_exception_source_cycles(&ea, size) - 2,
                                );
                            }
                            addr = next_addr_low;
                            bus.write_word(addr, low);

                            // HIGH word @ (An - 4)
                            let next_addr_high = addr.wrapping_sub(2);

                            addr = next_addr_high;
                            bus.write_word(addr, high);
                        }
                        _ => unreachable!(),
                    }
                }

                // Commit final address after successful completion.
                self.a[reg as usize] = addr;
            } else {
                // Normal order: D0..D7, A0..A7.
                for i in 0..16 {
                    if (mask >> i) & 1 == 0 {
                        continue;
                    }

                    let value = if i < 8 {
                        self.d[i as usize]
                    } else {
                        self.a[(i - 8) as usize]
                    };

                    if !self.movem_write_mem(bus, prefetch, size, addr, value) {
                        // For (An)+ we should preserve progress; for other modes no An update.
                        if matches!(ea.mode, AddressingMode::AddressIndirectPostInc(_)) {
                            // Tom Harte expectation: post-increment happens even if the access faults.
                            let inc = if size == Size::Long { 2 } else { bytes_per };
                            self.a[reg as usize] = addr.wrapping_add(inc);
                        }
                        return InstructionResult::prefetch(get_move_exception_source_cycles(
                            &ea, size,
                        ));
                    }

                    addr = addr.wrapping_add(bytes_per);
                }

                // Post-increment: update An after all transfers.
                if matches!(ea.mode, AddressingMode::AddressIndirectPostInc(_)) {
                    self.a[reg as usize] = addr;
                }
            }
        } else {
            // Memory -> Registers (always D0..D7, A0..A7 order).
            for i in 0..16 {
                if (mask >> i) & 1 == 0 {
                    continue;
                }

                let Some(value) = self.movem_read_mem(bus, prefetch, size, addr) else {
                    if matches!(ea.mode, AddressingMode::AddressIndirectPostInc(_)) {
                        // Tom Harte expectation: post-increment happens even if the access faults.
                        let inc = if size == Size::Long { 2 } else { bytes_per };
                        self.a[reg as usize] = addr.wrapping_add(inc);
                    }
                    return InstructionResult::prefetch(get_move_exception_source_cycles(
                        &ea, size,
                    ));
                };

                if i < 8 {
                    self.d[i as usize] = value;
                } else {
                    self.a[(i - 8) as usize] = value;
                }

                addr = addr.wrapping_add(bytes_per);
            }

            // Post-increment: update An after all transfers.
            if matches!(ea.mode, AddressingMode::AddressIndirectPostInc(_)) {
                self.a[reg as usize] = addr;
            }
        }

        // Timing (total cycles). This codebase currently expects `cpu.step()` to return
        // the full cycle count for non-exceptional execution paths (Harte runner).
        //
        // MC68000 reference formulas (total cycles):
        // - Regs->Mem: base + (4*n) for word, base + (8*n) for long
        // - Mem->Regs: base + (4*n) for word, base + (8*n) for long
        // Where base depends on EA complexity and direction.
        let per_reg = if size == Size::Long { 4 } else { 0 };
        let base = match (to_memory, &ea.mode) {
            // -(An) has a slightly different base on 68000 (commonly quoted as 8 + ...).
            (true, AddressingMode::AddressIndirectPreDec(_)) => 8,

            (true, AddressingMode::AddressIndirect(_)) => 8,
            // (An)/(An)+ tend to be 12-ish for mem->regs and 8/12-ish for regs->mem.
            (true, AddressingMode::AddressIndirectDisplacement(_, _))
            | (true, AddressingMode::AbsoluteShort(_)) => 12,

            (true, AddressingMode::AddressIndirectIndex(_, _, _, _)) => 14,

            (true, AddressingMode::AbsoluteLong(_)) => 16,

            (false, AddressingMode::AddressIndirect(_))
            | (false, AddressingMode::AddressIndirectPostInc(_)) => 12,

            (false, AddressingMode::AddressIndirectDisplacement(_, _))
            | (false, AddressingMode::AbsoluteShort(_))
            | (false, AddressingMode::PcDisplacement(_)) => 16,

            (false, AddressingMode::AddressIndirectIndex(_, _, _, _))
            | (false, AddressingMode::PcIndex(_, _, _)) => 18,

            (false, AddressingMode::AbsoluteLong(_)) => 20,

            // Control/absolute/PC-relative modes are slower.
            (true, _) => 16,
            (false, _) => 16,
        };

        let cycles = base + (4 + per_reg) * regs_count;
        InstructionResult::prefetch(cycles)
    }

    #[inline]
    fn movem_read_mem(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        addr: u32,
    ) -> Option<u32> {
        if !self.check_address_alignment_with_prefetch(addr, Size::Word, prefetch, false) {
            return None;
        }

        let v = match size {
            Size::Word => bus.read_word(addr) as i16 as i32 as u32, // sign-extend
            Size::Long => {
                let hi = bus.read_word(addr) as u32;
                let lo = bus.read_word(addr.wrapping_add(2)) as u32;
                (hi << 16) | lo
            }
            _ => unreachable!(),
        };

        Some(v)
    }

    #[inline]
    fn movem_write_mem(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        addr: u32,
        value: u32,
    ) -> bool {
        if !self.check_address_alignment_with_prefetch(addr, Size::Word, prefetch, true) {
            return false;
        }

        match size {
            Size::Word => bus.write_word(addr, value as u16),
            Size::Long => {
                bus.write_word(addr, (value >> 16) as u16);
                bus.write_word(addr.wrapping_add(2), value as u16);
            }
            _ => unreachable!(),
        }

        true
    }

    /// MOVEP - Move Peripheral Data
    /// Opcode: 0000ddd1mm001aaa
    /// Transfers data between data register and alternate bytes in memory
    pub fn movep(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        data_reg: u8,
        addr_reg: u8,
        to_memory: bool,
    ) -> InstructionResult {
        // Read displacement from the instruction stream (prefetch).
        let displacement = prefetch.read_word(bus) as i16 as i32 as u32;

        // Calculate base address: d16(An)
        let base_addr = self.a[addr_reg as usize].wrapping_add(displacement);

        if to_memory {
            // Data register to memory (alternate bytes)
            let value = self.d[data_reg as usize];

            match size {
                Size::Word => {
                    // MOVEP.W uses the low 16 bits of Dn, written as two bytes at +0 and +2.
                    let w = (value & 0xFFFF) as u16;
                    bus.write(base_addr, (w >> 8) as u8);
                    bus.write(base_addr.wrapping_add(2), w as u8);
                }
                Size::Long => {
                    bus.write(base_addr, (value >> 24) as u8);
                    bus.write(base_addr.wrapping_add(2), (value >> 16) as u8);
                    bus.write(base_addr.wrapping_add(4), (value >> 8) as u8);
                    bus.write(base_addr.wrapping_add(6), value as u8);
                }
                _ => unreachable!(),
            }
        } else {
            // Memory to data register (alternate bytes)
            let value = match size {
                Size::Word => {
                    let high = bus.read(base_addr) as u32;
                    let low = bus.read(base_addr.wrapping_add(2)) as u32;
                    (high << 8) | low
                }
                Size::Long => {
                    let b0 = bus.read(base_addr) as u32;
                    let b1 = bus.read(base_addr.wrapping_add(2)) as u32;
                    let b2 = bus.read(base_addr.wrapping_add(4)) as u32;
                    let b3 = bus.read(base_addr.wrapping_add(6)) as u32;
                    (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
                }
                _ => unreachable!(),
            };

            if size == Size::Word {
                // MOVEP.W updates only the low word of Dn; high word is preserved.
                let old = self.d[data_reg as usize];
                self.d[data_reg as usize] = (old & 0xFFFF_0000) | (value & 0xFFFF);
            } else {
                self.d[data_reg as usize] = value;
            }
        }

        // Cycles: 16 for word, 24 for long
        let cycles = if size == Size::Word { 16 } else { 24 };
        InstructionResult::prefetch(cycles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_move_data_direct() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0x12345678;
        cpu.pc = 0x1000;

        // MOVE.L D0, D1
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.move_instr(&mut ram, &mut prefetch, Size::Long, 0, 1, 0, 0);

        assert_eq!(cpu.d[1], 0x12345678);
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
        assert!(!cpu.get_flag(super::super::Flag::V));
        assert!(!cpu.get_flag(super::super::Flag::C));
    }

    #[test]
    fn test_move_zero_sets_flags() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0;
        cpu.pc = 0x1000;

        // MOVE.W D0, D1
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.move_instr(&mut ram, &mut prefetch, Size::Word, 0, 1, 0, 0);

        assert_eq!(cpu.d[1], 0);
        assert!(cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_move_negative_sets_flags() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0x8000; // Negative word
        cpu.pc = 0x1000;

        // MOVE.W D0, D1
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.move_instr(&mut ram, &mut prefetch, Size::Word, 0, 1, 0, 0);

        assert_eq!(cpu.d[1], 0x8000);
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_movea_word() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0x8000; // Negative word
        cpu.pc = 0x1000;

        // MOVEA.W D0, A1
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.movea(&mut ram, &mut prefetch, Size::Word, 1, 0, 0);

        // Should sign-extend: 0x8000 -> 0xFFFF8000
        assert_eq!(cpu.a[1], 0xFFFF8000);
    }

    #[test]
    fn test_movea_long() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0x12345678;
        cpu.pc = 0x1000;

        // MOVEA.L D0, A1
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.movea(&mut ram, &mut prefetch, Size::Long, 1, 0, 0);

        assert_eq!(cpu.a[1], 0x12345678);
    }

    #[test]
    fn test_moveq() {
        let mut cpu = M68k::new();

        // MOVEQ #0x7F, D0 (positive)
        let result = cpu.moveq(0, 0x7F);

        assert_eq!(cpu.d[0], 0x0000007F);
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
        assert_eq!(result.cycles, 4);

        // MOVEQ #-1, D1 (negative, sign-extended)
        let result = cpu.moveq(1, -1);

        assert_eq!(cpu.d[1], 0xFFFFFFFF);
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(cpu.get_flag(super::super::Flag::N));

        // MOVEQ #0, D2
        let result = cpu.moveq(2, 0);

        assert_eq!(cpu.d[2], 0);
        assert!(cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_lea_absolute() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x1000;
        // Absolute short address: 0x5000 (sign-extended to 0x00005000).
        // The prefetch queue starts AFTER the opcode at PC, so extension
        // words live at PC+2 (not at PC).
        ram.write_word(0x1002, 0x5000);

        // LEA (0x5000).W, A0
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.lea(&mut ram, &mut prefetch, 0, 7, 0); // Mode 7 reg 0 = Absolute Short

        assert_eq!(cpu.a[0], 0x00005000);
    }

    #[test]
    fn test_lea_address_indirect_displacement() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.a[1] = 0x1000;
        cpu.pc = 0x2000;
        // Extension word lives at PC+2 (prefetch starts after the opcode).
        ram.write_word(0x2002, 0x0100); // Displacement = 0x0100

        // LEA $100(A1), A0
        let mut prefetch = Prefetch::new(0x2000);
        let result = cpu.lea(&mut ram, &mut prefetch, 0, 5, 1); // Mode 5 = (d16,An)

        assert_eq!(cpu.a[0], 0x1100); // 0x1000 + 0x0100
    }

    #[test]
    fn test_clr_byte() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0x12345678;

        let mut prefetch = Prefetch::new(0);
        let result = cpu.clr(&mut ram, &mut prefetch, Size::Byte, 0, 0);

        assert_eq!(cpu.d[0] & 0xFF, 0);
        assert!(cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
        assert!(!cpu.get_flag(super::super::Flag::V));
        assert!(!cpu.get_flag(super::super::Flag::C));
    }

    #[test]
    fn test_clr_word() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0x12345678;

        let mut prefetch = Prefetch::new(0);
        let result = cpu.clr(&mut ram, &mut prefetch, Size::Word, 0, 0);

        assert_eq!(cpu.d[0] & 0xFFFF, 0);
        assert!(cpu.get_flag(super::super::Flag::Z));
    }

    #[test]
    fn test_swap() {
        let mut cpu = M68k::new();

        cpu.d[0] = 0x12345678;

        let result = cpu.swap(0);

        assert_eq!(cpu.d[0], 0x56781234);
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_swap_zero() {
        let mut cpu = M68k::new();

        cpu.d[0] = 0x00000000;

        let result = cpu.swap(0);

        assert_eq!(cpu.d[0], 0x00000000);
        assert!(cpu.get_flag(super::super::Flag::Z));
    }

    #[test]
    fn test_exg_data_registers() {
        let mut cpu = M68k::new();

        cpu.d[0] = 0x11111111;
        cpu.d[1] = 0x22222222;

        let result = cpu.exg(0, 1, 0b01000);

        assert_eq!(cpu.d[0], 0x22222222);
        assert_eq!(cpu.d[1], 0x11111111);
    }

    #[test]
    fn test_exg_address_registers() {
        let mut cpu = M68k::new();

        cpu.a[0] = 0xAAAAAAAA;
        cpu.a[1] = 0xBBBBBBBB;

        let result = cpu.exg(0, 1, 0b01001);

        assert_eq!(cpu.a[0], 0xBBBBBBBB);
        assert_eq!(cpu.a[1], 0xAAAAAAAA);
    }

    #[test]
    fn test_exg_data_address() {
        let mut cpu = M68k::new();

        cpu.d[2] = 0x12345678;
        cpu.a[3] = 0x87654321;

        let result = cpu.exg(2, 3, 0b10001);

        assert_eq!(cpu.d[2], 0x87654321);
        assert_eq!(cpu.a[3], 0x12345678);
    }

    #[test]
    fn test_pea() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.a[7] = 0x1000; // Stack pointer
        cpu.a[1] = 0x5000;

        // PEA (A1)
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.pea(&mut ram, &mut prefetch, 2, 1);

        // SP should be decremented by 4
        assert_eq!(cpu.a[7], 0x0FFC);

        // Read back the address from stack using Addressable trait
        use retro_core::cpu::Addressable;
        let high = ram.read_word(0x0FFC) as u32;
        let low = ram.read_word(0x0FFE) as u32;
        let pushed_addr = (high << 16) | low;
        assert_eq!(pushed_addr, 0x5000);
    }

    #[test]
    fn test_link() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.a[6] = 0xAAAAAAAA; // Frame pointer
        cpu.a[7] = 0x2000; // Stack pointer
        cpu.pc = 0x1000;

        // Displacement: -16. Extension word at PC+2.
        ram.write_word(0x1002, 0xFFF0);

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.link(&mut ram, &mut prefetch, 6);

        // Old A6 should be pushed
        assert_eq!(ram.read_word(0x1FFC), 0xAAAA);
        assert_eq!(ram.read_word(0x1FFE), 0xAAAA);

        // A6 = old SP
        assert_eq!(cpu.a[6], 0x1FFC);

        // SP = A6 + displacement (-16)
        assert_eq!(cpu.a[7], 0x1FFC - 16);
    }

    #[test]
    fn test_unlk() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.a[6] = 0x1FF0; // Frame pointer
        cpu.a[7] = 0x1FE0; // Stack pointer (doesn't matter)

        // Old A6 value on stack (Big Endian: high word first, then low word)
        ram.write_word(0x1FF0, 0x1234); // High word
        ram.write_word(0x1FF2, 0x5678); // Low word

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.unlk(&mut ram, &mut prefetch, 6);

        // SP restored from A6
        assert_eq!(cpu.a[7], 0x1FF4); // 0x1FF0 + 4

        // A6 restored from stack (Big Endian: 0x1234 << 16 | 0x5678 = 0x12345678)
        assert_eq!(cpu.a[6], 0x12345678);
    }

    #[test]
    fn test_movem_to_memory() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0x11111111;
        cpu.d[1] = 0x22222222;
        cpu.a[0] = 0x1000;
        cpu.pc = 0x2000;

        // Mask: D0 and D1 (bits 0 and 1). Extension word at PC+2.
        ram.write_word(0x2002, 0x0003);

        let mut prefetch = Prefetch::new(0x2000);
        let result = cpu.movem(&mut ram, &mut prefetch, Size::Long, 2, 0, true); // (A0)

        // Check memory
        let d0_addr = 0x1000;
        let d1_addr = 0x1004;

        use retro_core::cpu::Addressable;
        let d0_high = ram.read_word(d0_addr) as u32;
        let d0_low = ram.read_word(d0_addr + 2) as u32;
        assert_eq!((d0_high << 16) | d0_low, 0x11111111);

        let d1_high = ram.read_word(d1_addr) as u32;
        let d1_low = ram.read_word(d1_addr + 2) as u32;
        assert_eq!((d1_high << 16) | d1_low, 0x22222222);
    }

    #[test]
    fn test_movem_from_memory() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        use retro_core::cpu::Addressable;

        // Setup memory
        ram.write_word(0x1000, 0x1111);
        ram.write_word(0x1002, 0x1111);
        ram.write_word(0x1004, 0x2222);
        ram.write_word(0x1006, 0x2222);

        cpu.a[0] = 0x1000;
        cpu.pc = 0x2000;

        // Mask: D0 and D1. Extension word at PC+2.
        ram.write_word(0x2002, 0x0003);

        let mut prefetch = Prefetch::new(0x2000);
        let result = cpu.movem(&mut ram, &mut prefetch, Size::Long, 2, 0, false); // (A0)

        assert_eq!(cpu.d[0], 0x11111111);
        assert_eq!(cpu.d[1], 0x22222222);
    }

    #[test]
    fn test_movep_to_memory_word() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0x12345678;
        cpu.a[0] = 0x1000;
        cpu.pc = 0x2000;

        ram.write_word(0x2002, 0x0004); // Displacement = 4 (extension at PC+2)

        let mut prefetch = Prefetch::new(0x2000);
        let result = cpu.movep(&mut ram, &mut prefetch, Size::Word, 0, 0, true);

        // Should write 0x56 at 0x1004 and 0x78 at 0x1006
        use retro_core::cpu::Addressable;
        assert_eq!(ram.read(0x1004), 0x56);
        assert_eq!(ram.read(0x1006), 0x78);
    }

    #[test]
    fn test_movep_from_memory_word() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        use retro_core::cpu::Addressable;

        ram.write(0x1004, 0xAB);
        ram.write(0x1006, 0xCD);

        cpu.a[0] = 0x1000;
        cpu.pc = 0x2000;

        ram.write_word(0x2002, 0x0004); // Displacement = 4 (extension at PC+2)

        let mut prefetch = Prefetch::new(0x2000);
        let result = cpu.movep(&mut ram, &mut prefetch, Size::Word, 0, 0, false);

        assert_eq!(cpu.d[0] & 0xFFFF, 0xABCD);
    }
}
