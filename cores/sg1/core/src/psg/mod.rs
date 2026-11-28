//! SN76489 Programmable Sound Generator (PSG)
//!
//! The SN76489 is a sound chip used in the Sega Genesis/Mega Drive (and many other systems).
//! It provides:
//! - 3 tone channels (square waves)
//! - 1 noise channel (white or periodic)
//! - 4-bit attenuation per channel (16 volume levels)
//!
//! The PSG is clocked at Master Clock / 15 (~3.58 MHz on Genesis).
//!
//! It runs at its own rate: the channels tick once every 16 input clocks,
//! and each change of a channel's output is kept with the input clock it
//! happened at. That record, the chip's output as levels at times, is what
//! the system's host stage makes into the host's band-limited samples.

pub mod noise;
pub mod tone;

use noise::NoiseChannel;
use tone::ToneChannel;

use retro_core::audio::{
    AudioChip, AudioChipInfo, AudioDebugger, ChannelState, ChannelType, IntegerRateConverter,
    SoundEvent,
};
use retro_core::cpu::Clockable;

/// The input clocks between two ticks of the channels: the chip divides its
/// input clock by 16 (about 223.7 kHz on the Mega Drive).
const CLOCKS_PER_TICK: u32 = 16;

/// The most changes the record keeps while nothing takes it (a frame that
/// does not end, a host that takes no sound): past it the oldest are
/// folded into the level the record starts from.
const MAX_OUTPUT_CHANGES: usize = 16384;

/// The ticks between two values of a channel's waveform for the debugger:
/// about 0.36 ms, so its 1,024 values span about 0.37 s.
const TICKS_PER_SCOPE_VALUE: u32 = 81;

/// The four channels' outputs at a moment, muted or not: tone channels 1
/// to 3, then the noise channel.
pub type PsgLevels = [f32; PSG_STEM_VALUES];

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

    /// The host's sample rate: what the chip's information reports, the
    /// rate [`AudioChip::generate_samples`] last read at, and the unit of
    /// the write log's sample offsets.
    sample_rate: u32,

    /// The input clocks received since the channels last ticked: the
    /// divider's progress, which the next ticks take, 16 a tick. Machine
    /// state.
    unticked: u32,

    /// Where the chip stands: the input clocks since master clock 0, as
    /// the system sets them at a reset and a load and each `clock` adds,
    /// the clock a change of its output is timed by. Not saved: derived.
    position: u64,

    /// The chip's output since the record was last taken: first the four
    /// outputs it started from and the input clock it started at, then
    /// each change, at the input clock it happened at. Taken by the
    /// system's host stage at each frame's end. A frame stopped part of
    /// the way through keeps it in the savestate's extension.
    output: Vec<(u64, PsgLevels)>,

    /// The sample-and-hold [`AudioChip::generate_samples`] reads the
    /// record with: the host's samples it handed out, on the chip's input
    /// clocks. Built at its first call. Not saved.
    sampler: Option<IntegerRateConverter>,

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

    /// The ticks left before the next value of the waveforms.
    scope_countdown: u32,

    /// When false (host audio disabled), `catch_up()` does not tick the
    /// channels and keeps no record, only the divider's place, so no CPU is
    /// spent on audio nobody hears. Not serialized: it is a host setting
    /// re-applied via `set_rendering_enabled`.
    rendering_enabled: bool,

    /// Optional capture of every byte written to the PSG, tagged with the
    /// in-frame output-sample offset at which it takes effect. `None` =
    /// disabled (zero overhead). Transient diagnostic for `sg1-cli
    /// sound-trace`; never serialized.
    sound_write_log: Option<Vec<PsgWrite>>,

    /// The input clocks the chip has received since it was built: the
    /// clock the sound log and the write log stamp a byte with. Never
    /// saved, and the same whether the chip renders or not.
    input_clocks: u64,

    /// The sound log while the host logs: each byte written, stamped with
    /// `input_clocks`. Not saved.
    sound_log: Option<Vec<(u64, SoundEvent)>>,
}

