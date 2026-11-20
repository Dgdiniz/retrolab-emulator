//! `run`: boot a ROM and advance it for a bounded window (frames,
//! instructions, until VBlank, or until the 65C816 reaches a PC),
//! printing an execution summary.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};
use retro_core::EmulatorCore;
use snes1_core::{SnesEmulator, MAIN_CPU};

/// Safety bound for `--instructions` / `--until-pc` so a stuck core
/// cannot spin forever.
const MAX_STEPS: u64 = 2_000_000_000;

#[derive(Args, Debug)]
pub struct RunArgs {
    /// Path to the ROM file (`.sfc` / `.smc`).
    pub rom: PathBuf,
    /// Run for exactly N frames.
    #[arg(long)]
    pub frames: Option<u64>,
    /// Run exactly N 65C816 instructions; a step that only waits or only
    /// takes an interrupt is not one.
    #[arg(long)]
    pub instructions: Option<u64>,
    /// Run the first frame to the start of its vertical blanking and stop
    /// there, the picture drawn and the NMI not yet raised.
    #[arg(long)]
    pub until_vblank: bool,
    /// Step until the 65C816 reaches this 24-bit PC (hex, e.g. `00ff03`).
    #[arg(long, value_parser = parse_hex_u32)]
    pub until_pc: Option<u32>,
    /// Show a progress bar.
    #[arg(long)]
    pub progress: bool,
    /// DSP-1 firmware + coprocessor-mode flags.
    #[command(flatten)]
    pub coprocessor: super::CoprocessorArgs,
}

fn parse_hex_u32(s: &str) -> Result<u32, String> {
    let cleaned = s.trim().trim_start_matches("0x").trim_start_matches('$');
    u32::from_str_radix(cleaned, 16).map_err(|e| format!("invalid hex address '{s}': {e}"))
}

pub fn run(args: RunArgs) -> Result<()> {
    let mode_count = [
        args.frames.is_some(),
        args.instructions.is_some(),
        args.until_vblank,
        args.until_pc.is_some(),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    if mode_count == 0 {
        anyhow::bail!("specify one of --frames, --instructions, --until-vblank, --until-pc");
    }
    if mode_count > 1 {
        anyhow::bail!(
            "--frames / --instructions / --until-vblank / --until-pc are mutually exclusive"
        );
    }

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
    // A host step records its instruction in the history, which nothing
    // here reads: the steps below keep none.
    emu.debugger().set_history_enabled(MAIN_CPU, false);

    let start = Instant::now();
    let mut instructions_executed = 0u64;

    if let Some(n) = args.frames {
        let pb = make_bar(args.progress, n, "frames");
        for _ in 0..n {
            emu.run_frame();
            if let Some(pb) = &pb {
                pb.inc(1);
            }
        }
        finish_bar(pb);
    } else if let Some(n) = args.instructions {
        let pb = make_bar(args.progress, n, "instructions");
        let mut steps = 0u64;
        while instructions_executed < n {
            // A step that only waits, or only takes an interrupt, runs no
            // instruction.
            let r = emu.step_cpu(0);
            steps += 1;
            if r.instruction_executed {
                instructions_executed += 1;
                if let Some(pb) = &pb {
                    pb.inc(1);
                }
            }
            if steps >= MAX_STEPS {
                anyhow::bail!(
                    "reached {MAX_STEPS} steps with {instructions_executed} of {n} instructions \
                     run (last PC ${:06x})",
                    current_pc(&emu)
                );
            }
        }
        finish_bar(pb);
    } else if args.until_vblank {
        emu.run_until_vblank();
        println!("Stopped at the start of vertical blanking.");
    } else if let Some(target) = args.until_pc {
        let mut steps = 0u64;
        loop {
            if current_pc(&emu) == target {
                break;
            }
            emu.step_cpu(0);
            instructions_executed += 1;
            steps += 1;
            if steps >= MAX_STEPS {
                anyhow::bail!(
                    "reached {MAX_STEPS} steps without hitting PC ${target:06x} (last PC ${:06x})",
                    current_pc(&emu)
                );
            }
        }
    }

    let elapsed = start.elapsed();
    println!();
    println!("Frames:        {}", emu.frame_count());
    if instructions_executed > 0 {
        println!("Instructions:  {instructions_executed}");
    }
    println!("Master cycles: {}", emu.master_cycles());
    println!("Wall time:     {:.3} s", elapsed.as_secs_f64());
    if elapsed.as_secs_f64() > 0.0 && emu.frame_count() > 0 {
        println!(
            "Speed:         {:.1} fps",
            emu.frame_count() as f64 / elapsed.as_secs_f64()
        );
    }
    if let Some(target) = args.until_pc {
        println!("Stopped at PC: ${target:06x}");
    }
    Ok(())
}

fn current_pc(emu: &SnesEmulator) -> u32 {
    let cpu = emu.cpu();
    ((cpu.pbr as u32) << 16) | cpu.pc as u32
}

fn make_bar(show: bool, len: u64, unit: &str) -> Option<ProgressBar> {
    if !show {
        return None;
    }
    let pb = ProgressBar::new(len);
    pb.set_style(
        ProgressStyle::default_bar()
            .template(&format!(
                "{{spinner:.green}} [{{bar:40.cyan/blue}}] {{pos}}/{{len}} {unit} ({{eta}})"
            ))
            .unwrap()
            .progress_chars("#>-"),
    );
    Some(pb)
}

fn finish_bar(pb: Option<ProgressBar>) {
    if let Some(pb) = pb {
        pb.finish_and_clear();
    }
}
