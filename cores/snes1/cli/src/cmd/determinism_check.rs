//! `determinism-check`: long-replay determinism gate for netplay.
//!
//! Netplay lockstep/rollback rests on one invariant: given an identical
//! ROM, build, region, and per-frame input stream, the core advances
//! through byte-identical machine state every frame, and a `save_state`
//! -> `load_state` round-trip lands on that same state. The CI test
//! `cores/snes1/core/tests/determinism_test.rs` pins this on a synthetic
//! ROM; this subcommand validates it against *real* commercial titles
//! over a long recorded replay, which is where subtle nondeterminism
//! (uninitialised reads, an incomplete savestate field, an APU catch-up
//! anchor) actually surfaces.
//!
//! Two modes, both driving `run_frame_headless()` (the exact frame method
//! netplay uses) and comparing `state_hash()` every frame:
//!
//! * **two-instance** (default): two independently booted emulators fed
//!   identical inputs must share the same hash on every frame. Catches
//!   hidden nondeterminism between two fresh boots.
//! * **rollback** (`--rollback-interval N`): a reference instance runs
//!   straight through; every `N` frames a *fresh* shadow is rebuilt from
//!   the reference's savestate and must then track it exactly. Catches an
//!   incomplete savestate, the rollback-correctness gate, on a real ROM.
//!
//! On the first divergent frame it reports both hashes and which
//! serialized subsystem(s) split (via `state_hash_parts`), then exits
//! non-zero so a CI wrapper fails.

use std::path::{Path, PathBuf};

use anyhow::Result;
use retro_core::{EmulatorCore, InputState};
use snes1_core::SnesEmulator;

use super::{apply_force_detect, parse_press_schedule, read_rom, CoprocessorArgs, PressSchedule};

#[derive(clap::Args, Debug)]
pub struct DeterminismCheckArgs {
    /// Path to the ROM file (`.sfc` / `.smc`).
    pub rom: PathBuf,
    /// Number of frames to replay and compare.
    #[arg(long, default_value_t = 3600)]
    pub frames: u64,
    /// Timed player-1 input, e.g. `"180:start;186:;240:a,b"` (frame:buttons).
    #[arg(long)]
    pub press_at: Option<String>,
    /// Force the video region (`ntsc` | `pal`) before running; default keeps
    /// the core's ROM-derived region. Both instances always agree.
    #[arg(long)]
    pub region: Option<String>,
    /// Rollback resync period in frames. `0` (default) runs a second
    /// lockstep instance instead; `> 0` rebuilds the shadow from a savestate
    /// every N frames to exercise save/load completeness on a real ROM.
    #[arg(long, default_value_t = 0)]
    pub rollback_interval: u64,
    /// Print a hash line every `--progress` frames (0 disables progress).
    #[arg(long, default_value_t = 600)]
    pub progress: u64,
    /// Load a low-confidence ROM (blank header / bad checksum) by forcing
    /// its vector-implied mapping past the detector gate.
    #[arg(long)]
    pub force_detect: bool,
    #[command(flatten)]
    pub coprocessor: CoprocessorArgs,
}

/// True when a rollback resync should happen after completing `frame`
/// (1-based count `frame + 1` is a multiple of `interval`). `interval == 0`
/// disables resync entirely (pure two-instance mode).
fn is_resync_frame(frame: u64, interval: u64) -> bool {
    interval != 0 && (frame + 1).is_multiple_of(interval)
}

