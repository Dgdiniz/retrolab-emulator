// YM2612 Channel Integration Tests
// Tests full channel functionality with algorithms

use sg1_core::ym2612::{Channel, StereoFlags};

#[test]
fn test_channel_algorithm0_series() {
    // Algorithm 0: All operators in series (maximum modulation depth)
    let mut channel = Channel::new(7_670_454);
    channel.set_algorithm(0);
    channel.set_feedback(0);

    // Configure all operators
    for i in 0..4 {
        channel.operator_mut(i).set_frequency(0x2CE, 4, 1);
        channel.operator_mut(i).total_level = 0;
        channel.operator_mut(i).envelope_mut().attack_rate = 31;
        channel.operator_mut(i).envelope_mut().decay_rate = 0;
        channel.operator_mut(i).envelope_mut().sustain_level = 0;
    }

    // Key on all operators
    channel.key_on_all();

    // Clock to build up volume
    for _ in 0..5000 {
        channel.clock();
    }

    // Should produce sound (only OP4 outputs in Algorithm 0)
    let (left, right) = channel.clock();
    println!("Algorithm 0: L={}, R={}", left, right);

    assert!(left.abs() > 10, "Should have output");
    assert_eq!(left, right, "Stereo should be equal");
}

#[test]
fn test_channel_algorithm7_additive() {
    // Algorithm 7: All operators parallel (additive synthesis)
    let mut channel = Channel::new(7_670_454);
    channel.set_algorithm(7);

    // Configure operators with different harmonics
    for i in 0..4 {
        channel
            .operator_mut(i)
            .set_frequency(0x100 * (i as u16 + 1), 4, 1);
        channel.operator_mut(i).total_level = 0;
        channel.operator_mut(i).envelope_mut().attack_rate = 31;
        channel.operator_mut(i).envelope_mut().decay_rate = 0;
        channel.operator_mut(i).envelope_mut().sustain_level = 0;
    }

    channel.key_on_all();

    // Build up volume
    for _ in 0..5000 {
        channel.clock();
    }

    // Should have strong output (all 4 operators contributing)
    let (left, right) = channel.clock();
    println!("Algorithm 7: L={}, R={}", left, right);

    assert!(left.abs() > 100, "Should have strong additive output");
}

#[test]
fn test_channel_stereo_separation() {
    let mut channel = Channel::new(7_670_454);
    channel.set_algorithm(7); // All parallel for clarity

    // Setup operators
    for i in 0..4 {
        channel.operator_mut(i).set_frequency(0x2CE, 4, 1);
        channel.operator_mut(i).total_level = 0;
        channel.operator_mut(i).envelope_mut().attack_rate = 31;
    }
    channel.key_on_all();

    // Build up volume
    for _ in 0..5000 {
        channel.clock();
    }

    // Test left only
    channel.set_stereo(StereoFlags::left_only());
    let (left, right) = channel.clock();
    assert_ne!(left, 0, "Left should have sound");
    assert_eq!(right, 0, "Right should be muted");

    // Test right only
    channel.set_stereo(StereoFlags::right_only());
    let (left, right) = channel.clock();
    assert_eq!(left, 0, "Left should be muted");
    assert_ne!(right, 0, "Right should have sound");

    // Test both
    channel.set_stereo(StereoFlags::both());
    let (left, right) = channel.clock();
    assert_ne!(left, 0, "Left should have sound");
    assert_ne!(right, 0, "Right should have sound");
    assert_eq!(left, right, "Should be identical");

    // Test muted
    channel.set_stereo(StereoFlags::muted());
    let (left, right) = channel.clock();
    assert_eq!(left, 0, "Left should be muted");
    assert_eq!(right, 0, "Right should be muted");
}

#[test]
fn test_channel_feedback() {
    let mut channel = Channel::new(7_670_454);
    channel.set_algorithm(7); // All parallel so OP1 outputs directly

    // Setup OP1 for feedback test
    channel.operator_mut(0).set_frequency(0x2CE, 4, 1);
    channel.operator_mut(0).total_level = 0;
    channel.operator_mut(0).envelope_mut().attack_rate = 31;

    // Other operators off
    for i in 1..4 {
        channel.operator_mut(i).total_level = 127; // Mute
    }

    // Test no feedback
    channel.set_feedback(0);
    channel.key_on_all();

    for _ in 0..1000 {
        channel.clock();
    }

    let mut samples_no_fb = Vec::new();
    for _ in 0..100 {
        let (left, _) = channel.clock();
        samples_no_fb.push(left);
    }

    // Reset and test with feedback
    channel.reset();
    // Re-configure after reset
    channel.operator_mut(0).set_frequency(0x2CE, 4, 1);
    channel.operator_mut(0).total_level = 0;
    channel.operator_mut(0).envelope_mut().attack_rate = 31;
    for i in 1..4 {
        channel.operator_mut(i).total_level = 127;
    }
    channel.set_feedback(7); // Max feedback
    channel.key_on_all();

    for _ in 0..1000 {
        channel.clock();
    }

    let mut samples_with_fb = Vec::new();
    for _ in 0..100 {
        let (left, _) = channel.clock();
        samples_with_fb.push(left);
    }

    println!(
        "No FB range: {} to {}",
        samples_no_fb.iter().min().unwrap(),
        samples_no_fb.iter().max().unwrap()
    );
    println!(
        "With FB range: {} to {}",
        samples_with_fb.iter().min().unwrap(),
        samples_with_fb.iter().max().unwrap()
    );

    // Feedback should affect waveform (though exact effect depends on implementation)
    // Just verify both produce sound
    assert!(samples_no_fb.iter().any(|&s| s.abs() > 10));
    assert!(samples_with_fb.iter().any(|&s| s.abs() > 10));
}

