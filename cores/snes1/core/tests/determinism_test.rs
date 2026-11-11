//! Deterministic-core regression harness for netplay (snes1).
//!
//! Netplay lockstep/rollback rests on a single invariant: given an
//! identical ROM, build, region, and per-frame input stream, the core
//! advances through byte-identical machine state every frame, and a
//! `save_state` -> `load_state` round-trip lands on that same state.
//! These CI-safe tests pin the invariant with a synthetic
//! auto-joypad-reading LoROM (no committed commercial ROM required):
//!
//!   1. two independent instances fed identical inputs share the same
//!      `state_hash()` on every frame (catches hidden nondeterminism,
//!      wall-clock, RNG, map iteration order, uninitialised memory);
//!   2. a `save_state` at frame K, restored into a fresh instance and
//!      run to N, matches the original's hash at every step (the
//!      rollback-correctness gate, proves the savestate is complete);
//!   3. a different input at frame J makes the hashes diverge (proves
//!      the hash actually covers input-affected state);
//!   4. `run_frame_headless()`, the exact frame method netplay uses,
//!      leaves the same `state_hash()` as the presented `run_frame()`,
//!      even with audio enabled (proves the skipped framebuffer
//!      post-process + host-audio drain are not part of machine state).

use retro_core::{EmulatorCore, InputState};
use snes1_core::SnesEmulator;

// -------------------------------------------------------------
// Synthetic LoROM (mirrors savestate_test.rs / controller_test.rs)
// -------------------------------------------------------------

const RESET_BASE: usize = 0x0000;
const NMI_HANDLER_OFF: usize = 0x0040;
const NMI_HANDLER_ADDR: u16 = 0x8040;
const IRQ_HANDLER_ADDR: u16 = 0x8060;

