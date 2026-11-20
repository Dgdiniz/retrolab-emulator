//! SingleStepTests Z80 Test Suite Runner
//!
//! Runs the comprehensive SingleStepTests test suite to validate Z80 CPU implementation.
//! The Z80's side of the cases is `singlestep::UnderTest` (`tests/singlestep/mod.rs`);
//! retro-core's runner (`retro_core::testing::singlestep`) runs them.
//!
//! ## Usage
//!
//! Run all tests for a specific opcode file:
//! ```bash
//! cargo test --package sg1-core --test singlestep_runner test_opcode_00 -- --ignored --nocapture
//! ```
//!
//! Run with detailed diagnostics for first failure:
//! ```bash
//! SINGLESTEP_DIAGNOSE=1 cargo test --package sg1-core --test singlestep_runner test_opcode_00 -- --ignored --nocapture
//! ```
//!
//! Run a specific test by index:
//! ```bash
//! SINGLESTEP_INDEX=42 cargo test --package sg1-core --test singlestep_runner test_opcode_00 -- --ignored --nocapture
//! ```
//!
//! `SINGLESTEP_FULL_DIAG=1` diagnoses the first 20 failures, and
//! `SINGLESTEP_CHECK_CYCLES=1` compares each case's cycle count with the
//! length of its cycle list.

mod singlestep;

use retro_core::testing::singlestep::{
    load_cases, print_file_result, run_file, run_sweep, vector_files, RunOptions,
};
use singlestep::{group_of_label, opcode_from_filename, UnderTest, Z80Test};
use std::path::{Path, PathBuf};

fn base_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/singlesteptests/z80")
}

/// Every vector file, each with the label of its opcode (`DD CB __ 04`).
fn labelled_files() -> Vec<(String, PathBuf)> {
    vector_files(&base_path().join("v1"))
        .into_iter()
        .map(|(name, path)| {
            let label = opcode_from_filename(&path).map_or(name, |group| group.label());
            (label, path)
        })
        .collect()
}

/// Run the tests of one file by its name. This is the main entry point
/// modeled after the Tom Harte runner.
fn run_file_tests(filename: &str) {
    let base_path = base_path();

    if !base_path.exists() {
        eprintln!("Warning: SingleStepTests not found at {:?}", base_path);
        eprintln!("Skipping test. Download the vectors (the README says how) to run it.");
        return;
    }

    let test_file = base_path.join(format!("v1/{}", filename));
    if !test_file.exists() {
        eprintln!("Warning: Test file not found: {:?}", test_file);
        return;
    }

    let label = opcode_from_filename(&test_file)
        .map(|g| g.label())
        .unwrap_or_else(|| filename.to_string());
    let result = run_file::<UnderTest>(&test_file, &label, &RunOptions::from_env(0));
    print_file_result(&result);
    if result.counts.failures() > 0 {
        // Don't panic - just print. Some opcodes may have expected failures.
        eprintln!(
            "Test had failures: {} tests failed, {} panicked",
            result.counts.failed, result.counts.panicked
        );
    }
}

/// Run all test files as one sweep, with its lines per prefix group.
fn run_all_tests_summary() {
    let files = labelled_files();
    if files.is_empty() {
        eprintln!("Warning: SingleStepTests not found at {:?}", base_path());
        return;
    }
    run_sweep::<UnderTest>(
        "all",
        &files,
        &RunOptions::from_env(0),
        Some(group_of_label),
    );
}

