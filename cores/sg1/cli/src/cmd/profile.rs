//! `profile`: per-subsystem timing breakdown. Warms up 60 frames, then
//! averages `run_frame_profiled()`'s nanosecond counters (68000, Z80,
//! VDP, YM2612, PSG, audio mix, framebuffer) and ranks the top
//! bottlenecks against the 16.67 ms/frame (60 FPS) budget.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use retro_core::{DebuggerCore, ExecutionMode};
use sg1_core::system::FrameProfile;

#[derive(Args, Debug)]
pub struct ProfileArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Number of frames to profile (default: 300 = 5 seconds).
    #[arg(short, long, default_value_t = 300)]
    pub frames: u64,
}

pub fn run(args: ProfileArgs) -> Result<()> {
    let (rom, mut emu) = super::load_rom(&args.rom)?;
    let num_frames = args.frames;

    println!("=== SG1 Performance Profile ===");
    println!("ROM: {}", args.rom.display());
    println!("ROM size: {} bytes", rom.len());
    super::print_rom_title(&emu);

    emu.debugger().set_execution_mode(ExecutionMode::Running);
    println!();

    println!("Warming up (60 frames)...");
    for _ in 0..60 {
        emu.run_frame_fast();
    }

    println!("Profiling {num_frames} frames...");
    println!();

    let mut totals = FrameProfile::default();
    let mut min_frame_ns = u64::MAX;
    let mut max_frame_ns = 0u64;

    for _ in 0..num_frames {
        let profile = emu.run_frame_profiled();
        totals.total_ns += profile.total_ns;
        totals.m68k_ns += profile.m68k_ns;
        totals.z80_ns += profile.z80_ns;
        totals.vdp_ns += profile.vdp_ns;
        totals.ym2612_ns += profile.ym2612_ns;
        totals.psg_ns += profile.psg_ns;
        totals.audio_mix_ns += profile.audio_mix_ns;
        totals.framebuffer_ns += profile.framebuffer_ns;
        totals.m68k_instructions += profile.m68k_instructions;
        totals.m68k_cycles += profile.m68k_cycles;
        totals.z80_instructions += profile.z80_instructions;
        min_frame_ns = min_frame_ns.min(profile.total_ns);
        max_frame_ns = max_frame_ns.max(profile.total_ns);
    }

    let n = num_frames.max(1) as f64;
    let avg = FrameProfile {
        total_ns: (totals.total_ns as f64 / n) as u64,
        m68k_ns: (totals.m68k_ns as f64 / n) as u64,
        z80_ns: (totals.z80_ns as f64 / n) as u64,
        vdp_ns: (totals.vdp_ns as f64 / n) as u64,
        ym2612_ns: (totals.ym2612_ns as f64 / n) as u64,
        psg_ns: (totals.psg_ns as f64 / n) as u64,
        audio_mix_ns: (totals.audio_mix_ns as f64 / n) as u64,
        framebuffer_ns: (totals.framebuffer_ns as f64 / n) as u64,
        m68k_instructions: (totals.m68k_instructions as f64 / n) as u64,
        m68k_cycles: (totals.m68k_cycles as f64 / n) as u64,
        z80_instructions: (totals.z80_instructions as f64 / n) as u64,
    };

    avg.print_summary("AVERAGE per frame:");

    println!();
    println!(
        "  Frame time range: {:.1} us - {:.1} us",
        min_frame_ns as f64 / 1000.0,
        max_frame_ns as f64 / 1000.0
    );
    println!(
        "  Effective FPS: {:.1}",
        1_000_000_000.0 / (totals.total_ns as f64 / n)
    );
    println!("  Target: 16667 us/frame (60 FPS)");

    println!();
    println!("=== Totals over {num_frames} frames ===");
    println!(
        "  Total time:          {:.2} ms",
        totals.total_ns as f64 / 1_000_000.0
    );
    println!("  68K instructions:    {}", totals.m68k_instructions);
    println!("  68K cycles:          {}", totals.m68k_cycles);
    println!("  Z80 instructions:    {}", totals.z80_instructions);
    println!("  Avg 68K instr/frame: {}", avg.m68k_instructions);
    println!("  Avg Z80 instr/frame: {}", avg.z80_instructions);

    println!();
    println!("=== Analysis ===");
    let total_us = avg.total_ns as f64 / 1000.0;
    let budget_us = 16667.0;
    if total_us > budget_us {
        println!(
            "  OVER BUDGET by {:.1} us ({:.1}%)",
            total_us - budget_us,
            (total_us - budget_us) / budget_us * 100.0
        );
    } else {
        println!(
            "  UNDER BUDGET by {:.1} us ({:.1}% headroom)",
            budget_us - total_us,
            (budget_us - total_us) / budget_us * 100.0
        );
    }

    let subsystems = [
        ("68K CPU", avg.m68k_ns),
        ("Z80 CPU", avg.z80_ns),
        ("VDP render", avg.vdp_ns),
        ("YM2612", avg.ym2612_ns),
        ("PSG", avg.psg_ns),
        ("Audio mix", avg.audio_mix_ns),
        ("Framebuffer", avg.framebuffer_ns),
    ];
    let mut sorted: Vec<_> = subsystems.iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.1));

    println!();
    println!("  Top bottlenecks:");
    for (i, (name, ns)) in sorted.iter().take(3).enumerate() {
        let us = *ns as f64 / 1000.0;
        let pct = us / total_us * 100.0;
        println!("    {}. {} = {:.1} us ({:.1}%)", i + 1, name, us, pct);
    }
    Ok(())
}
