//! Mini ROM tests for PSG
//!
//! These tests simulate Z80 code writing to the PSG port,
//! validating PSG behavior as it would be accessed in real games.

use retro_core::audio::{AudioChip, AudioDebugger};
use retro_core::cpu::{Addressable, Clockable};
use retro_core::memory::Ram;
use sg1_core::psg::Psg;
use sg1_core::z80::Z80;

const TEST_CLOCK: u32 = 3_579_545;
const TEST_SAMPLE_RATE: u32 = 44100;

// Helper to create a simple bus with RAM and PSG
struct SimpleBus {
    ram: Ram,
    psg: Psg,
}

impl SimpleBus {
    fn new() -> Self {
        Self {
            ram: Ram::new(0x2000), // 8KB Z80 RAM
            psg: Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE),
        }
    }

    fn get_psg(&self) -> &Psg {
        &self.psg
    }

    fn generate_psg_samples(&mut self, output: &mut [f32], sample_rate: u32) -> usize {
        // CRITICAL: Must clock PSG before generating samples
        // calculate cycles needed for requested samples
        // cycles_per_sample = clock_freq / sample_rate
        // total_cycles = cycles_per_sample * sample_count
        let cycles_needed = ((TEST_CLOCK as f32 / sample_rate as f32) * output.len() as f32) as u32;

        // Add some buffer to ensure enough cycles
        self.psg.clock(cycles_needed + 1000);

        // Now generate samples
        self.psg.generate_samples(output, sample_rate)
    }
}

impl Addressable for SimpleBus {
    fn read(&mut self, addr: u32) -> u8 {
        match addr & 0xFFFF {
            0x0000..=0x1FFF => self.ram.read(addr),
            0x7F00..=0x7FFF => 0xFF, // PSG read returns 0xFF
            _ => 0xFF,
        }
    }

    fn write(&mut self, addr: u32, value: u8) {
        let addr_masked = addr & 0xFFFF;

        // PSG is memory-mapped at 0x7F11 (and mirrors with bit 0 = 1)
        // Genesis Z80 accesses PSG via memory writes, not I/O ports
        if (0x7F00..=0x7FFF).contains(&addr_masked) && ((addr_masked & 0x01) == 1) {
            self.psg.write(value);
            return;
        }

        if let 0x0000..=0x1FFF = addr_masked {
            self.ram.write(addr, value)
        }
    }

    fn read_word(&mut self, addr: u32) -> u16 {
        let high = self.read(addr) as u16;
        let low = self.read(addr + 1) as u16;
        (high << 8) | low
    }

    fn write_word(&mut self, addr: u32, value: u16) {
        self.write(addr, (value >> 8) as u8);
        self.write(addr + 1, value as u8);
    }
}

// Helper to create a mini ROM and execute it
fn execute_mini_rom(rom: &[u8], cycles_limit: u32) -> SimpleBus {
    let mut bus = SimpleBus::new();
    let mut z80 = Z80::new();

    // Load ROM into RAM
    for (i, &byte) in rom.iter().enumerate() {
        bus.ram.write(i as u32, byte);
    }

    z80.pc = 0;

    // Execute until HALT or cycles limit
    let mut total_cycles = 0;
    while total_cycles < cycles_limit && !z80.halted {
        let cycles = z80.step(&mut bus);
        total_cycles += cycles;
    }

    bus
}

#[test]
fn test_mini_rom_single_tone() {
    // Mini ROM: Z80 plays a single tone on channel 0
    // NOTE: PSG is memory-mapped at 0x7F11, not I/O port
    let rom = &[
        // Set frequency to 440 Hz (freq_value ~ 254 = 0x0FE)
        0x3E, 0x8E, // LD A, 0x8E (latch ch0, tone, low bits = 0xE)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        0x3E, 0x0F, // LD A, 0x0F (high bits)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        // Set volume to max (attenuation = 0)
        0x3E, 0x90, // LD A, 0x90 (latch ch0, volume = 0)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        0x76, // HALT
    ];

    let mut bus = execute_mini_rom(rom, 1000);

    // Verify PSG state
    let psg = bus.get_psg();
    let state = psg.get_channel_state(0).unwrap();

    assert!(
        state.enabled,
        "Channel 0 should be enabled (attenuation = {}, freq_raw = 0x{:03X})",
        state.attenuation, state.frequency_raw
    );
    assert_eq!(state.attenuation, 0, "Channel 0 should be at max volume");
    assert_eq!(
        state.frequency_raw, 0x0FE,
        "Channel 0 frequency should be 0x0FE"
    );

    // Verify audio generation
    let mut samples = vec![0.0; 1024];
    bus.generate_psg_samples(&mut samples, TEST_SAMPLE_RATE);

    let max_amplitude = samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    assert!(
        max_amplitude > 0.15,
        "Should generate audio (got {})",
        max_amplitude
    );
}

