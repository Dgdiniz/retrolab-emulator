//! YM2612 SSG-EG (Software-controlled Envelope Generator) Tests
//!
//! Tests the 16 SSG-EG modes

use sg1_core::ym2612::Ym2612;

#[test]
fn test_ssg_eg_register_write() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Write SSG-EG values to channel 0, operator 0 (register 0x90)
    // Register 0x90: bits 3-0 = SSG-EG mode
    for mode in 0..=15 {
        ym.write_register(0, 0x90, mode);

        // Verify SSG-EG mode was set correctly
        let envelope = ym.channel(0).unwrap().operator(0).envelope();
        assert_eq!(
            envelope.get_ssg_eg(),
            mode,
            "SSG-EG mode should be {} for register value 0x{:02X}",
            mode,
            mode
        );
    }
}

#[test]
fn test_ssg_eg_enable_bit() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // SSG-EG disabled (bit 3 = 0)
    ym.write_register(0, 0x90, 0b0000); // Mode 0, disabled
    assert!(!ym
        .channel(0)
        .unwrap()
        .operator(0)
        .envelope()
        .is_ssg_eg_enabled());

    ym.write_register(0, 0x90, 0b0111); // Mode 7, disabled
    assert!(!ym
        .channel(0)
        .unwrap()
        .operator(0)
        .envelope()
        .is_ssg_eg_enabled());

    // SSG-EG enabled (bit 3 = 1)
    ym.write_register(0, 0x90, 0b1000); // Mode 8, enabled
    assert!(ym
        .channel(0)
        .unwrap()
        .operator(0)
        .envelope()
        .is_ssg_eg_enabled());

    ym.write_register(0, 0x90, 0b1111); // Mode 15, enabled
    assert!(ym
        .channel(0)
        .unwrap()
        .operator(0)
        .envelope()
        .is_ssg_eg_enabled());
}

#[test]
fn test_ssg_eg_all_modes() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Test all 16 modes (0x0 to 0xF)
    let modes = [
        // Disabled modes (bit 3 = 0)
        (0x0, false, "Disabled"),
        (0x1, false, "Disabled + Hold"),
        (0x2, false, "Disabled + Attack"),
        (0x3, false, "Disabled + Attack + Hold"),
        (0x4, false, "Disabled + Alternate"),
        (0x5, false, "Disabled + Alternate + Hold"),
        (0x6, false, "Disabled + Alternate + Attack"),
        (0x7, false, "Disabled + All bits"),
        // Enabled modes (bit 3 = 1)
        (0x8, true, "\\\\\\\\_ (attack-decay-off)"),
        (0x9, true, "\\\\\\\\\\\\ (attack-hold)"),
        (0xA, true, "\\/\\/\\/ (alternating)"),
        (0xB, true, "\\/--- (attack-inv-hold)"),
        (0xC, true, "/_/_ (inv-alternating)"),
        (0xD, true, "/---- (inv-hold)"),
        (0xE, true, "/\\/\\ (inv-alternating-2)"),
        (0xF, true, "/\\--- (inv-attack-hold)"),
    ];

    for (mode, should_be_enabled, description) in modes {
        ym.write_register(0, 0x90, mode);

        let envelope = ym.channel(0).unwrap().operator(0).envelope();
        assert_eq!(
            envelope.get_ssg_eg(),
            mode,
            "Mode 0x{:X} ({})",
            mode,
            description
        );
        assert_eq!(
            envelope.is_ssg_eg_enabled(),
            should_be_enabled,
            "Mode 0x{:X} ({}) enable state",
            mode,
            description
        );
    }
}

#[test]
fn test_ssg_eg_all_operators() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Set different SSG-EG modes for each operator in channel 0
    for op_idx in 0..4 {
        let mode = 0x8 | (op_idx as u8); // Modes 0x8, 0x9, 0xA, 0xB
        let reg = 0x90 | (op_idx * 4) as u8; // 0x90, 0x94, 0x98, 0x9C

        ym.write_register(0, reg, mode);

        assert_eq!(
            ym.channel(0)
                .unwrap()
                .operator(op_idx)
                .envelope()
                .get_ssg_eg(),
            mode,
            "Operator {} should have SSG-EG mode 0x{:X}",
            op_idx,
            mode
        );
    }
}

