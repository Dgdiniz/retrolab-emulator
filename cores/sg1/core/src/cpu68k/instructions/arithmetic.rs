//! Arithmetic instructions: ADD, ADDI, ADDQ, SUB, SUBI, SUBQ.

use super::{InstructionResult, M68k};
use crate::cpu68k::{
    addressing::{AddressingMode, EffectiveAddress, Size},
    decode,
    prefetch::Prefetch,
};
use retro_core::cpu::Addressable;

fn ea_to_index(mode: &AddressingMode) -> usize {
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

fn get_exception_cycles(src_ea: &EffectiveAddress, size: Size) -> u32 {
    let src_idx = ea_to_index(&src_ea.mode);

    // Cycle tables for MOVE Byte/Word
    // Rows = Source EA, Columns = Destination EA
    #[rustfmt::skip]
    const MOVE_BYTE_WORD_CYCLES: [u32; 12] = [
        // Dn    An    (An)  (An)+ -(An) d(An) d(An,ix) xxx.W xxx.L d(PC) d(PC,ix) #xxx
          10,   10,    10,   10,   12,   10,   12,     10,   10,   10,   12,      10  
        ];

    // Cycle tables for MOVE Long
    #[rustfmt::skip]
    const MOVE_LONG_CYCLES: [u32; 12] = [
        // Dn    An    (An)  (An)+ -(An) d(An) d(An,ix) xxx.W xxx.L d(PC) d(PC,ix) #xxx
          10,   10,    10,   10,   12,   10,   12,     10,   10,   10,   12,      10  
        ];

    let table = match size {
        Size::Byte | Size::Word => &MOVE_BYTE_WORD_CYCLES,
        Size::Long => &MOVE_LONG_CYCLES,
    };

    table[src_idx]
}

impl M68k {
    /// ADD - Add
    /// Opcode: 1101rrrmssssseee
    /// where rrr = data register, m = direction (0=ea+Dn->Dn, 1=Dn+ea->ea)
    #[allow(clippy::too_many_arguments)]
    pub fn add(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        reg: u8,
        mode: u8,
        ea_reg: u8,
        to_ea: bool,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, ea_reg, size);

        let (result, src, dst, cycles) = if to_ea {
            // Dn + <ea> -> <ea>
            let src = self.d[reg as usize] & size.mask();
            let dst = ea.read(self, bus, prefetch, size);

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let result = src.wrapping_add(dst);

            ea.write(self, bus, prefetch, size, result);

            let cycles = if size == Size::Long { 12 } else { 8 };

            (result, src, dst, cycles + ea.cycles)
        } else {
            // <ea> + Dn -> Dn
            let src = ea.read(self, bus, prefetch, size);

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let dst = self.d[reg as usize] & size.mask();
            let result = src.wrapping_add(dst);

            let mask = size.mask();
            self.d[reg as usize] = (self.d[reg as usize] & !mask) | (result & mask);

            let cycles = if size == Size::Long {
                match ea.mode {
                    AddressingMode::DataDirect(_) => 8,
                    AddressingMode::AddressDirect(_) => 8,
                    AddressingMode::Immediate(_) => 8,
                    _ => 6,
                }
            } else {
                4
            };

            (result, src, dst, cycles + ea.cycles)
        };

        self.update_flags_nzvxc(result, dst, src, size, false);

        InstructionResult::prefetch(cycles)
    }

    /// ADDI
    /// Opcode: 00000110ssmmmrrr
    pub fn addi(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        let immediate = match size {
            Size::Byte | Size::Word => {
                let value = prefetch.read_word(bus) as u32;
                value & size.mask()
            }
            Size::Long => prefetch.read_long(bus),
        };

        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);

        let dst = ea.read(self, bus, prefetch, size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, size));
        }

        let result = immediate.wrapping_add(dst); // Perform addition

        ea.write(self, bus, prefetch, size, result);

        self.update_flags_nzvxc(result, dst, immediate, size, false);

        let cycles = if size == Size::Long {
            match ea.mode {
                AddressingMode::DataDirect(_) => 16,
                _ => 20,
            }
        } else {
            match ea.mode {
                AddressingMode::DataDirect(_) => 8,
                _ => 12,
            }
        };

        InstructionResult::prefetch(cycles + ea.cycles)
    }

    /// ADDQ - Add Quick (1-8 immediate to EA)
    /// Opcode: 0101ddd0ssmmmrrr
    pub fn addq(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        immediate: u8,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        // Immediate is 3-bit value (1-8), where 0 represents 8
        let imm_value = if immediate == 0 { 8 } else { immediate as u32 };

        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);

        // For address registers, no flags are affected and size is always long
        if mode == 1 {
            // An
            let result = self.a[reg as usize].wrapping_add(imm_value);
            self.a[reg as usize] = result;

            InstructionResult::prefetch(if size == Size::Long { 6 } else { 8 })
        } else {
            // Other EA
            let dst = ea.read(self, bus, prefetch, size);

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let result = dst.wrapping_add(imm_value);

            ea.write(self, bus, prefetch, size, result);

            self.update_flags_nzvxc(result, dst, imm_value, size, false);

            let cycles = if size == Size::Long {
                match ea.mode {
                    AddressingMode::DataDirect(_) => 8,
                    _ => 12,
                }
            } else {
                match ea.mode {
                    AddressingMode::DataDirect(_) => 4,
                    _ => 8,
                }
            };

            InstructionResult::prefetch(cycles + ea.cycles)
        }
    }

    /// ADDA - Add to Address Register
    /// Opcode: 1101rrr0s1mmmrrr (s = size: 0=word, 1=long)
    /// Adds <ea> to An (no flags affected)
    pub fn adda(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        reg: u8,
        mode: u8,
        ea_reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, ea_reg, size);

        let src = ea.read(self, bus, prefetch, size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, size));
        }

        // Sign-extend if word size
        let src = if size == Size::Word {
            src as i16 as i32 as u32
        } else {
            src
        };

        self.a[reg as usize] = self.a[reg as usize].wrapping_add(src);

        // ADDA does not affect flags

        let cycles = if size == Size::Long {
            match ea.mode {
                AddressingMode::DataDirect(_) => 8,
                AddressingMode::AddressDirect(_) => 8,
                AddressingMode::Immediate(_) => 8,
                _ => 6,
            }
        } else {
            8
        };

        InstructionResult::prefetch(cycles + ea.cycles)
    }

    /// ADDX - Add Extended
    /// Opcode: 1101rrr1ss00mrrr (register) or 1101rrr1ss001rrr (memory)
    /// Adds with extend flag
    pub fn addx(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        dst_reg: u8,
        src_reg: u8,
        memory_mode: bool,
    ) -> InstructionResult {
        let x = if self.get_flag(super::Flag::X) { 1 } else { 0 };

        let (result, src, dst, cycles) = if memory_mode {
            if size == Size::Long {
                // Special handling for Long: process as two Word operations
                // to ensure correct register state on Address Error.
                // -(An) mode decrements by 2 for each word access, not 4 at once.

                // Create EA for -(An) with Word size
                let src_ea =
                    decode::decode_ea_with_cycles(self, bus, prefetch, 4, src_reg, Size::Word);
                let dst_ea =
                    decode::decode_ea_with_cycles(self, bus, prefetch, 4, dst_reg, Size::Word);

                // With -(An), first decrement reads the LOW word (higher address),
                // second decrement reads the HIGH word (lower address)

                // Read source LOW word first (this decrements src An by 2)
                let src_low = src_ea.read(self, bus, prefetch, Size::Word);

                if self.address_error.is_some() {
                    return InstructionResult::prefetch(12);
                }

                // Read source HIGH word (this decrements src An by 2 more)
                let src_high = src_ea.read(self, bus, prefetch, Size::Word);

                // Read destination LOW word first (this decrements dst An by 2)
                let dst_low = dst_ea.read(self, bus, prefetch, Size::Word);

                if self.address_error.is_some() {
                    return InstructionResult::prefetch(12);
                }

                // Read destination HIGH word (this decrements dst An by 2 more)
                let dst_high = dst_ea.read(self, bus, prefetch, Size::Word);

                // Assemble 32-bit values from high and low words
                let src = (src_high << 16) | src_low;
                let dst = (dst_high << 16) | dst_low;

                // Perform 32-bit addition with extend
                let result = src.wrapping_add(dst.wrapping_add(x));

                dst_ea.write(self, bus, prefetch, Size::Long, result);

                (result, src, dst, 30)
            } else {
                // Byte/Word: standard EA processing (single access)
                let src_ea = decode::decode_ea_with_cycles(self, bus, prefetch, 4, src_reg, size);
                let dst_ea = decode::decode_ea_with_cycles(self, bus, prefetch, 4, dst_reg, size);

                // -(An) + -(An) -> -(An)
                let src = src_ea.read(self, bus, prefetch, size);

                if self.address_error.is_some() {
                    return InstructionResult::prefetch(get_exception_cycles(&src_ea, size));
                }

                let dst = dst_ea.read(self, bus, prefetch, size);

                if self.address_error.is_some() {
                    return InstructionResult::prefetch(get_exception_cycles(&dst_ea, size));
                }

                let result = src.wrapping_add(dst.wrapping_add(x));

                dst_ea.write(self, bus, prefetch, size, result);

                (result, src, dst, 18)
            }
        } else {
            // Dn + Dn -> Dn
            let src = self.d[src_reg as usize] & size.mask();
            let dst = self.d[dst_reg as usize] & size.mask();

            let result = src.wrapping_add(dst + x);

            let mask = size.mask();
            self.d[dst_reg as usize] = (self.d[dst_reg as usize] & !mask) | (result & mask);

            let cycles = if size == Size::Long { 8 } else { 4 };
            (result, src, dst, cycles)
        };

        // Save Z flag, because for addx, if the result is 0, the Z flag should remain the same as before the operation
        let old_z = self.get_flag(super::Flag::Z);

        // Update flags normally
        self.update_flags_nzvxc(result, dst, src, size, false);

        // Fix carry flag to consider all 3 operands: src + dst + x, because update_flags_nzvxc
        // only considers 2 operands: dst + src
        let mask = size.mask();
        let full_result = (src as u64) + (dst as u64) + (x as u64);
        let carry = full_result > mask as u64;
        self.set_flag(super::Flag::C, carry);
        self.set_flag(super::Flag::X, carry);

        // Special Z flag behavior for ADDX: only clear if result != 0, otherwise keep old value
        if (result & mask) != 0 {
            self.set_flag(super::Flag::Z, false);
        } else {
            self.set_flag(super::Flag::Z, old_z); // Restore old Z if result is zero
        }

        InstructionResult::prefetch(cycles)
    }

    /// SUB - Subtract
    /// Opcode: 1001rrrmssssseee
    /// where rrr = data register, m = direction (0=Dn-ea->Dn, 1=ea-Dn->ea)
    #[allow(clippy::too_many_arguments)]
    pub fn sub(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        reg: u8,
        mode: u8,
        ea_reg: u8,
        to_ea: bool,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, ea_reg, size);

        let (result, src, dst, cycles) = if to_ea {
            // <ea> - Dn -> <ea>
            let src = self.d[reg as usize] & size.mask();
            let dst = ea.read(self, bus, prefetch, size);

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let result = dst.wrapping_sub(src);

            ea.write(self, bus, prefetch, size, result);

            let cycles = if size == Size::Long { 12 } else { 8 };

            (result, src, dst, cycles + ea.cycles)
        } else {
            // Dn - <ea> -> Dn
            let src = ea.read(self, bus, prefetch, size);

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let dst = self.d[reg as usize] & size.mask();
            let result = dst.wrapping_sub(src);

            let mask = size.mask();
            self.d[reg as usize] = (self.d[reg as usize] & !mask) | (result & mask);

            let cycles = if size == Size::Long {
                match ea.mode {
                    AddressingMode::DataDirect(_) => 8,
                    AddressingMode::AddressDirect(_) => 8,
                    AddressingMode::Immediate(_) => 8,
                    _ => 6,
                }
            } else {
                4
            };

            (result, src, dst, cycles + ea.cycles)
        };

        self.update_flags_nzvxc(result, dst, src, size, true);

        InstructionResult::prefetch(cycles)
    }

    /// SUBI - Subtract Immediate
    /// Opcode: 00000100ssmmmrrr
    pub fn subi(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        let immediate = match size {
            Size::Byte | Size::Word => {
                let value = prefetch.read_word(bus) as u32;
                value & size.mask()
            }
            Size::Long => prefetch.read_long(bus),
        };

        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);

        let dst = ea.read(self, bus, prefetch, size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, size));
        }

        let result = dst.wrapping_sub(immediate); // Perform subtraction

        ea.write(self, bus, prefetch, size, result);

        self.update_flags_nzvxc(result, dst, immediate, size, true);

        let cycles = if size == Size::Long {
            match ea.mode {
                AddressingMode::DataDirect(_) => 16,
                _ => 20,
            }
        } else {
            match ea.mode {
                AddressingMode::DataDirect(_) => 8,
                _ => 12,
            }
        };

        InstructionResult::prefetch(cycles + ea.cycles)
    }

    /// SUBQ - Subtract Quick (1-8 immediate from EA)
    /// Opcode: 0101ddd1ssmmmrrr
    pub fn subq(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        immediate: u8,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        // Immediate is 3-bit value (1-8), where 0 represents 8
        let imm_value = if immediate == 0 { 8 } else { immediate as u32 };

        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);

        // For address registers, no flags are affected and size is always long
        if mode == 1 {
            // An
            let result = self.a[reg as usize].wrapping_sub(imm_value);
            self.a[reg as usize] = result;

            InstructionResult::prefetch(if size == Size::Long { 6 } else { 8 })
        } else {
            // Other EA
            let dst = ea.read(self, bus, prefetch, size);

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let result = dst.wrapping_sub(imm_value);

            ea.write(self, bus, prefetch, size, result);

            self.update_flags_nzvxc(result, dst, imm_value, size, true);

            let cycles = if size == Size::Long {
                match ea.mode {
                    AddressingMode::DataDirect(_) => 8,
                    _ => 12,
                }
            } else {
                match ea.mode {
                    AddressingMode::DataDirect(_) => 4,
                    _ => 8,
                }
            };

            InstructionResult::prefetch(cycles + ea.cycles)
        }
    }

    /// SUBA - Subtract from Address Register
    /// Opcode: 1001rrr0s1mmmrrr (s = size: 0=word, 1=long)
    /// Subtracts <ea> from An (no flags affected)
    pub fn suba(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        reg: u8,
        mode: u8,
        ea_reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, ea_reg, size);

        let src = ea.read(self, bus, prefetch, size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, size));
        }

        // Sign-extend if word size
        let src = if size == Size::Word {
            src as i16 as i32 as u32
        } else {
            src
        };

        self.a[reg as usize] = self.a[reg as usize].wrapping_sub(src);

        // ADDA does not affect flags

        let cycles = if size == Size::Long {
            match ea.mode {
                AddressingMode::DataDirect(_) => 8,
                AddressingMode::AddressDirect(_) => 8,
                AddressingMode::Immediate(_) => 8,
                _ => 6,
            }
        } else {
            8
        };

        InstructionResult::prefetch(cycles + ea.cycles)
    }

    /// SUBX - Subtract Extended
    /// Similar to ADDX but subtraction
    pub fn subx(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        dst_reg: u8,
        src_reg: u8,
        memory_mode: bool,
    ) -> InstructionResult {
        let x = if self.get_flag(super::Flag::X) { 1 } else { 0 };

        let (result, src, dst, cycles) = if memory_mode {
            if size == Size::Long {
                // Special handling for Long: process as two Word operations
                // to ensure correct register state on Address Error.
                // -(An) mode decrements by 2 for each word access, not 4 at once.

                // Create EA for -(An) with Word size
                let src_ea =
                    decode::decode_ea_with_cycles(self, bus, prefetch, 4, src_reg, Size::Word);
                let dst_ea =
                    decode::decode_ea_with_cycles(self, bus, prefetch, 4, dst_reg, Size::Word);

                // With -(An), first decrement reads the LOW word (higher address),
                // second decrement reads the HIGH word (lower address)

                // Read source LOW word first (this decrements src An by 2)
                let src_low = src_ea.read(self, bus, prefetch, Size::Word);

                if self.address_error.is_some() {
                    return InstructionResult::prefetch(12);
                }

                // Read source HIGH word (this decrements src An by 2 more)
                let src_high = src_ea.read(self, bus, prefetch, Size::Word);

                // Read destination LOW word first (this decrements dst An by 2)
                let dst_low = dst_ea.read(self, bus, prefetch, Size::Word);

                if self.address_error.is_some() {
                    return InstructionResult::prefetch(12);
                }

                // Read destination HIGH word (this decrements dst An by 2 more)
                let dst_high = dst_ea.read(self, bus, prefetch, Size::Word);

                // Assemble 32-bit values from high and low words
                let src = (src_high << 16) | src_low;
                let dst = (dst_high << 16) | dst_low;

                // Perform 32-bit subtraction with extend
                let result = dst.wrapping_sub(src).wrapping_sub(x);

                dst_ea.write(self, bus, prefetch, Size::Long, result);

                (result, src, dst, 30)
            } else {
                // Byte/Word: standard EA processing (single access)
                let src_ea = decode::decode_ea_with_cycles(self, bus, prefetch, 4, src_reg, size);
                let dst_ea = decode::decode_ea_with_cycles(self, bus, prefetch, 4, dst_reg, size);

                // -(An) - -(An) -> -(An)
                let src = src_ea.read(self, bus, prefetch, size);

                if self.address_error.is_some() {
                    return InstructionResult::prefetch(get_exception_cycles(&src_ea, size));
                }

                let dst = dst_ea.read(self, bus, prefetch, size);

                if self.address_error.is_some() {
                    return InstructionResult::prefetch(get_exception_cycles(&dst_ea, size));
                }

                let result = dst.wrapping_sub(src).wrapping_sub(x);

                dst_ea.write(self, bus, prefetch, size, result);

                (result, src, dst, 18)
            }
        } else {
            // Dn - Dn -> Dn
            let src = self.d[src_reg as usize] & size.mask();
            let dst = self.d[dst_reg as usize] & size.mask();

            let result = dst.wrapping_sub(src).wrapping_sub(x);

            let mask = size.mask();
            self.d[dst_reg as usize] = (self.d[dst_reg as usize] & !mask) | (result & mask);

            let cycles = if size == Size::Long { 8 } else { 4 };
            (result, src, dst, cycles)
        };

        // Save Z flag, because for subx, if the result is 0, the Z flag should remain the same as before the operation
        let old_z = self.get_flag(super::Flag::Z);

        // Update flags normally (for subtraction)
        self.update_flags_nzvxc(result, dst, src, size, true);

        // Fix carry flag to consider all 3 operands: dst - src - x, because update_flags_nzvxc
        // only considers 2 operands: dst - src
        let mask = size.mask();
        // For subtraction: borrow if dst < (src + x)
        let borrow = (dst as u64) < ((src as u64) + (x as u64));
        self.set_flag(super::Flag::C, borrow);
        self.set_flag(super::Flag::X, borrow);

        // Special Z flag behavior for SUBX: only clear if result != 0, otherwise keep old value
        if (result & mask) != 0 {
            self.set_flag(super::Flag::Z, false);
        } else {
            self.set_flag(super::Flag::Z, old_z); // Restore old Z if result is zero
        }

        InstructionResult::prefetch(cycles)
    }

    /// CMP - Compare
    /// Opcode: 1011rrroommmmrrr
    /// Subtracts <ea> from Dn and sets flags, but doesn't modify Dn
    pub fn cmp(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        reg: u8,
        mode: u8,
        ea_reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, ea_reg, size);
        let src = ea.read(self, bus, prefetch, size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, size));
        }

        let dst = self.d[reg as usize] & size.mask();
        let result = dst.wrapping_sub(src);

        // Update flags (like SUB) but don't modify Dn
        self.update_flags_nzvc(result, dst, src, size, true); // true = subtraction

        let cycles = if size == Size::Long { 6 } else { 4 } + ea.cycles;
        InstructionResult::prefetch(cycles)
    }

    /// CMPI - Compare Immediate
    /// Opcode: 00001100ssmmmrrr
    /// Compares immediate data with destination
    pub fn cmpi(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        let immediate = match size {
            Size::Byte => (prefetch.read_word(bus) & 0xFF) as u32,
            Size::Word => prefetch.read_word(bus) as u32,
            Size::Long => prefetch.read_long(bus),
        };

        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);
        let dst = ea.read(self, bus, prefetch, size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, size));
        }

        let result = dst.wrapping_sub(immediate);

        self.update_flags_nzvc(result, dst, immediate, size, true);

        let cycles = if size == Size::Long {
            match ea.mode {
                AddressingMode::DataDirect(_) => 14,
                _ => 12,
            }
        } else {
            8
        } + ea.cycles;
        InstructionResult::prefetch(cycles)
    }

    /// CMPM - Compare Memory to Memory
    /// Opcode: 1011rrr1ss001rrr
    /// (Ay)+ - (Ax)+, set flags
    pub fn cmpm(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        dst_reg: u8,
        src_reg: u8,
    ) -> InstructionResult {
        let dst_ea = decode::decode_ea_with_cycles(self, bus, prefetch, 3, src_reg, size);

        let src_ea = decode::decode_ea_with_cycles(self, bus, prefetch, 3, dst_reg, size);

        let src_value = dst_ea.read(self, bus, prefetch, size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&src_ea, size));
        }

        let dst_value = src_ea.read(self, bus, prefetch, size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&dst_ea, size));
        }

        let result = dst_value.wrapping_sub(src_value);

        self.update_flags_nzvc(result, dst_value, src_value, size, true);

        let cycles = 4 + src_ea.cycles + dst_ea.cycles;
        InstructionResult::prefetch(cycles)
    }

    /// CMPA - Compare Address
    /// Opcode: 1011rrr0s1mmmrrr (s = size: 0=word, 1=long)
    /// Compares <ea> with An and sets flags
    pub fn cmpa(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        reg: u8,
        mode: u8,
        ea_reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, ea_reg, size);
        let src = ea.read(self, bus, prefetch, size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, size));
        }

        // Sign-extend if word size
        let src = if size == Size::Word {
            src as i16 as i32 as u32
        } else {
            src
        };

        let dst = self.a[reg as usize];
        let result = dst.wrapping_sub(src);

        // Update flags (compare always uses long size for An)
        self.update_flags_nzvc(result, dst, src, Size::Long, true);

        let cycles = 6 + ea.cycles;

        InstructionResult::prefetch(cycles)
    }

    /// NEG - Negate
    /// Opcode: 01000100ssmmmrrr
    /// Subtracts operand from zero (0 - operand)
    pub fn neg(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);

        let src = ea.read(self, bus, prefetch, size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, size));
        }

        let result = 0u32.wrapping_sub(src);

        ea.write(self, bus, prefetch, size, result);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, size));
        }

        // Update flags (NEG modifies X: 0 - src)
        self.update_flags_nzvxc(result, 0, src, size, true);

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

    /// NEGX - Negate with Extend
    /// 0 - operand - X -> operand
    pub fn negx(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);
        let src = ea.read(self, bus, prefetch, size);

        // Check for Address Error during read
        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, size));
        }

        let x = if self.get_flag(super::Flag::X) { 1 } else { 0 };

        // NEGX = 0 - src - X
        let result = 0u32.wrapping_sub(src).wrapping_sub(x);

        ea.write(self, bus, prefetch, size, result);

        // Save Z flag, because for negx, if the result is 0, the Z flag should remain the same as before the operation
        let old_z = self.get_flag(super::Flag::Z);

        // Update flags normally (for subtraction: 0 - src - x)
        self.update_flags_nzvxc(result, 0, src, size, true);

        // Fix carry flag to consider all 3 operands: 0 - src - x
        let mask = size.mask();
        // For NEGX: borrow if 0 < (src + x)
        let borrow = (src as u64) + (x as u64) > 0;
        self.set_flag(super::Flag::C, borrow);
        self.set_flag(super::Flag::X, borrow);

        // Special Z flag behavior for NEGX: only clear if result != 0, otherwise keep old value
        if (result & mask) != 0 {
            self.set_flag(super::Flag::Z, false);
        } else {
            self.set_flag(super::Flag::Z, old_z); // Restore old Z if result is zero
        }

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

    /// MULU - Unsigned Multiply
    /// Opcode: 1100rrr011mmmrrr
    /// Dn = Dn.W x <ea>.W (16x16 = 32-bit result)
    pub fn mulu(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        reg: u8,
        mode: u8,
        ea_reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, ea_reg, Size::Word);
        let src = ea.read(self, bus, prefetch, Size::Word) & 0xFFFF;

        // Check for Address Error before modifying register
        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, Size::Long));
        }

        let dst = self.d[reg as usize] & 0xFFFF;

        let result = src * dst;
        self.d[reg as usize] = result;

        // Update flags
        self.update_flags_nz(result, Size::Long);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

        // Cycles: The formula is 38 + 2*n, where n is the number of ones in the  <ea>.
        let cycles = 38 + 2 * src.count_ones();

        InstructionResult::prefetch(cycles + ea.cycles)
    }

    /// MULS - Signed Multiply
    /// Opcode: 1100rrr111mmmrrr
    /// Dn = Dn.W x <ea>.W (16x16 = 32-bit result, signed)
    pub fn muls(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        reg: u8,
        mode: u8,
        ea_reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, ea_reg, Size::Word);

        let src_word = (ea.read(self, bus, prefetch, Size::Word) & 0xFFFF) as u16;
        let src = src_word as i16 as i32;

        // Check for Address Error before modifying register
        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, Size::Long));
        }

        let dst = (self.d[reg as usize] & 0xFFFF) as i16 as i32;

        let result = (src * dst) as u32;
        self.d[reg as usize] = result;

        // Update flags
        self.update_flags_nz(result, Size::Long);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

        // Cycles: The formula is 38 + 2*n
        //  n = concatenate the <ea> with a zero as the LSB;
        //   n is the resultant number of 10 or 01 patterns
        //   in the 17-bit source; i.e., worst case happens
        //   when the source is $5555

        // 17-bit value: src_word concatenated with 0 as LSB
        let x: u32 = (src_word as u32) << 1;

        // Transitions 01/10 are exactly the bit-changes between adjacent bits.
        // Transitions between adjacent bits (1..16) appear in bits 0..15 of (x ^ (x >> 1))
        let transitions = (x ^ (x >> 1)) & 0xFFFF;
        let n = transitions.count_ones();

        let cycles = 38 + 2 * n;

        InstructionResult::prefetch(cycles + ea.cycles)
    }

    /// DIVU - Unsigned Divide
    /// Opcode: 1000rrr011mmmrrr
    /// Dn.L / <ea>.W -> quotient.W:remainder.W
    pub fn divu(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        reg: u8,
        mode: u8,
        ea_reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, ea_reg, Size::Word);

        let divisor = ea.read(self, bus, prefetch, Size::Word) & 0xFFFF;

        // Check for Address Error before accessing register
        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, Size::Long));
        }

        let dividend = self.d[reg as usize];

        if divisor == 0 {
            // Division by zero: trigger exception (vector #5)
            // M68000 pushes the PC of the NEXT instruction, so advance PC first
            // +2 for opcode word, +consumed for extension words
            self.pc = self.pc.wrapping_add(2 + prefetch.consumed());
            // DIVU: flags reflect internal state after testing dividend
            // N = MSB of dividend, Z = (dividend == 0)
            self.set_flag(super::Flag::N, (dividend & 0x80000000) != 0);
            self.set_flag(super::Flag::Z, dividend == 0);
            self.set_flag(super::Flag::V, false);
            self.set_flag(super::Flag::C, false);
            let cycles = self.divide_by_zero_exception(bus);
            return InstructionResult::branch_taken(cycles + ea.cycles);
        }

        let quotient = dividend / divisor;
        let remainder = dividend % divisor;

        // Check for overflow (quotient doesn't fit in 16 bits)
        if quotient > 0xFFFF {
            self.set_flag(super::Flag::V, true);
            self.set_flag(super::Flag::C, false);
            // N is set based on MSB of dividend on overflow
            self.set_flag(super::Flag::N, (dividend & 0x80000000) != 0);
            self.set_flag(super::Flag::Z, dividend == 0);
            // Don't modify register on overflow
            let cycles = 10 + ea.cycles;
            return InstructionResult::prefetch(cycles);
        }

        // Result: lower word = quotient, upper word = remainder
        let result = (remainder << 16) | (quotient & 0xFFFF);
        self.d[reg as usize] = result;

        // Update flags based on quotient
        self.set_flag(super::Flag::Z, quotient == 0);
        self.set_flag(super::Flag::N, (quotient & 0x8000) != 0);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

        let cycles = divu_cycles(dividend, divisor as u16);
        InstructionResult::prefetch(cycles + ea.cycles)
    }

    /// DIVS - Signed Divide
    /// Opcode: 1000rrr111mmmrrr
    /// Dn.L / <ea>.W -> quotient.W:remainder.W (signed)
    pub fn divs(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        reg: u8,
        mode: u8,
        ea_reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, ea_reg, Size::Word);
        let divisor = (ea.read(self, bus, prefetch, Size::Word) & 0xFFFF) as i16 as i32;

        // Check for Address Error before accessing register
        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, Size::Long));
        }

        let dividend = self.d[reg as usize] as i32;

        if divisor == 0 {
            // Division by zero: trigger exception (vector #5)
            // M68000 pushes the PC of the NEXT instruction, so advance PC first
            // +2 for opcode word, +consumed for extension words
            self.pc = self.pc.wrapping_add(2 + prefetch.consumed());
            // DIVS: flags reflect testing the divisor value
            // N = MSB of divisor (always 0 when divisor is 0), Z = (divisor == 0)
            self.set_flag(super::Flag::N, false);
            self.set_flag(super::Flag::Z, true);
            self.set_flag(super::Flag::V, false);
            self.set_flag(super::Flag::C, false);
            let cycles = self.divide_by_zero_exception(bus);
            return InstructionResult::branch_taken(cycles + ea.cycles);
        }

        // Wrapping, because the one quotient that does not fit 32 bits
        // (-2^31 / -1) is an overflow the check below reports like any other;
        // the plain operators would stop the program there.
        let quotient = dividend.wrapping_div(divisor);
        let remainder = dividend.wrapping_rem(divisor);

        // Check for overflow (quotient doesn't fit in signed 16 bits)
        if !(-32768..=32767).contains(&quotient) {
            self.set_flag(super::Flag::V, true);
            self.set_flag(super::Flag::C, false);
            // N is set based on MSB of dividend on overflow
            self.set_flag(super::Flag::N, (dividend as u32 & 0x80000000) != 0);
            self.set_flag(super::Flag::Z, false);
            let cycles = divs_cycles(dividend, divisor as i16);
            return InstructionResult::prefetch(cycles + ea.cycles);
        }

        // Result: lower word = quotient, upper word = remainder
        let result = (((remainder as u16) as u32) << 16) | ((quotient as u16) as u32);
        self.d[reg as usize] = result;

        // Update flags based on quotient
        self.set_flag(super::Flag::Z, quotient == 0);
        self.set_flag(super::Flag::N, quotient < 0);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

        let cycles = divs_cycles(dividend, divisor as i16);
        InstructionResult::prefetch(cycles + ea.cycles)
    }
}

