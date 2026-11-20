//! `determinism-check`: a long-replay determinism gate for netplay.
//!
//! Netplay's lockstep and rollback rest on one invariant: given the same
//! ROM, build, region and input every frame, the machine goes through
//! byte-identical states, and a `save_state` -> `load_state` round trip
//! lands on that same state. The cores' determinism tests pin it on a
//! synthetic ROM; this command checks it on a real game over a long
//! replay, which is where a hidden nondeterminism (an uninitialised read,
//! a field the savestate misses) shows.
//!
//! Two modes, both running the frame netplay runs (`run_frame_headless`)
//! and comparing `state_hash` every frame:
//!
//! * **two-instance** (by default): two machines booted apart and given
//!   the same input must hash the same every frame.
//! * **rollback** (`--rollback-interval N`): a reference machine runs
//!   straight through, and every `N` frames a new shadow is built from its
//!   savestate and must then follow it exactly: the rollback's gate, on a
//!   real ROM.
//!
//! At the first frame they part it reports both hashes, whether the work
//! RAM differs, and the parts of the state that split, then exits non-zero
//! so a script running it fails.

use std::path::Path;

use anyhow::Result;
use retro_core::savestate::RomCheck;
use retro_core::EmulatorCore;

use crate::console::Console;
use crate::input::InputArgs;
use crate::rom::{load_image, read_rom, RomArgs};

#[derive(clap::Args, Debug)]
pub struct DeterminismCheckArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Number of frames to replay and compare.
    #[arg(long, default_value_t = 3600)]
    pub frames: u64,
    #[command(flatten)]
    pub input: InputArgs,
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

/// Whether the shadow is rebuilt after `frame` (0-based): every
/// `interval` frames, never when it is 0.
fn is_resync_frame(frame: u64, interval: u64) -> bool {
    interval != 0 && (frame + 1).is_multiple_of(interval)
}

/// A machine booted on `rom`, its region forced when one is given.
fn start<C: Console>(rom: &[u8], boot: &C::BootArgs, region: Option<&str>) -> Result<C::Emulator> {
    let mut emu = load_image::<C>(rom, boot)?;
    if let Some(region) = region {
        emu.set_region_str(region)
            .map_err(|e| anyhow::anyhow!("invalid --region: {e}"))?;
    }
    Ok(emu)
}

pub fn run<C: Console>(args: DeterminismCheckArgs<C::BootArgs>) -> Result<()> {
    if args.frames == 0 {
        anyhow::bail!("--frames must be > 0");
    }
    let input = args.input.plan::<C::Emulator>()?;
    let path = &args.rom.path;
    let rom = read_rom(path)?;
    let boot = &args.rom.boot;
    let region = args.region.as_deref();

    let mut a = start::<C>(&rom, boot, region)?;
    let mut b = start::<C>(&rom, boot, region)?;

    let mode = if args.rollback_interval > 0 {
        format!("rollback (resync every {} frames)", args.rollback_interval)
    } else {
        "two-instance lockstep".to_string()
    };
    println!(
        "determinism-check: {} | {} | region={}",
        path.display(),
        mode,
        a.region_str(),
    );

    for frame in 0..args.frames {
        input.before_frame(&mut a, frame);
        input.before_frame(&mut b, frame);
        a.run_frame_headless();
        b.run_frame_headless();

        let (ha, hb) = (a.state_hash(), b.state_hash());
        if ha != hb {
            report_divergence::<C>(path, frame, ha, hb, &mut a, &mut b);
            anyhow::bail!("determinism check FAILED: first divergence at frame {frame}");
        }

        if is_resync_frame(frame, args.rollback_interval) {
            let state = a.save_state();
            // The shadow is booted as the reference was, the region forced
            // again, before the state loads over it.
            b = start::<C>(&rom, boot, region)?;
            b.load_state(&state, RomCheck::SameRom)
                .map_err(|e| anyhow::anyhow!("failed to load savestate: {e}"))?;
            // A rebuilt shadow that does not land on the reference's state
            // shows a savestate that misses something, before a single
            // further frame runs.
            let hb_restored = b.state_hash();
            if ha != hb_restored {
                report_divergence::<C>(path, frame, ha, hb_restored, &mut a, &mut b);
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

/// The report of a divergence: the frame, both hashes, whether the work
/// RAM differs (a hint at whether the split is in RAM or in a chip's
/// state, which the hash covers too), the parts of the state that split,
/// and the commands that look closer.
fn report_divergence<C: Console>(
    rom: &Path,
    frame: u64,
    ha: u64,
    hb: u64,
    a: &mut C::Emulator,
    b: &mut C::Emulator,
) {
    let (start, size) = C::WORK_RAM;
    let ram_note = if a.read_memory_debug(start, size) == b.read_memory_debug(start, size) {
        "Work RAM identical (divergence is in CPU, video or audio state)"
    } else {
        "Work RAM DIFFERS"
    };
    eprintln!("DIVERGENCE in {} at frame {frame}", rom.display());
    eprintln!("  reference hash = {ha:016x}");
    eprintln!("  shadow    hash = {hb:016x}");
    eprintln!("  {ram_note}");

    let parts_a = a.state_hash_parts();
    let parts_b = b.state_hash_parts();
    let diverged: Vec<&'static str> = parts_a
        .iter()
        .zip(&parts_b)
        .filter(|((_, av), (_, bv))| av != bv)
        .map(|((name, _), _)| *name)
        .collect();
    eprintln!("  diverged subsystems: {diverged:?}");

    eprintln!(
        "  bisect with: {} fingerprint {} --frames {} [--press-at ...]",
        C::TOOL,
        rom.display(),
        frame + 1
    );
    eprintln!(
        "  reproduce with: {} trace {} --frames {} [--press-at ...]",
        C::TOOL,
        rom.display(),
        frame + 1
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_resync_when_the_interval_is_zero() {
        assert!((0..10).all(|frame| !is_resync_frame(frame, 0)));
    }

    #[test]
    fn a_resync_falls_after_every_interval() {
        // Every 5 frames: after frames 4, 9 and 14 (the 5th, 10th, 15th).
        let resyncs: Vec<u64> = (0..15).filter(|&f| is_resync_frame(f, 5)).collect();
        assert_eq!(resyncs, vec![4, 9, 14]);
        assert!((0..10).all(|frame| is_resync_frame(frame, 1)));
    }
}
