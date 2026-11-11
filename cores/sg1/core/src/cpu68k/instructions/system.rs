//! System control instructions: RESET, STOP, RTE, TRAP, TRAPV, CHK, etc.

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

fn get_exception_cycles(ea: &EffectiveAddress, size: Size) -> u32 {
    let idx = ea_to_index(&ea.mode);
    #[rustfmt::skip]
    const MOVE_BYTE_WORD_CYCLES: [u32; 12] = [
        // Dn   An   (An) (An)+ -(An) d(An) d(An,ix) xxx.W xxx.L d(PC) d(PC,ix) #xxx
          10,  10,   10,   10,   12,   10,     12,    10,   10,   10,    12,     10
    ];
    #[rustfmt::skip]
    const MOVE_LONG_CYCLES: [u32; 12] = [
        // Dn   An   (An) (An)+ -(An) d(An) d(An,ix) xxx.W xxx.L d(PC) d(PC,ix) #xxx
          10,  10,   10,   10,   12,   10,     12,    10,   10,   10,    12,     10
    ];

    let table = match size {
        Size::Byte | Size::Word => &MOVE_BYTE_WORD_CYCLES,
        Size::Long => &MOVE_LONG_CYCLES,
    };

    table[idx]
}

impl M68k {
    /// RESET - Reset External Devices
    /// Opcode: 0100111001110000
    /// Privileged instruction
    pub fn reset_devices(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        // Check if in supervisor mode
        if (self.sr & 0x2000) == 0 {
            // User mode: generate privilege violation exception
            let cycles = self.privilege_violation_exception(bus);
            return InstructionResult::branch_taken(cycles); // PC set by exception handler
        }

        // In real hardware, this asserts RESET line to peripherals
        // In emulation, we don't need to do anything special

        InstructionResult::new(132, 2)
    }

    /// STOP - Load Status Register and Stop
    /// Opcode: 0100111001110010
    /// Privileged instruction
    pub fn stop(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
    ) -> InstructionResult {
        // Check if in supervisor mode
        if (self.sr & 0x2000) == 0 {
            // User mode: generate privilege violation exception
            let cycles = self.privilege_violation_exception(bus);
            return InstructionResult::branch_taken(cycles);
        }

        // Read immediate data (new SR value) from the word after the opcode
        let new_sr = prefetch.read_word(bus);

        // Valid SR bits: T(15), S(13), I2-I0(10-8), X(4), N(3), Z(2), V(1), C(0)
        const SR_MASK: u16 = 0xA71F;

        let was_supervisor = (self.sr & 0x2000) != 0;
        self.sr = new_sr & SR_MASK;
        let will_be_user = (self.sr & 0x2000) == 0;

        if was_supervisor && will_be_user {
            self.ssp = self.a[7];
            self.a[7] = self.usp;
        }

        self.stopped = true;

        InstructionResult::prefetch(4)
    }

    /// RTE - Return from Exception
    /// Opcode: 0100111001110011
    /// Privileged instruction
    pub fn rte(
        &mut self,
        bus: &mut dyn Addressable,
        _prefetch: &mut Prefetch,
    ) -> InstructionResult {
        // Check if in supervisor mode
        if (self.sr & 0x2000) == 0 {
            // User mode: generate privilege violation exception
            let cycles = self.privilege_violation_exception(bus);
            return InstructionResult::branch_taken(cycles); // PC set by exception handler
        }

        let sp = self.a[7];

        // Remember if we were in supervisor mode BEFORE updating SR
        let was_supervisor = (self.sr & 0x2000) != 0;

        // Pop SR from stack
        let new_sr_from_stack = bus.read_word(sp);

        // SR valid bits mask: T(15), S(13), I2-I0(10-8), X(4), N(3), Z(2), V(1), C(0)
        const SR_MASK: u16 = 0xA71F;

        // Apply SR mask
        let new_sr = new_sr_from_stack & SR_MASK;
        let will_be_user = (new_sr & 0x2000) == 0;

        // Pop PC from stack (Big Endian: high word, then low word)
        let pc_high = bus.read_word(sp.wrapping_add(2)) as u32;
        let pc_low = bus.read_word(sp.wrapping_add(4)) as u32;

        let raw_pc = (pc_high << 16) | pc_low;

        // Update A7 (stack pop happens regardless of Address Error)
        // RTE pops 1 word SR + 1 long PC => 6 bytes
        self.a[7] = sp.wrapping_add(6);

        // Check for Address Error (odd PC)
        if raw_pc & 1 != 0 {
            // Address Error during RTE:
            // - Stack pop (SP += 6) already happened (handled above)
            // - CPU must stay on supervisor stack (no USP/SSP swap), but the SR pushed in the
            //   exception frame is the SR restored from the stack (without forcing S=1).
            self.sr = new_sr | 0x2000; // CPU state remains supervisor for exception handling

            let fc = if (new_sr & 0x2000) != 0 {
                // Restored SR is supervisor => program space
                0x06
            } else {
                // Restored SR is user => program space (user)
                0x02
            };

            self.address_error = Some(crate::cpu68k::AddressErrorInfo {
                access_addr: raw_pc,
                // Tom Harte expects the stacked PC to be the faulting target PC - 4
                pc_at_error: raw_pc.wrapping_sub(4),
                is_write: false,
                function_code: fc,
                in_bit: true,
                sr_for_frame: Some(new_sr),
                force_supervisor_stack: true,
            });
            return InstructionResult::prefetch(10);
        }

        // No Address Error: update SR normally
        self.sr = new_sr;

        // If switching to user mode, swap stack pointers
        if was_supervisor && will_be_user {
            self.ssp = self.a[7];
            self.a[7] = self.usp;
        }

        // Set PC (full 32-bit; bus masks to 24 bits on access)
        self.pc = raw_pc;

        InstructionResult::branch_taken(20)
    }

