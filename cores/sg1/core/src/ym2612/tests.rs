//! The chip's unit tests.

use retro_core::audio::{AudioChip, ChannelType};
use retro_core::cpu::Clockable;

use super::*;

#[test]
fn test_ym2612_creation() {
    let ym = Ym2612::new(7_670_454, 44100);

    assert_eq!(ym.clock_freq, 7_670_454);
    assert_eq!(ym.sample_rate, 44100);
    assert!(!ym.lfo().is_enabled());
    assert!(!ym.is_dac_enabled());
}

#[test]
fn test_channel_access() {
    let ym = Ym2612::new(7_670_454, 44100);

    // Should have 6 channels
    for i in 0..6 {
        assert!(ym.channel(i).is_some());
    }

    assert!(ym.channel(6).is_none());
}

#[test]
fn test_lfo_register_write() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Write to LFO register (0x22)
    ym.write_register(0, 0x22, 0x0F); // Enable LFO, freq 7

    assert!(ym.lfo().is_enabled());
    assert_eq!(ym.lfo().get_frequency(), 7);

    // Disable LFO
    ym.write_register(0, 0x22, 0x03);
    assert!(!ym.lfo().is_enabled());
    assert_eq!(ym.lfo().get_frequency(), 3);
}

#[test]
fn test_dac_registers() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable DAC
    ym.write_register(0, 0x2B, 0x80);
    assert!(ym.is_dac_enabled());

    // Write DAC data
    ym.write_register(0, 0x2A, 0xFF);
    assert_eq!(ym.get_dac_value(), 127); // 255 - 128 = 127

    ym.write_register(0, 0x2A, 0x00);
    assert_eq!(ym.get_dac_value(), -128); // 0 - 128 = -128
}

#[test]
fn test_dac_enable_disable() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Initially disabled
    assert!(!ym.is_dac_enabled());

    // Enable DAC (bit 7 = 1)
    ym.write_register(0, 0x2B, 0x80);
    assert!(ym.is_dac_enabled());

    // Disable DAC (bit 7 = 0)
    ym.write_register(0, 0x2B, 0x00);
    assert!(!ym.is_dac_enabled());

    // Other bits should not affect enable
    ym.write_register(0, 0x2B, 0xFF);
    assert!(ym.is_dac_enabled());

    ym.write_register(0, 0x2B, 0x7F);
    assert!(!ym.is_dac_enabled());
}

#[test]
fn test_dac_value_range() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Test full range of DAC values (0-255 -> -128 to +127)
    ym.write_register(0, 0x2A, 0x00);
    assert_eq!(ym.get_dac_value(), -128);

    ym.write_register(0, 0x2A, 0x80);
    assert_eq!(ym.get_dac_value(), 0);

    ym.write_register(0, 0x2A, 0xFF);
    assert_eq!(ym.get_dac_value(), 127);

    // Test specific values
    ym.write_register(0, 0x2A, 0x40);
    assert_eq!(ym.get_dac_value(), -64);

    ym.write_register(0, 0x2A, 0xC0);
    assert_eq!(ym.get_dac_value(), 64);
}

#[test]
fn test_dac_direct_method() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Test direct set_dac_value method
    ym.set_dac_value(100);
    assert_eq!(ym.get_dac_value(), 100);

    ym.set_dac_value(-50);
    assert_eq!(ym.get_dac_value(), -50);

    ym.set_dac_value(0);
    assert_eq!(ym.get_dac_value(), 0);
}

#[test]
fn test_dac_channel_type() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Initially channel 6 (index 5) is FM
    let info = ym.get_info();
    assert_eq!(info.channel_types[5], ChannelType::FM);

    // Enable DAC
    ym.write_register(0, 0x2B, 0x80);

    let info = ym.get_info();
    assert_eq!(info.channel_types[5], ChannelType::PCM);

    // Other channels remain FM
    for i in 0..5 {
        assert_eq!(info.channel_types[i], ChannelType::FM);
    }

    // Disable DAC
    ym.write_register(0, 0x2B, 0x00);

    let info = ym.get_info();
    assert_eq!(info.channel_types[5], ChannelType::FM);
}

#[test]
fn test_key_on_off() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Key on channel 0, all operators (0xF0)
    ym.write_register(0, 0x28, 0xF0);

    let channel = ym.channel(0).unwrap();
    for i in 0..4 {
        assert!(channel.operator(i).envelope().is_key_on());
    }

    // Key off channel 0
    ym.write_register(0, 0x28, 0x00);

    let channel = ym.channel(0).unwrap();
    for i in 0..4 {
        assert!(!channel.operator(i).envelope().is_key_on());
    }
}

