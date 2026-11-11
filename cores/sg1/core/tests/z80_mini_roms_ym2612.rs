//! Z80 Mini ROMs - YM2612 Control Tests
//!
//! Tests with REAL Z80 programs that run and control the YM2612.
//! They cover what the unit tests do not: the whole path from a Z80
//! program through the Z80 bus to the YM2612.

mod helpers;

use helpers::{Z80GenesisTestRunner, Z80RomBuilder};
use retro_core::audio::AudioChip;
use retro_core::cpu::Clockable;

// ============================================================================
// Test 1: Basic LFO Control
// ============================================================================

#[test]
fn test_minirom_lfo_enable() {
    let rom = Z80RomBuilder::new(0x0000)
        // Enable LFO (register 0x22 = 0x08)
        .ym_write_part0(0x22, 0x08)
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    assert!(runner.ym2612().lfo().is_enabled());
}

#[test]
fn test_minirom_lfo_disable() {
    let rom = Z80RomBuilder::new(0x0000)
        // Enable LFO
        .ym_write_part0(0x22, 0x08)
        // Disable LFO
        .ym_write_part0(0x22, 0x00)
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    assert!(!runner.ym2612().lfo().is_enabled());
}

#[test]
fn test_minirom_lfo_frequency() {
    // Test all 8 LFO frequencies
    for freq in 0..8 {
        let rom = Z80RomBuilder::new(0x0000)
            .ym_write_part0(0x22, 0x08 | freq) // Enable + frequency
            .halt()
            .build();

        let mut runner = Z80GenesisTestRunner::new();
        runner.load_rom(&rom, 0x0000);
        runner.run_until_halt().expect("Should halt");

        assert!(runner.ym2612().lfo().is_enabled());
        assert_eq!(runner.ym2612().lfo().get_frequency(), freq);
    }
}

// ============================================================================
// Test 2: DAC Mode Control
// ============================================================================

#[test]
fn test_minirom_dac_enable() {
    let rom = Z80RomBuilder::new(0x0000)
        // Enable DAC (register 0x2B = 0x80)
        .ym_write_part0(0x2B, 0x80)
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    assert!(runner.ym2612().is_dac_enabled());
}

#[test]
fn test_minirom_dac_disable() {
    let rom = Z80RomBuilder::new(0x0000)
        .ym_write_part0(0x2B, 0x80) // Enable DAC
        .ym_write_part0(0x2B, 0x00) // Disable DAC
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    assert!(!runner.ym2612().is_dac_enabled());
}

#[test]
fn test_minirom_dac_write_samples() {
    let rom = Z80RomBuilder::new(0x0000)
        .ym_write_part0(0x2B, 0x80) // Enable DAC
        .ym_write_part0(0x2A, 0x00) // Write sample 0x00
        .ym_write_part0(0x2A, 0x7F) // Write sample 0x7F
        .ym_write_part0(0x2A, 0xFF) // Write sample 0xFF
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    assert!(runner.ym2612().is_dac_enabled());
    // DAC sample should be the last written value
    assert_eq!(runner.ym2612().get_dac_sample(), 0xFF);
}

// ============================================================================
// Test 3: Key On/Off Control
// ============================================================================

#[test]
fn test_minirom_key_on_channel_0() {
    let rom = Z80RomBuilder::new(0x0000)
        // Key on channel 0, all operators (0x28 = 0xF0)
        .ym_write_part0(0x28, 0xF0)
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    let ch = runner.ym2612().channel(0).unwrap();
    for op_idx in 0..4 {
        assert!(
            ch.operator(op_idx).is_key_on(),
            "Operator {} should be key on",
            op_idx
        );
    }
}

#[test]
fn test_minirom_key_off_channel_0() {
    let rom = Z80RomBuilder::new(0x0000)
        .ym_write_part0(0x28, 0xF0) // Key on all
        .ym_write_part0(0x28, 0x00) // Key off all
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    let ch = runner.ym2612().channel(0).unwrap();
    for op_idx in 0..4 {
        assert!(
            !ch.operator(op_idx).is_key_on(),
            "Operator {} should be key off",
            op_idx
        );
    }
}

