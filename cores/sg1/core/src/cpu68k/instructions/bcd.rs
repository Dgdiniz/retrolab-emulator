//! BCD (Binary Coded Decimal) instructions: ABCD, SBCD, NBCD.

use super::{InstructionResult, M68k};
use crate::cpu68k::addressing::{AddressingMode, EffectiveAddress, Size};
use crate::cpu68k::prefetch::Prefetch;
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
    /// ABCD - Add BCD with Extend
    /// Opcode: 1100rrr10000mrrr (register) or 1100rrr10001mrrr (memory)
    pub fn abcd(
        &mut self,
        bus: &mut dyn Addressable,
        _prefetch: &mut Prefetch,
        dst_reg: u8,
        src_reg: u8,
        memory_mode: bool,
    ) -> InstructionResult {
        let (src, dst, dst_addr_for_write) = if memory_mode {
            // -(Ay), -(Ax) mode - always decrement by 2 for A7 (SP) to maintain alignment
            let src_decrement = if src_reg == 7 { 2 } else { 1 };
            let dst_decrement = if dst_reg == 7 { 2 } else { 1 };

            // Source: -(Ay)
            let src_addr = self.a[src_reg as usize].wrapping_sub(src_decrement);
            self.a[src_reg as usize] = src_addr;
            let src = bus.read(src_addr);

            // Destination: -(Ax)
            // ALWAYS decrement, even if src_reg == dst_reg (decrements twice)
            let dst_addr = self.a[dst_reg as usize].wrapping_sub(dst_decrement);
            self.a[dst_reg as usize] = dst_addr;
            let dst = bus.read(dst_addr);

            (src, dst, Some(dst_addr))
        } else {
            let src = (self.d[src_reg as usize] & 0xFF) as u8;
            let dst = (self.d[dst_reg as usize] & 0xFF) as u8;
            (src, dst, None)
        };

        // BCD addition with extend
        let x = if self.get_flag(super::Flag::X) { 1 } else { 0 };

        // 1) Binary sum (uncorrected)
        let uncorrected = (src as u16) + (dst as u16) + (x as u16); // 0..0x1FF
        let un8 = (uncorrected & 0xFF) as u8;
        let carry1 = uncorrected > 0xFF;

        // 2) Decide low nibble correction (0x06)
        let half_carry = ((src & 0x0F) + (dst & 0x0F) + x) > 0x0F;
        let need_low = ((un8 & 0x0F) > 9) || half_carry;
        let mut correction = if need_low { 0x06 } else { 0x00 };

        // 3) Decide high nibble correction (0x60)
        // Use u16 to avoid wrapping for the comparison
        let tmp16 = (un8 as u16) + (correction as u16);
        let need_high = (tmp16 > 0x9F) || carry1;
        if need_high {
            correction |= 0x60;
        }

        // 4) Second internal sum: un8 + correction
        let result = un8.wrapping_add(correction);

        // C/X: decimal carry
        let carry = need_high;

        // Write result
        if let Some(addr) = dst_addr_for_write {
            bus.write(addr, result);
        } else {
            self.d[dst_reg as usize] = (self.d[dst_reg as usize] & 0xFFFFFF00) | (result as u32);
        }

        // Update flags
        if result != 0 {
            self.set_flag(super::Flag::Z, false);
        }
        // N = MSB of result (hardware behavior)
        self.set_flag(super::Flag::N, (result & 0x80) != 0);
        // V = overflow of the second internal sum (un8 + correction)
        // Formula: (~(a ^ b) & (a ^ result) & 0x80) != 0
        let overflow = (!(un8 ^ correction) & (un8 ^ result) & 0x80) != 0;
        self.set_flag(super::Flag::V, overflow);
        self.set_flag(super::Flag::C, carry);
        self.set_flag(super::Flag::X, carry);

        let cycles = if memory_mode { 18 } else { 6 };
        InstructionResult::prefetch(cycles)
    }

    /// SBCD - Subtract BCD with Extend
    /// Opcode: 1000rrr10000mrrr (register) or 1000rrr10001mrrr (memory)
    pub fn sbcd(
        &mut self,
        bus: &mut dyn Addressable,
        _prefetch: &mut Prefetch,
        dst_reg: u8,
        src_reg: u8,
        memory_mode: bool,
    ) -> InstructionResult {
        let (src, dst, dst_addr_for_write) = if memory_mode {
            // -(Ay), -(Ax) mode - always decrement by 2 for A7 (SP) to maintain alignment
            let src_decrement = if src_reg == 7 { 2 } else { 1 };
            let dst_decrement = if dst_reg == 7 { 2 } else { 1 };

            // Source: -(Ay)
            let src_addr = self.a[src_reg as usize].wrapping_sub(src_decrement);
            self.a[src_reg as usize] = src_addr;
            let src = bus.read(src_addr);

            // Destination: -(Ax)
            // ALWAYS decrement, even if src_reg == dst_reg (decrements twice)
            let dst_addr = self.a[dst_reg as usize].wrapping_sub(dst_decrement);
            self.a[dst_reg as usize] = dst_addr;
            let dst = bus.read(dst_addr);

            (src, dst, Some(dst_addr))
        } else {
            let src = (self.d[src_reg as usize] & 0xFF) as u8;
            let dst = (self.d[dst_reg as usize] & 0xFF) as u8;
            (src, dst, None)
        };

        // BCD subtraction with extend
        let x = if self.get_flag(super::Flag::X) { 1 } else { 0 };

        // 1) Binary subtraction (uncorrected)
        let un = (dst as i16) - (src as i16) - (x as i16); // range -256..255
        let un8 = (un & 0xFF) as u8; // wrap to 8-bit

        // 2) Calculate BCD correction factor (corf)
        let mut corf: u8 = 0;

        // Low nibble borrow
        let low = (dst & 0x0F) as i8 - (src & 0x0F) as i8 - x as i8;
        if low < 0 {
            corf |= 0x06;
        }

        // High nibble borrow (based on uncorrected)
        if un < 0 {
            corf |= 0x60;
        }

        // 3) Second internal subtraction: result = un8 - corf
        let a = un8;
        let b = corf;
        let r16 = (a as i16) - (b as i16);
        let result = (r16 & 0xFF) as u8;

        // C/X: borrow if EITHER subtraction had borrow
        let borrow = (un < 0) || (r16 < 0);

        // Write result
        if let Some(addr) = dst_addr_for_write {
            bus.write(addr, result);
        } else {
            self.d[dst_reg as usize] = (self.d[dst_reg as usize] & 0xFFFFFF00) | (result as u32);
        }

        // Update flags
        if result != 0 {
            self.set_flag(super::Flag::Z, false);
        }
        // N = MSB of result (hardware behavior)
        self.set_flag(super::Flag::N, (result & 0x80) != 0);
        // V = overflow of the second internal subtraction (un8 - corf)
        // For subtraction: V = (a ^ b) & (a ^ result) & 0x80
        let overflow = ((a ^ b) & (a ^ result) & 0x80) != 0;
        self.set_flag(super::Flag::V, overflow);
        self.set_flag(super::Flag::C, borrow);
        self.set_flag(super::Flag::X, borrow);

        let cycles = if memory_mode { 18 } else { 6 };
        InstructionResult::prefetch(cycles)
    }

    /// NBCD - Negate BCD with Extend
    /// Opcode: 0100100000mmmrrr
    /// 0 - operand - X (in BCD)
    pub fn nbcd(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        use crate::cpu68k::decode;

        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, Size::Byte);
        let src = ea.read(self, bus, prefetch, Size::Byte) as u8;

        // Check for Address Error before continuing
        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, Size::Byte));
        }

        let x = if self.get_flag(super::Flag::X) { 1 } else { 0 };

        // BCD negation: 0 - src - X (same as SBCD with dst=0)
        let dst = 0u8;

        // 1) Binary subtraction (uncorrected)
        let un = (dst as i16) - (src as i16) - (x as i16); // range -256..255
        let un8 = (un & 0xFF) as u8; // wrap to 8-bit

        // 2) Calculate BCD correction factor (corf)
        let mut corf: u8 = 0;

        // Low nibble borrow
        let low = (dst & 0x0F) as i8 - (src & 0x0F) as i8 - x as i8;
        if low < 0 {
            corf |= 0x06;
        }

        // High nibble borrow (based on uncorrected)
        if un < 0 {
            corf |= 0x60;
        }

        // 3) Second internal subtraction: result = un8 - corf
        let a = un8;
        let b = corf;
        let r16 = (a as i16) - (b as i16);
        let result = (r16 & 0xFF) as u32;

        // C/X: borrow if EITHER subtraction had borrow
        let borrow = (un < 0) || (r16 < 0);

        ea.write(self, bus, prefetch, Size::Byte, result);

        // Update flags
        if result != 0 {
            self.set_flag(super::Flag::Z, false);
        }
        // N = MSB of result (hardware behavior)
        self.set_flag(super::Flag::N, (result & 0x80) != 0);
        // V = overflow of the second internal subtraction (un8 - corf)
        // For subtraction: V = (a ^ b) & (a ^ result) & 0x80
        let overflow = ((a ^ b) & (a ^ (result as u8)) & 0x80) != 0;
        self.set_flag(super::Flag::V, overflow);
        self.set_flag(super::Flag::C, borrow);
        self.set_flag(super::Flag::X, borrow);

        let cycles = if mode == 0 { 6 } else { 8 + ea.cycles };
        InstructionResult::prefetch(cycles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_abcd_simple() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);
        let mut prefetch = Prefetch::new(0);

        cpu.d[0] = 0x09; // 9 in BCD
        cpu.d[1] = 0x01; // 1 in BCD

        let result = cpu.abcd(&mut ram, &mut prefetch, 0, 1, false);

        assert_eq!(cpu.d[0] & 0xFF, 0x10); // 9 + 1 = 10 in BCD
    }

    #[test]
    fn test_abcd_with_adjust() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);
        let mut prefetch = Prefetch::new(0);

        cpu.d[0] = 0x09; // 9 in BCD
        cpu.d[1] = 0x09; // 9 in BCD

        let result = cpu.abcd(&mut ram, &mut prefetch, 0, 1, false);

        assert_eq!(cpu.d[0] & 0xFF, 0x18); // 9 + 9 = 18 in BCD
    }

    #[test]
    fn test_abcd_with_carry() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);
        let mut prefetch = Prefetch::new(0);

        cpu.d[0] = 0x99; // 99 in BCD
        cpu.d[1] = 0x01; // 1 in BCD

        let result = cpu.abcd(&mut ram, &mut prefetch, 0, 1, false);

        assert_eq!(cpu.d[0] & 0xFF, 0x00); // 99 + 1 = 100, but only 8 bits (00)
        assert!(cpu.get_flag(super::super::Flag::C)); // Carry set
        assert!(cpu.get_flag(super::super::Flag::X)); // Extend set
    }

    #[test]
    fn test_sbcd_simple() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);
        let mut prefetch = Prefetch::new(0);

        cpu.d[0] = 0x09; // 9 in BCD
        cpu.d[1] = 0x05; // 5 in BCD

        let result = cpu.sbcd(&mut ram, &mut prefetch, 0, 1, false);

        assert_eq!(cpu.d[0] & 0xFF, 0x04); // 9 - 5 = 4 in BCD
    }

    #[test]
    fn test_sbcd_with_borrow() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);
        let mut prefetch = Prefetch::new(0);

        cpu.d[0] = 0x10; // 10 in BCD
        cpu.d[1] = 0x01; // 1 in BCD

        let result = cpu.sbcd(&mut ram, &mut prefetch, 0, 1, false);

        assert_eq!(cpu.d[0] & 0xFF, 0x09); // 10 - 1 = 09 in BCD
    }

    #[test]
    fn test_nbcd_zero() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);
        let mut prefetch = Prefetch::new(0);

        cpu.d[0] = 0x00;

        let result = cpu.nbcd(&mut ram, &mut prefetch, 0, 0);

        assert_eq!(cpu.d[0] & 0xFF, 0x00);
    }

    #[test]
    fn test_nbcd_simple() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);
        let mut prefetch = Prefetch::new(0);

        cpu.d[0] = 0x01; // 1 in BCD

        let result = cpu.nbcd(&mut ram, &mut prefetch, 0, 0);

        assert_eq!(cpu.d[0] & 0xFF, 0x99); // 0 - 1 = 99 in BCD (with borrow)
    }
}