    /// TRAP - Trap
    /// Opcode: 010011100100vvvv
    /// Generates exception vector 32-47
    pub fn trap(
        &mut self,
        bus: &mut dyn Addressable,
        _prefetch: &mut Prefetch,
        vector: u8,
    ) -> InstructionResult {
        // TRAP always generates an exception, saving PC and SR on stack
        // The saved PC points to the instruction AFTER the TRAP
        // (this is handled by incrementing PC before calling trap_exception)

        // Saved PC must point to the instruction AFTER TRAP (2-byte opcode).
        self.pc = self.pc.wrapping_add(2);

        // Generate TRAP exception
        let cycles = self.trap_exception(bus, vector);

        InstructionResult::branch_taken(cycles)
    }

    /// TRAPV - Trap on Overflow
    /// Opcode: 0100111001110110
    pub fn trapv(
        &mut self,
        bus: &mut dyn Addressable,
        _prefetch: &mut Prefetch,
    ) -> InstructionResult {
        // Check V flag
        if (self.sr & 0x0002) != 0 {
            // Overflow: generate TRAPV exception
            // Saved PC must point to the instruction AFTER TRAPV (2-byte opcode).
            self.pc = self.pc.wrapping_add(2);
            let cycles = self.trapv_exception(bus);
            InstructionResult::branch_taken(cycles)
        } else {
            // No overflow: continue
            InstructionResult::prefetch(4)
        }
    }

    /// CHK - Check Register Against Bounds
    /// Opcode: 0100rrr110mmmrrr
    /// If Dn < 0 or Dn > <ea>, generate exception
    pub fn chk(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        data_reg: u8,
        mode: u8,
        ea_reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, ea_reg, Size::Word);

