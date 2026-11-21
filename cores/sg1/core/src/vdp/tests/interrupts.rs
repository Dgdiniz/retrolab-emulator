//! Unit tests of the line step and the interrupts: the vertical blank's flag
//! and interrupt, the horizontal interrupt's counter, and the odd frame.

use super::*;

#[test]
fn test_scanline_vblank() {
    let mut vdp = Vdp::new();

    // Before VBLANK
    vdp.scanline(100);
    assert!(!vdp.in_vblank());

    // During VBLANK (line 224+)
    vdp.scanline(224);
    assert!(vdp.in_vblank());
}

#[test]
fn test_vint_enable() {
    let mut vdp = Vdp::new();

    // VINT disabled
    vdp.timing.in_vblank = true;
    vdp.timing.set_vint_pending();
    vdp.registers[1] &= !0x20;

    // Should still be pending (enable flag only controls triggering)
    assert!(vdp.should_trigger_vint());

    // Clear pending
    vdp.ack_vint();
    assert!(!vdp.should_trigger_vint());
}

#[test]
fn test_odd_frame_toggle() {
    let mut vdp = Vdp::new();

    // Initially even frame
    assert!(!vdp.odd_frame);

    // Simulate entering VBLANK (triggers frame toggle)
    vdp.scanline(223); // Last visible line
    assert!(!vdp.odd_frame);

    vdp.scanline(224); // First VBLANK line
    assert!(vdp.odd_frame); // Should toggle to odd

    // Simulate full frame
    for line in 225..262 {
        vdp.scanline(line);
    }
    for line in 0..224 {
        vdp.scanline(line);
    }
    vdp.scanline(224); // Next VBLANK
    assert!(!vdp.odd_frame); // Should toggle back to even
}

/// Drive one full NTSC frame of `scanline()` calls (0..262), like the
/// per-line loop every frame tier runs. The VDP boots inside VBLANK at line
/// 224, so the first active->VBLANK transition only occurs partway through
/// this first frame, exactly the transition the hardware suppresses.
fn advance_one_frame(vdp: &mut Vdp) {
    for line in 0..262 {
        vdp.scanline(line);
    }
}

#[test]
fn test_vint_trigger_at_vblank_start() {
    let mut vdp = Vdp::new();

    // Enable VINT (register 1 bit 5)
    vdp.set_test_register(1, 0x24);

    // The first frame after a VDP reset raises NO VINT (the VDP comes out
    // of reset already past VBLANK). Consume it, then assert it stayed
    // quiet.
    advance_one_frame(&mut vdp);
    assert!(
        !vdp.should_trigger_vint(),
        "first post-reset frame must not raise VINT"
    );

    // Before VBLANK - VINT should not trigger
    vdp.scanline(223);
    assert!(!vdp.should_trigger_vint());

    // Enter VBLANK on the next frame - VINT now triggers normally
    vdp.scanline(224);
    assert!(vdp.should_trigger_vint());

    // After acknowledging, should not trigger again until next frame
    vdp.ack_vint();
    assert!(!vdp.should_trigger_vint());
}

/// The VDP boots already inside VBLANK, so the very first
/// frame after a reset raises neither the F flag nor the level-6 request.
/// Without this, SG1 would deliver every game's first VINT one frame early
/// and run a logic frame ahead of Genesis Plus GX from boot. The
/// suppression must re-arm on every `reset()` (a soft reset restarts the
/// same cadence).
#[test]
fn test_first_frame_vint_suppressed_after_reset() {
    let mut vdp = Vdp::new();
    vdp.set_test_register(1, 0x24); // enable VINT + display

    // First frame after construction: its VBLANK is suppressed, no F flag.
    advance_one_frame(&mut vdp);
    assert!(!vdp.should_trigger_vint());
    assert!(!vdp.timing.vblank_pending);

    // Second frame: VBLANK now raises the VINT + F flag normally.
    advance_one_frame(&mut vdp);
    assert!(vdp.should_trigger_vint());
    assert!(vdp.timing.vblank_pending);

    // A soft reset re-arms the one-frame suppression.
    vdp.reset();
    vdp.set_test_register(1, 0x24);
    advance_one_frame(&mut vdp);
    assert!(
        !vdp.should_trigger_vint(),
        "reset must re-arm first-frame VINT suppression"
    );
}

