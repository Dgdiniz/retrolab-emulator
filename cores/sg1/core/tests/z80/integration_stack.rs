//! Z80 Stack Instructions - Integration Tests

use super::common::*;

// ========================================
// PUSH qq
// ========================================

#[test]
fn test_push_bc_integration() {
    let rom = [0xC5]; // PUSH BC
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFE;
    cpu.set_bc(0x1234);

    cpu.step(&mut ram);

    assert_eq!(cpu.sp, 0xFFFC, "SP should decrement by 2");
    assert_eq!(ram.read(0xFFFC), 0x34, "Low byte on stack");
    assert_eq!(ram.read(0xFFFD), 0x12, "High byte on stack");
}

#[test]
fn test_push_de_integration() {
    let rom = [0xD5]; // PUSH DE
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFE;
    cpu.set_de(0x5678);

    cpu.step(&mut ram);

    assert_eq!(cpu.sp, 0xFFFC);
    assert_eq!(ram.read(0xFFFC), 0x78);
    assert_eq!(ram.read(0xFFFD), 0x56);
}

#[test]
fn test_push_hl_integration() {
    let rom = [0xE5]; // PUSH HL
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFE;
    cpu.set_hl(0xABCD);

    cpu.step(&mut ram);

    assert_eq!(cpu.sp, 0xFFFC);
    assert_eq!(ram.read(0xFFFC), 0xCD);
    assert_eq!(ram.read(0xFFFD), 0xAB);
}

#[test]
fn test_push_af_integration() {
    let rom = [0xF5]; // PUSH AF
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFE;
    cpu.a = 0x42;
    cpu.f = 0x80; // Some flags

    cpu.step(&mut ram);

    assert_eq!(cpu.sp, 0xFFFC);
    assert_eq!(ram.read(0xFFFD), 0x42, "A on stack (high byte)");
    assert_eq!(ram.read(0xFFFC), 0x80, "F on stack (low byte)");
}

// ========================================
// POP qq
// ========================================

#[test]
fn test_pop_bc_integration() {
    let rom = [0xC1]; // POP BC
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFC;
    ram.write(0xFFFC, 0x34); // Low byte
    ram.write(0xFFFD, 0x12); // High byte

    cpu.step(&mut ram);

    assert_eq!(cpu.bc(), 0x1234);
    assert_eq!(cpu.sp, 0xFFFE, "SP should increment by 2");
}

#[test]
fn test_pop_de_integration() {
    let rom = [0xD1]; // POP DE
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFC;
    ram.write(0xFFFC, 0x78);
    ram.write(0xFFFD, 0x56);

    cpu.step(&mut ram);

    assert_eq!(cpu.de(), 0x5678);
    assert_eq!(cpu.sp, 0xFFFE);
}

#[test]
fn test_pop_hl_integration() {
    let rom = [0xE1]; // POP HL
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFC;
    ram.write(0xFFFC, 0xCD);
    ram.write(0xFFFD, 0xAB);

    cpu.step(&mut ram);

    assert_eq!(cpu.hl(), 0xABCD);
}

#[test]
fn test_pop_af_integration() {
    let rom = [0xF1]; // POP AF
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFC;
    ram.write(0xFFFC, 0x80); // F (flags)
    ram.write(0xFFFD, 0x42); // A

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x42);
    assert_eq!(cpu.f, 0x80);
    assert_eq!(cpu.sp, 0xFFFE);
}

// ========================================
// PUSH and POP sequences
// ========================================

#[test]
fn test_push_pop_preserves_value() {
    // PUSH BC; POP DE (move BC to DE via stack)
    let rom = [
        0xC5, // PUSH BC
        0xD1, // POP DE
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFE;
    cpu.set_bc(0x1234);
    cpu.set_de(0x0000);

    execute_n_instructions(&mut cpu, &mut ram, 2);

    assert_eq!(cpu.de(), 0x1234, "Value transferred via stack");
    assert_eq!(cpu.sp, 0xFFFE, "SP should be restored");
}

#[test]
fn test_multiple_push_pop() {
    // Push multiple values, pop in reverse order
    // PUSH BC; PUSH DE; POP HL; POP AF
    let rom = [
        0xC5, // PUSH BC
        0xD5, // PUSH DE
        0xE1, // POP HL
        0xF1, // POP AF
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFE;
    cpu.set_bc(0x1111);
    cpu.set_de(0x2222);

    execute_n_instructions(&mut cpu, &mut ram, 4);

    // Stack is LIFO, so HL gets DE, AF gets BC
    assert_eq!(cpu.hl(), 0x2222, "HL = last pushed (DE)");
    assert_eq!(cpu.a, 0x11, "A = high byte of BC");
    assert_eq!(cpu.f, 0x11, "F = low byte of BC");
    assert_eq!(cpu.sp, 0xFFFE, "SP restored");
}

// ========================================
// Stack pointer management
// ========================================

#[test]
fn test_stack_underflow_safety() {
    // Multiple POPs should keep incrementing SP (0x0002 to 0x0006: no wrap)
    let rom = [0xC1, 0xC1]; // POP BC x 2
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0x0002;
    // Write stack data (don't overwrite ROM at 0x0000-0x0001!)
    ram.write(0x0002, 0x33);
    ram.write(0x0003, 0x44);
    ram.write(0x0004, 0x55);
    ram.write(0x0005, 0x66);

    execute_n_instructions(&mut cpu, &mut ram, 2);

    assert_eq!(cpu.sp, 0x0006, "SP increments by 2 per POP");
}

#[test]
fn test_stack_overflow_safety() {
    // Multiple PUSHes should keep decrementing SP
    // Place ROM at higher address to avoid stack collision
    let rom = [0xC5, 0xC5, 0xC5]; // PUSH BC x 3
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Load ROM at 0x1000 to avoid stack collision
    for (i, &byte) in rom.iter().enumerate() {
        ram.write(0x1000 + i as u32, byte);
    }

    cpu.pc = 0x1000;
    cpu.sp = 0x0006;
    cpu.set_bc(0xFFFF);

    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert_eq!(cpu.sp, 0x0000, "SP: 0x0006 - 6 = 0x0000");
}

// ========================================
// Complex stack operations
// ========================================

#[test]
fn test_save_restore_registers() {
    // Save all registers to stack and restore them (nothing changes them
    // in between):
    // PUSH AF; PUSH BC; PUSH DE; PUSH HL;
    // POP HL; POP DE; POP BC; POP AF

    let rom = [
        0xF5, // PUSH AF
        0xC5, // PUSH BC
        0xD5, // PUSH DE
        0xE5, // PUSH HL
        0xE1, // POP HL
        0xD1, // POP DE
        0xC1, // POP BC
        0xF1, // POP AF
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFF00;
    cpu.a = 0xAA;
    cpu.f = 0xFF;
    cpu.set_bc(0x1111);
    cpu.set_de(0x2222);
    cpu.set_hl(0x3333);

    // Push all
    execute_n_instructions(&mut cpu, &mut ram, 4);
    assert_eq!(cpu.sp, 0xFEF8, "SP decreased by 8");

    // Pop all (LIFO order)
    execute_n_instructions(&mut cpu, &mut ram, 4);

    assert_eq!(cpu.hl(), 0x3333, "HL restored");
    assert_eq!(cpu.de(), 0x2222, "DE restored");
    assert_eq!(cpu.bc(), 0x1111, "BC restored");
    assert_eq!(cpu.a, 0xAA, "A restored");
    assert_eq!(cpu.f, 0xFF, "F restored");
    assert_eq!(cpu.sp, 0xFF00, "SP fully restored");
}
