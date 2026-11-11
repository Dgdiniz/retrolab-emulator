//! Disassembler tests
//!
//! Sample tests for each instruction group to validate disassembly output.

use sg1_core::bus::AddressBus;
use sg1_core::cartridge::composable::build_genesis_cartridge;
use sg1_core::cpu68k::disasm;

/// Helper to setup a bus with test opcodes
fn setup_bus_with_opcodes(opcodes: &[u16]) -> AddressBus {
    let mut bus = AddressBus::new();
    let mut rom = vec![0u8; 0x1000];

    // Genesis header (required for factory function)
    rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
    rom[0x120..0x138].copy_from_slice(b"TEST ROM                ");

    // Write opcodes to ROM
    for (i, &opcode) in opcodes.iter().enumerate() {
        let addr = i * 2;
        rom[addr] = (opcode >> 8) as u8;
        rom[addr + 1] = (opcode & 0xFF) as u8;
    }

    let cartridge = build_genesis_cartridge(rom).expect("Failed to create test cartridge");
    bus.set_cartridge(cartridge);
    bus
}

// ========================================
// Group 0x0: Immediate Operations
// ========================================

#[test]
fn test_disasm_ori_byte() {
    let mut bus = setup_bus_with_opcodes(&[0x0000, 0x0012]); // ORI.B #$12, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "ORI.B #$12, D0");
    assert_eq!(bytes, 4);
}

#[test]
fn test_disasm_andi_word() {
    let mut bus = setup_bus_with_opcodes(&[0x0240, 0x1234]); // ANDI.W #$1234, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "ANDI.W #$1234, D0");
    assert_eq!(bytes, 4);
}

#[test]
fn test_disasm_subi_long() {
    let mut bus = setup_bus_with_opcodes(&[0x0480, 0x0000, 0x5678]); // SUBI.L #$00005678, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "SUBI.L #$00005678, D0");
    assert_eq!(bytes, 6);
}

#[test]
fn test_disasm_ori_to_ccr() {
    let mut bus = setup_bus_with_opcodes(&[0x003C, 0x001F]); // ORI #$1F, CCR
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "ORI     #$1F, CCR");
    assert_eq!(bytes, 4);
}

#[test]
fn test_disasm_andi_to_sr() {
    let mut bus = setup_bus_with_opcodes(&[0x027C, 0xF8FF]); // ANDI #$F8FF, SR
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "ANDI    #$F8FF, SR");
    assert_eq!(bytes, 4);
}

#[test]
fn test_disasm_btst_immediate() {
    let mut bus = setup_bus_with_opcodes(&[0x0800, 0x0005]); // BTST #5, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "BTST    #$5, D0");
    assert_eq!(bytes, 4);
}

#[test]
fn test_disasm_bset_register() {
    let mut bus = setup_bus_with_opcodes(&[0x07C0]); // BSET D3, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "BSET    D3, D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_movep_all_four_opmodes() {
    // Opmode bit 7 is the direction (1 = to memory) and bit 6 the size
    // (1 = long): 100, 101, 110, 111.
    let cases = [
        (0x0108, "MOVEP.W $0010(A0), D0"),
        (0x0148, "MOVEP.L $0010(A0), D0"),
        (0x0188, "MOVEP.W D0, $0010(A0)"),
        (0x01C8, "MOVEP.L D0, $0010(A0)"),
    ];
    for (opcode, text) in cases {
        let mut bus = setup_bus_with_opcodes(&[opcode, 0x0010]);
        let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
        assert_eq!(instr, text, "opcode {:04X}", opcode);
        assert_eq!(bytes, 4);
    }
}

// ========================================
// Group 0x1/2/3: MOVE
// ========================================