fn stamp_header(chip: &mut [u8], header_offset: usize, reset_vector: u16) {
    let title = b"NETPLAY DET TEST     ";
    chip[header_offset + 0x10..header_offset + 0x10 + 21].copy_from_slice(title);
    chip[header_offset + 0x25] = 0x20; // LoROM
    chip[header_offset + 0x26] = 0x00; // ROM only
    chip[header_offset + 0x27] = 0x0A; // 1 MiB
    chip[header_offset + 0x28] = 0x00; // no SRAM
    chip[header_offset + 0x29] = 0x01; // USA -> NTSC
    chip[header_offset + 0x2A] = 0x01;
    chip[header_offset + 0x2B] = 0x00;
    let checksum: u16 = 0x1234;
    chip[header_offset + 0x2C..header_offset + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    chip[header_offset + 0x2E..header_offset + 0x30].copy_from_slice(&checksum.to_le_bytes());
    chip[header_offset + 0x4C..header_offset + 0x4E].copy_from_slice(&reset_vector.to_le_bytes());
}

/// A 1 MiB LoROM whose reset routine enables NMI + auto-joypad read,
/// turns the display on, then loops reading the auto-joypad result
/// (`$4218`/`$4219`) and accumulating both bytes into work RAM at direct
/// page `$00`/`$01`. Because the accumulator is part of the savestate and
/// tracks the pressed buttons latched each V-blank, the machine state
/// becomes a deterministic function of the per-frame input, exactly what
/// a netplay state hash must capture.
///
/// ```text
/// LDA #$80 / STA $2100   ; forced blank
/// LDA #$81 / STA $4200   ; enable NMI (b7) + auto-joypad read (b0)
/// LDA #$0F / STA $2100   ; display on, full brightness
/// loop:
///   LDA $4218 / CLC / ADC $00 / STA $00   ; RAM[$00].b += JOY1L
///   LDA $4219 / CLC / ADC $01 / STA $01   ; RAM[$01].b += JOY1H
///   BRA loop
/// ```
#[rustfmt::skip]
const RESET_INPUT: [u8; 33] = [
    0xA9, 0x80, 0x8D, 0x00, 0x21, // LDA #$80 / STA $2100  (forced blank)
    0xA9, 0x81, 0x8D, 0x00, 0x42, // LDA #$81 / STA $4200  (NMI + auto-joypad)
    0xA9, 0x0F, 0x8D, 0x00, 0x21, // LDA #$0F / STA $2100  (display on)
    // loop: (ROM offset 15 = $800F)
    0xAD, 0x18, 0x42, // LDA $4218  (JOY1L)
    0x18, 0x65, 0x00, 0x85, 0x00, // CLC / ADC $00 / STA $00
    0xAD, 0x19, 0x42, // LDA $4219  (JOY1H)
    0x18, 0x65, 0x01, 0x85, 0x01, // CLC / ADC $01 / STA $01
    0x80, 0xEE, // BRA loop (-18)
];

fn build_rom() -> Vec<u8> {
    const SIZE: usize = 0x10_0000;
    let mut chip = vec![0u8; SIZE];
    chip[RESET_BASE..RESET_BASE + RESET_INPUT.len()].copy_from_slice(&RESET_INPUT);
    chip[NMI_HANDLER_OFF] = 0x40; // NMI handler: bare RTI
    chip[0x0060] = 0x40; // IRQ handler: bare RTI (never armed)
    stamp_header(&mut chip, 0x7FB0, 0x8000);
    chip[0x7FFA..0x7FFC].copy_from_slice(&NMI_HANDLER_ADDR.to_le_bytes());
    chip[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    chip[0x7FFE..0x8000].copy_from_slice(&IRQ_HANDLER_ADDR.to_le_bytes());
    chip
}

/// SNES pad bits in `InputState::buttons` (indices mirror the CLI's
/// `parse_button_mask`: A=4, B=5, Start=10). A spans the JOY1 low byte
/// and Start the high byte, so pressing both perturbs both accumulators.
const BTN_A: u32 = 1 << 4;
const BTN_B: u32 = 1 << 5;
const BTN_START: u32 = 1 << 10;

const FRAMES: u64 = 600;

/// Boot a fresh emulator on the auto-joypad ROM. No controller-connect
/// step: the two standard SNES pads are always present.
fn booted() -> SnesEmulator {
    let mut emu = SnesEmulator::new();
    emu.load_rom(&build_rom()).expect("load synthetic LoROM");
    emu
}

/// A deterministic per-frame input pattern: press A+B for one stretch,
/// Start for the next, otherwise nothing. Both stretches change the
/// auto-joypad read, so they actually perturb the accumulator.
fn schedule(frame: u64) -> u32 {
    match frame {
        120..=239 => BTN_A | BTN_B,
        240..=359 => BTN_START,
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
    // state. This is the completeness check, the hash covers precisely
    // what the savestate restores.
    let mut c = booted();
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

    // From J on, D holds A+B while A holds nothing. The differing read
    // feeds the accumulator, so the hashes must part.
    for _ in J..(J + 60) {
        a.set_input(0, InputState { buttons: 0 });
        d.set_input(
            0,
            InputState {
                buttons: BTN_A | BTN_B,
            },
        );
        a.run_frame_headless();
        d.run_frame_headless();
    }
    assert_ne!(
        a.state_hash(),
        d.state_hash(),
        "state hash ignored the divergent input"
    );
}

#[test]
fn headless_matches_presented() {
    // A presented `run_frame()` and a headless `run_frame_headless()`
    // must leave identical machine state. Audio is enabled so the
    // presented instance actually drains the APU to the host ring (which
    // is NOT serialized), proving the skipped presentation steps
    // (framebuffer post-process + host-audio drain) are outside the
    // state hash.
    let mut presented = booted();
    let mut headless = booted();
    presented.set_audio_enabled(true);
    headless.set_audio_enabled(true);

    for frame in 0..FRAMES {
        let buttons = schedule(frame);
        presented.set_input(0, InputState { buttons });
        headless.set_input(0, InputState { buttons });
        presented.run_frame();
        headless.run_frame_headless();
        assert_eq!(
            presented.state_hash(),
            headless.state_hash(),
            "headless frame diverged from presented frame at frame {frame}"
        );
    }
}

/// Pull everything the host ring holds, the way the browser's audio service
/// does every frame, so the next assertion measures only what was queued
/// after this point.
fn drain_host_audio(emu: &mut SnesEmulator) {
    let mut sink = vec![0.0f32; emu.audio_available().max(1)];
    emu.drain_audio_into(&mut sink);
}

#[test]
fn discarding_pending_audio_does_not_move_the_state_hash() {
    // The netplay runner drops the audio a rollback re-simulation queued,
    // because the player already heard those frames in their mispredicted
    // form. That is only safe if the queue is outside the serialized state:
    // if it were inside, one peer discarding would desync the session.
    let mut kept = booted();
    let mut discarded = booted();
    kept.set_audio_enabled(true);
    discarded.set_audio_enabled(true);

    for frame in 0..FRAMES {
        let buttons = schedule(frame);
        kept.set_input(0, InputState { buttons });
        discarded.set_input(0, InputState { buttons });
        // Headless on both, so both queue samples the host never drained.
        kept.run_frame_headless();
        discarded.run_frame_headless();
        discarded.discard_pending_audio();
        assert_eq!(
            kept.state_hash(),
            discarded.state_hash(),
            "discarding queued audio moved the state hash at frame {frame}"
        );
    }
}

#[test]
fn a_resim_burst_stops_replaying_audio_the_player_already_heard() {
    // A rollback in miniature: save, present three frames, load,
    // re-simulate those three headlessly, then present one. Without the
    // discard the presented frame drains all four frames at once, which
    // plays as an echo. With it, roughly one frame's worth is queued.
    let mut emu = booted();
    emu.set_audio_enabled(true);

    for frame in 0..3u64 {
        emu.set_input(
            0,
            InputState {
                buttons: schedule(frame),
            },
        );
        emu.run_frame();
    }
    drain_host_audio(&mut emu); // the host consumed what it was given
    let snapshot = emu.save_state();

    for frame in 3..6u64 {
        emu.set_input(
            0,
            InputState {
                buttons: schedule(frame),
            },
        );
        emu.run_frame();
    }
    drain_host_audio(&mut emu);

    emu.load_state(&snapshot).expect("load");
    for frame in 3..6u64 {
        emu.set_input(
            0,
            InputState {
                buttons: schedule(frame),
            },
        );
        emu.run_frame_headless();
    }
    emu.discard_pending_audio();

    emu.set_input(
        0,
        InputState {
            buttons: schedule(6),
        },
    );
    emu.run_frame();

    // One presented frame at 44.1 kHz stereo is ~1470 slots. Allow a wide
    // band, the point is that it is one frame and not four.
    let queued = emu.audio_available();
    assert!(
        queued < 2200,
        "expected about one frame of audio after the discard, got {queued} slots"
    );
}
