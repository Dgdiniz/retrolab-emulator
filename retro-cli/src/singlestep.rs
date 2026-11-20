//! The front-end of a tool's command that runs a CPU's test vectors (the
//! SNES tool's `singlestep-65816` and `singlestep-spc700`), over
//! retro-core's runner (`retro_core::testing::singlestep`): where the
//! dataset is, the runs and their report, and the command's exit.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use retro_core::testing::singlestep::{
    print_file_result, run_file, run_sweep, vector_files, CpuUnderTest, Diagnose, RunOptions,
};

/// A set of test vectors a command runs.
pub struct Suite {
    /// The command's name (`singlestep-65816`), which its failure names.
    pub command: &'static str,
    /// The set's name in the messages (`65816`).
    pub name: &'static str,
    /// Where the set can be downloaded from.
    pub url: &'static str,
}

/// What a command runs of its dataset.
pub enum Runs {
    /// Every vector file, as one sweep.
    Sweep,
    /// The files of one opcode (the 65C816 has one a mode), each with its
    /// label, each reported on its own.
    Opcode {
        opcode: u8,
        files: Vec<(String, PathBuf)>,
    },
}

/// Run `runs` of `suite`'s dataset under `base` (its files are in
/// `base/v1/`), `cases` of each file (`0`: every case), diagnosing each
/// file's first failure when `diagnose` is set. A dataset that is not
/// there prints where to get it and is not an error; a failing case is.
pub fn run_suite<T: CpuUnderTest>(
    suite: &Suite,
    base: &Path,
    runs: Runs,
    cases: usize,
    diagnose: bool,
) -> Result<()> {
    let mut options = RunOptions::from_env(cases);
    if diagnose && options.diagnose == Diagnose::No {
        options.diagnose = Diagnose::First;
    }

    let dir = base.join("v1");
    if !dir.is_dir() {
        eprintln!(
            "SingleStepTests {} dataset not found at {} (expected <base>/v1/*.json).",
            suite.name,
            base.display()
        );
        eprintln!(
            "  Download the per-opcode JSON files from {} into that directory,",
            suite.url
        );
        eprintln!("  or pass --base <DIR>. Skipping (exit 0 so CI stays green).");
        return Ok(());
    }

    match runs {
        Runs::Sweep => {
            let files = vector_files(&dir);
            if files.is_empty() {
                eprintln!("no test files under {}", dir.display());
                return Ok(());
            }
            let totals = run_sweep::<T>("cli", &files, &options, None).totals();
            if totals.failures() > 0 {
                bail!(
                    "{} failed, {} panicked of {}",
                    totals.failed,
                    totals.panicked,
                    totals.total()
                );
            }
            Ok(())
        }
        Runs::Opcode { opcode, files } => {
            let mut ran_any = false;
            let mut had_failure = false;
            for (label, path) in &files {
                if !path.is_file() {
                    println!("(no dataset file for {label})");
                    continue;
                }
                ran_any = true;
                let result = run_file::<T>(path, label, &options);
                print_file_result(&result);
                had_failure |= result.counts.failures() > 0;
            }
            if !ran_any {
                bail!(
                    "no dataset file for opcode {:02X} under {}",
                    opcode,
                    dir.display()
                );
            }
            if had_failure {
                bail!("{} had failures", suite.command);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::testing::singlestep::CycleCheck;

    /// A register that adds a case's operand: a case is `[a, operand,
    /// final]`.
    struct Adder(u8);

    impl CpuUnderTest for Adder {
        type Case = Vec<u8>;
        const CYCLES: CycleCheck = CycleCheck::OnRequest;
        const FILE_KIND: &'static str = "opcodes";

        fn case_name(_: &Vec<u8>) -> &str {
            "case"
        }
        fn prepare(case: &Vec<u8>) -> Self {
            Self(case[0])
        }
        fn execute(&mut self, case: &Vec<u8>) -> u32 {
            self.0 = self.0.wrapping_add(case[1]);
            1
        }
        fn differences(&mut self, case: &Vec<u8>) -> Vec<String> {
            if self.0 == case[2] {
                Vec::new()
            } else {
                vec![format!("A: expected {}, got {}", case[2], self.0)]
            }
        }
        fn expected_cycles(_: &Vec<u8>) -> u32 {
            1
        }
        fn describe(&mut self, _: &Vec<u8>) -> String {
            String::new()
        }
    }

    const SUITE: Suite = Suite {
        command: "singlestep-adder",
        name: "adder",
        url: "https://example.com/adder",
    };

    #[test]
    fn a_dataset_runs_and_its_failures_fail_the_command() {
        let base =
            std::env::temp_dir().join(format!("retro-cli-singlestep-{}", std::process::id()));
        let dir = base.join("v1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("01.json"), "[[1, 1, 2], [1, 1, 3]]").unwrap();
        std::fs::write(dir.join("02.json"), "[[1, 2, 3]]").unwrap();
        let opcode = |opcode: u8| Runs::Opcode {
            opcode,
            files: vec![(
                format!("{opcode:02X}"),
                dir.join(format!("{opcode:02x}.json")),
            )],
        };

        assert!(run_suite::<Adder>(&SUITE, &base, opcode(2), 0, false).is_ok());
        let failure = run_suite::<Adder>(&SUITE, &base, opcode(1), 0, false).unwrap_err();
        assert_eq!(failure.to_string(), "singlestep-adder had failures");
        let missing = run_suite::<Adder>(&SUITE, &base, opcode(3), 0, false).unwrap_err();
        assert_eq!(
            missing.to_string(),
            format!("no dataset file for opcode 03 under {}", dir.display())
        );
        let sweep = run_suite::<Adder>(&SUITE, &base, Runs::Sweep, 0, false).unwrap_err();
        assert_eq!(sweep.to_string(), "1 failed, 0 panicked of 3");
        assert!(run_suite::<Adder>(&SUITE, &base, Runs::Sweep, 1, false).is_ok());

        std::fs::remove_dir_all(&base).unwrap();
        assert!(run_suite::<Adder>(&SUITE, &base, Runs::Sweep, 0, false).is_ok());
    }
}
