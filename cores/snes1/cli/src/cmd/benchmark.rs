//! `benchmark`: measure the three execution tiers the SNES core's
//! `run_frame()` auto-selects from debugger state.
//!
//! The SNES core has a single `run_frame()` that dispatches on
//! `debugger.execution_tier()`: clean (Running, no breakpoints, no tracing) ->
//! Fast; a breakpoint or tracing flips it to LightDebug. We therefore drive
//! each tier purely through the public `DebuggerCore` trait, no core-private
//! hooks, mirroring `sg1-cli benchmark`:
//!
//! - **fast**: clean state (Fast tier).
//! - **light**: one never-hit execute breakpoint (LightDebug tier).
//! - **light-trace**: 65C816 tracing on (LightDebug tier + trace recording);
//!   the trace buffer is drained each time it asks to flush so memory stays
//!   bounded, exactly like a host draining it per frame.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Args, ValueEnum};
use retro_core::{Breakpoint, BreakpointType, EmulatorCore, ExecutionMode};
use snes1_core::SnesEmulator;

/// Target frame budget at 60 Hz (NTSC), in milliseconds.
const FRAME_BUDGET_MS: f64 = 1000.0 / 60.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum TierArg {
    /// Run all three tiers (default).
    All,
    /// Fast path only (no debug instrumentation).
    Fast,
    /// LightDebug via a never-hit breakpoint.
    Light,
    /// LightDebug with 65C816 tracing active.
    LightTrace,
}

#[derive(Args, Debug)]
pub struct BenchmarkArgs {
    /// Path to the ROM file (`.sfc` / `.smc`).
    pub rom: PathBuf,
    /// Frames to run per tier.
    #[arg(long, default_value_t = 600)]
    pub frames: u64,
    /// Which tier(s) to measure.
    #[arg(long, value_enum, default_value_t = TierArg::All)]
    pub mode: TierArg,
}

struct BenchResult {
    label: &'static str,
    elapsed: Duration,
    frames: u64,
    master_cycles: u64,
}

pub fn run(args: BenchmarkArgs) -> Result<()> {
    if args.frames == 0 {
        anyhow::bail!("--frames must be > 0");
    }

    let rom = super::read_rom(&args.rom)?;
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;

    println!("=== snes1 Benchmark ===");
    println!("ROM: {}", args.rom.display());
    if let Some(info) = emu.get_rom_info() {
        println!("Title: {:?} [{}]", info.title.trim(), info.region);
    }
    println!("Frames/tier: {}", args.frames);

    let run_fast = matches!(args.mode, TierArg::All | TierArg::Fast);
    let run_light = matches!(args.mode, TierArg::All | TierArg::Light);
    let run_trace = matches!(args.mode, TierArg::All | TierArg::LightTrace);

    if run_fast {
        let r = bench_tier(&mut emu, "fast", args.frames, |_| {}, false);
        print_result(&r);
    }
    if run_light {
        let r = bench_tier(
            &mut emu,
            "light",
            args.frames,
            |emu| {
                // Address out of any real execute path, so it never fires,
                // its mere presence flips run_frame() to the LightDebug tier.
                emu.debugger().add_breakpoint(Breakpoint {
                    address: 0xFF_FFFF,
                    bp_type: BreakpointType::execute(),
                    cpu_id: None,
                    condition: None,
                });
            },
            false,
        );
        print_result(&r);
    }
    if run_trace {
        let r = bench_tier(
            &mut emu,
            "light+trace",
            args.frames,
            |emu| emu.debugger().set_tracing_enabled(0, true),
            true,
        );
        print_result(&r);
    }

    Ok(())
}

/// Reset to a clean Running state, apply the tier's `configure` step, then
/// time `frames` of `run_frame()`. When `drain_trace` is set, the trace
/// buffers are flushed whenever the core asks, keeping memory bounded.
fn bench_tier(
    emu: &mut SnesEmulator,
    label: &'static str,
    frames: u64,
    configure: impl FnOnce(&mut SnesEmulator),
    drain_trace: bool,
) -> BenchResult {
    make_clean(emu);
    configure(emu);

    let cycles_before = emu.master_cycles();
    let start = Instant::now();
    for _ in 0..frames {
        emu.run_frame();
        if drain_trace && emu.debugger().needs_trace_flush() {
            let _ = emu.debugger().get_trace_flush_data(0);
            let _ = emu.debugger().get_trace_flush_data(1);
        }
    }
    let elapsed = start.elapsed();
    let master_cycles = emu.master_cycles().saturating_sub(cycles_before);

    BenchResult {
        label,
        elapsed,
        frames,
        master_cycles,
    }
}

/// Drop all breakpoints + tracing and return to `Running` so the next tier
/// starts from a clean slate (and the core re-derives its tier from scratch).
fn make_clean(emu: &mut SnesEmulator) {
    emu.reset();
    let dbg = emu.debugger();
    let ids: Vec<_> = dbg.list_breakpoints().iter().map(|b| b.id).collect();
    for id in ids {
        dbg.remove_breakpoint(id);
    }
    dbg.set_tracing_enabled(0, false);
    dbg.set_tracing_enabled(1, false);
    dbg.set_execution_mode(ExecutionMode::Running);
}

fn print_result(r: &BenchResult) {
    let total_ms = r.elapsed.as_secs_f64() * 1000.0;
    let ms_per_frame = total_ms / r.frames as f64;
    let fps = if ms_per_frame > 0.0 {
        1000.0 / ms_per_frame
    } else {
        f64::INFINITY
    };
    let realtime_x = FRAME_BUDGET_MS / ms_per_frame;

    println!();
    println!("[{}]", r.label);
    println!("  Total time:    {:.2} ms", total_ms);
    println!("  Avg ms/frame:  {:.4} ms", ms_per_frame);
    println!("  Effective FPS: {:.1}", fps);
    println!("  Real-time:     {:.1}x (60 fps budget)", realtime_x);
    println!("  Master cycles: {}", r.master_cycles);
}
