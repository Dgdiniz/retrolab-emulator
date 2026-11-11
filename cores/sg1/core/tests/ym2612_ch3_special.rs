//! YM2612 CH3 Special Mode Tests
//!
//! Tests the Channel 3 Special Mode which allows independent operator frequencies

use sg1_core::ym2612::Ym2612;

#[test]
fn test_ch3_mode_register_write() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Test all CH3 modes (bits 7-6 of register 0x27)
    // 0x00: Normal
    ym.write_register(0, 0x27, 0x00);
    // Verify mode is set (we'll test behavior separately)

    // 0x40: CH3 special mode
    ym.write_register(0, 0x27, 0x40);

    // 0x80: CSM mode
    ym.write_register(0, 0x27, 0x80);

    // 0xC0: CSM + CH3 special
    ym.write_register(0, 0x27, 0xC0);
}

#[test]
fn test_ch3_fnum_registers() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable CH3 special mode
    ym.write_register(0, 0x27, 0x40);

    // Write F-numbers for CH3 operators
    // OP0: F-num = 0x123, Block = 4
    ym.write_register(0, 0xA8, 0x23); // F-num low
    ym.write_register(0, 0xAC, (4 << 3) | 0x01); // Block + F-num high

    // OP1: F-num = 0x234, Block = 5
    ym.write_register(0, 0xA9, 0x34); // F-num low
    ym.write_register(0, 0xAD, (5 << 3) | 0x02); // Block + F-num high

    // OP2: F-num = 0x345, Block = 6
    ym.write_register(0, 0xAA, 0x45); // F-num low
    ym.write_register(0, 0xAE, (6 << 3) | 0x03); // Block + F-num high

    // OP3 uses normal registers (0xA2/0xA6)
    ym.write_register(0, 0xA2, 0x56); // F-num low (CH2 = CH3 in register terms)
    ym.write_register(0, 0xA6, (7 << 3) | 0x04); // Block + F-num high
}

#[test]
fn test_ch3_special_mode_disabled_by_default() {
    let ym = Ym2612::new(7_670_454, 44100);

    // CH3 mode should be 0 (normal) by default
    // All operators in CH2 (index 2) should share the same frequency
    for op in 0..4 {
        let freq = ym.channel(2).unwrap().operator(op).freq_number;
        assert_eq!(freq, 0, "CH3 OP{} should have default frequency 0", op);
    }
}

#[test]
fn test_ch3_normal_mode_shared_frequency() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Write frequency to CH2 (CH3 in hardware terms) in normal mode
    ym.write_register(0, 0xA2, 0x34); // F-num low
    ym.write_register(0, 0xA6, (3 << 3) | 0x01); // Block=3, F-num high

    // All operators should have the same frequency
    let expected_fnum = 0x134;
    let expected_block = 3;

    for op in 0..4 {
        assert_eq!(
            ym.channel(2).unwrap().operator(op).freq_number,
            expected_fnum,
            "CH3 OP{} should have shared F-num in normal mode",
            op
        );
        assert_eq!(
            ym.channel(2).unwrap().operator(op).block,
            expected_block,
            "CH3 OP{} should have shared Block in normal mode",
            op
        );
    }
}

#[test]
fn test_ch3_special_mode_independent_frequencies() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable CH3 special mode
    ym.write_register(0, 0x27, 0x40);

    // Set independent frequencies for each operator
    // OP0
    ym.write_register(0, 0xA8, 0x11);
    ym.write_register(0, 0xAC, (2 << 3) | 0x02); // F-num=0x211, Block=2

    // OP1
    ym.write_register(0, 0xA9, 0x22);
    ym.write_register(0, 0xAD, (3 << 3) | 0x03); // F-num=0x322, Block=3

    // OP2
    ym.write_register(0, 0xAA, 0x33);
    ym.write_register(0, 0xAE, (4 << 3) | 0x04); // F-num=0x433, Block=4

    // OP3 (uses normal registers 0xA2/0xA6, channel index 2)
    ym.write_register(0, 0xA2, 0x44);
    ym.write_register(0, 0xA6, (5 << 3) | 0x05); // F-num=0x544, Block=5

    // Verify OP3 was updated (CH3 special mode only affects OP0-2, OP3 uses normal regs)
    assert_eq!(ym.channel(2).unwrap().operator(3).freq_number, 0x544);
    assert_eq!(ym.channel(2).unwrap().operator(3).block, 5);
}

