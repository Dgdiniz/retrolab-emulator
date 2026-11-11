//! SN76489 Programmable Sound Generator (PSG)
//!
//! The SN76489 is a sound chip used in the Sega Genesis/Mega Drive (and many other systems).
//! It provides:
//! - 3 tone channels (square waves)
//! - 1 noise channel (white or periodic)
//! - 4-bit attenuation per channel (16 volume levels)
//!
//! The PSG is clocked at Master Clock / 15 (~3.58 MHz on Genesis).

pub mod noise;
pub mod tone;

use noise::NoiseChannel;
use tone::ToneChannel;

use retro_core::audio::{AudioChip, AudioChipInfo, AudioDebugger, ChannelState, ChannelType};
use retro_core::cpu::Clockable;

/// Upper bound on buffered, undrained mono samples. If the host stops consuming
/// audio (disabled, debugger paused, tab backgrounded), `catch_up()` trims the
/// oldest samples instead of growing without bound.
const MAX_PSG_FRAME_SAMPLES: usize = 8192;

/// Latch type for the SN76489 register write protocol
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LatchType {
    Tone,
    Volume,
}

/// SN76489 PSG chip
///
/// The PSG uses a latch-based register write protocol:
/// - First byte with bit 7 set: Latch channel/register and write low bits
/// - Subsequent bytes with bit 7 clear: Write high bits to latched register
pub struct Psg {
    /// 3 tone channels
    tone_channels: [ToneChannel; 3],

    /// 1 noise channel
    noise_channel: NoiseChannel,

    /// Master clock frequency
    clock_freq: u32,

    /// Sample rate for audio generation
    sample_rate: u32,

    /// Accumulated clock cycles (for sample generation timing)
    clock_accumulator: f32,

    /// Cycles per sample
    cycles_per_sample: f32,

    /// Latched channel (0-3: tone 0-2, noise)
    latched_channel: u8,

    /// Latched register type
    latched_type: LatchType,

    /// Waveform buffer for visualization (last N samples per channel) -- ring buffers
    waveform_buffer: [std::collections::VecDeque<f32>; 4],

    /// Mixed output waveform buffer -- ring buffer
    mixed_waveform: std::collections::VecDeque<f32>,

    /// Maximum waveform samples to store
    max_waveform_samples: usize,

    /// Internal clock accumulator for the /16 divider.
    /// The SN76489 divides its input clock by 16 for the internal tone/noise generators.
    /// Channels should be clocked at ~223.7 kHz (PSG_clock / 16), not at the output sample rate.
    internal_clock_accumulator: f32,

    /// Mono samples already rendered this frame by `catch_up()`. The chip renders
    /// the audio produced *before* each register write using the state in effect
    /// at that moment, so sub-frame frequency/volume/noise changes land at their
    /// correct sample position instead of collapsing to the frame's final state.
    /// Drained by `generate_samples()`; never serialized (transient per frame).
    frame_samples: Vec<f32>,

    /// Running sample counter used to decimate `store_waveform_sample()` to every
    /// 16th sample, keeping the visualization ring buffer covering ~0.4 s of audio
    /// even though `catch_up()` renders across many calls per frame.
    waveform_decimation: u32,

    /// When false (host audio disabled), `catch_up()` skips synthesis and just
    /// keeps the accumulator bounded, so no CPU is spent on audio nobody hears.
    /// Not serialized: it is a host setting re-applied via `set_rendering_enabled`.
    rendering_enabled: bool,

    /// Optional capture of every byte written to the PSG, tagged with the
    /// in-frame output-sample offset at which it takes effect. `None` =
    /// disabled (zero overhead). Transient diagnostic for `sg1-cli
    /// sound-trace`; never serialized.
    sound_write_log: Option<Vec<PsgWrite>>,
}

/// A single SN76489 byte write captured by the optional sound-write log.
///
/// `sample_pos` is the (mono) output-sample offset within the current frame
/// at which the write takes effect, how many samples `catch_up()` had
/// already rendered when the byte arrived. `data` is the raw command byte;
/// the SN76489 latch/data encoding (channel, tone/volume, data nibble) lives
/// in the byte itself, so the log stays a faithful record of the wire bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PsgWrite {
    pub sample_pos: u32,
    pub data: u8,
}

