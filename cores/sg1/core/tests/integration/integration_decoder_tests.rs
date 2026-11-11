// Decoder integration tests
// They test the WHOLE PATH: fetch -> decode -> execute

use sg1_core::bus::AddressBus;
use sg1_core::cartridge::composable::build_genesis_cartridge;
use sg1_core::cpu68k::instructions::Flag;
use sg1_core::cpu68k::M68k;

/// Creates a CPU and a bus with a ROM
fn setup_cpu_with_rom(rom: &[u8]) -> (M68k, AddressBus) {
    let mut cpu = M68k::new();
    let mut bus = AddressBus::new();

    // Build a full ROM with reset vectors (minimum 512 bytes for valid header)
    let mut full_rom = vec![0u8; (0x400 + rom.len()).max(512)];

    // Reset vectors at 0x000000
    full_rom[0] = 0x00;
    full_rom[1] = 0x00;
    full_rom[2] = 0x10;
    full_rom[3] = 0x00; // SSP = 0x00001000
    full_rom[4] = 0x00;
    full_rom[5] = 0x00;
    full_rom[6] = 0x04;
    full_rom[7] = 0x00; // PC = 0x00000400 (default)

    // Genesis header (required for factory function)
    full_rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
    full_rom[0x120..0x138].copy_from_slice(b"TEST ROM                ");

    // The tests, though, run from PC=0 directly: copy the ROM to offset 0,
    // over the vectors above as far as it reaches (the tests set PC themselves)
    for (i, &byte) in rom.iter().enumerate() {
        full_rom[i] = byte;
    }

    let cartridge = build_genesis_cartridge(full_rom).expect("Failed to create test cartridge");
    bus.set_cartridge(cartridge);
    cpu.reset(&mut bus);

    (cpu, bus)
}

// ========================================
// IMMEDIATE OPERATIONS
// ========================================

#[test]
fn test_decoder_ori_byte() {
    // ORI.B #$12, D0
    let rom = [
        0x00, 0x00, // ORI.B #$12, D0
        0x00, 0x12,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000000;

    let cycles = cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x00000012, "D0 should be 0x12");
    assert!(cycles > 0, "Should consume cycles");
}

#[test]
fn test_decoder_andi_word() {
    // ANDI.W #$00FF, D1
    let rom = [
        0x02, 0x41, // ANDI.W #$00FF, D1
        0x00, 0xFF,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[1] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[1] & 0xFFFF, 0x0078, "D1 lower word should be ANDed");
}

