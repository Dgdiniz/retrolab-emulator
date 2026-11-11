//! Z80 Integration Tests - Rotate & Shift Instructions
//!
//! Tests for RLCA/RRCA/RLA/RRA, CB prefix rotates/shifts, and RLD/RRD

use retro_core::cpu::Addressable;
use retro_core::memory::Ram;
use sg1_core::z80::Z80;

#[test]
fn test_accumulator_rotates() {
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Mini ROM: Test RLCA, RRCA, RLA, RRA
    let rom = [
        0x3E, 0xA5, // LD A, 0xA5 (10100101)
        0x07, // RLCA -> 0x4B (01001011), C=1
        0x0F, // RRCA -> 0xA5 (10100101), C=1
        0x17, // RLA -> 0x4B (01001011), C=1 (A=4A + old_carry=1)
        0x1F, // RRA -> 0xA5 (10100101), C=1
        0x76, // HALT
    ];

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.pc = 0;

    let mut iterations = 0;
    while !cpu.halted && iterations < 50 {
        cpu.step(&mut ram);
        iterations += 1;
    }

    assert!(cpu.halted);
    assert_eq!(cpu.a, 0xA5);
    assert!(cpu.flag_c());
}

#[test]
fn test_cb_prefix_rotates() {
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Mini ROM: Test CB rotates
    let rom = [
        0x06, 0x81, // LD B, 0x81 (10000001)
        0xCB, 0x00, // RLC B -> 0x03 (00000011), C=1
        0x0E, 0x02, // LD C, 0x02 (00000010)
        0xCB, 0x09, // RRC C -> 0x01 (00000001), C=0
        0x16, 0x80, // LD D, 0x80 (10000000)
        0xCB, 0x12, // RL D -> 0x00 (00000000), C=1
        0x1E, 0x40, // LD E, 0x40 (01000000)
        0xCB, 0x1B, // RR E -> 0xA0 (10100000), C=0
        0x76, // HALT
    ];

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.pc = 0;

    let mut iterations = 0;
    while !cpu.halted && iterations < 50 {
        cpu.step(&mut ram);
        iterations += 1;
    }

    assert!(cpu.halted);
    assert_eq!(cpu.b, 0x03);
    assert_eq!(cpu.c, 0x01);
    assert_eq!(cpu.d, 0x00);
    assert_eq!(cpu.e, 0xA0);
    assert!(!cpu.flag_c()); // Last operation was RR E, bit 0 was 0
}

#[test]
fn test_cb_prefix_shifts() {
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Mini ROM: Test CB shifts
    let rom = [
        0x26, 0x55, // LD H, 0x55 (01010101)
        0xCB, 0x24, // SLA H -> 0xAA (10101010), C=0
        0x2E, 0x80, // LD L, 0x80 (10000000)
        0xCB, 0x2D, // SRA L -> 0xC0 (11000000), C=0
        0x3E, 0x01, // LD A, 0x01 (00000001)
        0xCB, 0x3F, // SRL A -> 0x00 (00000000), C=1
        0x76, // HALT
    ];

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.pc = 0;

    let mut iterations = 0;
    while !cpu.halted && iterations < 50 {
        cpu.step(&mut ram);
        iterations += 1;
    }

    assert!(cpu.halted);
    assert_eq!(cpu.h, 0xAA);
    assert_eq!(cpu.l, 0xC0);
    assert_eq!(cpu.a, 0x00);
    assert!(cpu.flag_c()); // Last operation was SRL A, bit 0 was 1
}

#[test]
fn test_rld_rrd() {
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Mini ROM: Test RLD and RRD
    let rom = [
        0x21, 0x10, 0x00, // LD HL, 0x0010
        0x36, 0x12, // LD (HL), 0x12
        0x3E, 0x34, // LD A, 0x34
        0xED, 0x6F, // RLD -> A=0x31, (HL)=0x24
        0xED, 0x67, // RRD -> A=0x34, (HL)=0x12
        0x76, // HALT
    ];

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.pc = 0;

    let mut iterations = 0;
    while !cpu.halted && iterations < 50 {
        cpu.step(&mut ram);
        iterations += 1;
    }

    assert!(cpu.halted);
    assert_eq!(cpu.a, 0x34);
    assert_eq!(ram.read(0x10), 0x12);
}

#[test]
fn test_cb_with_hl_indirect() {
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Mini ROM: Test CB operations on (HL)
    let rom = [
        0x21, 0x10, 0x00, // LD HL, 0x0010
        0x36, 0x80, // LD (HL), 0x80
        0xCB, 0x06, // RLC (HL) -> 0x01, C=1
        0x7E, // LD A, (HL)
        0x76, // HALT
    ];

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.pc = 0;

    let mut iterations = 0;
    while !cpu.halted && iterations < 50 {
        cpu.step(&mut ram);
        iterations += 1;
    }

    assert!(cpu.halted);
    assert_eq!(cpu.a, 0x01);
    assert_eq!(ram.read(0x10), 0x01);
    assert!(cpu.flag_c());
}

#[test]
fn test_bitwise_manipulation() {
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Mini ROM: Complex bit manipulation
    let rom = [
        0x3E, 0xFF, // LD A, 0xFF
        0x0F, // RRCA -> 0xFF, C=1
        0x0F, // RRCA -> 0xFF, C=1
        0x07, // RLCA -> 0xFF, C=1
        0x06, 0x00, // LD B, 0x00
        0xCB, 0x30, // SLL B -> 0x01, C=0 (undocumented) (opcode 0x30 = SLL B)
        0x76, // HALT
    ];

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.pc = 0;

    let mut iterations = 0;
    while !cpu.halted && iterations < 50 {
        cpu.step(&mut ram);
        iterations += 1;
    }

    assert!(cpu.halted);
    assert_eq!(cpu.a, 0xFF);
    assert_eq!(cpu.b, 0x01);
    assert!(!cpu.flag_c()); // Last SLL set bit 0 to 1, no carry
}

#[test]
fn test_flag_preservation() {
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Mini ROM: Test that S, Z, PV are preserved in accumulator rotates
    let rom = [
        0x3E, 0x00, // LD A, 0x00
        0xCB, 0x27, // SLA A -> Z=1, S=0, PV=1 (even parity)
        0x3E, 0x80, // LD A, 0x80
        0x07, // RLCA -> A=0x01, C=1 (S, Z, PV unchanged from CB)
        0x76, // HALT
    ];

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.pc = 0;

    let mut iterations = 0;
    while !cpu.halted && iterations < 50 {
        cpu.step(&mut ram);
        iterations += 1;
    }

    assert!(cpu.halted);
    assert_eq!(cpu.a, 0x01);
    // Note: RLCA doesn't change S, Z, PV, so they should still be from the SLA
    assert!(cpu.flag_z()); // From SLA A (A was 0)
}
