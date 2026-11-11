//! Shared "enable tracing, run, drain, format" loop used by both the
//! `trace` dump and the `compare-trace` differ.
//!
//! The core records trace entries into a per-CPU binary buffer; this
//! module enables tracing on the requested CPU, advances the emulator,
//! drains the buffer after each step/frame, formats every entry into
//! the exact bsnes-plus text line, and feeds it to a caller-supplied
//! callback. The callback writes (for `trace`) or compares (for
//! `compare-trace`) and may request an early stop.

use std::ops::ControlFlow;

use anyhow::Result;
use indicatif::ProgressBar;
use retro_core::EmulatorCore;
use snes1_core::{
    format_trace_line_65816, format_trace_line_spc700, SnesEmulator, TraceEntryBinary65816,
    TraceEntryBinarySpc700, TRACE_ENTRY_SIZE_65816, TRACE_ENTRY_SIZE_SPC700,
};
#[cfg(feature = "chip-cx4")]
use snes1_core::{format_trace_line_cx4, TraceEntryBinaryCx4, TRACE_ENTRY_SIZE_CX4};
#[cfg(feature = "chip-superfx")]
use snes1_core::{format_trace_line_gsu, TraceEntryBinaryGsu, TRACE_ENTRY_SIZE_GSU};

use crate::trace_ref::TraceCpu;

/// How far to run while tracing.
#[derive(Clone, Copy, Debug)]
pub enum RunLimit {
    /// Run exactly this many frames (`run_frame`).
    Frames(u64),
    /// Step exactly this many 65C816 instructions (`step_cpu(0)`).
    /// Only valid for [`TraceCpu::Snes`].
    Instructions(u64),
    /// Run frames until the callback breaks (used by `compare-trace`,
    /// which stops at reference EOF or the divergence cap). A safety
    /// cap prevents a genuinely stuck emulator from spinning forever.
    Unbounded,
}

/// Run accounting returned to the caller for the final summary.
#[derive(Clone, Copy, Debug, Default)]
pub struct RunStats {
    pub frames: u64,
    /// Trace lines emitted (one per executed instruction).
    pub instructions: u64,
    pub master_cycles: u64,
}

/// Upper bound on frames for [`RunLimit::Unbounded`] so a hung core
/// can't loop indefinitely. Reaching it is reported by the caller via
/// the frame count vs. the reference position.
const SAFETY_FRAME_CAP: u64 = 200_000;

/// Drive the emulator while tracing `cpu`, feeding each formatted line
/// to `on_line`. The callback returns [`ControlFlow::Break`] to stop
/// early; any I/O error it raises propagates out via `Result`.
///
/// `pre_frame(emu, frame_index)` runs immediately before each
/// `run_frame` in the frame-bounded and unbounded modes, so callers can
/// inject per-frame controller input (the `trace --press` path uses this
/// to drive a game past input-gated screens). It is **not** called in
/// instruction-stepped mode (`--instructions`), which has no frame
/// boundary to hook.
pub fn drive_trace<F, P>(
    emu: &mut SnesEmulator,
    cpu: TraceCpu,
    limit: RunLimit,
    progress: Option<&ProgressBar>,
    mut pre_frame: P,
    mut on_line: F,
) -> Result<RunStats>
where
    F: FnMut(&str) -> Result<ControlFlow<()>>,
    P: FnMut(&mut SnesEmulator, u64),
{
    let cpu_id = cpu.cpu_id();
    if matches!(limit, RunLimit::Instructions(_)) && !matches!(cpu, TraceCpu::Snes) {
        anyhow::bail!(
            "--instructions tracing is only supported for --cpu snes; the SPC700, Super FX \
             GSU and Cx4 HG51B169 only advance via catch-up, so use --frames for spc700/gsu/cx4"
        );
    }

    emu.debugger().set_tracing_enabled(cpu_id, true);

    let start_frames = emu.frame_count();
    let start_mc = emu.master_cycles();
    let mut stats = RunStats::default();
    let mut stop = false;

    match limit {
        RunLimit::Instructions(n) => {
            let mut stepped = 0u64;
            while stepped < n && !stop {
                emu.step_cpu(0);
                stepped += 1;
                if emu.debugger().needs_trace_flush() {
                    stop = drain_and_feed(emu, cpu, &mut on_line, &mut stats)?;
                }
                if let Some(pb) = progress {
                    pb.set_position(stepped);
                }
            }
        }
        RunLimit::Frames(n) => {
            for i in 0..n {
                if stop {
                    break;
                }
                pre_frame(emu, i);
                emu.run_frame();
                stop = drain_and_feed(emu, cpu, &mut on_line, &mut stats)?;
                if let Some(pb) = progress {
                    pb.set_position(emu.frame_count() - start_frames);
                }
            }
        }
        RunLimit::Unbounded => {
            let mut frames_run = 0u64;
            while !stop && frames_run < SAFETY_FRAME_CAP {
                pre_frame(emu, frames_run);
                emu.run_frame();
                frames_run += 1;
                stop = drain_and_feed(emu, cpu, &mut on_line, &mut stats)?;
                if let Some(pb) = progress {
                    pb.set_position(stats.instructions);
                }
            }
        }
    }

    // One final drain to flush whatever is still buffered, unless the
    // callback already asked to stop.
    if !stop {
        drain_and_feed(emu, cpu, &mut on_line, &mut stats)?;
    }

    emu.debugger().set_tracing_enabled(cpu_id, false);
    stats.frames = emu.frame_count() - start_frames;
    stats.master_cycles = emu.master_cycles() - start_mc;
    Ok(stats)
}

