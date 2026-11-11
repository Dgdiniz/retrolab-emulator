//! `savestate save|load`: capture or restore the full emulator state via
//! `EmulatorCore::save_state` / `load_state`.
//!
//! `save` boots a ROM, advances `--frames`, and writes the compressed state
//! envelope. `load` boots the same ROM, restores the envelope, optionally
//! advances `--frames` more, and can render a verification PNG (reusing the
//! `screenshot` crop helper). The envelope is ROM-hash-stamped, so a state
//! captured against a different game is rejected on load. Both take the
//! DSP-1 flags: a state saved on the low-level emulation carries the
//! uPD7725's registers, and only a load that runs the same emulation has a
//! chip to put them back into.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use retro_core::EmulatorCore;
use snes1_core::SnesEmulator;

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
    /// Path to the ROM file (`.sfc` / `.smc`).
    pub rom: PathBuf,
    /// Frames to run before capturing the state.
    #[arg(long, default_value_t = 0)]
    pub frames: u64,
    /// Output savestate path.
    #[arg(long, short)]
    pub output: PathBuf,
    /// DSP-1 firmware + coprocessor-mode flags.
    #[command(flatten)]
    pub coprocessor: super::CoprocessorArgs,
}

#[derive(Args, Debug)]
pub struct LoadArgs {
    /// Path to the ROM file (`.sfc` / `.smc`) the state was captured against.
    pub rom: PathBuf,
    /// Savestate file to restore.
    #[arg(long)]
    pub state: PathBuf,
    /// Frames to run after restoring (0 = just restore + report).
    #[arg(long, default_value_t = 0)]
    pub frames: u64,
    /// Render a verification PNG after restoring (+ optional extra frames).
    #[arg(long)]
    pub screenshot: Option<PathBuf>,
    /// DSP-1 firmware + coprocessor-mode flags; give the ones the state
    /// was saved with.
    #[command(flatten)]
    pub coprocessor: super::CoprocessorArgs,
}

pub fn run(args: SavestateArgs) -> Result<()> {
    match args.action {
        SavestateAction::Save(a) => save(a),
        SavestateAction::Load(a) => load(a),
    }
}

fn save(args: SaveArgs) -> Result<()> {
    let rom = super::read_rom(&args.rom)?;
    let mut emu = SnesEmulator::new();
    args.coprocessor.apply(&mut emu)?;
    emu.load_rom(&rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;

    if let Some(info) = emu.get_rom_info() {
        println!("Loaded {:?} [{}]", info.title.trim(), info.region);
    }
    if let Some(chip) = emu.coprocessor_label() {
        println!("Coprocessor: {chip}");
    }

    for _ in 0..args.frames {
        emu.run_frame();
    }

    let bytes = emu.save_state();
    if bytes.is_empty() {
        anyhow::bail!("save_state() returned no bytes (serialization failed)");
    }
    std::fs::write(&args.output, &bytes)
        .with_context(|| format!("failed to write savestate: {}", args.output.display()))?;

    println!(
        "Saved {} byte state to {} (after {} frame(s), frame_count={})",
        bytes.len(),
        args.output.display(),
        args.frames,
        emu.frame_count()
    );
    Ok(())
}

fn load(args: LoadArgs) -> Result<()> {
    let rom = super::read_rom(&args.rom)?;
    let mut emu = SnesEmulator::new();
    args.coprocessor.apply(&mut emu)?;
    emu.load_rom(&rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;

    let bytes = std::fs::read(&args.state)
        .with_context(|| format!("failed to read savestate: {}", args.state.display()))?;
    emu.load_state(&bytes)
        .map_err(|e| anyhow::anyhow!("failed to load savestate: {e}"))?;

    println!(
        "Restored state from {} (frame_count={}, master_cycles={})",
        args.state.display(),
        emu.frame_count(),
        emu.master_cycles()
    );

    for _ in 0..args.frames {
        emu.run_frame();
    }
    if args.frames > 0 {
        println!(
            "Advanced {} frame(s) -> frame_count={}",
            args.frames,
            emu.frame_count()
        );
    }

    if let Some(png) = args.screenshot {
        let (disp_w, disp_h) = {
            let disp = emu.get_display_info();
            (disp.width, disp.height)
        };
        let fb = emu.get_framebuffer();
        let (w, h, rgb) =
            super::screenshot::crop_framebuffer_rgb(&fb.data, fb.width, fb.height, disp_w, disp_h);
        let img = image::RgbImage::from_raw(w, h, rgb)
            .ok_or_else(|| anyhow::anyhow!("failed to build {w}x{h} image from framebuffer"))?;
        img.save(&png)
            .with_context(|| format!("failed to save screenshot: {}", png.display()))?;
        println!(
            "Wrote {}x{} verification screenshot to {}",
            w,
            h,
            png.display()
        );
    }

    Ok(())
}
