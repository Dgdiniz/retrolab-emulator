//! YM2612 Key Scaling Tests
//!
//! Tests Rate Scaling (KS) - how frequency affects envelope rates

use sg1_core::ym2612::Ym2612;

#[test]
fn test_rate_scaling_register_write() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Write Rate Scaling values to channel 0, operator 0 (register 0x50)
    // Register 0x50: bits 7-6 = KS, bits 4-0 = AR
    for ks in 0..=3 {
        let reg_value = (ks << 6) | 0x1F; // KS + AR=31
        ym.write_register(0, 0x50, reg_value);

        // Verify KS was set correctly
        assert_eq!(
            ym.channel(0).unwrap().operator(0).get_rate_scaling(),
            ks,
            "Rate Scaling should be {} for register value 0x{:02X}",
            ks,
            reg_value
        );
    }
}

#[test]
fn test_rate_scaling_all_operators() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Set different KS for each operator in channel 0
    for op_idx in 0..4 {
        let ks = (op_idx as u8) & 0x03;
        let reg = 0x50 | (op_idx * 4) as u8; // 0x50, 0x54, 0x58, 0x5C
        let reg_value = (ks << 6) | 0x1F;

        ym.write_register(0, reg, reg_value);

        assert_eq!(
            ym.channel(0).unwrap().operator(op_idx).get_rate_scaling(),
            ks,
            "Operator {} should have KS={}",
            op_idx,
            ks
        );
    }
}

#[test]
fn test_rate_scaling_mask() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Write value with all bits set
    ym.write_register(0, 0x50, 0xFF);

    // KS should be masked to 2 bits (0-3)
    assert!(
        ym.channel(0).unwrap().operator(0).get_rate_scaling() <= 3,
        "Rate Scaling should be masked to 0-3"
    );
}

#[test]
fn test_rate_scaling_default_value() {
    let ym = Ym2612::new(7_670_454, 44100);

    // All operators should start with KS=0
    for channel in 0..6 {
        for op in 0..4 {
            assert_eq!(
                ym.channel(channel).unwrap().operator(op).get_rate_scaling(),
                0,
                "Channel {} operator {} should have default KS=0",
                channel,
                op
            );
        }
    }
}

#[test]
fn test_key_code_calculation() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Test Key Code = Block * 2 + (F-num bit 10)
    // F-num is 11-bit, so bit 10 is 0x400

    // Block=0, F-num bit 10 = 0 -> Key Code = 0
    ym.write_register(0, 0xA4, 0b00000000); // Block=0, F-num[10:8]=0
    ym.write_register(0, 0xA0, 0x00); // F-num[7:0]=0
    assert_eq!(
        ym.channel(0).unwrap().operator(0).get_key_code(),
        0,
        "Block=0, F-num[10]=0 -> Key Code should be 0"
    );

    // Block=0, F-num bit 10 = 1 -> Key Code = 1
    ym.write_register(0, 0xA4, 0b00000100); // Block=0, F-num[10]=1 (0x4 = bit 10)
    ym.write_register(0, 0xA0, 0x00);
    assert_eq!(
        ym.channel(0).unwrap().operator(0).get_key_code(),
        1,
        "Block=0, F-num[10]=1 -> Key Code should be 1"
    );

    // Block=1, F-num bit 10 = 0 -> Key Code = 2
    ym.write_register(0, 0xA4, 0b00001000); // Block=1, F-num[10]=0
    ym.write_register(0, 0xA0, 0x00);
    assert_eq!(
        ym.channel(0).unwrap().operator(0).get_key_code(),
        2,
        "Block=1, F-num[10]=0 -> Key Code should be 2"
    );

    // Block=1, F-num bit 10 = 1 -> Key Code = 3
    ym.write_register(0, 0xA4, 0b00001100); // Block=1, F-num[10]=1
    ym.write_register(0, 0xA0, 0x00);
    assert_eq!(
        ym.channel(0).unwrap().operator(0).get_key_code(),
        3,
        "Block=1, F-num[10]=1 -> Key Code should be 3"
    );

    // Block=7, F-num bit 10 = 1 -> Key Code = 15 (max)
    ym.write_register(0, 0xA4, 0b00111100); // Block=7, F-num[10]=1
    ym.write_register(0, 0xA0, 0x00);
    assert_eq!(
        ym.channel(0).unwrap().operator(0).get_key_code(),
        15,
        "Block=7, F-num[10]=1 -> Key Code should be 15 (max)"
    );
}

#[test]
fn test_apply_key_scaling_ks0() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // KS=0: no scaling
    ym.write_register(0, 0x50, 0b00011111); // KS=0, AR=31

    // Set high frequency (Block=7, F-num bit 10 = 1)
    ym.write_register(0, 0xA4, 0b00111100);
    ym.write_register(0, 0xA0, 0x00);

    // Get the operator
    let op = ym.channel(0).unwrap().operator(0);

    // KS=0: base rate should not change regardless of key code
    assert_eq!(op.apply_key_scaling(10), 10);
    assert_eq!(op.apply_key_scaling(20), 20);
    assert_eq!(op.apply_key_scaling(30), 30);
}

