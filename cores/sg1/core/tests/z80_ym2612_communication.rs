//! Z80 <-> YM2612 communication tests
//!
//! The YM2612 as the Z80 reaches it: memory-mapped on the Z80's bus, its four
//! ports repeated every four bytes from 0x4000 to 0x5FFF.
//!
//! - 0x4000: YM2612 address part 0 / status
//! - 0x4001: YM2612 data part 0
//! - 0x4002: YM2612 address part 1 / status
//! - 0x4003: YM2612 data part 1

use retro_core::audio::AudioChip;
use retro_core::cpu::{Addressable, Clockable};
use sg1_core::bus::AddressBus;

/// A Z80 write to the YM2612's window, `offset` bytes past 0x4000.
fn ym_write(bus: &mut AddressBus, offset: u32, value: u8) {
    bus.create_z80_bus().write(0x4000 + offset, value);
}

/// A Z80 read from the YM2612's window, `offset` bytes past 0x4000.
fn ym_read(bus: &mut AddressBus, offset: u32) -> u8 {
    bus.create_z80_bus().read(0x4000 + offset)
}

#[test]
fn test_z80_can_write_ym2612_address_latch() {
    let mut bus = AddressBus::new();

    // Write to address latch (Part 0)
    ym_write(&mut bus, 0, 0x22); // Register 0x22 (LFO Enable)

    // Write data
    ym_write(&mut bus, 1, 0x08); // Enable LFO

    // Verify YM2612 received the command
    assert!(bus.ym2612().lfo().is_enabled());
}

#[test]
fn test_z80_can_read_ym2612_status() {
    let mut bus = AddressBus::new();

    // Read status (should work on both ports 0x4000 and 0x4002)
    let status1 = ym_read(&mut bus, 0);
    let status2 = ym_read(&mut bus, 2);

    // Status should be same from both parts
    assert_eq!(status1, status2);

    // Initially, no timers overflow, so status should be 0
    assert_eq!(status1, 0x00);
}

#[test]
fn test_ym2612_port_mapping_mirroring() {
    let mut bus = AddressBus::new();

    // The four ports repeat every four bytes up to 0x5FFF:
    // 0x4004 behaves like 0x4000
    ym_write(&mut bus, 0x4, 0x22);
    ym_write(&mut bus, 0x5, 0x08);

    assert!(bus.ym2612().lfo().is_enabled());

    // And so does 0x5FFC, the last repeat
    ym_write(&mut bus, 0x1FFC, 0x22);
    ym_write(&mut bus, 0x1FFD, 0x00); // Disable LFO

    assert!(!bus.ym2612().lfo().is_enabled());
}

#[test]
fn test_ym2612_data_port_read_returns_status() {
    let mut bus = AddressBus::new();

    // This core answers a read on any of the YM2612's four ports with the
    // status register. Genesis Plus GX and ymfm answer it only at port 0 on
    // a discrete YM2612, and at all four on the integrated chip. With no
    // timers armed and the chip idle, status is 0x00 (busy clear, no timer
    // overflow).
    let data0 = ym_read(&mut bus, 1);
    let data1 = ym_read(&mut bus, 3);

    assert_eq!(data0, 0x00);
    assert_eq!(data1, 0x00);
}

#[test]
fn test_ym2612_part0_and_part1_independence() {
    let mut bus = AddressBus::new();

    // Part 0: Write to channel 0 frequency
    ym_write(&mut bus, 0, 0xA0); // F-Number low, channel 0
    ym_write(&mut bus, 1, 0x44);

    // Part 1: Write to channel 3 frequency
    ym_write(&mut bus, 2, 0xA0); // F-Number low, channel 3 (Part 1, ch 0)
    ym_write(&mut bus, 3, 0x88);

    // Verify both were written correctly
    let ch0_freq = bus.ym2612().channel(0).unwrap().operator(0).freq_number;
    let ch3_freq = bus.ym2612().channel(3).unwrap().operator(0).freq_number;

    // Lower 8 bits should match what we wrote
    assert_eq!(ch0_freq & 0xFF, 0x44);
    assert_eq!(ch3_freq & 0xFF, 0x88);
}

