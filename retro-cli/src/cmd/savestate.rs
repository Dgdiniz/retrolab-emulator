//! `savestate save|load`: write or restore the whole machine's state
//! (`EmulatorCore::save_state`, a state taken where a frame ends, and
//! `load_state`).
//!
//! `save` boots a ROM, runs `--frames` (none unless asked), optionally with
//! an SRAM image loaded first, and writes the state. `load` boots the same
//! ROM, restores the state, which must have been taken on that ROM,
//! optionally runs `--frames` more and can save a PNG to check it. A state
//! belongs to its core: another core's does not restore. A console's
//! start-up flags are part of what a state needs: give `load` the ones
//! `save` was given.

use std::path::PathBuf;

use anyhow::{Context, Result};
use retro_core::savestate::RomCheck;
use retro_core::EmulatorCore;

use crate::console::Console;
use crate::image::write_frame_png;
use crate::input::InputArgs;
use crate::rom::{load, write_banner, RomArgs};

#[derive(clap::Args, Debug)]
pub struct SavestateArgs<B: clap::Args> {
    #[command(subcommand)]
    pub action: SavestateAction<B>,
}

#[derive(clap::Subcommand, Debug)]
pub enum SavestateAction<B: clap::Args> {
    /// Run a ROM for a window and write its savestate.
    Save(SaveArgs<B>),
    /// Restore a savestate into a ROM and (optionally) screenshot it.
    Load(LoadArgs<B>),
}

#[derive(clap::Args, Debug)]
pub struct SaveArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Frames to run before capturing the state.
    #[arg(long, short, default_value_t = 0)]
    pub frames: u64,
    /// Output savestate path.
    #[arg(long, short, default_value = "savestate.bin")]
    pub output: PathBuf,
    /// Load an SRAM image before running.
    #[arg(long)]
    pub sram: Option<PathBuf>,
    #[command(flatten)]
    pub input: InputArgs,
}

#[derive(clap::Args, Debug)]
pub struct LoadArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Savestate file to restore.
    #[arg(long, short)]
    pub state: PathBuf,
    /// Frames to run after restoring (0 = just restore + report).
    #[arg(long, short, default_value_t = 0)]
    pub frames: u64,
    /// Render a verification PNG after restoring (+ optional extra frames).
    #[arg(long)]
    pub screenshot: Option<PathBuf>,
    #[command(flatten)]
    pub input: InputArgs,
}

pub fn run<C: Console>(args: SavestateArgs<C::BootArgs>) -> Result<()> {
    match args.action {
        SavestateAction::Save(args) => save::<C>(args),
        SavestateAction::Load(args) => restore::<C>(args),
    }
}

fn save<C: Console>(args: SaveArgs<C::BootArgs>) -> Result<()> {
    let input = args.input.plan::<C::Emulator>()?;
    let (_, mut emu) = load::<C>(&args.rom)?;
    write_banner(&mut emu, &mut std::io::stdout())?;

    if let Some(path) = &args.sram {
        let sram = std::fs::read(path)
            .with_context(|| format!("failed to read SRAM: {}", path.display()))?;
        emu.set_save_ram(&sram);
        println!("SRAM loaded: {} bytes from {}", sram.len(), path.display());
    }

    for frame in 0..args.frames {
        input.before_frame(&mut emu, frame);
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

fn restore<C: Console>(args: LoadArgs<C::BootArgs>) -> Result<()> {
    let input = args.input.plan::<C::Emulator>()?;
    let (_, mut emu) = load::<C>(&args.rom)?;

    let bytes = std::fs::read(&args.state)
        .with_context(|| format!("failed to read savestate: {}", args.state.display()))?;
    emu.load_state(&bytes, RomCheck::SameRom)
        .map_err(|e| anyhow::anyhow!("failed to load savestate: {e}"))?;
    println!(
        "Restored state from {} (frame_count={}, master_cycles={})",
        args.state.display(),
        emu.frame_count(),
        emu.master_cycles()
    );

    for frame in 0..args.frames {
        input.before_frame(&mut emu, frame);
        emu.run_frame();
    }
    if args.frames > 0 {
        println!(
            "Advanced {} frame(s) -> frame_count={}",
            args.frames,
            emu.frame_count()
        );
    }

    if let Some(path) = &args.screenshot {
        let (width, height) = write_frame_png(&emu, path)?;
        println!(
            "Wrote {width}x{height} verification screenshot to {}",
            path.display()
        );
    }
    Ok(())
}
