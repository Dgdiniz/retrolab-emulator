// Integration tests for the arithmetic instructions
// ADD, ADDI, ADDQ, ADDA, ADDX, SUB, SUBI, SUBQ, SUBA, SUBX, NEG, NEGX

use super::common::*;
use sg1_core::cpu68k::instructions::Flag;

// ========================================
// ADD
// ========================================

#[test]
fn test_add_byte() {
    let rom = [0xD0, 0x01]; // ADD.B D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x10;
    cpu.d[1] = 0x20;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x30);
}

#[test]
fn test_add_word() {
    let rom = [0xD0, 0x41]; // ADD.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x1000;
    cpu.d[1] = 0x0200;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x1200);
}

#[test]
fn test_add_long() {
    let rom = [0xD0, 0x81]; // ADD.L D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x10000000;
    cpu.d[1] = 0x01000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x11000000);
}

// ========================================
// ADDI
// ========================================

#[test]
fn test_addi_byte() {
    let rom = [
        0x06, 0x00, // ADDI.B #imm, D0
        0x00, 0x12,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x12);
}

#[test]
fn test_addi_word() {
    let rom = [
        0x06, 0x40, // ADDI.W #imm, D0
        0x12, 0x34,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00001000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x2234);
}

#[test]
fn test_addi_long() {
    let rom = [
        0x06, 0x80, // ADDI.L #imm, D0
        0x12, 0x34, 0x56, 0x78,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x10000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x22345678);
}

// ========================================
// ADDQ
// ========================================

#[test]
fn test_addq_byte() {
    let rom = [0x52, 0x00]; // ADDQ.B #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x10;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x11);
}

#[test]
fn test_addq_word() {
    let rom = [0x5C, 0x40]; // ADDQ.W #6, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x1000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x1006);
}

#[test]
fn test_addq_long() {
    let rom = [0x50, 0x80]; // ADDQ.L #8, D0 (data=0 means 8)

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00001000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x00001008);
}

// ========================================
// ADDA
// ========================================

#[test]
fn test_adda_word() {
    let rom = [0xD2, 0xC0]; // ADDA.W D0, A1

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[1] = 0x00001000;
    cpu.d[0] = 0x0100;

    cpu.step(&mut bus);

    assert_eq!(cpu.a[1], 0x00001100);
}

// TODO: enable this test
// #[test]
// fn test_adda_long() {
//     let rom = [0xD3, 0xC0]; // ADDA.L D0, A1
//     let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
//     cpu.pc = 0x0000;
//     cpu.a[1] = 0x10000000;
//     cpu.d[0] = 0x01000000;
//     cpu.step(&mut bus);
//     assert_eq!(cpu.a[1], 0x11000000);
// }

// ========================================
// ADDX
// ========================================

#[test]
fn test_addx_byte() {
    let rom = [0xD1, 0x01]; // ADDX.B D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x10;
    cpu.d[1] = 0x20;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x31, "0x10 + 0x20 + 1 (X)");
}

#[test]
fn test_addx_word() {
    let rom = [0xD1, 0x41]; // ADDX.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x1000;
    cpu.d[1] = 0x2000;
    cpu.set_flag(Flag::X, false);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x3000);
}

#[test]
fn test_addx_long() {
    let rom = [0xD1, 0x81]; // ADDX.L D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x10000000;
    cpu.d[1] = 0x01000000;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x11000001);
}

// ========================================
// SUB
// ========================================

#[test]
fn test_sub_byte() {
    let rom = [0x90, 0x01]; // SUB.B D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x30;
    cpu.d[1] = 0x10;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x20);
}

#[test]
fn test_sub_word() {
    let rom = [0x90, 0x41]; // SUB.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x1000;
    cpu.d[1] = 0x0100;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x0F00);
}

#[test]
fn test_sub_long() {
    let rom = [0x90, 0x81]; // SUB.L D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;
    cpu.d[1] = 0x00005678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x12340000);
}