#[test]
fn test_vint_pending_cleared_on_frame_start() {
    let mut vdp = Vdp::new();

    // Enable VINT
    vdp.set_test_register(1, 0x24);

    // Consume the first post-reset frame, whose VBLANK raises no VINT
    // (see above); every frame after it behaves normally.
    advance_one_frame(&mut vdp);
    assert!(!vdp.should_trigger_vint());

    // The frame above left the VDP at scanline 261 (VBLANK); exit VBLANK by
    // running the VBLANK scanlines again and wrapping to scanline 0
    for line in 225..262 {
        vdp.scanline(line);
    }
    vdp.scanline(0); // Exit VBLANK (new frame)
    assert!(!vdp.timing.in_vblank);

    // Run through visible area
    for line in 1..224 {
        vdp.scanline(line);
    }

    // Enter VBLANK and trigger VINT
    vdp.scanline(224);
    assert!(vdp.should_trigger_vint());

    // Run through VBLANK
    for line in 225..262 {
        vdp.scanline(line);
    }

    // Start new frame (scanline 0) - F flag persists until IACK
    vdp.scanline(0);
    assert!(vdp.timing.vblank_pending); // F flag NOT cleared by scanline transition

    // Reading status register does NOT clear F flag (only IACK does)
    let _status = vdp.read_word_port(0xC00004);
    assert!(vdp.timing.vblank_pending); // Still set - only IACK clears it

    // Simulate IACK (interrupt acknowledge) clearing the F flag
    vdp.ack_vint_iack();
    assert!(!vdp.timing.vblank_pending); // NOW cleared by IACK

    // Next VBLANK should trigger VINT again
    for line in 1..224 {
        vdp.scanline(line);
    }
    vdp.scanline(224);
    assert!(vdp.should_trigger_vint());
}

#[test]
fn test_vint_disabled_when_register_clear() {
    let mut vdp = Vdp::new();

    // VINT disabled (register 1 bit 5 = 0)
    vdp.set_test_register(1, 0x04);

    // Enter VBLANK
    vdp.scanline(224);

    // VINT should not trigger (disabled)
    assert!(!vdp.should_trigger_vint());
}

#[test]
fn test_hint_counter_reload() {
    let mut vdp = Vdp::new();

    // Set HINT counter reload value to 3 (interrupt every 4 scanlines)
    vdp.set_test_register(10, 3);

    // Writing R10 updates ONLY the reload value; the live counter is
    // reloaded from it during vblank / on underflow,
    // NOT reset on the register write itself.
    assert_eq!(vdp.timing.h_counter_reload, 3);

    // A vblank scanline reloads the live counter from the reload value.
    vdp.scanline(240);
    assert_eq!(vdp.timing.h_counter_current, 3);
}

#[test]
fn test_hint_trigger_at_counter_zero() {
    let mut vdp = Vdp::new();

    // Enable HINT (register 0 bit 4)
    vdp.set_test_register(0, 0x14);

    // Set HINT counter to 2 (triggers after 3 scanlines)
    vdp.set_test_register(10, 2);

    // Prime the live counter from the reload value via a vblank line:
    // writing R10 does not reset the live counter directly.
    vdp.scanline(240);
    assert_eq!(vdp.timing.h_counter_current, 2);

    // Scanline 1: counter 2 -> 1
    vdp.scanline(1);
    assert!(!vdp.should_trigger_hint());

    // Scanline 2: counter 1 -> 0
    vdp.scanline(2);
    assert!(!vdp.should_trigger_hint()); // Not yet

    // Scanline 3: counter 0 -> trigger and reload
    vdp.scanline(3);
    assert!(vdp.should_trigger_hint());

    // Acknowledge HINT
    vdp.ack_hint();
    assert!(!vdp.should_trigger_hint());
}

#[test]
fn test_hint_disabled_when_register_clear() {
    let mut vdp = Vdp::new();

    // HINT disabled (register 0 bit 4 = 0)
    vdp.set_test_register(0, 0x04);

    // Set HINT counter to trigger immediately
    vdp.set_test_register(10, 0);
    vdp.timing.h_counter_current = 0;

    // Process scanlines
    vdp.scanline(1);

    // HINT should not trigger (disabled)
    assert!(!vdp.should_trigger_hint());
}
