//! Tom Harte 68000 Test Suite Runner
//!
//! Runs the comprehensive ProcessorTests test suite to validate CPU implementation.
//! The 68000's side of the cases is `tom_harte::UnderTest` (`tests/tom_harte/mod.rs`);
//! retro-core's runner (`retro_core::testing::singlestep`) runs them. A group's test
//! fails when one of its cases does.
//!
//! ## Usage
//!
//! Run all tests for an instruction:
//! ```bash
//! cargo test --package sg1-core --test tom_harte_runner test_harte_add_b -- --ignored --nocapture
//! ```
//!
//! Run with detailed diagnostics for first failure:
//! ```bash
//! SINGLESTEP_DIAGNOSE=1 cargo test --package sg1-core --test tom_harte_runner test_harte_add_b -- --ignored --nocapture
//! ```
//!
//! Run a specific test by index:
//! ```bash
//! SINGLESTEP_INDEX=42 cargo test --package sg1-core --test tom_harte_runner test_harte_add_b -- --ignored --nocapture
//! ```
//!
//! `SINGLESTEP_FULL_DIAG=1` diagnoses the first 20 failures, and
//! `SINGLESTEP_CHECK_CYCLES=1` compares each case's cycle count with the
//! vector's. The older names `HARTE_INDEX`, `HARTE_DIAGNOSE`,
//! `HARTE_FULL_DIAG` and `HARTE_CHECK_CYCLES` still work: each is read when
//! its `SINGLESTEP_` name is not set.

mod tom_harte;

use retro_core::testing::singlestep::{
    load_cases, print_file_result, run_cases, vector_files, Diagnose, RunOptions,
};
use std::collections::BTreeMap;
use tom_harte::{vector_dir, vector_file, HarteTest, UnderTest};

/// Test runner for a specific instruction file
fn run_instruction_tests(instruction_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let test_file = vector_file(instruction_name)
        .ok_or_else(|| format!("Test file for {} not found", instruction_name))?;
    let tests: Vec<HarteTest> = load_cases(&test_file)?;
    let options = RunOptions::from_env_with_fallback(0, "HARTE");
    let result = run_cases::<UnderTest>(&tests, instruction_name, &options);
    print_file_result(&result);

    let failures = result.counts.failures();
    if failures == 0 {
        return Ok(());
    }
    if options.only_case.is_none() {
        print_failure_analysis(&tests, &result.failing_cases);
        if options.diagnose == Diagnose::No {
            println!("\nTIP: Run with SINGLESTEP_DIAGNOSE=1 for detailed first failure analysis");
            println!("TIP: Run with SINGLESTEP_FULL_DIAG=1 to see all failures (limited to 20)");
            if let Some(&idx) = result.failing_cases.first() {
                println!(
                    "TIP: Run with SINGLESTEP_INDEX={} to test only that case",
                    idx
                );
            }
        }
    }
    Err(format!("{} tests failed", failures).into())
}

/// The opcodes of the failing cases, the most common first, and for a
/// MOVE the addressing modes of its operands.
fn print_failure_analysis(tests: &[HarteTest], failing: &[usize]) {
    let mut failure_opcodes: BTreeMap<u16, u32> = BTreeMap::new();
    let mut failure_dest_modes: BTreeMap<u8, u32> = BTreeMap::new();
    let mut failure_src_modes: BTreeMap<u8, u32> = BTreeMap::new();
    for &i in failing {
        let opcode = tests[i].initial.prefetch[0];
        *failure_opcodes.entry(opcode).or_insert(0) += 1;

        // For MOVE instructions, track EA modes
        if (opcode & 0xC000) == 0x0000 && (opcode & 0x3000) != 0x0000 {
            let dst_mode = ((opcode >> 6) & 0x7) as u8;
            // Source mode is bits 5-3, not bits 2-0
            let src_mode = ((opcode >> 3) & 0x7) as u8;
            *failure_dest_modes.entry(dst_mode).or_insert(0) += 1;
            *failure_src_modes.entry(src_mode).or_insert(0) += 1;
        }
    }

    // Print most common failing opcodes
    println!("\n--- Most Common Failing Opcodes ---");
    let mut sorted_opcodes: Vec<_> = failure_opcodes.into_iter().collect();
    sorted_opcodes.sort_by_key(|e| std::cmp::Reverse(e.1));
    for (opcode, count) in sorted_opcodes.iter().take(50) {
        println!("  {:04X}: {} failures", opcode, count);
    }

    // For MOVE instructions, print EA mode analysis
    if !failure_dest_modes.is_empty() {
        let failed = failing.len();
        println!(
            "\n--- MOVE Instruction EA Mode Analysis (ALL {} failures) ---",
            failed
        );
        for (title, modes) in [
            ("Destination EA Modes:", failure_dest_modes),
            ("Source EA Modes:", failure_src_modes),
        ] {
            println!("\n{}", title);
            let mut sorted: Vec<_> = modes.into_iter().collect();
            sorted.sort_by_key(|e| std::cmp::Reverse(e.1));
            for (mode, count) in &sorted {
                let mode_name = match mode {
                    0 => "Dn",
                    1 => "An",
                    2 => "(An)",
                    3 => "(An)+",
                    4 => "-(An)",
                    5 => "(d16,An)",
                    6 => "(d8,An,Xn)",
                    7 => "mode 7 (xxx/imm/PC)",
                    _ => "unknown",
                };
                let percentage = (*count as f64 / failed as f64) * 100.0;
                println!(
                    "  mode={} {}: {} failures ({:.1}%)",
                    mode, mode_name, count, percentage
                );
            }
        }
    }
}

