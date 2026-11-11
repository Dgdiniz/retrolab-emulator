//! Shared "enable tracing, run, drain, format" loop used by both the
//! `trace` dump and the `compare-trace` differ.
//!
//! The core records trace entries into a per-CPU binary ring; this
//! module enables tracing on the requested CPU, advances the emulator,
//! drains the ring after each step/frame, formats every entry into a
//! MAME-style text line, and feeds it to a caller-supplied callback. The
//! callback writes (for `trace`) or compares (for `compare-trace`) and
//! may request an early stop.
//!
//! The 68000 line is disassembled live (`disassemble_at` re-reads the
//! current instruction memory), so formatting borrows the emulator. The
//! Z80 line disassembles from the raw bytes captured *at trace time*
//! (`TraceEntryBinaryZ80::raw_bytes`), so it needs no emulator access and
//! stays correct even after Z80 RAM is overwritten.

use std::ops::ControlFlow;

use anyhow::Result;
use indicatif::ProgressBar;
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};
use sg1_core::debugger::{
    TraceEntryBinary, TraceEntryBinaryZ80, TRACE_ENTRY_SIZE, Z80_TRACE_ENTRY_SIZE,
};
use sg1_core::GenesisEmulator;

use crate::trace_ref::TraceCpu;

/// How far to run while tracing.
#[derive(Clone, Copy, Debug)]
pub enum RunLimit {
    /// Run exactly this many frames (`run_frame`).
    Frames(u64),
    /// Step exactly this many 68000 instructions (`step`). Only valid for
    /// [`TraceCpu::M68k`]; the Z80 is driven a frame at a time.
    Instructions(u64),
    /// Run frames until the callback breaks (used by `compare-trace`,
    /// which stops at reference EOF or the divergence cap). A safety cap
    /// prevents a genuinely stuck core from spinning forever.
    Unbounded,
}

/// Run accounting returned to the caller for the final summary.
#[derive(Clone, Copy, Debug, Default)]
pub struct RunStats {
    pub frames: u64,
    /// Trace lines emitted (one per executed instruction).
    pub instructions: u64,
}

/// How to run a trace: which CPU, how far, how much untraced warmup, and
/// whether to prepend the MAME timing columns. Bundled into one struct so
/// the [`drive_trace`] signature stays small.
#[derive(Clone, Copy, Debug)]
pub struct TraceRun {
    pub cpu: TraceCpu,
    pub limit: RunLimit,
    /// 68000 instructions to step (untraced) before tracing begins, so a
    /// boot/init prologue is skipped without polluting the trace.
    pub warmup: u64,
    /// Prepend the `TC`(total cycles)/`BY`(scanline) columns to each
    /// 68000 line so `compare-trace` reports show timing context; the
    /// `trace` dump leaves them off for the bare register format.
    pub with_timing: bool,
}

/// Upper bound on frames for [`RunLimit::Unbounded`] so a hung core
/// can't loop indefinitely.
const SAFETY_FRAME_CAP: u64 = 200_000;