#[test]
fn test_mini_rom_chord() {
    // Mini ROM: Play a C major chord (3 channels)
    let rom = &[
        // Channel 0: C (261.63 Hz, freq_value ~ 428 = 0x1AC)
        0x3E, 0x8C, // LD A, 0x8C (ch0, low bits = 0xC)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        0x3E, 0x1A, // LD A, 0x1A (high bits)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        0x3E, 0x90, // LD A, 0x90 (ch0, volume = 0)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        // Channel 1: E (329.63 Hz, freq_value ~ 340 = 0x154)
        0x3E, 0xA4, // LD A, 0xA4 (ch1, low bits = 0x4)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        0x3E, 0x15, // LD A, 0x15 (high bits)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        0x3E, 0xB0, // LD A, 0xB0 (ch1, volume = 0)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        // Channel 2: G (392.00 Hz, freq_value ~ 286 = 0x11E)
        0x3E, 0xCE, // LD A, 0xCE (ch2, low bits = 0xE)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        0x3E, 0x11, // LD A, 0x11 (high bits)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        0x3E, 0xD0, // LD A, 0xD0 (ch2, volume = 0)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        0x76, // HALT
    ];

    let mut bus = execute_mini_rom(rom, 2000);

    // Verify all 3 channels are active
    let psg = bus.get_psg();
    for channel in 0..3 {
        let state = psg.get_channel_state(channel).unwrap();
        assert!(state.enabled, "Channel {} should be enabled", channel);
        assert_eq!(
            state.attenuation, 0,
            "Channel {} should be at max volume",
            channel
        );
    }

    // Verify audio generation
    let mut samples = vec![0.0; 1024];
    bus.generate_psg_samples(&mut samples, TEST_SAMPLE_RATE);

    let max_amplitude = samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    assert!(
        max_amplitude > 0.15,
        "Chord should generate audio (got {})",
        max_amplitude
    );
}

#[test]
fn test_mini_rom_noise() {
    // Mini ROM: Generate white noise
    let rom = &[
        // CRITICAL: Configure Tone2 (channel 2) for Tone3 rate noise
        0x3E, 0xCE, // LD A, 0xCE (ch2, freq low = 0xE)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        0x3E, 0x01, // LD A, 0x01 (ch2, freq high = 0x1) -> 0x01E = 30
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        // Configure noise: white, Tone3 rate
        0x3E, 0xE7, // LD A, 0xE7 (noise, control = 0x7)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        // Set noise volume to max
        0x3E, 0xF0, // LD A, 0xF0 (noise, volume = 0)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        0x76, // HALT
    ];

    let mut bus = execute_mini_rom(rom, 1000);

    // Verify noise channel state
    let psg = bus.get_psg();
    let state = psg.get_channel_state(3).unwrap();

    assert!(state.enabled, "Noise channel should be enabled");
    assert_eq!(state.attenuation, 0, "Noise should be at max volume");
    assert!(state.noise_params.is_some(), "Should have noise parameters");

    let params = state.noise_params.unwrap();
    assert_eq!(params.mode, retro_core::audio::NoiseMode::White);

    // Verify audio generation
    let mut samples = vec![0.0; 1024];
    bus.generate_psg_samples(&mut samples, TEST_SAMPLE_RATE);

    let max_amplitude = samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    assert!(
        max_amplitude > 0.15,
        "Noise should generate audio (got {})",
        max_amplitude
    );
}

#[test]
fn test_mini_rom_volume_control() {
    // Mini ROM: Set a tone and vary its volume
    let rom = &[
        // Set frequency
        0x3E, 0x8D, // LD A, 0x8D (ch0, freq low)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        0x3E, 0x01, // LD A, 0x01 (freq high)
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        // Volume = 0 (max)
        0x3E, 0x90, // LD A, 0x90
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        // Volume = 7 (medium)
        0x3E, 0x97, // LD A, 0x97
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        // Volume = 15 (silence)
        0x3E, 0x9F, // LD A, 0x9F
        0x32, 0x11, 0x7F, // LD (0x7F11), A
        0x76, // HALT
    ];

    let bus = execute_mini_rom(rom, 1500);

    // Verify final state (should be silent)
    let psg = bus.get_psg();
    let state = psg.get_channel_state(0).unwrap();

    assert_eq!(state.attenuation, 15, "Should be at min volume (silence)");
    assert!(!state.enabled, "Channel should be disabled (volume = 15)");
}