#[test]
fn test_minirom_key_on_individual_operators() {
    // Test each operator individually for channel 0
    for op_mask in [0x10, 0x20, 0x40, 0x80] {
        let rom = Z80RomBuilder::new(0x0000)
            .ym_write_part0(0x28, op_mask) // Key on single operator
            .halt()
            .build();

        let mut runner = Z80GenesisTestRunner::new();
        runner.load_rom(&rom, 0x0000);
        runner.run_until_halt().expect("Should halt");

        // YM2612 register 0x28 has a non-standard slot ordering:
        //   bit 4 = SLOT1 (op 0)
        //   bit 5 = SLOT3 (op 2)
        //   bit 6 = SLOT2 (op 1)
        //   bit 7 = SLOT4 (op 3)
        // (See the Yamaha datasheet; Genesis Plus GX orders them the same.)
        let expected_op = match op_mask {
            0x10 => 0,
            0x20 => 2,
            0x40 => 1,
            0x80 => 3,
            _ => unreachable!(),
        };

        let ch = runner.ym2612().channel(0).unwrap();
        for op_idx in 0..4 {
            if op_idx == expected_op {
                assert!(
                    ch.operator(op_idx).is_key_on(),
                    "Operator {} should be key on",
                    op_idx
                );
            } else {
                assert!(
                    !ch.operator(op_idx).is_key_on(),
                    "Operator {} should be key off",
                    op_idx
                );
            }
        }
    }
}

// ============================================================================
// Test 4: Timer Control
// ============================================================================

#[test]
fn test_minirom_timer_a_enable() {
    let rom = Z80RomBuilder::new(0x0000)
        // Set Timer A period to 0x3FF (1023) for shortest possible
        // overflow. Register 0x24 = high 8 bits (T9..T2), register 0x25
        // = low 2 bits (T1..T0).
        .ym_write_part0(0x24, 0xFF)
        .ym_write_part0(0x25, 0x03)
        // Enable Timer A: bit 0 (Load/run) + bit 2 (status flag enable).
        .ym_write_part0(0x27, 0x05)
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    // `clock()` runs the timers (`clock_timers`, at most eight timer ticks
    // per call), so Timer A overflows here; draining samples does not tick them.
    let mut scratch = [0.0f32; 32];
    runner.ym2612_mut().clock(20_000);
    runner.ym2612_mut().generate_samples(&mut scratch, 44100);

    // Verify Timer A overflow via YM2612 status
    let status = runner.ym2612().get_status();
    assert_ne!(status & 0x01, 0, "Timer A should have overflowed");
}

#[test]
fn test_minirom_timer_b_enable() {
    let rom = Z80RomBuilder::new(0x0000)
        // Set Timer B period (very short)
        .ym_write_part0(0x26, 0xFF) // Period = 0xFF = 255
        // Enable Timer B: bit 1 (Load/run) + bit 3 (status flag enable).
        .ym_write_part0(0x27, 0x0A)
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    // Timer B is clocked inside `clock_timers`, which caps the internal
    // accumulator at MAX_TIMER_ACCUMULATOR (1152 input clocks ~ 8 ticks
    // per call). Timer B with period 0xFF needs 16 ticks to overflow,
    // so we drain in chunks.
    let mut scratch = [0.0f32; 4096];
    for _ in 0..20 {
        runner.ym2612_mut().clock(1024);
    }
    runner.ym2612_mut().generate_samples(&mut scratch, 44100);

    // Verify Timer B overflow via YM2612 status
    let status = runner.ym2612().get_status();
    assert_ne!(status & 0x02, 0, "Timer B should have overflowed");
}

