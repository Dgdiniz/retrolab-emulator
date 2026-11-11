//! Z80 Exception and Interrupt Handling
//!
//! The Z80 has a simpler interrupt system than the 68000, with three interrupt modes:
//! - IM 0: Device places an instruction on the bus (usually RST n)
//! - IM 1: Jump to fixed address 0x0038
//! - IM 2: Vectored interrupt using I register as high byte
//! - NMI: Non-maskable interrupt, always jumps to 0x0066

use super::Z80;
use retro_core::cpu::Addressable;

/// Z80 interrupt modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InterruptMode {
    /// Mode 0: Device places instruction on bus (typically RST)
    Mode0 = 0,
    /// Mode 1: Jump to fixed address 0x0038
    Mode1 = 1,
    /// Mode 2: Vectored interrupt using I register
    Mode2 = 2,
}

/// Z80 interrupt types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptType {
    /// Maskable interrupt (can be disabled by DI, enabled by EI)
    Maskable,
    /// Non-maskable interrupt (always processed)
    NonMaskable,
}

/// Fixed interrupt addresses
pub mod interrupt_vectors {
    /// NMI vector address (Non-Maskable Interrupt)
    pub const NMI: u16 = 0x0066;

    /// Mode 1 interrupt vector address
    pub const MODE1: u16 = 0x0038;

    /// RST instruction addresses (used in Mode 0)
    pub const RST_00: u16 = 0x0000;
    pub const RST_08: u16 = 0x0008;
    pub const RST_10: u16 = 0x0010;
    pub const RST_18: u16 = 0x0018;
    pub const RST_20: u16 = 0x0020;
    pub const RST_28: u16 = 0x0028;
    pub const RST_30: u16 = 0x0030;
    pub const RST_38: u16 = 0x0038;
}

impl Z80 {
    /// Check for pending interrupts and process if enabled
    ///
    /// This should be called after each instruction execution.
    /// Returns true if an interrupt was processed, along with the number of cycles consumed.
    ///
    /// # Arguments
    /// * `bus` - Memory bus for reading interrupt vectors
    /// * `interrupt_pending` - Whether a maskable interrupt is pending
    /// * `nmi_pending` - Whether a non-maskable interrupt is pending
    ///
    /// # Returns
    /// (interrupt_processed, cycles_consumed)
    pub fn check_interrupts(
        &mut self,
        bus: &mut dyn Addressable,
        interrupt_pending: bool,
        nmi_pending: bool,
    ) -> (bool, u32) {
        // NMI has highest priority and cannot be masked
        if nmi_pending {
            let cycles = self.process_nmi(bus);
            return (true, cycles);
        }

        // Maskable interrupts can only be processed if IFF1 is set
        // AND we're not in the one-instruction delay after EI.
        // The EI instruction sets self.ei=1, which is cleared at the start of
        // the next step(). During that one instruction, interrupts are blocked.
        if interrupt_pending && self.iff1 && self.ei == 0 {
            let cycles = self.process_maskable_interrupt(bus);
            return (true, cycles);
        }

        // If halted but no interrupt, stay halted
        // If an interrupt occurs, unhalt but don't process until next check
        if self.halted && (interrupt_pending || nmi_pending) {
            self.halted = false;
        }

        (false, 0)
    }

