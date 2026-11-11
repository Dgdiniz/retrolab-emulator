//! Deterministic-core regression harness for netplay (sg1).
//!
//! Netplay lockstep/rollback rests on a single invariant: given an
//! identical ROM, build, region, and per-frame input stream, the core
//! advances through byte-identical machine state every frame, and a
//! `save_state` -> `load_state` round-trip lands on that same state.
//! These CI-safe tests pin the invariant with a synthetic
//! input-reading ROM (no committed commercial ROM required):
//!
//!   1. two independent instances fed identical inputs share the same
//!      `state_hash()` on every frame (catches hidden nondeterminism:
//!      wall-clock, RNG, map iteration order, uninitialised memory);
//!   2. a `save_state` at frame K, restored into a fresh instance and
//!      run to N, matches the original's hash at every step (the
//!      rollback-correctness gate: it proves the savestate is complete);
//!   3. a different input at frame J makes the hashes diverge (proves
//!      the hash actually covers input-affected state).
//!
//! The tests drive `run_frame_headless()`, the exact frame method
//! netplay uses, so they also prove the headless path is
//! bit-equivalent to a presented frame for state purposes.

use retro_core::types::ExecutionMode;
use retro_core::{DebuggerCore, EmulatorCore, InputState};
use sg1_core::io::devices::genesis_3button::Genesis3ButtonDevice;
use sg1_core::GenesisEmulator;

mod helpers;
use helpers::MiniCartridgeBuilder;

// Button bits in `InputState::buttons`, matching the Genesis 3-button
// device layout. The ROM never toggles the TH line, so the controller
// read stays in its TH=1 phase: only C/B/directions are visible there
// (A/Start live in the TH=0 phase), so the input tests press C and B.
const BTN_B: u32 = 1 << 4;
const BTN_C: u32 = 1 << 5;

const FRAMES: u64 = 600;

/// A tiny 68000 program that continuously reads controller port 1
/// (`$A10003`) and accumulates the byte into work RAM at `$FF0000`.
/// Because the read reflects the pressed buttons and the accumulator is
/// part of the savestate, the machine state becomes a deterministic
/// function of the per-frame input, exactly what a netplay state hash
/// must capture.
///
/// ```text
/// 0x400: MOVE.B $00A10003, D1   ; 1239 00A1 0003  read controller data port 1
/// 0x406: ADD.B  D1, $00FF0000   ; D339 00FF 0000  RAM[$FF0000].b += D1.b
/// 0x40C: BRA.w  0x400           ; 6000 FFF2       loop forever
/// ```
fn input_reading_rom() -> Vec<u8> {
    MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("NETPLAY_DET")
        .with_instructions(
            0x400,
            &[
                0x1239, 0x00A1, 0x0003, // MOVE.B $00A10003, D1
                0xD339, 0x00FF, 0x0000, // ADD.B  D1, $00FF0000
                0x6000, 0xFFF2, // BRA.w 0x400
            ],
        )
        .build()
}

/// Boot a fresh emulator on the input-reading ROM with 3-button pads on
/// both ports and the CPU running. A device MUST be connected or the
/// port reads its pulled-up pins (`0x7F`: bit 7 reads 0) and a press would
/// never reach the CPU.
fn booted() -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&input_reading_rom()).expect("load mini rom");
    connect_pads(&mut emu);
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    emu
}

fn connect_pads(emu: &mut GenesisEmulator) {
    for port in 0..2 {
        if let Some(p) = emu.bus_mut().io_mut().port_mut(port) {
            p.connect_device(Box::new(Genesis3ButtonDevice::new()));
        }
    }
}

/// A deterministic per-frame input pattern for the identical-input
/// tests: hold C for one stretch, B for the next, otherwise nothing.
/// Both use TH=1-visible buttons so they actually perturb the read.
fn schedule(frame: u64) -> u32 {
    match frame {
        120..=239 => BTN_C,
        240..=359 => BTN_B,
        _ => 0,
    }
}

#[test]
fn two_instances_stay_bit_identical() {
    let mut a = booted();
    let mut b = booted();

    for frame in 0..FRAMES {
        let buttons = schedule(frame);
        a.set_input(0, InputState { buttons });
        b.set_input(0, InputState { buttons });
        a.run_frame_headless();
        b.run_frame_headless();
        assert_eq!(
            a.state_hash(),
            b.state_hash(),
            "state diverged at frame {frame} despite identical inputs"
        );
        assert_eq!(a.frame_count(), b.frame_count());
    }
}

#[test]
fn save_load_round_trip_is_hash_preserving() {
    const K: u64 = 300;

    let mut a = booted();
    for frame in 0..K {
        a.set_input(
            0,
            InputState {
                buttons: schedule(frame),
            },
        );
        a.run_frame_headless();
    }
    let saved = a.save_state();
    let hash_at_k = a.state_hash();

    // Restore into a fresh instance: it must land on the exact same
    // state. This is the completeness check: the hash covers precisely
    // what the savestate restores.
    let mut c = GenesisEmulator::new();
    c.load_rom(&input_reading_rom()).expect("load mini rom");
    connect_pads(&mut c);
    c.load_state(&saved).expect("load state");

    assert_eq!(hash_at_k, c.state_hash(), "hash differs right after load");
    assert_eq!(
        a.frame_count(),
        c.frame_count(),
        "frame_count differs after load"
    );

    // Run both forward with identical inputs; they must stay locked.
    for frame in K..FRAMES {
        let buttons = schedule(frame);
        a.set_input(0, InputState { buttons });
        c.set_input(0, InputState { buttons });
        a.run_frame_headless();
        c.run_frame_headless();
        assert_eq!(
            a.state_hash(),
            c.state_hash(),
            "restored instance diverged at frame {frame}"
        );
    }
}