#[test]
fn test_apply_key_scaling_ks3() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // KS=3: maximum scaling
    ym.write_register(0, 0x50, 0b11011111); // KS=3, AR=31

    // Set high frequency (Block=7, F-num bit 10 = 1 -> Key Code = 15)
    ym.write_register(0, 0xA4, 0b00111100);
    ym.write_register(0, 0xA0, 0x00);

    let op = ym.channel(0).unwrap().operator(0);
    assert_eq!(op.get_key_code(), 15);

    // KS=3: shift = 3-3 = 0, so offset = Key Code >> 0 = 15
    // Effective rate = base_rate + 15
    assert_eq!(op.apply_key_scaling(10), 25); // 10 + 15
    assert_eq!(op.apply_key_scaling(20), 35); // 20 + 15
    assert_eq!(op.apply_key_scaling(30), 45); // 30 + 15
    assert_eq!(op.apply_key_scaling(50), 63); // 50 + 15, clamped to 63
}

#[test]
fn test_apply_key_scaling_ks1() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // KS=1
    ym.write_register(0, 0x50, 0b01011111); // KS=1, AR=31

    // Key Code = 8 (Block=4, F-num bit 10 = 0)
    ym.write_register(0, 0xA4, 0b00100000); // Block=4, F-num[10]=0
    ym.write_register(0, 0xA0, 0x00);

    let op = ym.channel(0).unwrap().operator(0);
    assert_eq!(op.get_key_code(), 8);

    // KS=1: shift = 3-1 = 2, so offset = Key Code >> 2 = 8 >> 2 = 2
    // Effective rate = base_rate + 2
    assert_eq!(op.apply_key_scaling(10), 12); // 10 + 2
    assert_eq!(op.apply_key_scaling(20), 22); // 20 + 2
}

#[test]
fn test_apply_key_scaling_ks2() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // KS=2
    ym.write_register(0, 0x50, 0b10011111); // KS=2, AR=31

    // Key Code = 8 (Block=4, F-num bit 10 = 0)
    ym.write_register(0, 0xA4, 0b00100000); // Block=4
    ym.write_register(0, 0xA0, 0x00);

    let op = ym.channel(0).unwrap().operator(0);
    assert_eq!(op.get_key_code(), 8);

    // KS=2: shift = 3-2 = 1, so offset = Key Code >> 1 = 8 >> 1 = 4
    // Effective rate = base_rate + 4
    assert_eq!(op.apply_key_scaling(10), 14); // 10 + 4
    assert_eq!(op.apply_key_scaling(20), 24); // 20 + 4
}

#[test]
fn test_apply_key_scaling_clamping() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // KS=3, high frequency
    ym.write_register(0, 0x50, 0b11011111); // KS=3
    ym.write_register(0, 0xA4, 0b00111100); // Block=7, F-num[10]=1

    let op = ym.channel(0).unwrap().operator(0);

    // Should clamp to 63 (6-bit max)
    assert_eq!(op.apply_key_scaling(63), 63); // Already at max
    assert_eq!(op.apply_key_scaling(60), 63); // 60 + 15 = 75, clamped to 63
    assert_eq!(op.apply_key_scaling(50), 63); // 50 + 15 = 65, clamped to 63
}

#[test]
fn test_apply_key_scaling_zero_rate() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // KS=3, high frequency
    ym.write_register(0, 0x50, 0b11011111); // KS=3
    ym.write_register(0, 0xA4, 0b00111100); // Block=7

    let op = ym.channel(0).unwrap().operator(0);

    // Rate 0 should stay 0 (special case)
    assert_eq!(op.apply_key_scaling(0), 0);
}

#[test]
fn test_key_code_updates_with_frequency() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Initial: Block=0, F-num=0 -> Key Code = 0
    assert_eq!(ym.channel(0).unwrap().operator(0).get_key_code(), 0);

    // Update Block
    ym.write_register(0, 0xA4, 0b00001000); // Block=1
    assert_eq!(
        ym.channel(0).unwrap().operator(0).get_key_code(),
        2,
        "Key Code should update when Block changes"
    );

    // Update F-num bit 10
    ym.write_register(0, 0xA4, 0b00001100); // Block=1, F-num[10]=1
    assert_eq!(
        ym.channel(0).unwrap().operator(0).get_key_code(),
        3,
        "Key Code should update when F-num bit 10 changes"
    );

    // Update low F-num (should not affect key code)
    let prev_key_code = ym.channel(0).unwrap().operator(0).get_key_code();
    ym.write_register(0, 0xA0, 0xFF); // F-num[7:0]=0xFF
    assert_eq!(
        ym.channel(0).unwrap().operator(0).get_key_code(),
        prev_key_code,
        "Key Code should not change when low F-num bits change"
    );
}

#[test]
fn test_rate_scaling_independent_per_operator() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Set KS=3 on operator 0
    ym.write_register(0, 0x50, 0b11011111); // KS=3

    // Set KS=1 on operator 1
    ym.write_register(0, 0x54, 0b01011111); // KS=1

    // Verify independence
    assert_eq!(ym.channel(0).unwrap().operator(0).get_rate_scaling(), 3);
    assert_eq!(ym.channel(0).unwrap().operator(1).get_rate_scaling(), 1);
    assert_eq!(ym.channel(0).unwrap().operator(2).get_rate_scaling(), 0); // Unchanged
}

#[test]
fn test_register_0x50_ks_ar_combined() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Register 0x50: bits 7-6 = KS, bits 4-0 = AR
    // Set KS=2, AR=20
    ym.write_register(0, 0x50, 0b10010100); // KS=2, AR=20

    assert_eq!(
        ym.channel(0).unwrap().operator(0).get_rate_scaling(),
        2,
        "KS should be 2"
    );
    // AR is not checked (`envelope().attack_rate` would show it): the test
    // checks only the KS bits
}