#[test]
fn test_disasm_move_byte() {
    let mut bus = setup_bus_with_opcodes(&[0x1200]); // MOVE.B D0, D1
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "MOVE.B  D0, D1");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_move_long() {
    let mut bus = setup_bus_with_opcodes(&[0x2200]); // MOVE.L D0, D1
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "MOVE.L  D0, D1");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_movea_word() {
    let mut bus = setup_bus_with_opcodes(&[0x3040]); // MOVEA.W D0, A0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "MOVEA.W D0, A0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_movea_long() {
    let mut bus = setup_bus_with_opcodes(&[0x2040]); // MOVEA.L D0, A0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "MOVEA.L D0, A0");
    assert_eq!(bytes, 2);
}

// ========================================
// Group 0x4: Miscellaneous
// ========================================

#[test]
fn test_disasm_nop() {
    let mut bus = setup_bus_with_opcodes(&[0x4E71]); // NOP
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "NOP");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_reset() {
    let mut bus = setup_bus_with_opcodes(&[0x4E70]); // RESET
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "RESET");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_rts() {
    let mut bus = setup_bus_with_opcodes(&[0x4E75]); // RTS
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "RTS");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_jsr() {
    let mut bus = setup_bus_with_opcodes(&[0x4E90]); // JSR (A0)
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "JSR     (A0)");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_jmp() {
    let mut bus = setup_bus_with_opcodes(&[0x4ED0]); // JMP (A0)
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "JMP     (A0)");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_lea() {
    let mut bus = setup_bus_with_opcodes(&[0x41F8, 0x1000]); // LEA $1000.W, A0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "LEA     $1000, A0");
    assert_eq!(bytes, 4);
}

#[test]
fn test_disasm_pea() {
    let mut bus = setup_bus_with_opcodes(&[0x4850]); // PEA (A0)
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "PEA     (A0)");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_link() {
    let mut bus = setup_bus_with_opcodes(&[0x4E50, 0xFFF0]); // LINK A0, #-16
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "LINK    A0, #$FFF0");
    assert_eq!(bytes, 4);
}

#[test]
fn test_disasm_unlk() {
    let mut bus = setup_bus_with_opcodes(&[0x4E58]); // UNLK A0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "UNLK    A0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_swap() {
    let mut bus = setup_bus_with_opcodes(&[0x4840]); // SWAP D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "SWAP    D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_ext_word() {
    let mut bus = setup_bus_with_opcodes(&[0x4880]); // EXT.W D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "EXT.W   D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_ext_long() {
    let mut bus = setup_bus_with_opcodes(&[0x48C0]); // EXT.L D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "EXT.L   D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_clr_byte() {
    let mut bus = setup_bus_with_opcodes(&[0x4200]); // CLR.B D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "CLR.B  D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_neg_word() {
    let mut bus = setup_bus_with_opcodes(&[0x4440]); // NEG.W D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "NEG.W  D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_not_long() {
    let mut bus = setup_bus_with_opcodes(&[0x4680]); // NOT.L D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "NOT.L  D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_tas() {
    let mut bus = setup_bus_with_opcodes(&[0x4AC0]); // TAS D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "TAS     D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_tst_byte() {
    let mut bus = setup_bus_with_opcodes(&[0x4A00]); // TST.B D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "TST.B  D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_move_from_sr() {
    let mut bus = setup_bus_with_opcodes(&[0x40C0]); // MOVE SR, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "MOVE    SR, D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_move_to_ccr() {
    let mut bus = setup_bus_with_opcodes(&[0x44C0]); // MOVE D0, CCR
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "MOVE    D0, CCR");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_chk() {
    let mut bus = setup_bus_with_opcodes(&[0x4180]); // CHK D0, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "CHK     D0, D0");
    assert_eq!(bytes, 2);
}

// ========================================
// Group 0x5: ADDQ/SUBQ/DBcc/Scc
// ========================================

#[test]
fn test_disasm_addq_byte() {
    let mut bus = setup_bus_with_opcodes(&[0x5000]); // ADDQ.B #8, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "ADDQ.B #8, D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_subq_word() {
    let mut bus = setup_bus_with_opcodes(&[0x5540]); // SUBQ.W #2, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "SUBQ.W #2, D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_dbcc() {
    let mut bus = setup_bus_with_opcodes(&[0x51C8, 0xFFFE]); // DBF D0, $-2 (condition 1 = F, also written DBRA)
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert!(instr.starts_with("DBF") || instr.starts_with("DBRA"));
    assert_eq!(bytes, 4);
}

