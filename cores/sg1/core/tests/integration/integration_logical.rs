// Integration tests for the logical instructions
// AND, ANDI, OR, ORI, EOR, EORI, NOT, TST

use super::common::*;
use sg1_core::cpu68k::instructions::Flag;

// ========================================
// AND
// ========================================

#[test]
fn test_and_byte() {
    let rom = [0xC0, 0x01]; // AND.B D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFF;
    cpu.d[1] = 0x0F;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x0F);
}

#[test]
fn test_and_word() {
    let rom = [0xC0, 0x41]; // AND.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFFFF;
    cpu.d[1] = 0x1234;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x1234);
}

#[test]
fn test_and_long() {
    let rom = [0xC0, 0x81]; // AND.L D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFFFFFFFF;
    cpu.d[1] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x12345678);
}

// ========================================
// ANDI
// ========================================

#[test]
fn test_andi_byte() {
    let rom = [
        0x02, 0x00, // ANDI.B #imm, D0
        0x00, 0x0F,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFF;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x0F);
}

#[test]
fn test_andi_word() {
    let rom = [
        0x02, 0x40, // ANDI.W #imm, D0
        0x00, 0xFF,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x0078);
}

#[test]
fn test_andi_long() {
    let rom = [
        0x02, 0x80, // ANDI.L #imm, D0
        0x00, 0x00, 0xFF, 0xFF,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x00005678);
}

// ========================================
// OR
// ========================================

#[test]
fn test_or_byte() {
    let rom = [0x80, 0x01]; // OR.B D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xF0;
    cpu.d[1] = 0x0F;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0xFF);
}

#[test]
fn test_or_word() {
    let rom = [0x80, 0x41]; // OR.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFF00;
    cpu.d[1] = 0x00FF;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0xFFFF);
}

#[test]
fn test_or_long() {
    let rom = [0x80, 0x81]; // OR.L D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFF00FF00;
    cpu.d[1] = 0x00FF00FF;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0xFFFFFFFF);
}

// ========================================
// ORI
// ========================================

#[test]
fn test_ori_byte() {
    let rom = [
        0x00, 0x00, // ORI.B #imm, D0
        0x00, 0x12,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x12);
}

#[test]
fn test_ori_word() {
    let rom = [
        0x00, 0x40, // ORI.W #imm, D0
        0x12, 0x34,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x1234);
}

// ========================================
// EOR
// ========================================

#[test]
fn test_eor_byte() {
    let rom = [0xB1, 0x01]; // EOR.B D0, D1

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFF;
    cpu.d[1] = 0xF0;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[1] & 0xFF, 0x0F);
}

#[test]
fn test_eor_word() {
    let rom = [0xB1, 0x41]; // EOR.W D0, D1

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFFFF;
    cpu.d[1] = 0x1234;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[1] & 0xFFFF, 0xEDCB);
}

#[test]
fn test_eor_long() {
    let rom = [0xB1, 0x81]; // EOR.L D0, D1

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFF00FF00;
    cpu.d[1] = 0x0F0F0F0F;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[1], 0xF00FF00F);
}

// ========================================
// EORI
// ========================================

#[test]
fn test_eori_byte() {
    let rom = [
        0x0A, 0x00, // EORI.B #imm, D0
        0x00, 0xFF,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0xFF);
}

// ========================================
// NOT
// ========================================

#[test]
fn test_not_byte() {
    let rom = [0x46, 0x00]; // NOT.B D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0xED);
}

#[test]
fn test_not_word() {
    let rom = [0x46, 0x40]; // NOT.W D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x1234;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0xEDCB);
}

#[test]
fn test_not_long() {
    let rom = [0x46, 0x80]; // NOT.L D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0xEDCBA987);
}

// ========================================
// TST
// ========================================

#[test]
fn test_tst_byte_zero() {
    let rom = [0x4A, 0x00]; // TST.B D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00;

    cpu.step(&mut bus);

    assert!(cpu.get_flag(Flag::Z));
}

#[test]
fn test_tst_word_negative() {
    let rom = [0x4A, 0x40]; // TST.W D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x8000;

    cpu.step(&mut bus);

    assert!(cpu.get_flag(Flag::N));
    assert!(!cpu.get_flag(Flag::Z));
}

#[test]
fn test_tst_long_positive() {
    let rom = [0x4A, 0x80]; // TST.L D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;

    cpu.step(&mut bus);

    assert!(!cpu.get_flag(Flag::N));
    assert!(!cpu.get_flag(Flag::Z));
}
