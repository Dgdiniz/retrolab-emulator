//! Control flow instructions: NOP, JMP, JSR, RTS.

use super::{InstructionResult, M68k};
use crate::cpu68k::addressing::{AddressingMode, EffectiveAddress, Size};
use crate::cpu68k::decode;
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

//TODO: This table is specific to JMP; it needs separating.
fn get_exception_cycles(src_ea: &EffectiveAddress, size: Size) -> u32 {
    let src_idx = ea_to_index(&src_ea.mode);

    // Cycle tables for MOVE Byte/Word
    // Rows = Source EA, Columns = Destination EA
    #[rustfmt::skip]
    const MOVE_BYTE_WORD_CYCLES: [u32; 12] = [
        // Dn    An    (An)  (An)+ -(An) d(An) d(An,ix) xxx.W xxx.L d(PC) d(PC,ix) #xxx
          10,   10,    10,   10,   12,    8,   12,      8,    6,    8,   12,      10  
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
    /// NOP - No Operation
    /// Opcode: 0100111001110001 (0x4E71)
    pub fn nop(&mut self) -> InstructionResult {
        // Does nothing, just consumes cycles
        InstructionResult::new(4, 2)
    }

    /// RTS - Return from Subroutine
    /// Opcode: 0100111001110101 (0x4E75)
    pub fn rts(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        // Pop PC from stack
        let sp = self.a[7];
        let pc_high = bus.read_word(sp) as u32;
        let pc_low = bus.read_word(sp.wrapping_add(2)) as u32;

        // Update A7 (stack pop happens regardless of Address Error)
        self.a[7] = sp.wrapping_add(4);

        // Check for odd target address (Address Error)
        // Use the RAW PC value (before 24-bit masking) for address error
        let raw_pc = (pc_high << 16) | pc_low;
        if raw_pc & 1 != 0 {
            // For RTS Address Error, use RAW PC from stack
            self.address_error = Some(crate::cpu68k::AddressErrorInfo {
                access_addr: raw_pc,
                pc_at_error: raw_pc.wrapping_sub(4),
                is_write: false,
                function_code: 0x06,
                in_bit: true,
                sr_for_frame: None,
                force_supervisor_stack: false,
            });
            // Cycles: 10 base for Address Error in RTS
            return InstructionResult::prefetch(10);
        }

        self.pc = raw_pc; // Store full 32-bit value; bus masks to 24 bits on access
        InstructionResult::branch_taken(16)
    }

    /// JMP - Jump
    /// Opcode: 0100111011mmmrrr
    pub fn jmp(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        // Use prefetch to decode EA properly
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, Size::Word);

        // Calculate the effective address (where to jump to)
        let target_pc = ea.calculate_address(self, prefetch.base());

        // Check for odd target address (Address Error)
        if target_pc & 1 != 0 {
            self.address_error = Some(crate::cpu68k::AddressErrorInfo {
                access_addr: target_pc,
                pc_at_error: target_pc.wrapping_sub(4),
                is_write: false,
                function_code: 0x06, // Supervisor program
                in_bit: true,
                sr_for_frame: None,
                force_supervisor_stack: false,
            });

            return InstructionResult::prefetch(get_exception_cycles(&ea, Size::Word));
        }

        // Set PC to target (full 32-bit; bus masks to 24 bits on access)
        self.pc = target_pc;

        // Cycles depend on addressing mode
        let cycles = match ea.mode {
            AddressingMode::AddressIndirect(_) => 8,
            AddressingMode::AddressIndirectDisplacement(_, _) => 10,
            AddressingMode::AddressIndirectIndex(_, _, _, _) => 14,
            AddressingMode::AbsoluteShort(_) => 10,
            AddressingMode::AbsoluteLong(_) => 12,
            AddressingMode::PcDisplacement(_) => 10,
            AddressingMode::PcIndex(_, _, _) => 14,
            _ => {
                self.illegal_instruction_pending.set(true);
                return InstructionResult::new(4, 0);
            }
        };

        InstructionResult::branch_taken(cycles)
    }

    /// JSR - Jump to Subroutine
    /// Opcode: 0100111010mmmrrr
    pub fn jsr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        // Use prefetch to decode EA properly
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, Size::Word);

        // Calculate the effective address (where to jump to)
        let target_pc = ea.calculate_address(self, prefetch.base());

        // Check for odd target address (Address Error)
        if target_pc & 1 != 0 {
            self.address_error = Some(crate::cpu68k::AddressErrorInfo {
                access_addr: target_pc,
                pc_at_error: target_pc.wrapping_sub(4),
                is_write: false,
                function_code: 0x06, // Supervisor program
                in_bit: true,
                sr_for_frame: None,
                force_supervisor_stack: false,
            });

            return InstructionResult::prefetch(get_exception_cycles(&ea, Size::Word));
        }

        // Calculate return address (PC after this instruction)
        let return_pc = self.pc.wrapping_add(2).wrapping_add(prefetch.consumed());

        // Push return address onto stack FIRST (before Address Error check)
        self.a[7] = self.a[7].wrapping_sub(4);
        let sp = self.a[7];
        bus.write_word(sp, (return_pc >> 16) as u16);
        bus.write_word(sp + 2, return_pc as u16);

        // Set PC to target (full 32-bit; bus masks to 24 bits on access)
        self.pc = target_pc;

        // Cycles depend on addressing mode
        let cycles = match ea.mode {
            AddressingMode::AddressIndirect(_) => 16,
            AddressingMode::AddressIndirectDisplacement(_, _) => 18,
            AddressingMode::AddressIndirectIndex(_, _, _, _) => 22,
            AddressingMode::AbsoluteShort(_) => 18,
            AddressingMode::AbsoluteLong(_) => 20,
            AddressingMode::PcDisplacement(_) => 18,
            AddressingMode::PcIndex(_, _, _) => 22,
            _ => {
                self.illegal_instruction_pending.set(true);
                return InstructionResult::new(4, 0);
            }
        };

        InstructionResult::branch_taken(cycles)
    }

    /// RTR - Return and Restore Condition Codes
    /// Opcode: 0100111001110111 (0x4E77)
    /// Like RTS but also pops CCR from stack
    pub fn rtr(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        // Pop CCR from stack (word, but only lower byte matters)
        let ccr = bus.read_word(self.a[7]) & 0x001F; // Only X, N, Z, V, C bits
        self.a[7] = self.a[7].wrapping_add(2);

        // Pop PC from stack
        let pc_high = bus.read_word(self.a[7]) as u32;
        let pc_low = bus.read_word(self.a[7].wrapping_add(2)) as u32;
        self.a[7] = self.a[7].wrapping_add(4);

        // Update CCR (preserve upper byte of SR)
        self.sr = (self.sr & 0xFF00) | ccr;

        // Check for odd target address (Address Error) using RAW PC
        let raw_pc = (pc_high << 16) | pc_low;
        if raw_pc & 1 != 0 {
            self.address_error = Some(crate::cpu68k::AddressErrorInfo {
                access_addr: raw_pc,
                pc_at_error: raw_pc.wrapping_sub(4),
                is_write: false,
                function_code: 0x06,
                in_bit: true,
                sr_for_frame: None,
                force_supervisor_stack: false,
            });
            return InstructionResult::prefetch(10);
        }

        // Store full 32-bit value; bus masks to 24 bits on access
        self.pc = raw_pc;
        InstructionResult::branch_taken(20)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_nop() {
        let mut cpu = M68k::new();
        let result = cpu.nop();

        assert_eq!(result.cycles, 4);
        assert_eq!(result.pc_increment, 2);
    }

    #[test]
    fn test_rts() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        // Setup stack with return address 0x00001234
        cpu.a[7] = 0x1000; // Stack pointer
        ram.write_word(0x1000, 0x0000); // High word
        ram.write_word(0x1002, 0x1234); // Low word

        let result = cpu.rts(&mut ram);

        assert_eq!(cpu.pc, 0x00001234);
        assert_eq!(cpu.a[7], 0x1004); // Stack pointer incremented
        assert_eq!(result.cycles, 16);
        assert_eq!(result.pc_increment, 0); // PC already set
    }

    #[test]
    fn test_jmp_absolute_long() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x1000;
        // Absolute long address: 0x00005678. Extension words live at PC+2
        // and PC+4 (prefetch starts after the opcode).
        ram.write_word(0x1002, 0x0000);
        ram.write_word(0x1004, 0x5678);

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.jmp(&mut ram, &mut prefetch, 7, 1); // Mode 7, reg 1 = Absolute Long

        assert_eq!(cpu.pc, 0x00005678);
        assert!(result.cycles >= 8);
        assert_eq!(result.pc_increment, 0);
    }

    #[test]
    fn test_jsr_and_rts() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x1000;
        cpu.a[7] = 0x2000; // Stack pointer

        // JSR to 0x00005000 (absolute long). Extension words at PC+2 / PC+4.
        ram.write_word(0x1002, 0x0000);
        ram.write_word(0x1004, 0x5000);

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.jsr(&mut ram, &mut prefetch, 7, 1);
        assert_eq!(cpu.pc, 0x00005000);
        assert_eq!(cpu.a[7], 0x1FFC); // Stack decremented by 4

        // Check return address on stack
        let ret_high = ram.read_word(0x1FFC);
        let ret_low = ram.read_word(0x1FFE);
        let return_addr = ((ret_high as u32) << 16) | (ret_low as u32);
        assert_eq!(return_addr, 0x1006); // PC + 6 (instruction + 4-byte extension)

        // Now RTS
        let result = cpu.rts(&mut ram);
        assert_eq!(cpu.pc, 0x1006);
        assert_eq!(cpu.a[7], 0x2000); // Stack restored
    }

    #[test]
    fn test_rtr() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.sr = 0x2700; // Supervisor mode, all flags clear
        cpu.a[7] = 0x1000; // Stack pointer

        // Setup stack with CCR = 0x1F (all flags set) and return address 0x00005678
        ram.write_word(0x1000, 0x001F); // CCR (only lower byte matters)
        ram.write_word(0x1002, 0x0000); // PC high
        ram.write_word(0x1004, 0x5678); // PC low

        let result = cpu.rtr(&mut ram);

        assert_eq!(cpu.pc, 0x00005678);
        assert_eq!(cpu.a[7], 0x1006); // Stack pointer incremented by 6
        assert_eq!(cpu.sr & 0x00FF, 0x001F); // CCR updated
        assert_eq!(cpu.sr & 0xFF00, 0x2700); // Upper byte preserved
        assert_eq!(result.cycles, 20);
    }
}