#[test]
fn test_ssg_eg_default_value() {
    let ym = Ym2612::new(7_670_454, 44100);

    // All operators should start with SSG-EG disabled
    for channel in 0..6 {
        for op in 0..4 {
            let envelope = ym.channel(channel).unwrap().operator(op).envelope();
            assert_eq!(
                envelope.get_ssg_eg(),
                0,
                "Channel {} operator {} should have default SSG-EG=0",
                channel,
                op
            );
            assert!(
                !envelope.is_ssg_eg_enabled(),
                "Channel {} operator {} should have SSG-EG disabled by default",
                channel,
                op
            );
        }
    }
}

#[test]
fn test_ssg_eg_mask() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Write value with bits set outside SSG-EG range
    ym.write_register(0, 0x90, 0xFF); // All bits set

    // SSG-EG should be masked to 4 bits (0-15)
    assert!(
        ym.channel(0).unwrap().operator(0).envelope().get_ssg_eg() <= 15,
        "SSG-EG should be masked to 0-15"
    );
}

#[test]
fn test_ssg_eg_independent_per_operator() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Set SSG-EG=0xA on operator 0
    ym.write_register(0, 0x90, 0xA);

    // Set SSG-EG=0xC on operator 1
    ym.write_register(0, 0x94, 0xC);

    // Verify independence
    assert_eq!(
        ym.channel(0).unwrap().operator(0).envelope().get_ssg_eg(),
        0xA
    );
    assert_eq!(
        ym.channel(0).unwrap().operator(1).envelope().get_ssg_eg(),
        0xC
    );
    assert_eq!(
        ym.channel(0).unwrap().operator(2).envelope().get_ssg_eg(),
        0
    ); // Unchanged
}

#[test]
fn test_ssg_eg_all_channels() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Test each channel individually with different SSG-EG modes
    // Channel 0, operator 0: register 0x90 (part 0, ch 0, op 0)
    ym.write_register(0, 0x90, 0x08);
    assert_eq!(
        ym.channel(0).unwrap().operator(0).envelope().get_ssg_eg(),
        0x08,
        "Channel 0, op 0 should have SSG-EG=0x8"
    );

    // Channel 1, operator 0: register 0x91 (part 0, ch 1, op 0)
    ym.write_register(0, 0x91, 0x09);
    assert_eq!(
        ym.channel(1).unwrap().operator(0).envelope().get_ssg_eg(),
        0x09,
        "Channel 1, op 0 should have SSG-EG=0x9"
    );

    // Channel 2, operator 0: register 0x92 (part 0, ch 2, op 0)
    ym.write_register(0, 0x92, 0x0A);
    assert_eq!(
        ym.channel(2).unwrap().operator(0).envelope().get_ssg_eg(),
        0x0A,
        "Channel 2, op 0 should have SSG-EG=0xA"
    );

    // Channel 3, operator 0: register 0x90 (part 1, ch 0, op 0)
    ym.write_register(1, 0x90, 0x0B);
    assert_eq!(
        ym.channel(3).unwrap().operator(0).envelope().get_ssg_eg(),
        0x0B,
        "Channel 3, op 0 should have SSG-EG=0xB"
    );

    // Channel 4, operator 0: register 0x91 (part 1, ch 1, op 0)
    ym.write_register(1, 0x91, 0x0C);
    assert_eq!(
        ym.channel(4).unwrap().operator(0).envelope().get_ssg_eg(),
        0x0C,
        "Channel 4, op 0 should have SSG-EG=0xC"
    );

    // Channel 5, operator 0: register 0x92 (part 1, ch 2, op 0)
    ym.write_register(1, 0x92, 0x0D);
    assert_eq!(
        ym.channel(5).unwrap().operator(0).envelope().get_ssg_eg(),
        0x0D,
        "Channel 5, op 0 should have SSG-EG=0xD"
    );
}

#[test]
fn test_register_0x90_combined() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Register 0x90: bits 3-0 = SSG-EG mode
    // Set mode 0xA (alternating mode)
    ym.write_register(0, 0x90, 0b00001010);

    let envelope = ym.channel(0).unwrap().operator(0).envelope();
    assert_eq!(envelope.get_ssg_eg(), 0xA, "SSG-EG mode should be 0xA");
    assert!(envelope.is_ssg_eg_enabled(), "SSG-EG should be enabled");
}

// Additional rigorous edge case tests

