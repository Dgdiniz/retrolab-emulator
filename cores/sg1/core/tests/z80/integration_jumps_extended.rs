// Z80 Integration Tests - Extended Jumps
// Tests JP/CALL/RET PO/PE/P/M, JP (HL)/(IX)/(IY), RST

use retro_core::cpu::{Addressable, Resettable};
use retro_core::memory::Ram;
use sg1_core::z80::Z80;

/// Helper to execute a mini ROM
fn execute_rom(rom: &[u8], steps: u32) -> (Z80, Ram) {
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Load ROM at address 0x0000
    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.reset();

    // Execute specified number of steps
    for _ in 0..steps {
        cpu.step(&mut ram);
    }

    (cpu, ram)
}

// ============================================================================
// JP (HL)/(IX)/(IY) TESTS
// ============================================================================

#[test]
fn test_jp_hl_integration() {
    // Mini ROM:
    // LD HL, $2000   ; 0x21 0x00 0x20
    // JP (HL)        ; 0xE9
    // ...
    // At 0x2000: NOP ; 0x00
    let rom = vec![
        0x21, 0x00, 0x20, // LD HL, $2000
        0xE9, // JP (HL)
    ];

    let (cpu, mut ram) = execute_rom(&rom, 2);

    // Write NOP at target
    ram.write(0x2000, 0x00);

    assert_eq!(cpu.pc, 0x2000); // Jumped to HL
}

#[test]
fn test_jp_ix_integration() {
    // Test JP (IX) by setting IX manually
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    ram.write(0x0000, 0xDD); // DD prefix
    ram.write(0x0001, 0xE9); // JP (IX)

    cpu.reset();
    cpu.ix = 0x3000; // Set after reset
    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x3000);
}

#[test]
fn test_jp_iy_integration() {
    // Test JP (IY) by setting IY manually
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    ram.write(0x0000, 0xFD); // FD prefix
    ram.write(0x0001, 0xE9); // JP (IY)

    cpu.reset();
    cpu.iy = 0x4000; // Set after reset
    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x4000);
}

// ============================================================================
// RST INSTRUCTION TESTS
// ============================================================================

#[test]
fn test_rst_vector_table() {
    // Test all 8 RST vectors
    // Mini ROM: Execute each RST and verify PC and stack
    let rst_vectors = [
        (0xC7, 0x00),
        (0xCF, 0x08),
        (0xD7, 0x10),
        (0xDF, 0x18),
        (0xE7, 0x20),
        (0xEF, 0x28),
        (0xF7, 0x30),
        (0xFF, 0x38),
    ];

    for (opcode, target) in rst_vectors.iter() {
        let rom = vec![*opcode];
        let (cpu, mut ram) = execute_rom(&rom, 1);

        assert_eq!(cpu.pc, *target as u16, "RST ${:02X} failed", opcode);

        // Check return address was pushed (should be 0x0001)
        assert_eq!(ram.read(cpu.sp as u32), 0x01); // Low byte
        assert_eq!(ram.read((cpu.sp + 1) as u32), 0x00); // High byte
    }
}

#[test]
fn test_rst_with_return() {
    // Mini ROM:
    // RST 10h        ; 0xD7 (at 0x0000, pushes 0x0001, jumps to 0x0010)
    // HALT           ; 0x76 (at 0x0001)
    // ...
    // At 0x0010: RET ; 0xC9
    let mut rom = vec![0; 0x1000];
    rom[0x0000] = 0xD7; // RST 10h
    rom[0x0001] = 0x76; // HALT
    rom[0x0010] = 0xC9; // RET

    let (cpu, _) = execute_rom(&rom, 2); // RST + RET

    assert_eq!(cpu.pc, 0x0001); // Returned to address after RST
}

// ============================================================================
// CONDITIONAL JP/CALL/RET PO/PE/P/M TESTS
// ============================================================================

#[test]
fn test_jp_po_taken() {
    // Jump if parity odd (PV flag = 0)
    // Set PV flag manually and test JP PO
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    ram.write(0x0000, 0xE2); // JP PO, $2000
    ram.write(0x0001, 0x00);
    ram.write(0x0002, 0x20);

    cpu.reset();
    cpu.set_flag_pv(false); // PV=0 (odd parity) - Set after reset
    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x2000); // Jump taken because PV=0
}

#[test]
fn test_jp_pe_taken() {
    // Jump if parity even (PV flag = 1)
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    ram.write(0x0000, 0xEA); // JP PE, $3000
    ram.write(0x0001, 0x00);
    ram.write(0x0002, 0x30);

    cpu.reset();
    cpu.set_flag_pv(true); // PV=1 (even parity) - Set after reset
    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x3000); // Jump taken because PV=1
}

#[test]
fn test_jp_p_taken() {
    // Jump if positive (S flag = 0)
    // Mini ROM:
    // LD A, $7F      ; 0x3E 0x7F (positive)
    // OR A           ; 0xB7 (sets S=0 because bit 7 is 0)
    // JP P, $4000    ; 0xF2 0x00 0x40
    let rom = vec![
        0x3E, 0x7F, // LD A, $7F
        0xB7, // OR A
        0xF2, 0x00, 0x40, // JP P, $4000
    ];

    let (cpu, _) = execute_rom(&rom, 3);

    assert_eq!(cpu.pc, 0x4000); // Jump taken
}