impl Psg {
    /// Create a new PSG with given clock frequency and sample rate
    ///
    /// # Arguments
    /// * `clock_freq` - PSG clock frequency (Genesis: Master/15 = ~3.58 MHz)
    /// * `sample_rate` - Audio sample rate (typically 44100 Hz)
    pub fn new(clock_freq: u32, sample_rate: u32) -> Self {
        let cycles_per_sample = clock_freq as f32 / sample_rate as f32;

        Self {
            tone_channels: [
                ToneChannel::new(clock_freq),
                ToneChannel::new(clock_freq),
                ToneChannel::new(clock_freq),
            ],
            noise_channel: NoiseChannel::new(clock_freq),
            clock_freq,
            sample_rate,
            clock_accumulator: 0.0,
            cycles_per_sample,
            latched_channel: 0,
            latched_type: LatchType::Tone,
            waveform_buffer: [
                std::collections::VecDeque::with_capacity(1024),
                std::collections::VecDeque::with_capacity(1024),
                std::collections::VecDeque::with_capacity(1024),
                std::collections::VecDeque::with_capacity(1024),
            ],
            mixed_waveform: std::collections::VecDeque::with_capacity(1024),
            max_waveform_samples: 1024,
            internal_clock_accumulator: 0.0,
            frame_samples: Vec::with_capacity(2048),
            waveform_decimation: 0,
            rendering_enabled: true,
            sound_write_log: None,
        }
    }

    /// Begin capturing PSG byte writes (see [`PsgWrite`]), discarding any
    /// prior capture. Off by default; only the CLI sound-trace diagnostic
    /// turns it on, so normal emulation pays nothing.
    pub fn enable_write_log(&mut self) {
        self.sound_write_log = Some(Vec::new());
    }

    /// Drain the PSG writes captured since the last call, leaving capture
    /// enabled. Returns empty if capture was never enabled.
    pub fn take_write_log(&mut self) -> Vec<PsgWrite> {
        match self.sound_write_log.as_mut() {
            Some(log) => std::mem::take(log),
            None => Vec::new(),
        }
    }

    /// Record one PSG byte at its current in-frame sample position. No-op
    /// unless capture is enabled.
    fn log_psg_write(&mut self, data: u8) {
        let sample_pos = self.frame_samples.len() as u32;
        if let Some(log) = self.sound_write_log.as_mut() {
            log.push(PsgWrite { sample_pos, data });
        }
    }

    /// Enable or disable audio rendering. When disabled (host audio off),
    /// `catch_up()` skips synthesis, so disabling audio buys back the FPS synthesis
    /// costs. The tone and noise generators are not clocked meanwhile, so their
    /// counters, polarities and LFSR (in savestates and the state hash) hold still.
    pub fn set_rendering_enabled(&mut self, enabled: bool) {
        self.rendering_enabled = enabled;
    }

    /// Render every whole output sample owed for the clocks accumulated since the
    /// last render point, appending them to `frame_samples` using the chip's
    /// current register state. Called before each register write (so the write's
    /// effect starts at the right sample) and once more at frame end by
    /// `generate_samples()`.
    fn catch_up(&mut self) {
        if !self.rendering_enabled {
            // Audio off: don't synthesize, just keep the accumulator bounded.
            if self.clock_accumulator >= self.cycles_per_sample {
                self.clock_accumulator %= self.cycles_per_sample;
            }
            return;
        }

        while self.clock_accumulator >= self.cycles_per_sample {
            self.clock_accumulator -= self.cycles_per_sample;

            // The SN76489 divides its input clock by 16 for the internal
            // tone/noise generators (~223.7 kHz on Genesis). Advance the divider
            // and clock channels for every 16 input clocks consumed this sample.
            self.internal_clock_accumulator += self.cycles_per_sample;
            while self.internal_clock_accumulator >= 16.0 {
                self.clock_channels();
                self.internal_clock_accumulator -= 16.0;
            }

            self.frame_samples.push(self.get_mixed_output());

            if self.waveform_decimation.is_multiple_of(16) {
                self.store_waveform_sample();
            }
            self.waveform_decimation = self.waveform_decimation.wrapping_add(1);
        }

        // Bound the buffer if the host stops draining it.
        if self.frame_samples.len() > MAX_PSG_FRAME_SAMPLES {
            let excess = self.frame_samples.len() - MAX_PSG_FRAME_SAMPLES;
            self.frame_samples.drain(..excess);
        }
    }

