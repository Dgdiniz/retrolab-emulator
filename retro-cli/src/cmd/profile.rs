//! `profile`: where a frame's time goes. Warms up 60 frames, then averages
//! the frame driver's profiles (`retro_core::frame::run_frame_profiled`:
//! the play tier with each part of the machine the core names timed apart)
//! over the frames asked for, by the console's report lines, and ranks the
//! top bottlenecks against the 16.67 ms (60 FPS) budget.

use anyhow::Result;
use retro_core::frame::{run_frame_on, run_frame_profiled, FrameProfile};
use retro_core::ExecutionTier;

use crate::console::{Console, ProfileLine};
use crate::input::InputArgs;
use crate::rom::{load, write_banner, RomArgs};

/// The frames run before the profiled ones, so the game is past its boot.
pub const WARMUP_FRAMES: u64 = 60;

#[derive(clap::Args, Debug)]
pub struct ProfileArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Number of frames to profile.
    #[arg(long, short)]
    pub frames: Option<u64>,
    #[command(flatten)]
    pub input: InputArgs,
}

/// Profiles summed (or averaged) over frames: the frame's time, each
/// report line's time and the units of work its part of the machine
/// counted, and the machine's clock.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ProfileSums {
    /// The frames' time, in nanoseconds.
    pub total_ns: f64,
    /// Each report line's time, in nanoseconds.
    pub lines: Vec<f64>,
    /// The units of work each report line's part of the machine counted
    /// (a CPU's instructions).
    pub units: Vec<u64>,
    /// The machine's clock (`FrameProfile::elapsed`).
    pub clock: u64,
}

impl ProfileSums {
    /// The sums of `profiles` by the report's `lines`, each part of the
    /// machine found by the name the core gives it.
    pub fn of(profiles: &[FrameProfile], lines: &[ProfileLine]) -> Self {
        let mut sums = Self {
            lines: vec![0.0; lines.len()],
            units: vec![0; lines.len()],
            ..Self::default()
        };
        for profile in profiles {
            sums.total_ns += profile.total_ns as f64;
            sums.clock += profile.elapsed;
            for chip in profile.chips() {
                if let Some(line) = lines.iter().position(|line| line.chip == chip.chip) {
                    sums.lines[line] += chip.ns as f64;
                    sums.units[line] += chip.units;
                }
            }
        }
        sums
    }

    /// The average frame of sums over `frames` frames (the counts cut to
    /// whole units).
    pub fn average(&self, frames: usize) -> Self {
        let n = frames.max(1) as f64;
        Self {
            total_ns: self.total_ns / n,
            lines: self.lines.iter().map(|ns| ns / n).collect(),
            units: self.units.iter().map(|&u| (u as f64 / n) as u64).collect(),
            clock: (self.clock as f64 / n) as u64,
        }
    }

    /// The time no report line names.
    fn other_ns(&self) -> f64 {
        self.total_ns - self.lines.iter().sum::<f64>()
    }
}

/// What the report is made of besides the sums: the console's lines, its
/// CPUs' labels (the main CPU's first) and what its clock counts.
#[derive(Clone, Copy, Debug)]
pub struct ReportShape<'a> {
    /// The report's lines ([`crate::Console::PROFILE`]).
    pub lines: &'a [ProfileLine],
    /// The labels of the console's CPUs, the main CPU's first.
    pub cpus: &'a [&'a str],
    /// What the machine's clock counts ([`crate::Console::PROFILE_CLOCK`]).
    pub clock: &'a str,
}

impl ReportShape<'_> {
    /// The lines whose part of the machine is a CPU, which count their
    /// instructions: their places in `lines`, the main CPU's first.
    fn counting(&self) -> Vec<usize> {
        let main = self.cpus.first().copied().unwrap_or_default();
        let mut counting: Vec<usize> = (0..self.lines.len())
            .filter(|&i| self.cpus.contains(&self.lines[i].chip))
            .collect();
        counting.sort_by_key(|&i| self.lines[i].chip != main);
        counting
    }

    fn is_main(&self, line: &ProfileLine) -> bool {
        self.cpus.first() == Some(&line.chip)
    }
}

/// The name of a report line in the totals: its label without ` CPU`.
fn short_name(line: &ProfileLine) -> &'static str {
    line.label.strip_suffix(" CPU").unwrap_or(line.label)
}

