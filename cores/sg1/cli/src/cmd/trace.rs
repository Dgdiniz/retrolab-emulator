//! `trace`: dump an execution trace in MAME text format.
//!
//! Runs the ROM for a bounded window while tracing the chosen CPU and
//! writes one disassembled register line per instruction. The 68000
//! format is `PC:.. SR:.. D0:.. .. A7:.. | mnemonic`; the Z80 format is
//! `PC:.. AF:.. .. HALT:.. | mnemonic`. The output is what `compare`
//! diffs against a MAME reference log offline (`compare-trace` does the
//! same diff live).

use std::io::Write;
use std::ops::ControlFlow;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};

use crate::trace_driver::{drive_trace, RunLimit, TraceRun};
use crate::trace_ref::TraceCpu;

#[derive(Args, Debug)]
pub struct TraceArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Which CPU to trace: `m68k` (68000) or `z80`.
    #[arg(long, default_value = "m68k")]
    pub cpu: String,
    /// Trace exactly N frames.
    #[arg(long, short)]
    pub frames: Option<u64>,
    /// Trace exactly N instructions (68000 only).
    #[arg(long, short)]
    pub instructions: Option<u64>,
    /// Write the trace here instead of stdout.
    #[arg(long, short)]
    pub output: Option<PathBuf>,
    /// Frame-scheduled player-1 input `"F:buttons;F:buttons"` (frame
    /// modes only, instruction stepping has no frame boundary to hook).
    #[arg(long)]
    pub press_at: Option<String>,
    /// Connect 6-button pads so `x`/`y`/`z`/`mode` reach the game.
    #[arg(long)]
    pub six_button: bool,
    /// Prepend `TC:`(cumulative cycles)/`BY:`(scanline) columns (68000
    /// only) so cycle pacing is visible per instruction.
    #[arg(long)]
    pub timing: bool,
    /// Show a progress bar.
    #[arg(long)]
    pub progress: bool,
}

pub fn run(args: TraceArgs) -> Result<()> {
    let cpu = TraceCpu::parse(&args.cpu)?;
    if args.frames.is_some() && args.instructions.is_some() {
        anyhow::bail!("--frames and --instructions are mutually exclusive");
    }
    let limit = match (args.frames, args.instructions) {
        (_, Some(n)) => RunLimit::Instructions(n),
        (Some(n), _) => RunLimit::Frames(n),
        (None, None) => anyhow::bail!("specify --frames or --instructions"),
    };

    let (_, mut emu) = super::load_rom(&args.rom)?;
    if args.six_button {
        super::connect_controllers(&mut emu, true);
    }

    let schedule = super::parse_press_schedule(args.press_at.as_deref())?;
    let mut out = super::open_output(args.output.as_ref())?;
    let progress = make_bar(args.progress, limit);

    let stats = drive_trace(
        &mut emu,
        TraceRun {
            cpu,
            limit,
            warmup: 0,
            with_timing: args.timing,
        },
        progress.as_ref(),
        |emu, frame| super::apply_press(emu, schedule.as_ref(), frame),
        |line| {
            writeln!(out, "{line}")?;
            Ok(ControlFlow::Continue(()))
        },
    )?;

    if let Some(pb) = progress {
        pb.finish_and_clear();
    }
    out.flush()?;
    eprintln!(
        "Traced {} instruction(s) over {} frame(s).",
        stats.instructions, stats.frames
    );
    Ok(())
}

fn make_bar(show: bool, limit: RunLimit) -> Option<ProgressBar> {
    if !show {
        return None;
    }
    let (len, unit) = match limit {
        RunLimit::Frames(n) => (n, "frames"),
        RunLimit::Instructions(n) => (n, "instructions"),
        RunLimit::Unbounded => (0, "instructions"),
    };
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