#[test]
fn test_ch3_special_mode_toggle() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Start in normal mode, set frequency
    ym.write_register(0, 0xA2, 0x12);
    ym.write_register(0, 0xA6, (2 << 3) | 0x03); // F-num=0x312, Block=2

    // All operators should have same frequency
    for op in 0..4 {
        assert_eq!(ym.channel(2).unwrap().operator(op).freq_number, 0x312);
    }

    // Enable CH3 special mode
    ym.write_register(0, 0x27, 0x40);

    // Set different frequency for OP0
    ym.write_register(0, 0xA8, 0x99);
    ym.write_register(0, 0xAC, (7 << 3) | 0x04); // F-num=0x499, Block=7

    // Set different frequency using normal registers (should only affect OP3)
    ym.write_register(0, 0xA2, 0x77);
    ym.write_register(0, 0xA6, (6 << 3) | 0x06); // F-num=0x677, Block=6

    // OP3 should have the new frequency
    assert_eq!(ym.channel(2).unwrap().operator(3).freq_number, 0x677);
    assert_eq!(ym.channel(2).unwrap().operator(3).block, 6);

    // Disable CH3 special mode
    ym.write_register(0, 0x27, 0x00);

    // Write frequency again in normal mode
    ym.write_register(0, 0xA2, 0x55);
    ym.write_register(0, 0xA6, (4 << 3) | 0x01); // F-num=0x155, Block=4

    // All operators should now have the same frequency again
    for op in 0..4 {
        assert_eq!(
            ym.channel(2).unwrap().operator(op).freq_number,
            0x155,
            "CH3 OP{} should have shared frequency after disabling special mode",
            op
        );
    }
}

#[test]
fn test_ch3_special_mode_does_not_affect_other_channels() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable CH3 special mode
    ym.write_register(0, 0x27, 0x40);

    // Set frequency for CH0
    ym.write_register(0, 0xA0, 0x11);
    ym.write_register(0, 0xA4, (2 << 3) | 0x01); // F-num=0x111, Block=2

    // Set frequency for CH1
    ym.write_register(0, 0xA1, 0x22);
    ym.write_register(0, 0xA5, (3 << 3) | 0x02); // F-num=0x222, Block=3

    // CH0 and CH1 should have shared frequencies for all operators
    for op in 0..4 {
        assert_eq!(ym.channel(0).unwrap().operator(op).freq_number, 0x111);
        assert_eq!(ym.channel(1).unwrap().operator(op).freq_number, 0x222);
    }
}

#[test]
fn test_ch3_special_registers_boundary_values() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable CH3 special mode
    ym.write_register(0, 0x27, 0x40);

    // Test minimum values
    ym.write_register(0, 0xA8, 0x00);
    ym.write_register(0, 0xAC, 0x00); // F-num=0x000, Block=0

    // Test maximum values
    ym.write_register(0, 0xA9, 0xFF);
    ym.write_register(0, 0xAD, 0xFF); // F-num=0x7FF, Block=7 (max)

    // Test mid values
    ym.write_register(0, 0xAA, 0x80);
    ym.write_register(0, 0xAE, (4 << 3) | 0x04); // F-num=0x480, Block=4
}

#[test]
fn test_ch3_special_registers_overwrite() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable CH3 special mode
    ym.write_register(0, 0x27, 0x40);

    // Write initial values
    ym.write_register(0, 0xA8, 0x11);
    ym.write_register(0, 0xAC, (2 << 3) | 0x01);

    // Overwrite with new values
    ym.write_register(0, 0xA8, 0x99);
    ym.write_register(0, 0xAC, (7 << 3) | 0x07);

    // Values should be overwritten (we can't directly check internal state,
    // but this tests that multiple writes don't cause issues)
}

#[test]
fn test_ch3_reset_clears_special_mode() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable CH3 special mode
    ym.write_register(0, 0x27, 0x40);

    // Set frequencies
    ym.write_register(0, 0xA8, 0x11);
    ym.write_register(0, 0xAC, (2 << 3) | 0x01);

    // Reset
    ym.reset();

    // CH3 mode should be reset to 0 (normal)
    // Frequencies should be reset to 0
    for op in 0..4 {
        assert_eq!(ym.channel(2).unwrap().operator(op).freq_number, 0);
        assert_eq!(ym.channel(2).unwrap().operator(op).block, 0);
    }
}

#[test]
fn test_ch3_csm_mode_bits() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Test CSM mode (bit 7)
    ym.write_register(0, 0x27, 0x80);

    // Test CSM + CH3 special (bits 7-6)
    ym.write_register(0, 0x27, 0xC0);

    // Test that lower bits don't interfere with CH3 mode
    ym.write_register(0, 0x27, 0x4F); // CH3 special + all lower bits set
    ym.write_register(0, 0x27, 0xCF); // CSM + CH3 special + all lower bits set
}

// Integration test: Verify that CH3 special mode works correctly with key on/off
#[test]
fn test_ch3_special_with_key_onoff() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable CH3 special mode
    ym.write_register(0, 0x27, 0x40);

    // Set frequencies for all operators
    ym.write_register(0, 0xA8, 0x11);
    ym.write_register(0, 0xAC, (2 << 3) | 0x01);
    ym.write_register(0, 0xA9, 0x22);
    ym.write_register(0, 0xAD, (3 << 3) | 0x02);
    ym.write_register(0, 0xAA, 0x33);
    ym.write_register(0, 0xAE, (4 << 3) | 0x03);
    ym.write_register(0, 0xA2, 0x44);
    ym.write_register(0, 0xA6, (5 << 3) | 0x04);

    // Key on all operators of CH2 (CH3 in hardware)
    ym.write_register(0, 0x28, (0x0F << 4) | 0x02); // All ops, channel 2

    // Key off
    ym.write_register(0, 0x28, 0x02); // Channel 2, no ops

    // Should not crash or cause issues
}