#[test]
fn test_frequency_registers() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Write F-Number low (0xA0 = channel 0)
    ym.write_register(0, 0xA0, 0xCE);

    // Write F-Number high + Block (0xA4 = channel 0)
    ym.write_register(0, 0xA4, 0x22); // Block 4, F-Num high 2

    let channel = ym.channel(0).unwrap();
    assert_eq!(channel.operator(0).freq_number, 0x2CE);
    assert_eq!(channel.operator(0).block, 4);
}

#[test]
fn test_algorithm_feedback_registers() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Write algorithm and feedback (0xB0 = channel 0)
    ym.write_register(0, 0xB0, 0x3F); // FB=7, ALG=7

    let channel = ym.channel(0).unwrap();
    assert_eq!(channel.get_algorithm(), Algorithm::Algorithm7);
    assert_eq!(channel.get_feedback(), 7);
}

#[test]
fn test_stereo_register() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Write stereo (0xB4 = channel 0)
    ym.write_register(0, 0xB4, 0xC0); // Both L+R

    let channel = ym.channel(0).unwrap();
    let stereo = channel.get_stereo();
    assert!(stereo.left && stereo.right);

    // Left only (bit 7 = L)
    ym.write_register(0, 0xB4, 0x80);
    let channel = ym.channel(0).unwrap(); // Get fresh borrow
    let stereo = channel.get_stereo();
    assert!(stereo.left && !stereo.right);
}

#[test]
fn test_operator_registers() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Detune/Multiple (0x30 = OP1, channel 0)
    ym.write_register(0, 0x30, 0x34); // DT=3, MUL=4

    let op = ym.channel(0).unwrap().operator(0);
    assert_eq!(op.detune, 3);
    assert_eq!(op.multiple, 4);

    // Total Level (0x40 = OP1, channel 0)
    ym.write_register(0, 0x40, 0x20);
    assert_eq!(ym.channel(0).unwrap().operator(0).total_level, 0x20);

    // Attack Rate (0x50 = OP1, channel 0)
    ym.write_register(0, 0x50, 0x1F); // AR=31
    assert_eq!(
        ym.channel(0).unwrap().operator(0).envelope().attack_rate,
        31
    );

    // Sustain Level / Release Rate (0x80 = OP1, channel 0)
    ym.write_register(0, 0x80, 0x8F); // SL=8, RR=15
    assert_eq!(
        ym.channel(0).unwrap().operator(0).envelope().sustain_level,
        8
    );
    assert_eq!(
        ym.channel(0).unwrap().operator(0).envelope().release_rate,
        15
    );
}

#[test]
fn test_reset() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Setup some state
    ym.write_register(0, 0x22, 0x0F);
    ym.write_register(0, 0x2B, 0x80);
    ym.write_register(0, 0x28, 0xF0);

    // Reset
    ym.reset();

    assert!(!ym.lfo().is_enabled());
    assert!(!ym.is_dac_enabled());
    assert!(!ym.channel(0).unwrap().operator(0).envelope().is_key_on());
}

#[test]
fn test_timer_a_register() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Write Timer A period (10-bit)
    // 0x24 holds bits 9-2 of the period, 0x25 bits 1-0
    // To set TA = 0x0355 (853): high = 0x0355 >> 2 = 0xD5, low = 0x0355 & 3 = 1
    ym.write_register(0, 0x24, 0xD5);
    ym.write_register(0, 0x25, 0x01);

    assert_eq!(ym.timers().get_timer_a_period(), 0x0355); // 853
}

#[test]
fn test_timer_b_register() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Write Timer B period (8-bit)
    ym.write_register(0, 0x26, 0xAB);

    assert_eq!(ym.timers().get_timer_b_period(), 0xAB); // 171
}

#[test]
fn test_timer_control() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable both timers to run
    // Bit 0: Timer A run, Bit 1: Timer B run
    ym.write_register(0, 0x27, 0x03); // Run A and B

    assert!(ym.timers().is_timer_a_running());
    assert!(ym.timers().is_timer_b_running());

    // Disable timers
    ym.write_register(0, 0x27, 0x00);

    assert!(!ym.timers().is_timer_a_running());
    assert!(!ym.timers().is_timer_b_running());
}

#[test]
fn test_csm_mode() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Set CSM mode (bit 7)
    ym.write_register(0, 0x27, 0x80);

    assert!(ym.timers().is_csm_mode());

    // Clear CSM mode
    ym.write_register(0, 0x27, 0x00);

    assert!(!ym.timers().is_csm_mode());
}

// ===== Sub-frame timestamp catch-up rendering =====