    /// Reset PSG to initial state (all channels muted)
    pub fn reset(&mut self) {
        // Reset all tone channels (sets attenuation to 15 = mute)
        for channel in &mut self.tone_channels {
            channel.set_frequency(0);
            channel.set_attenuation(15);
        }

        // Reset noise channel
        self.noise_channel.set_control(0);
        self.noise_channel.set_attenuation(15);

        // Reset internal state
        self.clock_accumulator = 0.0;
        self.internal_clock_accumulator = 0.0;
        self.latched_channel = 0;
        self.latched_type = LatchType::Tone;
        self.frame_samples.clear();
        self.waveform_decimation = 0;

        // Clear waveform buffers
        for buffer in &mut self.waveform_buffer {
            buffer.clear();
        }
        self.mixed_waveform.clear();
    }

    /// Write data to PSG
    ///
    /// SN76489 protocol:
    /// - Bit 7 = 1 (0x80): Latch/Data byte
    ///   - Bits 6-5: Channel select (00-10 = tone 0-2, 11 = noise)
    ///   - Bit 4: Type (0 = tone/noise data, 1 = volume)
    ///   - Bits 3-0: Data (low 4 bits of frequency or volume)
    /// - Bit 7 = 0: Data byte (high 6 bits of frequency)
    ///
    /// # Arguments
    /// * `data` - 8-bit data to write
    pub fn write(&mut self, data: u8) {
        // Render the audio produced up to this write using the current register
        // state, so the write's effect begins at the correct sample position
        // (Genesis Plus GX and PicoDrive run the PSG to the write's time the
        // same way).
        self.catch_up();
        self.log_psg_write(data);

        if (data & 0x80) != 0 {
            // Latch/Data byte
            let channel = (data >> 5) & 0x03;
            let is_volume = (data & 0x10) != 0;
            let data_bits = data & 0x0F;

            self.latched_channel = channel;
            self.latched_type = if is_volume {
                LatchType::Volume
            } else {
                LatchType::Tone
            };

            // Write data based on type
            match (channel, self.latched_type) {
                // Tone channels - frequency low 4 bits
                (0..=2, LatchType::Tone) => {
                    let ch = &mut self.tone_channels[channel as usize];
                    let freq = (ch.get_frequency() & 0x3F0) | (data_bits as u16);
                    ch.set_frequency(freq);
                }
                // Tone channels - volume
                (0..=2, LatchType::Volume) => {
                    self.tone_channels[channel as usize].set_attenuation(data_bits);
                }
                // Noise channel - control
                (3, LatchType::Tone) => {
                    self.noise_channel.set_control(data_bits & 0x07);
                }
                // Noise channel - volume
                (3, LatchType::Volume) => {
                    self.noise_channel.set_attenuation(data_bits);
                }
                _ => {} // Unreachable due to masking
            }
        } else {
            // Data byte (high 6 bits of frequency for tone channels)
            if self.latched_channel < 3 && self.latched_type == LatchType::Tone {
                let ch = &mut self.tone_channels[self.latched_channel as usize];
                let freq = (ch.get_frequency() & 0x00F) | ((data as u16 & 0x3F) << 4);
                ch.set_frequency(freq);
            }
        }

        // Update noise channel tone3 reference
        if self.latched_channel == 2 && self.latched_type == LatchType::Tone {
            self.noise_channel
                .set_tone3_frequency(self.tone_channels[2].get_frequency());
        }
    }

    /// Clock all channels one cycle
    fn clock_channels(&mut self) {
        // Clock all 3 tone channels
        for ch in &mut self.tone_channels {
            ch.clock();
        }

        // Clock noise channel
        self.noise_channel.clock();
    }

    /// Get mixed output from all channels
    fn get_mixed_output(&self) -> f32 {
        let mut sum = 0.0;

        // Sum tone channels
        for ch in &self.tone_channels {
            sum += ch.get_output();
        }

        // Add noise channel
        sum += self.noise_channel.get_output();

        // Average (divide by 4 channels) and clamp
        (sum / 4.0).clamp(-1.0, 1.0)
    }

    /// Store current output in waveform buffers (uses ring buffers for O(1) ops)
    fn store_waveform_sample(&mut self) {
        for (i, ch) in self.tone_channels.iter().enumerate() {
            let sample = ch.get_output();
            if self.waveform_buffer[i].len() >= self.max_waveform_samples {
                self.waveform_buffer[i].pop_front();
            }
            self.waveform_buffer[i].push_back(sample);
        }

        let noise_sample = self.noise_channel.get_output();
        if self.waveform_buffer[3].len() >= self.max_waveform_samples {
            self.waveform_buffer[3].pop_front();
        }
        self.waveform_buffer[3].push_back(noise_sample);

        let mixed = self.get_mixed_output();
        if self.mixed_waveform.len() >= self.max_waveform_samples {
            self.mixed_waveform.pop_front();
        }
        self.mixed_waveform.push_back(mixed);
    }
}

