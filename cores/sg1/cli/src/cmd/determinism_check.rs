//! `determinism-check`: long-replay determinism gate for netplay.
//!
//! Netplay lockstep/rollback rests on one invariant: given an identical
//! ROM, build, region, and per-frame input stream, the core advances
//! through byte-identical machine state every frame, and a `save_state`
//! -> `load_state` round-trip lands on that same state. The CI test
//! `cores/sg1/core/tests/determinism_test.rs` pins this on a synthetic
//! ROM; this subcommand validates it against *real* commercial titles
//! over a long recorded replay, which is where subtle nondeterminism
//! (uninitialised reads, RNG, an incomplete savestate field) actually
//! surfaces.
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
//! On the first divergent frame it reports both hashes and whether Work
//! RAM differs (a cheap hint at whether the divergence is in RAM or in a
//! CPU/VDP/audio register), then exits non-zero so a CI wrapper fails.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};
use sg1_core::GenesisEmulator;

use super::{apply_press, connect_controllers, parse_press_schedule};

#[derive(clap::Args, Debug)]
pub struct DeterminismCheckArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Number of frames to replay and compare.
    #[arg(long, default_value_t = 3600)]
    pub frames: u64,
    /// Timed player-1 input, e.g. `"180:start;186:;240:a+c"` (frame:buttons).
    #[arg(long)]
    pub press_at: Option<String>,
    /// Connect 6-button pads instead of the default 3-button pads.
    #[arg(long)]
    pub six_button: bool,
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
}

/// True when a rollback resync should happen after completing `frame`
/// (1-based count `frame + 1` is a multiple of `interval`). `interval == 0`
/// disables resync entirely (pure two-instance mode).
fn is_resync_frame(frame: u64, interval: u64) -> bool {
    interval != 0 && (frame + 1).is_multiple_of(interval)
}

/// Boot a fresh emulator on `rom`, connect pads, optionally force a region,
/// and put the CPU into `Running` (otherwise `run_frame_*` never advances).
fn boot(rom: &[u8], six_button: bool, region: Option<&str>) -> Result<GenesisEmulator> {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;
    if six_button {
        connect_controllers(&mut emu, true);
    }
    if let Some(region) = region {
        emu.set_region_str(region)
            .map_err(|e| anyhow::anyhow!("invalid --region: {e}"))?;
    }
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    Ok(emu)
}

/// Rebuild a fresh instance from `state`, the rollback resync path. The
/// region is re-forced before the load because it is not carried in the
/// savestate and the restored rate converter is anchored on it.
fn restore(
    rom: &[u8],
    six_button: bool,
    region: Option<&str>,
    state: &[u8],
) -> Result<GenesisEmulator> {
    let mut emu = boot(rom, six_button, region)?;
    emu.load_state(state)
        .map_err(|e| anyhow::anyhow!("failed to load savestate: {e}"))?;
    Ok(emu)
}

pub fn run(args: DeterminismCheckArgs) -> Result<()> {
    if args.frames == 0 {
        anyhow::bail!("--frames must be > 0");
    }
    let schedule = parse_press_schedule(args.press_at.as_deref())?;
    let rom = std::fs::read(&args.rom)
        .with_context(|| format!("failed to read ROM: {}", args.rom.display()))?;
    let region = args.region.as_deref();

    let mut a = boot(&rom, args.six_button, region)?;
    let mut b = boot(&rom, args.six_button, region)?;

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
        apply_press(&mut a, schedule.as_ref(), frame);
        apply_press(&mut b, schedule.as_ref(), frame);
        a.run_frame_headless();
        b.run_frame_headless();

        let (ha, hb) = (a.state_hash(), b.state_hash());
        if ha != hb {
            report_divergence(&args.rom, frame, ha, hb, &mut a, &mut b);
            anyhow::bail!("determinism check FAILED: first divergence at frame {frame}");
        }

        if is_resync_frame(frame, args.rollback_interval) {
            let state = a.save_state();
            b = restore(&rom, args.six_button, region, &state)?;
            // The rebuilt shadow must land on the reference's exact state;
            // if not, the savestate is incomplete (the divergence is at the
            // resync itself, before a single further frame runs).
            let hb_restored = b.state_hash();
            if ha != hb_restored {
                report_divergence(&args.rom, frame, ha, hb_restored, &mut a, &mut b);
                anyhow::bail!(
                    "determinism check FAILED: save/load not hash-preserving at resync frame {frame}"
                );
            }
        }

        if args.progress != 0 && (frame + 1) % args.progress == 0 {
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

/// Print a divergence report: the frame, both state hashes, and whether
/// the 64 KB Work RAM differs (a cheap hint at whether the split is in RAM
/// vs. a CPU/VDP/audio register the full hash also covers).
fn report_divergence(
    rom: &Path,
    frame: u64,
    ha: u64,
    hb: u64,
    a: &mut GenesisEmulator,
    b: &mut GenesisEmulator,
) {
    let ram_a = a.read_memory_debug(0xFF_0000, 0x1_0000);
    let ram_b = b.read_memory_debug(0xFF_0000, 0x1_0000);
    let ram_note = if ram_a == ram_b {
        "Work RAM identical (divergence is in CPU/VDP/audio state)"
    } else {
        "Work RAM DIFFERS"
    };
    eprintln!("DIVERGENCE in {} at frame {frame}", rom.display());
    eprintln!("  reference hash = {ha:016x}");
    eprintln!("  shadow    hash = {hb:016x}");
    eprintln!("  {ram_note}");

    // Pinpoint the subsystem(s) whose serialized state split.
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
        "  bisect with: sg1-cli fingerprint {} --frames {} [--press-at ...]",
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
