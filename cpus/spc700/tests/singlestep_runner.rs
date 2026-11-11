//! SingleStepTests runner for the SPC700.
//!
//! The per-case harness + dataset loaders live in `spc700::singlestep`
//! (compiled via the crate's `singlestep` feature, which the self
//! dev-dependency in `Cargo.toml` turns on for tests). This file only wires
//! the dataset location + the `#[test]` entry points.
//!
//! ## Usage
//!
//! Smoke (always runs, parses one file and steps 10 cases):
//! ```bash
//! cargo test -p spc700 --test singlestep_runner test_singlestep_infrastructure
//! ```
//!
//! Per-opcode (the 1 000 cases of one opcode):
//! ```bash
//! cargo test --release -p spc700 --test singlestep_runner test_opcode_e8 -- --ignored --nocapture
//! ```
//!
//! Sample sweep (50 cases x 256 opcodes, quick, runs by default):
//! ```bash
//! cargo test -p spc700 --test singlestep_runner singlestep_sweep_sample -- --nocapture
//! ```
//!
//! Full sweep (1 000 cases x 256 opcodes, under a second in release):
//! ```bash
//! cargo test --release -p spc700 --test singlestep_runner singlestep_sweep_summary -- --ignored --nocapture
//! ```
//!
//! Env vars (honoured by the shared runner):
//! - `SINGLESTEP_INDEX=N`: diagnose a single case by index (skips the rest).
//! - `SINGLESTEP_DIAGNOSE=1`: print initial/final/diff for the first failure.

use spc700::singlestep;
use std::path::{Path, PathBuf};

fn base_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/singlesteptests/spc700")
}

