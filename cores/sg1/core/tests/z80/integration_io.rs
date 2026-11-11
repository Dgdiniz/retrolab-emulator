//! Integration tests for Z80 I/O instructions

use retro_core::cpu::Addressable;
use retro_core::memory::Ram;
use sg1_core::z80::Z80;

/// Helper: Execute a mini ROM and return CPU state
fn execute_rom(rom: &[u8]) -> Z80 {
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Load ROM at address 0
    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    // Execute until HALT (0x76)
    let mut cycles = 0;
    while cycles < 10000 {
        // Safety limit
        let opcode = ram.read(cpu.pc as u32);
        if opcode == 0x76 {
            // HALT
            break;
        }

        let result_cycles = cpu.step(&mut ram);
        cycles += result_cycles;
    }

    cpu
}

// ============================================================================
// Basic I/O Tests
// ============================================================================

#[test]
fn test_in_a_n_integration() {
    // Mini ROM: IN A, (n) followed by OUT (n), A
    let rom = vec![
        0xDB, 0x42, // IN A, (0x42) - reads port 0x42 into A
        0x3E, 0x11, // LD A, 0x11   - overwrite A
        0xD3, 0x80, // OUT (0x80), A - output A to port 0x80
        0x76, // HALT
    ];

    let cpu = execute_rom(&rom);

    // A should be 0x11 after LD
    assert_eq!(cpu.a, 0x11);
}

#[test]
fn test_out_n_a_integration() {
    // Mini ROM: Load A and output to port
    let rom = vec![
        0x3E, 0x55, // LD A, 0x55
        0xD3, 0x42, // OUT (0x42), A
        0x76, // HALT
    ];

    let cpu = execute_rom(&rom);

    assert_eq!(cpu.a, 0x55);
}

// ============================================================================
// Extended I/O Tests (ED prefix)
// ============================================================================

#[test]
fn test_in_r_c_integration() {
    // Mini ROM: Load C with port, then IN various registers
    let rom = vec![
        0x0E, 0x80, // LD C, 0x80
        0xED, 0x40, // IN B, (C) - B = 0xFF (floating bus)
        0xED, 0x48, // IN C, (C) - C = 0xFF
        0xED, 0x50, // IN D, (C) - D = 0xFF
        0xED, 0x58, // IN E, (C) - E = 0xFF
        0x76, // HALT
    ];

    let cpu = execute_rom(&rom);

    // All should be 0xFF (floating bus)
    assert_eq!(cpu.b, 0xFF);
    assert_eq!(cpu.c, 0xFF);
    assert_eq!(cpu.d, 0xFF);
    assert_eq!(cpu.e, 0xFF);

    // Flags from last IN
    assert!(cpu.flag_s()); // Sign (0xFF is negative)
    assert!(!cpu.flag_z()); // Not zero
    assert!(cpu.flag_pv()); // Parity (0xFF has even parity)
}

#[test]
fn test_in_sets_flags() {
    // Mini ROM: Test flag setting
    let rom = vec![
        0x0E, 0x00, // LD C, 0x00
        0xED, 0x78, // IN A, (C) - A = 0xFF, flags set
        0x76, // HALT
    ];

    let cpu = execute_rom(&rom);

    assert_eq!(cpu.a, 0xFF);
    assert!(cpu.flag_s()); // Sign flag
    assert!(!cpu.flag_z()); // Not zero
    assert!(!cpu.flag_h()); // H cleared
    assert!(cpu.flag_pv()); // Parity
    assert!(!cpu.flag_n()); // N cleared
}

#[test]
fn test_out_c_r_integration() {
    // Mini ROM: OUT various registers to port
    let rom = vec![
        0x0E, 0x80, // LD C, 0x80
        0x06, 0x11, // LD B, 0x11
        0xED, 0x41, // OUT (C), B
        0x16, 0x22, // LD D, 0x22
        0xED, 0x51, // OUT (C), D
        0x3E, 0x33, // LD A, 0x33
        0xED, 0x79, // OUT (C), A
        0x76, // HALT
    ];

    let cpu = execute_rom(&rom);

    // Registers should be unchanged
    assert_eq!(cpu.c, 0x80);
    assert_eq!(cpu.b, 0x11);
    assert_eq!(cpu.d, 0x22);
    assert_eq!(cpu.a, 0x33);
}

#[test]
fn test_in_c_flags_only_undocumented() {
    // Mini ROM: Test undocumented IN (C) with no destination
    let rom = vec![
        0x0E, 0x42, // LD C, 0x42
        0x06, 0x11, // LD B, 0x11
        0xED, 0x70, // IN (C) - flags only, B unchanged
        0x76, // HALT
    ];

    let cpu = execute_rom(&rom);

    // B should be unchanged
    assert_eq!(cpu.b, 0x11);

    // Flags should be set
    assert!(cpu.flag_s()); // 0xFF is negative
    assert!(!cpu.flag_z());
    assert!(cpu.flag_pv());
}

#[test]
fn test_out_c_zero_undocumented() {
    // Mini ROM: Test undocumented OUT (C), 0
    let rom = vec![
        0x0E, 0x80, // LD C, 0x80
        0xED, 0x71, // OUT (C), 0 - undocumented
        0x76, // HALT
    ];

    let cpu = execute_rom(&rom);

    assert_eq!(cpu.c, 0x80);
}

#[test]
fn test_io_preserves_carry() {
    // Mini ROM: Test that IN preserves carry flag
    let rom = vec![
        0x37, // SCF - set carry flag
        0x0E, 0x00, // LD C, 0x00
        0xED, 0x78, // IN A, (C)
        0x76, // HALT
    ];

    let cpu = execute_rom(&rom);

    assert!(cpu.flag_c()); // Carry should be preserved
}

#[test]
fn test_mixed_io_operations() {
    // Mini ROM: Mix of basic and extended I/O
    let rom = vec![
        0x3E, 0x42, // LD A, 0x42
        0xD3, 0x80, // OUT (0x80), A - basic OUT
        0xDB, 0x81, // IN A, (0x81)  - basic IN
        0x0E, 0x90, // LD C, 0x90
        0xED, 0x79, // OUT (C), A    - extended OUT
        0xED, 0x78, // IN A, (C)     - extended IN
        0x76, // HALT
    ];

    let cpu = execute_rom(&rom);

    // A should be 0xFF from last IN
    assert_eq!(cpu.a, 0xFF);
    assert_eq!(cpu.c, 0x90);
}
