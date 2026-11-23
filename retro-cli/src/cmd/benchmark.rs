//! `benchmark`: how fast the machine runs on each of the frame driver's
//! tiers, each row from a reset with the debugger cleared:
//!
//! - **fast**: the play tier, nothing armed;
//! - **light**: the light debug tier, one execute breakpoint armed that
//!   nothing reaches;
//! - **light-trace**: the light debug tier with the main CPU traced, the
//!   trace taken whenever the debugger asks, as a host takes it;
//! - **fast-recording**: the play tier with the two live recorders armed
//!   (who wrote each byte of video memory, and each frame's writes), the
//!   writes taken after every frame as a host takes them;
//! - **host-steps**: a frame at a time in host steps of the main CPU, as a
//!   debugger steps, each with its history and the CPU states written; at
//!   most as many frames as fit in 15 seconds;
//! - **fast-sound**: the play tier with the sound on and every sound hook
//!   armed (every stem), what they record taken after every frame as a
//!   sound tool takes it.
//!
//! Each row's block, then a summary of the rows against the fast one.

use std::time::{Duration, Instant};

use anyhow::Result;
use retro_core::audio::{SoundCore, StemSet};
use retro_core::capture::{CaptureCore, CaptureRecording};
use retro_core::debug::MAIN_CPU;
use retro_core::frame::run_frame_on;
use retro_core::{Breakpoint, BreakpointType, EmulatorCore, ExecutionMode, ExecutionTier};

use crate::console::Console;
use crate::rom::{load, RomArgs};

/// The frame budget at 60 frames a second, in milliseconds.
const FRAME_BUDGET_MS: f64 = 1000.0 / 60.0;

/// The longest the host-steps row runs for: it takes as many frames as fit
/// when the frames asked for would take longer.
pub const HOST_STEPS_TIME_LIMIT: Duration = Duration::from_secs(15);

/// The rows `--mode` picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Mode {
    /// Run all five rows (default).
    All,
    /// Fast path only (no debug instrumentation).
    Fast,
    /// LightDebug via a never-hit breakpoint.
    Light,
    /// LightDebug with the main CPU's tracing active.
    LightTrace,
    /// Fast path with the two live recorders armed and their log taken
    /// every frame.
    FastRecording,
    /// Host steps of the main CPU, as a debugger steps.
    HostSteps,
    /// Fast path with the sound on and every sound hook armed (every
    /// stem), what they record taken every frame, as a sound tool takes it.
    FastSound,
}

#[derive(clap::Args, Debug)]
pub struct BenchmarkArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Number of frames to run per tier, e.g. `600` (10 s at 60 fps). The
    /// host-steps row runs fewer when they would take more than 15 seconds.
    #[arg(long, short)]
    pub frames: Option<u64>,
    /// Which tier(s) to measure.
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,
}

/// One row, measured.
struct Row {
    /// The row's name in its block.
    label: &'static str,
    /// The row's name in the summary.
    summary: &'static str,
    elapsed: Duration,
    frames: u64,
    master_cycles: u64,
}

impl Row {
    fn ms_per_frame(&self) -> f64 {
        self.elapsed.as_secs_f64() * 1000.0 / self.frames.max(1) as f64
    }

    fn fps(&self) -> f64 {
        let ms = self.ms_per_frame();
        if ms > 0.0 {
            1000.0 / ms
        } else {
            f64::INFINITY
        }
    }
}