/// Public entry used by the macro-generated per-opcode tests. Exits early if
/// the dataset is not present so CI without the fixture stays green.
fn run_opcode(opcode: u8) {
    let base = base_path();
    match singlestep::run_opcode(&base, opcode, 0) {
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
            "SingleStepTests SPC700 fixture/file missing under {} for opcode {:02X} - skipping.",
            base.display(),
            opcode
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
            "SingleStepTests SPC700 fixture not found at {} - skipping smoke.",
            base.display()
        );
        return;
    }
    // Use NOP (0x00), the simplest opcode.
    let Some(path) = singlestep::find_test_file(&base, 0x00) else {
        panic!("00.json missing under {}", base.display());
    };
    let tests = singlestep::load_test_file(&path).expect("parse 00.json");
    assert!(
        !tests.is_empty(),
        "00.json contains no cases under {}",
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
fn singlestep_sweep_sample() {
    // Quick sweep, 50 cases per opcode, runs by default (no #[ignore]).
    if singlestep::run_sweep(&base_path(), 50, "sample").is_none() {
        eprintln!(
            "SingleStepTests SPC700 fixture not found at {} - skipping sweep.",
            base_path().display()
        );
    }
}

#[test]
#[ignore = "SingleStep sweep - full pass, run with --release"]
fn singlestep_sweep_summary() {
    if singlestep::run_sweep(&base_path(), 10_000, "full").is_none() {
        eprintln!(
            "SingleStepTests SPC700 fixture not found at {} - skipping sweep.",
            base_path().display()
        );
    }
}

// ============================================================================
// Per-opcode tests (256)
// ============================================================================

macro_rules! opcode_tests {
    ($($opc:literal => $fn_name:ident;)+) => {
        $(
            #[test]
            #[ignore = "SingleStep per-opcode - run manually with --ignored --nocapture"]
            fn $fn_name() {
                run_opcode($opc);
            }
        )+
    };
}

#[rustfmt::skip]
opcode_tests! {
    0x00 => test_opcode_00; 0x01 => test_opcode_01; 0x02 => test_opcode_02; 0x03 => test_opcode_03;
    0x04 => test_opcode_04; 0x05 => test_opcode_05; 0x06 => test_opcode_06; 0x07 => test_opcode_07;
    0x08 => test_opcode_08; 0x09 => test_opcode_09; 0x0A => test_opcode_0a; 0x0B => test_opcode_0b;
    0x0C => test_opcode_0c; 0x0D => test_opcode_0d; 0x0E => test_opcode_0e; 0x0F => test_opcode_0f;
    0x10 => test_opcode_10; 0x11 => test_opcode_11; 0x12 => test_opcode_12; 0x13 => test_opcode_13;
    0x14 => test_opcode_14; 0x15 => test_opcode_15; 0x16 => test_opcode_16; 0x17 => test_opcode_17;
    0x18 => test_opcode_18; 0x19 => test_opcode_19; 0x1A => test_opcode_1a; 0x1B => test_opcode_1b;
    0x1C => test_opcode_1c; 0x1D => test_opcode_1d; 0x1E => test_opcode_1e; 0x1F => test_opcode_1f;
    0x20 => test_opcode_20; 0x21 => test_opcode_21; 0x22 => test_opcode_22; 0x23 => test_opcode_23;
    0x24 => test_opcode_24; 0x25 => test_opcode_25; 0x26 => test_opcode_26; 0x27 => test_opcode_27;
    0x28 => test_opcode_28; 0x29 => test_opcode_29; 0x2A => test_opcode_2a; 0x2B => test_opcode_2b;
    0x2C => test_opcode_2c; 0x2D => test_opcode_2d; 0x2E => test_opcode_2e; 0x2F => test_opcode_2f;
    0x30 => test_opcode_30; 0x31 => test_opcode_31; 0x32 => test_opcode_32; 0x33 => test_opcode_33;
    0x34 => test_opcode_34; 0x35 => test_opcode_35; 0x36 => test_opcode_36; 0x37 => test_opcode_37;
    0x38 => test_opcode_38; 0x39 => test_opcode_39; 0x3A => test_opcode_3a; 0x3B => test_opcode_3b;
    0x3C => test_opcode_3c; 0x3D => test_opcode_3d; 0x3E => test_opcode_3e; 0x3F => test_opcode_3f;
    0x40 => test_opcode_40; 0x41 => test_opcode_41; 0x42 => test_opcode_42; 0x43 => test_opcode_43;
    0x44 => test_opcode_44; 0x45 => test_opcode_45; 0x46 => test_opcode_46; 0x47 => test_opcode_47;
    0x48 => test_opcode_48; 0x49 => test_opcode_49; 0x4A => test_opcode_4a; 0x4B => test_opcode_4b;
    0x4C => test_opcode_4c; 0x4D => test_opcode_4d; 0x4E => test_opcode_4e; 0x4F => test_opcode_4f;
    0x50 => test_opcode_50; 0x51 => test_opcode_51; 0x52 => test_opcode_52; 0x53 => test_opcode_53;
    0x54 => test_opcode_54; 0x55 => test_opcode_55; 0x56 => test_opcode_56; 0x57 => test_opcode_57;
    0x58 => test_opcode_58; 0x59 => test_opcode_59; 0x5A => test_opcode_5a; 0x5B => test_opcode_5b;
    0x5C => test_opcode_5c; 0x5D => test_opcode_5d; 0x5E => test_opcode_5e; 0x5F => test_opcode_5f;
    0x60 => test_opcode_60; 0x61 => test_opcode_61; 0x62 => test_opcode_62; 0x63 => test_opcode_63;
    0x64 => test_opcode_64; 0x65 => test_opcode_65; 0x66 => test_opcode_66; 0x67 => test_opcode_67;
    0x68 => test_opcode_68; 0x69 => test_opcode_69; 0x6A => test_opcode_6a; 0x6B => test_opcode_6b;
    0x6C => test_opcode_6c; 0x6D => test_opcode_6d; 0x6E => test_opcode_6e; 0x6F => test_opcode_6f;
    0x70 => test_opcode_70; 0x71 => test_opcode_71; 0x72 => test_opcode_72; 0x73 => test_opcode_73;
    0x74 => test_opcode_74; 0x75 => test_opcode_75; 0x76 => test_opcode_76; 0x77 => test_opcode_77;
    0x78 => test_opcode_78; 0x79 => test_opcode_79; 0x7A => test_opcode_7a; 0x7B => test_opcode_7b;
    0x7C => test_opcode_7c; 0x7D => test_opcode_7d; 0x7E => test_opcode_7e; 0x7F => test_opcode_7f;
    0x80 => test_opcode_80; 0x81 => test_opcode_81; 0x82 => test_opcode_82; 0x83 => test_opcode_83;
    0x84 => test_opcode_84; 0x85 => test_opcode_85; 0x86 => test_opcode_86; 0x87 => test_opcode_87;
    0x88 => test_opcode_88; 0x89 => test_opcode_89; 0x8A => test_opcode_8a; 0x8B => test_opcode_8b;
    0x8C => test_opcode_8c; 0x8D => test_opcode_8d; 0x8E => test_opcode_8e; 0x8F => test_opcode_8f;
    0x90 => test_opcode_90; 0x91 => test_opcode_91; 0x92 => test_opcode_92; 0x93 => test_opcode_93;
    0x94 => test_opcode_94; 0x95 => test_opcode_95; 0x96 => test_opcode_96; 0x97 => test_opcode_97;
    0x98 => test_opcode_98; 0x99 => test_opcode_99; 0x9A => test_opcode_9a; 0x9B => test_opcode_9b;
    0x9C => test_opcode_9c; 0x9D => test_opcode_9d; 0x9E => test_opcode_9e; 0x9F => test_opcode_9f;
    0xA0 => test_opcode_a0; 0xA1 => test_opcode_a1; 0xA2 => test_opcode_a2; 0xA3 => test_opcode_a3;
    0xA4 => test_opcode_a4; 0xA5 => test_opcode_a5; 0xA6 => test_opcode_a6; 0xA7 => test_opcode_a7;
    0xA8 => test_opcode_a8; 0xA9 => test_opcode_a9; 0xAA => test_opcode_aa; 0xAB => test_opcode_ab;
    0xAC => test_opcode_ac; 0xAD => test_opcode_ad; 0xAE => test_opcode_ae; 0xAF => test_opcode_af;
    0xB0 => test_opcode_b0; 0xB1 => test_opcode_b1; 0xB2 => test_opcode_b2; 0xB3 => test_opcode_b3;
    0xB4 => test_opcode_b4; 0xB5 => test_opcode_b5; 0xB6 => test_opcode_b6; 0xB7 => test_opcode_b7;
    0xB8 => test_opcode_b8; 0xB9 => test_opcode_b9; 0xBA => test_opcode_ba; 0xBB => test_opcode_bb;
    0xBC => test_opcode_bc; 0xBD => test_opcode_bd; 0xBE => test_opcode_be; 0xBF => test_opcode_bf;
    0xC0 => test_opcode_c0; 0xC1 => test_opcode_c1; 0xC2 => test_opcode_c2; 0xC3 => test_opcode_c3;
    0xC4 => test_opcode_c4; 0xC5 => test_opcode_c5; 0xC6 => test_opcode_c6; 0xC7 => test_opcode_c7;
    0xC8 => test_opcode_c8; 0xC9 => test_opcode_c9; 0xCA => test_opcode_ca; 0xCB => test_opcode_cb;
    0xCC => test_opcode_cc; 0xCD => test_opcode_cd; 0xCE => test_opcode_ce; 0xCF => test_opcode_cf;
    0xD0 => test_opcode_d0; 0xD1 => test_opcode_d1; 0xD2 => test_opcode_d2; 0xD3 => test_opcode_d3;
    0xD4 => test_opcode_d4; 0xD5 => test_opcode_d5; 0xD6 => test_opcode_d6; 0xD7 => test_opcode_d7;
    0xD8 => test_opcode_d8; 0xD9 => test_opcode_d9; 0xDA => test_opcode_da; 0xDB => test_opcode_db;
    0xDC => test_opcode_dc; 0xDD => test_opcode_dd; 0xDE => test_opcode_de; 0xDF => test_opcode_df;
    0xE0 => test_opcode_e0; 0xE1 => test_opcode_e1; 0xE2 => test_opcode_e2; 0xE3 => test_opcode_e3;
    0xE4 => test_opcode_e4; 0xE5 => test_opcode_e5; 0xE6 => test_opcode_e6; 0xE7 => test_opcode_e7;
    0xE8 => test_opcode_e8; 0xE9 => test_opcode_e9; 0xEA => test_opcode_ea; 0xEB => test_opcode_eb;
    0xEC => test_opcode_ec; 0xED => test_opcode_ed; 0xEE => test_opcode_ee; 0xEF => test_opcode_ef;
    0xF0 => test_opcode_f0; 0xF1 => test_opcode_f1; 0xF2 => test_opcode_f2; 0xF3 => test_opcode_f3;
    0xF4 => test_opcode_f4; 0xF5 => test_opcode_f5; 0xF6 => test_opcode_f6; 0xF7 => test_opcode_f7;
    0xF8 => test_opcode_f8; 0xF9 => test_opcode_f9; 0xFA => test_opcode_fa; 0xFB => test_opcode_fb;
    0xFC => test_opcode_fc; 0xFD => test_opcode_fd; 0xFE => test_opcode_fe; 0xFF => test_opcode_ff;
}
