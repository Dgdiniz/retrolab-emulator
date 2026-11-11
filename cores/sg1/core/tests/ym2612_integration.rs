// YM2612 Integration Tests
// Tests interaction between Operator, Envelope, Algorithms, and Channels

use sg1_core::ym2612::{EnvelopeState, Operator};

#[test]
fn test_operator_with_envelope_attack() {
    let mut op = Operator::new(7_670_454);
    op.set_frequency(0x2CE, 4, 1); // ~292 Hz
    op.total_level = 0; // TL 0: no attenuation

    // Configure fast attack with decay
    op.envelope_mut().attack_rate = 31;
    op.envelope_mut().decay_rate = 20; // Non-zero decay
    op.envelope_mut().sustain_level = 8; // Mid sustain level

    // Before key on, output should be zero (envelope off)
    let output_before = op.clock(0);
    assert_eq!(output_before, 0, "Should be silent before key on");

    // Key on. Note: with attack_rate=31 (maximum) the hardware models
    // "instant attack": the envelope drops to level 0 on the very first
    // clock. We deliberately don't assert that the first sample is quiet
    // because that's only true for slower attack rates.
    op.key_on();

    // Clock until attack completes
    for _ in 0..5000 {
        op.clock(0);
        if op.envelope().get_state() != EnvelopeState::Attack {
            break;
        }
    }

    // Should have reached full volume and be in decay
    let mut max_output = 0i16;
    for _ in 0..1000 {
        let out = op.clock(0);
        max_output = max_output.max(out.abs());
    }

    assert!(
        max_output > 3000,
        "Should reach high volume after attack, got {}",
        max_output
    );
    assert!(
        op.envelope().get_state() == EnvelopeState::Decay
            || op.envelope().get_state() == EnvelopeState::Sustain,
        "Should be in decay or sustain phase"
    );
}

#[test]
fn test_operator_with_envelope_release() {
    let mut op = Operator::new(7_670_454);
    op.set_frequency(0x2CE, 4, 1);
    op.total_level = 0;

    // Configure envelope
    op.envelope_mut().attack_rate = 31;
    op.envelope_mut().release_rate = 12;

    // Key on and wait for full volume
    op.key_on();
    for _ in 0..5000 {
        op.clock(0);
    }

    let mut max_output_on = 0i16;
    for _ in 0..100 {
        let out = op.clock(0);
        max_output_on = max_output_on.max(out.abs());
    }

    // Key off (release)
    op.key_off();
    assert_eq!(op.envelope().get_state(), EnvelopeState::Release);

    // Clock through release
    for _ in 0..100000 {
        op.clock(0);
        if op.envelope().get_state() == EnvelopeState::Off {
            break;
        }
    }

    // Should be silent or very quiet
    let mut max_output_off = 0i16;
    for _ in 0..100 {
        let out = op.clock(0);
        max_output_off = max_output_off.max(out.abs());
    }

    println!("On: {}, Off: {}", max_output_on, max_output_off);

    // Release should make it much quieter (may not reach absolute zero due to exponential decay)
    assert!(
        max_output_off < max_output_on / 2,
        "Should be much quieter after release, was {}/{}",
        max_output_off,
        max_output_on
    );
}

#[test]
fn test_operator_sustain_level_affects_output() {
    // Operator with low sustain level (loud sustain)
    let mut op_loud = Operator::new(7_670_454);
    op_loud.set_frequency(0x2CE, 4, 1);
    op_loud.total_level = 0;
    op_loud.envelope_mut().attack_rate = 31;
    op_loud.envelope_mut().decay_rate = 30;
    op_loud.envelope_mut().sustain_level = 2; // Low (loud)
    op_loud.key_on();

    // Wait for sustain phase
    for _ in 0..20000 {
        op_loud.clock(0);
        if op_loud.envelope().get_state() == EnvelopeState::Sustain {
            break;
        }
    }

    let mut max_loud = 0i16;
    for _ in 0..1000 {
        let out = op_loud.clock(0);
        max_loud = max_loud.max(out.abs());
    }

    // Operator with high sustain level (quiet sustain)
    let mut op_quiet = Operator::new(7_670_454);
    op_quiet.set_frequency(0x2CE, 4, 1);
    op_quiet.total_level = 0;
    op_quiet.envelope_mut().attack_rate = 31;
    op_quiet.envelope_mut().decay_rate = 30;
    op_quiet.envelope_mut().sustain_level = 12; // High (quiet)
    op_quiet.key_on();

    // Wait for sustain phase
    for _ in 0..20000 {
        op_quiet.clock(0);
        if op_quiet.envelope().get_state() == EnvelopeState::Sustain {
            break;
        }
    }

    let mut max_quiet = 0i16;
    for _ in 0..1000 {
        let out = op_quiet.clock(0);
        max_quiet = max_quiet.max(out.abs());
    }

    println!("Loud sustain: {}, Quiet sustain: {}", max_loud, max_quiet);

    assert!(max_loud > max_quiet, "Low sustain level should be louder");
    assert!(max_quiet < max_loud / 2, "Should be significantly quieter");
}

