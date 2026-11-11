// Z80 Integration Tests - Bit Operations (BIT/SET/RES)
// Tests the full instruction fetch-decode-execute cycle via CPU step()

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
// BIT INSTRUCTION TESTS
// ============================================================================

#[test]
fn test_bit_instruction_basic() {
    // Mini ROM:
    // LD A, $AA      ; 0x3E 0xAA
    // BIT 0, A       ; 0xCB 0x47 (bit 0 of 0xAA is 0, so Z should be set)
    // BIT 1, A       ; 0xCB 0x4F (bit 1 of 0xAA is 1, so Z should be clear)
    // BIT 7, A       ; 0xCB 0x7F (bit 7 of 0xAA is 1, so Z should be clear)
    let rom = vec![
        0x3E, 0xAA, // LD A, $AA
        0xCB, 0x47, // BIT 0, A
        0xCB, 0x4F, // BIT 1, A
        0xCB, 0x7F, // BIT 7, A
    ];

    let (cpu, _) = execute_rom(&rom, 4);

    // After BIT 7, A (last instruction)
    assert_eq!(cpu.a, 0xAA); // A unchanged
    assert!(!cpu.flag_z()); // Bit 7 is set, so Z=0
    assert!(cpu.flag_h()); // H always set by BIT
    assert!(!cpu.flag_n()); // N always reset by BIT
    assert!(cpu.flag_s()); // S reflects bit 7 when testing bit 7
}

#[test]
fn test_bit_instruction_zero_flag() {
    // Mini ROM:
    // LD B, $10      ; 0x06 0x10 (bit 4 set, others clear)
    // BIT 4, B       ; 0xCB 0x60 (bit 4 set, Z=0)
    // BIT 5, B       ; 0xCB 0x68 (bit 5 clear, Z=1)
    let rom = vec![
        0x06, 0x10, // LD B, $10
        0xCB, 0x60, // BIT 4, B
        0xCB, 0x68, // BIT 5, B
    ];

    let (cpu, _) = execute_rom(&rom, 3);

    // After BIT 5, B (last instruction)
    assert_eq!(cpu.b, 0x10);
    assert!(cpu.flag_z()); // Bit 5 is clear
    assert!(cpu.flag_h());
    assert!(!cpu.flag_n());
}

#[test]
fn test_bit_instruction_hl_indirect() {
    // Mini ROM:
    // LD HL, $1000   ; 0x21 0x00 0x10
    // LD (HL), $80   ; 0x36 0x80 (bit 7 set)
    // BIT 7, (HL)    ; 0xCB 0x7E
    // BIT 0, (HL)    ; 0xCB 0x46
    let rom = vec![
        0x21, 0x00, 0x10, // LD HL, $1000
        0x36, 0x80, // LD (HL), $80
        0xCB, 0x7E, // BIT 7, (HL)
        0xCB, 0x46, // BIT 0, (HL)
    ];

    let (cpu, mut ram) = execute_rom(&rom, 4);

    // After BIT 0, (HL) (last instruction)
    assert_eq!(ram.read(0x1000), 0x80); // Memory unchanged
    assert!(cpu.flag_z()); // Bit 0 is clear
    assert!(cpu.flag_h());
}

#[test]
fn test_bit_all_bits_sequence() {
    // Test all 8 bits of 0xF0 (bits 4-7 set)
    // LD C, $F0      ; 0x0E 0xF0
    // BIT 0, C       ; 0xCB 0x41 (Z=1)
    // BIT 1, C       ; 0xCB 0x49 (Z=1)
    // BIT 2, C       ; 0xCB 0x51 (Z=1)
    // BIT 3, C       ; 0xCB 0x59 (Z=1)
    // BIT 4, C       ; 0xCB 0x61 (Z=0)
    // BIT 5, C       ; 0xCB 0x69 (Z=0)
    // BIT 6, C       ; 0xCB 0x71 (Z=0)
    // BIT 7, C       ; 0xCB 0x79 (Z=0)
    let rom = vec![
        0x0E, 0xF0, // LD C, $F0
        0xCB, 0x41, // BIT 0, C
        0xCB, 0x49, // BIT 1, C
        0xCB, 0x51, // BIT 2, C
        0xCB, 0x59, // BIT 3, C
        0xCB, 0x61, // BIT 4, C
        0xCB, 0x69, // BIT 5, C
        0xCB, 0x71, // BIT 6, C
        0xCB, 0x79, // BIT 7, C
    ];

    let (cpu, _) = execute_rom(&rom, 9);

    // After BIT 7, C (bit 7 is set in 0xF0)
    assert_eq!(cpu.c, 0xF0);
    assert!(!cpu.flag_z()); // Bit 7 is set
}

// ============================================================================
// SET INSTRUCTION TESTS
// ============================================================================

