//! The SNES's own commands: each module owns its clap arguments and a
//! `run` entry point, and [`Command`] lists them for `retro-cli`, which runs
//! the commands both tools share. The helpers the commands share live here.

pub mod apu_dump;
pub mod audio_dump;
pub mod benchmark;
pub mod determinism_check;
pub mod disasm;
pub mod golden;
pub mod info;
pub mod memory;
pub mod ppu_dump;
pub mod run;
pub mod savestate;
pub mod screenshot;
pub mod singlestep;

use std::collections::BTreeSet;

use anyhow::Result;
use clap::Subcommand;
use retro_core::cartridge::ExpansionChipState;
use snes1_core::SnesEmulator;

pub(crate) use crate::console::{apply_force_detect, CoprocessorArgs};
pub(crate) use retro_cli::output::open_output;
pub(crate) use retro_cli::rom::read_rom;
pub(crate) use retro_core::input_schedule::PressSchedule;

/// The SNES's own commands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Inspect a ROM's header, mapper, and CPU memory map.
    Info(info::InfoArgs),
    /// Boot a ROM and run it for a bounded window.
    Run(run::RunArgs),
    /// Verify netplay determinism over a long replay (lockstep / rollback).
    DeterminismCheck(determinism_check::DeterminismCheckArgs),
    /// Run a ROM and save the visible PPU output as a PNG.
    Screenshot(screenshot::ScreenshotArgs),
    /// Forward-disassemble a CPU's instruction stream.
    Disasm(disasm::DisasmArgs),
    /// Run a ROM and summarise PPU state (+ optional dumps / PNGs).
    PpuDump(ppu_dump::PpuDumpArgs),
    /// Run a ROM and summarise APU (S-DSP) state (+ optional dumps).
    ApuDump(apu_dump::ApuDumpArgs),
    /// Run a ROM with audio capture and save the S-DSP output as a WAV.
    AudioDump(audio_dump::AudioDumpArgs),
    /// Run the 65C816 SingleStepTests (Tom Harte) against the core.
    #[command(name = "singlestep-65816")]
    Singlestep65816(singlestep::Singlestep65816Args),
    /// Run the SPC700 SingleStepTests against the core.
    #[command(name = "singlestep-spc700")]
    SinglestepSpc700(singlestep::SinglestepSpc700Args),
    /// Benchmark the three execution tiers (Fast / LightDebug / +trace).
    Benchmark(benchmark::BenchmarkArgs),
    /// Run a ROM and dump a region of CPU-visible memory (hex + ASCII).
    Memory(memory::MemoryArgs),
    /// Save or load an emulator savestate.
    Savestate(savestate::SavestateArgs),
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
        Command::Info(args) => info::run(args),
        Command::Run(args) => run::run(args),
        Command::DeterminismCheck(args) => determinism_check::run(args),
        Command::Screenshot(args) => screenshot::run(args),
        Command::Disasm(args) => disasm::run(args),
        Command::PpuDump(args) => ppu_dump::run(args),
        Command::ApuDump(args) => apu_dump::run(args),
        Command::AudioDump(args) => audio_dump::run(args),
        Command::Singlestep65816(args) => singlestep::run_65816(args),
        Command::SinglestepSpc700(args) => singlestep::run_spc700(args),
        Command::Benchmark(args) => benchmark::run(args),
        Command::Memory(args) => memory::run(args),
        Command::Savestate(args) => savestate::run(args),
        Command::Golden(args) => golden::run(args),
        Command::GoldenSet(args) => golden::run_set(args),
        Command::GoldenCompare(args) => golden::run_compare(args),
    }
}

/// Print the installed coprocessor(s): a label summary followed by each
/// chip's live state. A plain cartridge prints `Coprocessor: none`; a
/// multi-chip cartridge enumerates every chip by `chip_id`. Shared by
/// `info` and `screenshot --coprocessor-state`.
pub(crate) fn print_coprocessors(emu: &SnesEmulator) {
    let labels = emu.coprocessor_labels();
    match labels.as_slice() {
        [] => {
            println!("Coprocessor: none");
            return;
        }
        [(_, label)] => println!("Coprocessor: {label}"),
        many => {
            println!("Coprocessors: {}", many.len());
            for (id, label) in many {
                println!("  [{id}] {label}");
            }
        }
    }
    for (id, state) in emu.coprocessors_debug_state() {
        print_coprocessor_state(id, &state);
    }
}

/// Print one coprocessor chip's live state (DSP-1 uPD7725 register file +
/// data RAM, the S-DD1 MMC latches + reconstructed DMA shadow, or the
/// SPC7110's decoder / ALU / data-port / mapping + RTC registers). Values
/// reflect however many frames the caller ran before dumping: `info`
/// defaults to a reset snapshot (`--frames 0`), while `info --frames N`
/// and `screenshot --coprocessor-state` show live state after N frames
/// (e.g. S-DD1 `DECOMPS > 0` once a game has driven a decompression, or the
/// SPC7110 `DEC_MODE`/`DX_OFF` after a decompression + bank switch).
/// Also consumed by the frontend debugger.
pub(crate) fn print_coprocessor_state(chip_id: u8, state: &ExpansionChipState) {
    println!();
    println!("Coprocessor chip state [{chip_id}]:");
    println!("  Type:           {:?}", state.chip_type);
    match state.program_counter {
        Some(pc) => println!("  PC:             ${pc:04x}"),
        None => println!("  PC:             (idle - no firmware loaded)"),
    }
    println!("  Cycle count:    {}", state.cycle_count);
    match &state.internal_ram {
        Some(ram) => println!(
            "  Internal RAM:   {} bytes ({} x 16-bit words)",
            ram.len(),
            ram.len() / 2
        ),
        None => println!("  Internal RAM:   (none)"),
    }
    if state.registers.is_empty() {
        println!("  Registers:      (none - chip present but not driven)");
    } else {
        println!("  Registers:");
        for r in &state.registers {
            println!("    {:<8} ${:04x}  {}", r.name, r.value, r.description);
        }
    }
}

/// Parse a comma-separated button list into the generic input bitmask
/// (`InputState::buttons`), in the pad's names; `None` when no spec was
/// given.
pub(crate) fn parse_button_mask(spec: Option<&str>) -> Result<Option<u32>> {
    retro_cli::input::parse_buttons::<SnesEmulator>(spec)
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
