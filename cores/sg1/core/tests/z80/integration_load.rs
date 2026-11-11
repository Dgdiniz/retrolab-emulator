//! Z80 Load Instructions - Integration Tests
//! Tests all load instructions by calling step() externally

use super::common::*;

// ========================================
// LD r, n (Load immediate to register)
// ========================================

#[test]
fn test_ld_b_n_integration() {
    let rom = [0x06, 0x42]; // LD B, 0x42
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.step(&mut ram);

    assert_eq!(cpu.b, 0x42, "B should be 0x42");
    assert_eq!(cpu.pc, 0x0002, "PC should advance by 2");
}

#[test]
fn test_ld_c_n_integration() {
    let rom = [0x0E, 0xFF]; // LD C, 0xFF
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.step(&mut ram);

    assert_eq!(cpu.c, 0xFF);
    assert_eq!(cpu.pc, 0x0002);
}

#[test]
fn test_ld_a_n_integration() {
    let rom = [0x3E, 0x7F]; // LD A, 0x7F
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x7F);
    assert_eq!(cpu.pc, 0x0002);
}

// ========================================
// LD r, r' (Load register to register)
// ========================================

#[test]
fn test_ld_b_a_integration() {
    let rom = [0x47]; // LD B, A
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x42;
    cpu.b = 0x00;

    cpu.step(&mut ram);

    assert_eq!(cpu.b, 0x42, "B should copy from A");
    assert_eq!(cpu.pc, 0x0001);
}

#[test]
fn test_ld_a_b_integration() {
    let rom = [0x78]; // LD A, B
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.b = 0x99;
    cpu.a = 0x00;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x99, "A should copy from B");
}

#[test]
fn test_ld_chain() {
    // LD B, 10; LD C, B; LD A, C
    let rom = [
        0x06, 0x0A, // LD B, 10
        0x48, // LD C, B (not 0x49 which is LD C, C)
        0x79, // LD A, C
    ];
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert_eq!(cpu.b, 10);
    assert_eq!(cpu.c, 10);
    assert_eq!(cpu.a, 10, "Value should propagate through registers");
}

// ========================================
// LD (BC), A / LD A, (BC)
// ========================================

#[test]
fn test_ld_bc_indirect_a_integration() {
    let rom = [0x02]; // LD (BC), A
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_bc(0x2000);
    cpu.a = 0x42;

    cpu.step(&mut ram);

    assert_eq!(ram.read(0x2000), 0x42, "Memory at (BC) should contain A");
    assert_eq!(cpu.pc, 0x0001);
}

#[test]
fn test_ld_a_bc_indirect_integration() {
    let rom = [0x0A]; // LD A, (BC)
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_bc(0x2000);
    ram.write(0x2000, 0x99);
    cpu.a = 0x00;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x99, "A should load from memory at (BC)");
}

// ========================================
// LD (DE), A / LD A, (DE)
// ========================================

#[test]
fn test_ld_de_indirect_a_integration() {
    let rom = [0x12]; // LD (DE), A
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_de(0x3000);
    cpu.a = 0xAB;

    cpu.step(&mut ram);

    assert_eq!(ram.read(0x3000), 0xAB);
}

#[test]
fn test_ld_a_de_indirect_integration() {
    let rom = [0x1A]; // LD A, (DE)
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_de(0x3000);
    ram.write(0x3000, 0xCD);

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0xCD);
}

// ========================================
// LD (nn), A / LD A, (nn)
// ========================================

#[test]
fn test_ld_nn_indirect_a_integration() {
    let rom = [0x32, 0x00, 0x40]; // LD (0x4000), A
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x55;

    cpu.step(&mut ram);

    assert_eq!(ram.read(0x4000), 0x55);
    assert_eq!(cpu.pc, 0x0003);
}

#[test]
fn test_ld_a_nn_indirect_integration() {
    let rom = [0x3A, 0x00, 0x50]; // LD A, (0x5000)
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    ram.write(0x5000, 0x77);

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x77);
    assert_eq!(cpu.pc, 0x0003);
}

// ========================================
// LD dd, nn (Load 16-bit immediate)
// ========================================

#[test]
fn test_ld_bc_nn_integration() {
    let rom = [0x01, 0x34, 0x12]; // LD BC, 0x1234
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.step(&mut ram);

    assert_eq!(cpu.bc(), 0x1234);
    assert_eq!(cpu.pc, 0x0003);
}

#[test]
fn test_ld_de_nn_integration() {
    let rom = [0x11, 0x78, 0x56]; // LD DE, 0x5678
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.step(&mut ram);

    assert_eq!(cpu.de(), 0x5678);
}

#[test]
fn test_ld_hl_nn_integration() {
    let rom = [0x21, 0xCD, 0xAB]; // LD HL, 0xABCD
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.step(&mut ram);

    assert_eq!(cpu.hl(), 0xABCD);
}

#[test]
fn test_ld_sp_nn_integration() {
    let rom = [0x31, 0xFE, 0xFF]; // LD SP, 0xFFFE
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.step(&mut ram);

    assert_eq!(cpu.sp, 0xFFFE);
}

// ========================================
// LD (nn), HL / LD HL, (nn)
// ========================================

#[test]
fn test_ld_nn_indirect_hl_integration() {
    let rom = [0x22, 0x00, 0x60]; // LD (0x6000), HL
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_hl(0x1234);

    cpu.step(&mut ram);

    assert_eq!(ram.read(0x6000), 0x34, "Low byte");
    assert_eq!(ram.read(0x6001), 0x12, "High byte");
}

#[test]
fn test_ld_hl_nn_indirect_integration() {
    let rom = [0x2A, 0x00, 0x70]; // LD HL, (0x7000)
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    ram.write(0x7000, 0x78);
    ram.write(0x7001, 0x56);

    cpu.step(&mut ram);

    assert_eq!(cpu.hl(), 0x5678);
}

// ========================================
// LD SP, HL
// ========================================

#[test]
fn test_ld_sp_hl_integration() {
    let rom = [0xF9]; // LD SP, HL
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_hl(0xDEAD);
    cpu.sp = 0x0000;

    cpu.step(&mut ram);

    assert_eq!(cpu.sp, 0xDEAD);
    assert_eq!(cpu.pc, 0x0001);
}

// ========================================
// Complex load sequences
// ========================================

#[test]
fn test_load_sequence_memory() {
    // Load value to memory via multiple steps
    // LD HL, 0x8000; LD A, 0x42; LD (HL), A; LD B, (HL)
    let rom = [
        0x21, 0x00, 0x80, // LD HL, 0x8000
        0x3E, 0x42, // LD A, 0x42
              // TODO: add LD (HL), A and LD B, (HL)
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    execute_n_instructions(&mut cpu, &mut ram, 2);

    assert_eq!(cpu.hl(), 0x8000);
    assert_eq!(cpu.a, 0x42);
}
