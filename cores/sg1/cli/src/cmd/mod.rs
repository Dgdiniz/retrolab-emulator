//! The Mega Drive's own commands: each module owns its clap arguments and
//! a `run` entry point, and [`Command`] lists them for `retro-cli`, which
//! runs the commands both tools share. The helpers the commands share live
//! here.

pub mod audio_dump;
pub mod audio_info;
pub mod benchmark;
pub mod determinism_check;
pub mod disasm;
pub mod dump_vdp;
pub mod fingerprint;
pub mod info;
pub mod memory;
pub mod profile;
pub mod run;
pub mod savestate;
pub mod screenshot;
pub mod sound_trace;
pub mod sram;
pub mod watch_ram;
pub mod watch_read;
pub mod watch_vram;

use std::path::Path;

use anyhow::{Context, Result};
use clap::Subcommand;
use retro_cli::RomArgs;
use retro_core::cartridge::ExpansionChipState;
use retro_core::input_schedule::PressSchedule;
use retro_core::EmulatorCore;
use sg1_core::GenesisEmulator;

pub(crate) use retro_cli::output::open_output;

use crate::console::Genesis;

/// The Mega Drive's own commands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Inspect a ROM's header (title, region, checksum, mapper).
    Info(info::InfoArgs),
    /// Boot a ROM and run it for a bounded window.
    Run(run::RunArgs),
    /// Run a ROM and save the framebuffer as a PNG (+ layer dumps).
    Screenshot(screenshot::ScreenshotArgs),
    /// Forward-disassemble a CPU's instruction stream.
    Disasm(disasm::DisasmArgs),
    /// Run a ROM and dump a region of CPU-visible memory (hex + ASCII).
    Memory(memory::MemoryArgs),
    /// Run a ROM and dump a full VDP state report.
    DumpVdp(dump_vdp::DumpVdpArgs),
    /// Log VDP writes that land in a VRAM address window while running.
    WatchVram(watch_vram::WatchVramArgs),
    /// Single-step a window and name the instruction that writes a RAM cell.
    WatchRam(watch_ram::WatchRamArgs),
    /// Single-step a window and name the instructions that read an address range.
    WatchRead(watch_read::WatchReadArgs),
    /// Emit a per-frame CRC32 of a work-RAM region for divergence bisection.
    Fingerprint(fingerprint::FingerprintArgs),
    /// Verify deterministic replay + savestate completeness for netplay.
    DeterminismCheck(determinism_check::DeterminismCheckArgs),
    /// Benchmark the execution tiers (fast / light-debug / +trace / full).
    Benchmark(benchmark::BenchmarkArgs),
    /// Profile per-subsystem frame timing and rank bottlenecks.
    Profile(profile::ProfileArgs),
    /// Run a ROM with audio capture and save the output as a WAV.
    AudioDump(audio_dump::AudioDumpArgs),
    /// Print a PSG + YM2612 channel-state summary.
    AudioInfo(audio_info::AudioInfoArgs),
    /// Dump YM2612 + PSG register writes with their in-frame sample offset.
    SoundTrace(sound_trace::SoundTraceArgs),
    /// Save or load an emulator savestate.
    Savestate(savestate::SavestateArgs),
    /// Dump or seed a game's battery-backed SRAM.
    Sram(sram::SramArgs),
}

/// Run one of the Mega Drive's own commands.
pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Info(args) => info::run(args),
        Command::Run(args) => run::run(args),
        Command::Screenshot(args) => screenshot::run(args),
        Command::Disasm(args) => disasm::run(args),
        Command::Memory(args) => memory::run(args),
        Command::DumpVdp(args) => dump_vdp::run(args),
        Command::WatchVram(args) => watch_vram::run(args),
        Command::WatchRam(args) => watch_ram::run(args),
        Command::WatchRead(args) => watch_read::run(args),
        Command::Fingerprint(args) => fingerprint::run(args),
        Command::DeterminismCheck(args) => determinism_check::run(args),
        Command::Benchmark(args) => benchmark::run(args),
        Command::Profile(args) => profile::run(args),
        Command::AudioDump(args) => audio_dump::run(args),
        Command::AudioInfo(args) => audio_info::run(args),
        Command::SoundTrace(args) => sound_trace::run(args),
        Command::Savestate(args) => savestate::run(args),
        Command::Sram(args) => sram::run(args),
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

/// Print the installed coprocessor(s): a label summary followed by each
/// chip's live state. A plain cartridge prints `Coprocessor: none`; a
/// multi-chip cartridge enumerates every chip by `chip_id`. Shared by
/// `info` and `screenshot --coprocessor-state`, mirroring the snes1-cli
/// helper of the same name.
pub(crate) fn print_coprocessors(emu: &GenesisEmulator) {
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

/// Print one coprocessor chip's live state (for the SVP: the SSP1601
/// program counter, cycle count, and IRAM size). Values reflect however
/// many frames the caller ran before dumping, `info` defaults to a reset
/// snapshot (`--frames 0`), while `info --frames N` and
/// `screenshot --coprocessor-state` show live state after N frames (e.g.
/// the SVP's `Cycle count` climbing once Virtua Racing has driven it).
pub(crate) fn print_coprocessor_state(chip_id: u8, state: &ExpansionChipState) {
    println!();
    println!("Coprocessor chip state [{chip_id}]:");
    println!("  Type:           {:?}", state.chip_type);
    match state.program_counter {
        Some(pc) => println!("  PC:             ${pc:04x}"),
        None => println!("  PC:             (idle - no program loaded)"),
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

/// Force 6-button pads on both DE-9 ports.
///
/// `load_rom` already auto-selects 6-button when the cartridge header
/// advertises `'6'` in the I/O-support field; call this only for the
/// explicit `--six-button` override (or to force 6-button on a ROM that
/// omits the tag).
pub(crate) fn connect_controllers(emu: &mut GenesisEmulator, six_button: bool) {
    emu.connect_controllers(six_button);
}

/// Save the emulator's framebuffer as a PNG. The Genesis framebuffer is
/// already display-sized (no overscan crop needed), so the whole RGBA
/// buffer is converted to RGB and encoded as-is.
pub(crate) fn save_screenshot(emu: &GenesisEmulator, output_path: &Path) -> Result<()> {
    let fb = emu.get_framebuffer();
    let width = fb.width;
    let height = fb.height;

    if fb.data.is_empty() || width == 0 || height == 0 {
        anyhow::bail!("framebuffer is empty ({width}x{height})");
    }

    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for px in fb.data.as_chunks::<4>().0 {
        rgb.push(px[0]);
        rgb.push(px[1]);
        rgb.push(px[2]);
    }

    let img = image::RgbImage::from_raw(width, height, rgb).ok_or_else(|| {
        anyhow::anyhow!("failed to build {width}x{height} image from framebuffer")
    })?;
    img.save(output_path)
        .with_context(|| format!("failed to save screenshot: {}", output_path.display()))?;

    println!(
        "Screenshot saved: {} ({}x{})",
        output_path.display(),
        width,
        height
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
