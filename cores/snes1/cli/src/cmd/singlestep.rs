//! `singlestep-65816` / `singlestep-spc700`: run the Tom Harte
//! SingleStepTests against the CPU cores.
//!
//! The actual harness (FlatMemoryBus + apply/step/check + the JSON DTOs)
//! lives in `wdc65816::singlestep` / `spc700::singlestep`, gated behind those
//! crates' `singlestep` feature. The CLI and `cargo test -p {wdc65816,spc700}`
//! therefore validate the cores against byte-identical logic.
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
    /// Dump the first failure's initial/final/diff (sets `SINGLESTEP_DIAGNOSE`).
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
    /// Cases per opcode file; `0` runs all 10 000.
    #[arg(long, default_value_t = 1000)]
    pub cases: usize,
    /// SingleStepTests dataset root (expects `<base>/v1/<op>.json`).
    #[arg(long, default_value = DEFAULT_BASE_SPC700)]
    pub base: PathBuf,
    /// Dump the first failure's initial/final/diff (sets `SINGLESTEP_DIAGNOSE`).
    #[arg(long)]
    pub diagnose: bool,
}

/// Parse `a9` / `0xA9` / `$A9` into a byte.
fn parse_hex_u8(s: &str) -> Result<u8, String> {
    let cleaned = s.trim().trim_start_matches("0x").trim_start_matches('$');
    u8::from_str_radix(cleaned, 16).map_err(|e| format!("invalid hex opcode '{s}': {e}"))
}

fn missing_dataset_hint(base: &std::path::Path, suite: &str, url: &str) {
    eprintln!(
        "SingleStepTests {suite} dataset not found at {} (expected <base>/v1/*.json).",
        base.display()
    );
    eprintln!("  Download the per-opcode JSON files from {url} into that directory,");
    eprintln!("  or pass --base <DIR>. Skipping (exit 0 so CI stays green).");
}

pub fn run_65816(args: Singlestep65816Args) -> Result<()> {
    use wdc65816::singlestep::{self, TestMode};

    if args.diagnose {
        // The shared runner reads this env var to dump the first failure.
        std::env::set_var("SINGLESTEP_DIAGNOSE", "1");
    }

    if !args.base.join("v1").is_dir() {
        missing_dataset_hint(
            &args.base,
            "65816",
            "https://github.com/SingleStepTests/65816",
        );
        return Ok(());
    }

    // Sweep mode: every opcode x both modes.
    if args.all || args.opcode.is_none() {
        return match singlestep::run_sweep(&args.base, args.cases, "cli") {
            Some(s) if s.failed + s.panicked > 0 => {
                anyhow::bail!(
                    "{} failed, {} panicked of {}",
                    s.failed,
                    s.panicked,
                    s.total
                )
            }
            Some(_) => Ok(()),
            None => {
                eprintln!("no test files under {}", args.base.join("v1").display());
                Ok(())
            }
        };
    }

    let opcode = args.opcode.expect("checked above");
    let modes: Vec<TestMode> = match args.mode {
        Some(ModeArg::E) => vec![TestMode::Emulation],
        Some(ModeArg::N) => vec![TestMode::Native],
        None => vec![TestMode::Emulation, TestMode::Native],
    };

    let mut had_failure = false;
    let mut ran_any = false;
    for mode in modes {
        match singlestep::run_opcode(&args.base, opcode, mode, args.cases) {
            Some(res) => {
                ran_any = true;
                singlestep::print_file_result(&res);
                had_failure |= res.failed + res.panicked > 0;
            }
            None => println!("(no dataset file for {:02X}.{})", opcode, mode.suffix()),
        }
    }
    if !ran_any {
        anyhow::bail!(
            "no dataset file for opcode {:02X} under {}",
            opcode,
            args.base.join("v1").display()
        );
    }
    if had_failure {
        anyhow::bail!("singlestep-65816 had failures");
    }
    Ok(())
}

pub fn run_spc700(args: SinglestepSpc700Args) -> Result<()> {
    use spc700::singlestep;

    if args.diagnose {
        std::env::set_var("SINGLESTEP_DIAGNOSE", "1");
    }

    if !args.base.join("v1").is_dir() {
        missing_dataset_hint(
            &args.base,
            "SPC700",
            "https://github.com/SingleStepTests/spc700",
        );
        return Ok(());
    }

    if args.all || args.opcode.is_none() {
        return match singlestep::run_sweep(&args.base, args.cases, "cli") {
            Some(s) if s.failed + s.panicked > 0 => {
                anyhow::bail!(
                    "{} failed, {} panicked of {}",
                    s.failed,
                    s.panicked,
                    s.total
                )
            }
            Some(_) => Ok(()),
            None => {
                eprintln!("no test files under {}", args.base.join("v1").display());
                Ok(())
            }
        };
    }

    let opcode = args.opcode.expect("checked above");
    match singlestep::run_opcode(&args.base, opcode, args.cases) {
        Some(res) => {
            singlestep::print_file_result(&res);
            if res.failed + res.panicked > 0 {
                anyhow::bail!("singlestep-spc700 had failures");
            }
            Ok(())
        }
        None => anyhow::bail!(
            "no dataset file for opcode {:02X} under {}",
            opcode,
            args.base.join("v1").display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_hex() {
        assert_eq!(parse_hex_u8("a9").unwrap(), 0xA9);
        assert_eq!(parse_hex_u8("00").unwrap(), 0x00);
        assert_eq!(parse_hex_u8("ff").unwrap(), 0xFF);
    }

    #[test]
    fn parses_prefixed_hex() {
        assert_eq!(parse_hex_u8("0xE8").unwrap(), 0xE8);
        assert_eq!(parse_hex_u8("$1b").unwrap(), 0x1B);
        assert_eq!(parse_hex_u8("  3C ").unwrap(), 0x3C);
    }

    #[test]
    fn rejects_garbage_and_overflow() {
        assert!(parse_hex_u8("zz").is_err());
        assert!(parse_hex_u8("100").is_err()); // > u8::MAX
        assert!(parse_hex_u8("").is_err());
    }
}