/// Boot a fresh emulator on `rom`, staging the coprocessor + force-detect
/// flags, then optionally force a region. SNES pads are fixed (two
/// standard controllers), so, unlike the Genesis check, there is no
/// controller-connect step.
fn boot(
    rom: &[u8],
    region: Option<&str>,
    coprocessor: &CoprocessorArgs,
    force_detect: bool,
) -> Result<SnesEmulator> {
    let mut emu = SnesEmulator::new();
    coprocessor.apply(&mut emu)?;
    apply_force_detect(&mut emu, rom, force_detect);
    emu.load_rom(rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;
    if let Some(region) = region {
        emu.set_region_str(region)
            .map_err(|e| anyhow::anyhow!("invalid --region: {e}"))?;
    }
    Ok(emu)
}

/// Rebuild a fresh instance from `state`, the rollback resync path. The
/// region is carried in the SNES savestate (`load_state` restores the
/// timing profile and re-anchors the APU/scheduler on it), but we still
/// boot with the same region so the pre-load machine matches the
/// reference's boot exactly.
fn restore(
    rom: &[u8],
    region: Option<&str>,
    coprocessor: &CoprocessorArgs,
    force_detect: bool,
    state: &[u8],
) -> Result<SnesEmulator> {
    let mut emu = boot(rom, region, coprocessor, force_detect)?;
    emu.load_state(state)
        .map_err(|e| anyhow::anyhow!("failed to load savestate: {e}"))?;
    Ok(emu)
}

/// Install the scheduled player-1 input for `frame` (untouched before the
/// first set-point, mirroring the `trace`/`screenshot` drivers).
fn apply_input(emu: &mut SnesEmulator, schedule: Option<&PressSchedule>, frame: u64) {
    if let Some(sched) = schedule {
        if let Some(buttons) = sched.mask_at(frame) {
            emu.set_input(0, InputState { buttons });
        }
    }
}

pub fn run(args: DeterminismCheckArgs) -> Result<()> {
    if args.frames == 0 {
        anyhow::bail!("--frames must be > 0");
    }
    let schedule = parse_press_schedule(args.press_at.as_deref())?;
    let rom = read_rom(&args.rom)?;
    let region = args.region.as_deref();

    let mut a = boot(&rom, region, &args.coprocessor, args.force_detect)?;
    let mut b = boot(&rom, region, &args.coprocessor, args.force_detect)?;

    let mode = if args.rollback_interval > 0 {
        format!("rollback (resync every {} frames)", args.rollback_interval)
    } else {
        "two-instance lockstep".to_string()
    };
    println!(
        "determinism-check: {} | {} | region={}",
        args.rom.display(),
        mode,
        a.region_str(),
    );

    for frame in 0..args.frames {
        apply_input(&mut a, schedule.as_ref(), frame);
        apply_input(&mut b, schedule.as_ref(), frame);
        a.run_frame_headless();
        b.run_frame_headless();

        let (ha, hb) = (a.state_hash(), b.state_hash());
        if ha != hb {
            report_divergence(&args.rom, frame, ha, hb, &a, &b);
            anyhow::bail!("determinism check FAILED: first divergence at frame {frame}");
        }

        if is_resync_frame(frame, args.rollback_interval) {
            let state = a.save_state();
            b = restore(&rom, region, &args.coprocessor, args.force_detect, &state)?;
            // The rebuilt shadow must land on the reference's exact state;
            // if not, the savestate is incomplete (the divergence is at the
            // resync itself, before a single further frame runs).
            let hb_restored = b.state_hash();
            if ha != hb_restored {
                report_divergence(&args.rom, frame, ha, hb_restored, &a, &b);
                anyhow::bail!(
                    "determinism check FAILED: save/load not hash-preserving at resync frame {frame}"
                );
            }
        }

        if args.progress != 0 && (frame + 1).is_multiple_of(args.progress) {
            println!(
                "  frame {:>7}/{:<7} ok  hash={ha:016x}",
                frame + 1,
                args.frames
            );
        }
    }

    println!(
        "determinism check PASSED: {} frames bit-identical (final hash {:016x})",
        args.frames,
        a.state_hash()
    );
    Ok(())
}

/// Print a divergence report: the frame, both state hashes, and which
/// serialized subsystem(s) diverged (`state_hash_parts` already breaks
/// out CPU / WRAM / PPU / APU / DMA / system-regs / scheduler / timing),
/// then a reproduce hint.
fn report_divergence(rom: &Path, frame: u64, ha: u64, hb: u64, a: &SnesEmulator, b: &SnesEmulator) {
    eprintln!("DIVERGENCE in {} at frame {frame}", rom.display());
    eprintln!("  reference hash = {ha:016x}");
    eprintln!("  shadow    hash = {hb:016x}");

    let parts_a = a.state_hash_parts();
    let parts_b = b.state_hash_parts();
    let diverged: Vec<&'static str> = parts_a
        .iter()
        .zip(&parts_b)
        .filter(|((_, av), (_, bv))| av != bv)
        .map(|((n, _), _)| *n)
        .collect();
    eprintln!("  diverged subsystems: {diverged:?}");

    eprintln!(
        "  reproduce with: snes1-cli trace {} --frames {} [--press-at ...]",
        rom.display(),
        frame + 1
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resync_disabled_when_interval_zero() {
        for f in 0..10 {
            assert!(!is_resync_frame(f, 0));
        }
    }

    #[test]
    fn resync_fires_on_interval_boundaries() {
        // interval 5 -> resync after completing frames 4, 9, 14 (1-based 5,10,15).
        assert!(!is_resync_frame(0, 5));
        assert!(!is_resync_frame(3, 5));
        assert!(is_resync_frame(4, 5));
        assert!(!is_resync_frame(5, 5));
        assert!(is_resync_frame(9, 5));
        assert!(is_resync_frame(14, 5));
    }

    #[test]
    fn resync_every_frame_when_interval_one() {
        for f in 0..10 {
            assert!(is_resync_frame(f, 1));
        }
    }
}