#[test]
fn test_ssg_eg_all_channels_all_operators() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Test all 6 channels x 4 operators = 24 operators
    for channel in 0..6 {
        for op in 0..4 {
            let mode = 0x8 | ((channel + op) as u8 & 0x07);
            let part = if channel < 3 { 0 } else { 1 };
            let ch_offset = (channel % 3) as u8; // 0-2
            let op_offset = (op as u8) << 2; // 0, 4, 8, 12
            let reg = 0x90 + op_offset + ch_offset;

            ym.write_register(part, reg, mode);

            assert_eq!(
                ym.channel(channel)
                    .unwrap()
                    .operator(op)
                    .envelope()
                    .get_ssg_eg(),
                mode,
                "Channel {}, operator {} should have SSG-EG=0x{:X}",
                channel,
                op,
                mode
            );
        }
    }
}

#[test]
fn test_ssg_eg_register_mapping() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Test register mapping for all operators in channel 0
    // 0x90, 0x94, 0x98, 0x9C = CH0 OP0-OP3: bits 3-2 pick the operator and
    // bits 1-0 the channel (in the chip's slot order, slots 1, 3, 2, 4)

    ym.write_register(0, 0x90, 0x08); // CH0 OP0
    ym.write_register(0, 0x94, 0x09); // CH0 OP1
    ym.write_register(0, 0x98, 0x0A); // CH0 OP2
    ym.write_register(0, 0x9C, 0x0B); // CH0 OP3

    assert_eq!(
        ym.channel(0).unwrap().operator(0).envelope().get_ssg_eg(),
        0x08,
        "CH0 OP0"
    );
    assert_eq!(
        ym.channel(0).unwrap().operator(1).envelope().get_ssg_eg(),
        0x09,
        "CH0 OP1"
    );
    assert_eq!(
        ym.channel(0).unwrap().operator(2).envelope().get_ssg_eg(),
        0x0A,
        "CH0 OP2"
    );
    assert_eq!(
        ym.channel(0).unwrap().operator(3).envelope().get_ssg_eg(),
        0x0B,
        "CH0 OP3"
    );
}

#[test]
fn test_ssg_eg_mode_boundaries() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Test all 16 modes (0x0 to 0xF) on a single operator
    for mode in 0x0..=0xF {
        ym.write_register(0, 0x90, mode);

        assert_eq!(
            ym.channel(0).unwrap().operator(0).envelope().get_ssg_eg(),
            mode,
            "SSG-EG mode 0x{:X} should be stored correctly",
            mode
        );

        let enabled = (mode & 0x08) != 0;
        assert_eq!(
            ym.channel(0)
                .unwrap()
                .operator(0)
                .envelope()
                .is_ssg_eg_enabled(),
            enabled,
            "SSG-EG mode 0x{:X} enable bit should be {}",
            mode,
            enabled
        );
    }
}

#[test]
fn test_ssg_eg_overwrite() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Write mode 0xA
    ym.write_register(0, 0x90, 0x0A);
    assert_eq!(
        ym.channel(0).unwrap().operator(0).envelope().get_ssg_eg(),
        0x0A
    );

    // Overwrite with mode 0x5
    ym.write_register(0, 0x90, 0x05);
    assert_eq!(
        ym.channel(0).unwrap().operator(0).envelope().get_ssg_eg(),
        0x05,
        "SSG-EG mode should be overwritten"
    );

    // Overwrite with mode 0xF
    ym.write_register(0, 0x90, 0x0F);
    assert_eq!(
        ym.channel(0).unwrap().operator(0).envelope().get_ssg_eg(),
        0x0F,
        "SSG-EG mode should be overwritten again"
    );
}

#[test]
fn test_ssg_eg_reset_clears_mode() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Set SSG-EG mode on multiple operators
    ym.write_register(0, 0x90, 0x0F);
    ym.write_register(0, 0x94, 0x0E);
    ym.write_register(0, 0x98, 0x0D);

    // Reset the system
    ym.reset();

    // All SSG-EG modes should be reset to 0
    for channel in 0..6 {
        for op in 0..4 {
            assert_eq!(
                ym.channel(channel)
                    .unwrap()
                    .operator(op)
                    .envelope()
                    .get_ssg_eg(),
                0,
                "After reset, CH{} OP{} SSG-EG should be 0",
                channel,
                op
            );
            assert!(
                !ym.channel(channel)
                    .unwrap()
                    .operator(op)
                    .envelope()
                    .is_ssg_eg_enabled(),
                "After reset, CH{} OP{} SSG-EG should be disabled",
                channel,
                op
            );
        }
    }
}

