//! Z80 Test Runner - a framework for running small Z80 test ROMs
//!
//! Shared by several independent test binaries, each of which compiles this
//! module separately and uses a different subset of it, so per-binary
//! dead-code lints fire on items a sibling binary depends on.
#![allow(dead_code)]

use retro_core::cpu::Addressable;
use retro_core::memory::Ram;
use sg1_core::memory::GenesisMemory;
use sg1_core::psg::Psg;
use sg1_core::ym2612::Ym2612;
use sg1_core::z80::Z80;
use sg1_core::z80_bus::Z80Bus;

/// Simple Z80 runner over plain RAM (for CPU-only tests)
pub struct Z80TestRunner {
    z80: Z80,
    ram: Ram,
    max_cycles: u64,
    cycle_count: u64,
}

/// Full runner: the Z80 on the Genesis Z80 bus (for tests with the YM2612, the PSG, etc)
pub struct Z80GenesisTestRunner {
    z80: Z80,
    memory: GenesisMemory,
    psg: Psg,
    ym2612: Ym2612,
    bank_register: u32,
    bank_bit_count: u8,
    max_cycles: u64,
    cycle_count: u64,
}

impl Z80TestRunner {
    /// Create a new runner over plain RAM (64KB)
    pub fn new() -> Self {
        Self {
            z80: Z80::new(),
            ram: Ram::new(0x10000), // 64KB RAM
            max_cycles: 1_000_000,
            cycle_count: 0,
        }
    }

    /// Load a ROM into Z80 memory (the whole 64KB space)
    pub fn load_rom(&mut self, rom: &[u8], org: u16) {
        for (i, &byte) in rom.iter().enumerate() {
            let addr = org.wrapping_add(i as u16) as u32;
            self.ram.write(addr, byte);
        }
        self.z80.set_pc(org);
    }

    /// Run until HALT or timeout
    pub fn run_until_halt(&mut self) -> Result<u64, String> {
        self.cycle_count = 0;

        while self.cycle_count < self.max_cycles {
            if self.z80.is_halted() {
                return Ok(self.cycle_count);
            }

            let cycles = self.z80.step(&mut self.ram);
            self.cycle_count += cycles as u64;
        }

        Err(format!("Timeout after {} cycles", self.max_cycles))
    }

    /// Get RAM reference
    pub fn ram(&self) -> &Ram {
        &self.ram
    }

    /// Get RAM mutable reference
    pub fn ram_mut(&mut self) -> &mut Ram {
        &mut self.ram
    }

    /// Run N instructions
    pub fn run_instructions(&mut self, count: u32) -> Result<u64, String> {
        self.cycle_count = 0;

        for _ in 0..count {
            if self.z80.is_halted() {
                break;
            }

            if self.cycle_count >= self.max_cycles {
                return Err(format!("Timeout after {} cycles", self.max_cycles));
            }

            let cycles = self.z80.step(&mut self.ram);
            self.cycle_count += cycles as u64;
        }

        Ok(self.cycle_count)
    }

    /// Run N cycles
    pub fn run_cycles(&mut self, cycles: u64) -> Result<(), String> {
        let target = self.cycle_count + cycles;

        while self.cycle_count < target && self.cycle_count < self.max_cycles {
            if self.z80.is_halted() {
                break;
            }

            let c = self.z80.step(&mut self.ram);
            self.cycle_count += c as u64;
        }

        if self.cycle_count >= self.max_cycles {
            Err(format!("Timeout after {} cycles", self.max_cycles))
        } else {
            Ok(())
        }
    }

    /// Set the timeout, in cycles
    pub fn set_max_cycles(&mut self, max: u64) {
        self.max_cycles = max;
    }

    /// Get Z80 reference
    pub fn z80(&self) -> &Z80 {
        &self.z80
    }

    /// Get Z80 mutable reference
    pub fn z80_mut(&mut self) -> &mut Z80 {
        &mut self.z80
    }

    /// Get cycle count
    pub fn cycle_count(&self) -> u64 {
        self.cycle_count
    }

    /// Reset cycle counter
    pub fn reset_cycle_counter(&mut self) {
        self.cycle_count = 0;
    }

    /// Check if halted
    pub fn is_halted(&self) -> bool {
        self.z80.is_halted()
    }

    /// Get PC
    pub fn get_pc(&self) -> u16 {
        self.z80.get_pc()
    }

    /// Get A register
    pub fn get_a(&self) -> u8 {
        self.z80.get_a()
    }

    /// Get B register
    pub fn get_b(&self) -> u8 {
        self.z80.get_b()
    }

    /// Get C register
    pub fn get_c(&self) -> u8 {
        self.z80.get_c()
    }

    /// Get HL register
    pub fn get_hl(&self) -> u16 {
        self.z80.get_hl()
    }

    /// Get flags
    pub fn get_f(&self) -> u8 {
        self.z80.get_f()
    }
}

impl Default for Z80TestRunner {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Z80GenesisTestRunner - for tests that need the YM2612, the PSG, etc
// ============================================================================

impl Z80GenesisTestRunner {
    /// Create a new runner with separate Genesis components
    pub fn new() -> Self {
        // Clock frequencies
        let psg_clock = 3_579_545; // ~3.58 MHz
        let ym_clock = 7_670_454; // ~7.67 MHz
        let sample_rate = 44100;

        Self {
            z80: Z80::new(),
            memory: GenesisMemory::new(),
            psg: Psg::new(psg_clock, sample_rate),
            ym2612: Ym2612::new(ym_clock, sample_rate),
            bank_register: 0,
            bank_bit_count: 0,
            max_cycles: 1_000_000,
            cycle_count: 0,
        }
    }