#[test]
fn test_jp_m_taken() {
    // Jump if minus (S flag = 1)
    // Mini ROM:
    // LD A, $FF      ; 0x3E 0xFF (negative)
    // OR A           ; 0xB7 (sets S=1 because bit 7 is 1)
    // JP M, $5000    ; 0xFA 0x00 0x50
    let rom = vec![
        0x3E, 0xFF, // LD A, $FF
        0xB7, // OR A
        0xFA, 0x00, 0x50, // JP M, $5000
    ];

    let (cpu, _) = execute_rom(&rom, 3);

    assert_eq!(cpu.pc, 0x5000); // Jump taken
}

#[test]
fn test_call_ret_po() {
    // CALL PO and RET PO sequence
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    ram.write(0x0000, 0xE4); // CALL PO, $1000
    ram.write(0x0001, 0x00);
    ram.write(0x0002, 0x10);
    ram.write(0x0003, 0x76); // HALT
    ram.write(0x1000, 0xE0); // RET PO

    cpu.reset();
    cpu.set_flag_pv(false); // PV=0 (odd parity) - Set after reset
    cpu.step(&mut ram); // CALL
    cpu.step(&mut ram); // RET

    assert_eq!(cpu.pc, 0x0003); // Returned to address after CALL
}

#[test]
fn test_call_ret_pe() {
    // CALL PE and RET PE sequence
    // Mini ROM:
    // LD A, $7F      ; 0x3E 0x7F
    // ADD A, $01     ; 0xC6 0x01 (overflow, PV=1)
    // CALL PE, $1000 ; 0xEC 0x00 0x10
    // HALT           ; 0x76
    // ...
    // At 0x1000: RET PE ; 0xE8
    let mut rom = vec![0; 0x1100];
    rom[0x0000] = 0x3E; // LD A, $7F
    rom[0x0001] = 0x7F;
    rom[0x0002] = 0xC6; // ADD A, $01
    rom[0x0003] = 0x01;
    rom[0x0004] = 0xEC; // CALL PE
    rom[0x0005] = 0x00;
    rom[0x0006] = 0x10;
    rom[0x0007] = 0x76; // HALT
    rom[0x1000] = 0xE8; // RET PE

    let (cpu, _) = execute_rom(&rom, 4); // LD + ADD + CALL + RET

    assert_eq!(cpu.pc, 0x0007); // Returned to address after CALL
}

#[test]
fn test_call_ret_p() {
    // CALL P and RET P sequence
    // Mini ROM:
    // LD A, $00      ; 0x3E 0x00 (positive)
    // OR A           ; 0xB7 (S=0)
    // CALL P, $1000  ; 0xF4 0x00 0x10
    // HALT           ; 0x76
    // ...
    // At 0x1000: RET P ; 0xF0
    let mut rom = vec![0; 0x1100];
    rom[0x0000] = 0x3E; // LD A, $00
    rom[0x0001] = 0x00;
    rom[0x0002] = 0xB7; // OR A
    rom[0x0003] = 0xF4; // CALL P
    rom[0x0004] = 0x00;
    rom[0x0005] = 0x10;
    rom[0x0006] = 0x76; // HALT
    rom[0x1000] = 0xF0; // RET P

    let (cpu, _) = execute_rom(&rom, 4); // LD + OR + CALL + RET

    assert_eq!(cpu.pc, 0x0006); // Returned to address after CALL
}

#[test]
fn test_call_ret_m() {
    // CALL M and RET M sequence
    // Mini ROM:
    // LD A, $80      ; 0x3E 0x80 (negative)
    // OR A           ; 0xB7 (S=1)
    // CALL M, $1000  ; 0xFC 0x00 0x10
    // HALT           ; 0x76
    // ...
    // At 0x1000: RET M ; 0xF8
    let mut rom = vec![0; 0x1100];
    rom[0x0000] = 0x3E; // LD A, $80
    rom[0x0001] = 0x80;
    rom[0x0002] = 0xB7; // OR A
    rom[0x0003] = 0xFC; // CALL M
    rom[0x0004] = 0x00;
    rom[0x0005] = 0x10;
    rom[0x0006] = 0x76; // HALT
    rom[0x1000] = 0xF8; // RET M

    let (cpu, _) = execute_rom(&rom, 4); // LD + OR + CALL + RET

    assert_eq!(cpu.pc, 0x0006); // Returned to address after CALL
}

// ============================================================================
// COMPLEX JUMP TABLE TEST
// ============================================================================

#[test]
fn test_jump_table_with_rst() {
    // Simulate a jump table using RST vectors
    // Mini ROM:
    // RST 08h        ; 0xCF
    // HALT           ; 0x76
    // ...
    // At 0x0008:
    // LD HL, $3000   ; 0x21 0x00 0x30
    // JP (HL)        ; 0xE9
    // ...
    // At 0x3000: NOP ; 0x00
    let mut rom = vec![0; 0x4000];
    rom[0x0000] = 0xCF; // RST 08h
    rom[0x0001] = 0x76; // HALT
    rom[0x0008] = 0x21; // LD HL, $3000
    rom[0x0009] = 0x00;
    rom[0x000A] = 0x30;
    rom[0x000B] = 0xE9; // JP (HL)
    rom[0x3000] = 0x00; // NOP

    let (cpu, _) = execute_rom(&rom, 3); // RST + LD + JP

    assert_eq!(cpu.pc, 0x3000); // Arrived at target via jump table
}
