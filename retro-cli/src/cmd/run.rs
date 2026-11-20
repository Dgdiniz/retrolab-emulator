//! `run`: boot a ROM and run it for a bounded window (frames, instructions
//! of the main CPU, the first frame up to its vertical blanking, or until
//! the main CPU reaches an address), then print what ran.

use std::time::Instant;

use anyhow::Result;
use retro_core::debug::MAIN_CPU;
use retro_core::EmulatorCore;

use crate::console::{address_mask, held_program_counter, program_counter, Console};
use crate::input::InputArgs;
use crate::parse::parse_hex_u32;
use crate::progress;
use crate::rom::{load, write_banner, RomArgs};
use crate::trace::STEP_CAP;

#[derive(clap::Args, Debug)]
pub struct RunArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Run for exactly N frames.
    #[arg(long, short)]
    pub frames: Option<u64>,
    /// Run exactly N instructions of the main CPU.
    #[arg(long, short)]
    pub instructions: Option<u64>,
    /// Run the first frame to the start of its vertical blanking and stop
    /// there.
    #[arg(long)]
    pub until_vblank: bool,
    /// Step until the main CPU reaches this PC (hex).
    #[arg(long, value_parser = parse_hex_u32)]
    pub until_pc: Option<u32>,
    /// Show a progress bar.
    #[arg(long)]
    pub progress: bool,
    #[command(flatten)]
    pub input: InputArgs,
}

pub fn run<C: Console>(args: RunArgs<C::BootArgs>) -> Result<()> {
    let modes = [
        args.frames.is_some(),
        args.instructions.is_some(),
        args.until_vblank,
        args.until_pc.is_some(),
    ]
    .iter()
    .filter(|on| **on)
    .count();
    if modes == 0 {
        anyhow::bail!("specify one of --frames, --instructions, --until-vblank, --until-pc");
    }
    if modes > 1 {
        anyhow::bail!(
            "--frames / --instructions / --until-vblank / --until-pc are mutually exclusive"
        );
    }
    let input = args.input.plan::<C::Emulator>()?;

    let (_, mut emu) = load::<C>(&args.rom)?;
    write_banner(&mut emu, &mut std::io::stdout())?;
    // A host step records its instruction in the history, which nothing
    // here reads: the steps below keep none.
    emu.debugger().set_history_enabled(MAIN_CPU, false);

    let start = Instant::now();
    let mut instructions = 0u64;
    let mut stopped_at = None;

    if let Some(n) = args.frames {
        let bar = progress::bar(args.progress, n, "frames");
        for frame in 0..n {
            input.before_frame(&mut emu, frame);
            emu.run_frame();
            if let Some(bar) = &bar {
                bar.inc(1);
            }
        }
        progress::finish(bar);
    } else if let Some(n) = args.instructions {
        let bar = progress::bar(args.progress, n, "instructions");
        let mut steps = 0u64;
        while instructions < n {
            // A step that only waits, or only takes an interrupt, runs no
            // instruction.
            let stepped = emu.step_cpu(MAIN_CPU);
            steps += 1;
            if stepped.instruction_executed {
                instructions += 1;
                if let Some(bar) = &bar {
                    bar.inc(1);
                }
            }
            if steps >= STEP_CAP && instructions < n {
                anyhow::bail!(
                    "reached {STEP_CAP} steps with {instructions} of {n} instructions run (last PC \
                     ${:06X})",
                    held_program_counter(&emu, MAIN_CPU)
                );
            }
        }
        progress::finish(bar);
    } else if args.until_vblank {
        input.before_frame(&mut emu, 0);
        emu.run_until_vblank();
        println!("Stopped at the start of vertical blanking.");
    } else if let Some(target) = args.until_pc {
        let bits = emu
            .system_info()
            .cpus
            .get(MAIN_CPU)
            .map_or(32, |cpu| cpu.address_width);
        let target = target & address_mask(bits);
        // A host step refreshes the debugger's CPU states after it, so the
        // program counter is read afresh only before the first.
        let mut pc = program_counter(&mut emu, MAIN_CPU);
        let mut steps = 0u64;
        while pc != target {
            if steps >= STEP_CAP {
                anyhow::bail!(
                    "reached {STEP_CAP} steps without hitting PC ${target:06X} (last PC ${pc:06X})"
                );
            }
            // A step that only waits, or only takes an interrupt, runs no
            // instruction.
            if emu.step_cpu(MAIN_CPU).instruction_executed {
                instructions += 1;
            }
            steps += 1;
            pc = held_program_counter(&emu, MAIN_CPU);
        }
        stopped_at = Some(target);
    }

    let elapsed = start.elapsed().as_secs_f64();
    println!();
    println!("Frames:        {}", emu.frame_count());
    if instructions > 0 {
        println!("Instructions:  {instructions}");
    }
    println!("Master cycles: {}", emu.master_cycles());
    println!("Wall time:     {elapsed:.3} s");
    if elapsed > 0.0 && emu.frame_count() > 0 {
        println!(
            "Speed:         {:.1} fps",
            emu.frame_count() as f64 / elapsed
        );
    }
    if let Some(target) = stopped_at {
        println!("Stopped at PC: ${target:06X}");
    }
    Ok(())
}