/// The values a sample stages for the stems: tone channels 1 to 3, then
/// the noise channel.
pub const PSG_STEM_VALUES: usize = 4;

/// A single SN76489 byte write captured by the optional sound-write log.
///
/// `sample_pos` is the host's output-sample offset at which the write takes
/// effect, counted from the start of the chip's record, which the system
/// takes at each frame's end: the input clocks since then at the host's
/// sample rate, rounded down. `data` is the raw command byte;
/// the SN76489 latch/data encoding (channel, tone/volume, data nibble) lives
/// in the byte itself, so the log stays a faithful record of the wire bytes.
/// `clock` is the chip's input clocks received when the byte arrived,
/// counted from when it was built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PsgWrite {
    pub sample_pos: u32,
    pub data: u8,
    pub clock: u64,
}

impl Psg {
    /// Create a new PSG with given clock frequency and sample rate
    ///
    /// # Arguments
    /// * `clock_freq` - PSG clock frequency (Genesis: Master/15 = ~3.58 MHz)
    /// * `sample_rate` - Audio sample rate (typically 44100 Hz)
    pub fn new(clock_freq: u32, sample_rate: u32) -> Self {
        let mut psg = Self {
            tone_channels: [
                ToneChannel::new(clock_freq),
                ToneChannel::new(clock_freq),
                ToneChannel::new(clock_freq),
            ],
            noise_channel: NoiseChannel::new(clock_freq),
            clock_freq,
            sample_rate,
            unticked: 0,
            position: 0,
            output: Vec::with_capacity(4096),
            sampler: None,
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
            scope_countdown: 0,
            rendering_enabled: true,
            sound_write_log: None,
            input_clocks: 0,
            sound_log: None,
        };
        psg.restart_output();
        psg
    }

    /// The record starts afresh where the chip stands, from its outputs as
    /// they are.
    fn restart_output(&mut self) {
        self.output.clear();
        let outputs = self.outputs();
        self.output.push((self.position, outputs));
    }

    /// The input clock, in Hz.
    pub fn clock_freq(&self) -> u32 {
        self.clock_freq
    }

    /// Run on another input clock, as a console of another region gives it
    /// (the Mega Drive's PSG runs on the Z80's clock): the tone channels'
    /// frequencies in Hz the debugger shows follow it, and so does the
    /// sample-and-hold's rate. What the chip holds is kept: the registers,
    /// the counters, the divider's place and the record. The noise channel
    /// reads no clock. The same clock changes nothing.
    pub fn set_clock_freq(&mut self, clock_freq: u32) {
        if clock_freq == self.clock_freq {
            return;
        }
        self.clock_freq = clock_freq;
        self.sampler = None;
        for channel in &mut self.tone_channels {
            channel.set_clock_freq(clock_freq);
        }
    }

    /// Stand at input clock `position`, counted from master clock 0, with
    /// nothing changed: a reset and a load derive it from the master clock.
    /// A record that holds no change starts again here, from the outputs as
    /// they are; one a savestate brought back keeps its times.
    pub fn set_position(&mut self, position: u64) {
        self.position = position;
        if self.output.len() <= 1 {
            self.restart_output();
        }
    }

    /// Where the chip stands: its input clocks since master clock 0.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// The four channels' outputs, muted or not.
    pub fn outputs(&self) -> PsgLevels {
        [
            self.tone_channels[0].raw_output(),
            self.tone_channels[1].raw_output(),
            self.tone_channels[2].raw_output(),
            self.noise_channel.raw_output(),
        ]
    }

    /// Which of the four channels the debugger muted: the mix leaves them
    /// out, their stems keep them.
    pub fn channel_mutes(&self) -> [bool; PSG_STEM_VALUES] {
        [
            self.tone_channels[0].muted,
            self.tone_channels[1].muted,
            self.tone_channels[2].muted,
            self.noise_channel.muted,
        ]
    }

    /// The chip's output up to now into `into`, emptied first: the outputs
    /// it started from and their input clock, then each change at its own
    /// (every channel ticked through the clocks received). The record goes
    /// on from here, from the outputs as they are.
    pub fn take_output(&mut self, into: &mut Vec<(u64, PsgLevels)>) {
        self.catch_up();
        into.clear();
        std::mem::swap(&mut self.output, into);
        self.restart_output();
    }

