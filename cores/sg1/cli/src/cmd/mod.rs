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
use retro_cli::RomArgs;
use retro_core::input_schedule::PressSchedule;
use retro_core::EmulatorCore;
use sg1_core::GenesisEmulator;

pub(crate) use retro_cli::output::open_output;

use crate::console::Genesis;

/// The Mega Drive's own commands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run a ROM and dump a full VDP state report.
    DumpVdp(dump_vdp::DumpVdpArgs),
    /// Log VDP writes that land in a VRAM address window while running.
    WatchVram(watch_vram::WatchVramArgs),
    /// Single-step a window and name the instructions that read an address range.
    WatchRead(watch_read::WatchReadArgs),
    /// Print a PSG + YM2612 channel-state summary.
    AudioInfo(audio_info::AudioInfoArgs),
    /// Dump YM2612 + PSG register writes with their in-frame sample offset.
    SoundTrace(sound_trace::SoundTraceArgs),
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

/// Read a ROM and boot a new emulator with it, the start-up flags at
/// their defaults: the ROM's bytes (some callers report the size) and the
/// loaded emulator.
pub(crate) fn load_rom(path: &Path) -> Result<(Vec<u8>, GenesisEmulator)> {
    retro_cli::rom::load::<Genesis>(&RomArgs::new(path))
}

/// Print the parsed ROM title (best-effort) to stdout.
pub(crate) fn print_rom_title(emu: &GenesisEmulator) {
    if let Some(info) = emu.get_rom_info() {
        println!("Title: {}", info.title.trim());
    }
}

/// Force 6-button pads on both DE-9 ports.
///
/// `load_rom` already auto-selects 6-button when the cartridge header
/// advertises `'6'` in the I/O-support field; call this only for the
/// explicit `--six-button` override (or to force 6-button on a ROM that
/// omits the tag).
pub(crate) fn connect_controllers(emu: &mut GenesisEmulator, six_button: bool) {
    emu.connect_controllers(six_button);
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

/// Parse a `--press-at "F:buttons;F:buttons"` schedule (e.g.
/// `"180:start;186:;240:a+c"`) in the pad's button names; `None` when none
/// was given. Each set-point holds its buttons from frame `F` until the
/// next; an empty list releases.
pub(crate) fn parse_press_schedule(spec: Option<&str>) -> Result<Option<PressSchedule>> {
    retro_cli::input::parse_press_schedule::<GenesisEmulator>(spec)
}

/// Install player 1's buttons for `frame` from `schedule` (if any), from
/// its first set-point on, through `set_input`, which a frame's input
/// resync keeps.
pub(crate) fn apply_press(emu: &mut GenesisEmulator, schedule: Option<&PressSchedule>, frame: u64) {
    if let Some(schedule) = schedule {
        retro_cli::input::apply_schedule(emu, schedule, frame);
    }
}