// ========================================
// SUBI
// ========================================

#[test]
fn test_subi_byte() {
    let rom = [
        0x04, 0x00, // SUBI.B #imm, D0
        0x00, 0x05,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x10;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x0B);
}

#[test]
fn test_subi_word() {
    let rom = [
        0x04, 0x40, // SUBI.W #imm, D0
        0x01, 0x00,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x2000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x1F00);
}

#[test]
fn test_subi_long() {
    let rom = [
        0x04, 0x80, // SUBI.L #imm, D0
        0x00, 0x00, 0x10, 0x00,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x12344678);
}

// ========================================
// SUBQ
// ========================================

// TODO: Fix - conflicts with ADDQ
// #[test]
// fn test_subq_byte() {
//     let rom = [0x52, 0x00]; // SUBQ.B #1, D0
//     let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
//     cpu.pc = 0x0000;
//     cpu.d[0] = 0x10;
//     cpu.step(&mut bus);
//     assert_eq!(cpu.d[0] & 0xFF, 0x0F);
// }

#[test]
fn test_subq_word() {
    let rom = [0x53, 0x40]; // SUBQ.W #1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00001000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x0FFF);
}

#[test]
fn test_subq_long() {
    let rom = [0x5B, 0x80]; // SUBQ.L #5, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00001000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x00000FFB);
}

// ========================================
// SUBA
// ========================================

#[test]
fn test_suba_word() {
    let rom = [0x92, 0xC0]; // SUBA.W D0, A1

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[1] = 0x00002000;
    cpu.d[0] = 0x0100;

    cpu.step(&mut bus);

    assert_eq!(cpu.a[1], 0x00001F00);
}

// TODO: enable this test
// #[test]
// fn test_suba_long() {
//     let rom = [0x93, 0xC0]; // SUBA.L D0, A1
//     let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
//     cpu.pc = 0x0000;
//     cpu.a[1] = 0x12340000;
//     cpu.d[0] = 0x00001000;
//     cpu.step(&mut bus);
//     assert_eq!(cpu.a[1], 0x1233F000);
// }

// ========================================
// SUBX
// ========================================

#[test]
fn test_subx_byte() {
    let rom = [0x91, 0x01]; // SUBX.B D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x30;
    cpu.d[1] = 0x10;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x1F, "0x30 - 0x10 - 1 (X)");
}

#[test]
fn test_subx_word() {
    let rom = [0x91, 0x41]; // SUBX.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x2000;
    cpu.d[1] = 0x1000;
    cpu.set_flag(Flag::X, false);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x1000);
}

#[test]
fn test_subx_long() {
    let rom = [0x91, 0x81]; // SUBX.L D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x20000000;
    cpu.d[1] = 0x10000000;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x0FFFFFFF);
}

// ========================================
// NEG
// ========================================

#[test]
fn test_neg_byte() {
    let rom = [0x44, 0x00]; // NEG.B D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x10;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0xF0, "Two's complement of 0x10");
}

#[test]
fn test_neg_word() {
    let rom = [0x44, 0x40]; // NEG.W D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00001234;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, (-0x1234i32 & 0xFFFF) as u32);
}

#[test]
fn test_neg_long() {
    let rom = [0x44, 0x80]; // NEG.L D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] as i32, -0x12345678i32);
}

// ========================================
// NEGX
// ========================================

#[test]
fn test_negx_byte() {
    let rom = [0x40, 0x00]; // NEGX.B D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x10;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0xEF, "Two's complement - X");
}

#[test]
fn test_negx_word() {
    let rom = [0x40, 0x40]; // NEGX.W D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x0100;
    cpu.set_flag(Flag::X, false);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0xFF00);
}

#[test]
fn test_negx_long() {
    let rom = [0x40, 0x80]; // NEGX.L D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000001;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0xFFFFFFFE);
}