#[test]
fn test_minirom_timer_reset_flags() {
    let rom = Z80RomBuilder::new(0x0000)
        // Set Timer A period to 0x3FF (1023), see test_minirom_timer_a_enable.
        .ym_write_part0(0x24, 0xFF)
        .ym_write_part0(0x25, 0x03)
        // Enable Timer A: bit 0 (run) + bit 2 (status enable).
        .ym_write_part0(0x27, 0x05)
        // Wait for overflow (will be done outside ROM)
        .nop()
        .nop()
        .nop()
        // Reset Timer A overflow flag: bit 4 is "Reset Timer A" (bit 5
        // is "Reset Timer B"). Combine with bits 0+2 (run + status
        // enable) to keep the timer alive: 0x05 | 0x10 = 0x15.
        .ym_write_part0(0x27, 0x15)
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);

    // Execute until first NOPs (using run_instructions to avoid borrow issues)
    runner
        .run_instructions(12)
        .expect("Should run instructions");

    // `clock()` ticks Timer A; draining samples does not.
    let mut scratch = [0.0f32; 32];
    runner.ym2612_mut().clock(20_000);
    runner.ym2612_mut().generate_samples(&mut scratch, 44100);

    // Verify overflow flag is set
    let status_before = runner.ym2612().get_status();
    assert_ne!(status_before & 0x01, 0, "Timer A should have overflowed");

    // Continue execution (reset flag)
    runner.run_until_halt().expect("Should halt");

    // Verify flag was reset
    let status_after = runner.ym2612().get_status();
    assert_eq!(
        status_after & 0x01,
        0,
        "Timer A overflow flag should be reset"
    );
}

// ============================================================================
// Test 5: Channel Frequency Programming
// ============================================================================

#[test]
fn test_minirom_channel_frequency() {
    let rom = Z80RomBuilder::new(0x0000)
        // Program frequency for channel 0
        // F-Number = 0x123 (low 8 bits = 0x23, high 3 bits = 0x01)
        // Block = 5
        .ym_write_part0(0xA0, 0x23) // F-Number low
        .ym_write_part0(0xA4, 0x29) // Block (5) | F-Number high (1) = (5<<3)|1 = 0x29
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    let ch = runner.ym2612().channel(0).unwrap();
    // Check all operators have the frequency (in normal mode)
    for op_idx in 0..4 {
        let op = ch.operator(op_idx);
        assert_eq!(op.get_freq_number(), 0x123);
        assert_eq!(op.get_block(), 5);
    }
}

#[test]
fn test_minirom_all_channels_frequency() {
    let rom = Z80RomBuilder::new(0x0000)
        // Channel 0 (Part 0): F-Number = 0x100, Block = 1 -> 0xA4 = (1<<3)|1 = 0x09
        .ym_write_part0(0xA0, 0x00)
        .ym_write_part0(0xA4, 0x09)
        // Channel 1 (Part 0): F-Number = 0x211, Block = 2 -> 0xA5 = (2<<3)|2 = 0x12
        .ym_write_part0(0xA1, 0x11)
        .ym_write_part0(0xA5, 0x12)
        // Channel 2 (Part 0): F-Number = 0x322, Block = 3 -> 0xA6 = (3<<3)|3 = 0x1B
        .ym_write_part0(0xA2, 0x22)
        .ym_write_part0(0xA6, 0x1B)
        // Channel 3 (Part 1): F-Number = 0x433, Block = 4 -> 0xA4 = (4<<3)|4 = 0x24
        .ym_write_part1(0xA0, 0x33)
        .ym_write_part1(0xA4, 0x24)
        // Channel 4 (Part 1): F-Number = 0x544, Block = 5 -> 0xA5 = (5<<3)|5 = 0x2D
        .ym_write_part1(0xA1, 0x44)
        .ym_write_part1(0xA5, 0x2D)
        // Channel 5 (Part 1): F-Number = 0x655, Block = 6 -> 0xA6 = (6<<3)|6 = 0x36
        .ym_write_part1(0xA2, 0x55)
        .ym_write_part1(0xA6, 0x36)
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    // Verify each channel has correct frequency
    let expected = [
        (0x100, 1),
        (0x211, 2),
        (0x322, 3),
        (0x433, 4),
        (0x544, 5),
        (0x655, 6),
    ];

    for (ch_idx, (expected_fnum, expected_block)) in expected.iter().enumerate() {
        let ch = runner.ym2612().channel(ch_idx).unwrap();
        let op = ch.operator(0); // Check operator 0
        assert_eq!(
            op.get_freq_number(),
            *expected_fnum,
            "Channel {} F-Number mismatch",
            ch_idx
        );
        assert_eq!(
            op.get_block(),
            *expected_block,
            "Channel {} Block mismatch",
            ch_idx
        );
    }
}

