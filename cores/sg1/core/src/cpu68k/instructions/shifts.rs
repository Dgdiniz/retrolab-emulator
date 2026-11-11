//! Shift and rotate instructions: LSL, LSR, ASL, ASR, ROL, ROR, ROXL, ROXR.

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

    // Cycles taken when reading the operand raises an address error, one
    // entry per addressing mode of the shifted operand. Byte/Word:
    #[rustfmt::skip]
    const MOVE_BYTE_WORD_CYCLES: [u32; 12] = [
        // Dn    An    (An)  (An)+ -(An) d(An) d(An,ix) xxx.W xxx.L d(PC) d(PC,ix) #xxx
          10,   10,    10,   10,   12,   10,   12,     10,   10,   10,   12,      10  
        ];

    // Long:
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
    /// LSL - Logical Shift Left
    ///
    /// Two forms:
    /// - Register form (ea_mode=None): LSL.b/w/l Dn, shift count from immediate or Dn
    /// - Memory form (ea_mode=Some): LSL.w <ea>, shift count = 1
    #[allow(clippy::too_many_arguments)]
    pub fn lsl(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        count: u8,
        reg: u8,
        is_register: bool,
        ea_mode: Option<u8>,
    ) -> InstructionResult {
        if let Some(mode) = ea_mode {
            let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);

            let value = ea.read(self, bus, prefetch, size) & 0xFFFF;

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let carry = (value & 0x8000) != 0;
            let result = (value << 1) & 0xFFFF;

            ea.write(self, bus, prefetch, size, result);

            // Flags
            self.update_flags_nz(result, Size::Word);
            self.set_flag(super::Flag::V, false);
            self.set_flag(super::Flag::C, carry);
            self.set_flag(super::Flag::X, carry);

            InstructionResult::prefetch(8 + ea.cycles)
        } else {
            // Register form: shift Dn
            let shift_count = if is_register {
                (self.d[count as usize] & 0x3F) as u8
            } else if count == 0 {
                8
            } else {
                count
            };

            let value = self.d[reg as usize] & size.mask();

            let result = if shift_count == 0 {
                value
            } else if shift_count < 32 {
                (value << shift_count) & size.mask()
            } else {
                0
            };

            let mask = size.mask();
            self.d[reg as usize] = (self.d[reg as usize] & !mask) | (result & mask);

            self.update_flags_nz(result, size);
            self.set_flag(super::Flag::V, false);

            if shift_count == 0 {
                self.set_flag(super::Flag::C, false);
            } else {
                let bit_size = match size {
                    Size::Byte => 8,
                    Size::Word => 16,
                    Size::Long => 32,
                };
                if shift_count <= bit_size {
                    let last_bit_shifted = value >> (bit_size - shift_count);
                    self.set_flag(super::Flag::C, (last_bit_shifted & 1) != 0);
                    self.set_flag(super::Flag::X, (last_bit_shifted & 1) != 0);
                } else {
                    self.set_flag(super::Flag::C, false);
                    self.set_flag(super::Flag::X, false);
                }
            }

            let cycles = if size == Size::Long { 8 } else { 6 } + 2 * (shift_count as u32);
            InstructionResult::prefetch(cycles)
        }
    }

    /// LSR - Logical Shift Right
    ///
    /// Two forms:
    /// - Register form (ea_mode=None): LSR.b/w/l Dn
    /// - Memory form (ea_mode=Some): LSR.w <ea>, shift count = 1
    #[allow(clippy::too_many_arguments)]
    pub fn lsr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        count: u8,
        reg: u8,
        is_register: bool,
        ea_mode: Option<u8>,
    ) -> InstructionResult {
        if let Some(mode) = ea_mode {
            let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);

            let value = ea.read(self, bus, prefetch, size) & 0xFFFF;

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let carry = (value & 0x0001) != 0;
            let result = (value >> 1) & 0xFFFF;

            ea.write(self, bus, prefetch, size, result);

            // Flags
            self.update_flags_nz(result, Size::Word);
            self.set_flag(super::Flag::V, false);
            self.set_flag(super::Flag::C, carry);
            self.set_flag(super::Flag::X, carry);

            InstructionResult::prefetch(8 + ea.cycles)
        } else {
            // Register form
            let shift_count = if is_register {
                (self.d[count as usize] & 0x3F) as u8
            } else if count == 0 {
                8
            } else {
                count
            };

            let value = self.d[reg as usize] & size.mask();

            let result = if shift_count == 0 {
                value
            } else if shift_count < 32 {
                (value >> shift_count) & size.mask()
            } else {
                0
            };

            let mask = size.mask();
            self.d[reg as usize] = (self.d[reg as usize] & !mask) | (result & mask);

            self.update_flags_nz(result, size);
            self.set_flag(super::Flag::V, false);
            // shift_count==0 then C=0 and X is unchanged
            self.set_flag(super::Flag::C, false);

            if shift_count > 0 && shift_count <= 32 {
                let last_bit_shifted = (value >> (shift_count - 1)) & 1;
                self.set_flag(super::Flag::C, last_bit_shifted != 0);
                self.set_flag(super::Flag::X, last_bit_shifted != 0);
            } else if shift_count > 32 {
                self.set_flag(super::Flag::C, false);
                self.set_flag(super::Flag::X, false);
            }

            let cycles = if size == Size::Long { 8 } else { 6 } + 2 * (shift_count as u32);
            InstructionResult::prefetch(cycles)
        }
    }

    /// ASL - Arithmetic Shift Left
    /// Same as LSL but V flag is set if sign changes during shift
    ///
    /// Two forms:
    /// - Register form (ea_mode=None): ASL.b/w/l Dn
    /// - Memory form (ea_mode=Some): ASL.w <ea>, shift count = 1
    #[allow(clippy::too_many_arguments)]
    pub fn asl(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        count: u8,
        reg: u8,
        is_register: bool,
        ea_mode: Option<u8>,
    ) -> InstructionResult {
        if let Some(mode) = ea_mode {
            // Memory form: shift left by 1
            let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);

            let value = ea.read(self, bus, prefetch, size) & 0xFFFF;

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let msb = (value & 0x8000) != 0;
            let carry = msb;
            let result = (value << 1) & 0xFFFF;
            let new_msb = (result & 0x8000) != 0;

            ea.write(self, bus, prefetch, size, result);

            self.update_flags_nz(result, Size::Word);
            self.set_flag(super::Flag::V, msb != new_msb); // Sign changed
            self.set_flag(super::Flag::C, carry);
            self.set_flag(super::Flag::X, carry);

            InstructionResult::prefetch(8 + ea.cycles)
        } else {
            // Register form
            let shift_count = if is_register {
                (self.d[count as usize] & 0x3F) as u8
            } else if count == 0 {
                8
            } else {
                count
            };

            let value = self.d[reg as usize] & size.mask();
            let bit_size = match size {
                Size::Byte => 8u8,
                Size::Word => 16,
                Size::Long => 32,
            };
            let original_sign = (value >> (bit_size - 1)) & 1;

            let result = if shift_count == 0 {
                value
            } else if shift_count < 32 {
                (value << shift_count) & size.mask()
            } else {
                0
            };

            let mask = size.mask();
            self.d[reg as usize] = (self.d[reg as usize] & !mask) | (result & mask);

            self.update_flags_nz(result, size);

            // V flag: set if sign changed during any shift
            // This happens if any of the top `shift_count` bits (below sign) differ from original sign
            let overflow = if shift_count == 0 {
                false
            } else if shift_count >= bit_size {
                // All bits shift out, result is always 0 (sign=0)
                // V = 1 if original sign was 1 (changed to 0), or if any bit was 1 (passed through sign)
                original_sign != 0 || value != 0
            } else {
                // Check the top shift_count bits below the sign bit
                // These are the bits that will pass through the sign position
                let check_mask = ((1u32 << shift_count) - 1) << (bit_size - 1 - shift_count);
                if original_sign == 0 {
                    (value & check_mask) != 0
                } else {
                    (value & check_mask) != check_mask
                }
            };
            self.set_flag(super::Flag::V, overflow);

            // shift_count==0 then C=0 and X is unchanged
            self.set_flag(super::Flag::C, false);

            if shift_count > 0 {
                if shift_count <= bit_size {
                    let last_bit_shifted = value >> (bit_size - shift_count);
                    self.set_flag(super::Flag::C, (last_bit_shifted & 1) != 0);
                    self.set_flag(super::Flag::X, (last_bit_shifted & 1) != 0);
                } else {
                    self.set_flag(super::Flag::C, false);
                    self.set_flag(super::Flag::X, false);
                }
            }

            let cycles = if size == Size::Long { 8 } else { 6 } + 2 * (shift_count as u32);
            InstructionResult::prefetch(cycles)
        }
    }

    /// ASR - Arithmetic Shift Right
    /// Shifts right while preserving sign bit
    ///
    /// Two forms:
    /// - Register form (ea_mode=None): ASR.b/w/l Dn
    /// - Memory form (ea_mode=Some): ASR.w <ea>, shift count = 1
    #[allow(clippy::too_many_arguments)]
    pub fn asr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        count: u8,
        reg: u8,
        is_register: bool,
        ea_mode: Option<u8>,
    ) -> InstructionResult {
        if let Some(mode) = ea_mode {
            // Memory form: arithmetic shift right by 1
            let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);

            let value = ea.read(self, bus, prefetch, size) & 0xFFFF;

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let carry = (value & 0x0001) != 0;
            let msb = value & 0x8000;
            let result = (value >> 1) | msb; // Preserve sign bit

            ea.write(self, bus, prefetch, size, result);

            self.update_flags_nz(result, Size::Word);
            self.set_flag(super::Flag::V, false);
            self.set_flag(super::Flag::C, carry);
            self.set_flag(super::Flag::X, carry);

            InstructionResult::prefetch(8 + ea.cycles)
        } else {
            // Register form
            let shift_count = if is_register {
                (self.d[count as usize] & 0x3F) as u8
            } else if count == 0 {
                8
            } else {
                count
            };

            let value = self.d[reg as usize] & size.mask();
            let sign_bit = match size {
                Size::Byte => 7,
                Size::Word => 15,
                Size::Long => 31,
            };
            let is_negative = (value >> sign_bit) & 1 != 0;

            let bit_size = size.mask().count_ones();
            let result = if shift_count == 0 {
                value
            } else if shift_count as u32 >= bit_size {
                // Shift amount >= size: result is all 1s or all 0s depending on sign
                if is_negative {
                    size.mask()
                } else {
                    0
                }
            } else {
                let shifted = value >> shift_count;
                if is_negative {
                    // Fill with 1s from the left
                    let fill = size.mask() << (bit_size - shift_count as u32);
                    (shifted | fill) & size.mask()
                } else {
                    shifted & size.mask()
                }
            };

            let mask = size.mask();
            self.d[reg as usize] = (self.d[reg as usize] & !mask) | (result & mask);

            self.update_flags_nz(result, size);
            self.set_flag(super::Flag::V, false);

            if shift_count == 0 {
                self.set_flag(super::Flag::C, false);
                // X unchanged
            } else if (shift_count as u32) > bit_size {
                // When shift > size, all original bits have shifted out
                // C = X = 0 (no more bits to shift)
                self.set_flag(super::Flag::C, false);
                self.set_flag(super::Flag::X, false);
            } else {
                // shift_count <= bit_size: last bit shifted out
                let last_bit_shifted = (value >> (shift_count - 1)) & 1;
                self.set_flag(super::Flag::C, last_bit_shifted != 0);
                self.set_flag(super::Flag::X, last_bit_shifted != 0);
            }

            let cycles = if size == Size::Long { 8 } else { 6 } + 2 * (shift_count as u32);
            InstructionResult::prefetch(cycles)
        }
    }

    /// ROL - Rotate Left
    /// Rotates bits left, MSB goes to LSB and to C flag
    ///
    /// Two forms:
    /// - Register form (ea_mode=None): ROL.b/w/l Dn
    /// - Memory form (ea_mode=Some): ROL.w <ea>, rotate count = 1
    #[allow(clippy::too_many_arguments)]
    pub fn rol(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        count: u8,
        reg: u8,
        is_register: bool,
        ea_mode: Option<u8>,
    ) -> InstructionResult {
        if let Some(mode) = ea_mode {
            // Memory form: rotate left by 1
            let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);

            let value = ea.read(self, bus, prefetch, size) & 0xFFFF;

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let msb = (value & 0x8000) != 0;
            let result = ((value << 1) | (if msb { 1 } else { 0 })) & 0xFFFF;

            ea.write(self, bus, prefetch, size, result);

            self.update_flags_nz(result, Size::Word);
            self.set_flag(super::Flag::V, false);
            self.set_flag(super::Flag::C, msb);
            // ROL does not affect X flag

            InstructionResult::prefetch(8 + ea.cycles)
        } else {
            // Register form
            let shift_count = if is_register {
                (self.d[count as usize] & 0x3F) as u8
            } else if count == 0 {
                8
            } else {
                count
            };

            let value = self.d[reg as usize] & size.mask();
            let bit_size = match size {
                Size::Byte => 8,
                Size::Word => 16,
                Size::Long => 32,
            };

            let result = if shift_count == 0 {
                value
            } else {
                let effective_count = shift_count % bit_size;
                if effective_count == 0 {
                    // Full rotation(s), value unchanged
                    value
                } else {
                    let left = (value << effective_count) & size.mask();
                    let right = (value >> (bit_size - effective_count)) & size.mask();
                    (left | right) & size.mask()
                }
            };

            let mask = size.mask();
            self.d[reg as usize] = (self.d[reg as usize] & !mask) | (result & mask);

            self.update_flags_nz(result, size);
            self.set_flag(super::Flag::V, false);

            if shift_count == 0 {
                self.set_flag(super::Flag::C, false);
            } else {
                // C = last bit rotated out (which is now the LSB)
                self.set_flag(super::Flag::C, (result & 1) != 0);
            }
            // ROL does not affect X flag

            let cycles = if size == Size::Long { 8 } else { 6 } + 2 * (shift_count as u32);
            InstructionResult::prefetch(cycles)
        }
    }

    /// ROR - Rotate Right
    /// Rotates bits right, LSB goes to MSB and to C flag
    ///
    /// Two forms:
    /// - Register form (ea_mode=None): ROR.b/w/l Dn
    /// - Memory form (ea_mode=Some): ROR.w <ea>, rotate count = 1
    #[allow(clippy::too_many_arguments)]
    pub fn ror(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        count: u8,
        reg: u8,
        is_register: bool,
        ea_mode: Option<u8>,
    ) -> InstructionResult {
        if let Some(mode) = ea_mode {
            // Memory form: rotate right by 1
            let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);

            let value = ea.read(self, bus, prefetch, size) & 0xFFFF;

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let lsb = (value & 0x0001) != 0;
            let result = ((value >> 1) | (if lsb { 0x8000 } else { 0 })) & 0xFFFF;

            ea.write(self, bus, prefetch, size, result);

            self.update_flags_nz(result, Size::Word);
            self.set_flag(super::Flag::V, false);
            self.set_flag(super::Flag::C, lsb);
            // ROR does not affect X flag

            InstructionResult::prefetch(8 + ea.cycles)
        } else {
            // Register form
            let shift_count = if is_register {
                (self.d[count as usize] & 0x3F) as u8
            } else if count == 0 {
                8
            } else {
                count
            };

            let value = self.d[reg as usize] & size.mask();
            let bit_size = match size {
                Size::Byte => 8,
                Size::Word => 16,
                Size::Long => 32,
            };

            let (result, last_bit_out) = if shift_count == 0 {
                (value, false)
            } else {
                let effective_count = shift_count % bit_size;
                if effective_count == 0 {
                    // Full rotation(s), value unchanged
                    // Last bit out was the MSB (after full rotations)
                    let msb = (value >> (bit_size - 1)) & 1;
                    (value, msb != 0)
                } else {
                    let last_bit = (value >> (effective_count - 1)) & 1;
                    let right = (value >> effective_count) & size.mask();
                    let left = (value << (bit_size - effective_count)) & size.mask();
                    ((left | right) & size.mask(), last_bit != 0)
                }
            };

            let mask = size.mask();
            self.d[reg as usize] = (self.d[reg as usize] & !mask) | (result & mask);

            self.update_flags_nz(result, size);
            self.set_flag(super::Flag::V, false);

            if shift_count == 0 {
                self.set_flag(super::Flag::C, false);
            } else {
                self.set_flag(super::Flag::C, last_bit_out);
            }
            // ROR does not affect X flag

            let cycles = if size == Size::Long { 8 } else { 6 } + 2 * (shift_count as u32);
            InstructionResult::prefetch(cycles)
        }
    }

    /// ROXL - Rotate Left through Extend
    /// Rotates left including X flag
    ///
    /// Two forms:
    /// - Register form (ea_mode=None): ROXL.b/w/l Dn
    /// - Memory form (ea_mode=Some): ROXL.w <ea>, rotate count = 1
    #[allow(clippy::too_many_arguments)]
    pub fn roxl(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        count: u8,
        reg: u8,
        is_register: bool,
        ea_mode: Option<u8>,
    ) -> InstructionResult {
        if let Some(mode) = ea_mode {
            // Memory form: rotate left through X by 1
            let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);

            let value = ea.read(self, bus, prefetch, size) & 0xFFFF;

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let old_x = self.get_flag(super::Flag::X);
            let msb = (value & 0x8000) != 0;
            let result = ((value << 1) | (if old_x { 1 } else { 0 })) & 0xFFFF;

            ea.write(self, bus, prefetch, size, result);

            self.update_flags_nz(result, Size::Word);
            self.set_flag(super::Flag::V, false);
            self.set_flag(super::Flag::C, msb);
            self.set_flag(super::Flag::X, msb);

            InstructionResult::prefetch(8 + ea.cycles)
        } else {
            // Register form
            let shift_count = if is_register {
                (self.d[count as usize] & 0x3F) as u8
            } else if count == 0 {
                8
            } else {
                count
            };

            let mut value = self.d[reg as usize] & size.mask();
            let bit_size = match size {
                Size::Byte => 8,
                Size::Word => 16,
                Size::Long => 32,
            };

            let mut x_flag = self.get_flag(super::Flag::X);

            for _ in 0..shift_count {
                let msb = (value >> (bit_size - 1)) & 1;
                value = ((value << 1) | (if x_flag { 1 } else { 0 })) & size.mask();
                x_flag = msb != 0;
            }

            let mask = size.mask();
            self.d[reg as usize] = (self.d[reg as usize] & !mask) | (value & mask);

            self.update_flags_nz(value, size);
            self.set_flag(super::Flag::V, false);

            if shift_count == 0 {
                // When shift count is 0, C = X (copy X to C)
                self.set_flag(super::Flag::C, self.get_flag(super::Flag::X));
            } else {
                self.set_flag(super::Flag::C, x_flag);
                self.set_flag(super::Flag::X, x_flag);
            }

            let cycles = if size == Size::Long { 8 } else { 6 } + 2 * (shift_count as u32);
            InstructionResult::prefetch(cycles)
        }
    }

    /// ROXR - Rotate Right through Extend
    /// Rotates right including X flag
    ///
    /// Two forms:
    /// - Register form (ea_mode=None): ROXR.b/w/l Dn
    /// - Memory form (ea_mode=Some): ROXR.w <ea>, rotate count = 1
    #[allow(clippy::too_many_arguments)]
    pub fn roxr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        size: Size,
        count: u8,
        reg: u8,
        is_register: bool,
        ea_mode: Option<u8>,
    ) -> InstructionResult {
        if let Some(mode) = ea_mode {
            // Memory form: rotate right through X by 1
            let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, size);

            let value = ea.read(self, bus, prefetch, size) & 0xFFFF;

            if self.address_error.is_some() {
                return InstructionResult::prefetch(get_exception_cycles(&ea, size));
            }

            let old_x = self.get_flag(super::Flag::X);
            let lsb = (value & 0x0001) != 0;
            let result = ((value >> 1) | (if old_x { 0x8000 } else { 0 })) & 0xFFFF;

            ea.write(self, bus, prefetch, size, result);

            self.update_flags_nz(result, Size::Word);
            self.set_flag(super::Flag::V, false);
            self.set_flag(super::Flag::C, lsb);
            self.set_flag(super::Flag::X, lsb);

            InstructionResult::prefetch(8 + ea.cycles)
        } else {
            // Register form
            let shift_count = if is_register {
                (self.d[count as usize] & 0x3F) as u8
            } else if count == 0 {
                8
            } else {
                count
            };

            let mut value = self.d[reg as usize] & size.mask();
            let bit_size = match size {
                Size::Byte => 8,
                Size::Word => 16,
                Size::Long => 32,
            };

            let mut x_flag = self.get_flag(super::Flag::X);

            for _ in 0..shift_count {
                let lsb = value & 1;
                value = (value >> 1) | (if x_flag { 1 << (bit_size - 1) } else { 0 });
                value &= size.mask();
                x_flag = lsb != 0;
            }

            let mask = size.mask();
            self.d[reg as usize] = (self.d[reg as usize] & !mask) | (value & mask);

            self.update_flags_nz(value, size);
            self.set_flag(super::Flag::V, false);

            if shift_count == 0 {
                // When shift count is 0, C = X (copy X to C)
                self.set_flag(super::Flag::C, self.get_flag(super::Flag::X));
            } else {
                self.set_flag(super::Flag::C, x_flag);
                self.set_flag(super::Flag::X, x_flag);
            }

            let cycles = if size == Size::Long { 8 } else { 6 } + 2 * (shift_count as u32);
            InstructionResult::prefetch(cycles)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_lsl_immediate() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b00000001;
        cpu.pc = 0x1000;

        // LSL.B #3, D0
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.lsl(&mut ram, &mut prefetch, Size::Byte, 3, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b00001000);
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_lsl_carry() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b11000000; // Byte
        cpu.pc = 0x1000;

        // LSL.B #1, D0 (shift left by 1, MSB goes into carry)
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.lsl(&mut ram, &mut prefetch, Size::Byte, 1, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b10000000);
        assert!(cpu.get_flag(super::super::Flag::C)); // Carry set
        assert!(cpu.get_flag(super::super::Flag::X)); // Extend set
        assert!(cpu.get_flag(super::super::Flag::N)); // Negative
    }

    #[test]
    fn test_lsl_shift_out() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0xFF;
        cpu.pc = 0x1000;

        // LSL.B #8, D0 (shift out everything)
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.lsl(&mut ram, &mut prefetch, Size::Byte, 0, 0, false, None); // 0 means 8

        assert_eq!(cpu.d[0] & 0xFF, 0);
        assert!(cpu.get_flag(super::super::Flag::Z));
    }

    #[test]
    fn test_lsr_immediate() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b10000000;
        cpu.pc = 0x1000;

        // LSR.B #3, D0
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.lsr(&mut ram, &mut prefetch, Size::Byte, 3, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b00010000);
        assert!(!cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_lsr_carry() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b00000011; // Byte
        cpu.pc = 0x1000;

        // LSR.B #1, D0 (shift right by 1, LSB goes into carry)
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.lsr(&mut ram, &mut prefetch, Size::Byte, 1, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b00000001);
        assert!(cpu.get_flag(super::super::Flag::C)); // Carry set
        assert!(cpu.get_flag(super::super::Flag::X)); // Extend set
    }

    #[test]
    fn test_lsr_zero() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b00000001;
        cpu.pc = 0x1000;

        // LSR.B #1, D0
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.lsr(&mut ram, &mut prefetch, Size::Byte, 1, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0);
        assert!(cpu.get_flag(super::super::Flag::Z));
        assert!(!cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_lsl_register() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b00000001;
        cpu.d[1] = 4; // Shift count in D1
        cpu.pc = 0x1000;

        // LSL.B D1, D0 (shift D0 left by value in D1)
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.lsl(&mut ram, &mut prefetch, Size::Byte, 1, 0, true, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b00010000);
    }

    #[test]
    fn test_asl_basic() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b00000010;

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.asl(&mut ram, &mut prefetch, Size::Byte, 2, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b00001000);
        assert!(!cpu.get_flag(super::super::Flag::V)); // No overflow
    }

    #[test]
    fn test_asl_overflow() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b01000000; // Positive

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.asl(&mut ram, &mut prefetch, Size::Byte, 1, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b10000000); // Now negative
        assert!(cpu.get_flag(super::super::Flag::V)); // Overflow!
        assert!(cpu.get_flag(super::super::Flag::N)); // Negative
    }

    #[test]
    fn test_asr_positive() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b01000000;

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.asr(&mut ram, &mut prefetch, Size::Byte, 2, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b00010000); // Zero-filled
        assert!(!cpu.get_flag(super::super::Flag::N));
    }

    #[test]
    fn test_asr_negative() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b10000000; // -128

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.asr(&mut ram, &mut prefetch, Size::Byte, 2, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b11100000); // Sign-extended (1s filled)
        assert!(cpu.get_flag(super::super::Flag::N)); // Still negative
    }

    #[test]
    fn test_rol_basic() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b10000001;

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.rol(&mut ram, &mut prefetch, Size::Byte, 1, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b00000011); // MSB rotated to LSB
        assert!(cpu.get_flag(super::super::Flag::C)); // C = bit rotated out (original MSB = 1)
    }

    #[test]
    fn test_rol_carry() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b10000000;

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.rol(&mut ram, &mut prefetch, Size::Byte, 1, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b00000001);
        assert!(cpu.get_flag(super::super::Flag::C)); // C = bit rotated out (original MSB = 1)
    }

    #[test]
    fn test_ror_basic() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b10000001;

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.ror(&mut ram, &mut prefetch, Size::Byte, 1, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b11000000); // LSB rotated to MSB
        assert!(cpu.get_flag(super::super::Flag::C)); // C gets LSB (which was 1)
    }

    #[test]
    fn test_ror_multiple() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b10000001;

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.ror(&mut ram, &mut prefetch, Size::Byte, 3, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b00110000); // 001 rotated to top
    }

    #[test]
    fn test_roxl_basic() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b10000001;
        cpu.set_flag(super::super::Flag::X, false); // X = 0

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.roxl(&mut ram, &mut prefetch, Size::Byte, 1, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b00000010); // MSB->X, X(0)->LSB
        assert!(cpu.get_flag(super::super::Flag::X)); // X = old MSB (1)
        assert!(cpu.get_flag(super::super::Flag::C)); // C = X
    }

    #[test]
    fn test_roxl_with_extend() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b00000000;
        cpu.set_flag(super::super::Flag::X, true); // X = 1

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.roxl(&mut ram, &mut prefetch, Size::Byte, 1, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b00000001); // X(1) rotated into LSB
        assert!(!cpu.get_flag(super::super::Flag::X)); // X = old MSB (0)
    }

    #[test]
    fn test_roxr_basic() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b10000001;
        cpu.set_flag(super::super::Flag::X, false); // X = 0

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.roxr(&mut ram, &mut prefetch, Size::Byte, 1, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b01000000); // LSB->X, X(0)->MSB
        assert!(cpu.get_flag(super::super::Flag::X)); // X = old LSB (1)
        assert!(cpu.get_flag(super::super::Flag::C)); // C = X
    }

    #[test]
    fn test_roxr_with_extend() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0b00000000;
        cpu.set_flag(super::super::Flag::X, true); // X = 1

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.roxr(&mut ram, &mut prefetch, Size::Byte, 1, 0, false, None);

        assert_eq!(cpu.d[0] & 0xFF, 0b10000000); // X(1) rotated into MSB
        assert!(!cpu.get_flag(super::super::Flag::X)); // X = old LSB (0)
    }
}
