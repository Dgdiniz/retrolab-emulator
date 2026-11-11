//! M68000 Exception Handling
//!
//! The 68000 has a sophisticated exception system with a vector table at 0x000-0x3FF.
//! Exceptions can be triggered by hardware (interrupts, bus errors) or software (TRAP, illegal instructions).

use super::M68k;
use retro_core::cpu::Addressable;

/// Exception vector numbers (each vector is 4 bytes, starting at address vector_num * 4)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExceptionVector {
    /// Reset: Initial SSP (vector 0) and Initial PC (vector 1)
    ResetSSP = 0,
    ResetPC = 1,

    /// Bus Error (e.g., invalid memory access)
    BusError = 2,

    /// Address Error (e.g., word/long access at odd address)
    AddressError = 3,

    /// Illegal Instruction
    IllegalInstruction = 4,

    /// Integer Divide by Zero
    DivideByZero = 5,

    /// CHK Instruction
    Chk = 6,

    /// TRAPV Instruction
    Trapv = 7,

    /// Privilege Violation
    PrivilegeViolation = 8,

    /// Trace
    Trace = 9,

    /// Line 1010 Emulator (unimplemented instruction starting with 1010)
    Line1010Emulator = 10,

    /// Line 1111 Emulator (unimplemented instruction starting with 1111)
    Line1111Emulator = 11,

    // Vectors 12-23 are reserved
    /// Spurious Interrupt
    SpuriousInterrupt = 24,

    /// Level 1 Interrupt Auto-Vector
    InterruptLevel1 = 25,

    /// Level 2 Interrupt Auto-Vector
    InterruptLevel2 = 26,

    /// Level 3 Interrupt Auto-Vector
    InterruptLevel3 = 27,

    /// Level 4 Interrupt Auto-Vector (Genesis: HBLANK)
    InterruptLevel4 = 28,

    /// Level 5 Interrupt Auto-Vector
    InterruptLevel5 = 29,

    /// Level 6 Interrupt Auto-Vector (Genesis: VBLANK)
    InterruptLevel6 = 30,

    /// Level 7 Interrupt Auto-Vector (NMI)
    InterruptLevel7 = 31,
    // TRAP instructions use vectors 32-47 (TRAP #0 through TRAP #15)

    // Vectors 48-63 are reserved

    // Vectors 64-255 are user-defined interrupt vectors
}

impl ExceptionVector {
    /// Get the vector number for a TRAP instruction
    pub fn trap(trap_num: u8) -> u8 {
        32 + (trap_num & 0xF)
    }

    /// Get the address in the vector table for this exception
    pub fn address(self) -> u32 {
        (self as u32) * 4
    }
}

/// Priority levels for exceptions (higher number = higher priority)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExceptionPriority {
    /// Reset (highest priority)
    Reset = 0,

    /// Bus Error, Address Error
    BusAddressError = 1,

    /// Trace
    Trace = 2,

    /// Interrupt
    Interrupt = 3,

    /// Illegal, Privilege, etc.
    Instruction = 4,

    /// TRAP, TRAPV, CHK, Divide by Zero
    Software = 5,
}

impl M68k {
    /// Process an exception
    ///
    /// This function:
    /// 1. Saves the current PC and SR on the supervisor stack
    /// 2. Enters supervisor mode
    /// 3. Sets the interrupt mask (for interrupts)
    /// 4. Loads the new PC from the vector table
    /// 5. Clears the trace flag (for most exceptions)
    ///
    /// Returns the number of cycles consumed
    pub fn exception(&mut self, bus: &mut dyn Addressable, vector: u8) -> u32 {
        // Calculate vector address
        let vector_addr = (vector as u32) * 4;

        // Save current state
        let old_sr = self.sr;
        let old_pc = self.pc;

        // Switch to supervisor mode if not already
        let was_supervisor = (self.sr & 0x2000) != 0;
        if !was_supervisor {
            // Save USP, restore SSP
            self.usp = self.a[7];
            self.a[7] = self.ssp;
        }

        // Enter supervisor mode
        self.sr |= 0x2000;

        // Clear trace flag (will be restored by RTE)
        let _trace_enabled = (self.sr & 0x8000) != 0;
        self.sr &= !0x8000;

        // Push exception stack frame (6 bytes):
        //  - SR (word) at lowest address
        //  - PC (long) following (high word, then low word)
        self.a[7] = self.a[7].wrapping_sub(6);
        bus.write_word(self.a[7], old_sr);
        let pc_high = (old_pc >> 16) as u16;
        let pc_low = (old_pc & 0xFFFF) as u16;
        bus.write_word(self.a[7].wrapping_add(2), pc_high);
        bus.write_word(self.a[7].wrapping_add(4), pc_low);

        // Read new PC from vector table (Big Endian)
        let new_pc_high = bus.read_word(vector_addr) as u32;
        let new_pc_low = bus.read_word(vector_addr + 2) as u32;
        let new_pc = (new_pc_high << 16) | new_pc_low;

        // Set new PC
        self.pc = new_pc;

        // For interrupts, update interrupt mask in SR
        if (25..=31).contains(&vector) {
            let interrupt_level = vector - 24; // Level 1-7
                                               // Set interrupt mask (bits 8-10 of SR)
            self.sr = (self.sr & !0x0700) | ((interrupt_level as u16) << 8);
        }

        // Exception processing takes different cycles depending on type

        match vector {
            0..=1 => 40,   // Reset
            2 => 50,       // Bus Error
            3 => 50,       // Address Error
            4 => 34,       // Illegal Instruction
            5 => 38,       // Divide by Zero
            6 => 38,       // CHK
            7 => 34,       // TRAPV
            8 => 34,       // Privilege Violation
            9 => 34,       // Trace
            10 => 34,      // Line 1010 Emulator
            11 => 34,      // Line 1111 Emulator
            24 => 44,      // Spurious Interrupt
            25..=31 => 44, // Interrupts
            32..=47 => 34, // TRAP #n
            _ => 34,       // User/Uninitialized
        }
    }