    /// The record of a frame in progress, for the savestate's extension:
    /// empty when it holds nothing but the outputs it started from, which
    /// a load derives.
    pub fn frame_output(&self) -> Vec<(u64, PsgLevels)> {
        if self.output.len() > 1 {
            self.output.clone()
        } else {
            Vec::new()
        }
    }

    /// Make the record a savestate's (after [`Self::from_savestate`]); an
    /// empty one starts where the system next sets the chip's position.
    pub fn restore_frame_output(&mut self, output: &[(u64, PsgLevels)]) {
        self.output.clear();
        self.output.extend_from_slice(output);
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

    /// Record one PSG byte at its current in-frame sample position, and in
    /// the sound log. No-op unless a log is on.
    fn log_psg_write(&mut self, data: u8) {
        let clock = self.input_clocks;
        let since_start = self
            .position
            .saturating_sub(self.output.first().map_or(0, |e| e.0));
        let sample_pos = (u128::from(since_start) * u128::from(self.sample_rate)
            / u128::from(self.clock_freq.max(1))) as u32;
        if let Some(log) = self.sound_write_log.as_mut() {
            log.push(PsgWrite {
                sample_pos,
                data,
                clock,
            });
        }
        if let Some(log) = self.sound_log.as_mut() {
            log.push((
                clock,
                SoundEvent::Write {
                    register: 0,
                    value: data,
                },
            ));
        }
    }

    /// Log each byte written on the chip's input clock (`true`), dropping
    /// what was logged before; or stop. A host setting, not saved.
    pub fn set_sound_log(&mut self, on: bool) {
        self.sound_log = on.then(Vec::new);
    }

    /// What the sound log holds since the last take: each byte with the
    /// input clocks received when it came ([`Self::input_clocks`]).
    pub fn take_sound_log(&mut self) -> Vec<(u64, SoundEvent)> {
        self.sound_log
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// The input clocks received since the chip was built.
    pub fn input_clocks(&self) -> u64 {
        self.input_clocks
    }

    /// The bytes that write the chip's state back: each tone channel's
    /// period and attenuation, the noise channel's control and attenuation,
    /// then the register the chip has latched, latched again with the value
    /// it holds, so a data byte after the bytes lands where it would have.
    /// A noise channel's control written restarts its shift register.
    pub fn register_image(&self) -> Vec<u8> {
        let mut image = Vec::with_capacity(12);
        for (channel, tone) in self.tone_channels.iter().enumerate() {
            let channel = (channel as u8) << 5;
            let period = tone.get_frequency();
            image.push(0x80 | channel | (period & 0x0F) as u8);
            image.push((period >> 4) as u8 & 0x3F);
            image.push(0x90 | channel | tone.get_attenuation());
        }
        image.push(0xE0 | self.noise_channel.get_control());
        image.push(0xF0 | self.noise_channel.get_attenuation());
        let latched = self.latched_channel;
        let (kind, value) = match (latched, self.latched_type) {
            (3, LatchType::Tone) => (0x00, self.noise_channel.get_control()),
            (3, LatchType::Volume) => (0x10, self.noise_channel.get_attenuation()),
            (channel, LatchType::Tone) => {
                let period = self.tone_channels[usize::from(channel)].get_frequency();
                (0x00, (period & 0x0F) as u8)
            }
            (channel, LatchType::Volume) => (
                0x10,
                self.tone_channels[usize::from(channel)].get_attenuation(),
            ),
        };
        image.push(0x80 | latched << 5 | kind | value);
        image
    }

    /// Enable or disable audio rendering. When disabled (host audio off),
    /// `catch_up()` neither ticks the channels nor keeps a record, so
    /// disabling audio buys back what they cost. The tone and noise
    /// generators hold still meanwhile, their counters, polarities and LFSR
    /// (in savestates and the state hash) as they were; the divider keeps
    /// its place among the 16 clocks of a tick. Enabled again, the record
    /// starts afresh from the outputs as they are.
    pub fn set_rendering_enabled(&mut self, enabled: bool) {
        if enabled && !self.rendering_enabled {
            self.restart_output();
        }
        self.rendering_enabled = enabled;
    }

    /// Tick the channels once for every 16 input clocks received since the
    /// last tick, each tick at the input clock its 16th clock came, keeping
    /// each change of their outputs in the record. Called before each
    /// register write, so the write lands after every tick of its time,
    /// and when the record is taken.
    fn catch_up(&mut self) {
        if !self.rendering_enabled {
            self.unticked %= CLOCKS_PER_TICK;
            return;
        }
        let first = self.position.saturating_sub(u64::from(self.unticked));
        let ticks = self.unticked / CLOCKS_PER_TICK;
        for tick in 1..=ticks {
            self.tick(first + u64::from(tick * CLOCKS_PER_TICK));
        }
        self.unticked %= CLOCKS_PER_TICK;
    }

    /// One tick of the channels at input clock `time`: a change of their
    /// outputs goes into the record, and every 81st tick into the
    /// waveforms.
    fn tick(&mut self, time: u64) {
        self.clock_channels();
        self.note_outputs(time);
        if self.scope_countdown == 0 {
            self.scope_countdown = TICKS_PER_SCOPE_VALUE;
            self.store_waveform_sample();
        }
        self.scope_countdown -= 1;
    }

    /// The outputs as they stand at input clock `time`, into the record if
    /// they changed.
    fn note_outputs(&mut self, time: u64) {
        let outputs = self.outputs();
        if self.output.last().is_some_and(|last| last.1 == outputs) {
            return;
        }
        self.output.push((time, outputs));
        if self.output.len() > MAX_OUTPUT_CHANGES {
            // The oldest changes fold into the level the record starts from.
            let cut = self.output.len() - MAX_OUTPUT_CHANGES / 2;
            self.output.drain(..cut);
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

        // Reset internal state; the record starts again from the reset's
        // outputs.
        self.unticked = 0;
        self.latched_channel = 0;
        self.latched_type = LatchType::Tone;
        self.restart_output();
        self.sampler = None;
        self.scope_countdown = 0;

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
        // Every tick of the clocks before this write first, so the write's
        // effect starts at its own time (Genesis Plus GX and PicoDrive run
        // the PSG to the write's time the same way).
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

        // A volume or a noise control moves the output at the write's time.
        if self.rendering_enabled {
            self.note_outputs(self.position);
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

    /// The chip's output read by a sample-and-hold at `sample_rate`: each
    /// output sample the mixed level (the channels' outputs, the muted left
    /// out, over 4) in force at its time, from the record, which this
    /// takes; as many as the input clocks received since the last call
    /// came to, the first `output.len()` of them written. For a tool or a
    /// test reading the chip alone: the machine's sound goes through its
    /// band-limited host stage instead, which takes the record itself.
    fn generate_samples(&mut self, output: &mut [f32], sample_rate: u32) -> usize {
        if sample_rate != self.sample_rate {
            self.sample_rate = sample_rate;
            self.sampler = None;
        }
        let mut record = Vec::new();
        self.take_output(&mut record);
        if !self.rendering_enabled {
            self.sampler = None;
            return 0;
        }
        let start = record.first().map_or(self.position, |e| e.0);
        let sampler = self.sampler.get_or_insert_with(|| {
            let mut sampler =
                IntegerRateConverter::new(u64::from(sample_rate), u64::from(self.clock_freq));
            sampler.reset(start);
            sampler
        });
        let cumulative = sampler.cycles_to_samples(self.position);
        let samples = (cumulative - sampler.samples_at_reference()) as usize;
        let mutes = [
            self.tone_channels[0].muted,
            self.tone_channels[1].muted,
            self.tone_channels[2].muted,
            self.noise_channel.muted,
        ];
        let mixed = |levels: &PsgLevels| {
            let sum: f32 = levels
                .iter()
                .zip(mutes)
                .map(|(&level, muted)| if muted { 0.0 } else { level })
                .sum();
            sum / 4.0
        };
        let mut level = record.first().map_or(0.0, |e| mixed(&e.1));
        let mut next = 1;
        let written = samples.min(output.len());
        for (j, slot) in output[..written].iter_mut().enumerate() {
            // A change at a time before this sample's is in force for it.
            while let Some(&(time, ref levels)) = record.get(next) {
                if sampler.position(time, 1).sample >= j as i64 {
                    break;
                }
                level = mixed(levels);
                next += 1;
            }
            *slot = level;
        }
        sampler.advance(self.position, cumulative);
        written
    }

    fn reset(&mut self) {
        for ch in &mut self.tone_channels {
            ch.reset();
        }
        self.noise_channel.reset();

        self.unticked = 0;
        self.latched_channel = 0;
        self.latched_type = LatchType::Tone;
        self.restart_output();
        self.sampler = None;
        self.scope_countdown = 0;

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
    fn chip_info(&self) -> AudioChipInfo {
        AudioChip::get_info(self)
    }

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
        // The clocks are counted; the channels tick through them in
        // catch_up(), before each register write and when the record is
        // taken.
        self.input_clocks += u64::from(cycles);
        self.position += u64::from(cycles);
        self.unticked += cycles;
        cycles
    }
}

// Savestate support
use crate::savestate::{NoiseChannelState, PsgExtension, PsgState, ToneChannelState};

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
            // As a build that makes its samples at the host's rate reads
            // them: no clocks towards a host sample, and the clocks
            // received since the last tick as its divider's progress.
            clock_accumulator: 0.0,
            internal_clock_accumulator: self.unticked as f32,
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
        // The clocks received and not yet ticked: those a build that makes
        // its samples at the host's rate counted towards its next sample,
        // which it had not given its divider yet, and the divider's own
        // (all of them in this build's states). A borrowed sample left the
        // first below 0.
        let unticked = state.clock_accumulator.max(0.0) + state.internal_clock_accumulator.max(0.0);
        self.unticked = unticked.floor() as u32;
        // The record goes on from the savestate's extension, or starts where
        // the system sets the chip's position.
        self.output.clear();
        self.sampler = None;
    }

    /// The part of the savestate's extension the layout gave the PSG: the
    /// samples made at the host's rate so far in a frame, which a build
    /// that makes them reads. Written empty: the chip makes none (its
    /// record goes into the extension's own part, [`Self::frame_output`]),
    /// so such a build loading a state saved part of the way through a
    /// frame plays that frame from the load.
    pub fn extension(&self) -> PsgExtension {
        PsgExtension {
            samples: Vec::new(),
        }
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
    fn the_register_image_written_into_a_fresh_chip_gives_it_back() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);
        for byte in [
            0x8E, 0x0F, 0x93, 0xA5, 0x21, 0xBA, 0xC1, 0x3F, 0xD0, 0xE5, 0xF2, 0xA7,
        ] {
            psg.write(byte);
        }
        let image = psg.register_image();
        let mut fresh = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);
        for &byte in &image {
            fresh.write(byte);
        }
        assert_eq!(fresh.register_image(), image);
        assert_eq!(
            image.last(),
            Some(&0xA7),
            "the latch: channel 1's tone, as written last"
        );
        // A data byte after the image lands where it would have.
        psg.write(0x12);
        fresh.write(0x12);
        assert_eq!(fresh.register_image(), psg.register_image());
    }

    #[test]
    fn the_sound_log_stamps_each_byte_with_the_input_clocks() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);
        psg.set_sound_log(true);
        psg.clock(77);
        psg.write(0x9F);
        assert_eq!(
            psg.take_sound_log(),
            [(
                77,
                SoundEvent::Write {
                    register: 0,
                    value: 0x9F
                }
            )]
        );
        assert_eq!(psg.input_clocks(), 77);
    }

    #[test]
    fn test_write_log_disabled_by_default() {
        let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);
        psg.set_rendering_enabled(true);
        psg.write(0x90);
        assert!(psg.take_write_log().is_empty());
    }
}
