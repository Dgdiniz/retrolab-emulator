//! `benchmark`: measure emulation speed on the frame driver's tiers: the
//! play tier (no debug), the light debug tier (a breakpoint active, then a
//! trace active), and the stepping a debugger does (a frame's worth of host
//! steps, each one instruction with its history and the CPU states
//! written).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Args;
use retro_core::frame::run_frame_on;
use retro_core::types::{Breakpoint, BreakpointType};
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode, ExecutionTier};
use sg1_core::debugger::MAIN_CPU;
use sg1_core::GenesisEmulator;

/// The longest the stepping row runs for: it takes as many frames as fit,
/// when the asked-for count would take longer.
const STEPPING_TIME_LIMIT: Duration = Duration::from_secs(15);

#[derive(Args, Debug)]
pub struct BenchmarkArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Number of frames to run per tier (default: 600 = 10 seconds). The
    /// stepping row runs fewer when these would take more than 15 seconds.
    #[arg(short, long, default_value_t = 600)]
    pub frames: u64,
}

pub fn run(args: BenchmarkArgs) -> Result<()> {
    let (rom, mut emu) = super::load_rom(&args.rom)?;
    let n = args.frames;

    println!("=== SG1 Benchmark ===");
    println!("ROM: {}", args.rom.display());
    println!("ROM size: {} bytes", rom.len());
    super::print_rom_title(&emu);

    // The play tier (no debug).
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    println!();
    println!("Running {n} frames on the play tier (no debug)...");
    let start = Instant::now();
    for _ in 0..n {
        run_frame_on(&mut emu, ExecutionTier::Play);
    }
    let ms_play = report(start.elapsed().as_secs_f64() * 1000.0, n);

    // The light debug tier with a breakpoint active that nothing reaches.
    emu.reset();
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    emu.debugger().add_breakpoint(Breakpoint {
        address: 0xFFFFFF,
        bp_type: BreakpointType::execute(),
        cpu_id: None,
        condition: None,
    });
    println!();
    println!("Running {n} frames on the light debug tier (breakpoint active)...");
    let start = Instant::now();
    for _ in 0..n {
        run_frame_on(&mut emu, ExecutionTier::LightDebug);
    }
    let ms_light = report(start.elapsed().as_secs_f64() * 1000.0, n);
    let bps: Vec<_> = emu
        .debugger()
        .list_breakpoints()
        .iter()
        .map(|b| b.id)
        .collect();
    for id in bps {
        emu.debugger().remove_breakpoint(id);
    }

    // The light debug tier with the 68000 traced.
    emu.reset();
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    emu.debugger().set_tracing_enabled(MAIN_CPU, true);
    println!();
    println!("Running {n} frames on the light debug tier (trace active)...");
    let start = Instant::now();
    for _ in 0..n {
        run_frame_on(&mut emu, ExecutionTier::LightDebug);
        emu.debugger().clear_trace_flushed(MAIN_CPU);
    }
    let ms_light_trace = report(start.elapsed().as_secs_f64() * 1000.0, n);
    emu.debugger().set_tracing_enabled(MAIN_CPU, false);

    // Stepping: a frame's worth of host steps, as a debugger steps.
    emu.reset();
    println!();
    let one = step_frame(&mut emu);
    let fit = (STEPPING_TIME_LIMIT.as_secs_f64() / one.as_secs_f64().max(1e-9)) as u64;
    let stepped = n.min(fit.max(1));
    if stepped < n {
        println!(
            "Stepping {stepped} frames with host steps ({n} would take more than {} seconds)...",
            STEPPING_TIME_LIMIT.as_secs()
        );
    } else {
        println!("Stepping {stepped} frames with host steps...");
    }
    let start = Instant::now();
    for _ in 0..stepped {
        step_frame(&mut emu);
    }
    let ms_stepping = report(start.elapsed().as_secs_f64() * 1000.0, stepped);

    println!();
    println!("=== Summary ===");
    println!("  Play tier:             {:.1} FPS", 1000.0 / ms_play);
    println!(
        "  Light debug (BP):      {:.1} FPS ({:.2}x vs play)",
        1000.0 / ms_light,
        ms_light / ms_play
    );
    println!(
        "  Light debug (trace):   {:.1} FPS ({:.2}x vs play)",
        1000.0 / ms_light_trace,
        ms_light_trace / ms_play
    );
    println!(
        "  Stepping (host steps): {:.1} FPS ({:.2}x vs play, {stepped} frames)",
        1000.0 / ms_stepping,
        ms_stepping / ms_play
    );
    println!();
    println!(
        "  Light debug (BP) vs stepping speedup: {:.2}x",
        ms_stepping / ms_light
    );
    Ok(())
}

/// Host steps of the 68000 until the frame in progress ends; how long they
/// took.
fn step_frame(emu: &mut GenesisEmulator) -> Duration {
    let target = emu.frame_count() + 1;
    let start = Instant::now();
    while emu.frame_count() < target {
        emu.step_cpu(MAIN_CPU);
    }
    start.elapsed()
}

/// Print per-tier timing and return the ms/frame for ratio reporting.
fn report(total_ms: f64, frames: u64) -> f64 {
    let ms_per_frame = total_ms / frames as f64;
    println!("  Total time:   {total_ms:.2} ms");
    println!("  Avg ms/frame: {ms_per_frame:.3} ms");
    println!("  Effective FPS: {:.1}", 1000.0 / ms_per_frame);
    println!("  Target: 16.67 ms/frame (60 FPS)");
    if ms_per_frame > 16.67 {
        println!(
            "  Status: TOO SLOW ({:.1}x over budget)",
            ms_per_frame / 16.67
        );
    } else {
        println!(
            "  Status: FAST ENOUGH ({:.1}x under budget)",
            16.67 / ms_per_frame
        );
    }
    ms_per_frame
}