#[test]
fn test_ym2612_lfo_enable_via_z80() {
    let mut bus = AddressBus::new();

    // LFO should be disabled initially
    assert!(!bus.ym2612().lfo().is_enabled());

    // Enable LFO (Register 0x22, value 0x08)
    ym_write(&mut bus, 0, 0x22);
    ym_write(&mut bus, 1, 0x08);

    assert!(bus.ym2612().lfo().is_enabled());

    // Disable LFO
    ym_write(&mut bus, 0, 0x22);
    ym_write(&mut bus, 1, 0x00);

    assert!(!bus.ym2612().lfo().is_enabled());
}

#[test]
fn test_ym2612_dac_enable_via_z80() {
    let mut bus = AddressBus::new();

    // DAC should be disabled initially
    assert!(!bus.ym2612().is_dac_enabled());

    // Enable DAC (Register 0x2B, bit 7)
    ym_write(&mut bus, 0, 0x2B);
    ym_write(&mut bus, 1, 0x80);

    assert!(bus.ym2612().is_dac_enabled());

    // Disable DAC
    ym_write(&mut bus, 0, 0x2B);
    ym_write(&mut bus, 1, 0x00);

    assert!(!bus.ym2612().is_dac_enabled());
}

#[test]
fn test_ym2612_key_on_via_z80() {
    let mut bus = AddressBus::new();

    // Key on all operators of channel 0
    // Register 0x28: bits 7-4 = operator mask, bits 2-0 = channel
    ym_write(&mut bus, 0, 0x28);
    ym_write(&mut bus, 1, 0xF0); // All ops (0xF0), channel 0

    // Verify operators are keyed on
    let channel = bus.ym2612().channel(0).unwrap();
    for op in 0..4 {
        assert!(
            channel.operator(op).envelope().is_key_on(),
            "Operator {} should be keyed on",
            op
        );
    }

    // Key off
    ym_write(&mut bus, 0, 0x28);
    ym_write(&mut bus, 1, 0x00); // No ops, channel 0

    // Verify operators are keyed off
    let channel = bus.ym2612().channel(0).unwrap();
    for op in 0..4 {
        assert!(
            !channel.operator(op).envelope().is_key_on(),
            "Operator {} should be keyed off",
            op
        );
    }
}

#[test]
fn test_ym2612_multiple_register_writes() {
    let mut bus = AddressBus::new();

    // Set multiple registers in sequence
    // 1. LFO Enable
    ym_write(&mut bus, 0, 0x22);
    ym_write(&mut bus, 1, 0x08);

    // 2. DAC Enable
    ym_write(&mut bus, 0, 0x2B);
    ym_write(&mut bus, 1, 0x80);

    // 3. Set algorithm for channel 0
    ym_write(&mut bus, 0, 0xB0);
    ym_write(&mut bus, 1, 0x07); // Algorithm 7

    // Verify all writes were processed
    assert!(bus.ym2612().lfo().is_enabled());
    assert!(bus.ym2612().is_dac_enabled());
    let algo = bus.ym2612().channel(0).unwrap().get_algorithm();
    assert_eq!(algo as u8, 7);
}