#[test]
fn test_set_instruction_basic() {
    // Mini ROM:
    // LD D, $00      ; 0x16 0x00
    // SET 0, D       ; 0xCB 0xC2 (D = 0x01)
    // SET 7, D       ; 0xCB 0xFA (D = 0x81)
    let rom = vec![
        0x16, 0x00, // LD D, $00
        0xCB, 0xC2, // SET 0, D
        0xCB, 0xFA, // SET 7, D
    ];

    let (cpu, _) = execute_rom(&rom, 3);

    assert_eq!(cpu.d, 0x81); // Bits 0 and 7 set
}

#[test]
fn test_set_all_bits() {
    // Set all 8 bits starting from 0x00
    // LD E, $00      ; 0x1E 0x00
    // SET 0, E       ; 0xCB 0xC3
    // SET 1, E       ; 0xCB 0xCB
    // SET 2, E       ; 0xCB 0xD3
    // SET 3, E       ; 0xCB 0xDB
    // SET 4, E       ; 0xCB 0xE3
    // SET 5, E       ; 0xCB 0xEB
    // SET 6, E       ; 0xCB 0xF3
    // SET 7, E       ; 0xCB 0xFB
    let rom = vec![
        0x1E, 0x00, // LD E, $00
        0xCB, 0xC3, // SET 0, E
        0xCB, 0xCB, // SET 1, E
        0xCB, 0xD3, // SET 2, E
        0xCB, 0xDB, // SET 3, E
        0xCB, 0xE3, // SET 4, E
        0xCB, 0xEB, // SET 5, E
        0xCB, 0xF3, // SET 6, E
        0xCB, 0xFB, // SET 7, E
    ];

    let (cpu, _) = execute_rom(&rom, 9);

    assert_eq!(cpu.e, 0xFF); // All bits set
}

#[test]
fn test_set_hl_indirect() {
    // Mini ROM:
    // LD HL, $2000   ; 0x21 0x00 0x20
    // LD (HL), $00   ; 0x36 0x00
    // SET 3, (HL)    ; 0xCB 0xDE
    // SET 5, (HL)    ; 0xCB 0xEE
    let rom = vec![
        0x21, 0x00, 0x20, // LD HL, $2000
        0x36, 0x00, // LD (HL), $00
        0xCB, 0xDE, // SET 3, (HL)
        0xCB, 0xEE, // SET 5, (HL)
    ];

    let (cpu, mut ram) = execute_rom(&rom, 4);

    assert_eq!(cpu.h, 0x20);
    assert_eq!(cpu.l, 0x00);
    assert_eq!(ram.read(0x2000), 0x28); // Bits 3 and 5 set (0b00101000)
}

#[test]
fn test_set_idempotent() {
    // SET should not affect already-set bits
    // LD H, $FF      ; 0x26 0xFF
    // SET 4, H       ; 0xCB 0xE4
    let rom = vec![
        0x26, 0xFF, // LD H, $FF
        0xCB, 0xE4, // SET 4, H
    ];

    let (cpu, _) = execute_rom(&rom, 2);

    assert_eq!(cpu.h, 0xFF); // No change
}

// ============================================================================
// RES INSTRUCTION TESTS
// ============================================================================

#[test]
fn test_res_instruction_basic() {
    // Mini ROM:
    // LD L, $FF      ; 0x2E 0xFF
    // RES 0, L       ; 0xCB 0x85 (L = 0xFE)
    // RES 7, L       ; 0xCB 0xBD (L = 0x7E)
    let rom = vec![
        0x2E, 0xFF, // LD L, $FF
        0xCB, 0x85, // RES 0, L
        0xCB, 0xBD, // RES 7, L
    ];

    let (cpu, _) = execute_rom(&rom, 3);

    assert_eq!(cpu.l, 0x7E); // Bits 0 and 7 cleared
}

#[test]
fn test_res_all_bits() {
    // Clear all 8 bits starting from 0xFF
    // LD A, $FF      ; 0x3E 0xFF
    // RES 0, A       ; 0xCB 0x87
    // RES 1, A       ; 0xCB 0x8F
    // RES 2, A       ; 0xCB 0x97
    // RES 3, A       ; 0xCB 0x9F
    // RES 4, A       ; 0xCB 0xA7
    // RES 5, A       ; 0xCB 0xAF
    // RES 6, A       ; 0xCB 0xB7
    // RES 7, A       ; 0xCB 0xBF
    let rom = vec![
        0x3E, 0xFF, // LD A, $FF
        0xCB, 0x87, // RES 0, A
        0xCB, 0x8F, // RES 1, A
        0xCB, 0x97, // RES 2, A
        0xCB, 0x9F, // RES 3, A
        0xCB, 0xA7, // RES 4, A
        0xCB, 0xAF, // RES 5, A
        0xCB, 0xB7, // RES 6, A
        0xCB, 0xBF, // RES 7, A
    ];

    let (cpu, _) = execute_rom(&rom, 9);

    assert_eq!(cpu.a, 0x00); // All bits cleared
}