/// Pinpoint the diverging subsystem on failure. Returns the parts that
/// differ between two instances, formatted for an assert message.
fn diff_parts(a: &GenesisEmulator, b: &GenesisEmulator) -> String {
    let pa = a.state_hash_parts();
    let pb = b.state_hash_parts();
    pa.iter()
        .zip(pb.iter())
        .filter(|((_, ha), (_, hb))| ha != hb)
        .map(|((name, ha), (_, hb))| format!("{name}: {ha:016x} != {hb:016x}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Presented (audible) frames and headless frames must land on
/// **identical serialized machine state**. Netplay runs the local peer's
/// frames audibly (`run_frame_fast`) but re-simulates rolled-back or
/// caught-up spans headlessly (`run_frame_headless`); if the two diverged,
/// the confirmed-frame checksum ladder would fire a full resync on every
/// remote input. Audio must therefore be *synthesized* (converter + chip
/// sample-phase accumulators advance) on headless frames too, and only the
/// mix / ring push / framebuffer copy may be presentation-gated.
#[test]
fn audible_and_headless_frames_hash_identically() {
    let mut audible = booted();
    let mut headless = booted();

    for frame in 0..FRAMES {
        let buttons = schedule(frame);
        audible.set_input(0, InputState { buttons });
        headless.set_input(0, InputState { buttons });
        audible.run_frame_fast(); // presents: mixes + pushes audio ring
        headless.run_frame_headless(); // suppresses presentation only
        assert_eq!(
            audible.state_hash(),
            headless.state_hash(),
            "presented vs headless state diverged at frame {frame} \
             (diverging parts: {})",
            diff_parts(&audible, &headless),
        );
        assert_eq!(audible.frame_count(), headless.frame_count());
    }
}

/// The realistic rollback scenario end to end: a reference instance runs
/// the whole span audibly; a second instance runs to a mid-point audibly,
/// then (as a rollback would) restores an earlier savestate and
/// re-simulates the remainder **headlessly**. Their hashes must match at
/// the end, proving a headless re-sim over a checksummed span reproduces
/// the audible run bit-for-bit, so no spurious desync is reported.
#[test]
fn headless_resim_after_rollback_matches_audible_run() {
    const K: u64 = 200;

    let mut reference = booted();
    let mut rolled_back = booted();

    // Both advance audibly to the rollback point.
    let mut snapshot = Vec::new();
    for frame in 0..K {
        let buttons = schedule(frame);
        reference.set_input(0, InputState { buttons });
        rolled_back.set_input(0, InputState { buttons });
        reference.run_frame_fast();
        rolled_back.run_frame_fast();
        if frame == K - 1 {
            snapshot = rolled_back.save_state();
        }
    }

    // The reference keeps running audibly; the other peer discovers a
    // misprediction, reloads the snapshot, and re-simulates headlessly.
    rolled_back.load_state(&snapshot).expect("reload snapshot");
    for frame in K..FRAMES {
        let buttons = schedule(frame);
        reference.set_input(0, InputState { buttons });
        rolled_back.set_input(0, InputState { buttons });
        reference.run_frame_fast();
        rolled_back.run_frame_headless();
        assert_eq!(
            reference.state_hash(),
            rolled_back.state_hash(),
            "headless re-sim diverged from the audible run at frame {frame} \
             (diverging parts: {})",
            diff_parts(&reference, &rolled_back),
        );
    }
}

#[test]
fn different_input_diverges() {
    const J: u64 = 300;

    let mut a = booted();
    let mut d = booted();

    // Identical (no input) until frame J.
    for _ in 0..J {
        a.set_input(0, InputState { buttons: 0 });
        d.set_input(0, InputState { buttons: 0 });
        a.run_frame_headless();
        d.run_frame_headless();
    }
    assert_eq!(
        a.state_hash(),
        d.state_hash(),
        "instances must be identical before the input diverges"
    );

    // From J on, D holds C while A holds nothing. The differing read
    // feeds the accumulator, so the hashes must part.
    for _ in J..(J + 60) {
        a.set_input(0, InputState { buttons: 0 });
        d.set_input(0, InputState { buttons: BTN_C });
        a.run_frame_headless();
        d.run_frame_headless();
    }
    assert_ne!(
        a.state_hash(),
        d.state_hash(),
        "state hash ignored the divergent input"
    );
}

/// Muting is **emulation state on this core**, not a presentation
/// toggle. `audio_enabled` is serialized into the savestate the hash
/// covers, and `set_audio_enabled` additionally re-gates the sound
/// chips' catch-up synthesis, so the mute also stops the rate
/// converter advancing. Two instances that disagree about it therefore
/// diverge by construction, and no reordering inside the frame can fix
/// that. It is why a netplay session keeps the core audible and applies
/// the listener's mute to the host output instead.
///
/// The test exists to fail loudly if that stops being true: were the
/// field to leave the hash, the host-side routing would no longer be
/// load-bearing and should be revisited rather than kept as cargo cult.
#[test]
fn muting_the_core_changes_the_state_hash() {
    let mut loud = booted();
    let mut quiet = booted();
    quiet.set_audio_enabled(false);

    for frame in 0..120u64 {
        let buttons = schedule(frame);
        loud.set_input(0, InputState { buttons });
        quiet.set_input(0, InputState { buttons });
        loud.run_frame_headless();
        quiet.run_frame_headless();
    }

    assert_ne!(
        loud.state_hash(),
        quiet.state_hash(),
        "muting no longer perturbs hashed state: revisit the netplay mute routing \
         (core audible, host output muted), which exists because it does"
    );
}