impl AudioChip for Psg {
    fn chip_name(&self) -> &'static str {
        "SN76489"
    }

    fn get_info(&self) -> AudioChipInfo {
        AudioChipInfo {
            name: "SN76489",
            manufacturer: "Texas Instruments",
            channels: 4,
            sample_rate: self.sample_rate,
            stereo: false, // PSG is mono
            channel_types: vec![
                ChannelType::Tone,
                ChannelType::Tone,
                ChannelType::Tone,
                ChannelType::Noise,
            ],
        }
    }

    fn write_register(&mut self, _address: u8, data: u8) {
        // PSG doesn't use address, only data
        self.write(data);
    }

    fn read_register(&mut self, _address: u8) -> u8 {
        // PSG has no readable registers
        0xFF
    }

    fn generate_samples(&mut self, output: &mut [f32], sample_rate: u32) -> usize {
        // Update cycles_per_sample if sample rate changed
        if sample_rate != self.sample_rate {
            self.sample_rate = sample_rate;
            self.cycles_per_sample = self.clock_freq as f32 / sample_rate as f32;
        }

        // Flush the samples owed for the clocks accumulated since the last write
        // (rendered at the current register state), then hand the caller what it
        // asked for. The drift-free sample *count* over time is owned by the
        // system's cycles->samples converter, not by this buffer, so any samples
        // beyond what the caller drains are a sub-sample rounding tail that must
        // NOT persist: carrying them would replay stale audio after a later
        // register write changed the chip state. Drop the remainder so each call
        // reflects state up to *now*; sub-frame accuracy comes from `catch_up()`
        // at each `write`.
        self.catch_up();

        let n = output.len().min(self.frame_samples.len());
        output[..n].copy_from_slice(&self.frame_samples[..n]);
        self.frame_samples.clear();

        n
    }

    fn reset(&mut self) {
        for ch in &mut self.tone_channels {
            ch.reset();
        }
        self.noise_channel.reset();

        self.clock_accumulator = 0.0;
        self.internal_clock_accumulator = 0.0;
        self.latched_channel = 0;
        self.latched_type = LatchType::Tone;
        self.frame_samples.clear();
        self.waveform_decimation = 0;

        // Clear waveform buffers
        for buf in &mut self.waveform_buffer {
            buf.clear();
        }
        self.mixed_waveform.clear();
    }

    fn get_output_level(&self) -> f32 {
        self.get_mixed_output()
    }
}

impl AudioDebugger for Psg {
    fn get_channel_count(&self) -> u32 {
        4
    }

    fn get_channel_state(&self, channel: u32) -> Option<ChannelState> {
        match channel {
            0..=2 => Some(self.tone_channels[channel as usize].get_state(channel)),
            3 => Some(self.noise_channel.get_state(3)),
            _ => None,
        }
    }

    fn mute_channel(&mut self, channel: u32, muted: bool) {
        match channel {
            0..=2 => self.tone_channels[channel as usize].muted = muted,
            3 => self.noise_channel.muted = muted,
            _ => {}
        }
    }

    fn get_waveform(&self, channel: u32, samples: u32) -> Vec<f32> {
        if channel > 3 {
            return Vec::new();
        }

        let buf = &self.waveform_buffer[channel as usize];
        let count = (samples as usize).min(buf.len());

        if count == 0 {
            return Vec::new();
        }

        buf.iter().skip(buf.len() - count).copied().collect()
    }

    fn get_output_waveform(&self, samples: u32) -> Vec<f32> {
        let count = (samples as usize).min(self.mixed_waveform.len());

        if count == 0 {
            return Vec::new();
        }

        self.mixed_waveform
            .iter()
            .skip(self.mixed_waveform.len() - count)
            .copied()
            .collect()
    }
}

impl Clockable for Psg {
    fn clock(&mut self, cycles: u32) -> u32 {
        // Accumulate cycles for sample generation
        // DON'T clock channels here - catch_up() does, before each register
        // write and from generate_samples()
        self.clock_accumulator += cycles as f32;
        cycles
    }
}