/// Clocks corresponding to roughly `samples` output samples at the test rate.
fn clocks_for_samples(samples: u32) -> u32 {
    ((7_670_454f32 / 44100f32) * samples as f32) as u32
}

// The system's cycles->samples converter decides how many stereo pairs a
// frame owes. This chip's accumulator is a second quantisation of the same
// elapsed time, so it can land a pair short -- and a short return would
// leave the caller's pre-zeroed tail as silence, which a listener hears as
// a tick. Ask for one pair more than the chip has earned: the buffer must
// still come back full, and the last pair must carry signal.
#[test]
fn generate_samples_fills_a_one_pair_shortfall_instead_of_leaving_silence() {
    let mut ym = Ym2612::new(7_670_454, 44100);
    ym.set_rendering_enabled(true);
    ym.write_register(0, 0x2B, 0x80); // DAC enable
    ym.write_register(1, 0xB6, 0xC0); // channel 6 (DAC) stereo L+R
    ym.write_port(0, 0x2A);
    ym.write_port(1, 0xFF); // hold the DAC at a non-zero level

    ym.clock(clocks_for_samples(400));
    ym.catch_up();
    let earned = ym.frame_samples.len() / 2;
    assert!(earned > 0, "test setup produced no samples");

    let want = (earned + 1) * 2;
    let mut buf = vec![0.0f32; want];
    let n = ym.generate_samples(&mut buf, 44100);

    assert_eq!(
        n, want,
        "the chip must fill every pair the system asked for"
    );
    assert!(
        buf[want - 2] != 0.0 && buf[want - 1] != 0.0,
        "the borrowed pair reached the mixer as silence"
    );
}

// The borrow is bounded: an oversized scratch buffer must not make the chip
// render audio the machine has not reached.
#[test]
fn generate_samples_does_not_run_ahead_of_the_machine_for_a_large_buffer() {
    let mut ym = Ym2612::new(7_670_454, 44100);
    ym.set_rendering_enabled(true);

    ym.clock(clocks_for_samples(100));
    ym.catch_up();
    let earned = ym.frame_samples.len() / 2;

    let mut buf = vec![0.0f32; 4096];
    let n = ym.generate_samples(&mut buf, 44100);

    assert!(
        n <= (earned + MAX_BORROWED_PAIRS) * 2,
        "rendered {n} slots for {earned} earned pairs -- the borrow is unbounded"
    );
}

// A register write must only affect samples produced after the write's cycle
// position within the frame, not retroactively rewrite the whole frame. The
// DAC is used as a deterministic probe (its value maps straight to output)
// because it travels the exact same `write_port` -> `catch_up` path that every
// FM register write (key-on, frequency, total level, ...) takes.
#[test]
fn test_catchup_dac_write_only_affects_samples_after_its_position() {
    let mut ym = Ym2612::new(7_670_454, 44100);
    ym.set_rendering_enabled(true);
    // dac_value defaults to 0, so the first half of the frame renders silent.
    ym.write_register(0, 0x2B, 0x80); // DAC enable
    ym.write_register(1, 0xB6, 0xC0); // channel 6 (DAC) stereo L+R

    let half = clocks_for_samples(100);
    ym.clock(half);

    // Mid-frame: route the DAC update through the port path so `catch_up`
    // renders the silent samples accrued so far *before* the value changes.
    ym.write_port(0, 0x2A);
    ym.write_port(1, 0xFF); // dac_value -> +127 (loud)
    ym.clock(half);

    let mut buf = vec![0.0f32; 512];
    let n = ym.generate_samples(&mut buf, 44100);
    assert!(n >= 360, "expected ~200 stereo pairs, got {n}");

    // ~80 pairs before the ~100-sample boundary: silent (dac was 0).
    let early_max = buf[..160].iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    // After the boundary: carries the loud DAC value.
    let late_max = buf[240..n].iter().fold(0.0f32, |m, &s| m.max(s.abs()));

    assert!(early_max < 0.01, "pre-write region not silent: {early_max}");
    assert!(late_max > 0.1, "post-write region not audible: {late_max}");
}

