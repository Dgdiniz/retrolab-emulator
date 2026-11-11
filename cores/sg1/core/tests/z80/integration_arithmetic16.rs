//! Integration tests for Z80 16-bit arithmetic instructions

use retro_core::cpu::Addressable;
use retro_core::memory::Ram;
use sg1_core::z80::Z80;

#[test]
fn test_16bit_counter() {
    // Test INC BC in a loop
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Program: Count BC from 0 to 5
    let rom = [
        0x01, 0x00, 0x00, // LD BC, 0x0000
        0x03, // loop: INC BC
        0x78, // LD A, B
        0xFE, 0x00, // CP 0
        0x20, 0x05, // JR NZ, done (skip if B != 0)
        0x79, // LD A, C
        0xFE, 0x05, // CP 5
        0x20, 0xF5, // JR NZ, loop
        0x76, // done: HALT
    ];

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.pc = 0x0000;

    // Run until HALT
    let mut iterations = 0;
    while !cpu.halted && iterations < 1000 {
        cpu.step(&mut ram);
        iterations += 1;
    }

    assert!(cpu.halted);
    assert_eq!(cpu.b, 0x00);
    assert_eq!(cpu.c, 0x05);
}

#[test]
fn test_16bit_subtraction() {
    // Test DEC HL in a loop
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Program: Count HL down from 5 to 0
    let rom = [
        0x21, 0x05, 0x00, // LD HL, 0x0005
        0x7C, // loop: LD A, H
        0xB5, // OR L (set Z if HL == 0)
        0x28, 0x03, // JR Z, done
        0x2B, // DEC HL
        0x18, 0xF8, // JR -8: to 0x02, the 0x00 before loop (it runs as a NOP)
        0x76, // done: HALT
    ];

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.pc = 0x0000;

    // Run until HALT
    let mut iterations = 0;
    while !cpu.halted && iterations < 1000 {
        cpu.step(&mut ram);
        iterations += 1;
    }

    assert!(cpu.halted);
    assert_eq!(cpu.h, 0x00);
    assert_eq!(cpu.l, 0x00);
}

#[test]
fn test_add_hl_sequence() {
    // Test ADD HL, rr with different register pairs
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Program: HL = HL + BC + DE + SP
    let rom = [
        0x21, 0x10, 0x00, // LD HL, 0x0010
        0x01, 0x05, 0x00, // LD BC, 0x0005
        0x11, 0x0A, 0x00, // LD DE, 0x000A
        0x31, 0x15, 0x00, // LD SP, 0x0015
        0x09, // ADD HL, BC  (HL = 0x0015)
        0x19, // ADD HL, DE  (HL = 0x001F)
        0x39, // ADD HL, SP  (HL = 0x0034)
        0x76, // HALT
    ];

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.pc = 0x0000;

    // Run until HALT
    let mut iterations = 0;
    while !cpu.halted && iterations < 100 {
        cpu.step(&mut ram);
        iterations += 1;
    }

    assert!(cpu.halted);
    let hl = ((cpu.h as u16) << 8) | (cpu.l as u16);
    assert_eq!(hl, 0x0034); // 0x10 + 0x05 + 0x0A + 0x15
}

#[test]
fn test_adc_hl_with_carry_propagation() {
    // Test ADC HL, rr with carry
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Program: Test carry propagation
    let rom = [
        0x21, 0x00, 0x80, // LD HL, 0x8000
        0x01, 0x00, 0x80, // LD BC, 0x8000
        0x09, // ADD HL, BC  (HL = 0x0000, C=1)
        0x11, 0x01, 0x00, // LD DE, 0x0001
        0xED, 0x5A, // ADC HL, DE  (HL = 0x0002 with carry)
        0x76, // HALT
    ];

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.pc = 0x0000;

    // Run until HALT
    let mut iterations = 0;
    while !cpu.halted && iterations < 100 {
        cpu.step(&mut ram);
        iterations += 1;
    }

    assert!(cpu.halted);
    let hl = ((cpu.h as u16) << 8) | (cpu.l as u16);
    assert_eq!(hl, 0x0002); // 0x0000 + 0x0001 + carry(1)
}

#[test]
fn test_sbc_hl_with_borrow() {
    // Test SBC HL, rr
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Program: Test subtraction with borrow
    let rom = [
        0x21, 0x00, 0x10, // LD HL, 0x1000
        0x01, 0x00, 0x05, // LD BC, 0x0500
        0xED, 0x42, // SBC HL, BC  (HL = 0x0B00, no borrow)
        0x11, 0x00, 0x06, // LD DE, 0x0600
        0xED, 0x52, // SBC HL, DE  (HL = 0x0500, no borrow)
        0x76, // HALT
    ];

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.pc = 0x0000;

    // Run until HALT
    let mut iterations = 0;
    while !cpu.halted && iterations < 100 {
        cpu.step(&mut ram);
        iterations += 1;
    }

    assert!(cpu.halted);
    let hl = ((cpu.h as u16) << 8) | (cpu.l as u16);
    assert_eq!(hl, 0x0500); // 0x1000 - 0x0500 - 0x0600
}

#[test]
fn test_inc_dec_sp() {
    // Test INC SP and DEC SP
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Program: Test SP manipulation
    let rom = [
        0x31, 0x00, 0xFF, // LD SP, 0xFF00
        0x33, // INC SP  (SP = 0xFF01)
        0x33, // INC SP  (SP = 0xFF02)
        0x3B, // DEC SP  (SP = 0xFF01)
        0x76, // HALT
    ];

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.pc = 0x0000;

    // Run until HALT
    let mut iterations = 0;
    while !cpu.halted && iterations < 100 {
        cpu.step(&mut ram);
        iterations += 1;
    }

    assert!(cpu.halted);
    assert_eq!(cpu.sp, 0xFF01);
}

#[test]
fn test_add_hl_hl_doubling() {
    // Test ADD HL, HL (doubling)
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Program: Double HL three times
    let rom = [
        0x21, 0x05, 0x00, // LD HL, 0x0005
        0x29, // ADD HL, HL  (HL = 0x000A)
        0x29, // ADD HL, HL  (HL = 0x0014)
        0x29, // ADD HL, HL  (HL = 0x0028)
        0x76, // HALT
    ];

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.pc = 0x0000;

    // Run until HALT
    let mut iterations = 0;
    while !cpu.halted && iterations < 100 {
        cpu.step(&mut ram);
        iterations += 1;
    }

    assert!(cpu.halted);
    let hl = ((cpu.h as u16) << 8) | (cpu.l as u16);
    assert_eq!(hl, 0x0028); // 5 * 2 * 2 * 2 = 40 (0x28)
}