        let upper_bound = ea.read(self, bus, prefetch, Size::Word) as i16;
        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, Size::Word));
        }

        // State-only correctness (Tom Harte): CHK clears V, Z and C regardless of outcome.
        // Do this after the EA read succeeds (no Address Error).
        self.set_flag(super::Flag::V, false);
        self.set_flag(super::Flag::Z, false);
        self.set_flag(super::Flag::C, false);

        // CHK has its own timing model (Tom Harte expectations):
        // compared to our generic EA cycle accounting, several modes pay +2 here
        // (extensions / indexing / immediate decoding overhead).
        //let mut chk_ea_cycles = ea.cycles;
        let chk_ea_cycles = match ea.mode {
            AddressingMode::DataDirect(_) => 0,
            AddressingMode::AddressIndirect(_) => 4,
            AddressingMode::AddressIndirectPostInc(_) => 4,
            AddressingMode::AddressIndirectPreDec(_) => 6,
            AddressingMode::AddressIndirectDisplacement(_, _) => 8,
            AddressingMode::AddressIndirectIndex(_, _, _, _) => 10,
            AddressingMode::AbsoluteShort(_) => 8,
            AddressingMode::AbsoluteLong(_) => 12,
            AddressingMode::PcDisplacement(_) => 8,
            AddressingMode::PcIndex(_, _, _) => 10,
            AddressingMode::Immediate(_) => 4,
            _ => 0,
        };

        let value = (self.d[data_reg as usize] & 0xFFFF) as i16;
        let above_bound = value > upper_bound;

        if value < 0 || above_bound {
            // N says which test failed: set for a negative value, clear for a
            // value above the bound alone. CHK clears V, Z and C as it traps.
            self.set_flag(super::Flag::N, value < 0);
            self.set_flag(super::Flag::V, false);
            self.set_flag(super::Flag::Z, false);
            self.set_flag(super::Flag::C, false);
            // Saved PC must point after this instruction (opcode + EA extension words).
            self.pc = self.pc.wrapping_add(2).wrapping_add(prefetch.consumed());
            let _ = self.chk_exception(bus);
            // The processor compares the value with the bound first: a value
            // above it traps in 38 clocks even when it is also negative, and
            // only a negative value within the bound takes 40.
            let base = if above_bound { 38 } else { 40 };
            return InstructionResult::branch_taken(base + chk_ea_cycles);
        }

        // Value is within bounds
        InstructionResult::prefetch(10 + chk_ea_cycles)
    }

    /// MOVE to SR - Move to Status Register
    /// Opcode: 01000110 11mmmrrr
    /// Privileged instruction
    pub fn move_to_sr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        // Check if in supervisor mode
        if (self.sr & 0x2000) == 0 {
            // User mode: generate privilege violation exception
            let cycles = self.privilege_violation_exception(bus);
            return InstructionResult::branch_taken(cycles); // PC set by exception handler
        }

        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, Size::Word);
        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, Size::Word));
        }

        let value = ea.read(self, bus, prefetch, Size::Word) as u16;
        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, Size::Word));
        }

        // Mask to defined SR bits (matches Tom Harte expectations used elsewhere)
        const SR_MASK: u16 = 0xA71F;
        let new_sr = value & SR_MASK;
        let was_supervisor = (self.sr & 0x2000) != 0;
        let will_be_user = (new_sr & 0x2000) == 0;

        self.sr = new_sr;

        // Supervisor -> User: swap stack pointers
        if was_supervisor && will_be_user {
            self.ssp = self.a[7];
            self.a[7] = self.usp;
        }

        InstructionResult::prefetch(12 + ea.cycles)
    }

    /// MOVE from SR - Move from Status Register
    /// Opcode: 0100000011mmmrrr
    pub fn move_from_sr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, Size::Word);
        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, Size::Word));
        }

        // Write-only operation: must use write_with_adjust (handles (An)+ and -(An))
        ea.write_with_adjust(self, bus, prefetch, Size::Word, self.sr as u32, false);
        if self.address_error.is_some() {
            // MOVEfromSR has instruction-specific Address Error semantics:
            // - For (An)+ destination: An is incremented even if the write faults.
            // - For the exception frame, tests expect the R/W bit to indicate "read" (bit4=1),
            //   so we override is_write=false for this instruction.
            match ea.mode {
                AddressingMode::AddressIndirectPostInc(reg) => {
                    let inc = 2;
                    self.a[reg as usize] = self.a[reg as usize].wrapping_add(inc);
                }
                AddressingMode::AddressIndirectPreDec(_) => {
                    // Keep the predecrement side effect; already handled inside write_with_adjust.
                }
                _ => {}
            }

            // MOVEfromSR with -(An) expects PC-at-error to point to the opcode word (not +2).
            // Our generic predecrement write-only alignment helper currently sets PC-at-error
            // with an extra +2; compensate here for this instruction.
            if matches!(ea.mode, AddressingMode::AddressIndirectPreDec(_)) {
                if let Some(err) = self.address_error.as_mut() {
                    err.pc_at_error = err.pc_at_error.wrapping_sub(2);
                }
            }

            if let Some(err) = self.address_error.as_mut() {
                err.is_write = false;
            }
            return InstructionResult::prefetch(get_exception_cycles(&ea, Size::Word));
        }

        let cycles = if mode == 0 { 6 } else { 8 + ea.cycles };
        InstructionResult::prefetch(cycles)
    }

    /// MOVE to CCR - Move to Condition Code Register
    /// Opcode: 01000100 11mmmrrr
    pub fn move_to_ccr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
        mode: u8,
        reg: u8,
    ) -> InstructionResult {
        let ea = decode::decode_ea_with_cycles(self, bus, prefetch, mode, reg, Size::Word);
        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, Size::Word));
        }

        let value = ea.read(self, bus, prefetch, Size::Word) as u16;
        if self.address_error.is_some() {
            return InstructionResult::prefetch(get_exception_cycles(&ea, Size::Word));
        }

        // CCR is only bits 0-4 (X, N, Z, V, C)
        let ccr = value & 0x001F;
        self.sr = (self.sr & 0xFF00) | ccr;

        InstructionResult::prefetch(12 + ea.cycles)
    }

    /// MOVE USP - Move User Stack Pointer
    /// Opcode: 0100111001100ddd (d=0: USP to An, d=1: An to USP)
    /// Privileged instruction
    pub fn move_usp(
        &mut self,
        bus: &mut dyn Addressable,
        addr_reg: u8,
        to_usp: bool,
    ) -> InstructionResult {
        // Check if in supervisor mode
        if (self.sr & 0x2000) == 0 {
            // User mode: generate privilege violation exception
            let cycles = self.privilege_violation_exception(bus);
            return InstructionResult::branch_taken(cycles); // PC set by exception handler
        }

        if to_usp {
            // An -> USP
            self.usp = self.a[addr_reg as usize];
        } else {
            // USP -> An
            self.a[addr_reg as usize] = self.usp;
        }

        InstructionResult::new(4, 2)
    }

    /// ANDI to SR - AND Immediate to Status Register
    /// Opcode: 0000001001111100
    /// Privileged instruction
    pub fn andi_to_sr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
    ) -> InstructionResult {
        // Check if in supervisor mode
        if (self.sr & 0x2000) == 0 {
            // User mode: generate privilege violation exception
            let cycles = self.privilege_violation_exception(bus);
            return InstructionResult::branch_taken(cycles); // PC set by exception handler
        }

        let immediate = prefetch.read_word(bus);

        // SR valid bits mask: T(15), S(13), I2-I0(10-8), X(4), N(3), Z(2), V(1), C(0)
        const SR_MASK: u16 = 0xA71F;

        // Check if switching from supervisor to user mode
        let was_supervisor = (self.sr & 0x2000) != 0;
        let new_sr = (self.sr & immediate) & SR_MASK;
        let will_be_user = (new_sr & 0x2000) == 0;

        self.sr = new_sr;

        // If switching to user mode, swap stack pointers
        if was_supervisor && will_be_user {
            self.ssp = self.a[7];
            self.a[7] = self.usp;
        }

        InstructionResult::prefetch(20)
    }

    /// EORI to SR - Exclusive OR Immediate to Status Register
    /// Opcode: 0000101001111100
    /// Privileged instruction
    pub fn eori_to_sr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
    ) -> InstructionResult {
        // Check if in supervisor mode
        if (self.sr & 0x2000) == 0 {
            // User mode: generate privilege violation exception
            let cycles = self.privilege_violation_exception(bus);
            return InstructionResult::branch_taken(cycles); // PC set by exception handler
        }

        let immediate = prefetch.read_word(bus);

        // SR valid bits mask: T(15), S(13), I2-I0(10-8), X(4), N(3), Z(2), V(1), C(0)
        const SR_MASK: u16 = 0xA71F;

        // Check if switching from supervisor to user mode
        let was_supervisor = (self.sr & 0x2000) != 0;
        let new_sr = (self.sr ^ immediate) & SR_MASK;
        let will_be_user = (new_sr & 0x2000) == 0;

        self.sr = new_sr;

        // If switching to user mode, swap stack pointers
        if was_supervisor && will_be_user {
            self.ssp = self.a[7];
            self.a[7] = self.usp;
        }

        InstructionResult::prefetch(20)
    }

    /// ORI to SR - OR Immediate to Status Register
    /// Opcode: 0000000001111100
    /// Privileged instruction
    pub fn ori_to_sr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
    ) -> InstructionResult {
        // Check if in supervisor mode
        if (self.sr & 0x2000) == 0 {
            // User mode: generate privilege violation exception
            let cycles = self.privilege_violation_exception(bus);
            return InstructionResult::branch_taken(cycles); // PC set by exception handler
        }

        let immediate = prefetch.read_word(bus);

        // SR valid bits mask: T(15), S(13), I2-I0(10-8), X(4), N(3), Z(2), V(1), C(0)
        const SR_MASK: u16 = 0xA71F;

        // Check if switching from supervisor to user mode (unlikely with ORI, but be safe)
        let was_supervisor = (self.sr & 0x2000) != 0;
        let new_sr = (self.sr | immediate) & SR_MASK;
        let will_be_user = (new_sr & 0x2000) == 0;

        self.sr = new_sr;

        // If switching to user mode, swap stack pointers
        if was_supervisor && will_be_user {
            self.ssp = self.a[7];
            self.a[7] = self.usp;
        }

        InstructionResult::prefetch(20)
    }

    /// ANDI to CCR - AND Immediate to Condition Code Register
    /// Opcode: 0000001000111100
    pub fn andi_to_ccr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
    ) -> InstructionResult {
        let immediate = prefetch.read_word(bus);

        // Only affect CCR (bits 0-4: X, N, Z, V, C)
        // ANDI clears bits where immediate has 0
        let ccr_mask = immediate | 0xFFE0; // Keep upper bits unchanged
        self.sr &= ccr_mask;

        InstructionResult::prefetch(20)
    }

    /// EORI to CCR - Exclusive OR Immediate to Condition Code Register
    /// Opcode: 0000101000111100
    pub fn eori_to_ccr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
    ) -> InstructionResult {
        let immediate = prefetch.read_word(bus);

        // Only affect CCR (bits 0-4: X, N, Z, V, C)
        let ccr_mask = immediate & 0x001F;
        self.sr ^= ccr_mask;

        InstructionResult::prefetch(20)
    }

    /// ORI to CCR - OR Immediate to Condition Code Register
    /// Opcode: 0000000000111100
    pub fn ori_to_ccr(
        &mut self,
        bus: &mut dyn Addressable,
        prefetch: &mut Prefetch,
    ) -> InstructionResult {
        let immediate = prefetch.read_word(bus);

        // Only affect CCR (bits 0-4: X, N, Z, V, C)
        let ccr_mask = immediate & 0x001F;
        self.sr |= ccr_mask;

        InstructionResult::prefetch(20)
    }

    /// ILLEGAL - Illegal Instruction
    /// Opcode: 0100101011111100 (0x4AFC)
    /// Generates illegal instruction exception
    pub fn illegal(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        // Generate illegal instruction exception
        // PC points to the illegal instruction
        let cycles = self.illegal_instruction_exception(bus);

        InstructionResult::branch_taken(cycles) // PC set by exception handler
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_reset_supervisor() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.sr = 0x2700; // Supervisor mode

        let result = cpu.reset_devices(&mut ram);

        assert_eq!(result.cycles, 132);
    }

    #[test]
    fn test_reset_user_mode() {
        use crate::bus::AddressBus;
        use crate::cartridge::composable::build_genesis_cartridge;
        use crate::cpu68k::make_test_rom_with_header;

        let mut cpu = M68k::new();
        let mut bus = AddressBus::new();

        // Setup privilege violation exception vector at 0x20: points to 0x1000
        let mut rom = make_test_rom_with_header(0x1100);
        rom[0x20] = 0x00;
        rom[0x21] = 0x00;
        rom[0x22] = 0x10;
        rom[0x23] = 0x00;

        let cartridge = build_genesis_cartridge(rom).expect("Failed to create test cartridge");
        bus.set_cartridge(cartridge);

        cpu.reset(&mut bus);

        cpu.sr = 0x0000; // User mode
        cpu.pc = 0x0400;
        cpu.a[7] = 0xFF1000; // USP
        cpu.ssp = 0xFF0000;

        let result = cpu.reset_devices(&mut bus);

        // Should generate privilege violation exception
        assert_eq!(result.cycles, 34); // Privilege violation exception cycles
        assert_eq!(cpu.pc, 0x1000); // PC should jump to exception handler
        assert_eq!(cpu.sr & 0x2000, 0x2000); // Should be in supervisor mode
    }

    #[test]
    fn chk_traps_in_38_clocks_above_the_bound_and_40_below_zero() {
        // (value, bound, clocks): CHK.W D1, D0 traps when D0 < 0 or D0 > D1.
        let cases: [(u32, u32, u32); 4] = [
            (0x0005, 0x0003, 38), // above a positive bound
            (0xFFFF, 0xFFFE, 38), // -1 above a bound of -2: negative, and above
            (0xFFFE, 0xFFFF, 40), // -2 within a bound of -1: negative only
            (0xFFFF, 0x0003, 40), // -1 within a bound of 3: negative only
        ];
        for (value, bound, clocks) in cases {
            let mut cpu = M68k::new();
            let mut ram = Ram::new(0x10000);
            cpu.sr = 0x2700;
            cpu.a[7] = 0x1000;
            cpu.d[0] = value;
            cpu.d[1] = bound;

            let mut prefetch = Prefetch::new(cpu.pc);
            let result = cpu.chk(&mut ram, &mut prefetch, 0, 0, 1);

            assert_eq!(
                result.cycles, clocks,
                "value {value:04X}, bound {bound:04X}"
            );
            assert_eq!(
                cpu.get_flag(super::super::Flag::N),
                (value as u16 as i16) < 0,
                "N for value {value:04X}"
            );
        }
    }

    #[test]
    fn chk_charges_a_pc_relative_bound_like_its_address_register_twin() {
        // CHK.W (d16,PC), D0 that does not trap: 10 clocks plus the 8 of
        // the displacement mode, as for (d16,An). The bound sits at the
        // extension word's address plus the displacement.
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);
        cpu.pc = 0x1000;
        ram.write_word(0x1002, 0x0010);
        ram.write_word(0x1012, 10);
        cpu.d[0] = 5;

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.chk(&mut ram, &mut prefetch, 0, 7, 2);

        assert_eq!(result.cycles, 18);
    }

    #[test]
    fn test_trapv_no_overflow() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.sr = 0x0000; // V flag clear

        let mut prefetch = Prefetch::new(cpu.pc);
        let result = cpu.trapv(&mut ram, &mut prefetch);

        assert_eq!(result.cycles, 4);
    }

    #[test]
    fn test_trapv_overflow() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.sr = 0x0002; // V flag set

        let mut prefetch = Prefetch::new(cpu.pc);
        let result = cpu.trapv(&mut ram, &mut prefetch);

        assert_eq!(result.cycles, 34); // Exception would be generated
    }

    #[test]
    fn test_move_from_sr() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.sr = 0x2704; // Some SR value
        cpu.d[0] = 0xFFFFFFFF;

        let mut prefetch = Prefetch::new(cpu.pc);
        let result = cpu.move_from_sr(&mut ram, &mut prefetch, 0, 0); // MOVE SR, D0

        assert_eq!(cpu.d[0] & 0xFFFF, 0x2704);
    }

    #[test]
    fn test_move_to_ccr() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.sr = 0x2700;
        cpu.d[0] = 0x001F; // All CCR flags set

        let mut prefetch = Prefetch::new(cpu.pc);
        let result = cpu.move_to_ccr(&mut ram, &mut prefetch, 0, 0); // MOVE D0, CCR

        assert_eq!(cpu.sr & 0x00FF, 0x001F); // CCR updated
        assert_eq!(cpu.sr & 0xFF00, 0x2700); // Upper byte unchanged
    }

    #[test]
    fn test_move_usp() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.sr = 0x2700; // Supervisor mode
        cpu.a[0] = 0x12345678;
        cpu.usp = 0xAABBCCDD;

        // USP -> A0
        let result = cpu.move_usp(&mut ram, 0, false);
        assert_eq!(cpu.a[0], 0xAABBCCDD);

        // A0 -> USP
        cpu.a[0] = 0x11223344;
        let result = cpu.move_usp(&mut ram, 0, true);
        assert_eq!(cpu.usp, 0x11223344);
    }

    #[test]
    fn test_andi_to_ccr() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.sr = 0x271F; // All CCR flags set
        cpu.pc = 0x1000;

        // Immediate value is at PC+2 (after opcode)
        ram.write_word(0x1002, 0x00F0); // Mask: keep upper nibble

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.andi_to_ccr(&mut ram, &mut prefetch);

        assert_eq!(cpu.sr & 0x00FF, 0x0010); // CCR masked
        assert_eq!(cpu.sr & 0xFF00, 0x2700); // Upper byte unchanged
    }

    #[test]
    fn test_ori_to_ccr() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        cpu.sr = 0x2700; // No CCR flags set
        cpu.pc = 0x1000;

        // Immediate value is at PC+2 (after opcode)
        ram.write_word(0x1002, 0x001F); // Set all CCR flags

        let mut prefetch = Prefetch::new(0x1000);
        let result = cpu.ori_to_ccr(&mut ram, &mut prefetch);

        assert_eq!(cpu.sr & 0x00FF, 0x001F); // CCR flags set
        assert_eq!(cpu.sr & 0xFF00, 0x2700); // Upper byte unchanged
    }

    #[test]
    fn test_illegal() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x10000);

        let result = cpu.illegal(&mut ram);

        // Should generate exception
        assert_eq!(result.cycles, 34);
    }
}