#[test]
fn test_operator_total_level_and_envelope_interaction() {
    let mut op = Operator::new(7_670_454);
    op.set_frequency(0x2CE, 4, 1);
    op.envelope_mut().attack_rate = 31;
    op.envelope_mut().decay_rate = 0; // No decay (stay at max)
    op.envelope_mut().sustain_level = 0;

    // TL 0: no attenuation
    op.total_level = 0;
    op.key_on();

    // Wait for attack
    for _ in 0..5000 {
        op.clock(0);
    }

    let mut max_tl_0 = 0i16;
    for _ in 0..1000 {
        let out = op.clock(0);
        max_tl_0 = max_tl_0.max(out.abs());
    }

    // TL at half
    op.reset();
    op.total_level = 64;
    op.key_on();

    for _ in 0..5000 {
        op.clock(0);
    }

    let mut max_tl_64 = 0i16;
    for _ in 0..1000 {
        let out = op.clock(0);
        max_tl_64 = max_tl_64.max(out.abs());
    }

    println!("TL=0: {}, TL=64: {}", max_tl_0, max_tl_64);

    // TL should attenuate even with full envelope
    assert!(max_tl_64 < max_tl_0, "Higher TL should be quieter");
}

#[test]
fn test_multiple_key_on_cycles() {
    let mut op = Operator::new(7_670_454);
    op.set_frequency(0x2CE, 4, 1);
    op.total_level = 0;
    op.envelope_mut().attack_rate = 31;
    op.envelope_mut().release_rate = 15;

    // First cycle
    op.key_on();
    for _ in 0..5000 {
        op.clock(0);
    }

    let mut max_first = 0i16;
    for _ in 0..100 {
        let out = op.clock(0);
        max_first = max_first.max(out.abs());
    }

    // Key off
    op.key_off();
    for _ in 0..20000 {
        op.clock(0);
        if op.envelope().get_state() == EnvelopeState::Off {
            break;
        }
    }

    // Second cycle
    op.key_on();
    for _ in 0..5000 {
        op.clock(0);
    }

    let mut max_second = 0i16;
    for _ in 0..100 {
        let out = op.clock(0);
        max_second = max_second.max(out.abs());
    }

    println!("First: {}, Second: {}", max_first, max_second);

    // Should behave consistently
    assert!(
        max_second > max_first / 2,
        "Second cycle should work similarly"
    );
}

#[test]
fn test_fm_modulation_with_envelope() {
    // Modulator operator
    let mut modulator = Operator::new(7_670_454);
    modulator.set_frequency(0x400, 5, 2); // Higher frequency
    modulator.total_level = 0;
    modulator.envelope_mut().attack_rate = 31;
    modulator.key_on();

    // Carrier operator
    let mut carrier = Operator::new(7_670_454);
    carrier.set_frequency(0x2CE, 4, 1);
    carrier.total_level = 0;
    carrier.envelope_mut().attack_rate = 31;
    carrier.key_on();

    // Wait for both to reach full volume
    for _ in 0..5000 {
        modulator.clock(0);
        carrier.clock(0);
    }

    // Generate FM sound
    let mut samples = Vec::new();
    for _ in 0..1000 {
        let mod_output = modulator.clock(0);
        let carrier_output = carrier.clock(mod_output);
        samples.push(carrier_output);
    }

    // Should have variety of values (FM effect)
    let max = samples.iter().map(|&s| s.abs()).max().unwrap();
    let min = samples.iter().map(|&s| s.abs()).min().unwrap();

    println!("FM range: {} to {}", min, max);

    assert!(max > 1000, "Should have strong signal");
    assert!(max > min * 2, "Should have variety (FM effect)");
}