#[test]
fn test_ym2612_status_timer_flags() {
    let mut bus = AddressBus::new();

    // Initially no overflow
    let status = ym_read(&mut bus, 0);
    assert_eq!(status & 0x03, 0x00); // Bits 0-1 should be 0

    // Configure Timer A with value 0x3FF (1023) for shortest period.
    // Register 0x24 holds the high 8 bits (T9..T2); register 0x25 holds
    // the low 2 bits (T1..T0). So 0x3FF = 0xFF at 0x24, 0x03 at 0x25.
    ym_write(&mut bus, 0, 0x24); // Timer A high (bits 9-2)
    ym_write(&mut bus, 1, 0xFF);
    ym_write(&mut bus, 0, 0x25); // Timer A low (bits 1-0)
    ym_write(&mut bus, 1, 0x03);

    // Enable Timer A. Register 0x27 needs bit 0 (Load/run Timer A) AND
    // bit 2 (status flag enable) to both run the counter and surface
    // the overflow in the status register.
    ym_write(&mut bus, 0, 0x27); // Control register
    ym_write(&mut bus, 1, 0x05); // bit 0 (run) | bit 2 (status enable)

    // `clock()` advances the timers; draining samples does not.
    // With a Timer A value of 1023 (period = 1 internal sample), the
    // overflow happens on the very first sample.
    let mut scratch = [0.0f32; 32];
    bus.ym2612_mut().clock(20_000); // accumulate enough cycles
    bus.ym2612_mut().generate_samples(&mut scratch, 44100);

    // Read status - Timer A should have overflowed
    let status = ym_read(&mut bus, 0);
    assert_ne!(status & 0x01, 0, "Timer A should have overflowed");
}

#[test]
fn test_z80_ym2612_channel_frequency_programming() {
    let mut bus = AddressBus::new();

    // Program channel 0 with specific frequency
    // F-num = 0x284, Block = 4 (~262 Hz, C4)

    // Write F-num low (0xA0 for channel 0)
    ym_write(&mut bus, 0, 0xA0);
    ym_write(&mut bus, 1, 0x84);

    // Write F-num high + Block (0xA4 for channel 0)
    ym_write(&mut bus, 0, 0xA4);
    ym_write(&mut bus, 1, (4 << 3) | 0x02); // Block 4, F-num high bits

    // Verify frequency was set
    let channel = bus.ym2612().channel(0).unwrap();
    assert_eq!(channel.operator(0).freq_number, 0x284);
    assert_eq!(channel.operator(0).block, 4);
}

#[test]
fn test_z80_ym2612_all_channels_independent() {
    let mut bus = AddressBus::new();

    // Program each of the 6 channels with different frequencies
    let frequencies = [0x100, 0x200, 0x300, 0x400, 0x500, 0x600];

    for (ch_idx, &freq) in frequencies.iter().enumerate() {
        let part = if ch_idx < 3 { 0 } else { 1 };
        let ch_offset = (ch_idx % 3) as u8;

        // Write F-num low
        let reg_low = 0xA0 + ch_offset;
        let port_addr = if part == 0 { 0 } else { 2 };
        let port_data = if part == 0 { 1 } else { 3 };

        ym_write(&mut bus, port_addr, reg_low);
        ym_write(&mut bus, port_data, (freq & 0xFF) as u8);

        // Write F-num high + Block
        let reg_high = 0xA4 + ch_offset;
        ym_write(&mut bus, port_addr, reg_high);
        ym_write(&mut bus, port_data, ((freq >> 8) & 0x07) as u8);
    }

    // Verify each channel has its own frequency
    for (ch_idx, &expected_freq) in frequencies.iter().enumerate() {
        let channel = bus.ym2612().channel(ch_idx).unwrap();
        assert_eq!(
            channel.operator(0).freq_number,
            expected_freq,
            "Channel {} frequency mismatch",
            ch_idx
        );
    }
}

#[test]
fn test_z80_io_ports_are_unconnected() {
    // The Mega Drive leaves the Z80's I/O space unconnected: an OUT reaches
    // nothing and an IN reads 0xFF, so the YM2612 is reached through memory
    // only.
    let mut bus = AddressBus::new();
    let mut z80_bus = bus.create_z80_bus();
    z80_bus.io_write(0x40, 0x22);
    z80_bus.io_write(0x41, 0x08);
    assert_eq!(z80_bus.io_read(0x40), 0xFF);
    assert!(!bus.ym2612().lfo().is_enabled());
}
