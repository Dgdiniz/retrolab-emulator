//! Z80 Arithmetic Instructions - Integration Tests

use super::common::*;

// ========================================
// INC r (Increment register)
// ========================================

#[test]
fn test_inc_b_integration() {
    let rom = [0x04]; // INC B
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.b = 0x10;

    cpu.step(&mut ram);

    assert_eq!(cpu.b, 0x11);
    assert!(!cpu.flag_z());
    assert!(!cpu.flag_s());
    assert!(!cpu.flag_n());
}

#[test]
fn test_inc_a_zero_flag() {
    let rom = [0x3C]; // INC A
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0xFF;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x00, "0xFF + 1 = 0x00");
    assert!(cpu.flag_z(), "Z flag should be set");
}

#[test]
fn test_inc_sequence() {
    // INC B three times
    let rom = [0x04, 0x04, 0x04]; // INC B x 3
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.b = 0x00;

    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert_eq!(cpu.b, 0x03);
}

// ========================================
// DEC r (Decrement register)
// ========================================

#[test]
fn test_dec_c_integration() {
    let rom = [0x0D]; // DEC C
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.c = 0x10;

    cpu.step(&mut ram);

    assert_eq!(cpu.c, 0x0F);
    assert!(cpu.flag_n(), "N flag should be set for subtraction");
}

#[test]
fn test_dec_zero_flag() {
    let rom = [0x3D]; // DEC A
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x01;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x00);
    assert!(cpu.flag_z());
}

#[test]
fn test_dec_underflow() {
    let rom = [0x05]; // DEC B
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.b = 0x00;

    cpu.step(&mut ram);

    assert_eq!(cpu.b, 0xFF, "0 - 1 = 0xFF (wraps)");
    assert!(cpu.flag_s(), "S flag should be set (negative)");
}

// ========================================
// ADD A, r
// ========================================

#[test]
fn test_add_a_b_integration() {
    let rom = [0x80]; // ADD A, B
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x10;
    cpu.b = 0x05;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x15);
    assert!(!cpu.flag_z());
    assert!(!cpu.flag_c());
}

#[test]
fn test_add_a_a_integration() {
    let rom = [0x87]; // ADD A, A (double A)
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x05;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x0A, "A should be doubled");
}

#[test]
fn test_add_carry_flag() {
    let rom = [0x81]; // ADD A, C
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0xFF;
    cpu.c = 0x01;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x00, "0xFF + 1 = 0x00");
    assert!(cpu.flag_z());
    assert!(cpu.flag_c(), "Carry flag should be set");
}

// ========================================
// ADD A, n
// ========================================

#[test]
fn test_add_a_n_integration() {
    let rom = [0xC6, 0x42]; // ADD A, 0x42
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x10;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x52);
    assert_eq!(cpu.pc, 0x0002);
}

// ========================================
// SUB r
// ========================================

#[test]
fn test_sub_b_integration() {
    let rom = [0x90]; // SUB B
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x10;
    cpu.b = 0x05;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x0B, "0x10 - 0x05 = 0x0B");
    assert!(cpu.flag_n(), "N flag should be set");
    assert!(!cpu.flag_c());
}

#[test]
fn test_sub_borrow_flag() {
    let rom = [0x92]; // SUB D
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x05;
    cpu.d = 0x10;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0xF5, "5 - 16 = -11 = 0xF5");
    assert!(cpu.flag_c(), "Borrow occurred");
    assert!(cpu.flag_s(), "Result is negative");
}

// ========================================
// SUB n
// ========================================

#[test]
fn test_sub_n_integration() {
    let rom = [0xD6, 0x20]; // SUB 0x20
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x50;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x30);
}

// ========================================
// CP r (Compare)
// ========================================

#[test]
fn test_cp_equal() {
    let rom = [0xB8]; // CP B
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x42;
    cpu.b = 0x42;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x42, "A should be unchanged");
    assert!(cpu.flag_z(), "Z flag should be set when equal");
}

#[test]
fn test_cp_less_than() {
    let rom = [0xB9]; // CP C
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x05;
    cpu.c = 0x10;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x05, "A unchanged");
    assert!(cpu.flag_c(), "Carry set when A < operand");
    assert!(!cpu.flag_z());
}

#[test]
fn test_cp_greater_than() {
    let rom = [0xBA]; // CP D
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x20;
    cpu.d = 0x10;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x20);
    assert!(!cpu.flag_c(), "No carry when A >= operand");
    assert!(!cpu.flag_z());
}

// ========================================
// CP n
// ========================================

#[test]
fn test_cp_n_integration() {
    let rom = [0xFE, 0x42]; // CP 0x42
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x42;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x42);
    assert!(cpu.flag_z());
}

// ========================================
// Complex arithmetic sequences
// ========================================

#[test]
fn test_arithmetic_expression() {
    // Calculate (5 + 3) - 2 = 6
    // LD A, 5; ADD A, 3; SUB 2
    let rom = [
        0x3E, 0x05, // LD A, 5
        0xC6, 0x03, // ADD A, 3
        0xD6, 0x02, // SUB 2
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert_eq!(cpu.a, 0x06);
}

#[test]
fn test_counter_loop_arithmetic() {
    // Decrement counter: LD B, 3, then DEC B three times in a row (no loop)
    let rom = [
        0x06, 0x03, // LD B, 3
        0x05, // DEC B
        0x05, // DEC B
        0x05, // DEC B
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    execute_n_instructions(&mut cpu, &mut ram, 4);

    assert_eq!(cpu.b, 0x00);
    assert!(cpu.flag_z(), "B should reach zero");
}