/// Clock cycles of a `DIVU` that completes, the divisor is not zero and
/// the quotient fits sixteen bits. The time of the effective address is
/// the caller's.
///
/// The processor divides by restoring division: sixteen steps, each
/// shifting the remainder one bit to the left and taking the divisor from
/// it when it fits. Its microcode spends a different time on a step
/// according to how the step went, for the first fifteen of them:
///
/// | the step                                                 | clocks |
/// |----------------------------------------------------------|--------|
/// | the shift carried a 1 out, so the divisor fits for sure  | 0      |
/// | no carry, and the divisor fits                           | 2      |
/// | no carry, and it does not                                | 4      |
///
/// over a base of 76: a division takes 76 to 136 clocks. (The rule was
/// worked out by Jorge Cwik from the processor's behaviour; checked
/// against Genesis Plus GX.)
fn divu_cycles(dividend: u32, divisor: u16) -> u32 {
    const BASE: u32 = 76;
    // The divisor lines up with the remainder's upper half.
    let divisor = u32::from(divisor) << 16;
    let mut remainder = dividend;
    let mut cycles = BASE;
    for _ in 0..15 {
        let carried_out = remainder & 0x8000_0000 != 0;
        remainder <<= 1;
        let fits = carried_out || remainder >= divisor;
        if fits {
            remainder = remainder.wrapping_sub(divisor);
        }
        cycles += match (carried_out, fits) {
            (true, _) => 0,
            (false, true) => 2,
            (false, false) => 4,
        };
    }
    cycles
}