    /// Check for pending interrupts and process if priority is high enough
    ///
    /// Returns true if an interrupt was processed
    pub fn check_interrupts(&mut self, bus: &mut dyn Addressable, interrupt_level: u8) -> bool {
        // Get current interrupt mask from SR (bits 8-10)
        let current_mask = ((self.sr >> 8) & 0x07) as u8;

        // Interrupt must be higher priority than current mask
        // Level 7 (NMI) always interrupts
        if interrupt_level > current_mask || interrupt_level == 7 {
            // Process interrupt
            let vector = 24 + interrupt_level; // Vectors 25-31 for levels 1-7
            self.exception(bus, vector);
            return true;
        }

        false
    }

    /// Trigger an illegal instruction exception
    pub fn illegal_instruction_exception(&mut self, bus: &mut dyn Addressable) -> u32 {
        self.exception(bus, ExceptionVector::IllegalInstruction as u8)
    }

    /// Trigger a privilege violation exception
    pub fn privilege_violation_exception(&mut self, bus: &mut dyn Addressable) -> u32 {
        self.exception(bus, ExceptionVector::PrivilegeViolation as u8)
    }

    /// Trigger a divide by zero exception
    pub fn divide_by_zero_exception(&mut self, bus: &mut dyn Addressable) -> u32 {
        self.exception(bus, ExceptionVector::DivideByZero as u8)
    }

    /// Trigger a CHK exception
    pub fn chk_exception(&mut self, bus: &mut dyn Addressable) -> u32 {
        self.exception(bus, ExceptionVector::Chk as u8)
    }

    /// Trigger a TRAPV exception
    pub fn trapv_exception(&mut self, bus: &mut dyn Addressable) -> u32 {
        self.exception(bus, ExceptionVector::Trapv as u8)
    }

    /// Trigger a TRAP #n exception
    pub fn trap_exception(&mut self, bus: &mut dyn Addressable, trap_num: u8) -> u32 {
        let vector = ExceptionVector::trap(trap_num);
        self.exception(bus, vector)
    }

    /// Trigger a trace exception (after executing one instruction in trace mode)
    pub fn trace_exception(&mut self, bus: &mut dyn Addressable) -> u32 {
        self.exception(bus, ExceptionVector::Trace as u8)
    }

    /// Trigger an address error exception with full stack frame.
    ///
    /// Address Error has a special 14-byte stack frame:
    /// - Function code/access type (2 bytes)
    /// - Access address (4 bytes)
    /// - Instruction register (2 bytes)
    /// - Status register (2 bytes)
    /// - Program counter (4 bytes)
    ///
    /// **Note**: Returns 0 as cycles are tracked by the bus automatically.
    /// Use `bus.cycles()` to get accurate cycle count.
    pub fn address_error_exception(&mut self, bus: &mut dyn Addressable) -> u32 {
        self.address_error_exception_full(bus, 0, 0, false);
        0 // Cycles tracked by bus
    }

