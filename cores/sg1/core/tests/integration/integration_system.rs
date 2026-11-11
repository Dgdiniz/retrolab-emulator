// Integration tests for the System Control instructions
// JMP, JSR, RTS, RTE, RTR, LINK, UNLK, TRAP, RESET, STOP, NOP, CHK, TRAPV,
// MOVE to/from SR and CCR, ILLEGAL, PEA

use super::common::*;
use retro_core::cpu::Addressable;
use sg1_core::cartridge::composable::build_genesis_cartridge;
use sg1_core::cpu68k::instructions::Flag;

/// Helper to load a ROM into bus with header
fn load_test_rom(bus: &mut sg1_core::bus::AddressBus, mut rom: Vec<u8>) {
    if rom.len() < 512 {
        rom.resize(512, 0);
    }
    if &rom[0x100..0x110] != b"SEGA MEGA DRIVE " {
        rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
        rom[0x120..0x138].copy_from_slice(b"TEST ROM                ");
    }
    let cartridge = build_genesis_cartridge(rom).expect("Failed to create test cartridge");
    bus.set_cartridge(cartridge);
}

// ========================================
// JMP - Jump
// ========================================

#[test]
fn test_jmp_address_indirect() {
    let rom = [0x4E, 0xD0]; // JMP (A0)

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[0] = 0x00002000;

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x2000, "PC should jump to A0");
}

#[test]
fn test_jmp_absolute_long() {
    let rom = [
        0x4E, 0xF9, // JMP addr.L
        0x00, 0x01, 0x00, 0x00,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x00010000, "PC should jump to absolute address");
}

// ========================================
// JSR - Jump to Subroutine
// ========================================

#[test]
fn test_jsr_address_indirect() {
    let rom = [0x4E, 0x90]; // JSR (A0)

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[0] = 0x00002000;
    cpu.a[7] = 0xFF1000; // Stack pointer in RAM

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x2000, "PC should jump to A0");
    assert_eq!(cpu.a[7], 0xFF0FFC, "SP should be decremented by 4");

    // Return address (0x0002) should be pushed to stack
    let ret_addr = bus.read_word(0xFF0FFC) as u32;
    let ret_addr_low = bus.read_word(0xFF0FFE) as u32;
    assert_eq!((ret_addr << 16) | ret_addr_low, 0x00000002);
}

#[test]
fn test_jsr_absolute_long() {
    let rom = [
        0x4E, 0xB9, // JSR addr.L
        0x00, 0x01, 0x00, 0x00,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[7] = 0xFF1000; // Stack in RAM

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x00010000);
    assert_eq!(cpu.a[7], 0xFF0FFC);
}

// ========================================
// RTS - Return from Subroutine
// ========================================

#[test]
fn test_rts() {
    let rom = [0x4E, 0x75]; // RTS

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[7] = 0xFF0FFC; // Stack in RAM (0xFF0000+)

    // Setup return address on stack
    bus.write_word(0xFF0FFC, 0x0000);
    bus.write_word(0xFF0FFE, 0x1234);

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x00001234, "PC should return to stacked address");
    assert_eq!(cpu.a[7], 0xFF1000, "SP should be incremented by 4");
}

// ========================================
// RTE - Return from Exception
// ========================================

#[test]
fn test_rte() {
    let rom = [0x4E, 0x73]; // RTE

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[7] = 0xFF0FF8; // SSP in RAM

    // Setup SR and PC on stack
    bus.write_word(0xFF0FF8, 0x2000); // SR
    bus.write_word(0xFF0FFA, 0x0000); // PC high
    bus.write_word(0xFF0FFC, 0x1234); // PC low

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x00001234);
    assert_eq!(cpu.sr, 0x2000);
    assert_eq!(cpu.a[7], 0xFF0FFE);
}

// ========================================
// RTR - Return and Restore
// ========================================

#[test]
fn test_rtr() {
    let rom = [0x4E, 0x77]; // RTR

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[7] = 0xFF0FFA; // Stack in RAM

    // Setup CCR and PC on stack
    bus.write_word(0xFF0FFA, 0x001F); // CCR (lower byte of SR)
    bus.write_word(0xFF0FFC, 0x0000); // PC high
    bus.write_word(0xFF0FFE, 0x5678); // PC low

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x00005678);
    assert_eq!(cpu.sr & 0x1F, 0x1F, "CCR should be restored");
    assert_eq!(cpu.a[7], 0xFF1000);
}

// ========================================
// LINK - Link
// ========================================

#[test]
fn test_link() {
    let rom = [
        0x4E, 0x50, // LINK A0, #disp
        0xFF, 0xF0, // -16
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[0] = 0x12345678;
    cpu.a[7] = 0xFF1000; // Stack in RAM

    cpu.step(&mut bus);

    assert_eq!(cpu.a[7], 0xFF0FEC, "SP should be SP - 4 + disp");
    // Old A0 should be pushed to stack
    let saved_a0_high = bus.read_word(0xFF0FFC) as u32;
    let saved_a0_low = bus.read_word(0xFF0FFE) as u32;
    assert_eq!((saved_a0_high << 16) | saved_a0_low, 0x12345678);
    assert_eq!(cpu.a[0], 0xFF0FFC, "A0 should point to saved frame");
}

// ========================================
// UNLK - Unlink
// ========================================

#[test]
fn test_unlk() {
    let rom = [0x4E, 0x58]; // UNLK A0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[0] = 0xFF0FF0; // Frame pointer in RAM
    cpu.a[7] = 0xFF0FE0; // Stack in RAM

    // Setup saved A0 on stack (Big Endian: high word first)
    bus.write_word(0xFF0FF0, 0x1234); // High word
    bus.write_word(0xFF0FF2, 0x5678); // Low word

    cpu.step(&mut bus);

    assert_eq!(cpu.a[7], 0xFF0FF4, "SP should be A0 + 4");
    assert_eq!(cpu.a[0], 0x12345678, "A0 should be restored");
}

// ========================================
// TRAP - Trap
// ========================================

#[test]
fn test_trap() {
    // Need to setup trap vectors in ROM
    let mut full_rom = vec![0u8; 0x1000];

    // Trap #0 vector at 0x80 (vector 32): points to 0x00001000
    full_rom[0x80] = 0x00;
    full_rom[0x81] = 0x00;
    full_rom[0x82] = 0x10;
    full_rom[0x83] = 0x00;

    // TRAP #0 instruction
    full_rom[0x00] = 0x4E;
    full_rom[0x01] = 0x40;

    let mut cpu = sg1_core::cpu68k::M68k::new();
    let mut bus = sg1_core::bus::AddressBus::new();

    load_test_rom(&mut bus, full_rom);
    cpu.reset(&mut bus);
    cpu.pc = 0x0000;

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x1000, "PC should jump to trap vector");
}