pub fn run<C: Console>(args: BenchmarkArgs<C::BootArgs>) -> Result<()> {
    let Some(frames) = args.frames else {
        anyhow::bail!("specify --frames");
    };
    if frames == 0 {
        anyhow::bail!("--frames must be > 0");
    }
    let (_, mut emu) = load::<C>(&args.rom)?;

    println!(
        "=== {} Benchmark ===",
        C::TOOL.strip_suffix("-cli").unwrap_or(C::TOOL)
    );
    println!("ROM: {}", args.rom.path.display());
    if let Some(info) = emu.get_rom_info() {
        println!("Title: {:?} [{}]", info.title.trim(), info.region);
    }
    println!("Frames/tier: {frames}");

    let wants = |mode: Mode| args.mode == Mode::All || args.mode == mode;
    let mut rows = Vec::new();
    if wants(Mode::Fast) {
        rows.push(measure(
            &mut emu,
            "fast",
            "Play tier",
            frames,
            |_| {},
            |emu| {
                run_frame_on(emu, ExecutionTier::Play);
            },
        ));
    }
    if wants(Mode::Light) {
        let arm = |emu: &mut C::Emulator| {
            // An address no program runs at: the breakpoint never fires,
            // and its being armed is what puts the frames on the light tier.
            emu.debugger().add_breakpoint(Breakpoint {
                address: 0xFF_FFFF,
                bp_type: BreakpointType::execute(),
                cpu_id: None,
                condition: None,
            });
        };
        rows.push(measure(
            &mut emu,
            "light",
            "Light debug (BP)",
            frames,
            arm,
            |emu| {
                run_frame_on(emu, ExecutionTier::LightDebug);
            },
        ));
    }
    if wants(Mode::LightTrace) {
        let arm = |emu: &mut C::Emulator| emu.debugger().set_tracing_enabled(MAIN_CPU, true);
        let frame = |emu: &mut C::Emulator| {
            run_frame_on(emu, ExecutionTier::LightDebug);
            if emu.debugger().any_trace_needs_flush() {
                // Take the bytes and empty the ring, as a host does; the
                // bytes go through `black_box` so the copy a host pays is
                // kept.
                std::hint::black_box(emu.debugger().get_trace_flush_data(MAIN_CPU));
                emu.debugger().clear_trace_flushed(MAIN_CPU);
            }
        };
        rows.push(measure(
            &mut emu,
            "light+trace",
            "Light debug (trace)",
            frames,
            arm,
            frame,
        ));
    }
    if wants(Mode::FastRecording) {
        let arm = |emu: &mut C::Emulator| {
            emu.set_capture_recording(CaptureRecording {
                video_memory_sources: true,
                frame_writes: true,
                ..CaptureRecording::default()
            });
        };
        let frame = |emu: &mut C::Emulator| {
            run_frame_on(emu, ExecutionTier::Play);
            // A host takes each frame's writes as it goes.
            let _ = emu.take_frame_writes();
        };
        rows.push(measure(
            &mut emu,
            "fast+recording",
            "Play + recording",
            frames,
            arm,
            frame,
        ));
    }
    let mut host_steps_asked = None;
    if wants(Mode::HostSteps) {
        make_clean(&mut emu);
        // One frame first, untimed, to know how many fit in the limit.
        let one = step_frame(&mut emu);
        let fit = (HOST_STEPS_TIME_LIMIT.as_secs_f64() / one.as_secs_f64().max(1e-9)) as u64;
        let stepped = frames.min(fit.max(1));
        if stepped < frames {
            host_steps_asked = Some(frames);
        }
        let cycles_before = emu.master_cycles();
        let start = Instant::now();
        for _ in 0..stepped {
            step_frame(&mut emu);
        }
        rows.push(Row {
            label: "host-steps",
            summary: "Stepping (host steps)",
            elapsed: start.elapsed(),
            frames: stepped,
            master_cycles: emu.master_cycles().saturating_sub(cycles_before),
        });
    }

    if wants(Mode::FastSound) {
        let arm = |emu: &mut C::Emulator| {
            emu.set_audio_enabled(true);
            if let Some(layout) = emu.stem_layout() {
                emu.arm_stems(layout.all());
            }
        };
        let frame = |emu: &mut C::Emulator| {
            run_frame_on(emu, ExecutionTier::Play);
            std::hint::black_box(emu.take_stems());
            let _ = emu.get_audio_samples();
        };
        rows.push(measure(
            &mut emu,
            "fast+sound",
            "Play + sound hooks",
            frames,
            arm,
            frame,
        ));
    }

    for row in &rows {
        print_block(row, host_steps_asked.filter(|_| row.label == "host-steps"));
    }
    print_summary(&rows);
    Ok(())
}

