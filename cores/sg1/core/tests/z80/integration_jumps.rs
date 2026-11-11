//! Z80 Jump and Call Instructions - Integration Tests

use super::common::*;

// ========================================
// JP nn (Unconditional jump)
// ========================================

#[test]
fn test_jp_nn_integration() {
    let rom = [0xC3, 0x00, 0x20]; // JP 0x2000
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x2000, "PC should jump to 0x2000");
}

// ========================================
// JP cc, nn (Conditional jumps)
// ========================================

#[test]
fn test_jp_z_taken() {
    let rom = [0xCA, 0x00, 0x30]; // JP Z, 0x3000
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_flag_z(true);

    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x3000, "Jump should be taken when Z=1");
}

#[test]
fn test_jp_z_not_taken() {
    let rom = [0xCA, 0x00, 0x30]; // JP Z, 0x3000
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_flag_z(false);

    cpu.step(&mut ram);

    assert_eq!(
        cpu.pc, 0x0003,
        "Jump not taken, PC advances past instruction"
    );
}

#[test]
fn test_jp_nz_integration() {
    let rom = [0xC2, 0x00, 0x40]; // JP NZ, 0x4000
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_flag_z(false);

    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x4000);
}

#[test]
fn test_jp_c_integration() {
    let rom = [0xDA, 0x00, 0x50]; // JP C, 0x5000
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_flag_c(true);

    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x5000);
}

// ========================================
// JR d (Relative jump)
// ========================================

#[test]
fn test_jr_forward() {
    let rom = [0x18, 0x05]; // JR +5
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.step(&mut ram);

    // PC starts at 0, after JR: PC = 0 + 2 (instruction size) + 5 (displacement) = 7
    assert_eq!(cpu.pc, 0x0007);
}

#[test]
fn test_jr_backward() {
    let rom = [
        0x00, 0x00, 0x00, 0x00, 0x00, // Padding
        0x18, 0xFB, // JR -5 (0xFB = -5 in two's complement)
    ];
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.pc = 0x0005;

    cpu.step(&mut ram);

    // PC = 0x0005 + 2 + (-5) = 0x0002
    assert_eq!(cpu.pc, 0x0002);
}

// ========================================
// JR cc, d (Conditional relative jump)
// ========================================

#[test]
fn test_jr_z_taken() {
    let rom = [0x28, 0x10]; // JR Z, +16
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_flag_z(true);

    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x0012); // 0 + 2 + 16
}

#[test]
fn test_jr_nz_not_taken() {
    let rom = [0x20, 0x10]; // JR NZ, +16
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_flag_z(true); // Z=1, so NZ is false

    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x0002, "Jump not taken");
}

// ========================================
// CALL nn
// ========================================

#[test]
fn test_call_nn_integration() {
    let rom = [0xCD, 0x00, 0x30]; // CALL 0x3000
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFE;

    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x3000, "PC should jump to subroutine");
    assert_eq!(cpu.sp, 0xFFFC, "SP should be decremented by 2");

    // Check return address on stack (0x0003 = address after CALL)
    let ret_low = ram.read(cpu.sp as u32);
    let ret_high = ram.read((cpu.sp as u32) + 1);
    let return_addr = ((ret_high as u16) << 8) | (ret_low as u16);
    assert_eq!(
        return_addr, 0x0003,
        "Return address should be pushed to stack"
    );
}

// ========================================
// RET
// ========================================

#[test]
fn test_ret_integration() {
    let rom = [0xC9]; // RET
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFC;

    // Set up return address on stack (0x0400)
    ram.write(0xFFFC, 0x00); // Low byte
    ram.write(0xFFFD, 0x04); // High byte

    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x0400, "PC should return to stacked address");
    assert_eq!(cpu.sp, 0xFFFE, "SP should be restored");
}

// ========================================
// CALL and RET sequence
// ========================================

#[test]
fn test_call_ret_sequence() {
    // Main: CALL sub; (next instruction)
    // Sub:  RET
    let rom = [
        0xCD, 0x06, 0x00, // @0x0000: CALL 0x0006
        0x00, 0x00, 0x00, // @0x0003: (padding, next instruction would be here)
        0xC9, // @0x0006: RET
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFE;
    cpu.pc = 0x0000;

    // Execute CALL
    cpu.step(&mut ram);
    assert_eq!(cpu.pc, 0x0006, "After CALL, PC at subroutine");
    assert_eq!(cpu.sp, 0xFFFC, "SP decremented");

    // Execute RET
    cpu.step(&mut ram);
    assert_eq!(cpu.pc, 0x0003, "After RET, PC at return address");
    assert_eq!(cpu.sp, 0xFFFE, "SP restored");
}

// ========================================
// Conditional CALL
// ========================================

#[test]
fn test_call_z_taken() {
    let rom = [0xCC, 0x00, 0x20]; // CALL Z, 0x2000
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFE;
    cpu.set_flag_z(true);

    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x2000);
    assert_eq!(cpu.sp, 0xFFFC, "SP decremented because call was taken");
}

#[test]
fn test_call_nz_not_taken() {
    let rom = [0xC4, 0x00, 0x20]; // CALL NZ, 0x2000
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFE;
    cpu.set_flag_z(true); // Z=1, so NZ is false

    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x0003, "Call not taken");
    assert_eq!(cpu.sp, 0xFFFE, "SP unchanged");
}

// ========================================
// Conditional RET
// ========================================

#[test]
fn test_ret_z_taken() {
    let rom = [0xC8]; // RET Z
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFC;
    cpu.set_flag_z(true);
    ram.write(0xFFFC, 0x00);
    ram.write(0xFFFD, 0x05);

    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x0500, "RET taken");
    assert_eq!(cpu.sp, 0xFFFE);
}

#[test]
fn test_ret_nz_not_taken() {
    let rom = [0xC0]; // RET NZ
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.sp = 0xFFFC;
    cpu.set_flag_z(true); // Z=1, so NZ is false

    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x0001, "RET not taken, PC advances");
    assert_eq!(cpu.sp, 0xFFFC, "SP unchanged");
}

// ========================================
// Complex control flow
// ========================================

#[test]
fn test_loop_simulation() {
    // Simple loop: counter in B, decrement and jump if not zero
    // LD B, 3; loop: DEC B; JR NZ, loop; (end)
    let rom = [
        0x06, 0x03, // @0x0000: LD B, 3
        0x05, // @0x0002: DEC B (loop label)
        0x20, 0xFD, // @0x0003: JR NZ, -3 (to 0x0002)
        0x00, // @0x0005: (end)
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    // Execute LD B, 3
    cpu.step(&mut ram);
    assert_eq!(cpu.b, 3);
    assert_eq!(cpu.pc, 0x0002);

    // Loop iteration 1
    cpu.step(&mut ram); // DEC B -> B=2
    assert_eq!(cpu.b, 2);
    cpu.step(&mut ram); // JR NZ (taken)
    assert_eq!(cpu.pc, 0x0002);

    // Loop iteration 2
    cpu.step(&mut ram); // DEC B -> B=1
    assert_eq!(cpu.b, 1);
    cpu.step(&mut ram); // JR NZ (taken)
    assert_eq!(cpu.pc, 0x0002);

    // Loop iteration 3
    cpu.step(&mut ram); // DEC B -> B=0
    assert_eq!(cpu.b, 0);
    assert!(cpu.flag_z());
    cpu.step(&mut ram); // JR NZ (not taken)
    assert_eq!(cpu.pc, 0x0005, "Loop exits");
}
