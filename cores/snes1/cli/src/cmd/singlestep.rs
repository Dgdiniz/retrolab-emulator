//! `singlestep-65816` / `singlestep-spc700`: run the SingleStepTests
//! against the CPU cores.
//!
//! The cases' side of each CPU (the JSON DTOs, the state a case sets, the
//! comparison) is `wdc65816::singlestep` / `spc700::singlestep`, gated behind
//! those crates' `singlestep` feature, and retro-core's runner runs them
//! through `retro_cli::singlestep::run_suite`. The CLI and
//! `cargo test -p {wdc65816,spc700}` therefore validate the cores against
//! byte-identical logic; this file holds the commands' flags and the files
//! they name.
//!
//! The dataset is **not** bundled in the repo, download the per-opcode JSON
//! files from <https://github.com/SingleStepTests/65816> and
//! <https://github.com/SingleStepTests/spc700> into
//! `tests/singlesteptests/{65816,spc700}/v1/`, or point
//! `--base` elsewhere. When the dataset is absent the command prints a hint
//! and exits without error so it stays CI-friendly.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, ValueEnum};
use retro_cli::parse::parse_hex_u8;
use retro_cli::singlestep::{run_suite, Runs, Suite};

/// Default dataset roots, relative to the repository's root (the cargo
/// workspace, where the command runs from); expect `<base>/v1/*.json`.
const DEFAULT_BASE_65816: &str = "tests/singlesteptests/65816";
const DEFAULT_BASE_SPC700: &str = "tests/singlesteptests/spc700";

/// 65C816 CPU mode for a test file (`<op>.e.json` vs `<op>.n.json`).
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ModeArg {
    /// Emulation mode (`.e.json`); M and X forced to 1.
    E,
    /// Native mode (`.n.json`); M/X come from the case's initial P.
    N,
}

#[derive(Args, Debug)]
pub struct Singlestep65816Args {
    /// Single opcode to run (hex, e.g. `a9` or `0xA9`). Omit to sweep all.
    #[arg(long, value_parser = parse_hex_u8)]
    pub opcode: Option<u8>,
    /// Mode for `--opcode`: `e` or `n`. Omit to run both.
    #[arg(long, value_enum)]
    pub mode: Option<ModeArg>,
    /// Sweep every opcode x both modes (same as omitting `--opcode`).
    #[arg(long)]
    pub all: bool,
    /// Cases per opcode-mode file; `0` runs all 10 000.
    #[arg(long, default_value_t = 1000)]
    pub cases: usize,
    /// SingleStepTests dataset root (expects `<base>/v1/<op>.<e|n>.json`).
    #[arg(long, default_value = DEFAULT_BASE_65816)]
    pub base: PathBuf,
    /// Dump the first failure's initial/final/diff (the same as setting
    /// `SINGLESTEP_DIAGNOSE`).
    #[arg(long)]
    pub diagnose: bool,
}

#[derive(Args, Debug)]
pub struct SinglestepSpc700Args {
    /// Single opcode to run (hex, e.g. `e8`). Omit to sweep all.
    #[arg(long, value_parser = parse_hex_u8)]
    pub opcode: Option<u8>,
    /// Sweep every opcode (same as omitting `--opcode`).
    #[arg(long)]
    pub all: bool,
    /// Cases per opcode file; `0` runs all 1 000.
    #[arg(long, default_value_t = 1000)]
    pub cases: usize,
    /// SingleStepTests dataset root (expects `<base>/v1/<op>.json`).
    #[arg(long, default_value = DEFAULT_BASE_SPC700)]
    pub base: PathBuf,
    /// Dump the first failure's initial/final/diff (the same as setting
    /// `SINGLESTEP_DIAGNOSE`).
    #[arg(long)]
    pub diagnose: bool,
}

const SUITE_65816: Suite = Suite {
    command: "singlestep-65816",
    name: "65816",
    url: "https://github.com/SingleStepTests/65816",
};

const SUITE_SPC700: Suite = Suite {
    command: "singlestep-spc700",
    name: "SPC700",
    url: "https://github.com/SingleStepTests/spc700",
};

pub fn run_65816(args: Singlestep65816Args) -> Result<()> {
    use wdc65816::singlestep::{file_name, label, TestMode, UnderTest};

    // The opcode asked for, in the modes asked for; otherwise a sweep of
    // every opcode x both modes.
    let runs = match args.opcode {
        Some(opcode) if !args.all => {
            let modes: Vec<TestMode> = match args.mode {
                Some(ModeArg::E) => vec![TestMode::Emulation],
                Some(ModeArg::N) => vec![TestMode::Native],
                None => vec![TestMode::Emulation, TestMode::Native],
            };
            let dir = args.base.join("v1");
            let files = modes
                .into_iter()
                .map(|mode| (label(opcode, mode), dir.join(file_name(opcode, mode))))
                .collect();
            Runs::Opcode { opcode, files }
        }
        _ => Runs::Sweep,
    };
    run_suite::<UnderTest>(&SUITE_65816, &args.base, runs, args.cases, args.diagnose)
}

pub fn run_spc700(args: SinglestepSpc700Args) -> Result<()> {
    use spc700::singlestep::{file_name, label, UnderTest};

    let runs = match args.opcode {
        Some(opcode) if !args.all => Runs::Opcode {
            opcode,
            files: vec![(label(opcode), args.base.join("v1").join(file_name(opcode)))],
        },
        _ => Runs::Sweep,
    };
    run_suite::<UnderTest>(&SUITE_SPC700, &args.base, runs, args.cases, args.diagnose)
}