/// Drive the emulator per `run` (see [`TraceRun`]), feeding each formatted
/// line to `on_line`. The callback returns [`ControlFlow::Break`] to stop
/// early; any error it raises propagates out via `Result`.
///
/// `pre_frame(emu, frame_index)` runs immediately before each `run_frame`
/// in the frame-bounded and unbounded modes, so callers can inject
/// per-frame controller input. It is **not** called in instruction-
/// stepped mode (`--instructions`), which has no frame boundary to hook.
pub fn drive_trace<F, P>(
    emu: &mut GenesisEmulator,
    run: TraceRun,
    progress: Option<&ProgressBar>,
    mut pre_frame: P,
    mut on_line: F,
) -> Result<RunStats>
where
    F: FnMut(&str) -> Result<ControlFlow<()>>,
    P: FnMut(&mut GenesisEmulator, u64),
{
    let TraceRun {
        cpu,
        limit,
        warmup,
        with_timing,
    } = run;
    if matches!(limit, RunLimit::Instructions(_)) && cpu == TraceCpu::Z80 {
        anyhow::bail!(
            "--instructions tracing is only supported for --cpu m68k; the Z80 is traced a frame \
             at a time, so use --frames for z80"
        );
    }

    // Warmup runs untraced so the trace starts at the comparison window.
    if warmup > 0 {
        emu.debugger().set_execution_mode(ExecutionMode::StepInto);
        let mut done = 0u64;
        while done < warmup {
            let r = emu.step();
            if r.instruction_executed {
                done += 1;
            }
        }
    }

    set_tracing(emu, cpu, true);

    let start_frames = emu.frame_count();
    let mut stats = RunStats::default();
    let mut stop = false;

    match limit {
        RunLimit::Instructions(n) => {
            emu.debugger().set_execution_mode(ExecutionMode::StepInto);
            let mut stepped = 0u64;
            while stepped < n && !stop {
                let r = emu.step();
                if !r.instruction_executed {
                    continue;
                }
                stepped += 1;
                if needs_flush(emu, cpu) {
                    stop = drain_and_feed(emu, cpu, with_timing, &mut on_line, &mut stats)?;
                }
                if let Some(pb) = progress {
                    pb.set_position(stepped);
                }
            }
        }
        RunLimit::Frames(n) => {
            emu.debugger().set_execution_mode(ExecutionMode::Running);
            for i in 0..n {
                if stop {
                    break;
                }
                pre_frame(emu, i);
                emu.run_frame();
                stop = drain_and_feed(emu, cpu, with_timing, &mut on_line, &mut stats)?;
                if let Some(pb) = progress {
                    pb.set_position(emu.frame_count() - start_frames);
                }
            }
        }
        RunLimit::Unbounded => {
            emu.debugger().set_execution_mode(ExecutionMode::Running);
            let mut frames_run = 0u64;
            while !stop && frames_run < SAFETY_FRAME_CAP {
                pre_frame(emu, frames_run);
                emu.run_frame();
                frames_run += 1;
                stop = drain_and_feed(emu, cpu, with_timing, &mut on_line, &mut stats)?;
                if let Some(pb) = progress {
                    pb.set_position(stats.instructions);
                }
            }
        }
    }

    // Final drain to flush whatever is still buffered, unless the callback
    // already asked to stop.
    if !stop {
        drain_and_feed(emu, cpu, with_timing, &mut on_line, &mut stats)?;
    }

    set_tracing(emu, cpu, false);
    stats.frames = emu.frame_count() - start_frames;
    Ok(stats)
}

fn set_tracing(emu: &mut GenesisEmulator, cpu: TraceCpu, enabled: bool) {
    match cpu {
        TraceCpu::M68k => emu.set_tracing_enabled(enabled),
        TraceCpu::Z80 => emu.set_z80_tracing_enabled(enabled),
    }
}

fn needs_flush(emu: &GenesisEmulator, cpu: TraceCpu) -> bool {
    match cpu {
        TraceCpu::M68k => emu.needs_trace_flush(),
        TraceCpu::Z80 => emu.needs_z80_trace_flush(),
    }
}

/// Drain the CPU's trace ring once and feed every formatted entry to
/// `on_line`. Returns `true` if the callback requested an early stop.
fn drain_and_feed<F>(
    emu: &mut GenesisEmulator,
    cpu: TraceCpu,
    with_timing: bool,
    on_line: &mut F,
    stats: &mut RunStats,
) -> Result<bool>
where
    F: FnMut(&str) -> Result<ControlFlow<()>>,
{
    match cpu {
        TraceCpu::M68k => {
            let data = emu.get_trace_flush_data().to_vec();
            emu.clear_trace_flushed();
            for chunk in data.as_chunks::<TRACE_ENTRY_SIZE>().0 {
                if let Some(entry) = TraceEntryBinary::from_bytes(chunk) {
                    let line = format_m68k_entry(&entry, emu, with_timing);
                    stats.instructions += 1;
                    if on_line(&line)?.is_break() {
                        return Ok(true);
                    }
                }
            }
        }
        TraceCpu::Z80 => {
            let data = emu.get_z80_trace_flush_data().to_vec();
            emu.clear_z80_trace_flushed();
            for chunk in data.as_chunks::<Z80_TRACE_ENTRY_SIZE>().0 {
                if let Some(entry) = TraceEntryBinaryZ80::from_bytes(chunk) {
                    let line = format_z80_entry(&entry);
                    stats.instructions += 1;
                    if on_line(&line)?.is_break() {
                        return Ok(true);
                    }
                }
            }
        }
    }
    Ok(false)
}