// Individual test for each instruction
// Start with simple instructions first

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_moveq() {
    run_instruction_tests("MOVE.q").expect("MOVE.q tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_move_b() {
    run_instruction_tests("MOVE.b").expect("MOVE.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_add_b() {
    run_instruction_tests("ADD.b").expect("ADD.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_nop() {
    run_instruction_tests("NOP").expect("NOP tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_clr_b() {
    run_instruction_tests("CLR.b").expect("CLR.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_add_l() {
    run_instruction_tests("ADD.l").expect("ADD.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_add_w() {
    run_instruction_tests("ADD.w").expect("ADD.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_adda_w() {
    run_instruction_tests("ADDA.w").expect("ADDA.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_adda_l() {
    run_instruction_tests("ADDA.l").expect("ADDA.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_addx_b() {
    run_instruction_tests("ADDX.b").expect("ADDX.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_addx_w() {
    run_instruction_tests("ADDX.w").expect("ADDX.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_addx_l() {
    run_instruction_tests("ADDX.l").expect("ADDX.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_move_w() {
    run_instruction_tests("MOVE.w").expect("MOVE.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_move_l() {
    run_instruction_tests("MOVE.l").expect("MOVE.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_movea_w() {
    run_instruction_tests("MOVEA.w").expect("MOVEA.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_movea_l() {
    run_instruction_tests("MOVEA.l").expect("MOVEA.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_movem_w() {
    run_instruction_tests("MOVEM.w").expect("MOVEM.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_movem_l() {
    run_instruction_tests("MOVEM.l").expect("MOVEM.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_movep_w() {
    run_instruction_tests("MOVEP.w").expect("MOVEP.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_movep_l() {
    run_instruction_tests("MOVEP.l").expect("MOVEP.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_clr_w() {
    run_instruction_tests("CLR.w").expect("CLR.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_clr_l() {
    run_instruction_tests("CLR.l").expect("CLR.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_exg() {
    run_instruction_tests("EXG").expect("EXG tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_swap() {
    run_instruction_tests("SWAP").expect("SWAP tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_link() {
    run_instruction_tests("LINK").expect("LINK tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_unlink() {
    run_instruction_tests("UNLINK").expect("UNLINK tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_sub_b() {
    run_instruction_tests("SUB.b").expect("SUB.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_sub_w() {
    run_instruction_tests("SUB.w").expect("SUB.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_sub_l() {
    run_instruction_tests("SUB.l").expect("SUB.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_suba_w() {
    run_instruction_tests("SUBA.w").expect("SUBA.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_suba_l() {
    run_instruction_tests("SUBA.l").expect("SUBA.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_subx_b() {
    run_instruction_tests("SUBX.b").expect("SUBX.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_subx_w() {
    run_instruction_tests("SUBX.w").expect("SUBX.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_subx_l() {
    run_instruction_tests("SUBX.l").expect("SUBX.l tests failed");
}

// ============================================================================
// CMP Family Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_cmp_b() {
    run_instruction_tests("CMP.b").expect("CMP.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_cmp_w() {
    run_instruction_tests("CMP.w").expect("CMP.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_cmp_l() {
    run_instruction_tests("CMP.l").expect("CMP.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_cmpa_w() {
    run_instruction_tests("CMPA.w").expect("CMPA.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_cmpa_l() {
    run_instruction_tests("CMPA.l").expect("CMPA.l tests failed");
}

// ============================================================================
// NEG Family Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_neg_b() {
    run_instruction_tests("NEG.b").expect("NEG.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_neg_w() {
    run_instruction_tests("NEG.w").expect("NEG.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_neg_l() {
    run_instruction_tests("NEG.l").expect("NEG.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_negx_b() {
    run_instruction_tests("NEGX.b").expect("NEGX.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_negx_w() {
    run_instruction_tests("NEGX.w").expect("NEGX.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_negx_l() {
    run_instruction_tests("NEGX.l").expect("NEGX.l tests failed");
}

// ============================================================================
// MUL/DIV Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_mulu() {
    run_instruction_tests("MULU").expect("MULU tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_muls() {
    run_instruction_tests("MULS").expect("MULS tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_divu() {
    run_instruction_tests("DIVU").expect("DIVU tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_divs() {
    run_instruction_tests("DIVS").expect("DIVS tests failed");
}

// ============================================================================
// BCD Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_abcd() {
    run_instruction_tests("ABCD").expect("ABCD tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_sbcd() {
    run_instruction_tests("SBCD").expect("SBCD tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_nbcd() {
    run_instruction_tests("NBCD").expect("NBCD tests failed");
}

// ============================================================================
// LEA / PEA Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_lea() {
    run_instruction_tests("LEA").expect("LEA tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_pea() {
    run_instruction_tests("PEA").expect("PEA tests failed");
}

// ============================================================================
// Logical Family Tests
// ============================================================================
#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_and_b() {
    run_instruction_tests("AND.b").expect("AND.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_and_w() {
    run_instruction_tests("AND.w").expect("AND.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_and_l() {
    run_instruction_tests("AND.l").expect("AND.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_or_b() {
    run_instruction_tests("OR.b").expect("OR.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_or_w() {
    run_instruction_tests("OR.w").expect("OR.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_or_l() {
    run_instruction_tests("OR.l").expect("OR.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_eor_b() {
    run_instruction_tests("EOR.b").expect("EOR.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_eor_w() {
    run_instruction_tests("EOR.w").expect("EOR.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_eor_l() {
    run_instruction_tests("EOR.l").expect("EOR.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_not_b() {
    run_instruction_tests("NOT.b").expect("NOT.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_not_w() {
    run_instruction_tests("NOT.w").expect("NOT.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_not_l() {
    run_instruction_tests("NOT.l").expect("NOT.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_tst_b() {
    run_instruction_tests("TST.b").expect("TST.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_tst_w() {
    run_instruction_tests("TST.w").expect("TST.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_tst_l() {
    run_instruction_tests("TST.l").expect("TST.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_btst() {
    run_instruction_tests("BTST").expect("BTST tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_bset() {
    run_instruction_tests("BSET").expect("BSET tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_bclr() {
    run_instruction_tests("BCLR").expect("BCLR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_bchg() {
    run_instruction_tests("BCHG").expect("BCHG tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_tas() {
    run_instruction_tests("TAS").expect("TAS tests failed");
}

// ============================================================================
// Shifts Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_lsl_b() {
    run_instruction_tests("LSL.b").expect("LSL.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_lsl_w() {
    run_instruction_tests("LSL.w").expect("LSL.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_lsl_l() {
    run_instruction_tests("LSL.l").expect("LSL.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_lsr_b() {
    run_instruction_tests("LSR.b").expect("LSR.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_lsr_w() {
    run_instruction_tests("LSR.w").expect("LSR.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_lsr_l() {
    run_instruction_tests("LSR.l").expect("LSR.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_asl_b() {
    run_instruction_tests("ASL.b").expect("ASL.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_asl_w() {
    run_instruction_tests("ASL.w").expect("ASL.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_asl_l() {
    run_instruction_tests("ASL.l").expect("ASL.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_asr_b() {
    run_instruction_tests("ASR.b").expect("ASR.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_asr_w() {
    run_instruction_tests("ASR.w").expect("ASR.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_asr_l() {
    run_instruction_tests("ASR.l").expect("ASR.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_rol_b() {
    run_instruction_tests("ROL.b").expect("ROL.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_rol_w() {
    run_instruction_tests("ROL.w").expect("ROL.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_rol_l() {
    run_instruction_tests("ROL.l").expect("ROL.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_ror_b() {
    run_instruction_tests("ROR.b").expect("ROR.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_ror_w() {
    run_instruction_tests("ROR.w").expect("ROR.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_ror_l() {
    run_instruction_tests("ROR.l").expect("ROR.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_roxl_b() {
    run_instruction_tests("ROXL.b").expect("ROXL.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_roxl_w() {
    run_instruction_tests("ROXL.w").expect("ROXL.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_roxl_l() {
    run_instruction_tests("ROXL.l").expect("ROXL.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_roxr_b() {
    run_instruction_tests("ROXR.b").expect("ROXR.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_roxr_w() {
    run_instruction_tests("ROXR.w").expect("ROXR.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_roxr_l() {
    run_instruction_tests("ROXR.l").expect("ROXR.l tests failed");
}

// ============================================================================
// Branch Family Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_bcc() {
    run_instruction_tests("Bcc").expect("Bcc tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_bsr() {
    run_instruction_tests("BSR").expect("BSR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_dbcc() {
    run_instruction_tests("DBcc").expect("DBcc tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_scc() {
    run_instruction_tests("Scc").expect("Scc tests failed");
}

// ============================================================================
// Control Flow Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_rts() {
    run_instruction_tests("RTS").expect("RTS tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_rtr() {
    run_instruction_tests("RTR").expect("RTR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_jmp() {
    run_instruction_tests("JMP").expect("JMP tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_jsr() {
    run_instruction_tests("JSR").expect("JSR tests failed");
}

// ============================================================================
// System Family Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_reset() {
    run_instruction_tests("RESET").expect("RESET tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_rte() {
    run_instruction_tests("RTE").expect("RTE tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_trap() {
    run_instruction_tests("TRAP").expect("TRAP tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_trapv() {
    run_instruction_tests("TRAPV").expect("TRAPV tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_chk() {
    run_instruction_tests("CHK").expect("CHK tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_move_to_sr() {
    run_instruction_tests("MOVEtoSR").expect("MOVEtoSR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_move_from_sr() {
    run_instruction_tests("MOVEfromSR").expect("MOVEfromSR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_move_to_ccr() {
    run_instruction_tests("MOVEtoCCR").expect("MOVEtoCCR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_move_usp() {
    run_instruction_tests("MOVEtoUSP").expect("MOVEtoUSP tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_move_from_usp() {
    run_instruction_tests("MOVEfromUSP").expect("MOVEfromUSP tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_andi_to_sr() {
    run_instruction_tests("ANDItoSR").expect("ANDItoSR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_andi_to_ccr() {
    run_instruction_tests("ANDItoCCR").expect("ANDItoCCR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_ori_to_sr() {
    run_instruction_tests("ORItoSR").expect("ORItoSR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_ori_to_ccr() {
    run_instruction_tests("ORItoCCR").expect("ORItoCCR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_eor_to_sr() {
    run_instruction_tests("EORItoSR").expect("EORItoSR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_eor_to_ccr() {
    run_instruction_tests("EORItoCCR").expect("EORItoCCR tests failed");
}

/// Smoke test: just load one file to verify infrastructure works
#[test]
fn test_harte_infrastructure() {
    let test_files = vector_files(&vector_dir());

    // The Tom Harte vectors are downloaded into `tests/tom_harte_repo` (the
    // README says how). Without them the per-instruction cases are all
    // `#[ignore]`d and never run; this smoke test must likewise skip rather
    // than fail, so a plain `cargo test --workspace` stays green.
    if test_files.is_empty() {
        eprintln!(
            "skipping test_harte_infrastructure: Tom Harte vectors not downloaded \
             (tests/tom_harte_repo is empty)"
        );
        return;
    }

    // The vectors are present: verify the loader actually parses the first
    // file so infra breakage is caught.
    let first_file = &test_files[0].1;
    let tests = load_cases::<HarteTest>(first_file).expect("Failed to load test file");
    assert!(!tests.is_empty(), "Test file was empty");

    println!("Tom Harte infrastructure working!");
    println!("   Found {} test files", test_files.len());
    println!("   First file has {} tests", tests.len());
}
