// Integration tests for the comparison instructions
// CMP, CMPA, CMPI, CMPM

use super::common::*;
use retro_core::cpu::Addressable;
use sg1_core::cpu68k::instructions::Flag;

// ========================================
// CMP
// ========================================

#[test]
fn test_cmp_byte_equal() {
    let rom = [0xB0, 0x01]; // CMP.B D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12;
    cpu.d[1] = 0x12;

    cpu.step(&mut bus);

    assert!(cpu.get_flag(Flag::Z), "Z flag should be set (equal)");
}

#[test]
fn test_cmp_word_less() {
    let rom = [0xB0, 0x41]; // CMP.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x1000;
    cpu.d[1] = 0x2000;

    cpu.step(&mut bus);

    assert!(cpu.get_flag(Flag::N), "N flag for negative result");
}

#[test]
fn test_cmp_long_greater() {
    let rom = [0xB0, 0x81]; // CMP.L D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x20000000;
    cpu.d[1] = 0x10000000;

    cpu.step(&mut bus);

    assert!(!cpu.get_flag(Flag::Z));
    assert!(!cpu.get_flag(Flag::N));
}

// ========================================
// CMPA
// ========================================

#[test]
fn test_cmpa_word() {
    let rom = [0xB0, 0xC0]; // CMPA.W D0, A0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[0] = 0x00001234;
    cpu.d[0] = 0x00001234;

    cpu.step(&mut bus);

    assert!(cpu.get_flag(Flag::Z));
}

#[test]
fn test_cmpa_long() {
    let rom = [0xB3, 0xC0]; // CMPA.L D0, A1

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[1] = 0x12345678;
    cpu.d[0] = 0x12345678;

    cpu.step(&mut bus);

    assert!(cpu.get_flag(Flag::Z));
}

// ========================================
// CMPI
// ========================================

#[test]
fn test_cmpi_byte() {
    let rom = [
        0x0C, 0x00, // CMPI.B #imm, D0
        0x00, 0x12,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12;

    cpu.step(&mut bus);

    assert!(cpu.get_flag(Flag::Z));
}

#[test]
fn test_cmpi_word() {
    let rom = [
        0x0C, 0x40, // CMPI.W #imm, D0
        0x12, 0x34,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00001234;

    cpu.step(&mut bus);

    assert!(cpu.get_flag(Flag::Z));
}

#[test]
fn test_cmpi_long() {
    let rom = [
        0x0C, 0x82, // CMPI.L #imm, D2
        0x12, 0x34, 0x56, 0x78,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[2] = 0x12345678;

    cpu.step(&mut bus);

    assert!(cpu.get_flag(Flag::Z));
}

// ========================================
// CMPM
// ========================================

#[test]
fn test_cmpm_byte() {
    let rom = [0xB1, 0x09]; // CMPM.B (A1)+, (A0)+

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[0] = 0x1000;
    cpu.a[1] = 0x2000;

    // Write test values
    bus.write(0x1000, 0x12);
    bus.write(0x2000, 0x12);

    cpu.step(&mut bus);

    assert!(cpu.get_flag(Flag::Z));
    assert_eq!(cpu.a[0], 0x1001, "A0 should increment");
    assert_eq!(cpu.a[1], 0x2001, "A1 should increment");
}

#[test]
fn test_cmpm_word() {
    let rom = [0xB1, 0x49]; // CMPM.W (A1)+, (A0)+

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[0] = 0x1000;
    cpu.a[1] = 0x2000;

    bus.write_word(0x1000, 0x1234);
    bus.write_word(0x2000, 0x1234);

    cpu.step(&mut bus);

    assert!(cpu.get_flag(Flag::Z));
    assert_eq!(cpu.a[0], 0x1002);
    assert_eq!(cpu.a[1], 0x2002);
}