    /// Trigger an address error exception with specific parameters.
    ///
    /// - `access_addr`: The address that caused the error
    /// - `instruction`: The instruction word being executed
    /// - `is_write`: True if this was a write access, false for read
    ///   Process Address Error exception with full stack frame.
    ///
    /// Address Error occurs when attempting to access a word/long at an odd address.
    /// The 68000 creates a special 14-byte stack frame containing:
    /// - PC at error (4 bytes)
    /// - SR (2 bytes)  
    /// - Instruction register (2 bytes)
    /// - Access address that caused the error (4 bytes)
    /// - Fault information word (2 bytes)
    ///
    /// **Cycle Timing**: All bus accesses (stack writes, vector reads) are tracked
    /// by the bus automatically. The caller should use `bus.cycles()` to get the
    /// total cycle count. This provides accurate timing that varies based on when
    /// the error occurred during instruction execution.
    pub fn address_error_exception_full(
        &mut self,
        bus: &mut dyn Addressable,
        access_addr: u32,
        instruction: u16,
        is_write: bool,
    ) {
        // Default: FC=5 (supervisor data), I/N=0 (instruction fetch)
        self.address_error_exception_with_fc(
            bus,
            access_addr,
            instruction,
            is_write,
            0x05,
            false,
            None,
            false,
        );
    }

    /// Address Error exception with explicit function code and I/N bit.
    /// Used by branch instructions which need FC=6 (supervisor program) and I/N=1.
    #[allow(clippy::too_many_arguments)]
    pub fn address_error_exception_with_fc(
        &mut self,
        bus: &mut dyn Addressable,
        access_addr: u32,
        instruction: u16,
        is_write: bool,
        function_code: u16,
        in_not_instruction: bool,
        sr_for_frame: Option<u16>,
        force_supervisor_stack: bool,
    ) {
        let vector_addr = (ExceptionVector::AddressError as u32) * 4;

        // Save current state
        let old_sr = sr_for_frame.unwrap_or(self.sr);
        let old_pc = self.pc;

        // Switch to supervisor mode if not already
        let was_supervisor = force_supervisor_stack || (self.sr & 0x2000) != 0;
        if !was_supervisor {
            // Save USP, restore SSP
            self.usp = self.a[7];
            self.a[7] = self.ssp;
        }

        // Enter supervisor mode
        self.sr |= 0x2000;

        // Clear trace flag
        self.sr &= !0x8000;

        // Build fault information word:
        // Bits 15-5: Upper bits of instruction register
        // Bit 4: R/W (0=write, 1=read)
        // Bit 3: I/N (0=instruction fetch, 1=not instruction fetch)
        // Bits 2-0: Function code
        let rw_bit: u16 = if is_write { 0 } else { 0x10 };
        let in_bit: u16 = if in_not_instruction { 0x08 } else { 0x00 };
        let fc: u16 = function_code & 0x07;
        let fault_info: u16 = (instruction & 0xFFE0) | rw_bit | in_bit | fc;

        // Push special stack frame (14 bytes total, pushed in reverse order):
        // Stack grows downward, so we push PC last (highest address) first
        // Each write_word costs 4 cycles (tracked by bus)

        // 1. PC (4 bytes) - at SP-4
        self.a[7] = self.a[7].wrapping_sub(4);
        bus.write_word(self.a[7], (old_pc >> 16) as u16);
        bus.write_word(self.a[7].wrapping_add(2), old_pc as u16);

        // 2. SR (2 bytes) - at SP-6
        self.a[7] = self.a[7].wrapping_sub(2);
        bus.write_word(self.a[7], old_sr);

        // 3. Instruction register (2 bytes) - at SP-8
        self.a[7] = self.a[7].wrapping_sub(2);
        bus.write_word(self.a[7], instruction);

        // 4. Access address (4 bytes) - at SP-12
        self.a[7] = self.a[7].wrapping_sub(4);
        bus.write_word(self.a[7], (access_addr >> 16) as u16);
        bus.write_word(self.a[7].wrapping_add(2), access_addr as u16);

        // 5. Fault information (2 bytes) - at SP-14
        self.a[7] = self.a[7].wrapping_sub(2);
        bus.write_word(self.a[7], fault_info);

        // Read new PC from vector table (2 reads = 8 cycles, tracked by bus)
        let new_pc_high = bus.read_word(vector_addr) as u32;
        let new_pc_low = bus.read_word(vector_addr + 2) as u32;
        self.pc = (new_pc_high << 16) | new_pc_low;

        // Total cycles for exception are automatically tracked:
        // - Stack frame: 7 write_word calls = 28 cycles
        // - Vector fetch: 2 read_word calls = 8 cycles
        // - Instruction execution until error: varies (already in bus.cycles())
        // Caller should use bus.cycles() to get the complete cycle count.
    }

