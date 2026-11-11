//! Z80 Miscellaneous Instructions - Integration Tests

use super::common::*;

// ========================================
// NOP
// ========================================

#[test]
fn test_nop_integration() {
    let rom = [0x00]; // NOP
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    let pc_before = cpu.pc;

    cpu.step(&mut ram);

    assert_eq!(cpu.pc, pc_before + 1, "PC should advance by 1");
}

#[test]
fn test_nop_sequence() {
    // Multiple NOPs
    let rom = [0x00, 0x00, 0x00, 0x00, 0x00]; // 5x NOP
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    execute_n_instructions(&mut cpu, &mut ram, 5);

    assert_eq!(cpu.pc, 0x0005);
}

// ========================================
// HALT
// ========================================

#[test]
fn test_halt_integration() {
    let rom = [0x76]; // HALT
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    assert!(!cpu.halted, "CPU should not be halted initially");

    cpu.step(&mut ram);

    assert!(cpu.halted, "CPU should be halted after HALT instruction");
    assert_eq!(cpu.pc, 0x0001, "PC advances");
}

#[test]
fn test_halt_stops_execution() {
    let rom = [
        0x76, // HALT
        0x3E, 0xFF, // LD A, 0xFF (should not execute)
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x00;

    // Execute HALT
    cpu.step(&mut ram);
    assert!(cpu.halted);

    // Try to execute next instruction (should remain halted)
    cpu.step(&mut ram);
    assert!(cpu.halted, "CPU should remain halted");
    assert_eq!(cpu.a, 0x00, "A should not change");
    assert_eq!(cpu.pc, 0x0001, "PC should not advance when halted");
}

// ========================================
// DI (Disable Interrupts)
// ========================================

#[test]
fn test_di_integration() {
    let rom = [0xF3]; // DI
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.iff1 = true;
    cpu.iff2 = true;

    cpu.step(&mut ram);

    assert!(!cpu.iff1, "IFF1 should be cleared");
    assert!(!cpu.iff2, "IFF2 should be cleared");
}

// ========================================
// EI (Enable Interrupts)
// ========================================

#[test]
fn test_ei_integration() {
    let rom = [0xFB]; // EI
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.iff1 = false;
    cpu.iff2 = false;

    cpu.step(&mut ram);

    assert!(cpu.iff1, "IFF1 should be set");
    assert!(cpu.iff2, "IFF2 should be set");
}

#[test]
fn test_di_ei_sequence() {
    // DI; EI
    let rom = [0xF3, 0xFB]; // DI; EI
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.iff1 = true;

    // Execute DI
    cpu.step(&mut ram);
    assert!(!cpu.iff1);

    // Execute EI
    cpu.step(&mut ram);
    assert!(cpu.iff1, "Interrupts should be re-enabled");
}

// ========================================
// SCF (Set Carry Flag)
// ========================================

#[test]
fn test_scf_integration() {
    let rom = [0x37]; // SCF
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_flag_c(false);
    cpu.set_flag_h(true);
    cpu.set_flag_n(true);

    cpu.step(&mut ram);

    assert!(cpu.flag_c(), "C flag should be set");
    assert!(!cpu.flag_h(), "H flag should be cleared");
    assert!(!cpu.flag_n(), "N flag should be cleared");
}

// ========================================
// CCF (Complement Carry Flag)
// ========================================

#[test]
fn test_ccf_set_to_clear() {
    let rom = [0x3F]; // CCF
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_flag_c(true);
    cpu.set_flag_h(false);

    cpu.step(&mut ram);

    assert!(!cpu.flag_c(), "C flag should be complemented (1->0)");
    assert!(cpu.flag_h(), "H = previous C");
}

#[test]
fn test_ccf_clear_to_set() {
    let rom = [0x3F]; // CCF
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_flag_c(false);

    cpu.step(&mut ram);

    assert!(cpu.flag_c(), "C flag should be complemented (0->1)");
}

#[test]
fn test_ccf_twice() {
    // CCF; CCF (should restore original C)
    let rom = [0x3F, 0x3F]; // CCF; CCF
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_flag_c(true);

    execute_n_instructions(&mut cpu, &mut ram, 2);

    assert!(cpu.flag_c(), "C should be restored after two CCFs");
}

// ========================================
// CPL (Complement A)
// ========================================

#[test]
fn test_cpl_integration() {
    let rom = [0x2F]; // CPL
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0b10101010;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0b01010101, "A should be bitwise inverted");
    assert!(cpu.flag_h(), "H flag should be set");
    assert!(cpu.flag_n(), "N flag should be set");
}