#[test]
fn test_disasm_scc() {
    let mut bus = setup_bus_with_opcodes(&[0x57C0]); // SEQ D0 (condition 7 = EQ)
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "SEQ     D0");
    assert_eq!(bytes, 2);
}

// ========================================
// Group 0x6: Branches
// ========================================

#[test]
fn test_disasm_bra() {
    // A zero 8-bit displacement means a 16-bit one follows: BRA.W, 4 bytes.
    let mut bus = setup_bus_with_opcodes(&[0x6000, 0x0010]); // BRA $+18
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert!(instr.starts_with("BRA"));
    assert_eq!(bytes, 4);
}

#[test]
fn test_disasm_beq() {
    let mut bus = setup_bus_with_opcodes(&[0x6706]); // BEQ $+8 (condition 7 = EQ)
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert!(instr.starts_with("BEQ"));
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_bsr() {
    let mut bus = setup_bus_with_opcodes(&[0x6110]); // BSR $+18 (condition 1 = F/BSR)
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert!(instr.starts_with("BSR"));
    assert_eq!(bytes, 2);
}

// ========================================
// Group 0x7: MOVEQ
// ========================================

#[test]
fn test_disasm_moveq() {
    let mut bus = setup_bus_with_opcodes(&[0x7012]); // MOVEQ #$12, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "MOVEQ   #$12, D0");
    assert_eq!(bytes, 2);
}

// ========================================
// Group 0x8: OR/DIVU/DIVS/SBCD
// ========================================

#[test]
fn test_disasm_or_byte() {
    let mut bus = setup_bus_with_opcodes(&[0x8000]); // OR.B D0, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "OR.B   D0, D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_divu() {
    let mut bus = setup_bus_with_opcodes(&[0x80C0]); // DIVU.W D0, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "DIVU.W  D0, D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_divs() {
    let mut bus = setup_bus_with_opcodes(&[0x81C0]); // DIVS.W D0, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "DIVS.W  D0, D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_sbcd() {
    let mut bus = setup_bus_with_opcodes(&[0x8100]); // SBCD D0, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "SBCD    D0, D0");
    assert_eq!(bytes, 2);
}

// ========================================
// Group 0x9: SUB/SUBA/SUBX
// ========================================

#[test]
fn test_disasm_sub_word() {
    let mut bus = setup_bus_with_opcodes(&[0x9040]); // SUB.W D0, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "SUB.W  D0, D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_suba_word() {
    let mut bus = setup_bus_with_opcodes(&[0x90C0]); // SUBA.W D0, A0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "SUBA.W D0, A0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_suba_long() {
    let mut bus = setup_bus_with_opcodes(&[0x91C0]); // SUBA.L D0, A0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "SUBA.L D0, A0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_subx() {
    let mut bus = setup_bus_with_opcodes(&[0x9140]); // SUBX.W D0, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "SUBX.W D0, D0");
    assert_eq!(bytes, 2);
}

// ========================================
// Group 0xB: CMP/CMPA/CMPM/EOR
// ========================================

#[test]
fn test_disasm_cmp_byte() {
    let mut bus = setup_bus_with_opcodes(&[0xB000]); // CMP.B D0, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "CMP.B  D0, D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_cmpa_word() {
    let mut bus = setup_bus_with_opcodes(&[0xB0C0]); // CMPA.W D0, A0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "CMPA.W D0, A0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_cmpm() {
    let mut bus = setup_bus_with_opcodes(&[0xB108]); // CMPM.B (A0)+, (A0)+
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "CMPM.B (A0)+, (A0)+");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_eor_word() {
    let mut bus = setup_bus_with_opcodes(&[0xB140]); // EOR.W D0, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "EOR.W  D0, D0");
    assert_eq!(bytes, 2);
}

// ========================================
// Group 0xC: AND/MULU/MULS/ABCD/EXG
// ========================================

#[test]
fn test_disasm_and_byte() {
    let mut bus = setup_bus_with_opcodes(&[0xC000]); // AND.B D0, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "AND.B  D0, D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_mulu() {
    let mut bus = setup_bus_with_opcodes(&[0xC0C0]); // MULU.W D0, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "MULU.W  D0, D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_muls() {
    let mut bus = setup_bus_with_opcodes(&[0xC1C0]); // MULS.W D0, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "MULS.W  D0, D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_abcd() {
    let mut bus = setup_bus_with_opcodes(&[0xC100]); // ABCD D0, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "ABCD    D0, D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_exg_dd() {
    let mut bus = setup_bus_with_opcodes(&[0xC140]); // EXG D0, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "EXG     D0, D0");
    assert_eq!(bytes, 2);
}

// ========================================
// Group 0xD: ADD/ADDA/ADDX
// ========================================

#[test]
fn test_disasm_add_word() {
    let mut bus = setup_bus_with_opcodes(&[0xD040]); // ADD.W D0, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "ADD.W  D0, D0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_adda_word() {
    let mut bus = setup_bus_with_opcodes(&[0xD0C0]); // ADDA.W D0, A0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "ADDA.W D0, A0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_adda_long() {
    let mut bus = setup_bus_with_opcodes(&[0xD1C0]); // ADDA.L D0, A0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "ADDA.L D0, A0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_addx() {
    let mut bus = setup_bus_with_opcodes(&[0xD140]); // ADDX.W D0, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "ADDX.W D0, D0");
    assert_eq!(bytes, 2);
}

// ========================================
// Group 0xE: Shifts and Rotates
// ========================================

#[test]
fn test_disasm_asl_immediate() {
    let mut bus = setup_bus_with_opcodes(&[0xE300]); // ASL.B #1, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert!(instr.contains("ASL"));
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_lsr_register() {
    let mut bus = setup_bus_with_opcodes(&[0xE228]); // LSR.B D1, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert!(instr.contains("LSR"));
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_rol_word() {
    let mut bus = setup_bus_with_opcodes(&[0xE378]); // ROL.W D1, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert!(instr.contains("ROL"));
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_roxr_long() {
    let mut bus = setup_bus_with_opcodes(&[0xE2B0]); // ROXR.L D1, D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert!(instr.contains("ROXR"));
    assert_eq!(bytes, 2);
}

// ========================================
// BCD Operations
// ========================================

#[test]
fn test_disasm_nbcd() {
    let mut bus = setup_bus_with_opcodes(&[0x4800]); // NBCD D0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "NBCD    D0");
    assert_eq!(bytes, 2);
}

// ========================================
// System Control
// ========================================

#[test]
fn test_disasm_trap() {
    let mut bus = setup_bus_with_opcodes(&[0x4E40]); // TRAP #0
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "TRAP    #0");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_trapv() {
    let mut bus = setup_bus_with_opcodes(&[0x4E76]); // TRAPV
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "TRAPV");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_rte() {
    let mut bus = setup_bus_with_opcodes(&[0x4E73]); // RTE
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "RTE");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_rtr() {
    let mut bus = setup_bus_with_opcodes(&[0x4E77]); // RTR
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "RTR");
    assert_eq!(bytes, 2);
}

#[test]
fn test_disasm_stop() {
    let mut bus = setup_bus_with_opcodes(&[0x4E72, 0x2700]); // STOP #$2700
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "STOP    #$2700");
    assert_eq!(bytes, 4);
}

#[test]
fn test_disasm_illegal() {
    let mut bus = setup_bus_with_opcodes(&[0x4AFC]); // ILLEGAL
    let (instr, bytes) = disasm::disassemble_one(&mut bus, 0);
    assert_eq!(instr, "ILLEGAL");
    assert_eq!(bytes, 2);
}
