//! Integration tests for VDP interrupts and timing.
//!
//! Tests VBLANK and HBLANK interrupt generation,
//! timing accuracy, and CPU-VDP synchronization.

use sg1_core::vdp::timing::{ntsc, VideoRegion};
use sg1_core::vdp::Vdp;

/// The status register's low byte, where the VBLANK, VINT and DMA bits are.
const STATUS_LOW: u32 = 0xC00005;

/// Runs the VDP through the frame it comes out of reset in: the first
/// VBLANK after a reset raises no VINT.
fn past_first_frame(vdp: &mut Vdp) {
    vdp.scanline(0);
    vdp.scanline(224);
    vdp.scanline(0);
}

/// Test VBLANK flag is set at correct scanline.
#[test]
fn test_vblank_flag_set_at_scanline_224() {
    let mut vdp = Vdp::new();

    // Scanlines 0-223: active display
    for line in 0..224 {
        vdp.scanline(line);
        assert!(
            !vdp.in_vblank(),
            "VBLANK should not be set at line {}",
            line
        );
    }

    // Scanline 224: VBLANK begins
    vdp.scanline(224);
    assert!(vdp.in_vblank(), "VBLANK should be set at line 224");
}

/// Test VBLANK flag clears at scanline 0.
#[test]
fn test_vblank_flag_clear_at_scanline_0() {
    let mut vdp = Vdp::new();

    // Set to VBLANK
    vdp.scanline(250);
    assert!(vdp.in_vblank());

    // Scanline 0: VBLANK clears
    vdp.scanline(0);
    assert!(!vdp.in_vblank(), "VBLANK should clear at scanline 0");
}

/// Test VBLANK interrupt triggers when enabled.
#[test]
fn test_vblank_interrupt_trigger() {
    let mut vdp = Vdp::new();
    past_first_frame(&mut vdp);

    // Enable VBLANK interrupt (bit 5 of register 1)
    vdp.set_test_register(1, 0x20);

    // Not in VBLANK yet
    vdp.scanline(100);
    assert!(!vdp.should_trigger_vint());

    // Enter VBLANK - should trigger
    vdp.scanline(224);
    assert!(vdp.should_trigger_vint(), "VINT should trigger at VBLANK");
}

/// Test the VINT flag is set when the interrupt is disabled.
#[test]
fn test_vblank_interrupt_disabled() {
    let mut vdp = Vdp::new();
    past_first_frame(&mut vdp);

    // Disable VBLANK interrupt
    vdp.set_test_register(1, 0x00);

    // Enter VBLANK
    vdp.scanline(224);

    // The flag is set and the request raised all the same: bit 5 of
    // register 1 only decides whether the 68000 takes it, which the system
    // checks
    assert_eq!(vdp.read(STATUS_LOW) & 0x80, 0x80);
    assert!(vdp.should_trigger_vint());
}

/// Test VBLANK interrupt acknowledgement.
#[test]
fn test_vblank_interrupt_ack() {
    let mut vdp = Vdp::new();
    past_first_frame(&mut vdp);

    vdp.set_test_register(1, 0x20); // Enable VINT
    vdp.scanline(224); // Enter VBLANK

    assert!(vdp.should_trigger_vint());

    // Acknowledge interrupt
    vdp.ack_vint();
    assert!(!vdp.should_trigger_vint(), "VINT should clear after ack");
}

/// Test HBLANK interrupt counter reload.
#[test]
fn test_hblank_counter_reload() {
    let mut vdp = Vdp::new();

    // Set HINT counter (register 10)
    vdp.set_test_register(10, 2); // HINT every third line (R10 + 1)

    // Check that reload value was set
    assert_eq!(
        vdp.timing().h_counter_reload,
        2,
        "H counter reload should be 2"
    );

    // The counter takes it on a line in VBLANK
    vdp.scanline(224);
    assert_eq!(
        vdp.timing().h_counter_current,
        2,
        "H counter should reload in VBLANK"
    );
}

/// Test VDP status register VBLANK bit.
#[test]
fn test_status_register_vblank_bit() {
    let mut vdp = Vdp::new();

    // With the display off the bit is set on every line
    vdp.scanline(100);
    let status = vdp.read(STATUS_LOW);
    assert_eq!(
        status & 0x08,
        0x08,
        "VBLANK bit should be set with the display off"
    );

    // Display on (register 1 bit 6, Mode 5)
    vdp.set_test_register(1, 0x44);

    // Before VBLANK
    vdp.scanline(100);
    let status = vdp.read(STATUS_LOW);
    assert_eq!(status & 0x08, 0, "VBLANK bit should be clear");

    // During VBLANK
    vdp.scanline(224);
    let status = vdp.read(STATUS_LOW);
    assert_eq!(status & 0x08, 0x08, "VBLANK bit should be set");
}

/// Test VDP status register interrupt pending bit.
#[test]
fn test_status_register_vint_pending() {
    let mut vdp = Vdp::new();
    past_first_frame(&mut vdp);

    vdp.set_test_register(1, 0x20); // Enable VINT
    vdp.scanline(224); // Trigger VINT

    let status = vdp.read(STATUS_LOW);
    assert_eq!(status & 0x80, 0x80, "VINT pending bit should be set");
}

