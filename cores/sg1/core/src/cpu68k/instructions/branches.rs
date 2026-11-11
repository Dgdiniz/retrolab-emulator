//! Branch instructions: Bcc (BRA, BEQ, BNE, etc.).

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
    /// Bcc - Branch Conditionally
    /// Opcode: 0110ccccdddddddd
    /// where cccc = condition code, dddddddd = 8-bit displacement (or 0x00 for 16-bit displacement in extension word)
    pub fn branch(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        condition: u8,
        displacement: i8,
    ) -> InstructionResult {
        // Get the full displacement using prefetch (like other instructions)
        let disp = if displacement == 0 {
            // 16-bit displacement in extension word
            prefetch.read_word(bus) as i16 as i32
        } else {
            // 8-bit displacement (sign-extended)
            displacement as i32
        };

        // Test condition
        let condition_met = self.test_condition(condition);

        if condition_met {
            // Branch taken: PC = instruction_pc + 2 + displacement
            // (displacement is relative to PC after opcode fetch)
            // Note: self.pc is still instruction_pc at this point (not incremented yet)
            let instruction_pc = self.pc;
            let target_pc_raw = instruction_pc.wrapping_add(2).wrapping_add(disp as u32);

            // Check for odd target address (Address Error)
            // Note: Use raw (un-masked) target for Address Error frame
            if target_pc_raw & 1 != 0 {
                // For branch Address Error:
                // - PC in stack frame is target_pc - 4
                // - FC = 6 (supervisor program)
                // - I/N = 1
                self.address_error = Some(crate::cpu68k::AddressErrorInfo {
                    access_addr: target_pc_raw,
                    pc_at_error: target_pc_raw.wrapping_sub(4),
                    is_write: false,
                    function_code: 0x06,
                    in_bit: true,
                    sr_for_frame: None,
                    force_supervisor_stack: false,
                });
                // Address Error base cycles: 12 for 8-bit, 8 for 16-bit
                // (16-bit already read the displacement word via prefetch)
                let cycles = if displacement == 0 { 8 } else { 12 };
                return InstructionResult::prefetch(cycles);
            }

            // Store full 32-bit value; bus masks to 24 bits on access
            self.pc = target_pc_raw;

            // Cycles: 10 for 8-bit, 10 for 16-bit
            InstructionResult::branch_taken(10)
        } else {
            // Branch not taken
            // Cycles: 8 for 8-bit, 12 for 16-bit
            let cycles = if displacement == 0 { 12 } else { 8 };
            InstructionResult::prefetch(cycles)
        }
    }

    /// BRA - Branch Always
    /// This is just Bcc with condition = 0 (T - always true)
    pub fn bra(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        displacement: i8,
    ) -> InstructionResult {
        self.branch(bus, prefetch, 0x0, displacement)
    }

    /// BEQ - Branch if Equal (Z=1)
    /// This is Bcc with condition = 0x7
    pub fn beq(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        displacement: i8,
    ) -> InstructionResult {
        self.branch(bus, prefetch, 0x7, displacement)
    }

    /// BNE - Branch if Not Equal (Z=0)
    /// This is Bcc with condition = 0x6
    pub fn bne(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        displacement: i8,
    ) -> InstructionResult {
        self.branch(bus, prefetch, 0x6, displacement)
    }

    /// BCC - Branch if Carry Clear (C=0)
    pub fn bcc(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        displacement: i8,
    ) -> InstructionResult {
        self.branch(bus, prefetch, 0x4, displacement)
    }

    /// BCS - Branch if Carry Set (C=1)
    pub fn bcs(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        displacement: i8,
    ) -> InstructionResult {
        self.branch(bus, prefetch, 0x5, displacement)
    }

    /// BGE - Branch if Greater or Equal (N=V)
    pub fn bge(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        displacement: i8,
    ) -> InstructionResult {
        self.branch(bus, prefetch, 0xC, displacement)
    }

    /// BLT - Branch if Less Than (N!=V)
    pub fn blt(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        displacement: i8,
    ) -> InstructionResult {
        self.branch(bus, prefetch, 0xD, displacement)
    }

    /// BGT - Branch if Greater Than (Z=0 AND N=V)
    pub fn bgt(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        displacement: i8,
    ) -> InstructionResult {
        self.branch(bus, prefetch, 0xE, displacement)
    }

    /// BLE - Branch if Less or Equal (Z=1 OR N!=V)
    pub fn ble(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        displacement: i8,
    ) -> InstructionResult {
        self.branch(bus, prefetch, 0xF, displacement)
    }

    /// BHI - Branch if Higher (C=0 AND Z=0)
    pub fn bhi(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        displacement: i8,
    ) -> InstructionResult {
        self.branch(bus, prefetch, 0x2, displacement)
    }

    /// BLS - Branch if Lower or Same (C=1 OR Z=1)
    pub fn bls(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        displacement: i8,
    ) -> InstructionResult {
        self.branch(bus, prefetch, 0x3, displacement)
    }

    /// BPL - Branch if Plus (N=0)
    pub fn bpl(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        displacement: i8,
    ) -> InstructionResult {
        self.branch(bus, prefetch, 0xA, displacement)
    }

    /// BMI - Branch if Minus (N=1)
    pub fn bmi(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        displacement: i8,
    ) -> InstructionResult {
        self.branch(bus, prefetch, 0xB, displacement)
    }

    /// BVC - Branch if Overflow Clear (V=0)
    pub fn bvc(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        displacement: i8,
    ) -> InstructionResult {
        self.branch(bus, prefetch, 0x8, displacement)
    }

    /// BVS - Branch if Overflow Set (V=1)
    pub fn bvs(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        displacement: i8,
    ) -> InstructionResult {
        self.branch(bus, prefetch, 0x9, displacement)
    }

    /// BSR - Branch to Subroutine
    /// Opcode: 01100001dddddddd
    /// Pushes return address on stack, then branches
    pub fn bsr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        displacement: i8,
    ) -> InstructionResult {
        // Get the full displacement using prefetch
        let is_16bit = displacement == 0;
        let disp = if is_16bit {
            prefetch.read_word(bus) as i16 as i32
        } else {
            displacement as i32
        };

        let instruction_pc = self.pc;
        let target_pc_raw = instruction_pc.wrapping_add(2).wrapping_add(disp as u32);

        // Calculate return address (address after BSR instruction)
        // For 8-bit: instruction_pc + 2
        // For 16-bit: instruction_pc + 4
        let return_addr = instruction_pc
            .wrapping_add(2)
            .wrapping_add(if is_16bit { 2 } else { 0 });

        // Push return address onto stack FIRST (before Address Error check)
        // The real 68000 pushes the return address before detecting the odd target
        self.a[7] = self.a[7].wrapping_sub(4);
        bus.write_word(self.a[7], (return_addr >> 16) as u16);
        bus.write_word(self.a[7].wrapping_add(2), return_addr as u16);

        // Check for odd target address (Address Error)
        if target_pc_raw & 1 != 0 {
            // For BSR Address Error:
            // - PC in stack frame is target_pc - 4
            // - FC = 6 (supervisor program)
            // - I/N = 1
            self.address_error = Some(crate::cpu68k::AddressErrorInfo {
                access_addr: target_pc_raw,
                pc_at_error: target_pc_raw.wrapping_sub(4),
                is_write: false,
                function_code: 0x06,
                in_bit: true,
                sr_for_frame: None,
                force_supervisor_stack: false,
            });
            // Address Error base cycles: 12 for 8-bit, 8 for 16-bit
            let cycles = if is_16bit { 8 } else { 12 };
            return InstructionResult::prefetch(cycles);
        }

        // Branch: PC = instruction_pc + 2 + displacement
        self.pc = target_pc_raw;

        // Cycles: 18 for both 8-bit and 16-bit
        InstructionResult::branch_taken(18)
    }

    /// DBcc - Test Condition, Decrement and Branch
    /// Opcode: 0101cccc11001rrr
    /// If condition false: Dn = Dn - 1; if Dn != -1, branch
    pub fn dbcc(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        condition: u8,
        reg: u8,
    ) -> InstructionResult {
        // Read 16-bit displacement using prefetch
        let displacement = prefetch.read_word(bus) as i16;

        // Test condition
        let condition_met = self.test_condition(condition);

        if condition_met {
            // Condition true: don't branch, don't decrement
            InstructionResult::prefetch(12)
        } else {
            // Condition false: decrement and check
            let value = (self.d[reg as usize] & 0xFFFF) as u16;
            let new_value = value.wrapping_sub(1);
            self.d[reg as usize] = (self.d[reg as usize] & 0xFFFF0000) | (new_value as u32);

            if new_value == 0xFFFF {
                // Counter expired: don't branch
                InstructionResult::prefetch(14)
            } else {
                // Counter not expired: branch
                let instruction_pc = self.pc;
                let target_pc_raw = instruction_pc
                    .wrapping_add(2)
                    .wrapping_add(displacement as i32 as u32);

                // Check for odd target address (Address Error)
                if target_pc_raw & 1 != 0 {
                    // For DBcc Address Error:
                    // - PC in stack frame is target_pc - 4
                    // - FC = 6 (supervisor program)
                    // - I/N = 1
                    self.address_error = Some(crate::cpu68k::AddressErrorInfo {
                        access_addr: target_pc_raw,
                        pc_at_error: target_pc_raw.wrapping_sub(4),
                        is_write: false,
                        function_code: 0x06,
                        in_bit: true,
                        sr_for_frame: None,
                        force_supervisor_stack: false,
                    });
                    // Address Error base cycles: 8 for DBcc (always 16-bit displacement)
                    return InstructionResult::prefetch(8);
                }

                // Branch: store full 32-bit PC
                self.pc = target_pc_raw;
                InstructionResult::branch_taken(10)
            }
        }
    }

    /// Scc - Set According to Condition
    /// Opcode: 0101cccc11mmmrrr
    /// If condition true: destination = 0xFF, else: destination = 0x00
    pub fn scc(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        condition: u8,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, Size::Byte);

        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, Size::Byte));
        }

        // Test condition
        let condition_met = self.test_condition(condition);

        // Set byte to 0xFF if true, 0x00 if false
        let value = if condition_met { 0xFF } else { 0x00 };
        // Scc is a write-only operation, so use write_with_adjust
        ea.write_with_adjust(self, bus, prefetch, Size::Byte, value, false);

        // Cycles: 4 if Dn and true, 4 if Dn and false, 8+ if memory
        let cycles = if mode == 0 {
            if condition_met {
                6
            } else {
                4
            }
        } else {
            8 + ea.cycles
        };

        InstructionResult::prefetch(cycles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_bra_8bit() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x1000; // instruction address (step() does NOT advance PC before calling branch)
        cpu.sr = 0x2700;

        // BRA with displacement +10
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.bra(&mut ram, &mut prefetch, 10);

        assert_eq!(cpu.pc, 0x100C); // 0x1000 + 2 + 10
        assert_eq!(result.cycles, 10);
        assert_eq!(result.pc_increment, 0); // PC already set
    }

    #[test]
    fn test_bra_16bit() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x1000;
        ram.write_word(0x1002, 0x0100); // 16-bit displacement = 0x0100 (at PC+2)

        // BRA with displacement = 0 (means 16-bit follows)
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.bra(&mut ram, &mut prefetch, 0);

        assert_eq!(cpu.pc, 0x1102); // 0x1000 + 2 + 0x100
        assert_eq!(result.cycles, 10);
    }

    #[test]
    fn test_bra_negative() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x1000;

        // BRA with displacement -10
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.bra(&mut ram, &mut prefetch, -10);

        assert_eq!(cpu.pc, 0x0FF8); // 0x1000 + 2 - 10
    }

    #[test]
    fn test_beq_taken() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::Z, true); // Zero flag set

        // BEQ with displacement +10 (should branch)
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.beq(&mut ram, &mut prefetch, 10);

        assert_eq!(cpu.pc, 0x100C);
        assert_eq!(result.cycles, 10);
    }

    #[test]
    fn test_beq_not_taken() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::Z, false); // Zero flag clear

        // BEQ with displacement +10 (should NOT branch)
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.beq(&mut ram, &mut prefetch, 10);

        assert_eq!(cpu.pc, 0x1000); // PC not changed by branch
        assert_eq!(result.cycles, 8);
        assert!(result.uses_prefetch); // step() will advance PC via prefetch consumed bytes
    }

    #[test]
    fn test_bne_taken() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::Z, false); // Zero flag clear

        // BNE with displacement +20 (should branch)
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.bne(&mut ram, &mut prefetch, 20);

        assert_eq!(cpu.pc, 0x1016); // 0x1000 + 2 + 20
        assert_eq!(result.cycles, 10);
    }

    #[test]
    fn test_bne_not_taken() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::Z, true); // Zero flag set

        // BNE with displacement +20 (should NOT branch)
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.bne(&mut ram, &mut prefetch, 20);

        assert_eq!(cpu.pc, 0x1000);
        assert_eq!(result.cycles, 8);
    }

    #[test]
    fn test_branch_conditions() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        // Test various condition codes

        // BCC (Carry Clear, C=0)
        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::C, false);
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.branch(&mut ram, &mut prefetch, 0x4, 10); // Condition 0x4 = CC
        assert_eq!(cpu.pc, 0x100C); // Branch taken

        // BCS (Carry Set, C=1)
        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::C, true);
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.branch(&mut ram, &mut prefetch, 0x5, 10); // Condition 0x5 = CS
        assert_eq!(cpu.pc, 0x100C); // Branch taken

        // BPL (Plus, N=0)
        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::N, false);
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.branch(&mut ram, &mut prefetch, 0xA, 10); // Condition 0xA = PL
        assert_eq!(cpu.pc, 0x100C); // Branch taken

        // BMI (Minus, N=1)
        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::N, true);
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.branch(&mut ram, &mut prefetch, 0xB, 10); // Condition 0xB = MI
        assert_eq!(cpu.pc, 0x100C); // Branch taken
    }

    #[test]
    fn test_bsr_8bit() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x1000;
        cpu.a[7] = 0x2000; // Stack pointer

        // BSR with displacement +100
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.bsr(&mut ram, &mut prefetch, 100);

        // Return address should be pushed
        assert_eq!(cpu.a[7], 0x1FFC); // SP - 4
        let high = ram.read_word(0x1FFC) as u32;
        let low = ram.read_word(0x1FFE) as u32;
        let return_addr = (high << 16) | low;
        assert_eq!(return_addr, 0x1002); // Address after BSR (instruction_pc + 2)

        // PC should be at destination
        assert_eq!(cpu.pc, 0x1066); // 0x1000 + 2 + 100
    }

    #[test]
    fn test_bsr_16bit() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x1000;
        cpu.a[7] = 0x2000;
        ram.write_word(0x1002, 0x0200); // 16-bit displacement (at PC+2)

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.bsr(&mut ram, &mut prefetch, 0); // 0 means 16-bit

        assert_eq!(cpu.a[7], 0x1FFC);
        assert_eq!(cpu.pc, 0x1202); // 0x1000 + 2 + 0x200
    }

    #[test]
    fn test_dbcc_condition_true() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 10; // Counter
        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::Z, true); // Condition = EQ (Z=1)
        ram.write_word(0x1002, 0xFFF0); // Displacement = -16 (at PC+2)

        // DBcc with condition 0x7 (EQ)
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.dbcc(&mut ram, &mut prefetch, 0x7, 0);

        // Condition true: don't branch, don't decrement
        assert_eq!(cpu.d[0] & 0xFFFF, 10); // Counter unchanged
        assert_eq!(cpu.pc, 0x1000); // PC unchanged (will be incremented by caller)
    }

    #[test]
    fn test_dbcc_condition_false_counter_not_expired() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 10; // Counter
        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::Z, true); // Condition = NE (Z=0) -> Set Z=1 to make NE FALSE
        ram.write_word(0x1002, 0xFFF0); // Displacement = -16 (at PC+2)

        // DBcc with condition 0x6 (NE: Z=0). Z=1 makes NE false
        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.dbcc(&mut ram, &mut prefetch, 0x6, 0);

        // Condition false: decrement and branch
        assert_eq!(cpu.d[0] & 0xFFFF, 9); // Decremented
        assert_eq!(cpu.pc, 0x0FF2); // Branched: 0x1000 + 2 - 16
    }

    #[test]
    fn test_dbcc_counter_expired() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0; // Counter = 0
        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::Z, true); // Make NE condition false
        ram.write_word(0x1002, 0xFFF0);

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.dbcc(&mut ram, &mut prefetch, 0x6, 0);

        // Counter becomes -1 (0xFFFF): don't branch
        assert_eq!(cpu.d[0] & 0xFFFF, 0xFFFF);
        assert_eq!(cpu.pc, 0x1000); // Not branched
    }

    #[test]
    fn test_scc_condition_true() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0x12345678;
        cpu.set_flag(super::super::Flag::Z, true); // EQ condition

        // Scc with condition 0x7 (EQ) on D0
        let mut prefetch = Prefetch::new(0x0);
        let result = cpu.scc(&mut ram, &mut prefetch, 0x7, 0, 0);

        // Condition true: set byte to 0xFF
        assert_eq!(cpu.d[0] & 0xFF, 0xFF);
        assert_eq!(cpu.d[0] & 0xFFFFFF00, 0x12345600); // Other bytes unchanged
    }

    #[test]
    fn test_scc_condition_false() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.d[0] = 0x12345678;
        cpu.set_flag(super::super::Flag::Z, false); // NE condition

        // Scc with condition 0x7 (EQ) on D0
        let mut prefetch = Prefetch::new(0x0);
        let result = cpu.scc(&mut ram, &mut prefetch, 0x7, 0, 0);

        // Condition false: set byte to 0x00
        assert_eq!(cpu.d[0] & 0xFF, 0x00);
    }

    #[test]
    fn test_bcc_all_conditions() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        // Test BCC (Carry Clear)
        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::C, false);
        let mut prefetch = Prefetch::new(0x1000);
        cpu.bcc(&mut ram, &mut prefetch, 10);
        assert_eq!(cpu.pc, 0x100C);

        // Test BCS (Carry Set)
        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::C, true);
        let mut prefetch = Prefetch::new(0x1000);
        cpu.bcs(&mut ram, &mut prefetch, 10);
        assert_eq!(cpu.pc, 0x100C);

        // Test BGE (Greater or Equal: N=V)
        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::N, false);
        cpu.set_flag(super::super::Flag::V, false);
        let mut prefetch = Prefetch::new(0x1000);
        cpu.bge(&mut ram, &mut prefetch, 10);
        assert_eq!(cpu.pc, 0x100C);

        // Test BLT (Less Than: N!=V)
        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::N, true);
        cpu.set_flag(super::super::Flag::V, false);
        let mut prefetch = Prefetch::new(0x1000);
        cpu.blt(&mut ram, &mut prefetch, 10);
        assert_eq!(cpu.pc, 0x100C);

        // Test BHI (Higher: C=0 AND Z=0)
        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::C, false);
        cpu.set_flag(super::super::Flag::Z, false);
        let mut prefetch = Prefetch::new(0x1000);
        cpu.bhi(&mut ram, &mut prefetch, 10);
        assert_eq!(cpu.pc, 0x100C);

        // Test BLS (Lower or Same: C=1 OR Z=1)
        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::C, true);
        let mut prefetch = Prefetch::new(0x1000);
        cpu.bls(&mut ram, &mut prefetch, 10);
        assert_eq!(cpu.pc, 0x100C);

        // Test BVC (Overflow Clear)
        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::V, false);
        let mut prefetch = Prefetch::new(0x1000);
        cpu.bvc(&mut ram, &mut prefetch, 10);
        assert_eq!(cpu.pc, 0x100C);

        // Test BVS (Overflow Set)
        cpu.pc = 0x1000;
        cpu.set_flag(super::super::Flag::V, true);
        let mut prefetch = Prefetch::new(0x1000);
        cpu.bvs(&mut ram, &mut prefetch, 10);
        assert_eq!(cpu.pc, 0x100C);
    }
}
