//! SingleStepTests runner for the WDC 65C816.
//!
//! The per-case harness + dataset loaders live in `wdc65816::singlestep`
//! (compiled via the crate's `singlestep` feature, which the self
//! dev-dependency in `Cargo.toml` turns on for tests). This file only wires
//! the dataset location + the `#[test]` entry points.
//!
//! ## Usage
//!
//! Smoke (always runs, parses `a9.e.json` and steps 10 cases):
//! ```bash
//! cargo test -p wdc65816 --test singlestep_runner test_singlestep_infrastructure
//! ```
//!
//! Per-opcode (10 000 cases of one opcode-mode):
//! ```bash
//! cargo test --release -p wdc65816 --test singlestep_runner test_opcode_a9_e -- --ignored --nocapture
//! ```
//!
//! Sample sweep (1 000 cases x 512 opcode-modes, ~5-10 min release):
//! ```bash
//! cargo test --release -p wdc65816 --test singlestep_runner singlestep_sweep_sample -- --ignored --nocapture
//! ```
//!
//! Full sweep (10 000 cases x 512 opcode-modes, ~60-90 min release):
//! ```bash
//! cargo test --release -p wdc65816 --test singlestep_runner singlestep_sweep_summary -- --ignored --nocapture
//! ```
//!
//! Env vars (honoured by the shared runner):
//! - `SINGLESTEP_INDEX=N`: diagnose a single case by index (skips the rest).
//! - `SINGLESTEP_DIAGNOSE=1`: print initial/final/diff for the first failure.

use std::path::{Path, PathBuf};
use wdc65816::singlestep::{self, TestMode};

fn base_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/singlesteptests/65816")
}

/// Public entry used by the macro-generated per-opcode tests. Exits early if
/// the dataset is not present so CI without the fixture stays green.
fn run_opcode(opcode: u8, mode: TestMode) {
    let base = base_path();
    match singlestep::run_opcode(&base, opcode, mode, 0) {
        Some(res) => {
            singlestep::print_file_result(&res);
            if res.failed > 0 || res.panicked > 0 {
                eprintln!(
                    "{}: {} failed, {} panicked (of {})",
                    res.label, res.failed, res.panicked, res.total
                );
            }
        }
        None => eprintln!(
            "SingleStepTests 65816 fixture/file missing under {} for {:02X}.{} - skipping.",
            base.display(),
            opcode,
            mode.suffix()
        ),
    }
}

// ============================================================================
// Smoke + sweep entry points
// ============================================================================

#[test]
fn test_singlestep_infrastructure() {
    let base = base_path();
    if !base.join("v1").is_dir() {
        eprintln!(
            "SingleStepTests 65816 fixture not found at {} - skipping smoke.",
            base.display()
        );
        return;
    }
    let Some(path) = singlestep::find_test_file(&base, 0xA9, TestMode::Emulation) else {
        panic!("a9.e.json missing under {}", base.display());
    };
    let tests = singlestep::load_test_file(&path).expect("parse a9.e.json");
    assert!(
        !tests.is_empty(),
        "a9.e.json contains no cases under {}",
        path.display()
    );

    let limit = tests.len().min(10);
    for test in tests.iter().take(limit) {
        let _ = singlestep::run_test(test);
    }
    println!(
        "infrastructure ok: parsed {} cases, ran {} without unrecoverable panic",
        tests.len(),
        limit
    );
}

#[test]
#[ignore = "SingleStep sweep - run manually with --ignored --nocapture"]
fn singlestep_sweep_sample() {
    if singlestep::run_sweep(&base_path(), 1_000, "sample").is_none() {
        eprintln!(
            "SingleStepTests 65816 fixture not found at {} - skipping sweep.",
            base_path().display()
        );
    }
}

#[test]
#[ignore = "SingleStep sweep - full pass (60-90 min release)"]
fn singlestep_sweep_summary() {
    if singlestep::run_sweep(&base_path(), 10_000, "full").is_none() {
        eprintln!(
            "SingleStepTests 65816 fixture not found at {} - skipping sweep.",
            base_path().display()
        );
    }
}