#[test]
fn test_mini_rom_all_channels_with_noise() {
    // Mini ROM: Use all 4 channels (3 tone + 1 noise)
    let rom = &[
        // Channel 0: 440 Hz
        0x3E, 0x8E, 0x32, 0x11, 0x7F, 0x3E, 0x0F, 0x32, 0x11, 0x7F, 0x3E, 0x90, 0x32, 0x11, 0x7F,
        // Channel 1: 577 Hz (0x0C2; 554 Hz would be 0x0CA)
        0x3E, 0xA2, 0x32, 0x11, 0x7F, 0x3E, 0x0C, 0x32, 0x11, 0x7F, 0x3E, 0xB0, 0x32, 0x11, 0x7F,
        // Channel 2: 659 Hz
        0x3E, 0xCA, 0x32, 0x11, 0x7F, 0x3E, 0x0A, 0x32, 0x11, 0x7F, 0x3E, 0xD0, 0x32, 0x11, 0x7F,
        // Noise: periodic, N512
        0x3E, 0xE0, 0x32, 0x11, 0x7F, 0x3E, 0xF0, 0x32, 0x11, 0x7F, 0x76, // HALT
    ];

    let mut bus = execute_mini_rom(rom, 3000);

    // Verify all channels are active
    let psg = bus.get_psg();
    for channel in 0..4 {
        let state = psg.get_channel_state(channel).unwrap();
        assert!(state.enabled, "Channel {} should be enabled", channel);
    }

    // Verify audio generation
    let mut samples = vec![0.0; 2048];
    bus.generate_psg_samples(&mut samples, TEST_SAMPLE_RATE);

    let max_amplitude = samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    assert!(
        max_amplitude > 0.15,
        "All channels should generate audio (got {})",
        max_amplitude
    );
}

#[test]
fn test_mini_rom_rapid_changes() {
    // Mini ROM: Rapidly change PSG registers (stress test)
    let rom = &[
        // Loop: change frequency rapidly
        0x3E, 0x8D, 0x32, 0x11, 0x7F, // Freq 1
        0x3E, 0x01, 0x32, 0x11, 0x7F, 0x3E, 0x90, 0x32, 0x11, 0x7F, // Volume max
        0x3E, 0x8F, 0x32, 0x11, 0x7F, // Freq 2
        0x3E, 0x02, 0x32, 0x11, 0x7F, 0x3E, 0x95, 0x32, 0x11, 0x7F, // Volume medium
        0x3E, 0x82, 0x32, 0x11, 0x7F, // Freq 3
        0x3E, 0x04, 0x32, 0x11, 0x7F, 0x3E, 0x9F, 0x32, 0x11, 0x7F, // Volume min
        0x76, // HALT
    ];

    let bus = execute_mini_rom(rom, 2000);

    // PSG should still be stable and in final state
    let psg = bus.get_psg();
    let state = psg.get_channel_state(0).unwrap();

    // Final state should be freq=0x042, volume=15
    assert_eq!(state.frequency_raw, 0x042);
    assert_eq!(state.attenuation, 15);
}

#[test]
fn test_mini_rom_frequency_sweep_effect() {
    // Mini ROM: Create a frequency sweep effect
    let rom = &[
        // Start frequency
        0x3E, 0x80, 0x32, 0x11, 0x7F, // Low
        0x3E, 0x10, 0x32, 0x11, 0x7F, // High (0x100)
        0x3E, 0x90, 0x32, 0x11, 0x7F, // Volume
        // Mid frequency
        0x3E, 0x88, 0x32, 0x11, 0x7F, // Low
        0x3E, 0x08, 0x32, 0x11, 0x7F, // High (0x088)
        // High frequency
        0x3E, 0x84, 0x32, 0x11, 0x7F, // Low
        0x3E, 0x04, 0x32, 0x11, 0x7F, // High (0x044)
        0x76, // HALT
    ];

    let bus = execute_mini_rom(rom, 2000);

    // Verify final frequency
    let psg = bus.get_psg();
    let state = psg.get_channel_state(0).unwrap();

    assert_eq!(state.frequency_raw, 0x044);
    assert!(state.enabled);
}
