//! `sram dump|load`: inspect or seed a game's battery-backed save RAM.
//!
//! `dump` boots a ROM, advances `--frames`, and writes the cartridge SRAM
//! image (the `.srm` that the platform shares across cores for the same
//! game). `load` seeds an SRAM image, runs `--frames`, and can re-dump
//! (to verify the game mutated it) and/or screenshot the result, handy
//! for confirming a save file is honoured by the core.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use retro_core::EmulatorCore;

#[derive(Args, Debug)]
pub struct SramArgs {
    #[command(subcommand)]
    pub action: SramAction,
}

#[derive(Subcommand, Debug)]
pub enum SramAction {
    /// Run a ROM and dump its SRAM to a file.
    Dump(DumpArgs),
    /// Seed SRAM from a file, run, and optionally re-dump / screenshot.
    Load(LoadArgs),
}

#[derive(Args, Debug)]
pub struct DumpArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Frames to run before dumping SRAM.
    #[arg(short, long, default_value_t = 1)]
    pub frames: u64,
    /// Output SRAM path.
    #[arg(short, long, default_value = "sram.srm")]
    pub output: PathBuf,
}

#[derive(Args, Debug)]
pub struct LoadArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// SRAM image to seed before running.
    #[arg(short, long)]
    pub sram: PathBuf,
    /// Frames to run after loading SRAM.
    #[arg(short, long, default_value_t = 60)]
    pub frames: u64,
    /// Render a PNG after running.
    #[arg(long)]
    pub screenshot: Option<PathBuf>,
    /// Re-dump SRAM after running (to verify the game changed it).
    #[arg(long)]
    pub dump_sram: Option<PathBuf>,
}

pub fn run(args: SramArgs) -> Result<()> {
    match args.action {
        SramAction::Dump(a) => dump(a),
        SramAction::Load(a) => load(a),
    }
}

fn dump(args: DumpArgs) -> Result<()> {
    let (_, mut emu) = super::load_rom(&args.rom)?;
    super::print_rom_title(&emu);

    println!("Running {} frames...", args.frames);
    for _ in 0..args.frames {
        emu.run_frame_fast();
    }

    match emu.get_save_ram() {
        Some(sram) => {
            let non_zero = sram.iter().filter(|&&b| b != 0).count();
            std::fs::write(&args.output, sram)
                .with_context(|| format!("failed to write SRAM: {}", args.output.display()))?;
            println!(
                "SRAM dumped: {} bytes ({} non-zero) -> {}",
                sram.len(),
                non_zero,
                args.output.display()
            );
        }
        None => println!("No SRAM present for this ROM."),
    }
    Ok(())
}

fn load(args: LoadArgs) -> Result<()> {
    let (_, mut emu) = super::load_rom(&args.rom)?;
    super::print_rom_title(&emu);

    let sram_data = std::fs::read(&args.sram)
        .with_context(|| format!("failed to read SRAM: {}", args.sram.display()))?;
    println!(
        "Loading SRAM: {} bytes from {}",
        sram_data.len(),
        args.sram.display()
    );
    emu.set_save_ram(&sram_data);

    println!("Running {} frames...", args.frames);
    for _ in 0..args.frames {
        emu.run_frame_fast();
    }

    if let Some(ref ss_path) = args.screenshot {
        super::save_screenshot(&emu, ss_path)?;
    }

    if let Some(ref dump_path) = args.dump_sram {
        match emu.get_save_ram() {
            Some(sram) => {
                let non_zero = sram.iter().filter(|&&b| b != 0).count();
                std::fs::write(dump_path, sram)
                    .with_context(|| format!("failed to write SRAM: {}", dump_path.display()))?;
                println!(
                    "SRAM re-dumped: {} bytes ({} non-zero) -> {}",
                    sram.len(),
                    non_zero,
                    dump_path.display()
                );
            }
            None => println!("No SRAM present after run."),
        }
    }
    Ok(())
}
