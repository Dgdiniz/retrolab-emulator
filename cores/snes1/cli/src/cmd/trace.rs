//! `trace`: dump an execution trace in the exact bsnes-plus text
//! format, for either the 65C816 (`--cpu snes`) or the SPC700
//! (`--cpu spc700`).

use std::io::Write;
use std::ops::ControlFlow;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};
use retro_core::{EmulatorCore, InputState};
use snes1_core::SnesEmulator;

use crate::trace_driver::{drive_trace, RunLimit};
use crate::trace_ref::TraceCpu;

#[derive(Args, Debug)]
pub struct TraceArgs {
    /// Path to the ROM file (`.sfc` / `.smc`).
    pub rom: PathBuf,
    /// Which CPU to trace: `snes` (65C816), `spc700`, `gsu` (Super FX),
    /// or `cx4` (Cx4 HG51B169).
    #[arg(long, default_value = "snes")]
    pub cpu: String,
    /// Trace for N frames (default: 1 frame).
    #[arg(long)]
    pub frames: Option<u64>,
    /// Trace N instructions (65C816 only).
    #[arg(long)]
    pub instructions: Option<u64>,
    /// Write the trace here instead of stdout.
    #[arg(long, short)]
    pub output: Option<PathBuf>,
    /// Show a progress bar (on stderr).
    #[arg(long)]
    pub progress: bool,
    /// Pulse these player-1 buttons (comma-separated:
    /// `start,select,a,b,x,y,l,r,up,down,left,right`) on/off every few
    /// frames while tracing, so a title/menu gated behind a *press edge*
    /// advances. Frame-bounded runs only. Diagnoses input-gated screens
    /// (e.g. a Mode 5 menu) the plain trace can't reach.
    #[arg(long)]
    pub press: Option<String>,
    /// Hold these player-1 buttons continuously every frame (same names
    /// as `--press`). Use to trace a routine while a button (e.g. a
    /// d-pad direction) is held. Takes precedence over `--press`.
    #[arg(long)]
    pub press_hold: Option<String>,
    /// Frame-scheduled player-1 input: `"F:buttons;F:buttons"` (e.g.
    /// `"180:start;186:"`). Each set-point installs that mask from frame
    /// `F` until the next set-point; an empty list releases. Reproduces
    /// the timed human presses in a bsnes-plus reference log. Takes
    /// precedence over `--press` / `--press-hold`. Frame-bounded only.
    #[arg(long)]
    pub press_at: Option<String>,
    /// Load low-confidence ROMs (blank title / bad checksum) by forcing
    /// the vector-implied mapping instead of failing the detector gate.
    #[arg(long)]
    pub force_detect: bool,
    /// DSP-1 firmware + coprocessor-mode flags.
    #[command(flatten)]
    pub coprocessor: super::CoprocessorArgs,
}

pub fn run(args: TraceArgs) -> Result<()> {
    let cpu = TraceCpu::parse(&args.cpu)?;
    if args.frames.is_some() && args.instructions.is_some() {
        anyhow::bail!("--frames and --instructions are mutually exclusive");
    }
    let limit = match (args.frames, args.instructions) {
        (Some(_), Some(_)) => unreachable!(),
        (_, Some(n)) => RunLimit::Instructions(n),
        (Some(n), _) => RunLimit::Frames(n),
        (None, None) => RunLimit::Frames(1),
    };

    let rom = super::read_rom(&args.rom)?;
    let mut emu = SnesEmulator::new();
    args.coprocessor.apply(&mut emu)?;
    super::apply_force_detect(&mut emu, &rom, args.force_detect);
    emu.load_rom(&rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;
    if let Some(chip) = emu.coprocessor_label() {
        eprintln!("coprocessor: {chip}");
    }

    let mut out = super::open_output(args.output.as_ref())?;
    let progress = make_spinner(args.progress, cpu.label());

    // Optional per-frame controller injection. `--press-at` runs a
    // frame-scheduled program (precise; reproduces a reference log's
    // timed presses); `--press-hold` holds the mask every frame;
    // `--press` pulses it (6 frames held, 6 released) so press-edge gates
    // advance. With no flag the closure leaves input untouched.
    let schedule = super::parse_press_schedule(args.press_at.as_deref())?;
    let hold_mask = super::parse_button_mask(args.press_hold.as_deref())?;
    let press_mask = super::parse_button_mask(args.press.as_deref())?;
    let pre_frame = |emu: &mut SnesEmulator, frame: u64| {
        if let Some(sched) = &schedule {
            if let Some(buttons) = sched.mask_at(frame) {
                emu.set_input(0, InputState { buttons });
            }
            return;
        }
        let buttons = if let Some(h) = hold_mask {
            h
        } else if let Some(p) = press_mask {
            if (frame / 6).is_multiple_of(2) {
                p
            } else {
                0
            }
        } else {
            return;
        };
        emu.set_input(0, InputState { buttons });
    };

    let stats = drive_trace(&mut emu, cpu, limit, progress.as_ref(), pre_frame, |line| {
        writeln!(out, "{line}")?;
        Ok(ControlFlow::Continue(()))
    })?;
    out.flush()?;

    if let Some(pb) = progress {
        pb.finish_and_clear();
    }

    // Summary goes to stderr so a redirected stdout trace stays clean.
    eprintln!(
        "Traced {} {} instructions over {} frame(s).",
        stats.instructions,
        cpu.label(),
        stats.frames
    );
    if let Some(p) = &args.output {
        eprintln!("Written to {}", p.display());
    }
    Ok(())
}

fn make_spinner(show: bool, label: &str) -> Option<ProgressBar> {
    if !show {
        return None;
    }
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template(&format!(
                "{{spinner:.green}} tracing {label}: {{pos}} instructions"
            ))
            .unwrap(),
    );
    Some(pb)
}