/// A row: the machine reset and cleared, `arm` setting what the row
/// measures, then `frame` called `frames` times and timed.
fn measure<E: EmulatorCore + CaptureCore + SoundCore>(
    emu: &mut E,
    label: &'static str,
    summary: &'static str,
    frames: u64,
    arm: impl FnOnce(&mut E),
    mut frame: impl FnMut(&mut E),
) -> Row {
    make_clean(emu);
    arm(emu);
    let cycles_before = emu.master_cycles();
    let start = Instant::now();
    for _ in 0..frames {
        frame(emu);
    }
    Row {
        label,
        summary,
        elapsed: start.elapsed(),
        frames,
        master_cycles: emu.master_cycles().saturating_sub(cycles_before),
    }
}

/// Reset the machine and clear what an earlier row armed (the recorders,
/// the breakpoints, the traces), the debugger running, so each row picks
/// its tier from a clean slate.
fn make_clean<E: EmulatorCore + CaptureCore + SoundCore>(emu: &mut E) {
    emu.set_capture_recording(CaptureRecording::default());
    emu.arm_stems(StemSet::EMPTY);
    emu.reset();
    let cpus = emu.system_info().cpus.len();
    let debugger = emu.debugger();
    let ids: Vec<_> = debugger.list_breakpoints().iter().map(|b| b.id).collect();
    for id in ids {
        debugger.remove_breakpoint(id);
    }
    for cpu in 0..cpus {
        debugger.set_tracing_enabled(cpu, false);
    }
    debugger.set_execution_mode(ExecutionMode::Running);
}

/// Host steps of the main CPU until the frame in progress ends; how long
/// they took.
fn step_frame<E: EmulatorCore>(emu: &mut E) -> Duration {
    let target = emu.frame_count() + 1;
    let start = Instant::now();
    while emu.frame_count() < target {
        emu.step_cpu(MAIN_CPU);
    }
    start.elapsed()
}

/// A row's block; `asked`, for a host-steps row that ran fewer frames than
/// asked for, the frames asked for.
fn print_block(row: &Row, asked: Option<u64>) {
    let total_ms = row.elapsed.as_secs_f64() * 1000.0;
    let ms_per_frame = row.ms_per_frame();
    println!();
    println!("[{}]", row.label);
    if let Some(asked) = asked {
        println!(
            "  Frames:        {} ({asked} would take more than {} seconds)",
            row.frames,
            HOST_STEPS_TIME_LIMIT.as_secs()
        );
    }
    println!("  Total time:    {total_ms:.2} ms");
    println!("  Avg ms/frame:  {ms_per_frame:.4} ms");
    println!("  Effective FPS: {:.1}", row.fps());
    println!(
        "  Real-time:     {:.1}x (60 fps budget)",
        FRAME_BUDGET_MS / ms_per_frame
    );
    println!("  Master cycles: {}", row.master_cycles);
}

/// Each row's speed, against the fast row's when it ran; and the light
/// row's speed-up over stepping when both ran.
fn print_summary(rows: &[Row]) {
    let fast = rows.iter().find(|row| row.label == "fast");
    println!();
    println!("=== Summary ===");
    for row in rows {
        let head = format!("  {:<23}{:.1} FPS", format!("{}:", row.summary), row.fps());
        let mut notes = Vec::new();
        if let Some(fast) = fast.filter(|_| row.label != "fast") {
            notes.push(format!(
                "{:.2}x vs play",
                row.ms_per_frame() / fast.ms_per_frame()
            ));
        }
        if row.label == "host-steps" {
            notes.push(format!("{} frames", row.frames));
        }
        match notes.is_empty() {
            true => println!("{head}"),
            false => println!("{head} ({})", notes.join(", ")),
        }
    }
    let light = rows.iter().find(|row| row.label == "light");
    let stepping = rows.iter().find(|row| row.label == "host-steps");
    if let (Some(light), Some(stepping)) = (light, stepping) {
        println!();
        println!(
            "  Light debug (BP) vs stepping speedup: {:.2}x",
            stepping.ms_per_frame() / light.ms_per_frame()
        );
    }
}
