//! Common utilities for Z80 integration tests
//!
//! Shared by several independent test binaries, each of which compiles this
//! module separately and uses a different subset of it, so per-binary
//! dead-code lints fire on items a sibling binary depends on.
#![allow(dead_code)]

pub use retro_core::cpu::Addressable;
pub use retro_core::memory::Ram;
pub use sg1_core::z80::Z80;

/// Setup a Z80 CPU with a ROM loaded at address 0x0000
pub fn setup_z80_with_rom(rom: &[u8]) -> (Z80, Ram) {
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Load ROM at address 0
    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.pc = 0x0000;
    (cpu, ram)
}

/// Helper to execute N instructions
pub fn execute_n_instructions(cpu: &mut Z80, bus: &mut dyn Addressable, n: usize) -> u32 {
    let mut total_cycles = 0;
    for _ in 0..n {
        total_cycles += cpu.step(bus);
    }
    total_cycles
}

/// Helper to execute until HALT or max instructions
pub fn run_until_halt(cpu: &mut Z80, bus: &mut dyn Addressable, max_instructions: u32) -> u32 {
    let mut total_cycles = 0;
    let mut instructions = 0;

    while !cpu.halted && instructions < max_instructions {
        total_cycles += cpu.step(bus);
        instructions += 1;
    }

    total_cycles
}

/// Assert flag state
pub fn assert_flag(cpu: &Z80, flag_name: &str, expected: bool) {
    let actual = match flag_name {
        "S" => cpu.flag_s(),
        "Z" => cpu.flag_z(),
        "H" => cpu.flag_h(),
        "PV" => cpu.flag_pv(),
        "N" => cpu.flag_n(),
        "C" => cpu.flag_c(),
        _ => panic!("Unknown flag: {}", flag_name),
    };

    assert_eq!(
        actual, expected,
        "Flag {} should be {}",
        flag_name, expected
    );
}
