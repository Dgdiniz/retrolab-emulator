//! The trace driver: turn a CPU's trace on, run the machine, take the
//! entries the debugger holds, turn them into text and hand each line to a
//! caller, which writes it (`trace`) or compares it (`compare-trace`) and
//! may stop the run.
//!
//! - [`line`]: the parsed trace line and its differ.
//! - [`compare`]: the comparison of a trace with a reference.

pub mod compare;
pub mod line;

use std::ops::ControlFlow;

use anyhow::Result;
use indicatif::ProgressBar;
use retro_core::debug::MAIN_CPU;
use retro_core::EmulatorCore;

use crate::console::{CliCpu, Console};

/// The frames a run with no limit stops at, so that a machine which never
/// gives the caller its stop cannot run for ever.
pub const FRAME_CAP: u64 = 200_000;

/// The host steps a run counting instructions stops at, so that a main CPU
/// which stopped, or waits for an interrupt that never comes, cannot spin
/// for ever.
pub const STEP_CAP: u64 = 2_000_000_000;

/// How far a trace runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunLimit {
    /// This many frames.
    Frames(u64),
    /// This many instructions of the main CPU, a host step at a time: a
    /// step that waits, or only takes an interrupt, runs none.
    Instructions(u64),
    /// Frames until the caller stops the run, at most [`FRAME_CAP`].
    Unbounded,
}

impl RunLimit {
    /// The limit `--frames` and `--instructions` give, `None` when neither
    /// is given; both is an error.
    pub fn from_flags(frames: Option<u64>, instructions: Option<u64>) -> Result<Option<Self>> {
        match (frames, instructions) {
            (Some(_), Some(_)) => {
                anyhow::bail!("--frames and --instructions are mutually exclusive")
            }
            (Some(n), None) => Ok(Some(RunLimit::Frames(n))),
            (None, Some(n)) => Ok(Some(RunLimit::Instructions(n))),
            (None, None) => Ok(None),
        }
    }
}

/// What a trace run did.
#[derive(Clone, Copy, Debug, Default)]
pub struct RunStats {
    /// Frames run.
    pub frames: u64,
    /// Lines handed to the caller, one per instruction traced.
    pub instructions: u64,
}

/// A trace run: the CPU traced, how far, the main CPU's instructions run
/// untraced first, and whether the lines carry the timing columns a
/// console writes only on request.
#[derive(Clone, Copy, Debug)]
pub struct TraceRun<'a> {
    pub cpu: &'a CliCpu,
    pub limit: RunLimit,
    pub warmup: u64,
    pub timing: bool,
}

/// Refuse a CPU this build cannot trace, or the machine's slot does not
/// hold.
pub fn check_cpu<C: Console>(emu: &C::Emulator, cpu: &CliCpu) -> Result<()> {
    cpu.check_available()?;
    cpu.check_slot::<C>(emu)
}

