// Integration tests for the shifts and rotates
// ASL, ASR, LSL, LSR, ROL, ROR, ROXL, ROXR

use super::common::*;
use sg1_core::cpu68k::instructions::Flag;

// ========================================
// ASL - Arithmetic Shift Left
// ========================================

#[test]
fn test_asl_byte() {
    let rom = [0xE3, 0x00]; // ASL.B #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x01;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x02);
}

#[test]
fn test_asl_word() {
    let rom = [0xE3, 0x40]; // ASL.W #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x0100;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x0200);
}

#[test]
fn test_asl_long() {
    let rom = [0xE3, 0x80]; // ASL.L #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x10000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x20000000);
}

// ========================================
// ASR - Arithmetic Shift Right
// ========================================

#[test]
fn test_asr_byte() {
    let rom = [0xE2, 0x00]; // ASR.B #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x80; // Negative

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0xC0, "Sign extend");
}

#[test]
fn test_asr_word() {
    let rom = [0xE2, 0x40]; // ASR.W #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x8000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0xC000);
}

#[test]
fn test_asr_long() {
    let rom = [0xE2, 0x80]; // ASR.L #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x80000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0xC0000000);
}

// ========================================
// LSL - Logical Shift Left
// ========================================

#[test]
fn test_lsl_byte() {
    let rom = [0xE3, 0x08]; // LSL.B #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x01;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x02);
}

#[test]
fn test_lsl_word() {
    let rom = [0xE3, 0x48]; // LSL.W #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x0001;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x0002);
}

#[test]
fn test_lsl_long() {
    let rom = [0xE3, 0x88]; // LSL.L #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000001;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x00000002);
}

// ========================================
// LSR - Logical Shift Right
// ========================================

#[test]
fn test_lsr_byte() {
    let rom = [0xE2, 0x08]; // LSR.B #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x80;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x40, "No sign extend");
}

#[test]
fn test_lsr_word() {
    let rom = [0xE2, 0x48]; // LSR.W #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x8000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x4000);
}

#[test]
fn test_lsr_long() {
    let rom = [0xE2, 0x88]; // LSR.L #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x80000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x40000000);
}

// ========================================
// ROL - Rotate Left
// ========================================

#[test]
fn test_rol_byte() {
    let rom = [0xE3, 0x18]; // ROL.B #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x81;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x03);
}

#[test]
fn test_rol_word() {
    let rom = [0xE3, 0x58]; // ROL.W #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x8001;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x0003);
}

#[test]
fn test_rol_long() {
    let rom = [0xE3, 0x98]; // ROL.L #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x80000001;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x00000003);
}

// ========================================
// ROR - Rotate Right
// ========================================

#[test]
fn test_ror_byte() {
    let rom = [0xE2, 0x18]; // ROR.B #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x03;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x81);
}

#[test]
fn test_ror_word() {
    let rom = [0xE2, 0x58]; // ROR.W #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x0003;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x8001);
}

#[test]
fn test_ror_long() {
    let rom = [0xE2, 0x98]; // ROR.L #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000003;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x80000001);
}

// ========================================
// ROXL - Rotate Left Through Extend
// ========================================

#[test]
fn test_roxl_byte() {
    let rom = [0xE3, 0x10]; // ROXL.B #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x80;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x01);
}

#[test]
fn test_roxl_word() {
    let rom = [0xE3, 0x50]; // ROXL.W #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x8000;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x0001);
}

#[test]
fn test_roxl_long() {
    let rom = [0xE3, 0x90]; // ROXL.L #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x80000000;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x00000001);
}

// ========================================
// ROXR - Rotate Right Through Extend
// ========================================

#[test]
fn test_roxr_byte() {
    let rom = [0xE2, 0x10]; // ROXR.B #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x01;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x80);
}

#[test]
fn test_roxr_word() {
    let rom = [0xE2, 0x50]; // ROXR.W #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x0001;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x8000);
}

#[test]
fn test_roxr_long() {
    let rom = [0xE2, 0x90]; // ROXR.L #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000001;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x80000000);
}