#[test]
fn test_trap_different_vector() {
    let mut full_rom = vec![0u8; 0x1000];

    // Trap #5 vector at 0x94 (vector 32+5=37, addr=(32+5)*4=148=0x94): should point to 0x00002000
    // Big Endian: 0x00 0x00 0x20 0x00
    full_rom[0x94] = 0x00; // High byte of high word
    full_rom[0x95] = 0x00; // Low byte of high word
    full_rom[0x96] = 0x20; // High byte of low word
    full_rom[0x97] = 0x00; // Low byte of low word

    // TRAP #5
    full_rom[0x00] = 0x4E;
    full_rom[0x01] = 0x45;

    let mut cpu = sg1_core::cpu68k::M68k::new();
    let mut bus = sg1_core::bus::AddressBus::new();

    load_test_rom(&mut bus, full_rom);
    cpu.reset(&mut bus);
    cpu.pc = 0x0000;

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x2000);
}

// ========================================
// TRAPV - Trap on Overflow
// ========================================

#[test]
fn test_trapv_no_overflow() {
    let rom = [0x4E, 0x76]; // TRAPV

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::V, false);

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x0002, "Should continue when V=0");
}

// ========================================
// CHK - Check Register Against Bounds
// ========================================

#[test]
fn test_chk_in_bounds() {
    let rom = [0x41, 0x81]; // CHK D1, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 50; // Value to check
    cpu.d[1] = 100; // Upper bound

    cpu.step(&mut bus);

    // Should not trap when 0 <= D0 <= D1
    assert_eq!(cpu.pc, 0x0002, "Should continue when in bounds");
}

// ========================================
// NOP - No Operation
// ========================================

#[test]
fn test_nop() {
    let rom = [0x4E, 0x71]; // NOP

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    let old_d0 = cpu.d[0];
    let old_a0 = cpu.a[0];

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x0002, "PC should advance by 2");
    assert_eq!(cpu.d[0], old_d0, "Registers unchanged");
    assert_eq!(cpu.a[0], old_a0, "Registers unchanged");
}

// ========================================
// RESET - Reset External Devices
// ========================================

#[test]
fn test_reset() {
    let rom = [0x4E, 0x70]; // RESET

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.sr = 0x2700; // Supervisor mode

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x0002, "PC should advance");
}

// ========================================
// STOP - Stop and Wait
// ========================================

#[test]
fn test_stop() {
    let rom = [
        0x4E, 0x72, // STOP #imm
        0x27, 0x00,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.sr = 0x2700; // Supervisor mode

    cpu.step(&mut bus);

    // STOP loads immediate into SR and halts
    // For now, just check PC advanced
    assert_eq!(cpu.pc, 0x0004);
}

// ========================================
// MOVE to/from SR and CCR
// ========================================

#[test]
fn test_move_to_ccr() {
    let rom = [0x44, 0xC0]; // MOVE D0, CCR

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x1F; // All flags set

    cpu.step(&mut bus);

    assert_eq!(cpu.sr & 0x1F, 0x1F, "CCR should be updated");
}

#[test]
fn test_move_from_sr() {
    let rom = [0x40, 0xC0]; // MOVE SR, D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.sr = 0x271F;
    cpu.d[0] = 0x00000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x271F, "D0 should contain SR");
}

#[test]
fn test_move_to_sr() {
    let rom = [0x46, 0xC0]; // MOVE D0, SR

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.sr = 0x2700; // Supervisor mode
    cpu.d[0] = 0x2000;

    cpu.step(&mut bus);

    assert_eq!(cpu.sr, 0x2000, "SR should be updated");
}

// ========================================
// ILLEGAL - Illegal Instruction
// ========================================

#[test]
fn test_illegal() {
    let rom = [0x4A, 0xFC]; // ILLEGAL

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;

    cpu.step(&mut bus);

    // ILLEGAL should trigger exception
    // For now, just verify it executes without panic
}

// ========================================
// PEA - Push Effective Address
// ========================================

#[test]
fn test_pea() {
    let rom = [0x48, 0x50]; // PEA (A0)

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[0] = 0x12345678;
    cpu.a[7] = 0xFF1000; // Stack in RAM

    cpu.step(&mut bus);

    assert_eq!(cpu.a[7], 0xFF0FFC, "SP should be decremented by 4");

    // Address should be pushed
    let pushed_high = bus.read_word(0xFF0FFC) as u32;
    let pushed_low = bus.read_word(0xFF0FFE) as u32;
    assert_eq!((pushed_high << 16) | pushed_low, 0x12345678);
}
