// Integration tests for the BCD operations
// ABCD, SBCD, NBCD

use super::common::*;
use sg1_core::cpu68k::instructions::Flag;

// ========================================
// ABCD - Add BCD with Extend
// ========================================

#[test]
fn test_abcd_no_extend() {
    let rom = [0xC1, 0x01]; // ABCD D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x09; // BCD 09
    cpu.d[1] = 0x01; // BCD 01
    cpu.set_flag(Flag::X, false);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x10, "BCD: 09 + 01 = 10");
}

#[test]
fn test_abcd_with_extend() {
    let rom = [0xC1, 0x01]; // ABCD D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x08;
    cpu.d[1] = 0x01;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x10, "BCD: 08 + 01 + X = 10");
}

#[test]
fn test_abcd_carry() {
    let rom = [0xC1, 0x01]; // ABCD D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x99;
    cpu.d[1] = 0x01;
    cpu.set_flag(Flag::X, false);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x00, "BCD: 99 + 01 = 00 (carry)");
    assert!(cpu.get_flag(Flag::C), "Carry should be set");
}

// ========================================
// SBCD - Subtract BCD with Extend
// ========================================

#[test]
fn test_sbcd_no_extend() {
    let rom = [0x81, 0x01]; // SBCD D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x15; // BCD 15
    cpu.d[1] = 0x05; // BCD 05
    cpu.set_flag(Flag::X, false);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x10, "BCD: 15 - 05 = 10");
}

#[test]
fn test_sbcd_with_extend() {
    let rom = [0x81, 0x01]; // SBCD D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x10;
    cpu.d[1] = 0x05;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x04, "BCD: 10 - 05 - X = 04");
}

#[test]
fn test_sbcd_borrow() {
    let rom = [0x81, 0x01]; // SBCD D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00;
    cpu.d[1] = 0x01;
    cpu.set_flag(Flag::X, false);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x99, "BCD: 00 - 01 = 99 (borrow)");
    assert!(cpu.get_flag(Flag::C), "Carry should be set");
}

// ========================================
// NBCD - Negate BCD with Extend
// ========================================

#[test]
fn test_nbcd_no_extend() {
    let rom = [0x48, 0x00]; // NBCD D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x05; // BCD 05
    cpu.set_flag(Flag::X, false);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x95, "BCD: -05 = 95");
}

#[test]
fn test_nbcd_with_extend() {
    let rom = [0x48, 0x00]; // NBCD D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x05;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x94, "BCD: -(05 + X) = 94");
}

#[test]
fn test_nbcd_zero() {
    let rom = [0x48, 0x00]; // NBCD D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00;
    cpu.set_flag(Flag::X, false);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x00, "BCD: -00 = 00");
}
