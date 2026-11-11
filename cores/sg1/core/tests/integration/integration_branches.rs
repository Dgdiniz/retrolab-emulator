// Integration tests for the branch instructions
// Bcc (every condition), BSR, DBcc, Scc

use super::common::*;
use sg1_core::cpu68k::instructions::Flag;

// ========================================
// Bcc - Branch on Condition (all 16 conditions)
// ========================================

#[test]
fn test_bra_unconditional() {
    let rom = [0x60, 0x0A]; // BRA.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch to PC+2+10");
}

#[test]
fn test_beq_taken() {
    let rom = [0x67, 0x0A]; // BEQ.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::Z, true); // Z=1

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when Z=1");
}

#[test]
fn test_beq_not_taken() {
    let rom = [0x67, 0x0A]; // BEQ.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::Z, false); // Z=0

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x0002, "Should not branch when Z=0");
}

#[test]
fn test_bne_taken() {
    let rom = [0x66, 0x0A]; // BNE.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::Z, false); // Z=0

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when Z=0");
}

#[test]
fn test_bcc_carry_clear() {
    let rom = [0x64, 0x0A]; // BCC.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::C, false); // C=0

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when C=0");
}

#[test]
fn test_bcs_carry_set() {
    let rom = [0x65, 0x0A]; // BCS.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::C, true); // C=1

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when C=1");
}

#[test]
fn test_bpl_plus() {
    let rom = [0x6A, 0x0A]; // BPL.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::N, false); // N=0 (plus)

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when N=0");
}

#[test]
fn test_bmi_minus() {
    let rom = [0x6B, 0x0A]; // BMI.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::N, true); // N=1 (minus)

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when N=1");
}

#[test]
fn test_bge_greater_or_equal() {
    let rom = [0x6C, 0x0A]; // BGE.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::N, false);
    cpu.set_flag(Flag::V, false); // N==V -> >=

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when N==V");
}

#[test]
fn test_blt_less_than() {
    let rom = [0x6D, 0x0A]; // BLT.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::N, true);
    cpu.set_flag(Flag::V, false); // N!=V -> <

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when N!=V");
}

#[test]
fn test_bgt_greater_than() {
    let rom = [0x6E, 0x0A]; // BGT.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::Z, false);
    cpu.set_flag(Flag::N, false);
    cpu.set_flag(Flag::V, false); // Z=0 && N==V

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when Z=0 && N==V");
}

#[test]
fn test_ble_less_or_equal() {
    let rom = [0x6F, 0x0A]; // BLE.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::Z, true); // Z=1 OR N!=V

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when Z=1");
}

#[test]
fn test_bhi_higher() {
    let rom = [0x62, 0x0A]; // BHI.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::C, false);
    cpu.set_flag(Flag::Z, false); // C=0 && Z=0

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when C=0 && Z=0");
}

#[test]
fn test_bls_lower_or_same() {
    let rom = [0x63, 0x0A]; // BLS.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::C, true); // C=1 OR Z=1

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when C=1");
}

#[test]
fn test_bvc_overflow_clear() {
    let rom = [0x68, 0x0A]; // BVC.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::V, false); // V=0

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when V=0");
}

#[test]
fn test_bvs_overflow_set() {
    let rom = [0x69, 0x0A]; // BVS.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::V, true); // V=1

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when V=1");
}

// ========================================
// BSR - Branch to Subroutine
// ========================================

#[test]
fn test_bsr_short() {
    let rom = [0x61, 0x0A]; // BSR.S +10

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[7] = 0x00001000; // Stack pointer

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "PC should jump to PC+2+10");
    assert_eq!(cpu.a[7], 0x0FFC, "SP should be decremented by 4");
}

// ========================================
// DBcc - Decrement and Branch
// ========================================

#[test]
fn test_dbra_continue_loop() {
    let rom = [
        0x51, 0xC8, // DBRA D0, -10
        0xFF, 0xF6,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 5; // Counter

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 4, "Counter should decrement");
    // Should branch back because counter != -1
}

#[test]
fn test_dbra_exit_loop() {
    let rom = [
        0x51, 0xC8, // DBRA D0, -10
        0xFF, 0xF6,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0; // Counter will become -1

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0xFFFF, "Counter should be -1");
    assert_eq!(cpu.pc, 0x0004, "Should not branch when counter = -1");
}

#[test]
fn test_dbeq_condition_true() {
    let rom = [
        0x57, 0xC8, // DBEQ D0, -10
        0xFF, 0xF6,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 5;
    cpu.set_flag(Flag::Z, true); // Condition true

    cpu.step(&mut bus);

    // When condition is true, don't loop (exit immediately)
    assert_eq!(cpu.d[0] & 0xFFFF, 5, "Counter unchanged when Z=1");
    assert_eq!(cpu.pc, 0x0004, "Should not branch when condition true");
}

// ========================================
// Scc - Set on Condition
// ========================================

#[test]
fn test_seq_true() {
    let rom = [0x57, 0xC0]; // SEQ D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00;
    cpu.set_flag(Flag::Z, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0xFF, "Should set to 0xFF when Z=1");
}

#[test]
fn test_seq_false() {
    let rom = [0x57, 0xC0]; // SEQ D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFF;
    cpu.set_flag(Flag::Z, false);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x00, "Should set to 0x00 when Z=0");
}

#[test]
fn test_sne_true() {
    let rom = [0x56, 0xC0]; // SNE D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00;
    cpu.set_flag(Flag::Z, false);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0xFF, "Should set to 0xFF when Z=0");
}

#[test]
fn test_scc_carry_clear() {
    let rom = [0x54, 0xC0]; // SCC D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00;
    cpu.set_flag(Flag::C, false);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0xFF, "Should set to 0xFF when C=0");
}

#[test]
fn test_scs_carry_set() {
    let rom = [0x55, 0xC0]; // SCS D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00;
    cpu.set_flag(Flag::C, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0xFF, "Should set to 0xFF when C=1");
}

#[test]
fn test_spl_plus() {
    let rom = [0x5A, 0xC0]; // SPL D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00;
    cpu.set_flag(Flag::N, false);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0xFF, "Should set to 0xFF when N=0");
}

#[test]
fn test_smi_minus() {
    let rom = [0x5B, 0xC0]; // SMI D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00;
    cpu.set_flag(Flag::N, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0xFF, "Should set to 0xFF when N=1");
}

#[test]
fn test_sge_greater_or_equal() {
    let rom = [0x5C, 0xC0]; // SGE D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00;
    cpu.set_flag(Flag::N, false);
    cpu.set_flag(Flag::V, false);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0xFF, "Should set to 0xFF when N==V");
}

#[test]
fn test_slt_less_than() {
    let rom = [0x5D, 0xC0]; // SLT D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00;
    cpu.set_flag(Flag::N, true);
    cpu.set_flag(Flag::V, false);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0xFF, "Should set to 0xFF when N!=V");
}

#[test]
fn test_st_always() {
    let rom = [0x50, 0xC0]; // ST D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0xFF, "Should always set to 0xFF");
}

#[test]
fn test_sf_never() {
    let rom = [0x51, 0xC0]; // SF D0

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFF;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x00, "Should always set to 0x00");
}