/// Format one 68000 trace entry as a MAME-style text line. With
/// `with_timing`, the `TC`(total cycles) and `BY`(beam Y / scanline)
/// columns are prepended. The instruction is disassembled live at the
/// entry's PC.
pub fn format_m68k_entry(
    entry: &TraceEntryBinary,
    emu: &mut GenesisEmulator,
    with_timing: bool,
) -> String {
    let instrs = emu.disassemble_at(entry.pc, 1);
    let mnemonic = if !instrs.is_empty() {
        format!("{} {}", instrs[0].mnemonic, instrs[0].operands)
    } else {
        format!("DC.W ${:04X}", entry.opcode)
    };
    let regs = format!(
        "PC:{:06X} SR:{:04X} D0:{:08X} D1:{:08X} D2:{:08X} D3:{:08X} D4:{:08X} D5:{:08X} D6:{:08X} D7:{:08X} A0:{:08X} A1:{:08X} A2:{:08X} A3:{:08X} A4:{:08X} A5:{:08X} A6:{:08X} A7:{:08X}",
        entry.pc,
        entry.sr,
        entry.d[0], entry.d[1], entry.d[2], entry.d[3],
        entry.d[4], entry.d[5], entry.d[6], entry.d[7],
        entry.a[0], entry.a[1], entry.a[2], entry.a[3],
        entry.a[4], entry.a[5], entry.a[6], entry.a[7],
    );
    if with_timing {
        format!(
            "TC:{:016X} BY:{:04X} {} | {}",
            entry.total_cycles, entry.scanline, regs, mnemonic
        )
    } else {
        format!("{regs} | {mnemonic}")
    }
}

/// Format one Z80 trace entry as a MAME-style text line, disassembling
/// from the raw bytes captured with the entry.
pub fn format_z80_entry(entry: &TraceEntryBinaryZ80) -> String {
    use sg1_core::z80::disasm;
    let (text, _) = disasm::disassemble_from_bytes(entry.pc, &entry.raw_bytes);
    let mnemonic = disasm::to_trace_format(&text);
    entry.to_text(&mnemonic)
}

/// Convert a raw 68000 binary trace blob to a multi-line text dump
/// (one line per entry, no timing columns), disassembling against `emu`.
pub fn convert_m68k_trace_to_text(data: &[u8], emu: &mut GenesisEmulator) -> String {
    let entry_count = data.len() / TRACE_ENTRY_SIZE;
    let mut out = String::with_capacity(entry_count * 200);
    for chunk in data.as_chunks::<TRACE_ENTRY_SIZE>().0 {
        if let Some(entry) = TraceEntryBinary::from_bytes(chunk) {
            out.push_str(&format_m68k_entry(&entry, emu, false));
            out.push('\n');
        }
    }
    out
}

/// Convert a raw Z80 binary trace blob to a multi-line text dump.
pub fn convert_z80_trace_to_text(data: &[u8]) -> String {
    let entry_count = data.len() / Z80_TRACE_ENTRY_SIZE;
    let mut out = String::with_capacity(entry_count * 120);
    for chunk in data.as_chunks::<Z80_TRACE_ENTRY_SIZE>().0 {
        if let Some(entry) = TraceEntryBinaryZ80::from_bytes(chunk) {
            out.push_str(&format_z80_entry(&entry));
            out.push('\n');
        }
    }
    out
}