// Savestate support
use crate::system::savestate::{NoiseChannelState, PsgState, ToneChannelState};

impl Psg {
    pub fn to_savestate(&self) -> PsgState {
        PsgState {
            tone_channels: [
                ToneChannelState {
                    frequency: self.tone_channels[0].frequency(),
                    attenuation: self.tone_channels[0].attenuation(),
                    counter: self.tone_channels[0].counter(),
                    polarity: self.tone_channels[0].polarity(),
                },
                ToneChannelState {
                    frequency: self.tone_channels[1].frequency(),
                    attenuation: self.tone_channels[1].attenuation(),
                    counter: self.tone_channels[1].counter(),
                    polarity: self.tone_channels[1].polarity(),
                },
                ToneChannelState {
                    frequency: self.tone_channels[2].frequency(),
                    attenuation: self.tone_channels[2].attenuation(),
                    counter: self.tone_channels[2].counter(),
                    polarity: self.tone_channels[2].polarity(),
                },
            ],
            noise_channel: NoiseChannelState {
                shift_rate: self.noise_channel.shift_rate_raw(),
                mode: self.noise_channel.mode_raw(),
                lfsr: self.noise_channel.lfsr(),
                attenuation: self.noise_channel.attenuation(),
                counter: self.noise_channel.counter(),
                polarity: self.noise_channel.polarity(),
                tone3_frequency: self.noise_channel.tone3_frequency(),
            },
            latched_channel: self.latched_channel,
            latched_type: match self.latched_type {
                LatchType::Tone => 0,
                LatchType::Volume => 1,
            },
            clock_accumulator: self.clock_accumulator,
            internal_clock_accumulator: self.internal_clock_accumulator,
        }
    }

    pub fn from_savestate(&mut self, state: &PsgState) {
        for (i, ts) in state.tone_channels.iter().enumerate() {
            self.tone_channels[i].restore_state(
                ts.frequency,
                ts.attenuation,
                ts.counter,
                ts.polarity,
            );
        }
        self.noise_channel.restore_state(
            state.noise_channel.shift_rate,
            state.noise_channel.mode,
            state.noise_channel.lfsr,
            state.noise_channel.attenuation,
            state.noise_channel.counter,
            state.noise_channel.polarity,
            state.noise_channel.tone3_frequency,
        );
        self.latched_channel = state.latched_channel;
        self.latched_type = if state.latched_type == 0 {
            LatchType::Tone
        } else {
            LatchType::Volume
        };
        self.clock_accumulator = state.clock_accumulator;
        self.internal_clock_accumulator = state.internal_clock_accumulator;
        self.frame_samples.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CLOCK: u32 = 3_579_545;
    const TEST_SAMPLE_RATE: u32 = 44100;

    #[test]
    fn test_psg_creation() {
        let psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);
        assert_eq!(psg.chip_name(), "SN76489");
        assert_eq!(psg.get_channel_count(), 4);
    }

    #[test]
    fn test_psg_info() {
        let psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);
        let info = psg.get_info();

