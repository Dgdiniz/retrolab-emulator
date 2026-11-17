//! `profile`: per-subsystem timing breakdown. Warms up 60 frames, then
//! averages the frame driver's profiles (`retro_core::frame::
//! run_frame_profiled`: the play tier with each part of the machine the
//! core names timed apart) over the frames asked for, and ranks the top
//! bottlenecks against the 16.67 ms/frame (60 FPS) budget.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use retro_core::frame::{run_frame_on, run_frame_profiled, FrameProfile};
use retro_core::{DebuggerCore, ExecutionMode, ExecutionTier};

#[derive(Args, Debug)]
pub struct ProfileArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Number of frames to profile (default: 300 = 5 seconds).
    #[arg(short, long, default_value_t = 300)]
    pub frames: u64,
}

/// The report's lines, each with the part of the machine the core names
/// for it. Work the core names otherwise (its cartridge's chips) counts in
/// "Other/overhead", with the driver's own.
const LINES: [(&str, &str); 7] = [
    ("68K CPU", "68000"),
    ("Z80 CPU", "Z80"),
    ("VDP render", "VDP"),
    ("YM2612", "YM2612"),
    ("PSG", "PSG"),
    ("Audio mix", "audio mix"),
    ("Framebuffer", "framebuffer"),
];

/// Profiles averaged over the frames: the frame's time, each report line's
/// time, and the counts.
#[derive(Debug, Default, Clone, PartialEq)]
struct Average {
    total_ns: f64,
    lines: [f64; LINES.len()],
    m68k_instructions: u64,
    m68k_cycles: u64,
    z80_instructions: u64,
}

impl Average {
    /// The time no report line names.
    fn other_ns(&self) -> f64 {
        self.total_ns - self.lines.iter().sum::<f64>()
    }
}

/// Fold `profiles` into their average, each part of the machine found by
/// the name the core gives it.
fn average(profiles: &[FrameProfile]) -> Average {
    let n = profiles.len().max(1) as f64;
    let mut sum = Average::default();
    let mut m68k_instructions = 0u64;
    let mut m68k_cycles = 0u64;
    let mut z80_instructions = 0u64;
    for profile in profiles {
        sum.total_ns += profile.total_ns as f64;
        m68k_cycles += profile.elapsed;
        for chip in profile.chips() {
            if let Some(line) = LINES.iter().position(|(_, name)| *name == chip.chip) {
                sum.lines[line] += chip.ns as f64;
            }
            match chip.chip {
                "68000" => m68k_instructions += chip.units,
                "Z80" => z80_instructions += chip.units,
                _ => {}
            }
        }
    }
    Average {
        total_ns: sum.total_ns / n,
        lines: sum.lines.map(|ns| ns / n),
        m68k_instructions: (m68k_instructions as f64 / n) as u64,
        m68k_cycles: (m68k_cycles as f64 / n) as u64,
        z80_instructions: (z80_instructions as f64 / n) as u64,
    }
}

