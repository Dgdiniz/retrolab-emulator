//! Command-line entry point for the SNES1 emulator.
//!
//! `main` is intentionally thin: it defines the `Commands` enum and
//! dispatches to the handlers under `cmd/`. The debugging workhorse is
//! the `trace` + `compare-trace` pair, which dumps an execution trace
//! in bsnes-plus format and diffs it against a reference log so we can
//! confirm the core's instructions match a known-good emulator.

mod cmd;
mod trace_driver;
mod trace_ref;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "snes1-cli",
    about = "SNES1 emulator command-line interface",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Print the CLI version.
    Version,
    /// Inspect a ROM's header, mapper, and CPU memory map.
    Info(cmd::info::InfoArgs),
    /// Boot a ROM and run it for a bounded window.
    Run(cmd::run::RunArgs),
    /// Dump an execution trace in bsnes-plus text format.
    Trace(cmd::trace::TraceArgs),
    /// Diff the core's live trace against a bsnes-plus reference log.
    CompareTrace(cmd::compare::CompareArgs),
    /// Verify netplay determinism over a long replay (lockstep / rollback).
    DeterminismCheck(cmd::determinism_check::DeterminismCheckArgs),
    /// Run a ROM and save the visible PPU output as a PNG.
    Screenshot(cmd::screenshot::ScreenshotArgs),
    /// Forward-disassemble a CPU's instruction stream.
    Disasm(cmd::disasm::DisasmArgs),
    /// Run a ROM and summarise PPU state (+ optional dumps / PNGs).
    PpuDump(cmd::ppu_dump::PpuDumpArgs),
    /// Run a ROM and summarise APU (S-DSP) state (+ optional dumps).
    ApuDump(cmd::apu_dump::ApuDumpArgs),
    /// Run a ROM with audio capture and save the S-DSP output as a WAV.
    AudioDump(cmd::audio_dump::AudioDumpArgs),
    /// Run the 65C816 SingleStepTests (Tom Harte) against the core.
    #[command(name = "singlestep-65816")]
    Singlestep65816(cmd::singlestep::Singlestep65816Args),
    /// Run the SPC700 SingleStepTests against the core.
    #[command(name = "singlestep-spc700")]
    SinglestepSpc700(cmd::singlestep::SinglestepSpc700Args),
    /// Benchmark the three execution tiers (Fast / LightDebug / +trace).
    Benchmark(cmd::benchmark::BenchmarkArgs),
    /// Run a ROM and dump a region of CPU-visible memory (hex + ASCII).
    Memory(cmd::memory::MemoryArgs),
    /// Save or load an emulator savestate.
    Savestate(cmd::savestate::SavestateArgs),
    /// Record a ROM's output frame by frame (state, picture and audio
    /// hashes, PNGs at chosen frames) into a folder.
    Golden(cmd::golden::GoldenArgs),
    /// Record every ROM of a manifest into a folder.
    GoldenSet(cmd::golden::GoldenSetArgs),
    /// Compare two golden folders: first differing frame and pixel diffs.
    GoldenCompare(cmd::golden::GoldenCompareArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Version => {
            println!("snes1-cli {}", env!("CARGO_PKG_VERSION"));
        }
        Command::Info(args) => cmd::info::run(args)?,
        Command::Run(args) => cmd::run::run(args)?,
        Command::Trace(args) => cmd::trace::run(args)?,
        Command::CompareTrace(args) => cmd::compare::run(args)?,
        Command::DeterminismCheck(args) => cmd::determinism_check::run(args)?,
        Command::Screenshot(args) => cmd::screenshot::run(args)?,
        Command::Disasm(args) => cmd::disasm::run(args)?,
        Command::PpuDump(args) => cmd::ppu_dump::run(args)?,
        Command::ApuDump(args) => cmd::apu_dump::run(args)?,
        Command::AudioDump(args) => cmd::audio_dump::run(args)?,
        Command::Singlestep65816(args) => cmd::singlestep::run_65816(args)?,
        Command::SinglestepSpc700(args) => cmd::singlestep::run_spc700(args)?,
        Command::Benchmark(args) => cmd::benchmark::run(args)?,
        Command::Memory(args) => cmd::memory::run(args)?,
        Command::Savestate(args) => cmd::savestate::run(args)?,
        Command::Golden(args) => cmd::golden::run(args)?,
        Command::GoldenSet(args) => cmd::golden::run_set(args)?,
        Command::GoldenCompare(args) => cmd::golden::run_compare(args)?,
    }
    Ok(())
}
