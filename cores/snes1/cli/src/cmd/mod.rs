//! The SNES's own commands: each module owns its clap arguments and a
//! `run` entry point, and [`Command`] lists them for `retro-cli`, which runs
//! the commands both tools share.

pub mod apu_dump;
pub mod ppu_dump;
pub mod singlestep;

use anyhow::Result;
use clap::Subcommand;

pub(crate) use retro_cli::output::open_output;

use crate::console::SnesBoot;

/// The SNES's own commands. `ppu-dump`'s arguments are boxed: they are
/// much larger than the others'.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run a ROM and summarise PPU state (+ optional dumps / PNGs).
    PpuDump(Box<ppu_dump::PpuDumpArgs<SnesBoot>>),
    /// Run a ROM and summarise APU (S-DSP) state (+ optional dumps).
    ApuDump(apu_dump::ApuDumpArgs<SnesBoot>),
    /// Run the 65C816 SingleStepTests (Tom Harte) against the core.
    #[command(name = "singlestep-65816")]
    Singlestep65816(singlestep::Singlestep65816Args),
    /// Run the SPC700 SingleStepTests against the core.
    #[command(name = "singlestep-spc700")]
    SinglestepSpc700(singlestep::SinglestepSpc700Args),
}

/// Run one of the SNES's own commands.
pub fn run(command: Command) -> Result<()> {
    match command {
        Command::PpuDump(args) => ppu_dump::run(*args),
        Command::ApuDump(args) => apu_dump::run(args),
        Command::Singlestep65816(args) => singlestep::run_65816(args),
        Command::SinglestepSpc700(args) => singlestep::run_spc700(args),
    }
}
