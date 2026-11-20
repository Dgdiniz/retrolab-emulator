//! `sram dump|load`: read or seed a game's battery-backed save RAM.
//!
//! `dump` boots a ROM, runs `--frames` and writes the cartridge's SRAM
//! image (the `.srm` file every core of a console shares for a game).
//! `load` seeds an SRAM image, runs `--frames`, and can dump it again (to
//! see that the game changed it) and save a PNG, to check that a save file
//! is honoured.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use retro_core::EmulatorCore;

use crate::console::Console;
use crate::image::write_frame_png;
use crate::input::{InputArgs, InputPlan};
use crate::rom::{load, write_banner, RomArgs};

#[derive(clap::Args, Debug)]
pub struct SramArgs<B: clap::Args> {
    #[command(subcommand)]
    pub action: SramAction<B>,
}

#[derive(clap::Subcommand, Debug)]
pub enum SramAction<B: clap::Args> {
    /// Run a ROM and dump its SRAM to a file.
    Dump(DumpArgs<B>),
    /// Seed SRAM from a file, run, and optionally re-dump / screenshot.
    Load(LoadArgs<B>),
}

#[derive(clap::Args, Debug)]
pub struct DumpArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Frames to run before dumping SRAM, e.g. `1`.
    #[arg(long, short)]
    pub frames: Option<u64>,
    /// Output SRAM path.
    #[arg(long, short, default_value = "sram.srm")]
    pub output: PathBuf,
    #[command(flatten)]
    pub input: InputArgs,
}

#[derive(clap::Args, Debug)]
pub struct LoadArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// SRAM image to seed before running.
    #[arg(long, short)]
    pub sram: PathBuf,
    /// Frames to run after loading SRAM, e.g. `60` (1 s at 60 fps).
    #[arg(long, short)]
    pub frames: Option<u64>,
    /// Render a PNG after running.
    #[arg(long)]
    pub screenshot: Option<PathBuf>,
    /// Re-dump SRAM after running (to verify the game changed it).
    #[arg(long)]
    pub dump_sram: Option<PathBuf>,
    #[command(flatten)]
    pub input: InputArgs,
}

pub fn run<C: Console>(args: SramArgs<C::BootArgs>) -> Result<()> {
    match args.action {
        SramAction::Dump(args) => dump::<C>(args),
        SramAction::Load(args) => seed::<C>(args),
    }
}

fn dump<C: Console>(args: DumpArgs<C::BootArgs>) -> Result<()> {
    let Some(frames) = args.frames else {
        anyhow::bail!("specify --frames");
    };
    let input = args.input.plan::<C::Emulator>()?;
    let (_, mut emu) = load::<C>(&args.rom)?;
    write_banner(&mut emu, &mut std::io::stdout())?;
    run_frames(&mut emu, &input, frames);
    match emu.get_save_ram() {
        Some(sram) => write_sram(sram, &args.output, "SRAM dumped")?,
        None => println!("No SRAM present for this ROM."),
    }
    Ok(())
}

fn seed<C: Console>(args: LoadArgs<C::BootArgs>) -> Result<()> {
    let Some(frames) = args.frames else {
        anyhow::bail!("specify --frames");
    };
    let input = args.input.plan::<C::Emulator>()?;
    let (_, mut emu) = load::<C>(&args.rom)?;
    write_banner(&mut emu, &mut std::io::stdout())?;

    let sram = std::fs::read(&args.sram)
        .with_context(|| format!("failed to read SRAM: {}", args.sram.display()))?;
    println!(
        "Loading SRAM: {} bytes from {}",
        sram.len(),
        args.sram.display()
    );
    emu.set_save_ram(&sram);
    run_frames(&mut emu, &input, frames);

    if let Some(path) = &args.screenshot {
        let (width, height) = write_frame_png(&emu, path)?;
        println!(
            "Wrote {width}x{height} verification screenshot to {}",
            path.display()
        );
    }
    if let Some(path) = &args.dump_sram {
        match emu.get_save_ram() {
            Some(sram) => write_sram(sram, path, "SRAM re-dumped")?,
            None => println!("No SRAM present after run."),
        }
    }
    Ok(())
}

/// Run `frames` frames, the input applied before each.
fn run_frames<E: EmulatorCore>(emu: &mut E, input: &InputPlan, frames: u64) {
    println!("Running {frames} frames...");
    for frame in 0..frames {
        input.before_frame(emu, frame);
        emu.run_frame();
    }
}

/// Write an SRAM image to `path`, and say so: `{what}: N bytes (M
/// non-zero) -> PATH`.
fn write_sram(sram: &[u8], path: &Path, what: &str) -> Result<()> {
    let non_zero = sram.iter().filter(|&&b| b != 0).count();
    std::fs::write(path, sram)
        .with_context(|| format!("failed to write SRAM: {}", path.display()))?;
    println!(
        "{what}: {} bytes ({non_zero} non-zero) -> {}",
        sram.len(),
        path.display()
    );
    Ok(())
}