// Two writes in the same frame must each take effect at their own position,
// rather than collapsing to the last value, as rendering a whole frame at once would.
#[test]
fn test_catchup_two_dac_writes_in_one_frame_both_render() {
    let mut ym = Ym2612::new(7_670_454, 44100);
    ym.set_rendering_enabled(true);
    ym.write_register(0, 0x2B, 0x80); // DAC enable
    ym.write_register(1, 0xB6, 0xC0); // channel 6 (DAC) stereo L+R

    let third = clocks_for_samples(100);

    // Region 1: dac = 0 (silent).
    ym.clock(third);
    // Region 2: dac = +127 (positive).
    ym.write_port(0, 0x2A);
    ym.write_port(1, 0xFF);
    ym.clock(third);
    // Region 3: dac = -128 (negative).
    ym.write_port(0, 0x2A);
    ym.write_port(1, 0x00);
    ym.clock(third);

    let mut buf = vec![0.0f32; 1024];
    let n = ym.generate_samples(&mut buf, 44100);
    assert!(n >= 560, "expected ~300 stereo pairs, got {n}");

    let r1 = &buf[..160]; // region 1 (~silent)
    let r2 = &buf[240..360]; // region 2 (positive)
    let r3 = &buf[440..n]; // region 3 (negative)

    let r1_max = r1.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    let r2_min = r2.iter().fold(f32::INFINITY, |m, &s| m.min(s));
    let r3_max = r3.iter().fold(f32::NEG_INFINITY, |m, &s| m.max(s));

    assert!(r1_max < 0.01, "region 1 not silent: {r1_max}");
    assert!(r2_min > 0.05, "region 2 not positive: {r2_min}");
    assert!(r3_max < -0.05, "region 3 not negative: {r3_max}");
}

// The same guarantee for a real FM key-on: nothing sounds before the key-on,
// output appears only afterward.
#[test]
fn test_catchup_fm_key_on_sounds_only_after_write() {
    let mut ym = Ym2612::new(7_670_454, 44100);
    ym.set_rendering_enabled(true);

    // Minimal audible patch on channel 0: algorithm 7 (all four operators are
    // carriers), max level, instant attack, no decay, both speakers.
    for op in [0x30u8, 0x34, 0x38, 0x3C] {
        ym.write_register(0, op, 0x01); // DT=0, MUL=1
    }
    for op in [0x40u8, 0x44, 0x48, 0x4C] {
        ym.write_register(0, op, 0x00); // TL=0 (loudest)
    }
    for op in [0x50u8, 0x54, 0x58, 0x5C] {
        ym.write_register(0, op, 0x1F); // AR=31 (instant attack)
    }
    for op in [0x60u8, 0x64, 0x68, 0x6C, 0x70, 0x74, 0x78, 0x7C] {
        ym.write_register(0, op, 0x00); // no decay / sustain ramp
    }
    for op in [0x80u8, 0x84, 0x88, 0x8C] {
        ym.write_register(0, op, 0x00); // SL=0, RR=0
    }
    ym.write_register(0, 0xB0, 0x07); // FB=0, ALG=7
    ym.write_register(0, 0xB4, 0xC0); // stereo L+R
    ym.write_register(0, 0xA4, 0x22); // block=4, F-num high
    ym.write_register(0, 0xA0, 0x00); // F-num low

    let half = clocks_for_samples(100);
    // Before key-on: envelopes are off -> silence.
    ym.clock(half);
    // Key-on channel 0, all operators, mid-frame (through the port path).
    ym.write_port(0, 0x28);
    ym.write_port(1, 0xF0);
    ym.clock(half);

    let mut buf = vec![0.0f32; 512];
    let n = ym.generate_samples(&mut buf, 44100);
    assert!(n >= 360, "expected ~200 stereo pairs, got {n}");

    let early_max = buf[..160].iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    let late_max = buf[240..n].iter().fold(0.0f32, |m, &s| m.max(s.abs()));

    assert!(
        early_max < 0.01,
        "pre key-on region not silent: {early_max}"
    );
    assert!(
        late_max > early_max + 0.02,
        "key-on produced no audible output: early={early_max} late={late_max}"
    );
}

// The Z80 reset line ($A11200) drives the YM2612 /IC pin: `ic_reset` clears FM
// synthesis + the DAC so a game that resets the Z80 to re-init its sound driver
// gets a silenced chip.
#[test]
fn test_ic_reset_silences_fm_chip() {
    let mut ym = Ym2612::new(7_670_454, 44100);
    ym.write_register(0, 0x2B, 0x80); // DAC enable
    ym.set_dac_value(120);
    ym.write_register(0, 0x28, 0xF0); // key on channel 0
    assert!(ym.is_dac_enabled());
    assert!(ym.channel(0).unwrap().operator(0).envelope().is_key_on());

    ym.ic_reset();

    assert!(!ym.is_dac_enabled(), "DAC should be disabled after /IC");
    assert_eq!(ym.get_dac_value(), 0, "DAC value should be cleared");
    for i in 0..4 {
        assert!(
            !ym.channel(0).unwrap().operator(i).envelope().is_key_on(),
            "operator {i} should be keyed off after /IC"
        );
    }
}