    /// Load a ROM into Z80 RAM (0x0000-0x1FFF in the Z80 address space)
    pub fn load_rom(&mut self, rom: &[u8], org: u16) {
        // Write the ROM straight into Z80 RAM
        for (i, &byte) in rom.iter().enumerate() {
            let addr = org as u32 + i as u32;
            if addr < 0x2000 {
                self.memory.z80_ram.write(addr, byte);
            }
        }
        self.z80.set_pc(org);
    }

    /// Run until HALT or timeout
    pub fn run_until_halt(&mut self) -> Result<u64, String> {
        self.cycle_count = 0;

        while self.cycle_count < self.max_cycles {
            if self.z80.is_halted() {
                return Ok(self.cycle_count);
            }

            // Create Z80Bus for this step
            let mut bus = Z80Bus::new(
                &mut self.psg,
                &mut self.ym2612,
                &mut self.bank_register,
                &mut self.bank_bit_count,
                None,
                &mut self.memory,
            );
            let cycles = self.z80.step(&mut bus);
            self.cycle_count += cycles as u64;
        }

        Err(format!("Timeout after {} cycles", self.max_cycles))
    }

    /// Run N instructions
    pub fn run_instructions(&mut self, count: u32) -> Result<u64, String> {
        self.cycle_count = 0;

        for _ in 0..count {
            if self.z80.is_halted() {
                break;
            }

            if self.cycle_count >= self.max_cycles {
                return Err(format!("Timeout after {} cycles", self.max_cycles));
            }

            let mut bus = Z80Bus::new(
                &mut self.psg,
                &mut self.ym2612,
                &mut self.bank_register,
                &mut self.bank_bit_count,
                None,
                &mut self.memory,
            );
            let cycles = self.z80.step(&mut bus);
            self.cycle_count += cycles as u64;
        }

        Ok(self.cycle_count)
    }

    /// Run N cycles
    pub fn run_cycles(&mut self, cycles: u64) -> Result<(), String> {
        let target = self.cycle_count + cycles;

        while self.cycle_count < target && self.cycle_count < self.max_cycles {
            if self.z80.is_halted() {
                break;
            }

            let mut bus = Z80Bus::new(
                &mut self.psg,
                &mut self.ym2612,
                &mut self.bank_register,
                &mut self.bank_bit_count,
                None,
                &mut self.memory,
            );
            let c = self.z80.step(&mut bus);
            self.cycle_count += c as u64;
        }

        if self.cycle_count >= self.max_cycles {
            Err(format!("Timeout after {} cycles", self.max_cycles))
        } else {
            Ok(())
        }
    }

    /// Set the timeout, in cycles
    pub fn set_max_cycles(&mut self, max: u64) {
        self.max_cycles = max;
    }

    /// Get Z80 reference
    pub fn z80(&self) -> &Z80 {
        &self.z80
    }

    /// Get Z80 mutable reference
    pub fn z80_mut(&mut self) -> &mut Z80 {
        &mut self.z80
    }

    /// Get PSG reference
    pub fn psg(&self) -> &Psg {
        &self.psg
    }

    /// Get YM2612 reference
    pub fn ym2612(&self) -> &Ym2612 {
        &self.ym2612
    }

    /// Get YM2612 mutable reference
    pub fn ym2612_mut(&mut self) -> &mut Ym2612 {
        &mut self.ym2612
    }

    /// Get cycle count
    pub fn cycle_count(&self) -> u64 {
        self.cycle_count
    }

    /// Reset cycle counter
    pub fn reset_cycle_counter(&mut self) {
        self.cycle_count = 0;
    }

    /// Check if halted
    pub fn is_halted(&self) -> bool {
        self.z80.is_halted()
    }

    /// Get PC
    pub fn get_pc(&self) -> u16 {
        self.z80.get_pc()
    }

    /// Get A register
    pub fn get_a(&self) -> u8 {
        self.z80.get_a()
    }

    /// Get B register
    pub fn get_b(&self) -> u8 {
        self.z80.get_b()
    }

    /// Get C register
    pub fn get_c(&self) -> u8 {
        self.z80.get_c()
    }

    /// Get HL register
    pub fn get_hl(&self) -> u16 {
        self.z80.get_hl()
    }

    /// Get flags
    pub fn get_f(&self) -> u8 {
        self.z80.get_f()
    }
}

impl Default for Z80GenesisTestRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::z80_rom_builder::Z80RomBuilder;

    #[test]
    fn test_runner_basic() {
        let rom = Z80RomBuilder::new(0x0000).ld_a(0x42).halt().build();

        let mut runner = Z80TestRunner::new();
        runner.load_rom(&rom, 0x0000);

        let cycles = runner.run_until_halt().expect("Should halt");
        assert_eq!(runner.get_a(), 0x42);
        assert!(cycles > 0);
    }

    #[test]
    fn test_runner_timeout() {
        let rom = Z80RomBuilder::new(0x0000)
            .infinite_loop() // JR -2 (infinite loop)
            .build();

        let mut runner = Z80TestRunner::new();
        runner.set_max_cycles(1000);
        runner.load_rom(&rom, 0x0000);

        let result = runner.run_until_halt();
        assert!(result.is_err());
    }

    #[test]
    fn test_runner_instruction_count() {
        let rom = Z80RomBuilder::new(0x0000).nop().nop().nop().halt().build();

        let mut runner = Z80TestRunner::new();
        runner.load_rom(&rom, 0x0000);

        runner
            .run_instructions(3)
            .expect("Should run 3 instructions");
        assert!(!runner.is_halted());

        runner.run_instructions(1).expect("Should run HALT");
        assert!(runner.is_halted());
    }
}
