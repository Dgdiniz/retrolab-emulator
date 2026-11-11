//! `run`: boot a ROM and advance it for a bounded window (frames,
//! instructions, until VBlank, or until the 68000 reaches a PC),
//! printing an execution summary.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};

/// Safety bound for `--instructions` / `--until-pc` so a stuck core
/// cannot spin forever.
const MAX_STEPS: u64 = 2_000_000_000;

#[derive(Args, Debug)]
pub struct RunArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Run for exactly N frames.
    #[arg(long, short)]
    pub frames: Option<u64>,
    /// Step exactly N 68000 instructions.
    #[arg(long, short)]
    pub instructions: Option<u64>,
    /// Run a single frame and stop at VBlank.
    #[arg(long)]
    pub until_vblank: bool,
    /// Step until the 68000 reaches this 24-bit PC (hex, e.g. `000400`).
    #[arg(long, value_parser = parse_hex_u32)]
    pub until_pc: Option<u32>,
    /// Show a progress bar.
    #[arg(long)]
    pub progress: bool,
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

    let (_, mut emu) = super::load_rom(&args.rom)?;
    super::print_rom_title(&emu);

    let start = Instant::now();
    let mut instructions_executed = 0u64;

    if let Some(n) = args.frames {
        emu.debugger().set_execution_mode(ExecutionMode::Running);
        let pb = make_bar(args.progress, n, "frames");
        for _ in 0..n {
            emu.run_frame();
            if let Some(pb) = &pb {
                pb.inc(1);
            }
        }
        finish_bar(pb);
    } else if let Some(n) = args.instructions {
        emu.debugger().set_execution_mode(ExecutionMode::StepInto);
        let pb = make_bar(args.progress, n, "instructions");
        while instructions_executed < n {
            if emu.step().instruction_executed {
                instructions_executed += 1;
                if let Some(pb) = &pb {
                    pb.inc(1);
                }
            }
        }
        finish_bar(pb);
    } else if args.until_vblank {
        emu.run_until_vblank();
    } else if let Some(target) = args.until_pc {
        emu.debugger().set_execution_mode(ExecutionMode::StepInto);
        let mut steps = 0u64;
        while (emu.cpu().pc & 0x00FF_FFFF) != (target & 0x00FF_FFFF) {
            if emu.step().instruction_executed {
                instructions_executed += 1;
            }
            steps += 1;
            if steps >= MAX_STEPS {
                anyhow::bail!(
                    "reached {MAX_STEPS} steps without hitting PC ${target:06X} (last PC ${:06X})",
                    emu.cpu().pc & 0x00FF_FFFF
                );
            }
        }
    }

    let elapsed = start.elapsed();
    println!();
    println!("Frames:       {}", emu.frame_count());
    if instructions_executed > 0 {
        println!("Instructions: {instructions_executed}");
    }
    println!("Wall time:    {:.3} s", elapsed.as_secs_f64());
    if elapsed.as_secs_f64() > 0.0 && emu.frame_count() > 0 {
        println!(
            "Speed:        {:.1} fps",
            emu.frame_count() as f64 / elapsed.as_secs_f64()
        );
    }
    if let Some(target) = args.until_pc {
        println!("Stopped at PC: ${target:06X}");
    }
    Ok(())
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