// ============================================================================
// Test 6: Operator Parameters
// ============================================================================

#[test]
fn test_minirom_operator_total_level() {
    let rom = Z80RomBuilder::new(0x0000)
        // Set Total Level for channel 0, operator 0 (register 0x40)
        .ym_write_part0(0x40, 0x7F) // Max attenuation
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    let ch = runner.ym2612().channel(0).unwrap();
    let op = ch.operator(0);
    assert_eq!(op.get_total_level(), 0x7F);
}

#[test]
fn test_minirom_operator_multiple_detune() {
    let rom = Z80RomBuilder::new(0x0000)
        // Set Multiple (4) and Detune (3) for channel 0, operator 0 (register 0x30)
        // Format: [DT2:1] [DT:3] [MUL:4]
        // DT2 = 0 (not used in YM2612), DT = 3, MUL = 4
        // Value = 0x34 (0b00110100)
        .ym_write_part0(0x30, 0x34)
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    let ch = runner.ym2612().channel(0).unwrap();
    let op = ch.operator(0);
    assert_eq!(op.get_multiple(), 4);
    assert_eq!(op.get_detune(), 3);
}

#[test]
fn test_minirom_operator_rate_scaling_ams() {
    let rom = Z80RomBuilder::new(0x0000)
        // Set RS (2) and AR (15) for channel 0, operator 0 (register 0x50)
        // Format: [RS:2 bits 7-6] [unused] [AR:5 bits 4-0]
        // RS=2 (10), AR=15 (01111) -> 0b10001111 = 0x8F
        .ym_write_part0(0x50, 0x8F)
        // AMS lives in register 0xB4 (per channel), NOT 0x60. Bits 5-4
        // carry AMS, bit 7 = L enable, bit 6 = R enable.
        // L=1, R=1, AMS=3, PMS=0 -> 0b11110000 = 0xF0
        .ym_write_part0(0xB4, 0xF0)
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    let ch = runner.ym2612().channel(0).unwrap();
    let op = ch.operator(0);
    assert_eq!(op.envelope().get_attack_rate(), 15);
    assert_eq!(op.get_rate_scaling(), 2);
    // AMS is per-channel; the chip mirrors it to all operators.
    assert_eq!(op.get_ams(), 3);
}

// ============================================================================
// Test 7: Channel Algorithm and Feedback
// ============================================================================

#[test]
fn test_minirom_channel_algorithm() {
    // Test all 8 algorithms for channel 0
    for algo in 0..8 {
        let rom = Z80RomBuilder::new(0x0000)
            // Set algorithm (register 0xB0)
            // Format: [FB:3] [ALGO:3]
            .ym_write_part0(0xB0, algo)
            .halt()
            .build();

        let mut runner = Z80GenesisTestRunner::new();
        runner.load_rom(&rom, 0x0000);
        runner.run_until_halt().expect("Should halt");

        let ch = runner.ym2612().channel(0).unwrap();
        assert_eq!(ch.get_algorithm() as u8, algo);
    }
}

#[test]
fn test_minirom_channel_feedback() {
    // Test all 8 feedback levels for channel 0
    for fb in 0..8 {
        let rom = Z80RomBuilder::new(0x0000)
            // Set feedback (register 0xB0)
            // Format: [FB:3] [ALGO:3]
            .ym_write_part0(0xB0, fb << 3)
            .halt()
            .build();

        let mut runner = Z80GenesisTestRunner::new();
        runner.load_rom(&rom, 0x0000);
        runner.run_until_halt().expect("Should halt");

        let ch = runner.ym2612().channel(0).unwrap();
        assert_eq!(ch.get_feedback(), fb);
    }
}

#[test]
fn test_minirom_channel_stereo() {
    let rom = Z80RomBuilder::new(0x0000)
        // Set stereo output for channel 0 (register 0xB4)
        // Format: [L:1] [R:1] [AMS:2] [PMS:3]
        .ym_write_part0(0xB4, 0xC0) // Both L and R enabled
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    let ch = runner.ym2612().channel(0).unwrap();
    assert!(ch.is_left_enabled());
    assert!(ch.is_right_enabled());
}

