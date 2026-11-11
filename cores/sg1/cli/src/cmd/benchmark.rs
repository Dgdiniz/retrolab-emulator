//! `benchmark`: measure emulation speed across the three execution
//! tiers: fast (no debug), light debug (a breakpoint active, then a trace
//! active), and full debug. Mirrors the `run_frame_fast` /
//! `run_frame_debug_light` / `run_frame` split documented in the core.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::Args;
use retro_core::types::{Breakpoint, BreakpointType};
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};

#[derive(Args, Debug)]
pub struct BenchmarkArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Number of frames to run per tier (default: 600 = 10 seconds).
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

    // Tier 1: fast path (no debug).
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    println!();
    println!("Running {n} frames with run_frame_fast() (no debug)...");
    let start = Instant::now();
    for _ in 0..n {
        emu.run_frame_fast();
    }
    let ms_fast = report(start.elapsed().as_secs_f64() * 1000.0, n);

    // Tier 2: light debug (breakpoint active, zero per-instruction allocs).
    emu.reset();
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    emu.debugger().add_breakpoint(Breakpoint {
        address: 0xFFFFFF,
        bp_type: BreakpointType::execute(),
        cpu_id: None,
        condition: None,
    });
    println!();
    println!("Running {n} frames with run_frame_debug_light() (breakpoint active)...");
    let start = Instant::now();
    for _ in 0..n {
        emu.run_frame_debug_light();
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

    // Tier 2b: light debug with trace.
    emu.reset();
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    emu.set_tracing_enabled(true);
    println!();
    println!("Running {n} frames with run_frame_debug_light() (trace active)...");
    let start = Instant::now();
    for _ in 0..n {
        emu.run_frame_debug_light();
    }
    let ms_light_trace = report(start.elapsed().as_secs_f64() * 1000.0, n);
    emu.set_tracing_enabled(false);

    // Tier 3: full debug.
    emu.reset();
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    println!();
    println!("Running {n} frames with run_frame() (full debug overhead)...");
    let start = Instant::now();
    for _ in 0..n {
        emu.run_frame();
    }
    let ms_full = report(start.elapsed().as_secs_f64() * 1000.0, n);

    println!();
    println!("=== Summary ===");
    println!("  Fast path:           {:.1} FPS", 1000.0 / ms_fast);
    println!(
        "  Light debug (BP):    {:.1} FPS ({:.2}x vs fast)",
        1000.0 / ms_light,
        ms_light / ms_fast
    );
    println!(
        "  Light debug (trace): {:.1} FPS ({:.2}x vs fast)",
        1000.0 / ms_light_trace,
        ms_light_trace / ms_fast
    );
    println!(
        "  Full debug:          {:.1} FPS ({:.2}x vs fast)",
        1000.0 / ms_full,
        ms_full / ms_fast
    );
    println!();
    println!("  Light vs Full speedup: {:.2}x", ms_full / ms_light);
    Ok(())
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