#[test]
fn test_cpl_twice() {
    // CPL; CPL (should restore original A)
    let rom = [0x2F, 0x2F]; // CPL; CPL
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x42;

    execute_n_instructions(&mut cpu, &mut ram, 2);

    assert_eq!(cpu.a, 0x42, "Double complement restores original value");
}

// ========================================
// NEG (Negate A)
// ========================================

#[test]
fn test_neg_integration() {
    let rom = [0xED, 0x44]; // NEG
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x10;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0xF0, "0 - 0x10 = -16 = 0xF0");
    assert!(cpu.flag_s(), "S flag set (negative)");
    assert!(cpu.flag_n(), "N flag set (subtraction)");
    assert!(cpu.flag_c(), "C flag set (borrow)");
}

#[test]
fn test_neg_zero() {
    let rom = [0xED, 0x44]; // NEG
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x00;

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x00, "0 - 0 = 0");
    assert!(cpu.flag_z(), "Z flag set");
    assert!(!cpu.flag_c(), "No borrow when negating zero");
}

#[test]
fn test_neg_overflow() {
    let rom = [0xED, 0x44]; // NEG
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x80; // -128

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x80, "Negating -128 overflows to -128");
    assert!(cpu.flag_pv(), "PV flag set for overflow");
}

// ========================================
// DAA (Decimal Adjust Accumulator)
// ========================================

#[test]
fn test_daa_bcd_addition() {
    let rom = [0x27]; // DAA
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    // Simulate: 15 + 27 = 42 in BCD
    // In binary: 0x15 + 0x27 = 0x3C
    cpu.a = 0x3C;
    cpu.set_flag_n(false); // Addition
    cpu.set_flag_h(false);
    cpu.set_flag_c(false);

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x42, "DAA adjusts 0x3C to BCD 0x42");
}

#[test]
fn test_daa_no_adjustment_needed() {
    let rom = [0x27]; // DAA
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.a = 0x42; // Already valid BCD
    cpu.set_flag_n(false);
    cpu.set_flag_h(false);
    cpu.set_flag_c(false);

    cpu.step(&mut ram);

    assert_eq!(cpu.a, 0x42, "Valid BCD requires no adjustment");
}

// ========================================
// Complex misc sequences
// ========================================

#[test]
fn test_flag_manipulation_sequence() {
    // SCF; CCF; SCF
    let rom = [0x37, 0x3F, 0x37]; // SCF; CCF; SCF
    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.set_flag_c(false);

    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert!(cpu.flag_c(), "Final C flag should be set");
}

