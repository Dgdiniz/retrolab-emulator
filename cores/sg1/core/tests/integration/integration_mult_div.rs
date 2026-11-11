// Integration tests for multiplication and division
// MULS, MULU, DIVS, DIVU

use super::common::*;

// ========================================
// MULU - Multiply Unsigned
// ========================================

#[test]
fn test_mulu_simple() {
    let rom = [0xC0, 0xC1]; // MULU.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x0010; // 16
    cpu.d[1] = 0x0020; // 32

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x0200, "16 * 32 = 512");
}

#[test]
fn test_mulu_large() {
    let rom = [0xC0, 0xC1]; // MULU.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFFFF; // 65535
    cpu.d[1] = 0x0002; // 2

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x0001FFFE, "65535 * 2 = 131070");
}

#[test]
fn test_mulu_zero() {
    let rom = [0xC0, 0xC1]; // MULU.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x1234;
    cpu.d[1] = 0x0000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x00000000, "Any * 0 = 0");
}

// ========================================
// MULS - Multiply Signed
// ========================================

#[test]
fn test_muls_positive() {
    let rom = [0xC1, 0xC1]; // MULS.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x0008; // 8
    cpu.d[1] = 0x0004; // 4

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] as i32, 32, "8 * 4 = 32");
}

#[test]
fn test_muls_negative() {
    let rom = [0xC1, 0xC1]; // MULS.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFFFFFFF0; // -16 (sign extended)
    cpu.d[1] = 0x00000002; // 2

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] as i32, -32, "-16 * 2 = -32");
}

#[test]
fn test_muls_negative_both() {
    let rom = [0xC1, 0xC1]; // MULS.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFFFFFFF8; // -8
    cpu.d[1] = 0xFFFFFFFC; // -4

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] as i32, 32, "-8 * -4 = 32");
}

// ========================================
// DIVU - Divide Unsigned
// ========================================

#[test]
fn test_divu_simple() {
    let rom = [0x80, 0xC1]; // DIVU.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000064; // 100
    cpu.d[1] = 0x0000000A; // 10

    cpu.step(&mut bus);

    let quotient = cpu.d[0] & 0xFFFF;
    let remainder = (cpu.d[0] >> 16) & 0xFFFF;

    assert_eq!(quotient, 10, "100 / 10 = 10");
    assert_eq!(remainder, 0, "100 % 10 = 0");
}

#[test]
fn test_divu_with_remainder() {
    let rom = [0x80, 0xC1]; // DIVU.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000017; // 23
    cpu.d[1] = 0x00000005; // 5

    cpu.step(&mut bus);

    let quotient = cpu.d[0] & 0xFFFF;
    let remainder = (cpu.d[0] >> 16) & 0xFFFF;

    assert_eq!(quotient, 4, "23 / 5 = 4");
    assert_eq!(remainder, 3, "23 % 5 = 3");
}

#[test]
fn test_divu_large_dividend() {
    let rom = [0x80, 0xC1]; // DIVU.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00010000; // 65536
    cpu.d[1] = 0x00000100; // 256

    cpu.step(&mut bus);

    let quotient = cpu.d[0] & 0xFFFF;
    assert_eq!(quotient, 256, "65536 / 256 = 256");
}

// ========================================
// DIVS - Divide Signed
// ========================================

#[test]
fn test_divs_positive() {
    let rom = [0x81, 0xC1]; // DIVS.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000064; // 100
    cpu.d[1] = 0x0000000A; // 10

    cpu.step(&mut bus);

    let quotient = (cpu.d[0] & 0xFFFF) as i16;
    let remainder = ((cpu.d[0] >> 16) & 0xFFFF) as i16;

    assert_eq!(quotient, 10, "100 / 10 = 10");
    assert_eq!(remainder, 0, "100 % 10 = 0");
}

#[test]
fn test_divs_negative_dividend() {
    let rom = [0x81, 0xC1]; // DIVS.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFFFFFFE0; // -32 (sign extended)
    cpu.d[1] = 0x0000FFFE; // -2 (sign extended word)

    cpu.step(&mut bus);

    let quotient = (cpu.d[0] & 0xFFFF) as i16;

    assert_eq!(quotient, 16, "-32 / -2 = 16");
}

#[test]
fn test_divs_negative_divisor() {
    let rom = [0x81, 0xC1]; // DIVS.W D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000064; // 100
    cpu.d[1] = 0x0000FFFA; // -6

    cpu.step(&mut bus);

    let quotient = (cpu.d[0] & 0xFFFF) as i16;

    assert_eq!(quotient, -16, "100 / -6 = -16");
}