// With rendering gated off (emulator audio disabled), `catch_up` must not
// synthesize anything, yet the accumulator stays bounded so re-enabling audio
// doesn't replay a giant backlog.
#[test]
fn test_rendering_disabled_produces_no_samples() {
    let mut ym = Ym2612::new(7_670_454, 44100);
    ym.set_rendering_enabled(false);
    ym.write_register(0, 0x2B, 0x80);
    ym.set_dac_value(120);

    ym.clock(clocks_for_samples(500));

    let mut buf = vec![0.0f32; 256];
    let n = ym.generate_samples(&mut buf, 44100);
    assert_eq!(n, 0, "no samples should be produced while rendering is off");
}

// The optional sound-write log records each FM *data* write (latches are
// not synthesis events) tagged with the in-frame sample offset where it
// takes effect, the same sub-frame placement the catch-up model renders,
// which is what makes the log diffable against a reference core.
#[test]
fn test_write_log_captures_part_register_and_sample_position() {
    let mut ym = Ym2612::new(7_670_454, 44100);
    ym.set_rendering_enabled(true);
    ym.enable_write_log();

    // Data write at frame start (nothing clocked yet) -> sample_pos 0.
    ym.write_port(0, 0x28); // latch reg 0x28 on part 0 (not logged)
    ym.write_port(1, 0xF0); // data -> logged

    // Render ~100 samples, then a part-1 data write -> sample_pos ~100.
    ym.clock(clocks_for_samples(100));
    ym.write_port(2, 0xB6); // latch reg 0xB6 on part 1 (not logged)
    ym.write_port(3, 0xC0); // data -> logged

    let log = ym.take_write_log();
    assert_eq!(log.len(), 2, "only the two data writes are logged");

    assert_eq!(log[0].part, 0);
    assert_eq!(log[0].register, 0x28);
    assert_eq!(log[0].value, 0xF0);
    assert_eq!(log[0].sample_pos, 0, "first write is at frame start");

    assert_eq!(log[1].part, 1);
    assert_eq!(log[1].register, 0xB6);
    assert_eq!(log[1].value, 0xC0);
    assert!(
        (90..=110).contains(&log[1].sample_pos),
        "second write should land near sample 100, got {}",
        log[1].sample_pos
    );

    // Draining leaves capture enabled but empties the buffer.
    assert!(ym.take_write_log().is_empty());
}

// With the log never enabled, writes allocate nothing and the drain is empty.
#[test]
fn test_write_log_disabled_by_default() {
    let mut ym = Ym2612::new(7_670_454, 44100);
    ym.set_rendering_enabled(true);
    ym.write_port(0, 0x28);
    ym.write_port(1, 0xF0);
    assert!(ym.take_write_log().is_empty());
}

/// Status must be readable from every port: data ports mirror status even
/// though they are write-only for register values (E-Swat polls `$4001`).
#[test]
fn test_read_port_returns_status_on_all_ports() {
    let mut ym = Ym2612::new(7_670_454, 44100);
    ym.current_cpu_cycle = 1000;
    ym.write_port(0, 0x2A);
    ym.write_port(1, 0x80); // data write -> busy

    for port in 0u8..=3 {
        assert_eq!(
            ym.read_port(port) & 0x80,
            0x80,
            "port {port} must mirror busy status after a data write"
        );
    }
}

/// Busy clears when the chip is clocked, even if `current_cpu_cycle` is
/// frozen, the Z80-burst case where only `clock()` advances time.
#[test]
fn test_busy_clears_via_clock_without_advancing_cpu_cycle() {
    let mut ym = Ym2612::new(7_670_454, 44100);
    ym.current_cpu_cycle = 1000;
    ym.write_port(0, 0x2A);
    ym.write_port(1, 0x40);

    assert_eq!(ym.get_status() & 0x80, 0x80, "busy set after data write");

    // Mid-busy: clock some but not all of the 192-cycle window. Leave
    // current_cpu_cycle untouched so a timestamp-only check would still
    // report busy forever.
    ym.clock(100);
    assert_eq!(
        ym.get_status() & 0x80,
        0x80,
        "still busy before the full window elapses"
    );
    assert_eq!(ym.current_cpu_cycle, 1000);

    ym.clock(92);
    assert_eq!(
        ym.get_status() & 0x80,
        0x00,
        "busy must clear from clock() alone"
    );
    assert_eq!(ym.current_cpu_cycle, 1000);

    // Same contract through the data-port mirror E-Swat uses.
    assert_eq!(ym.read_port(1) & 0x80, 0x00);
}