        assert_eq!(info.name, "SN76489");
        assert_eq!(info.manufacturer, "Texas Instruments");
        assert_eq!(info.channels, 4);
        assert_eq!(info.sample_rate, TEST_SAMPLE_RATE);
        assert!(!info.stereo);
        assert_eq!(info.channel_types.len(), 4);
    }

    #[test]
    fn test_write_latch_tone_frequency() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

        // Latch tone channel 0, frequency low 4 bits = 0x0D
        psg.write(0x8D); // 1000 1101 = latch ch0, tone, data=0xD

        let state = psg.get_channel_state(0).unwrap();
        assert_eq!(state.frequency_raw & 0x0F, 0x0D);

        // Write high 6 bits = 0x01
        psg.write(0x01); // 0000 0001

        let state = psg.get_channel_state(0).unwrap();
        // Frequency should now be 0x01D (low 4 bits + high 6 bits)
        assert_eq!(state.frequency_raw, 0x01D);
    }

    #[test]
    fn test_write_latch_volume() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

        // Latch tone channel 1, volume = 0x05
        psg.write(0xB5); // 1011 0101 = latch ch1, volume, data=0x5

        let state = psg.get_channel_state(1).unwrap();
        assert_eq!(state.attenuation, 0x05);
    }

    #[test]
    fn test_write_noise_control() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

        // Latch noise channel, control = 0x07 (Tone3, White)
        psg.write(0xE7); // 1110 0111 = latch ch3, tone(control), data=0x7

        let state = psg.get_channel_state(3).unwrap();
        let params = state.noise_params.unwrap();

        assert!(matches!(
            params.shift_rate,
            retro_core::audio::NoiseShiftRate::Tone3
        ));
        assert!(matches!(params.mode, retro_core::audio::NoiseMode::White));
    }

    #[test]
    fn test_write_noise_volume() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

        // Latch noise channel, volume = 0x03
        psg.write(0xF3); // 1111 0011 = latch ch3, volume, data=0x3

        let state = psg.get_channel_state(3).unwrap();
        assert_eq!(state.attenuation, 0x03);
    }

    #[test]
    fn test_clockable() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

        // Set up a tone
        psg.write(0x8D); // Ch0 frequency
        psg.write(0x01);
        psg.write(0x90); // Ch0 volume max

        // Clock some cycles
        psg.clock(1000);

        // Should have generated some output
        let output = psg.get_output_level();
        assert!(output.abs() > 0.0);
    }

    #[test]
    fn test_generate_samples() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

        // Set up a tone
        psg.write(0x8D);
        psg.write(0x01);
        psg.write(0x90);

        // CRITICAL: Must clock the PSG to accumulate cycles for sample generation
        // cycles_per_sample = TEST_CLOCK / TEST_SAMPLE_RATE = 3579545 / 44100 = ~81.1 cycles/sample
        // For 1024 samples, we need ~83k cycles
        let cycles_needed = ((TEST_CLOCK as f32 / TEST_SAMPLE_RATE as f32) * 1024.0) as u32;
        psg.clock(cycles_needed + 1000); // Add some buffer

        let mut samples = vec![0.0; 1024];
        let generated = psg.generate_samples(&mut samples, TEST_SAMPLE_RATE);

        assert_eq!(generated, 1024);

        // Verify samples are not all zero
        let non_zero = samples.iter().any(|&s| s != 0.0);
        assert!(non_zero);
    }

    #[test]
    fn test_mute_channel() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

        // Set up channels
        psg.write(0x8D);
        psg.write(0x01);
        psg.write(0x90);

        // Generate sample
        psg.clock(100);
        let output_unmuted = psg.get_output_level();

        // Mute channel 0
        psg.mute_channel(0, true);
        let output_muted = psg.get_output_level();

        // Muted output should be less (or zero if only ch0 was playing)
        assert!(output_muted.abs() < output_unmuted.abs() || output_muted == 0.0);
    }

    #[test]
    fn test_reset() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

        // Set up some state
        psg.write(0x8D);
        psg.write(0x01);
        psg.write(0x90);

        psg.reset();

        // All channels should be muted after reset
        for ch in 0..4 {
            let state = psg.get_channel_state(ch).unwrap();
            assert_eq!(state.attenuation, 15);
        }
    }

    #[test]
    fn test_waveform_retrieval() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

        // Generate some samples
        psg.write(0x8D);
        psg.write(0x01);
        psg.write(0x90);

        // CRITICAL: Must clock before generating samples
        let cycles_needed = ((TEST_CLOCK as f32 / TEST_SAMPLE_RATE as f32) * 1024.0) as u32;
        psg.clock(cycles_needed + 1000);

        let mut samples = vec![0.0; 1024];
        psg.generate_samples(&mut samples, TEST_SAMPLE_RATE);

        // Get waveform
        let waveform = psg.get_waveform(0, 100);
        assert!(!waveform.is_empty());

        let mixed = psg.get_output_waveform(100);
        assert!(!mixed.is_empty());
    }

    #[test]
    fn test_channel_state() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

        psg.write(0x8D); // Ch0 freq
        psg.write(0x01);
        psg.write(0x95); // Ch0 vol = 5

        let state = psg.get_channel_state(0).unwrap();

        assert_eq!(state.channel_id, 0);
        assert_eq!(state.channel_type, ChannelType::Tone);
        assert_eq!(state.attenuation, 5);
        assert!(state.enabled); // Attenuation < 15
        assert!(!state.muted);
    }

    #[test]
    fn test_all_channels_simultaneously() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

        // Set up all 3 tone channels + noise
        psg.write(0x8D); // Ch0 freq
        psg.write(0x01);
        psg.write(0x90); // Ch0 vol max

        psg.write(0xAE); // Ch1 freq
        psg.write(0x02);
        psg.write(0xB0); // Ch1 vol max

        psg.write(0xCF); // Ch2 freq
        psg.write(0x03);
        psg.write(0xD0); // Ch2 vol max

        psg.write(0xE4); // Noise white
        psg.write(0xF0); // Noise vol max

        // CRITICAL: Must clock before generating samples
        let cycles_needed = ((TEST_CLOCK as f32 / TEST_SAMPLE_RATE as f32) * 1024.0) as u32;
        psg.clock(cycles_needed + 1000);

        let mut samples = vec![0.0; 1024];
        psg.generate_samples(&mut samples, TEST_SAMPLE_RATE);

        // All channels should contribute to output
        let max_amplitude = samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(max_amplitude > 0.1);
    }

    // A PSG register write must only affect samples produced after the write's
    // cycle position, just like the FM chip. Here channel 0 is silent (max
    // attenuation) for the first half of the frame and then turned to full volume
    // mid-frame; `catch_up` at `write()` renders the silent run before applying the
    // new volume, so the early samples stay quiet and only the later ones sound.
    #[test]
    fn test_catchup_volume_change_only_affects_samples_after_its_position() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);
        psg.set_rendering_enabled(true);

        // Channel 0 tone with an audible period, but attenuated to silence.
        psg.write(0x8D); // latch ch0 tone, freq low nibble
        psg.write(0x01); // freq high bits
        psg.write(0x9F); // ch0 volume = 0x0F (silent)

        let half = ((TEST_CLOCK as f32 / TEST_SAMPLE_RATE as f32) * 100.0) as u32;
        psg.clock(half);

        // Mid-frame: full volume. `write()` runs `catch_up()` first.
        psg.write(0x90); // ch0 volume = 0x00 (loudest)
        psg.clock(half);

        let mut buf = vec![0.0f32; 512];
        let n = psg.generate_samples(&mut buf, TEST_SAMPLE_RATE);
        assert!(n >= 180, "expected ~200 samples, got {n}");

        let early_max = buf[..80].iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        let late_max = buf[120..n].iter().fold(0.0f32, |m, &s| m.max(s.abs()));

        assert!(early_max < 0.01, "pre-write region not silent: {early_max}");
        assert!(late_max > 0.02, "post-write region not audible: {late_max}");
    }

    // Audio gated off must not synthesize, but the accumulator stays bounded.
    #[test]
    fn test_rendering_disabled_produces_no_samples() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);
        psg.set_rendering_enabled(false);
        psg.write(0x8D);
        psg.write(0x01);
        psg.write(0x90);

        psg.clock(((TEST_CLOCK as f32 / TEST_SAMPLE_RATE as f32) * 500.0) as u32);

        let mut buf = vec![0.0f32; 256];
        let n = psg.generate_samples(&mut buf, TEST_SAMPLE_RATE);
        assert_eq!(n, 0, "no samples should be produced while rendering is off");
    }

    // The optional sound-write log (the cross-core probe) records every PSG
    // byte tagged with the in-frame sample offset where it takes effect.
    #[test]
    fn test_write_log_captures_bytes_and_sample_position() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);
        psg.set_rendering_enabled(true);
        psg.enable_write_log();

        // Two bytes before any clocking -> sample_pos 0.
        psg.write(0x8D); // latch ch0 tone + freq low nibble
        psg.write(0x01); // freq high bits

        let hundred = ((TEST_CLOCK as f32 / TEST_SAMPLE_RATE as f32) * 100.0) as u32;
        psg.clock(hundred);
        psg.write(0x90); // ch0 volume -> ~sample 100

        let log = psg.take_write_log();
        assert_eq!(log.len(), 3, "every PSG byte is logged (mono, one port)");
        assert_eq!(log[0].data, 0x8D);
        assert_eq!(log[0].sample_pos, 0);
        assert_eq!(log[1].data, 0x01);
        assert_eq!(log[1].sample_pos, 0);
        assert_eq!(log[2].data, 0x90);
        assert!(
            (90..=110).contains(&log[2].sample_pos),
            "third write should land near sample 100, got {}",
            log[2].sample_pos
        );

        assert!(psg.take_write_log().is_empty());
    }

    #[test]
    fn test_write_log_disabled_by_default() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);
        psg.set_rendering_enabled(true);
        psg.write(0x90);
        assert!(psg.take_write_log().is_empty());
    }
}
