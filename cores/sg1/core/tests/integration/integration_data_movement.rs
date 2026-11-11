// Integration tests for the data movement instructions
// MOVE, MOVEA, MOVEQ, LEA, EXG, SWAP, CLR, NOP

use super::common::*;

// ========================================
// MOVE (every size)
// ========================================

#[test]
fn test_move_byte_dn_dn() {
    let rom = [0x12, 0x00]; // MOVE.B D0, D1

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;
    cpu.d[1] = 0xFFFFFFFF;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[1] & 0xFF, 0x78);
    assert_eq!(cpu.d[1] & 0xFFFFFF00, 0xFFFFFF00, "Upper bytes unchanged");
}

#[test]
fn test_move_word_dn_dn() {
    let rom = [0x32, 0x00]; // MOVE.W D0, D1

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;
    cpu.d[1] = 0xFFFFFFFF;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[1] & 0xFFFF, 0x5678);
    assert_eq!(cpu.d[1] & 0xFFFF0000, 0xFFFF0000, "Upper word unchanged");
}

#[test]
fn test_move_long_dn_dn() {
    let rom = [0x22, 0x00]; // MOVE.L D0, D1

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;
    cpu.d[1] = 0x00000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[1], 0x12345678);
}

#[test]
fn test_movea_long() {
    let rom = [0x22, 0x40]; // MOVEA.L D0, A1

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;
    cpu.a[1] = 0x00000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.a[1], 0x12345678);
}

#[test]
fn test_movea_word_sign_extend() {
    let rom = [0x30, 0x40]; // MOVEA.W D0, A0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12348000; // Negative word
    cpu.a[0] = 0x00000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.a[0], 0xFFFF8000, "Should sign-extend");
}

// ========================================
// MOVEQ
// ========================================

#[test]
fn test_moveq_positive() {
    let rom = [0x70, 0x12]; // MOVEQ #$12, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFFFFFFFF;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x00000012);
}

#[test]
fn test_moveq_negative() {
    let rom = [0x70, 0xFF]; // MOVEQ #-1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] as i32, -1, "Should sign-extend");
}

// ========================================
// LEA
// ========================================

#[test]
fn test_lea_address_indirect() {
    let rom = [0x41, 0xD1]; // LEA (A1), A0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[1] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.a[0], 0x12345678);
}

// ========================================
// EXG
// ========================================

#[test]
fn test_exg_data_registers() {
    let rom = [0xC1, 0x41]; // EXG D0, D1

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;
    cpu.d[1] = 0xAABBCCDD;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0xAABBCCDD);
    assert_eq!(cpu.d[1], 0x12345678);
}

#[test]
fn test_exg_address_registers() {
    let rom = [0xC1, 0x49]; // EXG A0, A1 (opmode=01001)

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[0] = 0x11111111;
    cpu.a[1] = 0x22222222;

    cpu.step(&mut bus);

    assert_eq!(cpu.a[0], 0x22222222);
    assert_eq!(cpu.a[1], 0x11111111);
}

#[test]
fn test_exg_data_address() {
    let rom = [0xC1, 0x89]; // EXG D0, A1

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;
    cpu.a[1] = 0xAABBCCDD;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0xAABBCCDD);
    assert_eq!(cpu.a[1], 0x12345678);
}

// ========================================
// SWAP
// ========================================

#[test]
fn test_swap() {
    let rom = [0x48, 0x40]; // SWAP D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x56781234);
}

// ========================================
// CLR
// ========================================

#[test]
fn test_clr_byte() {
    let rom = [0x42, 0x00]; // CLR.B D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x00);
    assert_eq!(cpu.d[0] & 0xFFFFFF00, 0x12345600);
}

#[test]
fn test_clr_word() {
    let rom = [0x42, 0x40]; // CLR.W D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x0000);
    assert_eq!(cpu.d[0] & 0xFFFF0000, 0x12340000);
}

#[test]
fn test_clr_long() {
    let rom = [0x42, 0x80]; // CLR.L D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x00000000);
}

// ========================================
// NOP
// ========================================

#[test]
fn test_nop() {
    let rom = [0x4E, 0x71]; // NOP

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x0002);
}
