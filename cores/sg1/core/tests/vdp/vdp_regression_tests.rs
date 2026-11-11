//! Regression tests for VDP.
//!
//! Ensures that VDP functionality doesn't break with new changes.

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

/// Sets a DMA of 16 words from 68000 memory at 0x1000 to VRAM 0x0000.
fn setup_dma(vdp: &mut Vdp) {
    vdp.set_test_register(1, 0x14); // Enable DMA
    vdp.set_test_register(19, 0x10);
    vdp.set_test_register(20, 0x00);
    vdp.set_test_register(21, 0x00);
    vdp.set_test_register(22, 0x08);
    vdp.set_test_register(23, 0x00);
    vdp.set_test_access(0x0000, 0x01);
}

/// Test complete VDP initialization.
#[test]
fn test_vdp_initialization() {
    let vdp = Vdp::new();

    // Display defaults
    assert_eq!(vdp.display_width(), 320); // H40
    assert_eq!(vdp.display_height(), 224); // V28

    // Timing defaults: the VDP comes out of reset on the first VBLANK line
    assert_eq!(vdp.current_scanline(), 224);
    assert!(vdp.in_vblank());

    // DMA idle
    assert!(!vdp.is_dma_active());

    // Framebuffer allocated
    assert_eq!(vdp.framebuffer().len(), 320 * 224 * 4);
}

/// Test VDP reset.
#[test]
fn test_vdp_reset() {
    let mut vdp = Vdp::new();

    // Modify state
    vdp.scanline(100);
    vdp.set_test_register(1, 0xFF);
    vdp.set_test_register(12, 0xFF);

    // Reset
    vdp.reset();

    // Check defaults restored
    let regs = vdp.registers();
    assert_eq!(regs[0], 0x04);
    assert_eq!(regs[1], 0x04);
    assert_eq!(regs[12], 0x81);
    assert_eq!(regs[15], 0x02);
}

/// Test register write/read.
#[test]
fn test_vdp_register_access() {
    let mut vdp = Vdp::new();

    // Write registers
    for i in 0..24 {
        vdp.set_test_register(i, i as u8);
    }

    // Read back
    let regs = vdp.registers();
    for (i, &value) in regs.iter().enumerate() {
        assert_eq!(value, i as u8);
    }
}

/// Test VRAM access doesn't crash.
#[test]
fn test_vram_access_no_crash() {
    let mut vdp = Vdp::new();

    // Set VRAM write mode
    vdp.set_test_access(0x0000, 0x01);

    // Write pattern
    for i in 0..1024 {
        vdp.write(0xC00000, (i & 0xFF) as u8);
    }

    // No crash = success
}

/// Test CRAM access doesn't crash.
#[test]
fn test_cram_access_no_crash() {
    let mut vdp = Vdp::new();

    // Set CRAM write mode
    vdp.set_test_access(0x0000, 0x03);

    // Write colors
    for i in 0..64 {
        vdp.write(0xC00000, ((i * 16) >> 8) as u8);
        vdp.write(0xC00000, ((i * 16) & 0xFF) as u8);
    }

    // No crash = success
}

/// Test scanline progression.
#[test]
fn test_scanline_progression() {
    let mut vdp = Vdp::new();

    // Active display
    for line in 0..224 {
        vdp.scanline(line);
        assert!(!vdp.in_vblank());
        assert_eq!(vdp.current_scanline(), line);
    }

    // VBLANK
    for line in 224..262 {
        vdp.scanline(line);
        assert!(vdp.in_vblank());
    }
}

/// Test VINT triggering.
#[test]
fn test_vint_basic_flow() {
    let mut vdp = Vdp::new();
    past_first_frame(&mut vdp);

    // Enable VINT
    vdp.set_test_register(1, 0x20);

    // Not in VBLANK
    vdp.scanline(100);
    assert!(!vdp.should_trigger_vint());

    // Enter VBLANK
    vdp.scanline(224);
    assert!(vdp.should_trigger_vint());

    // Acknowledge
    vdp.ack_vint();
    assert!(!vdp.should_trigger_vint());
}

/// Test DMA basic flow.
#[test]
fn test_dma_basic_flow() {
    let mut vdp = Vdp::new();
    let cpu_memory = vec![0u8; 0x10000];

    setup_dma(&mut vdp);

    // Start DMA
    vdp.start_dma(&cpu_memory);
    assert!(vdp.is_dma_active());

    // Execute until complete
    vdp.execute_dma_cycles(10000);
    assert!(!vdp.is_dma_active());
}

/// Test display mode switching.
#[test]
fn test_display_mode_switching() {
    let mut vdp = Vdp::new();

    // H40 V28 (default)
    assert_eq!(vdp.display_width(), 320);
    assert_eq!(vdp.display_height(), 224);

    // H32
    vdp.set_test_register(12, 0x80);
    assert_eq!(vdp.display_width(), 256);

    // V30
    vdp.set_test_register(1, 0x08);
    assert_eq!(vdp.display_height(), 240);

    // Back to H40
    vdp.set_test_register(12, 0x81);
    assert_eq!(vdp.display_width(), 320);
}

