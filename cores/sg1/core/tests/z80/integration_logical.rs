//! Z80 Logical Instructions - Integration Tests

use super::common::*;

// ========================================
// AND r
// ========================================

#[test]
fn test_and_b_integration() {
    let rom = [0xA0]; // AND B
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0b11110000;
    cpu.b = 0b10101010;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0b10100000);
    assert!(cpu.flag_h(), "H flag should be set for AND");
    assert!(!cpu.flag_n());
}

#[test]
fn test_and_zero_result() {
    let rom = [0xA1]; // AND C
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0b11110000;
    cpu.c = 0b00001111;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x00);
    assert!(cpu.flag_z(), "Z flag should be set");
    assert!(cpu.flag_pv(), "PV is parity - even parity (0)");
}

// ========================================
// AND n
// ========================================

#[test]
fn test_and_n_integration() {
    let rom = [0xE6, 0x0F]; // AND 0x0F
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0xFF;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x0F, "Mask lower nibble");
    assert_eq!(cpu.pc, 0x0002);
}

// ========================================
// OR r
// ========================================

#[test]
fn test_or_b_integration() {
    let rom = [0xB0]; // OR B
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0b11110000;
    cpu.b = 0b00001111;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0xFF);
    assert!(!cpu.flag_z());
    assert!(!cpu.flag_h(), "H flag cleared for OR");
}

#[test]
fn test_or_self_zero() {
    let rom = [0xB7]; // OR A (test A for zero)
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x00;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x00);
    assert!(cpu.flag_z());
}

// ========================================
// OR n
// ========================================

#[test]
fn test_or_n_integration() {
    let rom = [0xF6, 0xF0]; // OR 0xF0
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x0F;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0xFF);
}

// ========================================
// XOR r
// ========================================

#[test]
fn test_xor_b_integration() {
    let rom = [0xA8]; // XOR B
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0b11110000;
    cpu.b = 0b10101010;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0b01011010);
}

#[test]
fn test_xor_self_clear() {
    let rom = [0xAF]; // XOR A (clear A)
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0xFF;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x00, "XOR A, A always clears A");
    assert!(cpu.flag_z());
    assert!(!cpu.flag_c());
}

// ========================================
// XOR n
// ========================================

#[test]
fn test_xor_n_integration() {
    let rom = [0xEE, 0xFF]; // XOR 0xFF
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0xAA;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x55, "Inverts all bits");
}

// ========================================
// Parity flag tests
// ========================================

#[test]
fn test_parity_even() {
    let rom = [0xA0]; // AND B
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0xFF;
    cpu.b = 0x03; // Result = 0x03 (2 bits set - even parity)

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x03);
    assert!(cpu.flag_pv(), "PV set for even parity");
}

#[test]
fn test_parity_odd() {
    let rom = [0xA1]; // AND C
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0xFF;
    cpu.c = 0x07; // Result = 0x07 (3 bits set - odd parity)

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x07);
    assert!(!cpu.flag_pv(), "PV cleared for odd parity");
}

// ========================================
// Complex logical sequences
// ========================================

#[test]
fn test_bit_manipulation_sequence() {
    // Set bits, clear bits, toggle bits
    // LD A, 0x00; OR 0xF0; AND 0xFE; XOR 0x01
    let rom = [
        0x3E, 0x00, // LD A, 0x00
        0xF6, 0xF0, // OR 0xF0    -> A = 0xF0
        0xE6, 0xFE, // AND 0xFE   -> A = 0xF0 (clear bit 0)
        0xEE, 0x01, // XOR 0x01   -> A = 0xF1 (toggle bit 0)
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    execute_n_instructions(&mut cpu, &mut ram, 4);

    assert_eq!(cpu.a, 0xF1);
}

#[test]
fn test_masking_operation() {
    // Extract nibbles
    // LD A, 0xAB; LD B, A; AND 0x0F; LD C, A; LD A, B; AND 0xF0
    let rom = [
        0x3E, 0xAB, // LD A, 0xAB
        0x47, // LD B, A (save original)
        0xE6, 0x0F, // AND 0x0F (extract low nibble)
        0x4F, // LD C, A
        0x78, // LD A, B (restore original)
        0xE6, 0xF0, // AND 0xF0 (extract high nibble)
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    execute_n_instructions(&mut cpu, &mut ram, 6);

    assert_eq!(cpu.c, 0x0B, "Low nibble");
    assert_eq!(cpu.a, 0xA0, "High nibble");
}