/// Run `emu` as `run` says, handing every line of the traced CPU to
/// `on_line`, which returns [`ControlFlow::Break`] to stop. `before_frame`
/// runs before each frame of a frame-bounded run (the scripted input); a
/// run counting instructions has no frame boundary and calls it never.
///
/// The steps keep no instruction history, which nothing here reads. What
/// the debugger still holds when the run ends is taken too.
pub fn drive_trace<C: Console>(
    emu: &mut C::Emulator,
    run: TraceRun<'_>,
    progress: Option<&ProgressBar>,
    mut before_frame: impl FnMut(&mut C::Emulator, u64),
    mut on_line: impl FnMut(&str) -> Result<ControlFlow<()>>,
) -> Result<RunStats> {
    let TraceRun {
        cpu,
        limit,
        warmup,
        timing,
    } = run;
    check_cpu::<C>(emu, cpu)?;
    if cpu.id != MAIN_CPU && matches!(limit, RunLimit::Instructions(_)) {
        let main = &C::CPUS[0];
        anyhow::bail!(
            "--instructions tracing is only supported for --cpu {}; the {} is traced a frame at \
             a time, so use --frames for --cpu {}",
            main.name(),
            cpu.label,
            cpu.name()
        );
    }

    emu.debugger().set_history_enabled(MAIN_CPU, false);
    step_instructions::<C>(emu, warmup, |_, _| Ok(false))?;
    emu.debugger().set_tracing_enabled(cpu.id, true);

    let start_frames = emu.frame_count();
    let mut stats = RunStats::default();
    let mut stop = false;
    // The frames a frame-bounded run ran, counted as it runs them: a reset
    // the scripted input asks for starts the console's own count again.
    let mut frames_run = None;

    match limit {
        RunLimit::Instructions(n) => {
            step_instructions::<C>(emu, n, |emu, ran| {
                if emu.debugger_ref().needs_trace_flush(cpu.id) {
                    stop = drain::<C>(emu, cpu, timing, &mut on_line, &mut stats)?;
                }
                if let Some(bar) = progress {
                    bar.set_position(ran);
                }
                Ok(stop)
            })?;
        }
        RunLimit::Frames(n) => {
            let mut frame = 0u64;
            while !stop && frame < n {
                before_frame(emu, frame);
                emu.run_frame();
                frame += 1;
                stop = drain::<C>(emu, cpu, timing, &mut on_line, &mut stats)?;
                if let Some(bar) = progress {
                    bar.set_position(frame);
                }
            }
            frames_run = Some(frame);
        }
        RunLimit::Unbounded => {
            let mut frame = 0u64;
            while !stop && frame < FRAME_CAP {
                before_frame(emu, frame);
                emu.run_frame();
                frame += 1;
                stop = drain::<C>(emu, cpu, timing, &mut on_line, &mut stats)?;
                if let Some(bar) = progress {
                    bar.set_position(stats.instructions);
                }
            }
            frames_run = Some(frame);
        }
    }

    if !stop {
        drain::<C>(emu, cpu, timing, &mut on_line, &mut stats)?;
    }
    emu.debugger().set_tracing_enabled(cpu.id, false);
    stats.frames = frames_run.unwrap_or_else(|| emu.frame_count() - start_frames);
    Ok(stats)
}

/// Host-step the main CPU until it has run `count` instructions, calling
/// `after` after each one (with the count so far) until it says to stop;
/// at most [`STEP_CAP`] steps.
fn step_instructions<C: Console>(
    emu: &mut C::Emulator,
    count: u64,
    mut after: impl FnMut(&mut C::Emulator, u64) -> Result<bool>,
) -> Result<()> {
    let mut ran = 0u64;
    let mut steps = 0u64;
    while ran < count {
        let executed = emu.step_cpu(MAIN_CPU).instruction_executed;
        steps += 1;
        if executed {
            ran += 1;
            if after(emu, ran)? {
                return Ok(());
            }
        }
        if steps >= STEP_CAP && ran < count {
            anyhow::bail!(
                "{steps} steps ran only {ran} of {count} instructions: the {} stopped, or waits \
                 for an interrupt that never comes",
                C::CPUS[0].label
            );
        }
    }
    Ok(())
}

/// Take what the debugger holds of `cpu`'s trace and hand each line to
/// `on_line`; `true` when it asked to stop.
fn drain<C: Console>(
    emu: &mut C::Emulator,
    cpu: &CliCpu,
    timing: bool,
    on_line: &mut impl FnMut(&str) -> Result<ControlFlow<()>>,
    stats: &mut RunStats,
) -> Result<bool> {
    let Some(data) = emu.debugger_ref().get_trace_flush_data(cpu.id) else {
        return Ok(false);
    };
    emu.debugger().clear_trace_flushed(cpu.id);
    let text = C::trace_text(emu, cpu, &data, timing)?;
    for line in text.lines() {
        stats.instructions += 1;
        if on_line(line)?.is_break() {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limit_flags_give_one_limit_or_none() {
        assert_eq!(
            RunLimit::from_flags(Some(2), None).unwrap(),
            Some(RunLimit::Frames(2))
        );
        assert_eq!(
            RunLimit::from_flags(None, Some(9)).unwrap(),
            Some(RunLimit::Instructions(9))
        );
        assert_eq!(RunLimit::from_flags(None, None).unwrap(), None);
        let err = RunLimit::from_flags(Some(1), Some(1)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "--frames and --instructions are mutually exclusive"
        );
    }
}