/// Test status register bits.
#[test]
fn test_status_register_bits() {
    let mut vdp = Vdp::new();

    // VBLANK bit, with the display on
    vdp.set_test_register(1, 0x44);
    vdp.scanline(100);
    let status1 = vdp.read(STATUS_LOW);
    assert_eq!(status1 & 0x08, 0);

    vdp.scanline(224);
    let status2 = vdp.read(STATUS_LOW);
    assert_eq!(status2 & 0x08, 0x08);
}

/// Test HV counter.
#[test]
fn test_hv_counter_reads() {
    let mut vdp = Vdp::new();

    vdp.scanline(150);

    // V counter
    let v_count = vdp.read(0xC00008);
    assert_eq!(v_count, 150);

    // H counter (should be 0 at scanline start)
    let h_count = vdp.read(0xC00009);
    assert_eq!(h_count, 0);
}

/// Test multiple register writes.
#[test]
fn test_multiple_register_writes() {
    let mut vdp = Vdp::new();

    // Write multiple registers
    for i in 0..24 {
        vdp.set_test_register(i, (i * 10) as u8);
    }

    // Verify all were written
    let regs = vdp.registers();
    for (i, &value) in regs.iter().enumerate() {
        assert_eq!(value, (i * 10) as u8);
    }
}

/// Test VDP with all layers disabled (backdrop only).
#[test]
fn test_backdrop_only_rendering() {
    let mut vdp = Vdp::new();

    // Set backdrop color (register 7)
    vdp.set_test_register(7, 0x0F); // Palette 0, color 15

    // Render should not crash with no planes/sprites
    vdp.scanline(0);
    vdp.scanline(100);
    vdp.scanline(223);

    // Success = no crash
}

/// Test timing state consistency.
#[test]
fn test_timing_state_consistency() {
    let mut vdp = Vdp::new();

    // Progress through frame
    for line in 0..262 {
        vdp.scanline(line);

        let timing = vdp.timing();
        assert_eq!(timing.scanline, line);

        // VBLANK should be consistent
        if line < 224 {
            assert!(!timing.in_vblank);
            assert!(!vdp.in_vblank());
        } else {
            assert!(timing.in_vblank);
            assert!(vdp.in_vblank());
        }
    }
}

/// Test DMA during different scanline periods.
#[test]
fn test_dma_timing_at_different_scanlines() {
    let cpu_memory = vec![0u8; 0x10000];

    // During active display
    let mut vdp1 = Vdp::new();
    vdp1.scanline(50);
    setup_dma(&mut vdp1);
    vdp1.start_dma(&cpu_memory);
    let active_cycles = vdp1.dma_state().cycles_remaining;

    // During VBLANK
    let mut vdp2 = Vdp::new();
    vdp2.scanline(230);
    setup_dma(&mut vdp2);
    vdp2.start_dma(&cpu_memory);
    let vblank_cycles = vdp2.dma_state().cycles_remaining;

    // VBLANK should be faster
    assert!(vblank_cycles < active_cycles);
}

/// Test framebuffer remains valid size after operations.
#[test]
fn test_framebuffer_size_stability() {
    let mut vdp = Vdp::new();

    // Initial size
    let initial_size = vdp.framebuffer().len();
    assert_eq!(initial_size, 320 * 224 * 4);

    // After various operations
    vdp.scanline(100);
    vdp.set_test_register(7, 0x3F);
    vdp.reset();
    vdp.scanline(224);

    // Size should remain consistent
    assert_eq!(vdp.framebuffer().len(), initial_size);
}

/// Test no panic on invalid register numbers.
#[test]
fn test_register_bounds_checking() {
    let mut vdp = Vdp::new();

    // Valid registers (0-23)
    vdp.set_test_register(0, 0xFF);
    vdp.set_test_register(23, 0xFF);

    // This shouldn't panic (should be ignored or wrapped)
    vdp.set_test_register(24, 0xFF);
    vdp.set_test_register(100, 0xFF);

    // Success = no panic
}

/// Test complete VDP lifecycle.
#[test]
fn test_vdp_complete_lifecycle() {
    let mut vdp = Vdp::new();
    let cpu_memory = vec![0u8; 0x10000];

    // Initialize: out of reset, in VBLANK
    assert!(!vdp.is_dma_active());
    assert!(vdp.in_vblank());

    // Run a frame
    for line in 0..262 {
        vdp.scanline(line);

        // Check VBLANK transition
        if line == 224 {
            assert!(vdp.in_vblank());
        }
    }

    // Start DMA
    setup_dma(&mut vdp);
    vdp.start_dma(&cpu_memory);
    assert!(vdp.is_dma_active());

    // Complete DMA
    vdp.execute_dma_cycles(1000);
    assert!(!vdp.is_dma_active());

    // Reset: back on the first VBLANK line
    vdp.reset();
    assert_eq!(vdp.current_scanline(), 224);
    assert!(vdp.in_vblank());

    // Success
}
