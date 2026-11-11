// Integration tests for the bit operations
// BTST, BSET, BCLR, BCHG, TAS

use super::common::*;
use sg1_core::cpu68k::instructions::Flag;

// ========================================
// BTST - Bit Test
// ========================================

#[test]
fn test_btst_immediate_bit_set() {
    let rom = [
        0x08, 0x00, // BTST #imm, D0
        0x00, 0x07,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000080; // Bit 7 set

    cpu.step(&mut bus);

    assert!(!cpu.get_flag(Flag::Z), "Z should be clear (bit is set)");
}

#[test]
fn test_btst_immediate_bit_clear() {
    let rom = [
        0x08, 0x00, // BTST #imm, D0
        0x00, 0x05,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000000; // Bit 5 clear

    cpu.step(&mut bus);

    assert!(cpu.get_flag(Flag::Z), "Z should be set (bit is clear)");
}

#[test]
fn test_btst_register() {
    let rom = [0x01, 0x01]; // BTST D0, D1

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 3; // Test bit 3
    cpu.d[1] = 0x00000008; // Bit 3 set

    cpu.step(&mut bus);

    assert!(!cpu.get_flag(Flag::Z));
}

// ========================================
// BSET - Bit Set
// ========================================

#[test]
fn test_bset_immediate() {
    let rom = [
        0x08, 0xC0, // BSET #imm, D0
        0x00, 0x05,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x00000020, "Bit 5 should be set");
}

#[test]
fn test_bset_register() {
    let rom = [0x01, 0xC1]; // BSET D0, D1

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 5; // Bit number
    cpu.d[1] = 0x00000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[1], 0x00000020, "Bit 5 should be set");
}

// ========================================
// BCLR - Bit Clear
// ========================================

#[test]
fn test_bclr_immediate() {
    let rom = [
        0x08, 0x80, // BCLR #imm, D0
        0x00, 0x05,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000020; // Bit 5 set

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x00000000, "Bit 5 should be cleared");
}

#[test]
fn test_bclr_register() {
    let rom = [0x01, 0x81]; // BCLR D0, D1

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 5;
    cpu.d[1] = 0x00000020; // Bit 5 set

    cpu.step(&mut bus);

    assert_eq!(cpu.d[1], 0x00000000, "Bit 5 should be cleared");
}

// ========================================
// BCHG - Bit Change
// ========================================

#[test]
fn test_bchg_immediate_set_to_clear() {
    let rom = [
        0x08, 0x40, // BCHG #imm, D0
        0x00, 0x05,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000020; // Bit 5 set

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x00000000, "Bit 5 should be toggled to clear");
}

#[test]
fn test_bchg_immediate_clear_to_set() {
    let rom = [
        0x08, 0x40, // BCHG #imm, D0
        0x00, 0x05,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000000; // Bit 5 clear

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x00000020, "Bit 5 should be toggled to set");
}

#[test]
fn test_bchg_register() {
    let rom = [0x01, 0x41]; // BCHG D0, D1

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 5;
    cpu.d[1] = 0x00000020; // Bit 5 set

    cpu.step(&mut bus);

    assert_eq!(cpu.d[1], 0x00000000, "Bit 5 should be toggled");
}

// ========================================
// TAS - Test and Set
// ========================================

#[test]
fn test_tas() {
    let rom = [0x4A, 0xC0]; // TAS D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000012;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x92, "Bit 7 should be set");
    assert!(!cpu.get_flag(Flag::Z));
}
