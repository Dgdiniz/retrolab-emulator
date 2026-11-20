//! The SNES's own commands: each module owns its clap arguments and a
//! `run` entry point, and [`Command`] lists them for `retro-cli`, which runs
//! the commands both tools share. The helpers the commands share live here.

pub mod apu_dump;
pub mod golden;
pub mod ppu_dump;
pub mod singlestep;

use std::collections::BTreeSet;

use anyhow::Result;
use clap::Subcommand;
use retro_core::input_schedule::PressSchedule;
use snes1_core::SnesEmulator;

pub(crate) use crate::console::{apply_force_detect, CoprocessorArgs};
pub(crate) use retro_cli::output::open_output;
pub(crate) use retro_cli::rom::read_rom;

/// The SNES's own commands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run a ROM and summarise PPU state (+ optional dumps / PNGs).
    PpuDump(ppu_dump::PpuDumpArgs),
    /// Run a ROM and summarise APU (S-DSP) state (+ optional dumps).
    ApuDump(apu_dump::ApuDumpArgs),
    /// Run the 65C816 SingleStepTests (Tom Harte) against the core.
    #[command(name = "singlestep-65816")]
    Singlestep65816(singlestep::Singlestep65816Args),
    /// Run the SPC700 SingleStepTests against the core.
    #[command(name = "singlestep-spc700")]
    SinglestepSpc700(singlestep::SinglestepSpc700Args),
    /// Record a ROM's output frame by frame (state, picture and audio
    /// hashes, PNGs at chosen frames) into a folder.
    Golden(golden::GoldenArgs),
    /// Record every ROM of a manifest into a folder.
    GoldenSet(golden::GoldenSetArgs),
    /// Compare two golden folders: first differing frame and pixel diffs.
    GoldenCompare(golden::GoldenCompareArgs),
}

/// Run one of the SNES's own commands.
pub fn run(command: Command) -> Result<()> {
    match command {
        Command::PpuDump(args) => ppu_dump::run(args),
        Command::ApuDump(args) => apu_dump::run(args),
        Command::Singlestep65816(args) => singlestep::run_65816(args),
        Command::SinglestepSpc700(args) => singlestep::run_spc700(args),
        Command::Golden(args) => golden::run(args),
        Command::GoldenSet(args) => golden::run_set(args),
        Command::GoldenCompare(args) => golden::run_compare(args),
    }
}

/// Parse a `--press-at "F:buttons;F:buttons"` spec (e.g.
/// `"180:start;186:;240:select"`) into a [`PressSchedule`]; `None` when no
/// spec was given.
pub(crate) fn parse_press_schedule(spec: Option<&str>) -> Result<Option<PressSchedule>> {
    retro_cli::input::parse_press_schedule::<SnesEmulator>(spec)
}

/// Parse the `--reset-at` frame list (`"360,720"`) into a sorted set of
/// frame indices before which the console is reset.
pub(crate) fn parse_reset_frames(spec: Option<&str>) -> Result<BTreeSet<u64>> {
    retro_cli::input::parse_reset_frames(spec)
}
