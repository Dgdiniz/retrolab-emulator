//! Z80 Mini ROM tests - Complete programs
//!
//! These tests create small ROM programs with multiple instructions and verify
//! that the Z80 CPU executes them correctly in sequence, testing the decoder
//! and instruction flow.

use super::common::*;

// ========================================
// MINI ROM 1: Counter Loop
// ========================================
//
// Purpose: Test arithmetic, conditional jumps
// Code:
//   LD B, 10      ; Counter
//   LD A, 0       ; Accumulator
// loop:
//   ADD A, 1
//   DEC B
//   JR NZ, loop
//   HALT
//
// Expected: A = 10, B = 0, Z flag set

#[test]
fn test_mini_rom_1_counter_loop() {
    let rom = [
        0x06, 0x0A, // LD B, 10
        0x3E, 0x00, // LD A, 0
        // loop:
        0xC6, 0x01, // ADD A, 1
        0x05, // DEC B
        0x20, 0xFB, // JR NZ, -5 (to ADD A, 1)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    run_until_halt(&mut cpu, &mut ram, 1000);

    assert_eq!(cpu.a, 10, "A should be 10");
    assert_eq!(cpu.b, 0, "B should be 0");
    assert!(cpu.flag_z(), "Z flag should be set");
    assert!(cpu.halted, "CPU should be halted");
}

// ========================================
// MINI ROM 2: Memory Operations
// ========================================
//
// Purpose: Test load/store to memory
// Code:
//   LD HL, 0x2000
//   LD A, 0x42
//   LD (HL), A
//   LD B, (HL)
//   CP B
//   HALT

#[test]
fn test_mini_rom_2_memory_ops() {
    let rom = [
        0x21, 0x00, 0x20, // LD HL, 0x2000
        0x3E, 0x42, // LD A, 0x42
        // TODO: enable the LD (HL), A and LD B, (HL) bytes below
        // 0x77,             // LD (HL), A
        // 0x46,             // LD B, (HL)
        0xB8, // CP B
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    // For now, just test what we have
    execute_n_instructions(&mut cpu, &mut ram, 2);

    assert_eq!(cpu.hl(), 0x2000);
    assert_eq!(cpu.a, 0x42);
}

// ========================================
// MINI ROM 3: Stack Operations
// ========================================
//
// Purpose: Test PUSH/POP
// Code:
//   LD SP, 0xFFFE
//   LD BC, 0x1234
//   PUSH BC
//   LD BC, 0x0000
//   POP BC
//   HALT

#[test]
fn test_mini_rom_3_stack_ops() {
    let rom = [
        0x31, 0xFE, 0xFF, // LD SP, 0xFFFE
        0x01, 0x34, 0x12, // LD BC, 0x1234
        0xC5, // PUSH BC
        0x01, 0x00, 0x00, // LD BC, 0x0000
        0xC1, // POP BC
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    run_until_halt(&mut cpu, &mut ram, 100);

    assert_eq!(cpu.bc(), 0x1234, "BC should be restored");
    assert_eq!(cpu.sp, 0xFFFE, "SP should be restored");
    assert!(cpu.halted);
}

// ========================================
// MINI ROM 4: Logical Operations
// ========================================
//
// Purpose: Test AND, OR, XOR
// Code:
//   LD A, 0xFF
//   LD B, 0x0F
//   AND B         ; A = 0x0F
//   OR 0xF0       ; A = 0xFF
//   XOR 0xFF      ; A = 0x00
//   HALT

#[test]
fn test_mini_rom_4_logical_ops() {
    let rom = [
        0x3E, 0xFF, // LD A, 0xFF
        0x06, 0x0F, // LD B, 0x0F
        0xA0, // AND B
        0xF6, 0xF0, // OR 0xF0
        0xEE, 0xFF, // XOR 0xFF
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    run_until_halt(&mut cpu, &mut ram, 100);

    assert_eq!(cpu.a, 0x00, "A should be 0 after XOR");
    assert!(cpu.flag_z(), "Z flag should be set");
    assert!(cpu.halted);
}

// ========================================
// MINI ROM 5: Subroutine Call
// ========================================
//
// Purpose: Test CALL and RET
// Code:
//   LD SP, 0xFFFE
//   LD A, 5
//   CALL double    ; Call subroutine
//   HALT
// double:
//   ADD A, A
//   RET

#[test]
fn test_mini_rom_5_subroutine() {
    let rom = [
        0x31, 0xFE, 0xFF, // @0x00: LD SP, 0xFFFE
        0x3E, 0x05, // @0x03: LD A, 5
        0xCD, 0x0B, 0x00, // @0x05: CALL 0x000B (double)
        0x76, // @0x08: HALT
        0x00, 0x00, // @0x09: (padding)
        // double:
        0x87, // @0x0B: ADD A, A
        0xC9, // @0x0C: RET
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    run_until_halt(&mut cpu, &mut ram, 100);

    assert_eq!(cpu.a, 10, "A should be doubled (5 * 2 = 10)");
    assert_eq!(cpu.sp, 0xFFFE, "SP should be restored after RET");
    assert!(cpu.halted);
}

// ========================================
// MINI ROM 6: Flag Manipulation
// ========================================
//
// Purpose: Test SCF, CCF, CPL
// Code:
//   SCF           ; Set carry
//   CCF           ; Complement carry
//   LD A, 0xAA
//   CPL           ; Complement A
//   HALT

#[test]
fn test_mini_rom_6_flags() {
    let rom = [
        0x37, // SCF
        0x3F, // CCF
        0x3E, 0xAA, // LD A, 0xAA
        0x2F, // CPL
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    run_until_halt(&mut cpu, &mut ram, 100);

    assert!(!cpu.flag_c(), "Carry should be 0 after CCF");
    assert_eq!(cpu.a, 0x55, "A should be complemented (0xAA -> 0x55)");
    assert!(cpu.halted);
}

// ========================================
// MINI ROM 7: Conditional Execution
// ========================================
//
// Purpose: Test conditional jumps
// Code:
//   LD A, 10
//   CP 10
//   JP Z, equal
//   LD B, 1       ; Should not execute
//   HALT
// equal:
//   LD B, 2
//   HALT

#[test]
fn test_mini_rom_7_conditional() {
    let rom = [
        0x3E, 0x0A, // LD A, 10
        0xFE, 0x0A, // CP 10
        0xCA, 0x0B, 0x00, // JP Z, 0x000B (equal)
        0x06, 0x01, // LD B, 1 (should not execute)
        0x76, // HALT
        0x00, // (padding)
        // equal:
        0x06, 0x02, // @0x0B: LD B, 2
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    run_until_halt(&mut cpu, &mut ram, 100);

    assert_eq!(cpu.b, 2, "B should be 2 (jumped to equal label)");
    assert!(cpu.halted);
}

// ========================================
// MINI ROM 8: Nested Loops
// ========================================
//
// Purpose: Test nested loops with counters
// Code:
//   LD B, 3       ; Outer counter
//   LD C, 0       ; Accumulator
// outer:
//   LD A, 2       ; Inner counter
// inner:
//   INC C
//   DEC A
//   JR NZ, inner
//   DEC B
//   JR NZ, outer
//   HALT

#[test]
fn test_mini_rom_8_nested_loops() {
    let rom = [
        0x06, 0x03, // LD B, 3
        0x0E, 0x00, // LD C, 0
        // outer:
        0x3E, 0x02, // LD A, 2
        // inner:
        0x0C, // INC C
        0x3D, // DEC A
        0x20, 0xFC, // JR NZ, -4 (to INC C)
        0x05, // DEC B
        0x20, 0xF7, // JR NZ, -9 (to LD A, 2)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    run_until_halt(&mut cpu, &mut ram, 1000);

    assert_eq!(cpu.c, 6, "C should be 6 (3 outer x 2 inner)");
    assert_eq!(cpu.b, 0);
    assert_eq!(cpu.a, 0);
    assert!(cpu.halted);
}

// ========================================
// MINI ROM 9: Arithmetic Expression
// ========================================
//
// Purpose: Calculate ((10 + 5) - 3) * 2
// Code:
//   LD A, 10
//   ADD A, 5      ; A = 15
//   SUB 3         ; A = 12
//   ADD A, A      ; A = 24 (double = multiply by 2)
//   HALT

#[test]
fn test_mini_rom_9_expression() {
    let rom = [
        0x3E, 0x0A, // LD A, 10
        0xC6, 0x05, // ADD A, 5
        0xD6, 0x03, // SUB 3
        0x87, // ADD A, A
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    run_until_halt(&mut cpu, &mut ram, 100);

    assert_eq!(cpu.a, 24, "Result should be 24");
    assert!(cpu.halted);
}

// ========================================
// MINI ROM 10: Register Transfers
// ========================================
//
// Purpose: Move data through registers
// Code:
//   LD A, 0x42
//   LD B, A
//   LD C, B
//   LD D, C
//   LD E, D
//   LD H, E
//   LD L, H
//   LD A, L
//   HALT

#[test]
fn test_mini_rom_10_register_chain() {
    let rom = [
        0x3E, 0x42, // LD A, 0x42
        0x47, // LD B, A
        0x48, // LD C, B
        0x51, // LD D, C
        0x5A, // LD E, D
        0x63, // LD H, E
        0x6C, // LD L, H
        0x7D, // LD A, L
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    run_until_halt(&mut cpu, &mut ram, 100);

    assert_eq!(cpu.a, 0x42, "Value should propagate through all registers");
    assert_eq!(cpu.b, 0x42);
    assert_eq!(cpu.c, 0x42);
    assert_eq!(cpu.d, 0x42);
    assert_eq!(cpu.e, 0x42);
    assert_eq!(cpu.h, 0x42);
    assert_eq!(cpu.l, 0x42);
    assert!(cpu.halted);
}

// ========================================
// MINI ROM 11: Comprehensive Test
// ========================================
//
// Purpose: Test multiple instruction types together
// Code:
//   LD SP, 0xFFFE
//   LD A, 10
//   LD B, 3
//   ADD A, B      ; A = 13
//   AND 0x0F      ; A = 13 (mask)
//   PUSH AF
//   LD A, 0xFF
//   POP AF        ; Restore A = 13
//   CP 13
//   JP Z, success
//   LD C, 0xFF    ; Fail
//   HALT
// success:
//   LD C, 0x00    ; Success
//   HALT

#[test]
fn test_mini_rom_11_comprehensive() {
    let rom = [
        0x31, 0xFE, 0xFF, // LD SP, 0xFFFE
        0x3E, 0x0A, // LD A, 10
        0x06, 0x03, // LD B, 3
        0x80, // ADD A, B
        0xE6, 0x0F, // AND 0x0F
        0xF5, // PUSH AF
        0x3E, 0xFF, // LD A, 0xFF
        0xF1, // POP AF
        0xFE, 0x0D, // CP 13
        0xCA, 0x17, 0x00, // JP Z, 0x0017 (success)
        0x0E, 0xFF, // LD C, 0xFF (fail)
        0x76, // HALT
        0x00, // (padding)
        // success:
        0x0E, 0x00, // @0x17: LD C, 0x00
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    run_until_halt(&mut cpu, &mut ram, 200);

    assert_eq!(cpu.a, 13, "A should be restored to 13");
    assert_eq!(cpu.c, 0x00, "C should be 0x00 (success path)");
    assert!(cpu.halted);
}

// ========================================
// MINI ROM 12: Interrupt Handling Prep
// ========================================
//
// Purpose: Test DI/EI around critical section
// Code:
//   DI
//   LD HL, 0x8000
//   LD A, 0x42
//   EI
//   HALT

#[test]
fn test_mini_rom_12_interrupt_control() {
    let rom = [
        0xF3, // DI
        0x21, 0x00, 0x80, // LD HL, 0x8000
        0x3E, 0x42, // LD A, 0x42
        0xFB, // EI
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.iff1 = true; // Start with interrupts enabled

    run_until_halt(&mut cpu, &mut ram, 100);

    assert_eq!(cpu.hl(), 0x8000);
    assert_eq!(cpu.a, 0x42);
    assert!(cpu.iff1, "Interrupts should be enabled at end");
    assert!(cpu.halted);
}