#[test]
fn test_critical_section() {
    // DI; (critical code); EI
    let rom = [
        0xF3, // DI
        0x3E, 0x42, // LD A, 0x42 (critical section)
        0xFB, // EI
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    cpu.iff1 = true;

    // Execute DI
    cpu.step(&mut ram);
    assert!(!cpu.iff1, "Interrupts disabled");

    // Execute critical section
    cpu.step(&mut ram);
    assert_eq!(cpu.a, 0x42);
    assert!(!cpu.iff1, "Still disabled");

    // Execute EI
    cpu.step(&mut ram);
    assert!(cpu.iff1, "Interrupts re-enabled");
}

// ========================================
// DJNZ (Decrement B and Jump if Not Zero)
// ========================================

#[test]
fn test_djnz_loop_integration() {
    // Loop 5 times using DJNZ
    let rom = [
        0x06, 0x05, // LD B, 5
        0x00, // NOP (loop body)
        0x10, 0xFD, // DJNZ -3 (jump back to NOP)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    // Execute LD B, 5
    cpu.step(&mut ram);
    assert_eq!(cpu.b, 5);

    // Execute loop 5 times
    for i in (1..=5).rev() {
        let pc_before = cpu.pc;
        cpu.step(&mut ram); // Execute NOP
        cpu.step(&mut ram); // Execute DJNZ
        assert_eq!(cpu.b, i - 1, "B should decrement");
        if i > 1 {
            assert_eq!(cpu.pc, pc_before, "Should jump back");
        } else {
            assert_eq!(cpu.pc, 0x0005, "Should exit loop");
        }
    }
}

#[test]
fn test_djnz_no_loop() {
    let rom = [
        0x06, 0x01, // LD B, 1
        0x10, 0x00, // DJNZ +0 (taken or not, it goes on to 0x0004)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    cpu.step(&mut ram); // LD B, 1
    cpu.step(&mut ram); // DJNZ

    assert_eq!(cpu.b, 0, "B should be 0");
    assert_eq!(cpu.pc, 0x0004, "Should continue after DJNZ");
}

// ========================================
// INC/DEC (HL)
// ========================================

#[test]
fn test_inc_hl_indirect_integration() {
    let rom = [
        0x21, 0x00, 0x80, // LD HL, 0x8000
        0x34, // INC (HL)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    ram.write(0x8000, 0x7F);

    execute_n_instructions(&mut cpu, &mut ram, 2);

    assert_eq!(
        ram.read(0x8000),
        0x80,
        "Memory at (HL) should be incremented"
    );
    assert!(cpu.flag_pv(), "Overflow flag should be set");
}

#[test]
fn test_dec_hl_indirect_integration() {
    let rom = [
        0x21, 0x00, 0x80, // LD HL, 0x8000
        0x35, // DEC (HL)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    ram.write(0x8000, 0x80);

    execute_n_instructions(&mut cpu, &mut ram, 2);

    assert_eq!(
        ram.read(0x8000),
        0x7F,
        "Memory at (HL) should be decremented"
    );
    assert!(cpu.flag_pv(), "Overflow flag should be set");
}

#[test]
fn test_inc_dec_hl_sequence() {
    // INC (HL); DEC (HL) should restore original value
    let rom = [
        0x21, 0x00, 0x80, // LD HL, 0x8000
        0x34, // INC (HL)
        0x35, // DEC (HL)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    ram.write(0x8000, 0x42);

    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert_eq!(ram.read(0x8000), 0x42, "Value should be restored");
}

// ========================================
// Exchange Instructions
// ========================================

#[test]
fn test_ex_de_hl_integration() {
    let rom = [
        0x21, 0x34, 0x12, // LD HL, 0x1234
        0x11, 0x78, 0x56, // LD DE, 0x5678
        0xEB, // EX DE, HL
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert_eq!(cpu.h, 0x56);
    assert_eq!(cpu.l, 0x78);
    assert_eq!(cpu.d, 0x12);
    assert_eq!(cpu.e, 0x34);
}

#[test]
fn test_ex_af_af_prime_integration() {
    let rom = [
        0x3E, 0x42, // LD A, 0x42
        0x37, // SCF (set carry flag)
        0x08, // EX AF, AF'
        0x3E, 0x24, // LD A, 0x24
        0x08, // EX AF, AF' (restore)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    execute_n_instructions(&mut cpu, &mut ram, 5);

    assert_eq!(cpu.a, 0x42, "A should be restored");
    assert!(cpu.flag_c(), "Carry flag should be restored");
}

#[test]
fn test_exx_integration() {
    let rom = [
        0x01, 0x34, 0x12, // LD BC, 0x1234
        0x11, 0x78, 0x56, // LD DE, 0x5678
        0x21, 0xBC, 0x9A, // LD HL, 0x9ABC
        0xD9, // EXX
        0x01, 0x00, 0x00, // LD BC, 0x0000
        0xD9, // EXX (restore)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    execute_n_instructions(&mut cpu, &mut ram, 6);

    assert_eq!(cpu.b, 0x12);
    assert_eq!(cpu.c, 0x34);
    assert_eq!(cpu.d, 0x56);
    assert_eq!(cpu.e, 0x78);
    assert_eq!(cpu.h, 0x9A);
    assert_eq!(cpu.l, 0xBC);
}

#[test]
fn test_ex_sp_hl_integration() {
    let rom = [
        0x21, 0x34, 0x12, // LD HL, 0x1234
        0x31, 0x00, 0x80, // LD SP, 0x8000
        0xE3, // EX (SP), HL
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    ram.write(0x8000, 0x78);
    ram.write(0x8001, 0x56);

    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert_eq!(cpu.h, 0x56, "H = (SP+1)");
    assert_eq!(cpu.l, 0x78, "L = (SP)");
    assert_eq!(ram.read(0x8000), 0x34, "(SP) = L");
    assert_eq!(ram.read(0x8001), 0x12, "(SP+1) = H");
}

// ========================================
// ADC A, r / (HL) / n
// ========================================

#[test]
fn test_adc_a_with_carry_propagation() {
    let rom = [
        0x3E, 0xFF, // LD A, 0xFF
        0x06, 0x01, // LD B, 0x01
        0x80, // ADD A, B (sets carry)
        0x3E, 0x10, // LD A, 0x10
        0x0E, 0x20, // LD C, 0x20
        0x89, // ADC A, C (0x10 + 0x20 + 1)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    execute_n_instructions(&mut cpu, &mut ram, 6);

    assert_eq!(cpu.a, 0x31, "0x10 + 0x20 + carry(1) = 0x31");
}

#[test]
fn test_adc_a_hl_integration() {
    let rom = [
        0x21, 0x00, 0x80, // LD HL, 0x8000
        0x3E, 0x10, // LD A, 0x10
        0x37, // SCF (set carry)
        0x8E, // ADC A, (HL)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    ram.write(0x8000, 0x20);

    execute_n_instructions(&mut cpu, &mut ram, 4);

    assert_eq!(cpu.a, 0x31, "0x10 + 0x20 + 1 = 0x31");
}

#[test]
fn test_adc_a_n_integration() {
    let rom = [
        0x3E, 0x50, // LD A, 0x50
        0x37, // SCF
        0xCE, 0x30, // ADC A, 0x30
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert_eq!(cpu.a, 0x81, "0x50 + 0x30 + 1 = 0x81");
}

// ========================================
// SBC A, r / (HL) / n
// ========================================

#[test]
fn test_sbc_a_with_borrow() {
    let rom = [
        0x3E, 0x50, // LD A, 0x50
        0x0E, 0x20, // LD C, 0x20
        0x37, // SCF (set carry for borrow)
        0x99, // SBC A, C
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    execute_n_instructions(&mut cpu, &mut ram, 4);

    assert_eq!(cpu.a, 0x2F, "0x50 - 0x20 - 1 = 0x2F");
}

#[test]
fn test_sbc_a_hl_integration() {
    let rom = [
        0x21, 0x00, 0x80, // LD HL, 0x8000
        0x3E, 0x50, // LD A, 0x50
        0x37, // SCF
        0x9E, // SBC A, (HL)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    ram.write(0x8000, 0x20);

    execute_n_instructions(&mut cpu, &mut ram, 4);

    assert_eq!(cpu.a, 0x2F, "0x50 - 0x20 - 1 = 0x2F");
}

#[test]
fn test_sbc_a_n_integration() {
    let rom = [
        0x3E, 0x80, // LD A, 0x80
        0xDE, 0x30, // SBC A, 0x30
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    execute_n_instructions(&mut cpu, &mut ram, 2);

    assert_eq!(cpu.a, 0x50, "0x80 - 0x30 = 0x50");
}

// ========================================
// Arithmetic and Logical Operations with (HL)
// ========================================

#[test]
fn test_add_a_hl_integration() {
    let rom = [
        0x21, 0x00, 0x80, // LD HL, 0x8000
        0x3E, 0x10, // LD A, 0x10
        0x86, // ADD A, (HL)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    ram.write(0x8000, 0x25);

    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert_eq!(cpu.a, 0x35, "0x10 + 0x25 = 0x35");
}

#[test]
fn test_sub_hl_integration() {
    let rom = [
        0x21, 0x00, 0x80, // LD HL, 0x8000
        0x3E, 0x50, // LD A, 0x50
        0x96, // SUB (HL)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    ram.write(0x8000, 0x30);

    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert_eq!(cpu.a, 0x20, "0x50 - 0x30 = 0x20");
}

#[test]
fn test_and_hl_integration() {
    let rom = [
        0x21, 0x00, 0x80, // LD HL, 0x8000
        0x3E, 0xFF, // LD A, 0xFF
        0xA6, // AND (HL)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    ram.write(0x8000, 0x0F);

    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert_eq!(cpu.a, 0x0F, "0xFF & 0x0F = 0x0F");
}

#[test]
fn test_xor_hl_integration() {
    let rom = [
        0x21, 0x00, 0x80, // LD HL, 0x8000
        0x3E, 0xFF, // LD A, 0xFF
        0xAE, // XOR (HL)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    ram.write(0x8000, 0xFF);

    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert_eq!(cpu.a, 0x00, "0xFF ^ 0xFF = 0x00");
    assert!(cpu.flag_z(), "Result is zero");
}

#[test]
fn test_or_hl_integration() {
    let rom = [
        0x21, 0x00, 0x80, // LD HL, 0x8000
        0x3E, 0xF0, // LD A, 0xF0
        0xB6, // OR (HL)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    ram.write(0x8000, 0x0F);

    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert_eq!(cpu.a, 0xFF, "0xF0 | 0x0F = 0xFF");
}

#[test]
fn test_cp_hl_integration() {
    let rom = [
        0x21, 0x00, 0x80, // LD HL, 0x8000
        0x3E, 0x50, // LD A, 0x50
        0xBE, // CP (HL)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);
    ram.write(0x8000, 0x50);

    execute_n_instructions(&mut cpu, &mut ram, 3);

    assert_eq!(cpu.a, 0x50, "A should not change");
    assert!(cpu.flag_z(), "Equal comparison sets Z");
}

// ========================================
// Complex Sequences
// ========================================

#[test]
fn test_multi_byte_addition_with_adc() {
    // 16-bit addition using ADC for carry propagation
    // 0x1234 + 0x5678 = 0x68AC
    let rom = [
        0x3E, 0x34, // LD A, 0x34 (low byte 1)
        0x06, 0x78, // LD B, 0x78 (low byte 2)
        0x80, // ADD A, B
        0x4F, // LD C, A (save low result)
        0x3E, 0x12, // LD A, 0x12 (high byte 1)
        0x16, 0x56, // LD D, 0x56 (high byte 2)
        0x8A, // ADC A, D (add with carry)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    execute_n_instructions(&mut cpu, &mut ram, 7);

    assert_eq!(cpu.c, 0xAC, "Low byte result");
    assert_eq!(cpu.a, 0x68, "High byte result with carry");
}

#[test]
fn test_exchange_swap_pattern() {
    // Pattern: Load -> Exchange -> Modify -> Exchange back
    let rom = [
        0x21, 0x34, 0x12, // LD HL, 0x1234
        0x11, 0x78, 0x56, // LD DE, 0x5678
        0xEB, // EX DE, HL
        0x23, // INC HL
        0xEB, // EX DE, HL
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    execute_n_instructions(&mut cpu, &mut ram, 5);

    // After first EX: HL=0x5678, DE=0x1234
    // After INC HL: HL=0x5679
    // After second EX: HL=0x1234, DE=0x5679
    assert_eq!(cpu.h, 0x12);
    assert_eq!(cpu.l, 0x34);
    assert_eq!(cpu.d, 0x56);
    assert_eq!(cpu.e, 0x79);
}

#[test]
fn test_djnz_with_memory_operations() {
    // Fill 5 bytes with 0xFF using DJNZ
    let rom = [
        0x06, 0x05, // LD B, 5
        0x21, 0x00, 0x80, // LD HL, 0x8000
        0x3E, 0xFF, // LD A, 0xFF
        // loop:
        0x77, // LD (HL), A
        0x23, // INC HL
        0x10, 0xFC, // DJNZ loop (-4)
        0x76, // HALT
    ];

    let (mut cpu, mut ram) = setup_z80_with_rom(&rom);

    // Execute setup
    execute_n_instructions(&mut cpu, &mut ram, 3);

    // Execute loop 5 times
    for _ in 0..5 {
        cpu.step(&mut ram); // LD (HL), A
        cpu.step(&mut ram); // INC HL
        cpu.step(&mut ram); // DJNZ
    }

    // Verify memory was filled
    for i in 0..5 {
        assert_eq!(ram.read(0x8000 + i), 0xFF, "Byte {} should be 0xFF", i);
    }
    assert_eq!(cpu.b, 0, "B should be 0 after loop");
}