    /// Process a Non-Maskable Interrupt (NMI)
    ///
    /// NMI always:
    /// 1. Copies IFF1 to IFF2 (preserving interrupt enable state)
    /// 2. Disables IFF1 (maskable interrupts)
    /// 3. Pushes PC onto stack
    /// 4. Jumps to 0x0066
    ///
    /// Returns the number of T-states consumed (11 cycles)
    pub fn process_nmi(&mut self, bus: &mut dyn Addressable) -> u32 {
        // If halted, unhalt
        if self.halted {
            self.halted = false;
        }

        // Copy IFF1 to IFF2 (this is how RETN knows to restore interrupts)
        self.iff2 = self.iff1;

        // Disable maskable interrupts
        self.iff1 = false;

        // Push PC onto stack (Z80 pushes high byte first, then low byte)
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp as u32, (self.pc >> 8) as u8); // High byte first
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp as u32, (self.pc & 0xFF) as u8); // Low byte second

        // Jump to NMI vector
        self.pc = interrupt_vectors::NMI;

        // NMI takes 11 T-states
        11
    }

    /// Process a maskable interrupt according to the current interrupt mode
    ///
    /// The behavior depends on the interrupt mode (IM 0, 1, or 2):
    /// - IM 0: Device places instruction on bus (typically RST n)
    /// - IM 1: Jump to 0x0038
    /// - IM 2: Vectored interrupt using I register
    ///
    /// Returns the number of T-states consumed
    pub fn process_maskable_interrupt(&mut self, bus: &mut dyn Addressable) -> u32 {
        // If halted, unhalt
        if self.halted {
            self.halted = false;
        }

        // Accepting the interrupt clears both IFF1 and IFF2, as DI does; the
        // handler sets them again with EI.
        self.iff1 = false;
        self.iff2 = false;

        // Push PC onto stack (Z80 pushes high byte first, then low byte)
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp as u32, (self.pc >> 8) as u8); // High byte first
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp as u32, (self.pc & 0xFF) as u8); // Low byte second

        // Process according to interrupt mode
        match self.im {
            0 => self.process_interrupt_mode0(bus),
            1 => self.process_interrupt_mode1(),
            2 => self.process_interrupt_mode2(bus),
            _ => {
                // Invalid mode, treat as Mode 1
                self.process_interrupt_mode1()
            }
        }
    }

    /// Process interrupt in Mode 0
    ///
    /// In Mode 0, the interrupting device places an instruction on the data bus.
    /// This is typically RST n, but can be any single-byte instruction.
    ///
    /// For simplicity and Genesis compatibility, we assume RST 38h (vector 0x0038),
    /// which is the same as Mode 1.
    ///
    /// Returns 13 T-states (2 for mode 0 instruction fetch + 11 for RST)
    fn process_interrupt_mode0(&mut self, _bus: &mut dyn Addressable) -> u32 {
        // In a real Z80, we would read the instruction from the data bus
        // For Genesis, we typically see RST 38h, which is same as Mode 1

        // Assume device provides RST 38h (opcode 0xFF)
        // RST already pushed PC, so just set PC to 0x0038
        self.pc = interrupt_vectors::MODE1;

        // Mode 0: 2 extra cycles for instruction fetch + 11 for RST = 13 total
        13
    }

    /// Process interrupt in Mode 1
    ///
    /// In Mode 1, the CPU always jumps to address 0x0038.
    /// This is the simplest and most commonly used mode.
    ///
    /// Returns 13 T-states
    fn process_interrupt_mode1(&mut self) -> u32 {
        // Jump to fixed vector
        self.pc = interrupt_vectors::MODE1;

        // Mode 1: 13 T-states total
        13
    }

    /// Process interrupt in Mode 2
    ///
    /// In Mode 2, the CPU uses a vectored interrupt system:
    /// 1. The I register contains the high byte of the vector table address
    /// 2. The interrupting device provides the low byte (typically even number)
    /// 3. The CPU reads a 16-bit address from the vector table
    /// 4. Jumps to that address
    ///
    /// For Genesis, the device typically provides 0x00, so vector table
    /// is at (I << 8 | 0x00).
    ///
    /// Returns 19 T-states
    fn process_interrupt_mode2(&mut self, bus: &mut dyn Addressable) -> u32 {
        // In a real Z80, the device provides the low byte
        // For Genesis/typical usage, assume device provides 0x00
        let vector_low: u8 = 0x00;

        // Construct vector table address
        let vector_table_addr = ((self.i as u16) << 8) | (vector_low as u16);

        // Read 16-bit interrupt vector from table (little endian)
        let vector_addr_low = bus.read(vector_table_addr as u32) as u16;
        let vector_addr_high = bus.read(vector_table_addr.wrapping_add(1) as u32) as u16;
        let interrupt_handler = (vector_addr_high << 8) | vector_addr_low;

        // Jump to interrupt handler
        self.pc = interrupt_handler;

        // Mode 2: 19 T-states total
        19
    }

    /// Set the interrupt mode
    ///
    /// Valid modes are 0, 1, and 2.
    /// Invalid modes are silently clamped to Mode 0.
    pub fn set_interrupt_mode(&mut self, mode: u8) {
        self.im = match mode {
            0..=2 => mode,
            _ => 0, // Invalid mode, default to 0
        };
    }

    /// Enable interrupts (EI instruction)
    ///
    /// Note: EI has a one-instruction delay before interrupts are actually enabled.
    /// This is handled by the instruction implementation, not here.
    pub fn enable_interrupts(&mut self) {
        self.iff1 = true;
        self.iff2 = true;
    }

    /// Disable interrupts (DI instruction)
    pub fn disable_interrupts(&mut self) {
        self.iff1 = false;
        self.iff2 = false;
    }

    /// Return from maskable interrupt (RETI instruction)
    ///
    /// RETI:
    /// 1. Pops PC from stack
    /// 2. Signals to peripheral that interrupt is acknowledged
    ///
    /// For most purposes, RETI behaves the same as RET, but peripherals
    /// may use it to daisy-chain interrupts.
    ///
    /// Returns 14 T-states
    pub fn reti(&mut self, bus: &mut dyn Addressable) -> u32 {
        // Pop PC from stack (little endian)
        let pc_low = bus.read(self.sp as u32) as u16;
        self.sp = self.sp.wrapping_add(1);
        let pc_high = bus.read(self.sp as u32) as u16;
        self.sp = self.sp.wrapping_add(1);

        self.pc = (pc_high << 8) | pc_low;

        // RETI signals daisy-chain peripherals (handled externally)
        // For now, we don't need to do anything special

        // RETI: 14 T-states
        14
    }

    /// Return from non-maskable interrupt (RETN instruction)
    ///
    /// RETN:
    /// 1. Pops PC from stack
    /// 2. Restores IFF1 from IFF2 (restoring interrupt enable state from before NMI)
    ///
    /// Returns 14 T-states
    pub fn retn(&mut self, bus: &mut dyn Addressable) -> u32 {
        // Pop PC from stack (little endian)
        let pc_low = bus.read(self.sp as u32) as u16;
        self.sp = self.sp.wrapping_add(1);
        let pc_high = bus.read(self.sp as u32) as u16;
        self.sp = self.sp.wrapping_add(1);

        self.pc = (pc_high << 8) | pc_low;

        // Restore IFF1 from IFF2
        self.iff1 = self.iff2;

        // RETN: 14 T-states
        14
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_nmi_saves_iff_state() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x1234;
        cpu.sp = 0x8000;
        cpu.iff1 = true;
        cpu.iff2 = false;

        cpu.process_nmi(&mut ram);

        assert!(!cpu.iff1, "IFF1 should be disabled");
        assert!(cpu.iff2, "IFF2 should preserve old IFF1");
        assert_eq!(cpu.pc, 0x0066, "PC should be at NMI vector");
    }

    #[test]
    fn test_nmi_pushes_pc() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x1234;
        cpu.sp = 0x8000;

        cpu.process_nmi(&mut ram);

        // Check stack (Z80 pushes high byte first, then low byte)
        // After push: (SP) = low byte, (SP+1) = high byte
        assert_eq!(ram.read(0x7FFE), 0x34, "Low byte at (SP)");
        assert_eq!(ram.read(0x7FFF), 0x12, "High byte at (SP+1)");
        assert_eq!(cpu.sp, 0x7FFE, "SP should be decremented by 2");
    }

    #[test]
    fn test_mode1_interrupt() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x5678;
        cpu.sp = 0x8000;
        cpu.iff1 = true;
        cpu.im = 1;

        cpu.process_maskable_interrupt(&mut ram);

        assert_eq!(cpu.pc, 0x0038, "PC should be at Mode 1 vector");
        assert!(!cpu.iff1, "Interrupts should be disabled");
        assert_eq!(ram.read(0x7FFE), 0x78, "Low byte at (SP)");
        assert_eq!(ram.read(0x7FFF), 0x56, "High byte at (SP+1)");
    }

    #[test]
    fn test_mode2_interrupt() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x5678;
        cpu.sp = 0x8000;
        cpu.iff1 = true;
        cpu.im = 2;
        cpu.i = 0x80; // Vector table at 0x8000

        // Setup interrupt vector at 0x8000 pointing to 0x1234
        ram.write(0x8000, 0x34); // Low byte
        ram.write(0x8001, 0x12); // High byte

        cpu.process_maskable_interrupt(&mut ram);

        assert_eq!(cpu.pc, 0x1234, "PC should be at vector address");
        assert!(!cpu.iff1, "Interrupts should be disabled");
    }

    #[test]
    fn test_retn_restores_iff() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        // Simulate NMI happened
        cpu.iff1 = false;
        cpu.iff2 = true; // Preserved from before NMI
        cpu.sp = 0x7FFE;
        cpu.pc = 0x0066;

        // Setup return address on stack
        ram.write(0x7FFE, 0x34); // Low byte
        ram.write(0x7FFF, 0x12); // High byte

        cpu.retn(&mut ram);

        assert_eq!(cpu.pc, 0x1234, "PC should be restored");
        assert!(cpu.iff1, "IFF1 should be restored from IFF2");
        assert_eq!(cpu.sp, 0x8000, "SP should be restored");
    }

    #[test]
    fn test_reti() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0x7FFE;
        cpu.pc = 0x0038;

        // Setup return address on stack
        ram.write(0x7FFE, 0x78); // Low byte
        ram.write(0x7FFF, 0x56); // High byte

        cpu.reti(&mut ram);

        assert_eq!(cpu.pc, 0x5678, "PC should be restored");
        assert_eq!(cpu.sp, 0x8000, "SP should be restored");
    }

    #[test]
    fn test_check_interrupts_maskable_disabled() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.iff1 = false; // Interrupts disabled
        cpu.im = 1;

        let (processed, _cycles) = cpu.check_interrupts(&mut ram, true, false);

        assert!(
            !processed,
            "Interrupt should not be processed when IFF1=false"
        );
    }

    #[test]
    fn test_check_interrupts_nmi_priority() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x1000;
        cpu.sp = 0x8000;
        cpu.iff1 = true;
        cpu.im = 1;

        // Both NMI and maskable interrupt pending
        let (processed, cycles) = cpu.check_interrupts(&mut ram, true, true);

        assert!(processed, "Interrupt should be processed");
        assert_eq!(cycles, 11, "NMI takes 11 cycles");
        assert_eq!(cpu.pc, 0x0066, "NMI has priority");
    }

    #[test]
    fn test_halted_cleared_on_interrupt() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.halted = true;
        cpu.iff1 = true;
        cpu.im = 1;
        cpu.sp = 0x8000;

        let (processed, _cycles) = cpu.check_interrupts(&mut ram, true, false);

        assert!(processed, "Interrupt should be processed");
        assert!(!cpu.halted, "HALT should be cleared");
    }

    #[test]
    fn test_set_interrupt_mode() {
        let mut cpu = Z80::new();

        cpu.set_interrupt_mode(0);
        assert_eq!(cpu.im, 0);

        cpu.set_interrupt_mode(1);
        assert_eq!(cpu.im, 1);

        cpu.set_interrupt_mode(2);
        assert_eq!(cpu.im, 2);

        // Invalid mode should default to 0
        cpu.set_interrupt_mode(3);
        assert_eq!(cpu.im, 0);
    }
}
