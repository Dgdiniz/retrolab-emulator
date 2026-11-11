//! YM2612 LFO Sensitivity Tests
//!
//! Tests PMS (Pitch Modulation Sensitivity) and AMS (Amplitude Modulation Sensitivity)

use sg1_core::ym2612::Ym2612;

#[test]
fn test_pms_register_write() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Write PMS values to channel 0 (register 0xB4)
    for pms in 0..=7 {
        let reg_value = 0xC0 | pms; // L+R enabled (bits 7-6) + PMS (bits 2-0)
        ym.write_register(0, 0xB4, reg_value);

        // Verify PMS was set correctly
        assert_eq!(
            ym.channel(0).unwrap().get_pms(),
            pms,
            "PMS should be {} for register value 0x{:02X}",
            pms,
            reg_value
        );
    }
}

#[test]
fn test_ams_register_write() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // AMS lives in register 0xB4 (per channel), NOT in 0x60. Bits 5-4
    // carry the AMS value; the chip then mirrors it onto all 4
    // operators of that channel.
    for ams in 0..=3 {
        let reg_value = 0xC0 | (ams << 4); // L+R enabled + AMS in bits 5-4
        ym.write_register(0, 0xB4, reg_value);

        // Verify AMS was propagated to operator 0 of channel 0
        assert_eq!(
            ym.channel(0).unwrap().operator(0).get_ams(),
            ams,
            "AMS should be {} for register 0xB4 value 0x{:02X}",
            ams,
            reg_value
        );
    }
}

#[test]
fn test_pms_all_channels() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Set different PMS for each channel
    for channel in 0..6 {
        let pms = (channel as u8) & 0x07;
        let part = if channel < 3 { 0 } else { 1 };
        let reg = 0xB4 | (channel % 3) as u8;
        let reg_value = 0xC0 | pms; // L+R + PMS

        ym.write_register(part, reg, reg_value);

        assert_eq!(
            ym.channel(channel).unwrap().get_pms(),
            pms,
            "Channel {} should have PMS={}",
            channel,
            pms
        );
    }
}

#[test]
fn test_ams_all_operators() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // AMS is per-channel (register 0xB4 / 0xB5 / 0xB6 for part 0; same
    // offsets for part 1). After writing, ALL 4 operators of that
    // channel receive the same AMS value.
    let ams = 1;
    let reg_value = 0xC0 | (ams << 4); // L+R + AMS=1
    ym.write_register(0, 0xB4, reg_value); // channel 0

    for op_idx in 0..4 {
        assert_eq!(
            ym.channel(0).unwrap().operator(op_idx).get_ams(),
            ams,
            "Operator {} should have AMS={}",
            op_idx,
            ams
        );
    }
}

#[test]
fn test_pms_mask() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Write value with bits set outside PMS range
    ym.write_register(0, 0xB4, 0xFF); // All bits set

    // PMS should be masked to 3 bits (0-7)
    assert!(
        ym.channel(0).unwrap().get_pms() <= 7,
        "PMS should be masked to 0-7"
    );
}

#[test]
fn test_ams_mask() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // AMS is bits 5-4 of 0xB4. Each write sets every other bit of the
    // register (L, R, the unused bit 3 and PMS = 7), so only a read of
    // exactly bits 5-4 gives back the AMS it carries. Each write in this
    // order changes the AMS the previous one set.
    for ams in [3u8, 0, 2, 1] {
        let reg_value = 0xCF | (ams << 4);
        ym.write_register(0, 0xB4, reg_value);

        for op_idx in 0..4 {
            assert_eq!(
                ym.channel(0).unwrap().operator(op_idx).get_ams(),
                ams,
                "Operator {} should have AMS={} for register 0xB4 value 0x{:02X}",
                op_idx,
                ams,
                reg_value
            );
        }
    }
}

#[test]
fn test_pms_default_value() {
    let ym = Ym2612::new(7_670_454, 44100);

    // All channels should start with PMS=0
    for channel in 0..6 {
        assert_eq!(
            ym.channel(channel).unwrap().get_pms(),
            0,
            "Channel {} should have default PMS=0",
            channel
        );
    }
}

#[test]
fn test_ams_default_value() {
    let ym = Ym2612::new(7_670_454, 44100);

    // All operators should start with AMS=0
    for channel in 0..6 {
        for op in 0..4 {
            assert_eq!(
                ym.channel(channel).unwrap().operator(op).get_ams(),
                0,
                "Channel {} operator {} should have default AMS=0",
                channel,
                op
            );
        }
    }
}

#[test]
fn test_pms_independent_per_channel() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Set PMS=7 on channel 0
    ym.write_register(0, 0xB4, 0xC7); // L+R (0xC0) + PMS=7 (0x07)

    // Set PMS=3 on channel 1
    ym.write_register(0, 0xB5, 0xC3); // L+R (0xC0) + PMS=3 (0x03)

    // Verify independence
    assert_eq!(ym.channel(0).unwrap().get_pms(), 7);
    assert_eq!(ym.channel(1).unwrap().get_pms(), 3);
    assert_eq!(ym.channel(2).unwrap().get_pms(), 0); // Unchanged
}

#[test]
fn test_ams_independent_per_channel() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // AMS is per-CHANNEL (not per-operator). Distinct AMS values can
    // be set on distinct channels and remain independent.
    // Channel 0: AMS=3 via 0xB4
    ym.write_register(0, 0xB4, 0xC0 | (3 << 4));
    // Channel 1: AMS=1 via 0xB5
    ym.write_register(0, 0xB5, 0xC0 | (1 << 4));

    assert_eq!(ym.channel(0).unwrap().operator(0).get_ams(), 3);
    assert_eq!(ym.channel(1).unwrap().operator(0).get_ams(), 1);
    // Channel 2 was never written -> still default 0
    assert_eq!(ym.channel(2).unwrap().operator(0).get_ams(), 0);
}

#[test]
fn test_register_0xb4_stereo_pms_combined() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Register 0xB4 layout (per the YM2612 datasheet):
    //   bit 7 = L output enable,
    //   bit 6 = R output enable,
    //   bits 5-4 = AMS,
    //   bits 2-0 = PMS.
    // Set L only (no R), AMS=0, PMS=5.
    ym.write_register(0, 0xB4, 0b10000101); // L=1, R=0, AMS=0, PMS=5

    let stereo = ym.channel(0).unwrap().get_stereo();
    assert!(stereo.left, "Left should be enabled");
    assert!(!stereo.right, "Right should be disabled");
    assert_eq!(ym.channel(0).unwrap().get_pms(), 5, "PMS should be 5");
}

#[test]
fn test_register_0xb4_ams_combined_with_pms() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Register 0xB4 carries both AMS (bits 5-4) and PMS (bits 2-0).
    // Verify that setting both at once preserves both fields and
    // that AMS is propagated to all operators of the channel.
    // L=1, R=1, AMS=2, PMS=5.
    ym.write_register(0, 0xB4, 0b11100101);

    assert_eq!(
        ym.channel(0).unwrap().operator(0).get_ams(),
        2,
        "AMS should be 2"
    );
    assert_eq!(ym.channel(0).unwrap().get_pms(), 5, "PMS should be 5");
}