// ============================================================================
// Test 8: Complex Sequences
// ============================================================================

#[test]
fn test_minirom_full_channel_setup() {
    let rom = Z80RomBuilder::new(0x0000)
        // Setup channel 0 completely
        // 1. Set algorithm and feedback
        .ym_write_part0(0xB0, 0x07) // Algorithm 7
        // 2. Set stereo and PMS/AMS
        .ym_write_part0(0xB4, 0xC0) // Both speakers
        // 3. Set frequency
        .ym_write_part0(0xA0, 0x00)
        .ym_write_part0(0xA4, 0x30) // Block 6 (bits 5-3)
        // 4. Setup all 4 operators
        // Operator 0
        .ym_write_part0(0x30, 0x01) // Multiple 1
        .ym_write_part0(0x40, 0x00) // TL 0
        .ym_write_part0(0x50, 0x1F) // AR max
        .ym_write_part0(0x60, 0x00) // D1R 0
        .ym_write_part0(0x70, 0x00) // D2R 0
        .ym_write_part0(0x80, 0x0F) // RR 15, SL 0
        // Operator 1
        .ym_write_part0(0x34, 0x02) // Multiple 2
        .ym_write_part0(0x44, 0x20) // TL 32
        .ym_write_part0(0x54, 0x1F) // AR max
        .ym_write_part0(0x64, 0x00) // D1R 0
        .ym_write_part0(0x74, 0x00) // D2R 0
        .ym_write_part0(0x84, 0x0F) // RR 15
        // 5. Key on all operators
        .ym_write_part0(0x28, 0xF0)
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    let ch = runner.ym2612().channel(0).unwrap();
    assert_eq!(ch.get_algorithm() as u8, 7);
    assert!(ch.is_left_enabled());
    assert!(ch.is_right_enabled());

    let op0 = ch.operator(0);
    assert!(op0.is_key_on());
    assert_eq!(op0.get_multiple(), 1);
    assert_eq!(op0.get_total_level(), 0);

    let op1 = ch.operator(1);
    assert!(op1.is_key_on());
    assert_eq!(op1.get_multiple(), 2);
    assert_eq!(op1.get_total_level(), 32);
}

#[test]
fn test_minirom_loop_programming() {
    // Use a Z80 loop to program the algorithm of all six channels, through
    // the YM2612's ports in the Z80's memory at 0x4000-0x4003. B counts the
    // three channels of a part down from 3 and C holds their register (0xB0,
    // 0xB1, 0xB2); each pass writes that register on part 0 with algorithm B
    // and on part 1 with algorithm B + 4, so every channel gets its own value.
    let mut builder = Z80RomBuilder::new(0x0000);
    builder.ld_b(3).ld_c(0xB0);
    let loop_start = builder.current_addr();
    builder
        .ld_a_c()
        .ld_nn_a(0x4000) // LD (0x4000), A: part 0 register
        .ld_a_b()
        .ld_nn_a(0x4001) // LD (0x4001), A: part 0 data, algorithm B
        .ld_a_c()
        .ld_nn_a(0x4002) // LD (0x4002), A: part 1 register
        .ld_a_b()
        .add_a(4)
        .ld_nn_a(0x4003) // LD (0x4003), A: part 1 data, algorithm B + 4
        .inc_c();
    // The displacement counts from the instruction after the DJNZ
    let back = loop_start as i32 - (builder.current_addr() as i32 + 2);
    builder.djnz(back as i8).halt();
    let rom = builder.build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);
    runner.run_until_halt().expect("Should halt");

    assert_eq!(runner.get_b(), 0); // Counter should be 0

    // Part 0's channels took algorithms 3, 2, 1 and part 1's took 7, 6, 5
    let expected = [3u8, 2, 1, 7, 6, 5];
    for (ch_idx, &algorithm) in expected.iter().enumerate() {
        let ch = runner.ym2612().channel(ch_idx).unwrap();
        assert_eq!(
            ch.get_algorithm() as u8,
            algorithm,
            "Channel {} algorithm",
            ch_idx
        );
    }
}
