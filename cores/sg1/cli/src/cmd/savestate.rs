//! `savestate save|load`: capture or restore the full emulator state via
//! `EmulatorCore::save_state` / `load_state`.
//!
//! `save` boots a ROM, advances `--frames`, and writes the binary state
//! blob (optionally pre-loading an SRAM image first). `load` boots the
//! same ROM, restores the blob, optionally advances `--frames` more, and
//! can render a verification PNG. Savestates are per-core (the binary
//! format differs between cores), so a state captured against a different
//! ROM/core will not restore cleanly.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use retro_core::EmulatorCore;

#[derive(Args, Debug)]
pub struct SavestateArgs {
    #[command(subcommand)]
    pub action: SavestateAction,
}

#[derive(Subcommand, Debug)]
pub enum SavestateAction {
    /// Run a ROM for a window and write its savestate.
    Save(SaveArgs),
    /// Restore a savestate into a ROM and (optionally) screenshot it.
    Load(LoadArgs),
}

#[derive(Args, Debug)]
pub struct SaveArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Frames to run before capturing the state.
    #[arg(short, long, default_value_t = 1)]
    pub frames: u64,
    /// Output savestate path.
    #[arg(short, long, default_value = "savestate.bin")]
    pub output: PathBuf,
    /// Load an SRAM image before running.
    #[arg(long)]
    pub sram: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct LoadArgs {
    /// Path to the ROM file the state was captured against.
    pub rom: PathBuf,
    /// Savestate file to restore.
    #[arg(short, long)]
    pub state: PathBuf,
    /// Frames to run after restoring (0 = just restore + report).
    #[arg(short, long, default_value_t = 1)]
    pub frames: u64,
    /// Render a verification PNG after restoring (+ optional extra frames).
    #[arg(long)]
    pub screenshot: Option<PathBuf>,
}

pub fn run(args: SavestateArgs) -> Result<()> {
    match args.action {
        SavestateAction::Save(a) => save(a),
        SavestateAction::Load(a) => load(a),
    }
}

fn save(args: SaveArgs) -> Result<()> {
    let (_, mut emu) = super::load_rom(&args.rom)?;
    super::print_rom_title(&emu);

    if let Some(ref sram_file) = args.sram {
        let sram_data = std::fs::read(sram_file)
            .with_context(|| format!("failed to read SRAM: {}", sram_file.display()))?;
        emu.set_save_ram(&sram_data);
        println!(
            "SRAM loaded: {} bytes from {}",
            sram_data.len(),
            sram_file.display()
        );
    }

    println!("Running {} frames...", args.frames);
    for _ in 0..args.frames {
        emu.run_frame_fast();
    }

    let state_data = emu.save_state();
    if state_data.is_empty() {
        anyhow::bail!("save_state() returned empty data");
    }
    std::fs::write(&args.output, &state_data)
        .with_context(|| format!("failed to write savestate: {}", args.output.display()))?;

    println!(
        "State saved: {} bytes ({:.1} KB) -> {}",
        state_data.len(),
        state_data.len() as f64 / 1024.0,
        args.output.display()
    );
    Ok(())
}

fn load(args: LoadArgs) -> Result<()> {
    let (_, mut emu) = super::load_rom(&args.rom)?;
    super::print_rom_title(&emu);

    let state_data = std::fs::read(&args.state)
        .with_context(|| format!("failed to read savestate: {}", args.state.display()))?;
    println!(
        "Loading state: {} bytes from {}",
        state_data.len(),
        args.state.display()
    );
    emu.load_state(&state_data)
        .map_err(|e| anyhow::anyhow!("failed to load state: {e}"))?;

    println!(
        "State loaded successfully. Running {} frames...",
        args.frames
    );
    for _ in 0..args.frames {
        emu.run_frame_fast();
    }

    if let Some(ref ss_path) = args.screenshot {
        super::save_screenshot(&emu, ss_path)?;
    }
    Ok(())
}