#[test]
fn test_res_hl_indirect() {
    // Mini ROM:
    // LD HL, $3000   ; 0x21 0x00 0x30
    // LD (HL), $FF   ; 0x36 0xFF
    // RES 2, (HL)    ; 0xCB 0x96
    // RES 6, (HL)    ; 0xCB 0xB6
    let rom = vec![
        0x21, 0x00, 0x30, // LD HL, $3000
        0x36, 0xFF, // LD (HL), $FF
        0xCB, 0x96, // RES 2, (HL)
        0xCB, 0xB6, // RES 6, (HL)
    ];

    let (cpu, mut ram) = execute_rom(&rom, 4);

    assert_eq!(cpu.h, 0x30);
    assert_eq!(cpu.l, 0x00);
    assert_eq!(ram.read(0x3000), 0xBB); // Bits 2 and 6 cleared (0b10111011)
}

#[test]
fn test_res_idempotent() {
    // RES should not affect already-cleared bits
    // LD B, $00      ; 0x06 0x00
    // RES 3, B       ; 0xCB 0x98
    let rom = vec![
        0x06, 0x00, // LD B, $00
        0xCB, 0x98, // RES 3, B
    ];

    let (cpu, _) = execute_rom(&rom, 2);

    assert_eq!(cpu.b, 0x00); // No change
}

// ============================================================================
// COMBINED TESTS
// ============================================================================

#[test]
fn test_bit_set_res_complex_sequence() {
    // Complex bit manipulation sequence
    // LD C, $AA      ; 0x0E 0xAA (0b10101010)
    // BIT 0, C       ; 0xCB 0x41 (Z=1, bit is clear)
    // SET 0, C       ; 0xCB 0xC1 (C = 0xAB)
    // BIT 0, C       ; 0xCB 0x41 (Z=0, bit is set)
    // RES 1, C       ; 0xCB 0x89 (C = 0xA9)
    // RES 7, C       ; 0xCB 0xB9 (C = 0x29)
    // BIT 7, C       ; 0xCB 0x79 (Z=1, bit is clear)
    let rom = vec![
        0x0E, 0xAA, // LD C, $AA
        0xCB, 0x41, // BIT 0, C
        0xCB, 0xC1, // SET 0, C
        0xCB, 0x41, // BIT 0, C
        0xCB, 0x89, // RES 1, C
        0xCB, 0xB9, // RES 7, C
        0xCB, 0x79, // BIT 7, C
    ];

    let (cpu, _) = execute_rom(&rom, 7);

    // After final BIT 7, C
    assert_eq!(cpu.c, 0x29); // 0b00101001
    assert!(cpu.flag_z()); // Bit 7 is clear
    assert!(cpu.flag_h());
    assert!(!cpu.flag_n());
}

#[test]
fn test_bit_mask_creation() {
    // Create a bit mask by setting specific bits
    // LD D, $00      ; 0x16 0x00
    // SET 0, D       ; 0xCB 0xC2
    // SET 2, D       ; 0xCB 0xD2
    // SET 4, D       ; 0xCB 0xE2
    // SET 6, D       ; 0xCB 0xF2
    // Result should be 0x55 (0b01010101)
    let rom = vec![
        0x16, 0x00, // LD D, $00
        0xCB, 0xC2, // SET 0, D
        0xCB, 0xD2, // SET 2, D
        0xCB, 0xE2, // SET 4, D
        0xCB, 0xF2, // SET 6, D
    ];

    let (cpu, _) = execute_rom(&rom, 5);

    assert_eq!(cpu.d, 0x55);
}

#[test]
fn test_flags_preserved_across_set_res() {
    // SET and RES should NOT affect flags
    // LD A, $00      ; 0x3E 0x00
    // XOR A          ; 0xAF (sets Z flag)
    // SET 3, A       ; 0xCB 0xDF
    // (Z flag should still be set from XOR)
    let rom = vec![
        0x3E, 0x00, // LD A, $00
        0xAF, // XOR A
        0xCB, 0xDF, // SET 3, A
    ];

    let (cpu, _) = execute_rom(&rom, 3);

    assert_eq!(cpu.a, 0x08); // Bit 3 set
    assert!(cpu.flag_z()); // Z flag preserved from XOR
}

#[test]
fn test_alternating_bits() {
    // Create alternating bit pattern 0xAA using SET
    // LD E, $00      ; 0x1E 0x00
    // SET 1, E       ; 0xCB 0xCB
    // SET 3, E       ; 0xCB 0xDB
    // SET 5, E       ; 0xCB 0xEB
    // SET 7, E       ; 0xCB 0xFB
    let rom = vec![
        0x1E, 0x00, // LD E, $00
        0xCB, 0xCB, // SET 1, E
        0xCB, 0xDB, // SET 3, E
        0xCB, 0xEB, // SET 5, E
        0xCB, 0xFB, // SET 7, E
    ];

    let (cpu, _) = execute_rom(&rom, 5);

    assert_eq!(cpu.e, 0xAA); // 0b10101010
}