/// `clock` with its first letter in upper case, as a label starts.
fn capitalized(clock: &str) -> String {
    let mut chars = clock.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// The report of one average frame, under `label`.
pub fn frame_report(label: &str, shape: &ReportShape<'_>, avg: &ProfileSums) -> Vec<String> {
    let counting = shape.counting();
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
    for (i, line) in shape.lines.iter().enumerate() {
        let us = avg.lines[i] / 1000.0;
        let head = format!(
            "  {:<13}{us:>10.1} us  ({:>5.1}%)",
            format!("{}:", line.label),
            pct(us)
        );
        out.push(if !counting.contains(&i) {
            head
        } else if shape.is_main(line) {
            format!(
                "{head}  [{} instr, {} {}]",
                avg.units[i], avg.clock, shape.clock
            )
        } else {
            format!("{head}  [{} instr]", avg.units[i])
        });
    }
    let other_us = avg.other_ns() / 1000.0;
    out.push(format!(
        "  Other/overhead: {other_us:>7.1} us  ({:>5.1}%)",
        pct(other_us)
    ));
    out
}

/// The totals over the frames: the time, each counting line's instructions
/// (the clock after the main CPU's), then each one's average a frame.
pub fn totals_report(
    shape: &ReportShape<'_>,
    totals: &ProfileSums,
    avg: &ProfileSums,
) -> Vec<String> {
    let counting = shape.counting();
    let row = |label: String, value: String| format!("  {label:<20} {value}");
    let mut out = vec![row(
        "Total time:".to_string(),
        format!("{:.2} ms", totals.total_ns / 1_000_000.0),
    )];
    for &i in &counting {
        let line = &shape.lines[i];
        out.push(row(
            format!("{} instructions:", short_name(line)),
            totals.units[i].to_string(),
        ));
        if shape.is_main(line) {
            out.push(row(
                format!("{}:", capitalized(shape.clock)),
                totals.clock.to_string(),
            ));
        }
    }
    for &i in &counting {
        out.push(row(
            format!("Avg {} instr/frame:", short_name(&shape.lines[i])),
            avg.units[i].to_string(),
        ));
    }
    out
}

pub fn run<C: Console>(args: ProfileArgs<C::BootArgs>) -> Result<()> {
    let Some(frames) = args.frames else {
        anyhow::bail!("specify --frames");
    };
    let input = args.input.plan::<C::Emulator>()?;
    let (rom, mut emu) = load::<C>(&args.rom)?;

    println!(
        "=== {} Performance Profile ===",
        C::TOOL
            .strip_suffix("-cli")
            .unwrap_or(C::TOOL)
            .to_ascii_uppercase()
    );
    println!("ROM: {}", args.rom.path.display());
    println!("ROM size: {} bytes", rom.len());
    write_banner(&mut emu, &mut std::io::stdout())?;
    println!();

    println!("Warming up ({WARMUP_FRAMES} frames)...");
    for frame in 0..WARMUP_FRAMES {
        input.before_frame(&mut emu, frame);
        run_frame_on(&mut emu, ExecutionTier::Play);
    }

    println!("Profiling {frames} frames...");
    let profiles: Vec<FrameProfile> = (0..frames)
        .map(|frame| {
            input.before_frame(&mut emu, WARMUP_FRAMES + frame);
            run_frame_profiled(&mut emu)
        })
        .collect();
    let period = profiles.first().map_or(1, |p| p.sample_period);
    println!(
        "Inside the {}'s steps each chip's work is timed one time in {period} and scaled by \
         {period}; a line's and a frame's work is timed every time.",
        C::CPUS[0].label
    );
    println!();

    let cpus: Vec<&str> = C::CPUS.iter().map(|cpu| cpu.label).collect();
    let shape = ReportShape {
        lines: C::PROFILE,
        cpus: &cpus,
        clock: C::PROFILE_CLOCK,
    };
    let totals = ProfileSums::of(&profiles, C::PROFILE);
    let avg = totals.average(profiles.len());
    for line in frame_report("AVERAGE per frame:", &shape, &avg) {
        println!("{line}");
    }

    let min_frame_ns = profiles.iter().map(|p| p.total_ns).min().unwrap_or(0);
    let max_frame_ns = profiles.iter().map(|p| p.total_ns).max().unwrap_or(0);
    let n = frames.max(1) as f64;
    println!();
    println!(
        "  Frame time range: {:.1} us - {:.1} us",
        min_frame_ns as f64 / 1000.0,
        max_frame_ns as f64 / 1000.0
    );
    println!(
        "  Effective FPS: {:.1}",
        1_000_000_000.0 / (totals.total_ns / n)
    );
    println!("  Target: 16667 us/frame (60 FPS)");

    println!();
    println!("=== Totals over {frames} frames ===");
    for line in totals_report(&shape, &totals, &avg) {
        println!("{line}");
    }

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

    let mut ranked: Vec<(&str, f64)> = C::PROFILE
        .iter()
        .zip(&avg.lines)
        .map(|(line, ns)| (line.label, *ns))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    println!();
    println!("  Top bottlenecks:");
    for (i, (name, ns)) in ranked.iter().take(3).enumerate() {
        let us = ns / 1000.0;
        let pct = us / total_us * 100.0;
        println!("    {}. {} = {:.1} us ({:.1}%)", i + 1, name, us, pct);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Mega Drive tool's lines, as the report had them.
    const LINES: [ProfileLine; 7] = [
        ProfileLine {
            label: "68K CPU",
            chip: "68000",
        },
        ProfileLine {
            label: "Z80 CPU",
            chip: "Z80",
        },
        ProfileLine {
            label: "VDP render",
            chip: "VDP",
        },
        ProfileLine {
            label: "YM2612",
            chip: "YM2612",
        },
        ProfileLine {
            label: "PSG",
            chip: "PSG",
        },
        ProfileLine {
            label: "Audio mix",
            chip: "audio mix",
        },
        ProfileLine {
            label: "Framebuffer",
            chip: "framebuffer",
        },
    ];

    fn shape() -> ReportShape<'static> {
        ReportShape {
            lines: &LINES,
            cpus: &["68000", "Z80"],
            clock: "68K cycles",
        }
    }

    fn avg() -> ProfileSums {
        ProfileSums {
            total_ns: 10_000.0,
            lines: vec![4000.0, 2000.0, 1000.0, 500.0, 500.0, 500.0, 500.0],
            units: vec![14, 7, 0, 0, 0, 0, 0],
            clock: 128,
        }
    }

    #[test]
    fn the_report_keeps_its_lines_and_puts_the_rest_in_other() {
        let lines = frame_report("AVERAGE per frame:", &shape(), &avg());
        assert_eq!(lines[0], "  AVERAGE per frame:");
        assert!(lines[3].starts_with("  68K CPU:"));
        assert!(
            lines[3].ends_with("[14 instr, 128 68K cycles]"),
            "{}",
            lines[3]
        );
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
        assert!(
            lines[5].ends_with("%)"),
            "a line that counts nothing has no note"
        );
        assert!(lines[10].starts_with("  Other/overhead:"));
        assert!(lines[10].contains("1.0 us"), "{}", lines[10]);
    }

    #[test]
    fn the_totals_name_each_cpu_and_the_clock_after_the_main_one() {
        let totals = ProfileSums {
            total_ns: 67_070_000.0,
            units: vec![688_408, 483_851, 0, 0, 0, 0, 0],
            clock: 7_680_345,
            ..avg()
        };
        assert_eq!(
            totals_report(&shape(), &totals, &avg()),
            [
                "  Total time:          67.07 ms",
                "  68K instructions:    688408",
                "  68K cycles:          7680345",
                "  Z80 instructions:    483851",
                "  Avg 68K instr/frame: 14",
                "  Avg Z80 instr/frame: 7",
            ]
        );
    }

    #[test]
    fn the_average_cuts_the_counts_to_whole_units() {
        let sums = ProfileSums {
            total_ns: 3.0,
            lines: vec![3.0],
            units: vec![7],
            clock: 5,
        };
        let avg = sums.average(2);
        assert_eq!(
            (avg.total_ns, avg.lines[0], avg.units[0], avg.clock),
            (1.5, 1.5, 3, 2)
        );
        assert_eq!(ProfileSums::default().average(0).total_ns, 0.0);
    }
}