#[test]
fn test_ssg_eg_part0_vs_part1() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Set same register on both parts, should affect different channels
    ym.write_register(0, 0x90, 0x08); // CH0 (part 0, ch 0)
    ym.write_register(1, 0x90, 0x0C); // CH3 (part 1, ch 0)

    assert_eq!(
        ym.channel(0).unwrap().operator(0).envelope().get_ssg_eg(),
        0x08,
        "Part 0 register 0x90 affects channel 0"
    );
    assert_eq!(
        ym.channel(3).unwrap().operator(0).envelope().get_ssg_eg(),
        0x0C,
        "Part 1 register 0x90 affects channel 3"
    );

    // Channels 1 and 2 should be unaffected
    assert_eq!(
        ym.channel(1).unwrap().operator(0).envelope().get_ssg_eg(),
        0,
        "Channel 1 should be unaffected"
    );
    assert_eq!(
        ym.channel(2).unwrap().operator(0).envelope().get_ssg_eg(),
        0,
        "Channel 2 should be unaffected"
    );
}

#[test]
fn test_ssg_eg_operator_isolation() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Set SSG-EG on operator 1 of channel 0
    ym.write_register(0, 0x94, 0x0A);

    // Operator 1 should have the mode
    assert_eq!(
        ym.channel(0).unwrap().operator(1).envelope().get_ssg_eg(),
        0x0A
    );

    // Other operators should not be affected
    assert_eq!(
        ym.channel(0).unwrap().operator(0).envelope().get_ssg_eg(),
        0
    );
    assert_eq!(
        ym.channel(0).unwrap().operator(2).envelope().get_ssg_eg(),
        0
    );
    assert_eq!(
        ym.channel(0).unwrap().operator(3).envelope().get_ssg_eg(),
        0
    );
}

#[test]
fn test_ssg_eg_enable_disable_transitions() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Start disabled
    ym.write_register(0, 0x90, 0x00);
    assert!(!ym
        .channel(0)
        .unwrap()
        .operator(0)
        .envelope()
        .is_ssg_eg_enabled());

    // Enable with mode 0x8
    ym.write_register(0, 0x90, 0x08);
    assert!(ym
        .channel(0)
        .unwrap()
        .operator(0)
        .envelope()
        .is_ssg_eg_enabled());

    // Disable with mode 0x7
    ym.write_register(0, 0x90, 0x07);
    assert!(!ym
        .channel(0)
        .unwrap()
        .operator(0)
        .envelope()
        .is_ssg_eg_enabled());

    // Enable with mode 0xF
    ym.write_register(0, 0x90, 0x0F);
    assert!(ym
        .channel(0)
        .unwrap()
        .operator(0)
        .envelope()
        .is_ssg_eg_enabled());

    // Disable with mode 0x0
    ym.write_register(0, 0x90, 0x00);
    assert!(!ym
        .channel(0)
        .unwrap()
        .operator(0)
        .envelope()
        .is_ssg_eg_enabled());
}

#[test]
fn test_ssg_eg_channel_independence() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Set different SSG-EG modes on each channel
    let modes = [0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D];

    for (idx, &mode) in modes.iter().enumerate() {
        let part = if idx < 3 { 0 } else { 1 };
        let ch_offset = (idx % 3) as u8; // 0-2
        let reg = 0x90 + ch_offset; // OP0 for each channel
        ym.write_register(part, reg, mode);
    }

    // Verify each channel has its own mode
    for (idx, &expected_mode) in modes.iter().enumerate() {
        assert_eq!(
            ym.channel(idx).unwrap().operator(0).envelope().get_ssg_eg(),
            expected_mode,
            "Channel {} should have mode 0x{:X}",
            idx,
            expected_mode
        );
    }

    // Changing one should not affect others
    ym.write_register(0, 0x90, 0x0F); // Change channel 0

    assert_eq!(
        ym.channel(0).unwrap().operator(0).envelope().get_ssg_eg(),
        0x0F,
        "Channel 0 should be updated"
    );

    // Others should remain unchanged
    for (idx, &expected_mode) in modes[1..].iter().enumerate() {
        let channel_idx = idx + 1;
        assert_eq!(
            ym.channel(channel_idx)
                .unwrap()
                .operator(0)
                .envelope()
                .get_ssg_eg(),
            expected_mode,
            "Channel {} should remain unchanged",
            channel_idx
        );
    }
}

// TODO: test the envelope output (the shape) of each SSG-EG mode
