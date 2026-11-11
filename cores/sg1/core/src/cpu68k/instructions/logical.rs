//! Logical instructions: AND, OR, EOR, NOT.

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
    /// AND - Logical AND
    /// Opcode: 1100rrrmssssseee
    #[allow(clippy::too_many_arguments)]
    pub fn and(
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

        let (result, cycles) = if to_ea {
            // Dn & <ea> -> <ea>
            let src = self.d[reg as usize] & size.mask();
            let dst = ea.read(self, bus, prefetch, size);

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let result = src & dst;

            ea.write(self, bus, prefetch, size, result);

            let cycles = if size == Size::Long { 12 } else { 8 };

            (result, cycles + ea.cycles)
        } else {
            // <ea> & Dn -> Dn
            let src = ea.read(self, bus, prefetch, size);

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let dst = self.d[reg as usize] & size.mask();
            let result = src & dst;

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

            (result, cycles + ea.cycles)
        };

        // Update flags
        self.update_flags_nz(result, size);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

        InstructionResult::prefetch(cycles)
    }

    /// ANDI - AND Immediate
    /// Opcode: 00000010ssmmmrrr
    pub fn andi(
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

        let result = dst & immediate;

        ea.write(self, bus, prefetch, size, result);

        // Update flags
        self.update_flags_nz(result, size);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

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

    /// OR - Logical OR
    /// Opcode: 1000rrrmssssseee
    #[allow(clippy::too_many_arguments)]
    pub fn or(
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

        let (result, cycles) = if to_ea {
            // Dn & <ea> -> <ea>
            let src = self.d[reg as usize] & size.mask();
            let dst = ea.read(self, bus, prefetch, size);

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let result = src | dst;

            ea.write(self, bus, prefetch, size, result);

            let cycles = if size == Size::Long { 12 } else { 8 };

            (result, cycles + ea.cycles)
        } else {
            // <ea> & Dn -> Dn
            let src = ea.read(self, bus, prefetch, size);

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let dst = self.d[reg as usize] & size.mask();
            let result = src | dst;

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

            (result, cycles + ea.cycles)
        };

        // Update flags
        self.update_flags_nz(result, size);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

        InstructionResult::prefetch(cycles)
    }

    /// ORI - OR Immediate
    /// Opcode: 00000000ssmmmrrr
    pub fn ori(
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

        let result = dst | immediate;

        ea.write(self, bus, prefetch, size, result);

        // Update flags
        self.update_flags_nz(result, size);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

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

    /// EOR - Logical Exclusive OR
    /// Opcode: 1011rrr1ssssseee
    pub fn eor(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        reg: u8,
        mode: u8,
        ea_reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, ea_reg, size);

        // Dn ^ <ea> -> <ea>
        let src = self.d[reg as usize] & size.mask();
        let dst = ea.read(self, bus, prefetch, size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, size));
        }

        let result = src ^ dst;

        ea.write(self, bus, prefetch, size, result);

        // Update flags
        self.update_flags_nz(result, size);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

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

    /// EORI - EOR Immediate
    /// Opcode: 00001010ssmmmrrr
    pub fn eori(
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

        let result = dst ^ immediate;

        ea.write(self, bus, prefetch, size, result);

        // Update flags
        self.update_flags_nz(result, size);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

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

    /// NOT - Logical Complement
    /// Opcode: 01000110ssmmmrrr
    pub fn not(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);
        let value = ea.read(self, bus, prefetch, size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, size));
        }

        let result = !value & size.mask();

        ea.write(self, bus, prefetch, size, result);

        // Update flags
        self.update_flags_nz(result, size);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

        let cycles = if size == Size::Long {
            match ea.mode {
                AddressingMode::DataDirect(_) => 6,
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

    /// TST - Test operand
    /// Opcode: 01001010ssmmmrrr
    /// Tests operand against zero and sets flags (like CMP with 0)
    pub fn tst(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);
        let value = ea.read(self, bus, prefetch, size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, size));
        }

        // Update flags based on value
        self.update_flags_nz(value, size);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

        InstructionResult::prefetch(4 + ea.cycles)
    }

    /// BTST - Test bit
    /// Opcode: 0000rrr100mmmrrr (Dn) or 0000100000mmmrrr (immediate)
    /// Tests bit and sets Z flag
    pub fn btst(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        bit_num: u8,
        mode: u8,
        reg: u8,
        is_immediate: bool,
    ) -> InstructionResult {
        // BTST operand size:
        // - Dn destination (mode==0): tests a LONG (bits 0..31)
        // - memory destination: tests a BYTE (bits 0..7)

        let bit_num = if is_immediate {
            let value = (prefetch.read_word(bus) & 0xFF) as u8;

            // Add the extra bytes read to the prefetch queue to account for the immediate word.
            // PC-relative addressing in this codebase uses `prefetch.base()` as the base (PC after opcode).
            // For BTST immediate, there is an extra immediate word BEFORE any EA extension words, so the
            // effective base for PC-relative EAs must be `prefetch.base() + 2`.
            //TODO: This is a hack to account for the immediate word. We should find a better way to do this.
            prefetch.add_extra_bytes_read(2);

            value
        } else {
            (self.d[bit_num as usize] & 0xFF) as u8
        };

        let operand_size = if mode == 0 { Size::Long } else { Size::Byte };
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, operand_size);

        let value = ea.read(self, bus, prefetch, operand_size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, operand_size));
        }

        let bit_mask = if mode == 0 {
            // Data register: 32-bit, bit 0-31
            1u32 << (bit_num & 31)
        } else {
            // Memory: 8-bit, bit 0-7
            1u32 << (bit_num & 7)
        };

        // Set Z flag based on bit test
        self.set_flag(super::Flag::Z, (value & bit_mask) == 0);

        let cycles = if is_immediate {
            match ea.mode {
                AddressingMode::DataDirect(_) => 10,
                AddressingMode::Immediate(_) => 10,
                _ => 8,
            }
        } else {
            match ea.mode {
                AddressingMode::DataDirect(_) => 6,
                AddressingMode::Immediate(_) => 6,
                _ => 4,
            }
        };

        InstructionResult::prefetch(cycles + ea.cycles)
    }

    /// BSET - Test bit and set
    /// Opcode: 0000rrr111mmmrrr (Dn) or 0000111100mmmrrr (immediate)
    pub fn bset(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        bit_num: u8,
        mode: u8,
        reg: u8,
        is_immediate: bool,
    ) -> InstructionResult {
        // BTST operand size:
        // - Dn destination (mode==0): tests a LONG (bits 0..31)
        // - memory destination: tests a BYTE (bits 0..7)

        let bit_num = if is_immediate {
            let value = (prefetch.read_word(bus) & 0xFF) as u8;

            // Add the extra bytes read to the prefetch queue to account for the immediate word.
            // PC-relative addressing in this codebase uses `prefetch.base()` as the base (PC after opcode).
            // For BTST immediate, there is an extra immediate word BEFORE any EA extension words, so the
            // effective base for PC-relative EAs must be `prefetch.base() + 2`.
            //TODO: This is a hack to account for the immediate word. We should find a better way to do this.
            prefetch.add_extra_bytes_read(2);

            value
        } else {
            (self.d[bit_num as usize] & 0xFF) as u8
        };

        let operand_size = if mode == 0 { Size::Long } else { Size::Byte };
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, operand_size);

        let value = ea.read(self, bus, prefetch, operand_size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, operand_size));
        }

        let bit_mask = if mode == 0 {
            // Data register: 32-bit, bit 0-31
            1u32 << (bit_num & 31)
        } else {
            // Memory: 8-bit, bit 0-7
            1u32 << (bit_num & 7)
        };

        // Set Z flag based on bit test
        self.set_flag(super::Flag::Z, (value & bit_mask) == 0);

        let result = value | bit_mask;
        ea.write(self, bus, prefetch, operand_size, result);

        // Got these formulas from here:
        // https://gist.github.com/cbmeeks/e759c7061d61ec4ac354a7df44a4a8f1
        let cycles = if is_immediate {
            match ea.mode {
                AddressingMode::DataDirect(_) if bit_mask < 0x10000 => 10,
                _ => 12,
            }
        } else {
            match ea.mode {
                AddressingMode::DataDirect(_) if bit_mask < 0x10000 => 6,
                _ => 8,
            }
        };

        InstructionResult::prefetch(cycles + ea.cycles)
    }

    /// BCLR - Test bit and clear
    /// Opcode: 0000rrr110mmmrrr (Dn) or 0000111000mmmrrr (immediate)
    pub fn bclr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        bit_num: u8,
        mode: u8,
        reg: u8,
        is_immediate: bool,
    ) -> InstructionResult {
        // BCLR operand size:
        // - Dn destination (mode==0): tests a LONG (bits 0..31)
        // - memory destination: tests a BYTE (bits 0..7)

        let bit_num = if is_immediate {
            let value = (prefetch.read_word(bus) & 0xFF) as u8;

            // Add the extra bytes read to the prefetch queue to account for the immediate word.
            // PC-relative addressing in this codebase uses `prefetch.base()` as the base (PC after opcode).
            // For BCLR immediate, there is an extra immediate word BEFORE any EA extension words, so the
            // effective base for PC-relative EAs must be `prefetch.base() + 2`.
            //TODO: This is a hack to account for the immediate word. We should find a better way to do this.
            prefetch.add_extra_bytes_read(2);

            value
        } else {
            (self.d[bit_num as usize] & 0xFF) as u8
        };

        let operand_size = if mode == 0 { Size::Long } else { Size::Byte };
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, operand_size);

        let value = ea.read(self, bus, prefetch, operand_size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, operand_size));
        }

        let bit_mask = if mode == 0 {
            // Data register: 32-bit, bit 0-31
            1u32 << (bit_num & 31)
        } else {
            // Memory: 8-bit, bit 0-7
            1u32 << (bit_num & 7)
        };

        // Set Z flag based on bit test
        self.set_flag(super::Flag::Z, (value & bit_mask) == 0);

        let result = value & !bit_mask;
        ea.write(self, bus, prefetch, operand_size, result);

        // Got these formulas from here:
        // https://gist.github.com/cbmeeks/e759c7061d61ec4ac354a7df44a4a8f1
        let cycles = if is_immediate {
            match ea.mode {
                AddressingMode::DataDirect(_) => {
                    if bit_mask < 0x10000 {
                        12
                    } else {
                        14
                    }
                }
                _ => 12,
            }
        } else {
            match ea.mode {
                AddressingMode::DataDirect(_) => {
                    if bit_mask < 0x10000 {
                        8
                    } else {
                        10
                    }
                }
                _ => 8,
            }
        };

        InstructionResult::prefetch(cycles + ea.cycles)
    }

    /// BCHG - Test bit and change
    /// Opcode: 0000rrr101mmmrrr (Dn) or 0000100100mmmrrr (immediate)
    pub fn bchg(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        bit_num: u8,
        mode: u8,
        reg: u8,
        is_immediate: bool,
    ) -> InstructionResult {
        // BCHG operand size:
        // - Dn destination (mode==0): tests a LONG (bits 0..31)
        // - memory destination: tests a BYTE (bits 0..7)

        let bit_num = if is_immediate {
            let value = (prefetch.read_word(bus) & 0xFF) as u8;

            // Add the extra bytes read to the prefetch queue to account for the immediate word.
            // PC-relative addressing in this codebase uses `prefetch.base()` as the base (PC after opcode).
            // For BCHG immediate, there is an extra immediate word BEFORE any EA extension words, so the
            // effective base for PC-relative EAs must be `prefetch.base() + 2`.
            //TODO: This is a hack to account for the immediate word. We should find a better way to do this.
            prefetch.add_extra_bytes_read(2);

            value
        } else {
            (self.d[bit_num as usize] & 0xFF) as u8
        };

        let operand_size = if mode == 0 { Size::Long } else { Size::Byte };
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, operand_size);

        let value = ea.read(self, bus, prefetch, operand_size);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, operand_size));
        }

        let bit_mask = if mode == 0 {
            // Data register: 32-bit, bit 0-31
            1u32 << (bit_num & 31)
        } else {
            // Memory: 8-bit, bit 0-7
            1u32 << (bit_num & 7)
        };

        // Set Z flag based on bit test
        self.set_flag(super::Flag::Z, (value & bit_mask) == 0);

        let result = value ^ bit_mask;
        ea.write(self, bus, prefetch, operand_size, result);

        // Got these formulas from here:
        // https://gist.github.com/cbmeeks/e759c7061d61ec4ac354a7df44a4a8f1
        let cycles = if is_immediate {
            match ea.mode {
                AddressingMode::DataDirect(_) if bit_mask < 0x10000 => 10,
                _ => 12,
            }
        } else {
            match ea.mode {
                AddressingMode::DataDirect(_) if bit_mask < 0x10000 => 6,
                _ => 8,
            }
        };

        InstructionResult::prefetch(cycles + ea.cycles)
    }

    /// TAS - Test and Set
    /// Opcode: 0100101011mmmrrr
    /// Tests operand and sets bit 7 (atomically on real hardware)
    pub fn tas(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, Size::Byte);

        let value = ea.read(self, bus, prefetch, Size::Byte);

        // Update flags based on value
        self.update_flags_nz(value, Size::Byte);
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::C, false);

        // Set bit 7
        let new_value = value | 0x80;
        ea.write(self, bus, prefetch, Size::Byte, new_value);

        // Note: On real hardware, TAS to memory has bus issues and is unreliable
        // For emulation purposes, we implement it correctly

        let cycles = if mode == 0 { 4 } else { 10 + ea.cycles };
        InstructionResult::prefetch(cycles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_and() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b11110000;
        cpu.d[1] = 0b10101010;
        cpu.pc = 0x1000;

        // AND.B D0, D1
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.and(&mut ram, &mut prefetch, Size::Byte, 1, 0, 0, false);

        assert_eq!(cpu.d[1] & 0xFF, 0b10100000);
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(cpu.get_flag(super::super::Flag::N)); // MSB set
        assert!(!cpu.get_flag(super::super::Flag::V));
        assert!(!cpu.get_flag(super::super::Flag::C));
    }

    #[test]
    fn test_and_zero() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b11110000;
        cpu.d[1] = 0b00001111;
        cpu.pc = 0x1000;

        // AND.B D0, D1 (should result in 0)
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.and(&mut ram, &mut prefetch, Size::Byte, 1, 0, 0, false);

        assert_eq!(cpu.d[1] & 0xFF, 0);
        assert!(cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_or() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b11110000;
        cpu.d[1] = 0b00001111;
        cpu.pc = 0x1000;

        // OR.B D0, D1
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.or(&mut ram, &mut prefetch, Size::Byte, 1, 0, 0, false);

        assert_eq!(cpu.d[1] & 0xFF, 0b11111111);
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_eor() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b11110000;
        cpu.d[1] = 0b10101010;
        cpu.pc = 0x1000;

        // EOR.B D0, D1 (D0 ^ D1 -> D1)
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.eor(&mut ram, &mut prefetch, Size::Byte, 0, 0, 1);

        assert_eq!(cpu.d[1] & 0xFF, 0b01011010);
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_eor_same() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b11001100;
        cpu.d[1] = 0b11001100;
        cpu.pc = 0x1000;

        // EOR.B D0, D1 (same value -> 0)
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.eor(&mut ram, &mut prefetch, Size::Byte, 0, 0, 1);

        assert_eq!(cpu.d[1] & 0xFF, 0);
        assert!(cpu.get_flag(super::super::Flag::Z));
    }

    #[test]
    fn test_not() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b11110000;
        cpu.pc = 0x1000;

        // NOT.B D0
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.not(&mut ram, &mut prefetch, Size::Byte, 0, 0);

        assert_eq!(cpu.d[0] & 0xFF, 0b00001111);
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_not_word() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0x00000000;
        cpu.pc = 0x1000;

        // NOT.W D0
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.not(&mut ram, &mut prefetch, Size::Word, 0, 0);

        assert_eq!(cpu.d[0] & 0xFFFF, 0xFFFF);
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_tst_nonzero() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0x12345678;

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.tst(&mut ram, &mut prefetch, Size::Long, 0, 0);

        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
        assert!(!cpu.get_flag(super::super::Flag::V));
        assert!(!cpu.get_flag(super::super::Flag::C));
    }

    #[test]
    fn test_tst_zero() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0;

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.tst(&mut ram, &mut prefetch, Size::Byte, 0, 0);

        assert!(cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_andi() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0xFF;
        cpu.pc = 0x1000;
        // Immediate value is at PC+2 (after opcode), so write at 0x1002
        ram.write_word(0x1002, 0xF0);

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.andi(&mut ram, &mut prefetch, Size::Byte, 0, 0);

        assert_eq!(cpu.d[0] & 0xFF, 0xF0);
        assert!(!cpu.get_flag(super::super::Flag::Z));
    }

    #[test]
    fn test_ori() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0x0F;
        cpu.pc = 0x1000;
        // Immediate value is at PC+2 (after opcode)
        ram.write_word(0x1002, 0xF0);

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.ori(&mut ram, &mut prefetch, Size::Byte, 0, 0);

        assert_eq!(cpu.d[0] & 0xFF, 0xFF);
    }

    #[test]
    fn test_eori() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b10101010;
        cpu.pc = 0x1000;
        // Immediate value is at PC+2 (after opcode)
        ram.write_word(0x1002, 0b11110000);

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.eori(&mut ram, &mut prefetch, Size::Byte, 0, 0);

        assert_eq!(cpu.d[0] & 0xFF, 0b01011010);
    }

    #[test]
    fn test_btst_bit_set() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b00001000; // Bit 3 is set
        cpu.d[1] = 3;

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.btst(&mut ram, &mut prefetch, 1, 0, 0, false);

        // Z flag should be 0 (bit is set)
        assert!(!cpu.get_flag(super::super::Flag::Z));
    }

    #[test]
    fn test_btst_bit_clear() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b00001000; // Bit 3 is set
        cpu.d[1] = 2; // Testing bit 2 (clear)

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.btst(&mut ram, &mut prefetch, 1, 0, 0, false);

        // Z flag should be 1 (bit is clear)
        assert!(cpu.get_flag(super::super::Flag::Z));
    }

    #[test]
    fn test_bset() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b00000000;
        cpu.d[1] = 5; // Set bit 5

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.bset(&mut ram, &mut prefetch, 1, 0, 0, false);

        // Z should be 1 (bit was clear before)
        assert!(cpu.get_flag(super::super::Flag::Z));
        // Bit 5 should now be set
        assert_eq!(cpu.d[0], 0b00100000);
    }

    #[test]
    fn test_bclr() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b00100000; // Bit 5 is set
        cpu.d[1] = 5; // Clear bit 5

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.bclr(&mut ram, &mut prefetch, 1, 0, 0, false);

        // Z should be 0 (bit was set before)
        assert!(!cpu.get_flag(super::super::Flag::Z));
        // Bit 5 should now be clear
        assert_eq!(cpu.d[0], 0b00000000);
    }

    #[test]
    fn test_bchg() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b00100000; // Bit 5 is set
        cpu.d[1] = 5; // Toggle bit 5

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.bchg(&mut ram, &mut prefetch, 1, 0, 0, false);

        // Z should be 0 (bit was set before)
        assert!(!cpu.get_flag(super::super::Flag::Z));
        // Bit 5 should now be clear (toggled)
        assert_eq!(cpu.d[0], 0b00000000);
    }

    #[test]
    fn test_bchg_toggle_twice() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b00000000;
        cpu.d[1] = 3;

        // First toggle: 0 -> 1
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.bchg(&mut ram, &mut prefetch, 1, 0, 0, false);
        assert_eq!(cpu.d[0], 0b00001000);

        // Second toggle: 1 -> 0
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.bchg(&mut ram, &mut prefetch, 1, 0, 0, false);
        assert_eq!(cpu.d[0], 0b00000000);
    }
}