/// Drain the CPU's trace buffer once and feed every formatted entry to
/// `on_line`. Returns `true` if the callback requested an early stop.
fn drain_and_feed<F>(
    emu: &mut SnesEmulator,
    cpu: TraceCpu,
    on_line: &mut F,
    stats: &mut RunStats,
) -> Result<bool>
where
    F: FnMut(&str) -> Result<ControlFlow<()>>,
{
    let cpu_id = cpu.cpu_id();
    let data = match emu.debugger().get_trace_flush_data(cpu_id) {
        Some(d) => d,
        None => return Ok(false),
    };
    emu.debugger().clear_trace_flushed(cpu_id);
    feed_bytes(&data, cpu, on_line, stats)
}

fn feed_bytes<F>(data: &[u8], cpu: TraceCpu, on_line: &mut F, stats: &mut RunStats) -> Result<bool>
where
    F: FnMut(&str) -> Result<ControlFlow<()>>,
{
    match cpu {
        TraceCpu::Snes => {
            for chunk in data.as_chunks::<TRACE_ENTRY_SIZE_65816>().0 {
                if let Some(entry) = TraceEntryBinary65816::from_bytes(chunk) {
                    let line = format_trace_line_65816(&entry);
                    stats.instructions += 1;
                    if on_line(&line)?.is_break() {
                        return Ok(true);
                    }
                }
            }
        }
        TraceCpu::Spc700 => {
            for chunk in data.as_chunks::<TRACE_ENTRY_SIZE_SPC700>().0 {
                if let Some(entry) = TraceEntryBinarySpc700::from_bytes(chunk) {
                    let line = format_trace_line_spc700(&entry);
                    stats.instructions += 1;
                    if on_line(&line)?.is_break() {
                        return Ok(true);
                    }
                }
            }
        }
        TraceCpu::Gsu => return feed_gsu(data, on_line, stats),
        TraceCpu::Hg51b => return feed_cx4(data, on_line, stats),
    }
    Ok(false)
}

/// The Super FX GSU entries, formatted one by one.
#[cfg(feature = "chip-superfx")]
fn feed_gsu<F>(data: &[u8], on_line: &mut F, stats: &mut RunStats) -> Result<bool>
where
    F: FnMut(&str) -> Result<ControlFlow<()>>,
{
    for chunk in data.as_chunks::<TRACE_ENTRY_SIZE_GSU>().0 {
        if let Some(entry) = TraceEntryBinaryGsu::from_bytes(chunk) {
            let line = format_trace_line_gsu(&entry);
            stats.instructions += 1;
            if on_line(&line)?.is_break() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Without the chip there is no GSU trace to format.
#[cfg(not(feature = "chip-superfx"))]
fn feed_gsu<F>(_data: &[u8], _on_line: &mut F, _stats: &mut RunStats) -> Result<bool>
where
    F: FnMut(&str) -> Result<ControlFlow<()>>,
{
    anyhow::bail!(
        "--cpu gsu: this snes1-cli was built without the Super FX (feature `chip-superfx`)"
    )
}

/// The Cx4 HG51B169 entries, formatted one by one.
#[cfg(feature = "chip-cx4")]
fn feed_cx4<F>(data: &[u8], on_line: &mut F, stats: &mut RunStats) -> Result<bool>
where
    F: FnMut(&str) -> Result<ControlFlow<()>>,
{
    for chunk in data.as_chunks::<TRACE_ENTRY_SIZE_CX4>().0 {
        if let Some(entry) = TraceEntryBinaryCx4::from_bytes(chunk) {
            let line = format_trace_line_cx4(&entry);
            stats.instructions += 1;
            if on_line(&line)?.is_break() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Without the chip there is no Cx4 trace to format.
#[cfg(not(feature = "chip-cx4"))]
fn feed_cx4<F>(_data: &[u8], _on_line: &mut F, _stats: &mut RunStats) -> Result<bool>
where
    F: FnMut(&str) -> Result<ControlFlow<()>>,
{
    anyhow::bail!("--cpu cx4: this snes1-cli was built without the Cx4 (feature `chip-cx4`)")
}
