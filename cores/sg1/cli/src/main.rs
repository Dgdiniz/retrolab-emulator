//! Command-line entry point for the SG1 Genesis/Mega Drive emulator.
//!
//! `main` is intentionally thin: it defines the `Commands` enum and
//! dispatches to the handlers under `cmd/`. The debugging workhorses are
//! the `trace` + `compare-trace` pair (dump an execution trace in MAME
//! text format and diff it live against a reference log) and the
//! `screenshot --render-layers` / `dump-vdp` graphics inspectors. Shared
//! ROM-loading, output, controller-input, and trace parse/format helpers
//! live in `cmd/mod.rs`, `trace_ref.rs`, and `trace_driver.rs`.

mod cmd;
mod trace_driver;
mod trace_ref;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "sg1-cli",
    about = "SG1 Genesis/Mega Drive emulator command-line interface",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Print the CLI version.
    Version,
    /// Inspect a ROM's header (title, region, checksum, mapper).
    Info(cmd::info::InfoArgs),
    /// Boot a ROM and run it for a bounded window.
    Run(cmd::run::RunArgs),
    /// Dump an execution trace in MAME text format.
    Trace(cmd::trace::TraceArgs),
    /// Diff the core's live trace against a MAME reference log.
    CompareTrace(cmd::compare_trace::CompareTraceArgs),
    /// Diff two text trace files offline (optimized for large files).
    Compare(cmd::compare::CompareArgs),
    /// Convert a binary trace blob to MAME text format.
    TraceToText(cmd::trace_to_text::TraceToTextArgs),
    /// Run a ROM and save the framebuffer as a PNG (+ layer dumps).
    Screenshot(cmd::screenshot::ScreenshotArgs),
    /// Forward-disassemble a CPU's instruction stream.
    Disasm(cmd::disasm::DisasmArgs),
    /// Run a ROM and dump a region of CPU-visible memory (hex + ASCII).
    Memory(cmd::memory::MemoryArgs),
    /// Run a ROM and dump a full VDP state report.
    DumpVdp(cmd::dump_vdp::DumpVdpArgs),
    /// Log VDP writes that land in a VRAM address window while running.
    WatchVram(cmd::watch_vram::WatchVramArgs),
    /// Single-step a window and name the instruction that writes a RAM cell.
    WatchRam(cmd::watch_ram::WatchRamArgs),
    /// Single-step a window and name the instructions that read an address range.
    WatchRead(cmd::watch_read::WatchReadArgs),
    /// Emit a per-frame CRC32 of a work-RAM region for divergence bisection.
    Fingerprint(cmd::fingerprint::FingerprintArgs),
    /// Verify deterministic replay + savestate completeness for netplay.
    DeterminismCheck(cmd::determinism_check::DeterminismCheckArgs),
    /// Benchmark the execution tiers (fast / light-debug / +trace / full).
    Benchmark(cmd::benchmark::BenchmarkArgs),
    /// Profile per-subsystem frame timing and rank bottlenecks.
    Profile(cmd::profile::ProfileArgs),
    /// Run a ROM with audio capture and save the output as a WAV.
    AudioDump(cmd::audio_dump::AudioDumpArgs),
    /// Print a PSG + YM2612 channel-state summary.
    AudioInfo(cmd::audio_info::AudioInfoArgs),
    /// Dump YM2612 + PSG register writes with their in-frame sample offset.
    SoundTrace(cmd::sound_trace::SoundTraceArgs),
    /// Save or load an emulator savestate.
    Savestate(cmd::savestate::SavestateArgs),
    /// Dump or seed a game's battery-backed SRAM.
    Sram(cmd::sram::SramArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Version => {
            println!("sg1-cli {}", env!("CARGO_PKG_VERSION"));
        }
        Commands::Info(args) => cmd::info::run(args)?,
        Commands::Run(args) => cmd::run::run(args)?,
        Commands::Trace(args) => cmd::trace::run(args)?,
        Commands::CompareTrace(args) => cmd::compare_trace::run(args)?,
        Commands::Compare(args) => cmd::compare::run(args)?,
        Commands::TraceToText(args) => cmd::trace_to_text::run(args)?,
        Commands::Screenshot(args) => cmd::screenshot::run(args)?,
        Commands::Disasm(args) => cmd::disasm::run(args)?,
        Commands::Memory(args) => cmd::memory::run(args)?,
        Commands::DumpVdp(args) => cmd::dump_vdp::run(args)?,
        Commands::WatchVram(args) => cmd::watch_vram::run(args)?,
        Commands::WatchRam(args) => cmd::watch_ram::run(args)?,
        Commands::WatchRead(args) => cmd::watch_read::run(args)?,
        Commands::Fingerprint(args) => cmd::fingerprint::run(args)?,
        Commands::DeterminismCheck(args) => cmd::determinism_check::run(args)?,
        Commands::Benchmark(args) => cmd::benchmark::run(args)?,
        Commands::Profile(args) => cmd::profile::run(args)?,
        Commands::AudioDump(args) => cmd::audio_dump::run(args)?,
        Commands::AudioInfo(args) => cmd::audio_info::run(args)?,
        Commands::SoundTrace(args) => cmd::sound_trace::run(args)?,
        Commands::Savestate(args) => cmd::savestate::run(args)?,
        Commands::Sram(args) => cmd::sram::run(args)?,
    }
    Ok(())
}