    /// Trigger a bus error exception
    pub fn bus_error_exception(&mut self, bus: &mut dyn Addressable) -> u32 {
        self.exception(bus, ExceptionVector::BusError as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::AddressBus;
    use crate::cartridge::composable::build_genesis_cartridge;
    use crate::cpu68k::make_test_rom_with_header;

    #[test]
    fn test_exception_vector_addresses() {
        assert_eq!(ExceptionVector::ResetSSP.address(), 0x0000);
        assert_eq!(ExceptionVector::ResetPC.address(), 0x0004);
        assert_eq!(ExceptionVector::IllegalInstruction.address(), 0x0010);
        assert_eq!(ExceptionVector::InterruptLevel6.address(), 0x0078);
    }

    #[test]
    fn test_trap_vector() {
        assert_eq!(ExceptionVector::trap(0), 32);
        assert_eq!(ExceptionVector::trap(15), 47);
    }

    #[test]
    fn test_illegal_instruction_exception() {
        let mut cpu = M68k::new();
        let mut bus = AddressBus::new();

        // Setup: ROM with exception vector
        let mut rom = make_test_rom_with_header(0x1000);

        // Illegal instruction vector (vector 4) at 0x10: points to 0x00000500
        rom[0x10] = 0x00;
        rom[0x11] = 0x00;
        rom[0x12] = 0x05;
        rom[0x13] = 0x00;

        let cartridge = build_genesis_cartridge(rom).expect("Failed to create test cartridge");
        bus.set_cartridge(cartridge);

        cpu.reset(&mut bus);

        // Set PC to 0x400, SR to user mode
        cpu.pc = 0x0400;
        cpu.sr = 0x0000; // User mode
        cpu.a[7] = 0xFF1000; // USP
        cpu.ssp = 0xFF0000; // SSP

        let cycles = cpu.illegal_instruction_exception(&mut bus);

        // Verify state
        assert_eq!(cpu.pc, 0x00000500, "PC should jump to exception vector");
        assert_eq!(cpu.sr & 0x2000, 0x2000, "Should be in supervisor mode");

        // Verify stack: SR at SP+0, PC high at SP+2, PC low at SP+4
        // (standard 68000 group-1/2 exception frame).
        assert_eq!(cpu.a[7], 0xFF0000 - 6, "SP should be decremented by 6");

        let saved_sr = bus.read_word(cpu.a[7]);
        assert_eq!(saved_sr, 0x0000, "Old SR should be saved");

        let saved_pc_high = bus.read_word(cpu.a[7] + 2) as u32;
        let saved_pc_low = bus.read_word(cpu.a[7] + 4) as u32;
        let saved_pc = (saved_pc_high << 16) | saved_pc_low;
        assert_eq!(saved_pc, 0x00000400, "Old PC should be saved");

        assert_eq!(cycles, 34, "Illegal instruction takes 34 cycles");
    }

    #[test]
    fn test_interrupt_level_6() {
        let mut cpu = M68k::new();
        let mut bus = AddressBus::new();

        // Setup: ROM with interrupt vector
        let mut rom = make_test_rom_with_header(0x1000);

        // Level 6 interrupt vector (vector 30) at 0x78: points to 0x00000600
        rom[0x78] = 0x00;
        rom[0x79] = 0x00;
        rom[0x7A] = 0x06;
        rom[0x7B] = 0x00;

        let cartridge = build_genesis_cartridge(rom).expect("Failed to create test cartridge");
        bus.set_cartridge(cartridge);

        cpu.reset(&mut bus);

        // Set PC, SR to supervisor mode with interrupt mask 0
        cpu.pc = 0x0400;
        cpu.sr = 0x2000; // Supervisor mode, interrupt mask = 0
        cpu.a[7] = 0xFF1000; // SSP

        let success = cpu.check_interrupts(&mut bus, 6);

        assert!(success, "Level 6 interrupt should be processed");
        assert_eq!(cpu.pc, 0x00000600, "PC should jump to interrupt vector");

        // Check interrupt mask is set to 6
        let int_mask = (cpu.sr >> 8) & 0x07;
        assert_eq!(int_mask, 6, "Interrupt mask should be set to 6");
    }

    #[test]
    fn test_interrupt_blocked_by_mask() {
        let mut cpu = M68k::new();
        let mut bus = AddressBus::new();

        let rom = make_test_rom_with_header(0x1000);
        let cartridge = build_genesis_cartridge(rom).expect("Failed to create test cartridge");
        bus.set_cartridge(cartridge);

        cpu.reset(&mut bus);

        // Set interrupt mask to 6
        cpu.sr = 0x2600; // Supervisor mode, interrupt mask = 6
        cpu.pc = 0x0400;

        let success = cpu.check_interrupts(&mut bus, 4);

        assert!(!success, "Level 4 interrupt should be blocked by mask 6");
        assert_eq!(cpu.pc, 0x0400, "PC should not change");
    }

    #[test]
    fn test_nmi_always_interrupts() {
        let mut cpu = M68k::new();
        let mut bus = AddressBus::new();

        let mut rom = make_test_rom_with_header(0x1000);

        // Level 7 interrupt vector (vector 31) at 0x7C: points to 0x00000700
        rom[0x7C] = 0x00;
        rom[0x7D] = 0x00;
        rom[0x7E] = 0x07;
        rom[0x7F] = 0x00;

        let cartridge = build_genesis_cartridge(rom).expect("Failed to create test cartridge");
        bus.set_cartridge(cartridge);

        cpu.reset(&mut bus);

        // Set interrupt mask to 7 (should still allow level 7 NMI)
        cpu.sr = 0x2700; // Supervisor mode, interrupt mask = 7
        cpu.pc = 0x0400;
        cpu.a[7] = 0xFF1000;

        let success = cpu.check_interrupts(&mut bus, 7);

        assert!(success, "Level 7 (NMI) should always interrupt");
        assert_eq!(cpu.pc, 0x00000700, "PC should jump to NMI vector");
    }

    #[test]
    fn test_trap_exception() {
        let mut cpu = M68k::new();
        let mut bus = AddressBus::new();

        let mut rom = make_test_rom_with_header(0x1000);

        // TRAP #0 vector (vector 32) at 0x80: points to 0x00001000
        rom[0x80] = 0x00;
        rom[0x81] = 0x00;
        rom[0x82] = 0x10;
        rom[0x83] = 0x00;

        let cartridge = build_genesis_cartridge(rom).expect("Failed to create test cartridge");
        bus.set_cartridge(cartridge);

        cpu.reset(&mut bus);

        cpu.pc = 0x0400;
        cpu.sr = 0x0000; // User mode
        cpu.a[7] = 0xFF1000; // USP
        cpu.ssp = 0xFF0000;

        let cycles = cpu.trap_exception(&mut bus, 0);

        assert_eq!(cpu.pc, 0x00001000, "PC should jump to TRAP #0 vector");
        assert_eq!(cycles, 34, "TRAP takes 34 cycles");
    }

    #[test]
    fn test_privilege_violation_exception() {
        let mut cpu = M68k::new();
        let mut bus = AddressBus::new();

        let mut rom = make_test_rom_with_header(0x1000);

        // Privilege violation vector (vector 8) at 0x20: points to 0x00000800
        rom[0x20] = 0x00;
        rom[0x21] = 0x00;
        rom[0x22] = 0x08;
        rom[0x23] = 0x00;

        let cartridge = build_genesis_cartridge(rom).expect("Failed to create test cartridge");
        bus.set_cartridge(cartridge);

        cpu.reset(&mut bus);

        cpu.pc = 0x0400;
        cpu.sr = 0x0000; // User mode
        cpu.a[7] = 0xFF1000; // USP
        cpu.ssp = 0xFF0000;

        let _cycles = cpu.privilege_violation_exception(&mut bus);

        assert_eq!(
            cpu.pc, 0x00000800,
            "PC should jump to privilege violation vector"
        );
        assert_eq!(cpu.sr & 0x2000, 0x2000, "Should be in supervisor mode");
    }

    #[test]
    fn test_divide_by_zero_exception() {
        let mut cpu = M68k::new();
        let mut bus = AddressBus::new();

        let mut rom = make_test_rom_with_header(0x1000);

        // Divide by zero vector (vector 5) at 0x14: points to 0x00000A00
        rom[0x14] = 0x00;
        rom[0x15] = 0x00;
        rom[0x16] = 0x0A;
        rom[0x17] = 0x00;

        let cartridge = build_genesis_cartridge(rom).expect("Failed to create test cartridge");
        bus.set_cartridge(cartridge);

        cpu.reset(&mut bus);

        cpu.pc = 0x0400;
        cpu.sr = 0x2000; // Supervisor mode
        cpu.a[7] = 0xFF1000;

        let cycles = cpu.divide_by_zero_exception(&mut bus);

        assert_eq!(
            cpu.pc, 0x00000A00,
            "PC should jump to divide by zero vector"
        );
        assert_eq!(cycles, 38, "Divide by zero takes 38 cycles");
    }
}