/// The report of one average frame, under `label`.
fn summary(label: &str, avg: &Average) -> Vec<String> {
    let total_us = avg.total_ns / 1000.0;
    let pct = |us: f64| -> f64 {
        if total_us > 0.0 {
            us / total_us * 100.0
        } else {
            0.0
        }
    };
    let mut out = vec![
        format!("  {label}"),
        format!(
            "  Total:       {total_us:>10.1} us ({:.2} ms)",
            total_us / 1000.0
        ),
        "  -------------------------------------------------------".to_string(),
    ];
    for (i, (name, _)) in LINES.iter().enumerate() {
        let us = avg.lines[i] / 1000.0;
        let head = format!(
            "  {:<13}{us:>10.1} us  ({:>5.1}%)",
            format!("{name}:"),
            pct(us)
        );
        out.push(match i {
            0 => format!(
                "{head}  [{} instr, {} cycles]",
                avg.m68k_instructions, avg.m68k_cycles
            ),
            1 => format!("{head}  [{} instr]", avg.z80_instructions),
            _ => head,
        });
    }
    let other_us = avg.other_ns() / 1000.0;
    out.push(format!(
        "  Other/overhead: {other_us:>7.1} us  ({:>5.1}%)",
        pct(other_us)
    ));
    out
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
        run_frame_on(&mut emu, ExecutionTier::Play);
    }

    println!("Profiling {num_frames} frames...");
    let profiles: Vec<FrameProfile> = (0..num_frames)
        .map(|_| run_frame_profiled(&mut emu))
        .collect();
    let period = profiles.first().map_or(1, |p| p.sample_period);
    println!(
        "Inside the 68000's steps each chip's work is timed one time in {period} and \
         scaled by {period}; a line's and a frame's work is timed every time."
    );
    println!();

    let avg = average(&profiles);
    for line in summary("AVERAGE per frame:", &avg) {
        println!("{line}");
    }

    let total_ns: u64 = profiles.iter().map(|p| p.total_ns).sum();
    let min_frame_ns = profiles.iter().map(|p| p.total_ns).min().unwrap_or(0);
    let max_frame_ns = profiles.iter().map(|p| p.total_ns).max().unwrap_or(0);
    let n = num_frames.max(1) as f64;
    println!();
    println!(
        "  Frame time range: {:.1} us - {:.1} us",
        min_frame_ns as f64 / 1000.0,
        max_frame_ns as f64 / 1000.0
    );
    println!(
        "  Effective FPS: {:.1}",
        1_000_000_000.0 / (total_ns as f64 / n)
    );
    println!("  Target: 16667 us/frame (60 FPS)");

    let m68k_instructions: u64 = profiles
        .iter()
        .flat_map(|p| p.chips())
        .filter(|c| c.chip == "68000")
        .map(|c| c.units)
        .sum();
    let z80_instructions: u64 = profiles
        .iter()
        .flat_map(|p| p.chips())
        .filter(|c| c.chip == "Z80")
        .map(|c| c.units)
        .sum();
    let m68k_cycles: u64 = profiles.iter().map(|p| p.elapsed).sum();
    println!();
    println!("=== Totals over {num_frames} frames ===");
    println!(
        "  Total time:          {:.2} ms",
        total_ns as f64 / 1_000_000.0
    );
    println!("  68K instructions:    {m68k_instructions}");
    println!("  68K cycles:          {m68k_cycles}");
    println!("  Z80 instructions:    {z80_instructions}");
    println!("  Avg 68K instr/frame: {}", avg.m68k_instructions);
    println!("  Avg Z80 instr/frame: {}", avg.z80_instructions);

    println!();
    println!("=== Analysis ===");
    let total_us = avg.total_ns / 1000.0;
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

    let mut sorted: Vec<(&str, f64)> = LINES
        .iter()
        .enumerate()
        .map(|(i, (name, _))| (*name, avg.lines[i]))
        .collect();
    sorted.sort_by(|a, b| b.1.total_cmp(&a.1));

    println!();
    println!("  Top bottlenecks:");
    for (i, (name, ns)) in sorted.iter().take(3).enumerate() {
        let us = ns / 1000.0;
        let pct = us / total_us * 100.0;
        println!("    {}. {} = {:.1} us ({:.1}%)", i + 1, name, us, pct);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::frame::{Profiled, StepHooks};
    use sg1_core::GenesisEmulator;

    /// A profile of `spans`, made the way the profiler makes one: through
    /// the hooks a core calls around its parts' work.
    fn profile_of(spans: &[(&'static str, u32)]) -> FrameProfile {
        let mut profiler = Profiled::with_sample_period(1);
        for (chip, units) in spans {
            <Profiled as StepHooks<GenesisEmulator>>::chip_start(&mut profiler, chip);
            <Profiled as StepHooks<GenesisEmulator>>::chip_finish(&mut profiler, *units);
        }
        profiler.into_profile()
    }

    #[test]
    fn each_line_is_found_by_the_name_the_core_gives_it() {
        let a = profile_of(&[("68000", 3), ("Z80", 2), ("PSG", 0), ("68000", 1)]);
        let b = profile_of(&[("Z80", 4), ("68000", 2), ("cartridge", 0)]);
        let avg = average(&[a, b]);
        assert_eq!(avg.m68k_instructions, 3, "(3 + 1 + 2) / 2");
        assert_eq!(avg.z80_instructions, 3, "(2 + 4) / 2");
    }

    #[test]
    fn the_report_keeps_its_lines_and_puts_the_rest_in_other() {
        let avg = Average {
            total_ns: 10_000.0,
            lines: [4000.0, 2000.0, 1000.0, 500.0, 500.0, 500.0, 500.0],
            m68k_instructions: 14,
            m68k_cycles: 128,
            z80_instructions: 7,
        };
        let lines = summary("AVERAGE per frame:", &avg);
        assert_eq!(lines[0], "  AVERAGE per frame:");
        assert!(lines[3].starts_with("  68K CPU:"));
        assert!(lines[3].ends_with("[14 instr, 128 cycles]"));
        assert!(lines[4].starts_with("  Z80 CPU:") && lines[4].ends_with("[7 instr]"));
        let labels: Vec<&str> = lines[3..10]
            .iter()
            .map(|l| l.trim_start().split(':').next().unwrap())
            .collect();
        assert_eq!(
            labels,
            [
                "68K CPU",
                "Z80 CPU",
                "VDP render",
                "YM2612",
                "PSG",
                "Audio mix",
                "Framebuffer"
            ]
        );
        assert!(lines[10].starts_with("  Other/overhead:"));
        assert!(lines[10].contains("1.0 us"), "{}", lines[10]);
    }
}