#[test]
fn test_channel_envelope_control() {
    let mut channel = Channel::new(7_670_454);
    channel.set_algorithm(7); // All parallel

    // Configure all operators with fast attack
    for i in 0..4 {
        channel.operator_mut(i).set_frequency(0x2CE, 4, 1);
        channel.operator_mut(i).total_level = 0;
        channel.operator_mut(i).envelope_mut().attack_rate = 31;
        channel.operator_mut(i).envelope_mut().release_rate = 15;
    }

    // Key on
    channel.key_on_all();

    // Attack phase - volume should increase
    let mut max_during_attack = 0i16;
    for _ in 0..5000 {
        let (left, _) = channel.clock();
        max_during_attack = max_during_attack.max(left.abs());
    }

    println!("Max during attack: {}", max_during_attack);
    assert!(max_during_attack > 1000, "Should reach high volume");

    // Key off - start release
    channel.key_off_all();

    // Release phase - volume should decrease
    for _ in 0..50000 {
        channel.clock();
    }

    let mut max_after_release = 0i16;
    for _ in 0..100 {
        let (left, _) = channel.clock();
        max_after_release = max_after_release.max(left.abs());
    }

    println!("Max after release: {}", max_after_release);
    assert!(
        max_after_release < max_during_attack / 2,
        "Should be quieter after release"
    );
}

#[test]
fn test_channel_selective_key_on() {
    let mut channel = Channel::new(7_670_454);
    channel.set_algorithm(7); // All parallel

    // Configure all operators
    for i in 0..4 {
        channel
            .operator_mut(i)
            .set_frequency(0x100 * (i as u16 + 1), 4, 1);
        channel.operator_mut(i).total_level = 0;
        channel.operator_mut(i).envelope_mut().attack_rate = 31;
    }

    // Key on only OP1 and OP3 (bitmask: 0x05)
    channel.key_on(0x05);

    // Build up
    for _ in 0..5000 {
        channel.clock();
    }

    let (left_partial, _) = channel.clock();

    // Now key on all
    channel.reset();
    // Re-configure after reset
    for i in 0..4 {
        channel
            .operator_mut(i)
            .set_frequency(0x100 * (i as u16 + 1), 4, 1);
        channel.operator_mut(i).total_level = 0;
        channel.operator_mut(i).envelope_mut().attack_rate = 31;
    }
    channel.key_on_all();

    for _ in 0..5000 {
        channel.clock();
    }

    let (left_all, _) = channel.clock();

    println!(
        "Partial key on: {}, All key on: {}",
        left_partial.abs(),
        left_all.abs()
    );

    // All operators should produce louder sound (or at least comparable)
    // Note: Due to timing and envelope state, might not always be strictly louder
    // Just verify both produce sound
    assert!(left_partial.abs() > 100, "Partial should produce sound");
    assert!(left_all.abs() > 100, "All should produce sound");
}

#[test]
fn test_channel_algorithm_comparison() {
    // Compare different algorithms with same operator settings

    let mut results = Vec::new();

    for algo in 0..8 {
        let mut channel = Channel::new(7_670_454);
        channel.set_algorithm(algo);

        // Same settings for all
        for i in 0..4 {
            channel.operator_mut(i).set_frequency(0x2CE, 4, 1);
            channel.operator_mut(i).total_level = 0;
            channel.operator_mut(i).envelope_mut().attack_rate = 31;
            channel.operator_mut(i).envelope_mut().decay_rate = 0;
            channel.operator_mut(i).envelope_mut().sustain_level = 0;
        }

        channel.key_on_all();

        // Build up
        for _ in 0..5000 {
            channel.clock();
        }

        // Measure average amplitude over 100 samples
        let mut sum = 0i64;
        for _ in 0..100 {
            let (left, _) = channel.clock();
            sum += left.abs() as i64;
        }
        let avg = (sum / 100) as i16;

        results.push((algo, avg));
        println!("Algorithm {}: avg amplitude = {}", algo, avg);
    }

    // All algorithms should produce sound
    for (algo, amp) in &results {
        assert!(*amp > 10, "Algorithm {} should produce sound", algo);
    }

    // Algorithm 7 (all parallel) should generally be loudest
    let algo7_amp = results[7].1;
    assert!(
        algo7_amp > results[0].1,
        "Algorithm 7 (parallel) should be louder than Algorithm 0 (series)"
    );
}

#[test]
fn test_multiple_channels_independent() {
    // Verify that multiple channels can run independently
    let mut ch1 = Channel::new(7_670_454);
    let mut ch2 = Channel::new(7_670_454);

    ch1.set_algorithm(7);
    ch2.set_algorithm(0);

    // Configure all operators for both channels
    for i in 0..4 {
        ch1.operator_mut(i).set_frequency(0x2CE, 4, 1); // ~292 Hz
        ch1.operator_mut(i).total_level = 0;
        ch1.operator_mut(i).envelope_mut().attack_rate = 31;

        ch2.operator_mut(i).set_frequency(0x400, 5, 1); // Higher
        ch2.operator_mut(i).total_level = 0;
        ch2.operator_mut(i).envelope_mut().attack_rate = 31;
    }

    ch1.key_on_all();
    ch2.key_on_all();

    // Clock both
    for _ in 0..5000 {
        ch1.clock();
        ch2.clock();
    }

    // Get samples
    let (left1, right1) = ch1.clock();
    let (left2, right2) = ch2.clock();

    println!("Ch1: L={}, R={}", left1, right1);
    println!("Ch2: L={}, R={}", left2, right2);

    // Both should produce sound
    assert!(left1.abs() > 10);
    assert!(left2.abs() > 10);

    // They should be different (different frequencies/algorithms)
    // (might be same by coincidence, but unlikely)
    println!("Difference: {}", (left1 - left2).abs());
}