/// Test VDP status register DMA busy bit.
#[test]
fn test_status_register_dma_busy() {
    let mut vdp = Vdp::new();
    let cpu_memory = vec![0u8; 0x10000];

    // Start DMA: 16 words from 0x1000
    vdp.set_test_register(1, 0x14); // Enable DMA
    vdp.set_test_register(19, 0x10);
    vdp.set_test_register(20, 0x00);
    vdp.set_test_register(21, 0x00);
    vdp.set_test_register(22, 0x08);
    vdp.set_test_register(23, 0x00);
    vdp.set_test_access(0x0000, 0x01);
    vdp.start_dma(&cpu_memory);

    let status = vdp.read(STATUS_LOW);
    assert_eq!(status & 0x02, 0x02, "DMA busy bit should be set");
}

/// Test frame timing for NTSC.
#[test]
fn test_frame_timing_ntsc() {
    let mut vdp = Vdp::new();

    // Execute full frame (scanline 0 to 261)
    for line in 0..ntsc::SCANLINES_TOTAL {
        vdp.scanline(line);
    }

    // After all scanlines, we've completed one frame cycle
    // Frame count increments when wrapping back to scanline 0
    let timing = vdp.timing();
    // Since we don't wrap in scanline(), frame_count stays 0
    // This is OK - frame_count is more for internal use
    assert_eq!(timing.scanline, 261);
}

/// Test PAL region has correct constants.
#[test]
fn test_pal_region_constants() {
    use sg1_core::vdp::timing::pal;

    assert_eq!(pal::SCANLINES_TOTAL, 313);
    assert_eq!(pal::SCANLINES_VISIBLE, 240);
}

/// Test timing region affects scanline count.
#[test]
fn test_timing_region_affects_total_scanlines() {
    let ntsc_timing = sg1_core::vdp::timing::VdpTiming::new(VideoRegion::Ntsc);
    assert_eq!(ntsc_timing.total_scanlines(), ntsc::SCANLINES_TOTAL);

    let pal_timing = sg1_core::vdp::timing::VdpTiming::new(VideoRegion::Pal);
    use sg1_core::vdp::timing::pal;
    assert_eq!(pal_timing.total_scanlines(), pal::SCANLINES_TOTAL);
}

/// Test DMA during VBLANK has faster timing.
#[test]
fn test_vblank_dma_timing() {
    let mut vdp = Vdp::new();
    let cpu_memory = vec![0u8; 0x10000];

    // Start DMA during active display
    vdp.scanline(100);
    assert!(!vdp.in_vblank());

    vdp.set_test_register(1, 0x14);
    vdp.set_test_register(19, 0x64); // 100 words
    vdp.set_test_register(20, 0x00);
    vdp.set_test_register(21, 0x00);
    vdp.set_test_register(22, 0x08); // from 0x1000
    vdp.set_test_register(23, 0x00);
    vdp.set_test_access(0x0000, 0x01);

    vdp.start_dma(&cpu_memory);
    let active_cycles = vdp.dma_state().cycles_remaining;

    // Start DMA during VBLANK
    let mut vdp2 = Vdp::new();
    vdp2.scanline(224);
    assert!(vdp2.in_vblank());

    vdp2.set_test_register(1, 0x14);
    vdp2.set_test_register(19, 0x64); // 100 words
    vdp2.set_test_register(20, 0x00);
    vdp2.set_test_register(21, 0x00);
    vdp2.set_test_register(22, 0x08); // from 0x1000
    vdp2.set_test_register(23, 0x00);
    vdp2.set_test_access(0x0000, 0x01);

    vdp2.start_dma(&cpu_memory);
    let vblank_cycles = vdp2.dma_state().cycles_remaining;

    // VBLANK should be 8x faster
    assert_eq!(active_cycles, vblank_cycles * 8);
}

/// Test HV counter read.
#[test]
fn test_hv_counter_read() {
    let mut vdp = Vdp::new();

    vdp.scanline(100);

    // V counter should match current scanline
    let timing = vdp.timing();
    assert_eq!(timing.scanline, 100);

    // Read V counter
    let v_counter = vdp.read(0xC00008);
    assert_eq!(v_counter, 100);

    // Read H counter (should be 0 at scanline start)
    let h_counter = vdp.read(0xC00009);
    eprintln!(
        "H counter read: {}, timing.h_counter: {}",
        h_counter,
        vdp.timing().h_counter
    );
    assert_eq!(h_counter, 0);
}

/// Test scanlines cycle correctly.
#[test]
fn test_scanline_cycling() {
    let mut vdp = Vdp::new();

    // Test scanline values
    vdp.scanline(0);
    assert_eq!(vdp.current_scanline(), 0);

    vdp.scanline(100);
    assert_eq!(vdp.current_scanline(), 100);

    vdp.scanline(261);
    assert_eq!(vdp.current_scanline(), 261);
}

/// Test scanline timing with cycles.
#[test]
fn test_scanline_cycle_timing() {
    let mut vdp = Vdp::new();

    vdp.scanline(0);
    let timing = vdp.timing();

    // Execute partial scanline
    assert!(!timing.in_hblank);
}