#[test]
fn test_decoder_cmpi_long() {
    // CMPI.L #$12345678, D2
    let rom = [
        0x0C, 0x82, // CMPI.L #$12345678, D2
        0x12, 0x34, 0x56, 0x78,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[2] = 0x12345678;

    cpu.step(&mut bus);

    // Z flag should be set (equal)
    assert!(cpu.get_flag(Flag::Z), "Z flag should be set");
}

// ========================================
// BIT OPERATIONS
// ========================================

#[test]
fn test_decoder_btst_immediate() {
    // BTST #7, D0
    let rom = [
        0x08, 0x00, // BTST #7, D0
        0x00, 0x07,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000080; // Bit 7 set

    cpu.step(&mut bus);

    assert!(!cpu.get_flag(Flag::Z), "Z should be clear (bit is set)");
}

#[test]
fn test_decoder_bset_register() {
    // BSET D0, D1 (bit number in D0)
    // Format: 0000 rrr 1 11 mmm rrr
    //         0000 000 1 11 000 001 = 0x01C1
    let rom = [
        0x01, 0xC1, // BSET D0, D1
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 5; // Bit number is in D0
    cpu.d[1] = 0x00000000;

    cpu.step(&mut bus);

    // After BSET D0,D1 where D0=5, D1 should have bit 5 set
    // Bit 5 = 0x00000020 (for long operations on data register)
    assert_eq!(cpu.d[1], 0x00000020, "Bit 5 should be set");
}

// ========================================
// COMPARISONS
// ========================================

#[test]
fn test_decoder_cmp_word() {
    // CMP.W D1, D0
    let rom = [
        0xB0, 0x41, // CMP.W D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x1234;
    cpu.d[1] = 0x1234;

    cpu.step(&mut bus);

    assert!(cpu.get_flag(Flag::Z), "Z flag should be set (equal)");
}

#[test]
fn test_decoder_cmpa_long() {
    // CMPA.L D0, A1
    let rom = [
        0xB3, 0xC0, // CMPA.L D0, A1
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[1] = 0x12345678;
    cpu.d[0] = 0x12345678;

    cpu.step(&mut bus);

    assert!(cpu.get_flag(Flag::Z), "Z flag should be set");
}

// ========================================
// SHIFTS/ROTATES
// ========================================

#[test]
fn test_decoder_lsl_immediate() {
    // LSL.W #1, D0
    let rom = [
        0xE3, 0x48, // LSL.W #1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000001;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x0002, "Should shift left by 1");
}

#[test]
fn test_decoder_ror_register() {
    // ROR.L D1, D0 (shift count in D1)
    let rom = [
        0xE2, 0xB8, // ROR.L D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[1] = 4; // Rotate 4 times
    cpu.d[0] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x81234567, "Should rotate right by 4");
}

// ========================================
// EXTENDED ARITHMETIC
// ========================================

#[test]
fn test_decoder_addx_data() {
    // ADDX.B D1, D0
    let rom = [
        0xD1, 0x01, // ADDX.B D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x10;
    cpu.d[1] = 0x20;
    cpu.set_flag(Flag::X, true); // X flag set

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x31, "Should be 0x10 + 0x20 + 1 (X)");
}

#[test]
fn test_decoder_adda_word() {
    // ADDA.W D0, A1
    let rom = [
        0xD2, 0xC0, // ADDA.W D0, A1
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[1] = 0x00001000;
    cpu.d[0] = 0x0100;

    cpu.step(&mut bus);

    assert_eq!(cpu.a[1], 0x00001100, "A1 should be incremented");
}

// ========================================
// BCD
// ========================================

#[test]
fn test_decoder_abcd() {
    // ABCD D1, D0
    let rom = [
        0xC1, 0x01, // ABCD D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x09; // BCD 09
    cpu.d[1] = 0x01; // BCD 01

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x10, "BCD: 09 + 01 = 10");
}

// ========================================
// MULT/DIV
// ========================================

#[test]
fn test_decoder_mulu() {
    // MULU.W D1, D0
    let rom = [
        0xC0, 0xC1, // MULU.W D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x0010;
    cpu.d[1] = 0x0020;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x0200, "16 * 32 = 512");
}

#[test]
fn test_decoder_divu() {
    // DIVU.W D1, D0
    let rom = [
        0x80, 0xC1, // DIVU.W D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000064; // 100
    cpu.d[1] = 0x0000000A; // 10

    cpu.step(&mut bus);

    let quotient = cpu.d[0] & 0xFFFF;
    let remainder = (cpu.d[0] >> 16) & 0xFFFF;
    assert_eq!(quotient, 10, "100 / 10 = 10");
    assert_eq!(remainder, 0, "100 % 10 = 0");
}

// ========================================
// DATA MOVEMENT EXTENDED
// ========================================

#[test]
fn test_decoder_exg_data() {
    // EXG D0, D1
    let rom = [
        0xC1, 0x41, // EXG D0, D1
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;
    cpu.d[1] = 0xAABBCCDD;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0xAABBCCDD, "D0 and D1 should be swapped");
    assert_eq!(cpu.d[1], 0x12345678);
}

#[test]
fn test_decoder_swap() {
    // SWAP D0
    let rom = [
        0x48, 0x40, // SWAP D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x56781234, "Words should be swapped");
}

// ========================================
// SYSTEM CONTROL
// ========================================

#[test]
fn test_decoder_nop() {
    // NOP
    let rom = [
        0x4E, 0x71, // NOP
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x0002, "PC should advance by 2");
}

#[test]
fn test_decoder_trap() {
    // Need to setup trap vectors in ROM first
    let mut full_rom = vec![0u8; 0x1000];

    // Trap #0 vector at 0x80 (vector 32): points to 0x00001000
    full_rom[0x80] = 0x00;
    full_rom[0x81] = 0x00;
    full_rom[0x82] = 0x10;
    full_rom[0x83] = 0x00;

    // TRAP #0 instruction at 0x0000
    full_rom[0x00] = 0x4E;
    full_rom[0x01] = 0x40;

    let mut cpu = sg1_core::cpu68k::M68k::new();
    let mut bus = sg1_core::bus::AddressBus::new();

    // Add header if needed
    if full_rom.len() < 512 {
        full_rom.resize(512, 0);
    }
    if &full_rom[0x100..0x110] != b"SEGA MEGA DRIVE " {
        full_rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
        full_rom[0x120..0x138].copy_from_slice(b"TEST ROM                ");
    }

    let cartridge = build_genesis_cartridge(full_rom).expect("Failed to create test cartridge");
    bus.set_cartridge(cartridge);
    cpu.reset(&mut bus);
    cpu.pc = 0x0000;

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x1000, "PC should jump to trap vector");
}

// ========================================
// BRANCHES
// ========================================

#[test]
fn test_decoder_bra() {
    // BRA.S +10
    let rom = [
        0x60, 0x0A, // BRA.S +10
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch to PC+2+10");
}

#[test]
fn test_decoder_beq() {
    // BEQ.S +10 (branch if zero)
    let rom = [
        0x67, 0x0A, // BEQ.S +10
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::Z, true); // Set Z flag

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when Z=1");
}

#[test]
fn test_decoder_dbcc() {
    // DBRA D0, -10 (decrement and branch)
    let rom = [
        0x51, 0xC8, // DBRA D0, -10
        0xFF, 0xF6, // displacement -10
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 5; // Counter

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 4, "Counter should decrement");
    // Should branch back because counter != -1
}

// ========================================
// MOVE TESTS (fundamental)
// ========================================

#[test]
fn test_decoder_move_long() {
    // MOVE.L D0, D1
    let rom = [
        0x22, 0x00, // MOVE.L D0, D1
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;
    cpu.d[1] = 0x00000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[1], 0x12345678, "D1 should equal D0");
}

#[test]
fn test_decoder_movea_long() {
    // MOVEA.L D0, A1
    let rom = [
        0x22, 0x40, // MOVEA.L D0, A1
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;
    cpu.a[1] = 0x00000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.a[1], 0x12345678, "A1 should equal D0");
}

// ========================================
// ADD/SUB TESTS (basic arithmetic)
// ========================================

#[test]
fn test_decoder_add_long() {
    // ADD.L D1, D0
    let rom = [
        0xD0, 0x81, // ADD.L D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x10000000;
    cpu.d[1] = 0x01000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x11000000, "Should add correctly");
}

#[test]
fn test_decoder_sub_word() {
    // SUB.W D1, D0
    let rom = [
        0x90, 0x41, // SUB.W D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x1000;
    cpu.d[1] = 0x0100;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x0F00, "Should subtract correctly");
}

// ========================================
// ADDITIONAL COVERAGE TESTS
// ========================================

#[test]
fn test_decoder_neg_word() {
    // NEG.W D0
    let rom = [
        0x44, 0x40, // NEG.W D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00001234;

    cpu.step(&mut bus);

    assert_eq!(
        cpu.d[0] & 0xFFFF,
        (-0x1234i32 & 0xFFFF) as u32,
        "Should negate"
    );
}

#[test]
fn test_decoder_not_long() {
    // NOT.L D0
    let rom = [
        0x46, 0x80, // NOT.L D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0xEDCBA987, "Should invert all bits");
}

#[test]
fn test_decoder_tst_byte() {
    // TST.B D0
    let rom = [
        0x4A, 0x00, // TST.B D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00000000;

    cpu.step(&mut bus);

    assert!(cpu.get_flag(Flag::Z), "Z flag should be set for zero");
}

#[test]
fn test_decoder_addq_long() {
    // ADDQ.L #8, D0
    let rom = [
        0x50, 0x80, // ADDQ.L #8, D0 (data=0 means 8)
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00001000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x00001008, "Should add 8");
}

#[test]
fn test_decoder_subq_word() {
    // SUBQ.W #1, D0
    let rom = [
        0x53, 0x40, // SUBQ.W #1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x00001000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x0FFF, "Should subtract 1");
}

#[test]
fn test_decoder_moveq() {
    // MOVEQ #$12, D0
    let rom = [
        0x70, 0x12, // MOVEQ #$12, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFFFFFFFF;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x00000012, "Should move immediate to D0");
}

#[test]
fn test_decoder_lea() {
    // LEA (A1), A0
    let rom = [
        0x41, 0xD1, // LEA (A1), A0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[1] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.a[0], 0x12345678, "A0 should get effective address");
}

#[test]
fn test_decoder_jmp() {
    // JMP (A0)
    let rom = [
        0x4E, 0xD0, // JMP (A0)
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[0] = 0x00002000;

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x2000, "PC should jump to A0");
}

#[test]
fn test_decoder_jsr() {
    // JSR (A0)
    let rom = [
        0x4E, 0x90, // JSR (A0)
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[0] = 0x00002000;
    cpu.a[7] = 0x00001000; // Stack pointer

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x2000, "PC should jump to A0");
    // Return address (0x0002) should be pushed to stack
    assert_eq!(cpu.a[7], 0x0FFC, "SP should be decremented by 4");
}

#[test]
fn test_decoder_bne() {
    // BNE.S +10 (branch if not zero)
    let rom = [
        0x66, 0x0A, // BNE.S +10
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::Z, false); // Clear Z flag

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when Z=0");
}

// ========================================
// SIZE VARIANTS
// ========================================

#[test]
fn test_decoder_move_byte() {
    // MOVE.B D0, D1
    let rom = [
        0x12, 0x00, // MOVE.B D0, D1
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;
    cpu.d[1] = 0xFFFFFFFF;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[1] & 0xFF, 0x78, "D1 lower byte should be copied");
    assert_eq!(
        cpu.d[1] & 0xFFFFFF00,
        0xFFFFFF00,
        "D1 upper bytes unchanged"
    );
}

#[test]
fn test_decoder_move_word() {
    // MOVE.W D0, D1
    let rom = [
        0x32, 0x00, // MOVE.W D0, D1
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;
    cpu.d[1] = 0xFFFFFFFF;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[1] & 0xFFFF, 0x5678, "D1 lower word should be copied");
    assert_eq!(cpu.d[1] & 0xFFFF0000, 0xFFFF0000, "D1 upper word unchanged");
}

#[test]
fn test_decoder_add_byte() {
    // ADD.B D1, D0
    let rom = [
        0xD0, 0x01, // ADD.B D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x10;
    cpu.d[1] = 0x20;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x30, "D0 lower byte should be sum");
}

#[test]
fn test_decoder_add_word() {
    // ADD.W D1, D0
    let rom = [
        0xD0, 0x41, // ADD.W D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x1000;
    cpu.d[1] = 0x0200;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x1200, "D0 lower word should be sum");
}

#[test]
fn test_decoder_sub_byte() {
    // SUB.B D1, D0
    let rom = [
        0x90, 0x01, // SUB.B D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x30;
    cpu.d[1] = 0x10;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x20, "D0 lower byte should be difference");
}

#[test]
fn test_decoder_sub_long() {
    // SUB.L D1, D0
    let rom = [
        0x90, 0x81, // SUB.L D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x12345678;
    cpu.d[1] = 0x00005678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x12340000, "D0 should be difference");
}

#[test]
fn test_decoder_and_byte() {
    // AND.B D1, D0
    let rom = [
        0xC0, 0x01, // AND.B D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFF;
    cpu.d[1] = 0x0F;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x0F, "D0 lower byte should be ANDed");
}

#[test]
fn test_decoder_and_long() {
    // AND.L D1, D0
    let rom = [
        0xC0, 0x81, // AND.L D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFFFFFFFF;
    cpu.d[1] = 0x12345678;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x12345678, "D0 should be ANDed");
}

#[test]
fn test_decoder_or_byte() {
    // OR.B D1, D0
    let rom = [
        0x80, 0x01, // OR.B D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xF0;
    cpu.d[1] = 0x0F;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0xFF, "D0 lower byte should be ORed");
}

#[test]
fn test_decoder_or_word() {
    // OR.W D1, D0
    let rom = [
        0x80, 0x41, // OR.W D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFF00;
    cpu.d[1] = 0x00FF;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0xFFFF, "D0 lower word should be ORed");
}

#[test]
fn test_decoder_or_long() {
    // OR.L D1, D0
    let rom = [
        0x80, 0x81, // OR.L D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFF00FF00;
    cpu.d[1] = 0x00FF00FF;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0xFFFFFFFF, "D0 should be ORed");
}

#[test]
fn test_decoder_eor_byte() {
    // EOR.B D0, D1
    let rom = [
        0xB1, 0x01, // EOR.B D0, D1
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFF;
    cpu.d[1] = 0xF0;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[1] & 0xFF, 0x0F, "D1 lower byte should be XORed");
}

// ========================================
// MORE SHIFTS & ROTATES
// ========================================

#[test]
fn test_decoder_asl_byte() {
    // ASL.B #1, D0
    let rom = [
        0xE3, 0x00, // ASL.B #1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x01;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x02, "Should shift left by 1");
}

#[test]
fn test_decoder_asr_word() {
    // ASR.W #1, D0 -> 1110 001 0 01 000 000 = 0xE240
    let rom = [
        0xE2, 0x40, // ASR.W #1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x8000; // Negative number

    cpu.step(&mut bus);

    assert_eq!(
        cpu.d[0] & 0xFFFF,
        0xC000,
        "Should arithmetic shift right (sign extend)"
    );
}

#[test]
fn test_decoder_lsr_long() {
    // LSR.L #1, D0
    let rom = [
        0xE2, 0x88, // LSR.L #1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x80000000;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0], 0x40000000, "Should logical shift right");
}

#[test]
fn test_decoder_rol_word() {
    // ROL.W #1, D0
    let rom = [
        0xE3, 0x58, // ROL.W #1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x8001;

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x0003, "Should rotate left");
}

#[test]
fn test_decoder_roxl_word() {
    // ROXL.W #1, D0
    let rom = [
        0xE3, 0x50, // ROXL.W #1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x8000;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x0001, "Should rotate left through X");
}

#[test]
fn test_decoder_roxr_word() {
    // ROXR.W #1, D0
    let rom = [
        0xE2, 0x50, // ROXR.W #1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x0001;
    cpu.set_flag(Flag::X, true);

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x8000, "Should rotate right through X");
}

// ========================================
// MORE BCD & MULT/DIV
// ========================================

#[test]
fn test_decoder_sbcd() {
    // SBCD D1, D0
    let rom = [
        0x81, 0x01, // SBCD D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x15; // BCD 15
    cpu.d[1] = 0x05; // BCD 05

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] & 0xFF, 0x10, "BCD: 15 - 05 = 10");
}

#[test]
fn test_decoder_nbcd() {
    // NBCD D0
    let rom = [
        0x48, 0x00, // NBCD D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0x05; // BCD 05

    cpu.step(&mut bus);

    // NBCD of 05 should be 95 (100 - 05 in BCD)
    assert_eq!(cpu.d[0] & 0xFF, 0x95, "BCD: -05 = 95");
}

#[test]
fn test_decoder_muls() {
    // MULS.W D1, D0
    let rom = [
        0xC1, 0xC1, // MULS.W D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFFFFFFF0; // -16 (sign extended)
    cpu.d[1] = 0x00000002; // 2

    cpu.step(&mut bus);

    assert_eq!(cpu.d[0] as i32, -32, "Signed multiply: -16 * 2 = -32");
}

#[test]
fn test_decoder_divs() {
    // DIVS.W D1, D0
    let rom = [
        0x81, 0xC1, // DIVS.W D1, D0
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.d[0] = 0xFFFFFFE0; // -32 (sign extended)
    cpu.d[1] = 0x0000FFFE; // -2 (sign extended word)

    cpu.step(&mut bus);

    let quotient = (cpu.d[0] & 0xFFFF) as i16;
    assert_eq!(quotient, 16, "Signed divide: -32 / -2 = 16");
}

// ========================================
// MORE BRANCHES
// ========================================

#[test]
fn test_decoder_bcc() {
    // BCC.S +10 (branch if carry clear)
    let rom = [
        0x64, 0x0A, // BCC.S +10
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::C, false); // Clear C flag

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when C=0");
}

#[test]
fn test_decoder_bcs() {
    // BCS.S +10 (branch if carry set)
    let rom = [
        0x65, 0x0A, // BCS.S +10
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::C, true); // Set C flag

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when C=1");
}

#[test]
fn test_decoder_bge() {
    // BGE.S +10 (branch if greater or equal - signed)
    let rom = [
        0x6C, 0x0A, // BGE.S +10
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::N, false);
    cpu.set_flag(Flag::V, false); // N == V -> >=

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when N==V");
}

#[test]
fn test_decoder_blt() {
    // BLT.S +10 (branch if less than - signed)
    let rom = [
        0x6D, 0x0A, // BLT.S +10
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.set_flag(Flag::N, true);
    cpu.set_flag(Flag::V, false); // N != V -> <

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "Should branch when N!=V");
}

#[test]
fn test_decoder_bsr() {
    // BSR.S +10 (branch to subroutine)
    let rom = [
        0x61, 0x0A, // BSR.S +10
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);
    cpu.pc = 0x0000;
    cpu.a[7] = 0x00001000; // Stack pointer

    cpu.step(&mut bus);

    assert_eq!(cpu.pc, 0x000C, "PC should jump to PC+2+10");
    assert_eq!(cpu.a[7], 0x0FFC, "SP should be decremented by 4");
}