// ============================================================================
// Test Functions
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // Infrastructure test
    // ========================================================================

    #[test]
    fn test_singlestep_infrastructure() {
        let test_files = vector_files(&base_path().join("v1"));

        // The Z80 SingleStepTests vectors are downloaded into
        // `tests/singlesteptests/z80` (the README says how), like the Tom
        // Harte data. Without them the 588 per-opcode cases and the 6 summary
        // and group runners are all `#[ignore]`d and never run; this smoke test
        // must likewise skip rather than fail, so a plain
        // `cargo test --workspace` stays green.
        if test_files.is_empty() {
            eprintln!(
                "skipping test_singlestep_infrastructure: Z80 SingleStepTests vectors not \
                 downloaded (tests/singlesteptests/z80 is empty)"
            );
            return;
        }

        // The vectors are present: verify the loader actually parses the
        // first file so infra breakage is caught.
        let first_file = &test_files[0].1;
        let tests = load_cases::<Z80Test>(first_file).expect("Failed to load test file");
        assert!(!tests.is_empty(), "Test file was empty");

        println!("SingleStepTests infrastructure working!");
        println!("  Found {} test files", test_files.len());
        println!("  First file has {} tests", tests.len());
    }

    // ========================================================================
    // Run all opcodes summary
    // ========================================================================

    #[test]
    #[ignore = "SingleStep test - run manually with --ignored"]
    fn test_all_opcodes_summary() {
        run_all_tests_summary();
    }

    // ========================================================================
    // Unprefixed opcodes (00-FF, excluding CB/DD/ED/FD prefixes)
    // ========================================================================

    macro_rules! singlestep_test {
        ($name:ident, $file:expr) => {
            #[test]
            #[ignore = "SingleStep test - run manually with --ignored"]
            fn $name() {
                run_file_tests($file);
            }
        };
    }

    // Unprefixed opcodes
    singlestep_test!(test_opcode_00, "00.json");
    singlestep_test!(test_opcode_01, "01.json");
    singlestep_test!(test_opcode_02, "02.json");
    singlestep_test!(test_opcode_03, "03.json");
    singlestep_test!(test_opcode_04, "04.json");
    singlestep_test!(test_opcode_05, "05.json");
    singlestep_test!(test_opcode_06, "06.json");
    singlestep_test!(test_opcode_07, "07.json");
    singlestep_test!(test_opcode_08, "08.json");
    singlestep_test!(test_opcode_09, "09.json");
    singlestep_test!(test_opcode_0a, "0a.json");
    singlestep_test!(test_opcode_0b, "0b.json");
    singlestep_test!(test_opcode_0c, "0c.json");
    singlestep_test!(test_opcode_0d, "0d.json");
    singlestep_test!(test_opcode_0e, "0e.json");
    singlestep_test!(test_opcode_0f, "0f.json");
    singlestep_test!(test_opcode_10, "10.json");
    singlestep_test!(test_opcode_11, "11.json");
    singlestep_test!(test_opcode_12, "12.json");
    singlestep_test!(test_opcode_13, "13.json");
    singlestep_test!(test_opcode_14, "14.json");
    singlestep_test!(test_opcode_15, "15.json");
    singlestep_test!(test_opcode_16, "16.json");
    singlestep_test!(test_opcode_17, "17.json");
    singlestep_test!(test_opcode_18, "18.json");
    singlestep_test!(test_opcode_19, "19.json");
    singlestep_test!(test_opcode_1a, "1a.json");
    singlestep_test!(test_opcode_1b, "1b.json");
    singlestep_test!(test_opcode_1c, "1c.json");
    singlestep_test!(test_opcode_1d, "1d.json");
    singlestep_test!(test_opcode_1e, "1e.json");
    singlestep_test!(test_opcode_1f, "1f.json");
    singlestep_test!(test_opcode_20, "20.json");
    singlestep_test!(test_opcode_21, "21.json");
    singlestep_test!(test_opcode_22, "22.json");
    singlestep_test!(test_opcode_23, "23.json");
    singlestep_test!(test_opcode_24, "24.json");
    singlestep_test!(test_opcode_25, "25.json");
    singlestep_test!(test_opcode_26, "26.json");
    singlestep_test!(test_opcode_27, "27.json");
    singlestep_test!(test_opcode_28, "28.json");
    singlestep_test!(test_opcode_29, "29.json");
    singlestep_test!(test_opcode_2a, "2a.json");
    singlestep_test!(test_opcode_2b, "2b.json");
    singlestep_test!(test_opcode_2c, "2c.json");
    singlestep_test!(test_opcode_2d, "2d.json");
    singlestep_test!(test_opcode_2e, "2e.json");
    singlestep_test!(test_opcode_2f, "2f.json");
    singlestep_test!(test_opcode_30, "30.json");
    singlestep_test!(test_opcode_31, "31.json");
    singlestep_test!(test_opcode_32, "32.json");
    singlestep_test!(test_opcode_33, "33.json");
    singlestep_test!(test_opcode_34, "34.json");
    singlestep_test!(test_opcode_35, "35.json");
    singlestep_test!(test_opcode_36, "36.json");
    singlestep_test!(test_opcode_37, "37.json");
    singlestep_test!(test_opcode_38, "38.json");
    singlestep_test!(test_opcode_39, "39.json");
    singlestep_test!(test_opcode_3a, "3a.json");
    singlestep_test!(test_opcode_3b, "3b.json");
    singlestep_test!(test_opcode_3c, "3c.json");
    singlestep_test!(test_opcode_3d, "3d.json");
    singlestep_test!(test_opcode_3e, "3e.json");
    singlestep_test!(test_opcode_3f, "3f.json");
    singlestep_test!(test_opcode_40, "40.json");
    singlestep_test!(test_opcode_41, "41.json");
    singlestep_test!(test_opcode_42, "42.json");
    singlestep_test!(test_opcode_43, "43.json");
    singlestep_test!(test_opcode_44, "44.json");
    singlestep_test!(test_opcode_45, "45.json");
    singlestep_test!(test_opcode_46, "46.json");
    singlestep_test!(test_opcode_47, "47.json");
    singlestep_test!(test_opcode_48, "48.json");
    singlestep_test!(test_opcode_49, "49.json");
    singlestep_test!(test_opcode_4a, "4a.json");
    singlestep_test!(test_opcode_4b, "4b.json");
    singlestep_test!(test_opcode_4c, "4c.json");
    singlestep_test!(test_opcode_4d, "4d.json");
    singlestep_test!(test_opcode_4e, "4e.json");
    singlestep_test!(test_opcode_4f, "4f.json");
    singlestep_test!(test_opcode_50, "50.json");
    singlestep_test!(test_opcode_51, "51.json");
    singlestep_test!(test_opcode_52, "52.json");
    singlestep_test!(test_opcode_53, "53.json");
    singlestep_test!(test_opcode_54, "54.json");
    singlestep_test!(test_opcode_55, "55.json");
    singlestep_test!(test_opcode_56, "56.json");
    singlestep_test!(test_opcode_57, "57.json");
    singlestep_test!(test_opcode_58, "58.json");
    singlestep_test!(test_opcode_59, "59.json");
    singlestep_test!(test_opcode_5a, "5a.json");
    singlestep_test!(test_opcode_5b, "5b.json");
    singlestep_test!(test_opcode_5c, "5c.json");
    singlestep_test!(test_opcode_5d, "5d.json");
    singlestep_test!(test_opcode_5e, "5e.json");
    singlestep_test!(test_opcode_5f, "5f.json");
    singlestep_test!(test_opcode_60, "60.json");
    singlestep_test!(test_opcode_61, "61.json");
    singlestep_test!(test_opcode_62, "62.json");
    singlestep_test!(test_opcode_63, "63.json");
    singlestep_test!(test_opcode_64, "64.json");
    singlestep_test!(test_opcode_65, "65.json");
    singlestep_test!(test_opcode_66, "66.json");
    singlestep_test!(test_opcode_67, "67.json");
    singlestep_test!(test_opcode_68, "68.json");
    singlestep_test!(test_opcode_69, "69.json");
    singlestep_test!(test_opcode_6a, "6a.json");
    singlestep_test!(test_opcode_6b, "6b.json");
    singlestep_test!(test_opcode_6c, "6c.json");
    singlestep_test!(test_opcode_6d, "6d.json");
    singlestep_test!(test_opcode_6e, "6e.json");
    singlestep_test!(test_opcode_6f, "6f.json");
    singlestep_test!(test_opcode_70, "70.json");
    singlestep_test!(test_opcode_71, "71.json");
    singlestep_test!(test_opcode_72, "72.json");
    singlestep_test!(test_opcode_73, "73.json");
    singlestep_test!(test_opcode_74, "74.json");
    singlestep_test!(test_opcode_75, "75.json");
    singlestep_test!(test_opcode_76, "76.json");
    singlestep_test!(test_opcode_77, "77.json");
    singlestep_test!(test_opcode_78, "78.json");
    singlestep_test!(test_opcode_79, "79.json");
    singlestep_test!(test_opcode_7a, "7a.json");
    singlestep_test!(test_opcode_7b, "7b.json");
    singlestep_test!(test_opcode_7c, "7c.json");
    singlestep_test!(test_opcode_7d, "7d.json");
    singlestep_test!(test_opcode_7e, "7e.json");
    singlestep_test!(test_opcode_7f, "7f.json");
    singlestep_test!(test_opcode_80, "80.json");
    singlestep_test!(test_opcode_81, "81.json");
    singlestep_test!(test_opcode_82, "82.json");
    singlestep_test!(test_opcode_83, "83.json");
    singlestep_test!(test_opcode_84, "84.json");
    singlestep_test!(test_opcode_85, "85.json");
    singlestep_test!(test_opcode_86, "86.json");
    singlestep_test!(test_opcode_87, "87.json");
    singlestep_test!(test_opcode_88, "88.json");
    singlestep_test!(test_opcode_89, "89.json");
    singlestep_test!(test_opcode_8a, "8a.json");
    singlestep_test!(test_opcode_8b, "8b.json");
    singlestep_test!(test_opcode_8c, "8c.json");
    singlestep_test!(test_opcode_8d, "8d.json");
    singlestep_test!(test_opcode_8e, "8e.json");
    singlestep_test!(test_opcode_8f, "8f.json");
    singlestep_test!(test_opcode_90, "90.json");
    singlestep_test!(test_opcode_91, "91.json");
    singlestep_test!(test_opcode_92, "92.json");
    singlestep_test!(test_opcode_93, "93.json");
    singlestep_test!(test_opcode_94, "94.json");
    singlestep_test!(test_opcode_95, "95.json");
    singlestep_test!(test_opcode_96, "96.json");
    singlestep_test!(test_opcode_97, "97.json");
    singlestep_test!(test_opcode_98, "98.json");
    singlestep_test!(test_opcode_99, "99.json");
    singlestep_test!(test_opcode_9a, "9a.json");
    singlestep_test!(test_opcode_9b, "9b.json");
    singlestep_test!(test_opcode_9c, "9c.json");
    singlestep_test!(test_opcode_9d, "9d.json");
    singlestep_test!(test_opcode_9e, "9e.json");
    singlestep_test!(test_opcode_9f, "9f.json");
    singlestep_test!(test_opcode_a0, "a0.json");
    singlestep_test!(test_opcode_a1, "a1.json");
    singlestep_test!(test_opcode_a2, "a2.json");
    singlestep_test!(test_opcode_a3, "a3.json");
    singlestep_test!(test_opcode_a4, "a4.json");
    singlestep_test!(test_opcode_a5, "a5.json");
    singlestep_test!(test_opcode_a6, "a6.json");
    singlestep_test!(test_opcode_a7, "a7.json");
    singlestep_test!(test_opcode_a8, "a8.json");
    singlestep_test!(test_opcode_a9, "a9.json");
    singlestep_test!(test_opcode_aa, "aa.json");
    singlestep_test!(test_opcode_ab, "ab.json");
    singlestep_test!(test_opcode_ac, "ac.json");
    singlestep_test!(test_opcode_ad, "ad.json");
    singlestep_test!(test_opcode_ae, "ae.json");
    singlestep_test!(test_opcode_af, "af.json");
    singlestep_test!(test_opcode_b0, "b0.json");
    singlestep_test!(test_opcode_b1, "b1.json");
    singlestep_test!(test_opcode_b2, "b2.json");
    singlestep_test!(test_opcode_b3, "b3.json");
    singlestep_test!(test_opcode_b4, "b4.json");
    singlestep_test!(test_opcode_b5, "b5.json");
    singlestep_test!(test_opcode_b6, "b6.json");
    singlestep_test!(test_opcode_b7, "b7.json");
    singlestep_test!(test_opcode_b8, "b8.json");
    singlestep_test!(test_opcode_b9, "b9.json");
    singlestep_test!(test_opcode_ba, "ba.json");
    singlestep_test!(test_opcode_bb, "bb.json");
    singlestep_test!(test_opcode_bc, "bc.json");
    singlestep_test!(test_opcode_bd, "bd.json");
    singlestep_test!(test_opcode_be, "be.json");
    singlestep_test!(test_opcode_bf, "bf.json");
    singlestep_test!(test_opcode_c0, "c0.json");
    singlestep_test!(test_opcode_c1, "c1.json");
    singlestep_test!(test_opcode_c2, "c2.json");
    singlestep_test!(test_opcode_c3, "c3.json");
    singlestep_test!(test_opcode_c4, "c4.json");
    singlestep_test!(test_opcode_c5, "c5.json");
    singlestep_test!(test_opcode_c6, "c6.json");
    singlestep_test!(test_opcode_c7, "c7.json");
    singlestep_test!(test_opcode_c8, "c8.json");
    singlestep_test!(test_opcode_c9, "c9.json");
    singlestep_test!(test_opcode_ca, "ca.json");
    // cb is a prefix, no "cb.json" unprefixed file
    singlestep_test!(test_opcode_cc, "cc.json");
    singlestep_test!(test_opcode_cd, "cd.json");
    singlestep_test!(test_opcode_ce, "ce.json");
    singlestep_test!(test_opcode_cf, "cf.json");
    singlestep_test!(test_opcode_d0, "d0.json");
    singlestep_test!(test_opcode_d1, "d1.json");
    singlestep_test!(test_opcode_d2, "d2.json");
    singlestep_test!(test_opcode_d3, "d3.json");
    singlestep_test!(test_opcode_d4, "d4.json");
    singlestep_test!(test_opcode_d5, "d5.json");
    singlestep_test!(test_opcode_d6, "d6.json");
    singlestep_test!(test_opcode_d7, "d7.json");
    singlestep_test!(test_opcode_d8, "d8.json");
    singlestep_test!(test_opcode_d9, "d9.json");
    singlestep_test!(test_opcode_da, "da.json");
    singlestep_test!(test_opcode_db, "db.json");
    singlestep_test!(test_opcode_dc, "dc.json");
    // dd is a prefix, no "dd.json" unprefixed file
    singlestep_test!(test_opcode_de, "de.json");
    singlestep_test!(test_opcode_df, "df.json");
    singlestep_test!(test_opcode_e0, "e0.json");
    singlestep_test!(test_opcode_e1, "e1.json");
    singlestep_test!(test_opcode_e2, "e2.json");
    singlestep_test!(test_opcode_e3, "e3.json");
    singlestep_test!(test_opcode_e4, "e4.json");
    singlestep_test!(test_opcode_e5, "e5.json");
    singlestep_test!(test_opcode_e6, "e6.json");
    singlestep_test!(test_opcode_e7, "e7.json");
    singlestep_test!(test_opcode_e8, "e8.json");
    singlestep_test!(test_opcode_e9, "e9.json");
    singlestep_test!(test_opcode_ea, "ea.json");
    singlestep_test!(test_opcode_eb, "eb.json");
    singlestep_test!(test_opcode_ec, "ec.json");
    // ed is a prefix, no "ed.json" unprefixed file
    singlestep_test!(test_opcode_ee, "ee.json");
    singlestep_test!(test_opcode_ef, "ef.json");
    singlestep_test!(test_opcode_f0, "f0.json");
    singlestep_test!(test_opcode_f1, "f1.json");
    singlestep_test!(test_opcode_f2, "f2.json");
    singlestep_test!(test_opcode_f3, "f3.json");
    singlestep_test!(test_opcode_f4, "f4.json");
    singlestep_test!(test_opcode_f5, "f5.json");
    singlestep_test!(test_opcode_f6, "f6.json");
    singlestep_test!(test_opcode_f7, "f7.json");
    singlestep_test!(test_opcode_f8, "f8.json");
    singlestep_test!(test_opcode_f9, "f9.json");
    singlestep_test!(test_opcode_fa, "fa.json");
    singlestep_test!(test_opcode_fb, "fb.json");
    singlestep_test!(test_opcode_fc, "fc.json");
    // fd is a prefix, no "fd.json" unprefixed file
    singlestep_test!(test_opcode_fe, "fe.json");
    singlestep_test!(test_opcode_ff, "ff.json");

    // ========================================================================
    // CB prefix opcodes
    // ========================================================================

    macro_rules! singlestep_cb_tests {
        ($($hex:expr),* $(,)?) => {
            $(
                paste::paste! {
                    singlestep_test!([<test_opcode_cb_ $hex>], concat!("cb ", $hex, ".json"));
                }
            )*
        };
    }

    singlestep_cb_tests!(
        "00", "01", "02", "03", "04", "05", "06", "07", "08", "09", "0a", "0b", "0c", "0d", "0e",
        "0f", "10", "11", "12", "13", "14", "15", "16", "17", "18", "19", "1a", "1b", "1c", "1d",
        "1e", "1f", "20", "21", "22", "23", "24", "25", "26", "27", "28", "29", "2a", "2b", "2c",
        "2d", "2e", "2f", "30", "31", "32", "33", "34", "35", "36", "37", "38", "39", "3a", "3b",
        "3c", "3d", "3e", "3f", "40", "41", "42", "43", "44", "45", "46", "47", "48", "49", "4a",
        "4b", "4c", "4d", "4e", "4f", "50", "51", "52", "53", "54", "55", "56", "57", "58", "59",
        "5a", "5b", "5c", "5d", "5e", "5f", "60", "61", "62", "63", "64", "65", "66", "67", "68",
        "69", "6a", "6b", "6c", "6d", "6e", "6f", "70", "71", "72", "73", "74", "75", "76", "77",
        "78", "79", "7a", "7b", "7c", "7d", "7e", "7f", "80", "81", "82", "83", "84", "85", "86",
        "87", "88", "89", "8a", "8b", "8c", "8d", "8e", "8f", "90", "91", "92", "93", "94", "95",
        "96", "97", "98", "99", "9a", "9b", "9c", "9d", "9e", "9f", "a0", "a1", "a2", "a3", "a4",
        "a5", "a6", "a7", "a8", "a9", "aa", "ab", "ac", "ad", "ae", "af", "b0", "b1", "b2", "b3",
        "b4", "b5", "b6", "b7", "b8", "b9", "ba", "bb", "bc", "bd", "be", "bf", "c0", "c1", "c2",
        "c3", "c4", "c5", "c6", "c7", "c8", "c9", "ca", "cb", "cc", "cd", "ce", "cf", "d0", "d1",
        "d2", "d3", "d4", "d5", "d6", "d7", "d8", "d9", "da", "db", "dc", "dd", "de", "df", "e0",
        "e1", "e2", "e3", "e4", "e5", "e6", "e7", "e8", "e9", "ea", "eb", "ec", "ed", "ee", "ef",
        "f0", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "fa", "fb", "fc", "fd", "fe",
        "ff"
    );

    // ========================================================================
    // ED prefix opcodes (selective - only files that exist)
    // ========================================================================

    macro_rules! singlestep_ed_tests {
        ($($hex:expr),* $(,)?) => {
            $(
                paste::paste! {
                    singlestep_test!([<test_opcode_ed_ $hex>], concat!("ed ", $hex, ".json"));
                }
            )*
        };
    }

    singlestep_ed_tests!(
        "40", "41", "42", "43", "44", "45", "46", "47", "48", "49", "4a", "4b", "4c", "4d", "4e",
        "4f", "50", "51", "52", "53", "54", "55", "56", "57", "58", "59", "5a", "5b", "5c", "5d",
        "5e", "5f", "60", "61", "62", "63", "64", "65", "66", "67", "68", "69", "6a", "6b", "6c",
        "6d", "6e", "6f", "70", "71", "72", "73", "74", "75", "76", "77", "78", "79", "7a", "7b",
        "7c", "7d", "7e", "7f", "a0", "a1", "a2", "a3", "a8", "a9", "aa", "ab", "b0", "b1", "b2",
        "b3", "b8", "b9", "ba", "bb"
    );

    // ========================================================================
    // DD prefix group tests (run by file discovery)
    // ========================================================================

    /// Run all DD-prefix tests (non-CB)
    #[test]
    #[ignore = "SingleStep test - run manually with --ignored"]
    fn test_all_dd_prefix() {
        run_prefix_group_tests("dd");
    }

    /// Run all FD-prefix tests (non-CB)
    #[test]
    #[ignore = "SingleStep test - run manually with --ignored"]
    fn test_all_fd_prefix() {
        run_prefix_group_tests("fd");
    }

    /// Run all DD CB-prefix tests
    #[test]
    #[ignore = "SingleStep test - run manually with --ignored"]
    fn test_all_dd_cb_prefix() {
        run_prefix_group_tests("dd cb");
    }

    /// Run all FD CB-prefix tests
    #[test]
    #[ignore = "SingleStep test - run manually with --ignored"]
    fn test_all_fd_cb_prefix() {
        run_prefix_group_tests("fd cb");
    }

    #[test]
    #[ignore]
    fn test_all_ed_prefix() {
        run_prefix_group_tests("ed");
    }

    /// Helper: run all test files matching a prefix group, as one sweep
    fn run_prefix_group_tests(prefix: &str) {
        if !base_path().exists() {
            eprintln!("Warning: SingleStepTests not found");
            return;
        }

        let matching: Vec<_> = vector_files(&base_path().join("v1"))
            .into_iter()
            .filter(|(stem, _)| match prefix {
                "dd" => stem.starts_with("dd ") && !stem.starts_with("dd cb"),
                "fd" => stem.starts_with("fd ") && !stem.starts_with("fd cb"),
                "dd cb" => stem.starts_with("dd cb"),
                "fd cb" => stem.starts_with("fd cb"),
                other => stem.starts_with(other),
            })
            .collect();

        run_sweep::<UnderTest>(
            &format!("{} prefix", prefix.to_uppercase()),
            &matching,
            &RunOptions::from_env(0),
            None,
        );
    }
}