/// Clock cycles of a `DIVS` whose divisor is not zero (0 for a zero
/// divisor, which traps instead). The time of the effective address is
/// the caller's.
///
/// A signed division is the unsigned one over the operands' magnitudes,
/// with the signs put back afterwards, and its time follows from that:
///
/// - a quotient that does not fit sixteen signed bits costs 16 clocks, 18
///   when the dividend is negative, the time the processor takes to give
///   up before dividing, charged here to every overflow;
/// - otherwise the two signs set a base, 120 clocks for two positive
///   operands, 122 for a negative divisor alone, 126 for a negative
///   dividend alone, 124 for both, and every 0 among the upper fifteen
///   bits of the quotient's magnitude adds 2.
///
/// That second path takes 120 to 156 clocks. (The rule was worked out by
/// Jorge Cwik from the processor's behaviour; checked against Genesis Plus
/// GX.)
fn divs_cycles(dividend: i32, divisor: i16) -> u32 {
    if divisor == 0 {
        return 0;
    }
    let negative_dividend = dividend < 0;
    let negative_divisor = divisor < 0;
    let magnitude = dividend.unsigned_abs();
    let divisor_magnitude = u32::from(divisor.unsigned_abs());

    // The quotient's magnitude may reach 32 768 only as a negative number
    // with nothing left over.
    let limit = divisor_magnitude << 15;
    let overflows =
        magnitude > limit || (magnitude == limit && negative_dividend == negative_divisor);
    if overflows {
        return if negative_dividend { 18 } else { 16 };
    }

    let for_the_signs = match (negative_dividend, negative_divisor) {
        (false, false) => 120,
        (false, true) => 122,
        (true, true) => 124,
        (true, false) => 126,
    };
    let quotient = (magnitude / divisor_magnitude) as u16;
    let zeros_in_the_upper_fifteen = (!quotient >> 1).count_ones();
    for_the_signs + 2 * zeros_in_the_upper_fifteen
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_add_data_direct() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 100;
        cpu.d[1] = 50;
        cpu.pc = 0x1000;

        // ADD.L D0, D1 (D0 + D1 -> D1)
        let mut prefetch = crate::cpu68k::prefetch::Prefetch::new(cpu.pc);
        let result = cpu.add(&mut ram, &mut prefetch, Size::Long, 1, 0, 0, false);

        assert_eq!(cpu.d[1], 150);
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
        assert!(!cpu.get_flag(super::super::Flag::V));
        assert!(!cpu.get_flag(super::super::Flag::C));
    }

    #[test]
    fn test_add_overflow() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0x7FFFFFFF; // Max positive int32
        cpu.d[1] = 1;
        cpu.pc = 0x1000;

        // ADD.L D1, D0
        let mut prefetch = crate::cpu68k::prefetch::Prefetch::new(cpu.pc);
        let result = cpu.add(&mut ram, &mut prefetch, Size::Long, 0, 0, 1, false);

        assert_eq!(cpu.d[0], 0x80000000); // Overflowed to negative
        assert!(cpu.get_flag(super::super::Flag::N));
        assert!(cpu.get_flag(super::super::Flag::V)); // Overflow set
        assert!(!cpu.get_flag(super::super::Flag::C));
    }

    #[test]
    fn test_addi() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 100;
        cpu.pc = 0x1000;
        ram.write_word(0x1002, 50); // Immediate value (after opcode at 0x1000)

        // ADDI.W #50, D0
        let mut prefetch = crate::cpu68k::prefetch::Prefetch::new(cpu.pc);
        let result = cpu.addi(&mut ram, &mut prefetch, Size::Word, 0, 0);

        assert_eq!(cpu.d[0] & 0xFFFF, 150);
        assert!(!cpu.get_flag(super::super::Flag::Z));
    }

    #[test]
    fn test_addq() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 100;
        cpu.pc = 0x1000;

        // ADDQ.L #5, D0
        let mut prefetch = crate::cpu68k::prefetch::Prefetch::new(cpu.pc);
        let result = cpu.addq(&mut ram, &mut prefetch, Size::Long, 5, 0, 0);

        assert_eq!(cpu.d[0], 105);
        // ADDQ.L to a data register takes 8 cycles on the MC68000
        // (byte/word forms take 4). See M68000 user manual table 8-4.
        assert_eq!(result.cycles, 8);
    }

    #[test]
    fn test_addq_to_address_register() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.a[0] = 0x1000;
        cpu.pc = 0x2000;

        // ADDQ.L #4, A0
        let mut prefetch = crate::cpu68k::prefetch::Prefetch::new(cpu.pc);
        let result = cpu.addq(&mut ram, &mut prefetch, Size::Long, 4, 1, 0);

        assert_eq!(cpu.a[0], 0x1004);
        // Flags should NOT be affected for address registers
    }

    #[test]
    fn test_sub() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 100;
        cpu.d[1] = 50;
        cpu.pc = 0x1000;

        // SUB.L D1, D0 (D0 - D1 -> D0)
        let mut prefetch = crate::cpu68k::prefetch::Prefetch::new(cpu.pc);
        let result = cpu.sub(&mut ram, &mut prefetch, Size::Long, 0, 0, 1, false);

        assert_eq!(cpu.d[0], 50);
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
        assert!(!cpu.get_flag(super::super::Flag::V));
        assert!(!cpu.get_flag(super::super::Flag::C));
    }

    #[test]
    fn test_sub_carry() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 50;
        cpu.d[1] = 100;
        cpu.pc = 0x1000;

        // SUB.L D1, D0 (50 - 100 = -50)
        let mut prefetch = crate::cpu68k::prefetch::Prefetch::new(cpu.pc);
        let result = cpu.sub(&mut ram, &mut prefetch, Size::Long, 0, 0, 1, false);

        assert_eq!(cpu.d[0] as i32, -50);
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(cpu.get_flag(super::super::Flag::N)); // Negative result
        assert!(cpu.get_flag(super::super::Flag::C)); // Borrow occurred
    }

    #[test]
    fn test_subi() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 100;
        cpu.pc = 0x1000;
        ram.write_word(0x1002, 30); // Immediate value (after opcode at 0x1000)

        // SUBI.W #30, D0
        let mut prefetch = crate::cpu68k::prefetch::Prefetch::new(cpu.pc);
        let result = cpu.subi(&mut ram, &mut prefetch, Size::Word, 0, 0);

        assert_eq!(cpu.d[0] & 0xFFFF, 70);
    }

    #[test]
    fn test_subq() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 100;
        cpu.pc = 0x1000;

        // SUBQ.L #3, D0
        let mut prefetch = crate::cpu68k::prefetch::Prefetch::new(cpu.pc);
        let result = cpu.subq(&mut ram, &mut prefetch, Size::Long, 3, 0, 0);

        assert_eq!(cpu.d[0], 97);
        // SUBQ.L to a data register takes 8 cycles on the MC68000
        // (byte/word forms take 4). See M68000 user manual table 8-4.
        assert_eq!(result.cycles, 8);
    }

    #[test]
    fn test_cmp() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 100;
        cpu.d[1] = 50;

        let mut prefetch = Prefetch::new(0);
        let result = cpu.cmp(&mut ram, &mut prefetch, Size::Byte, 0, 0, 1);

        // D0 should be unchanged
        assert_eq!(cpu.d[0], 100);
        // Flags should be set (100 - 50 = 50, positive)
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_cmp_equal() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 100;
        cpu.d[1] = 100;

        let mut prefetch = Prefetch::new(0);
        let result = cpu.cmp(&mut ram, &mut prefetch, Size::Byte, 0, 0, 1);

        assert!(cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_cmpi_word_negative() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 50;
        cpu.pc = 0x1000;
        // Prefetch starts at PC+2, so immediate value should be at 0x1002
        ram.write_word(0x1002, 100); // Immediate value

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.cmpi(&mut ram, &mut prefetch, Size::Word, 0, 0);

        // D0 unchanged
        assert_eq!(cpu.d[0] & 0xFFFF, 50);
        // 50 - 100 = -50, negative
        assert!(cpu.get_flag(super::super::Flag::N));
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::V));
        assert!(cpu.get_flag(super::super::Flag::C)); // Borrow occurred
    }

    #[test]
    fn test_cmpi_word_equal() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[1] = 0x1234;
        cpu.pc = 0x2000;
        ram.write_word(0x2002, 0x1234); // Immediate value equal to D1

        let mut prefetch = Prefetch::new(0x2000);
        let result = cpu.cmpi(&mut ram, &mut prefetch, Size::Word, 0, 1);

        // D1 unchanged
        assert_eq!(cpu.d[1] & 0xFFFF, 0x1234);
        // Equal: Z=1, N=0, C=0, V=0
        assert!(cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
        assert!(!cpu.get_flag(super::super::Flag::V));
        assert!(!cpu.get_flag(super::super::Flag::C));
    }

    #[test]
    fn test_cmpi_byte() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[2] = 0xAA; // Byte value
        cpu.pc = 0x3000;
        ram.write_word(0x3002, 0x55); // Immediate byte value (in word)

        let mut prefetch = Prefetch::new(0x3000);
        let result = cpu.cmpi(&mut ram, &mut prefetch, Size::Byte, 0, 2);

        // D2 unchanged
        assert_eq!(cpu.d[2] & 0xFF, 0xAA);
        // 0xAA - 0x55 = 0x55, positive
        assert!(!cpu.get_flag(super::super::Flag::N));
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::C));
    }

    #[test]
    fn test_cmpi_long() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[3] = 0x12345678;
        cpu.pc = 0x4000;
        // Prefetch reads long from 0x4002 and 0x4004
        ram.write_word(0x4002, 0x1234);
        ram.write_word(0x4004, 0x9999);

        let mut prefetch = Prefetch::new(0x4000);
        let result = cpu.cmpi(&mut ram, &mut prefetch, Size::Long, 0, 3);

        // D3 unchanged
        assert_eq!(cpu.d[3], 0x12345678);
        // 0x12345678 - 0x12349999 = negative
        assert!(cpu.get_flag(super::super::Flag::N));
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(cpu.get_flag(super::super::Flag::C)); // Borrow
    }

    #[test]
    fn test_cmpa() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.a[0] = 0x1000;
        cpu.d[0] = 0x0500;

        let mut prefetch = Prefetch::new(0);
        let result = cpu.cmpa(&mut ram, &mut prefetch, Size::Word, 0, 0, 0);

        // A0 unchanged
        assert_eq!(cpu.a[0], 0x1000);
        // Flags set based on comparison
        assert!(!cpu.get_flag(super::super::Flag::Z));
    }

    #[test]
    fn test_neg() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 50;

        let mut prefetch = Prefetch::new(0);
        let result = cpu.neg(&mut ram, &mut prefetch, Size::Byte, 0, 0);

        assert_eq!(cpu.d[0] & 0xFF, 256 - 50);
        assert!(cpu.get_flag(super::super::Flag::N));
        assert!(!cpu.get_flag(super::super::Flag::Z));
    }

    #[test]
    fn test_neg_zero() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0;

        let mut prefetch = Prefetch::new(0);
        let result = cpu.neg(&mut ram, &mut prefetch, Size::Byte, 0, 0);

        assert_eq!(cpu.d[0] & 0xFF, 0);
        assert!(cpu.get_flag(super::super::Flag::Z));
    }

    #[test]
    fn test_adda() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.a[0] = 0x1000;
        cpu.d[0] = 0x0500;

        let mut prefetch = Prefetch::new(0);
        let result = cpu.adda(&mut ram, &mut prefetch, Size::Word, 0, 0, 0);

        assert_eq!(cpu.a[0], 0x1500);
        // ADDA doesn't affect flags
    }

    #[test]
    fn test_adda_sign_extend() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.a[0] = 0x00001000;
        cpu.d[0] = 0x0000FFFF; // -1 in word

        let mut prefetch = Prefetch::new(0);
        let result = cpu.adda(&mut ram, &mut prefetch, Size::Word, 0, 0, 0);

        // Should sign-extend 0xFFFF to 0xFFFFFFFF and add
        assert_eq!(cpu.a[0], 0x00000FFF); // 0x1000 + 0xFFFFFFFF
    }

    #[test]
    fn test_suba() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.a[0] = 0x1000;
        cpu.d[0] = 0x0500;

        let mut prefetch = Prefetch::new(0);
        let result = cpu.suba(&mut ram, &mut prefetch, Size::Word, 0, 0, 0);

        assert_eq!(cpu.a[0], 0x0B00);
        // SUBA doesn't affect flags
    }

    #[test]
    fn test_mulu() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 100;
        cpu.d[1] = 50;

        let mut prefetch = Prefetch::new(0);
        let result = cpu.mulu(&mut ram, &mut prefetch, 0, 0, 1);

        assert_eq!(cpu.d[0], 5000);
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_muls_positive() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 10;
        cpu.d[1] = 20;

        let mut prefetch = Prefetch::new(0);
        let result = cpu.muls(&mut ram, &mut prefetch, 0, 0, 1);

        assert_eq!(cpu.d[0], 200);
        assert!(!cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_muls_negative() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 10;
        cpu.d[1] = 0xFFFF; // -1 as signed word

        let mut prefetch = Prefetch::new(0);
        let result = cpu.muls(&mut ram, &mut prefetch, 0, 0, 1);

        assert_eq!(cpu.d[0] as i32, -10);
        assert!(cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_divu() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 100;
        cpu.d[1] = 7;

        let mut prefetch = Prefetch::new(0);
        let result = cpu.divu(&mut ram, &mut prefetch, 0, 0, 1);

        // Quotient should be in lower word, remainder in upper word
        let quotient = cpu.d[0] & 0xFFFF;
        let remainder = (cpu.d[0] >> 16) & 0xFFFF;

        assert_eq!(quotient, 14); // 100 / 7 = 14
        assert_eq!(remainder, 2); // 100 % 7 = 2
    }

    #[test]
    fn test_divu_by_zero() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 100;
        cpu.d[1] = 0;

        let mut prefetch = Prefetch::new(0);
        let result = cpu.divu(&mut ram, &mut prefetch, 0, 0, 1);

        // Division by zero traps via vector 5. The MC68000 reference
        // manual leaves V/N/Z undefined on this path; this implementation
        // chooses to clear V. We only assert the side that is well-defined:
        // the destination register must NOT be modified.
        assert_eq!(cpu.d[0], 100);
    }

    #[test]
    fn divs_of_the_most_negative_dividend_by_minus_one_overflows() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        // +2^31 fits neither sixteen bits nor thirty-two.
        cpu.d[0] = 0x8000_0000;
        cpu.d[1] = 0xFFFF;

        let mut prefetch = Prefetch::new(0);
        let result = cpu.divs(&mut ram, &mut prefetch, 0, 0, 1);

        assert_eq!(cpu.d[0], 0x8000_0000, "the register is left alone");
        assert!(cpu.get_flag(super::super::Flag::V));
        assert_eq!(result.cycles, 18);
    }

    #[test]
    fn divu_time_follows_the_steps_of_the_division() {
        // A quotient of zero: every step fails, and none carries.
        assert_eq!(divu_cycles(0, 1), 136);
        // A quotient of 0xFFFF out of a dividend whose top bit stays set:
        // every step carries.
        assert_eq!(divu_cycles(0xFFFE_0001, 0xFFFF), 76);
        assert_eq!(divu_cycles(100, 7), 130);
        assert_eq!(divu_cycles(0x1234_5678, 0x5678), 120);
    }

    #[test]
    fn divs_time_follows_the_signs_and_the_quotient() {
        // 0 has fifteen zeros in its upper bits: the base plus thirty.
        assert_eq!(divs_cycles(0, 1), 150);
        // The same magnitudes under the four combinations of signs.
        assert_eq!(divs_cycles(100, 7), 144);
        assert_eq!(divs_cycles(100, -7), 146);
        assert_eq!(divs_cycles(-100, -7), 148);
        assert_eq!(divs_cycles(-100, 7), 150);
        // The largest quotients that fit.
        assert_eq!(divs_cycles(32767, 1), 122);
        assert_eq!(divs_cycles(-32768, 1), 154);
        // A quotient that does not fit, and a zero divisor.
        assert_eq!(divs_cycles(32768, 1), 16);
        assert_eq!(divs_cycles(-32769, 1), 18);
        assert_eq!(divs_cycles(1, 0), 0);
    }

    #[test]
    fn test_divs() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 100;
        cpu.d[1] = 0xFFF9; // -7 as signed word

        let mut prefetch = Prefetch::new(0);
        let result = cpu.divs(&mut ram, &mut prefetch, 0, 0, 1);

        let quotient = (cpu.d[0] & 0xFFFF) as i16;
        let remainder = ((cpu.d[0] >> 16) & 0xFFFF) as i16;

        assert_eq!(quotient, -14); // 100 / -7 = -14
        assert_eq!(remainder, 2); // 100 % -7 = 2
    }
}