// ============================================================================
// Per-opcode tests (256 x 2 = 512)
// ============================================================================

macro_rules! opcode_tests {
    ($($opc:literal => ($e_fn:ident, $n_fn:ident);)+) => {
        $(
            #[test]
            #[ignore = "SingleStep per-opcode - run manually with --ignored --nocapture"]
            fn $e_fn() {
                run_opcode($opc, TestMode::Emulation);
            }

            #[test]
            #[ignore = "SingleStep per-opcode - run manually with --ignored --nocapture"]
            fn $n_fn() {
                run_opcode($opc, TestMode::Native);
            }
        )+
    };
}

opcode_tests! {
    0x00 => (test_opcode_00_e, test_opcode_00_n);
    0x01 => (test_opcode_01_e, test_opcode_01_n);
    0x02 => (test_opcode_02_e, test_opcode_02_n);
    0x03 => (test_opcode_03_e, test_opcode_03_n);
    0x04 => (test_opcode_04_e, test_opcode_04_n);
    0x05 => (test_opcode_05_e, test_opcode_05_n);
    0x06 => (test_opcode_06_e, test_opcode_06_n);
    0x07 => (test_opcode_07_e, test_opcode_07_n);
    0x08 => (test_opcode_08_e, test_opcode_08_n);
    0x09 => (test_opcode_09_e, test_opcode_09_n);
    0x0A => (test_opcode_0a_e, test_opcode_0a_n);
    0x0B => (test_opcode_0b_e, test_opcode_0b_n);
    0x0C => (test_opcode_0c_e, test_opcode_0c_n);
    0x0D => (test_opcode_0d_e, test_opcode_0d_n);
    0x0E => (test_opcode_0e_e, test_opcode_0e_n);
    0x0F => (test_opcode_0f_e, test_opcode_0f_n);
    0x10 => (test_opcode_10_e, test_opcode_10_n);
    0x11 => (test_opcode_11_e, test_opcode_11_n);
    0x12 => (test_opcode_12_e, test_opcode_12_n);
    0x13 => (test_opcode_13_e, test_opcode_13_n);
    0x14 => (test_opcode_14_e, test_opcode_14_n);
    0x15 => (test_opcode_15_e, test_opcode_15_n);
    0x16 => (test_opcode_16_e, test_opcode_16_n);
    0x17 => (test_opcode_17_e, test_opcode_17_n);
    0x18 => (test_opcode_18_e, test_opcode_18_n);
    0x19 => (test_opcode_19_e, test_opcode_19_n);
    0x1A => (test_opcode_1a_e, test_opcode_1a_n);
    0x1B => (test_opcode_1b_e, test_opcode_1b_n);
    0x1C => (test_opcode_1c_e, test_opcode_1c_n);
    0x1D => (test_opcode_1d_e, test_opcode_1d_n);
    0x1E => (test_opcode_1e_e, test_opcode_1e_n);
    0x1F => (test_opcode_1f_e, test_opcode_1f_n);
    0x20 => (test_opcode_20_e, test_opcode_20_n);
    0x21 => (test_opcode_21_e, test_opcode_21_n);
    0x22 => (test_opcode_22_e, test_opcode_22_n);
    0x23 => (test_opcode_23_e, test_opcode_23_n);
    0x24 => (test_opcode_24_e, test_opcode_24_n);
    0x25 => (test_opcode_25_e, test_opcode_25_n);
    0x26 => (test_opcode_26_e, test_opcode_26_n);
    0x27 => (test_opcode_27_e, test_opcode_27_n);
    0x28 => (test_opcode_28_e, test_opcode_28_n);
    0x29 => (test_opcode_29_e, test_opcode_29_n);
    0x2A => (test_opcode_2a_e, test_opcode_2a_n);
    0x2B => (test_opcode_2b_e, test_opcode_2b_n);
    0x2C => (test_opcode_2c_e, test_opcode_2c_n);
    0x2D => (test_opcode_2d_e, test_opcode_2d_n);
    0x2E => (test_opcode_2e_e, test_opcode_2e_n);
    0x2F => (test_opcode_2f_e, test_opcode_2f_n);
    0x30 => (test_opcode_30_e, test_opcode_30_n);
    0x31 => (test_opcode_31_e, test_opcode_31_n);
    0x32 => (test_opcode_32_e, test_opcode_32_n);
    0x33 => (test_opcode_33_e, test_opcode_33_n);
    0x34 => (test_opcode_34_e, test_opcode_34_n);
    0x35 => (test_opcode_35_e, test_opcode_35_n);
    0x36 => (test_opcode_36_e, test_opcode_36_n);
    0x37 => (test_opcode_37_e, test_opcode_37_n);
    0x38 => (test_opcode_38_e, test_opcode_38_n);
    0x39 => (test_opcode_39_e, test_opcode_39_n);
    0x3A => (test_opcode_3a_e, test_opcode_3a_n);
    0x3B => (test_opcode_3b_e, test_opcode_3b_n);
    0x3C => (test_opcode_3c_e, test_opcode_3c_n);
    0x3D => (test_opcode_3d_e, test_opcode_3d_n);
    0x3E => (test_opcode_3e_e, test_opcode_3e_n);
    0x3F => (test_opcode_3f_e, test_opcode_3f_n);
    0x40 => (test_opcode_40_e, test_opcode_40_n);
    0x41 => (test_opcode_41_e, test_opcode_41_n);
    0x42 => (test_opcode_42_e, test_opcode_42_n);
    0x43 => (test_opcode_43_e, test_opcode_43_n);
    0x44 => (test_opcode_44_e, test_opcode_44_n);
    0x45 => (test_opcode_45_e, test_opcode_45_n);
    0x46 => (test_opcode_46_e, test_opcode_46_n);
    0x47 => (test_opcode_47_e, test_opcode_47_n);
    0x48 => (test_opcode_48_e, test_opcode_48_n);
    0x49 => (test_opcode_49_e, test_opcode_49_n);
    0x4A => (test_opcode_4a_e, test_opcode_4a_n);
    0x4B => (test_opcode_4b_e, test_opcode_4b_n);
    0x4C => (test_opcode_4c_e, test_opcode_4c_n);
    0x4D => (test_opcode_4d_e, test_opcode_4d_n);
    0x4E => (test_opcode_4e_e, test_opcode_4e_n);
    0x4F => (test_opcode_4f_e, test_opcode_4f_n);
    0x50 => (test_opcode_50_e, test_opcode_50_n);
    0x51 => (test_opcode_51_e, test_opcode_51_n);
    0x52 => (test_opcode_52_e, test_opcode_52_n);
    0x53 => (test_opcode_53_e, test_opcode_53_n);
    0x54 => (test_opcode_54_e, test_opcode_54_n);
    0x55 => (test_opcode_55_e, test_opcode_55_n);
    0x56 => (test_opcode_56_e, test_opcode_56_n);
    0x57 => (test_opcode_57_e, test_opcode_57_n);
    0x58 => (test_opcode_58_e, test_opcode_58_n);
    0x59 => (test_opcode_59_e, test_opcode_59_n);
    0x5A => (test_opcode_5a_e, test_opcode_5a_n);
    0x5B => (test_opcode_5b_e, test_opcode_5b_n);
    0x5C => (test_opcode_5c_e, test_opcode_5c_n);
    0x5D => (test_opcode_5d_e, test_opcode_5d_n);
    0x5E => (test_opcode_5e_e, test_opcode_5e_n);
    0x5F => (test_opcode_5f_e, test_opcode_5f_n);
    0x60 => (test_opcode_60_e, test_opcode_60_n);
    0x61 => (test_opcode_61_e, test_opcode_61_n);
    0x62 => (test_opcode_62_e, test_opcode_62_n);
    0x63 => (test_opcode_63_e, test_opcode_63_n);
    0x64 => (test_opcode_64_e, test_opcode_64_n);
    0x65 => (test_opcode_65_e, test_opcode_65_n);
    0x66 => (test_opcode_66_e, test_opcode_66_n);
    0x67 => (test_opcode_67_e, test_opcode_67_n);
    0x68 => (test_opcode_68_e, test_opcode_68_n);
    0x69 => (test_opcode_69_e, test_opcode_69_n);
    0x6A => (test_opcode_6a_e, test_opcode_6a_n);
    0x6B => (test_opcode_6b_e, test_opcode_6b_n);
    0x6C => (test_opcode_6c_e, test_opcode_6c_n);
    0x6D => (test_opcode_6d_e, test_opcode_6d_n);
    0x6E => (test_opcode_6e_e, test_opcode_6e_n);
    0x6F => (test_opcode_6f_e, test_opcode_6f_n);
    0x70 => (test_opcode_70_e, test_opcode_70_n);
    0x71 => (test_opcode_71_e, test_opcode_71_n);
    0x72 => (test_opcode_72_e, test_opcode_72_n);
    0x73 => (test_opcode_73_e, test_opcode_73_n);
    0x74 => (test_opcode_74_e, test_opcode_74_n);
    0x75 => (test_opcode_75_e, test_opcode_75_n);
    0x76 => (test_opcode_76_e, test_opcode_76_n);
    0x77 => (test_opcode_77_e, test_opcode_77_n);
    0x78 => (test_opcode_78_e, test_opcode_78_n);
    0x79 => (test_opcode_79_e, test_opcode_79_n);
    0x7A => (test_opcode_7a_e, test_opcode_7a_n);
    0x7B => (test_opcode_7b_e, test_opcode_7b_n);
    0x7C => (test_opcode_7c_e, test_opcode_7c_n);
    0x7D => (test_opcode_7d_e, test_opcode_7d_n);
    0x7E => (test_opcode_7e_e, test_opcode_7e_n);
    0x7F => (test_opcode_7f_e, test_opcode_7f_n);
    0x80 => (test_opcode_80_e, test_opcode_80_n);
    0x81 => (test_opcode_81_e, test_opcode_81_n);
    0x82 => (test_opcode_82_e, test_opcode_82_n);
    0x83 => (test_opcode_83_e, test_opcode_83_n);
    0x84 => (test_opcode_84_e, test_opcode_84_n);
    0x85 => (test_opcode_85_e, test_opcode_85_n);
    0x86 => (test_opcode_86_e, test_opcode_86_n);
    0x87 => (test_opcode_87_e, test_opcode_87_n);
    0x88 => (test_opcode_88_e, test_opcode_88_n);
    0x89 => (test_opcode_89_e, test_opcode_89_n);
    0x8A => (test_opcode_8a_e, test_opcode_8a_n);
    0x8B => (test_opcode_8b_e, test_opcode_8b_n);
    0x8C => (test_opcode_8c_e, test_opcode_8c_n);
    0x8D => (test_opcode_8d_e, test_opcode_8d_n);
    0x8E => (test_opcode_8e_e, test_opcode_8e_n);
    0x8F => (test_opcode_8f_e, test_opcode_8f_n);
    0x90 => (test_opcode_90_e, test_opcode_90_n);
    0x91 => (test_opcode_91_e, test_opcode_91_n);
    0x92 => (test_opcode_92_e, test_opcode_92_n);
    0x93 => (test_opcode_93_e, test_opcode_93_n);
    0x94 => (test_opcode_94_e, test_opcode_94_n);
    0x95 => (test_opcode_95_e, test_opcode_95_n);
    0x96 => (test_opcode_96_e, test_opcode_96_n);
    0x97 => (test_opcode_97_e, test_opcode_97_n);
    0x98 => (test_opcode_98_e, test_opcode_98_n);
    0x99 => (test_opcode_99_e, test_opcode_99_n);
    0x9A => (test_opcode_9a_e, test_opcode_9a_n);
    0x9B => (test_opcode_9b_e, test_opcode_9b_n);
    0x9C => (test_opcode_9c_e, test_opcode_9c_n);
    0x9D => (test_opcode_9d_e, test_opcode_9d_n);
    0x9E => (test_opcode_9e_e, test_opcode_9e_n);
    0x9F => (test_opcode_9f_e, test_opcode_9f_n);
    0xA0 => (test_opcode_a0_e, test_opcode_a0_n);
    0xA1 => (test_opcode_a1_e, test_opcode_a1_n);
    0xA2 => (test_opcode_a2_e, test_opcode_a2_n);
    0xA3 => (test_opcode_a3_e, test_opcode_a3_n);
    0xA4 => (test_opcode_a4_e, test_opcode_a4_n);
    0xA5 => (test_opcode_a5_e, test_opcode_a5_n);
    0xA6 => (test_opcode_a6_e, test_opcode_a6_n);
    0xA7 => (test_opcode_a7_e, test_opcode_a7_n);
    0xA8 => (test_opcode_a8_e, test_opcode_a8_n);
    0xA9 => (test_opcode_a9_e, test_opcode_a9_n);
    0xAA => (test_opcode_aa_e, test_opcode_aa_n);
    0xAB => (test_opcode_ab_e, test_opcode_ab_n);
    0xAC => (test_opcode_ac_e, test_opcode_ac_n);
    0xAD => (test_opcode_ad_e, test_opcode_ad_n);
    0xAE => (test_opcode_ae_e, test_opcode_ae_n);
    0xAF => (test_opcode_af_e, test_opcode_af_n);
    0xB0 => (test_opcode_b0_e, test_opcode_b0_n);
    0xB1 => (test_opcode_b1_e, test_opcode_b1_n);
    0xB2 => (test_opcode_b2_e, test_opcode_b2_n);
    0xB3 => (test_opcode_b3_e, test_opcode_b3_n);
    0xB4 => (test_opcode_b4_e, test_opcode_b4_n);
    0xB5 => (test_opcode_b5_e, test_opcode_b5_n);
    0xB6 => (test_opcode_b6_e, test_opcode_b6_n);
    0xB7 => (test_opcode_b7_e, test_opcode_b7_n);
    0xB8 => (test_opcode_b8_e, test_opcode_b8_n);
    0xB9 => (test_opcode_b9_e, test_opcode_b9_n);
    0xBA => (test_opcode_ba_e, test_opcode_ba_n);
    0xBB => (test_opcode_bb_e, test_opcode_bb_n);
    0xBC => (test_opcode_bc_e, test_opcode_bc_n);
    0xBD => (test_opcode_bd_e, test_opcode_bd_n);
    0xBE => (test_opcode_be_e, test_opcode_be_n);
    0xBF => (test_opcode_bf_e, test_opcode_bf_n);
    0xC0 => (test_opcode_c0_e, test_opcode_c0_n);
    0xC1 => (test_opcode_c1_e, test_opcode_c1_n);
    0xC2 => (test_opcode_c2_e, test_opcode_c2_n);
    0xC3 => (test_opcode_c3_e, test_opcode_c3_n);
    0xC4 => (test_opcode_c4_e, test_opcode_c4_n);
    0xC5 => (test_opcode_c5_e, test_opcode_c5_n);
    0xC6 => (test_opcode_c6_e, test_opcode_c6_n);
    0xC7 => (test_opcode_c7_e, test_opcode_c7_n);
    0xC8 => (test_opcode_c8_e, test_opcode_c8_n);
    0xC9 => (test_opcode_c9_e, test_opcode_c9_n);
    0xCA => (test_opcode_ca_e, test_opcode_ca_n);
    0xCB => (test_opcode_cb_e, test_opcode_cb_n);
    0xCC => (test_opcode_cc_e, test_opcode_cc_n);
    0xCD => (test_opcode_cd_e, test_opcode_cd_n);
    0xCE => (test_opcode_ce_e, test_opcode_ce_n);
    0xCF => (test_opcode_cf_e, test_opcode_cf_n);
    0xD0 => (test_opcode_d0_e, test_opcode_d0_n);
    0xD1 => (test_opcode_d1_e, test_opcode_d1_n);
    0xD2 => (test_opcode_d2_e, test_opcode_d2_n);
    0xD3 => (test_opcode_d3_e, test_opcode_d3_n);
    0xD4 => (test_opcode_d4_e, test_opcode_d4_n);
    0xD5 => (test_opcode_d5_e, test_opcode_d5_n);
    0xD6 => (test_opcode_d6_e, test_opcode_d6_n);
    0xD7 => (test_opcode_d7_e, test_opcode_d7_n);
    0xD8 => (test_opcode_d8_e, test_opcode_d8_n);
    0xD9 => (test_opcode_d9_e, test_opcode_d9_n);
    0xDA => (test_opcode_da_e, test_opcode_da_n);
    0xDB => (test_opcode_db_e, test_opcode_db_n);
    0xDC => (test_opcode_dc_e, test_opcode_dc_n);
    0xDD => (test_opcode_dd_e, test_opcode_dd_n);
    0xDE => (test_opcode_de_e, test_opcode_de_n);
    0xDF => (test_opcode_df_e, test_opcode_df_n);
    0xE0 => (test_opcode_e0_e, test_opcode_e0_n);
    0xE1 => (test_opcode_e1_e, test_opcode_e1_n);
    0xE2 => (test_opcode_e2_e, test_opcode_e2_n);
    0xE3 => (test_opcode_e3_e, test_opcode_e3_n);
    0xE4 => (test_opcode_e4_e, test_opcode_e4_n);
    0xE5 => (test_opcode_e5_e, test_opcode_e5_n);
    0xE6 => (test_opcode_e6_e, test_opcode_e6_n);
    0xE7 => (test_opcode_e7_e, test_opcode_e7_n);
    0xE8 => (test_opcode_e8_e, test_opcode_e8_n);
    0xE9 => (test_opcode_e9_e, test_opcode_e9_n);
    0xEA => (test_opcode_ea_e, test_opcode_ea_n);
    0xEB => (test_opcode_eb_e, test_opcode_eb_n);
    0xEC => (test_opcode_ec_e, test_opcode_ec_n);
    0xED => (test_opcode_ed_e, test_opcode_ed_n);
    0xEE => (test_opcode_ee_e, test_opcode_ee_n);
    0xEF => (test_opcode_ef_e, test_opcode_ef_n);
    0xF0 => (test_opcode_f0_e, test_opcode_f0_n);
    0xF1 => (test_opcode_f1_e, test_opcode_f1_n);
    0xF2 => (test_opcode_f2_e, test_opcode_f2_n);
    0xF3 => (test_opcode_f3_e, test_opcode_f3_n);
    0xF4 => (test_opcode_f4_e, test_opcode_f4_n);
    0xF5 => (test_opcode_f5_e, test_opcode_f5_n);
    0xF6 => (test_opcode_f6_e, test_opcode_f6_n);
    0xF7 => (test_opcode_f7_e, test_opcode_f7_n);
    0xF8 => (test_opcode_f8_e, test_opcode_f8_n);
    0xF9 => (test_opcode_f9_e, test_opcode_f9_n);
    0xFA => (test_opcode_fa_e, test_opcode_fa_n);
    0xFB => (test_opcode_fb_e, test_opcode_fb_n);
    0xFC => (test_opcode_fc_e, test_opcode_fc_n);
    0xFD => (test_opcode_fd_e, test_opcode_fd_n);
    0xFE => (test_opcode_fe_e, test_opcode_fe_n);
    0xFF => (test_opcode_ff_e, test_opcode_ff_n);
}
