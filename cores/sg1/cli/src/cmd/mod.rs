//! The Mega Drive's own commands: each module owns its clap arguments and
//! a `run` entry point, and [`Command`] lists them for `retro-cli`, which
//! runs the commands both tools share. The helpers the commands share live
//! here.

pub mod audio_info;
pub mod dump_vdp;
pub mod sound_trace;
pub mod watch_read;
pub mod watch_vram;

use std::path::Path;

use anyhow::Result;
use clap::Subcommand;
use retro_core::EmulatorCore;
use sg1_core::GenesisEmulator;

pub(crate) use retro_cli::output::open_output;

use crate::console::GenesisBoot;

/// The Mega Drive's own commands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run a ROM and dump a full VDP state report.
    DumpVdp(dump_vdp::DumpVdpArgs<GenesisBoot>),
    /// Log VDP writes that land in a VRAM address window while running.
    WatchVram(watch_vram::WatchVramArgs<GenesisBoot>),
    /// Single-step a window and name the instructions that read an address range.
    WatchRead(watch_read::WatchReadArgs<GenesisBoot>),
    /// Print a PSG + YM2612 channel-state summary.
    AudioInfo(audio_info::AudioInfoArgs<GenesisBoot>),
    /// Dump YM2612 + PSG register writes with their in-frame sample offset.
    SoundTrace(sound_trace::SoundTraceArgs<GenesisBoot>),
}

/// Run one of the Mega Drive's own commands.
pub fn run(command: Command) -> Result<()> {
    match command {
        Command::DumpVdp(args) => dump_vdp::run(args),
        Command::WatchVram(args) => watch_vram::run(args),
        Command::WatchRead(args) => watch_read::run(args),
        Command::AudioInfo(args) => audio_info::run(args),
        Command::SoundTrace(args) => sound_trace::run(args),
    }
}

/// Print the parsed ROM title (best-effort) to stdout.
pub(crate) fn print_rom_title(emu: &GenesisEmulator) {
    if let Some(info) = emu.get_rom_info() {
        println!("Title: {}", info.title.trim());
    }
}

/// Save the picture the emulator shows as a PNG
/// ([`retro_cli::image::write_frame_png`]), and say so.
pub(crate) fn save_screenshot(emu: &GenesisEmulator, output_path: &Path) -> Result<()> {
    let (width, height) = retro_cli::image::write_frame_png(emu, output_path)?;
    println!(
        "Screenshot saved: {} ({width}x{height})",
        output_path.display()
    );
    Ok(())
}
