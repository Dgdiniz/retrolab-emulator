//! Z80 Exception and Interrupt Handling - Integration Tests

use super::common::*;

// ========================================
// Interrupt Mode 1 (IM 1) Tests
// ========================================

#[test]
fn test_im1_interrupt_handler() {
    // Setup: IM 1, EI, then trigger interrupt
    let rom = [
        0xED, 0x56, // IM 1
        0xFB, // EI
        0x00, // NOP (wait for interrupt)
        0x00, // NOP
              // Interrupt handler at 0x0038 (Mode 1 vector)
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    // Setup interrupt handler at 0x0038
    ram.write(0x0038, 0xED); // RETI
    ram.write(0x0039, 0x4D);

    cpu.sp = 0x8000;

    // Execute IM 1
    cpu.step(&mut ram);
    assert_eq!(cpu.im, 1, "Should be in Mode 1");

    // Execute EI
    cpu.step(&mut ram);
    assert!(cpu.iff1, "Interrupts should be enabled");

    // Execute NOP
    cpu.step(&mut ram);
    let pc_before_int = cpu.pc; // PC after NOP execution

    // Trigger interrupt
    let (processed, cycles) = cpu.check_interrupts(&mut ram, true, false);

    assert!(processed, "Interrupt should be processed");
    assert_eq!(cycles, 13, "Mode 1 interrupt takes 13 cycles");
    assert_eq!(cpu.pc, 0x0038, "PC should be at Mode 1 vector");
    assert!(!cpu.iff1, "Interrupts should be disabled during handler");

    // Stack should have return address (PC after the NOP)
    assert_eq!(cpu.sp, 0x7FFE, "SP should be decremented");
    let ret_low = ram.read(0x7FFE);
    let ret_high = ram.read(0x7FFF);
    let ret_addr = ((ret_high as u16) << 8) | (ret_low as u16);
    assert_eq!(ret_addr, pc_before_int, "Return address should be saved");
}

#[test]
fn test_im1_interrupt_disabled() {
    let rom = [
        0xED, 0x56, // IM 1
        0xF3, // DI
        0x00, // NOP
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    // Execute IM 1 and DI
    execute_n_instructions(&mut cpu, &mut ram, 2);

    assert!(!cpu.iff1, "Interrupts should be disabled");

    // Try to trigger interrupt
    let (processed, _) = cpu.check_interrupts(&mut ram, true, false);

    assert!(
        !processed,
        "Interrupt should not be processed when disabled"
    );
}

// ========================================
// Interrupt Mode 2 (IM 2) Tests
// ========================================

#[test]
fn test_im2_vectored_interrupt() {
    let rom = [
        0xED, 0x5E, // IM 2
        0xED, 0x47, // LD I, A (set I register)
        0xFB, // EI
        0x00, // NOP
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    // Set I register to 0x80 (vector table at 0x8000)
    cpu.a = 0x80;
    cpu.sp = 0x9000;

    // Setup vector table at 0x8000 pointing to handler at 0x1234
    ram.write(0x8000, 0x34); // Low byte
    ram.write(0x8001, 0x12); // High byte

    // Setup interrupt handler at 0x1234
    ram.write(0x1234, 0xED); // RETI
    ram.write(0x1235, 0x4D);

    // Execute IM 2, LD I,A, EI
    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert_eq!(cpu.im, 2, "Should be in Mode 2");
    assert_eq!(cpu.i, 0x80, "I register should be set");
    assert!(cpu.iff1, "Interrupts should be enabled");

    // Execute NOP
    cpu.step(&mut ram);

    // Trigger Mode 2 interrupt
    let (processed, cycles) = cpu.check_interrupts(&mut ram, true, false);

    assert!(processed, "Interrupt should be processed");
    assert_eq!(cycles, 19, "Mode 2 interrupt takes 19 cycles");
    assert_eq!(cpu.pc, 0x1234, "PC should be at vector address");
}

// ========================================
// Non-Maskable Interrupt (NMI) Tests
// ========================================

#[test]
fn test_nmi_always_processed() {
    let rom = [
        0xF3, // DI (disable maskable interrupts)
        0x00, // NOP
        0x00, // NOP
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.sp = 0x8000;

    // Setup NMI handler at 0x0066
    ram.write(0x0066, 0xED); // RETN
    ram.write(0x0067, 0x45);

    // Execute DI
    cpu.step(&mut ram);
    assert!(!cpu.iff1, "Maskable interrupts disabled");

    // Execute NOP
    let _pc_before = cpu.pc;
    cpu.step(&mut ram);

    // Trigger NMI (should work even with IFF1=false)
    let (processed, cycles) = cpu.check_interrupts(&mut ram, false, true);

    assert!(processed, "NMI should always be processed");
    assert_eq!(cycles, 11, "NMI takes 11 cycles");
    assert_eq!(cpu.pc, 0x0066, "PC should be at NMI vector");
    assert!(!cpu.iff1, "IFF1 should be disabled");
    assert!(!cpu.iff2, "IFF2 should preserve old IFF1 (false)");
}

#[test]
fn test_nmi_preserves_iff1_in_iff2() {
    let rom = [
        0xFB, // EI
        0x00, // NOP
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.sp = 0x8000;

    // Execute EI
    cpu.step(&mut ram);
    assert!(cpu.iff1, "Interrupts enabled");

    // Execute NOP
    cpu.step(&mut ram);

    // Trigger NMI
    cpu.check_interrupts(&mut ram, false, true);

    assert!(!cpu.iff1, "IFF1 should be disabled after NMI");
    assert!(cpu.iff2, "IFF2 should preserve old IFF1 (true)");
}

// ========================================
// RETI and RETN Tests
// ========================================

#[test]
fn test_reti_returns_from_interrupt() {
    let rom = [
        0xED, 0x56, // IM 1
        0xFB, // EI
        0x00, // NOP (at 0x0003)
        0x00, // NOP (at 0x0004)
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.sp = 0x8000;

    // Setup interrupt handler at 0x0038
    ram.write(0x0038, 0xED); // RETI
    ram.write(0x0039, 0x4D);

    // Execute IM 1, EI, NOP
    execute_n_instructions(&mut cpu, &mut ram, 3);

    let pc_after_nop = cpu.pc; // Should be 0x0004

    // Trigger interrupt
    cpu.check_interrupts(&mut ram, true, false);
    assert_eq!(cpu.pc, 0x0038, "At interrupt handler");

    // Execute RETI
    cpu.step(&mut ram);

    assert_eq!(cpu.pc, pc_after_nop, "Should return to after NOP");
    assert_eq!(cpu.sp, 0x8000, "SP should be restored");
}

#[test]
fn test_retn_restores_iff1() {
    let rom = [
        0xFB, // EI
        0x00, // NOP
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.sp = 0x8000;

    // Setup NMI handler at 0x0066
    ram.write(0x0066, 0xED); // RETN
    ram.write(0x0067, 0x45);

    // Execute EI
    cpu.step(&mut ram);
    assert!(cpu.iff1, "Interrupts enabled");

    // Execute NOP
    cpu.step(&mut ram);

    // Trigger NMI
    cpu.check_interrupts(&mut ram, false, true);

    assert!(!cpu.iff1, "IFF1 disabled in NMI");
    assert!(cpu.iff2, "IFF2 preserved old IFF1");

    // Execute RETN
    cpu.step(&mut ram);

    assert!(cpu.iff1, "IFF1 should be restored from IFF2");
}

// ========================================
// HALT and Interrupts Tests
// ========================================

#[test]
fn test_halt_cleared_by_interrupt() {
    let rom = [
        0xED, 0x56, // IM 1
        0xFB, // EI
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.sp = 0x8000;

    // Setup interrupt handler
    ram.write(0x0038, 0xED); // RETI
    ram.write(0x0039, 0x4D);

    // Execute IM 1, EI
    execute_n_instructions(&mut cpu, &mut ram, 2);

    // Execute HALT
    cpu.step(&mut ram);
    assert!(cpu.halted, "CPU should be halted");

    // Trigger interrupt
    let (processed, _) = cpu.check_interrupts(&mut ram, true, false);

    assert!(processed, "Interrupt should be processed");
    assert!(!cpu.halted, "HALT should be cleared");
    assert_eq!(cpu.pc, 0x0038, "Should be at interrupt handler");
}

#[test]
fn test_halt_cleared_by_nmi() {
    let rom = [
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.sp = 0x8000;

    // Setup NMI handler
    ram.write(0x0066, 0xED); // RETN
    ram.write(0x0067, 0x45);

    // Execute HALT
    cpu.step(&mut ram);
    assert!(cpu.halted, "CPU should be halted");

    // Trigger NMI
    let (processed, _) = cpu.check_interrupts(&mut ram, false, true);

    assert!(processed, "NMI should be processed");
    assert!(!cpu.halted, "HALT should be cleared by NMI");
    assert_eq!(cpu.pc, 0x0066, "Should be at NMI handler");
}

// ========================================
// Complex Interrupt Scenarios
// ========================================

#[test]
fn test_nested_interrupts_disabled() {
    // Test that interrupts are disabled during handler
    // preventing re-entry
    let rom = [
        0xED, 0x56, // IM 1
        0xFB, // EI
        0x00, // NOP
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.sp = 0x8000;

    // Setup interrupt handler with RETI
    ram.write(0x0038, 0x00); // NOP
    ram.write(0x0039, 0xED); // RETI
    ram.write(0x003A, 0x4D);

    // Execute IM 1, EI, NOP
    execute_n_instructions(&mut cpu, &mut ram, 3);

    // Trigger first interrupt
    cpu.check_interrupts(&mut ram, true, false);
    assert_eq!(cpu.pc, 0x0038, "At interrupt handler");
    assert!(!cpu.iff1, "Interrupts disabled in handler");

    // Try to trigger another interrupt while in handler
    let (processed, _) = cpu.check_interrupts(&mut ram, true, false);

    assert!(!processed, "Nested interrupt should not be processed");
}

#[test]
fn test_interrupt_priority_nmi_over_maskable() {
    // NMI has higher priority than maskable interrupt
    let rom = [
        0xED, 0x56, // IM 1
        0xFB, // EI
        0x00, // NOP
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.sp = 0x8000;

    // Execute IM 1, EI, NOP
    execute_n_instructions(&mut cpu, &mut ram, 3);

    // Trigger both NMI and maskable interrupt simultaneously
    let (processed, _) = cpu.check_interrupts(&mut ram, true, true);

    assert!(processed, "Interrupt should be processed");
    assert_eq!(cpu.pc, 0x0066, "NMI should take priority");
}

// ========================================
// Mini ROM: Interrupt-Driven Counter
// ========================================

#[test]
fn test_mini_rom_interrupt_counter() {
    // Program that counts interrupts using IM 1
    let rom = [
        // Main program (0x0000)
        0x3E, 0x00, // LD A, 0 (counter = 0)
        0xED, 0x56, // IM 1
        0xFB, // EI
        // Loop (0x0005)
        0x00, // NOP
        0x00, // NOP
        0x00, // NOP
        0x18,
        0xFB, // JR -5 (loop forever)

              // Interrupt handler at 0x0038, written below:
              // INC A
              // EI
              // RETI
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.sp = 0x8000;

    // Setup interrupt handler at 0x0038
    ram.write(0x0038, 0x3C); // INC A
    ram.write(0x0039, 0xFB); // EI (re-enable interrupts)
    ram.write(0x003A, 0xED); // RETI
    ram.write(0x003B, 0x4D);

    // Execute main program setup (LD A, 0; IM 1; EI)
    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert_eq!(cpu.a, 0, "Counter starts at 0");
    assert!(cpu.iff1, "Interrupts enabled");

    // Execute a few NOPs
    execute_n_instructions(&mut cpu, &mut ram, 3);

    // Trigger interrupt 1
    cpu.check_interrupts(&mut ram, true, false);
    cpu.step(&mut ram); // Execute INC A
    cpu.step(&mut ram); // Execute EI
    cpu.step(&mut ram); // Execute RETI

    assert_eq!(cpu.a, 1, "Counter should be 1");
    assert_eq!(cpu.sp, 0x8000, "SP restored");
    assert!(cpu.iff1, "Interrupts re-enabled");

    // Execute more NOPs
    execute_n_instructions(&mut cpu, &mut ram, 3);

    // Trigger interrupt 2
    cpu.check_interrupts(&mut ram, true, false);
    cpu.step(&mut ram); // Execute INC A
    cpu.step(&mut ram); // Execute EI
    cpu.step(&mut ram); // Execute RETI

    assert_eq!(cpu.a, 2, "Counter should be 2");

    // Trigger interrupt 3
    cpu.check_interrupts(&mut ram, true, false);
    cpu.step(&mut ram); // Execute INC A
    cpu.step(&mut ram); // Execute EI
    cpu.step(&mut ram); // Execute RETI

    assert_eq!(cpu.a, 3, "Counter should be 3");
}

// ========================================
// Mini ROM: NMI Handler
// ========================================

#[test]
fn test_mini_rom_nmi_handler() {
    // Program with NMI handler that sets a flag
    let rom = [
        // Main program
        0x3E, 0x00, // LD A, 0 (flag = 0)
        0xFB, // EI
        0x00, // NOP
        0x00, // NOP
        0x76, // HALT (wait for interrupt)

              // NMI handler at 0x0066 would be:
              // LD A, 0xFF
              // RETN
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.sp = 0x8000;

    // Setup NMI handler at 0x0066
    ram.write(0x0066, 0x3E); // LD A, n
    ram.write(0x0067, 0xFF);
    ram.write(0x0068, 0xED); // RETN
    ram.write(0x0069, 0x45);

    // Execute main program (LD A, 0; EI; NOP; NOP; HALT)
    execute_n_instructions(&mut cpu, &mut ram, 5);

    assert_eq!(cpu.a, 0, "Flag should be 0");
    assert!(cpu.halted, "CPU should be halted");
    assert!(cpu.iff1, "Interrupts enabled");

    // Trigger NMI
    cpu.check_interrupts(&mut ram, false, true);

    assert!(!cpu.halted, "HALT cleared");
    assert_eq!(cpu.pc, 0x0066, "At NMI handler");
    assert!(cpu.iff2, "IFF2 preserved IFF1");

    // Execute NMI handler (LD A, 0xFF; RETN)
    cpu.step(&mut ram);
    assert_eq!(cpu.a, 0xFF, "Flag set by NMI handler");

    cpu.step(&mut ram);
    assert!(cpu.iff1, "Interrupts restored by RETN");
    assert_eq!(cpu.sp, 0x8000, "SP restored");
}
