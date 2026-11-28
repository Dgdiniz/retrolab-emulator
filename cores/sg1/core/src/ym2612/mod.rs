// YM2612 FM Synthesis Chip (Yamaha)
// Used in Sega Genesis/Mega Drive for music and sound effects
//
// Features:
// - 6 FM channels (4 operators each)
// - 8 connection algorithms
// - ADSR envelopes per operator
// - LFO (vibrato/tremolo)
// - DAC mode (channel 6 can play PCM samples)
// - Stereo output (L/R panning)
//
// It runs at its own rate: one sample each 144 input clocks, its envelope
// stepping every third, its LFO one chip sample a sample, each operator's
// phase by the chip's own increment. Each sample is kept with the input
// clock it was made at, the chip's output as levels at times, which the
// system's host stage holds from sample to sample and band-limits.

pub mod algorithms;
pub mod channel;
mod dac;
mod debug;
pub mod envelope;
pub mod lfo;
pub mod operator;
mod registers;
mod state;
mod synthesis;
pub mod tables;
#[cfg(test)]
mod tests;
pub mod timers;

// Re-exports
pub use algorithms::Algorithm;
pub use channel::{Channel, StereoFlags};
pub use dac::FmDacModel;
pub use debug::{DacStatus, FmOperator, FmPatch, LfoStatus, TimerStatus};
pub use envelope::{EnvelopeGenerator, EnvelopeState};
pub use lfo::Lfo;
pub use operator::Operator;
pub use synthesis::YM_STEM_VALUES;
pub use tables::SINE_TABLE;
pub use timers::Timers;

use std::collections::VecDeque;

use retro_core::audio::{IntegerRateConverter, SoundEvent, SoundLogContents};

/// Cap on `timer_accumulator`, in input clocks: eight timer ticks' worth.
/// It bounds the timer work of one `clock()` call; a longer call runs eight
/// ticks and drops the rest.
const MAX_TIMER_ACCUMULATOR: f32 = 144.0 * 8.0;

/// The input clocks of one of the chip's samples.
const CLOCKS_PER_SAMPLE: u32 = 144;

/// The chip's samples between two steps of its envelopes.
const SAMPLES_PER_ENVELOPE_STEP: u32 = 3;

/// The most samples the record keeps while nothing takes it (a frame that
/// does not end, a host that takes no sound): past it the oldest are
/// folded into the level the record starts from.
const MAX_OUTPUT_SAMPLES: usize = 16384;

/// The chip's left and right output at a moment, each in [-1, 1].
pub type YmLevels = [f32; 2];

/// YM2612 FM Synthesis Chip
///
/// The YM2612 (aka OPN2) is a 6-channel FM synthesis chip used in the Sega Genesis.
/// Each channel has 4 operators, and various routing algorithms.
///
/// Features:
/// - 6 FM channels (4 operators each)
/// - 8 connection algorithms per channel
/// - Stereo output (L/R panning per channel)
/// - LFO (Low Frequency Oscillator) - global
/// - 2 Timers
/// - DAC mode (channel 6 can play PCM samples)
/// - Special mode: Channel 3 can have independent operator frequencies
pub struct Ym2612 {
    /// 6 FM channels
    channels: [Channel; 6],

    /// Chip clock frequency (typically 7,670,454 Hz for Genesis)
    clock_freq: u32,

    /// The host's sample rate: what the chip's information reports, the
    /// rate [`retro_core::audio::AudioChip::generate_samples`] last read
    /// at, and the unit of the write log's sample offsets.
    sample_rate: u32,

    /// Global LFO (Low Frequency Oscillator)
    lfo: Lfo,

    /// Timers A and B
    timers: Timers,

    /// DAC enable flag
    dac_enabled: bool,

    /// DAC value (9-bit signed)
    dac_value: i16,

    /// The input clocks received since the chip's last sample, which the
    /// next samples take, 144 a sample. Machine state.
    unsampled: u32,

    /// Where the chip stands: its input clocks since master clock 0, as the
    /// system sets them at a reset and a load and each `clock` adds, the
    /// clock its samples are timed by. Not saved: derived.
    position: u64,

    /// The chip's output since the record was last taken: first the level
    /// it held as the record started (its last sample) and the input clock
    /// the record started at, then each sample at the input clock it was
    /// made at. Taken by the system's host stage at each frame's end; a
    /// frame stopped part of the way through keeps it in the savestate's
    /// extension.
    output: Vec<(u64, YmLevels)>,

    /// The sample-and-hold [`retro_core::audio::AudioChip::generate_samples`]
    /// reads the record with: the host's samples it handed out, on the
    /// chip's input clocks. Built at its first call. Not saved.
    sampler: Option<IntegerRateConverter>,

    /// Timer accumulator: ticks timers at the internal FM rate (clock / 144 ~ 53,267 Hz).
    /// Separate from `unsampled`: the timers run whether the chip makes
    /// samples or not.
    timer_accumulator: f32,

    // ===== CH3 Special Mode =====
    /// CH3 Special Mode enable (register 0x27 bits 7-6)
    /// 0x00: Normal, 0x40: CH3 special, 0x80: CSM, 0xC0: CSM + CH3
    ch3_mode: u8,

    /// CH3 operator frequencies (F-number and Block) for special mode
    /// [OP0, OP1, OP2, OP3] - only used when CH3 special mode is active
    ch3_fnum: [u16; 4],
    ch3_block: [u8; 4],

    /// CSM key-on shift register for delayed key-off.
    /// Bit 0: CSM key-on happened this sample.
    /// Bit 1: CSM key-on happened previous sample (trigger key-off).
    /// Shifted left each timer tick; when bit 1 is set, key-off fires.
    csm_key_state: u8,

    // ===== Z80 Communication (Address Latches) =====
    /// Address latch for Part 0 (Z80 port 0)
    address_latch_part0: u8,

    /// Address latch for Part 1 (Z80 port 2)
    address_latch_part1: u8,

    // ===== Debugging Support =====
    /// The channels the debugger muted: each still runs, and the mix
    /// leaves its output out. A host setting, not saved; a reset keeps it.
    mute_flags: [bool; 6],

    /// The DAC the channels reach the output through: which chip the
    /// console has. A host setting, not saved; a reset keeps it.
    dac_model: FmDacModel,

    /// Output level for visualization
    output_level: f32,

    /// Waveform buffer for visualization (last N samples) -- ring buffer
    waveform_buffer: VecDeque<f32>,

    /// Max waveform buffer size
    max_waveform_samples: usize,

    /// Last output samples (left, right) for each channel, muted or not
    channel_outputs: [(f32, f32); 6],

    /// Retained only for savestate-format compatibility. DAC continuity
    /// lives in `dac_value`, which is rendered live through `catch_up()`.
    dac_start_value: i16,

    // ===== Envelope Generator Timing =====
    /// The chip's samples since its envelopes last stepped, 0 to 2: they
    /// step every third sample (clock/144/3 ~ 17,756 Hz). Machine state.
    eg_samples: u32,

    /// Global EG counter (12-bit, 1-4095: it skips zero when it wraps).
    /// Advanced, reset and saved, but the EG never uses it: each envelope keeps its own
    /// counter.
    eg_counter: u32,

    /// Busy flag counter in input clock cycles. After a register data write,
    /// the chip sets the busy flag (bit 7 of the status register) for
    /// 32 * prescale = 192 input clock cycles (~25 microseconds at 7.67 MHz).
    /// Games poll this flag in tight loops before writing the next register.
    busy_counter: u32,
    /// Absolute CPU cycle at which the busy period would end under a
    /// timestamp model (`current_cpu_cycle + 192` at write time). Kept in
    /// sync for savestate compatibility; live status uses `busy_counter`.
    busy_end_cycle: u64,
    /// 68K cycle snapshot mirrored by the system before each instruction.
    /// Stamped into `busy_end_cycle` on data writes; not used by
    /// `get_status` (see busy_counter).
    pub current_cpu_cycle: u64,

    /// Optional capture of every FM *data-port* write, tagged with the
    /// in-frame output-sample offset at which it takes effect. `None` =
    /// disabled (zero overhead, no allocation). Transient diagnostic for the
    /// `sg1-cli sound-trace` cross-core timing diff, never serialized.
    sound_write_log: Option<Vec<FmWrite>>,

    /// The stems' tap while the host takes stems (`synthesis::YmStems`):
    /// each sample's channels staged beside the record. Not saved.
    stems: Option<Box<synthesis::YmStems>>,

    /// The input clocks the chip has received since it was built: the
    /// clock the sound log and the write log stamp a write with. Never
    /// saved, and the same whether the chip renders or not.
    input_clocks: u64,

    /// The sound log while the host logs: what `sound_log_contents` asks
    /// for (each data write, and each reset of the /IC pin followed by the
    /// registers it leaves; each channel's keys), stamped with
    /// `input_clocks`. Not saved.
    sound_log: Option<Vec<(u64, SoundEvent)>>,
    sound_log_contents: SoundLogContents,
}

/// A single YM2612 data-port write captured by the optional sound-write log.
///
/// `sample_pos` is the host's output-sample offset at which the write takes
/// audible effect, counted from the start of the chip's record, which the
/// system takes at each frame's end: the input clocks since then at the
/// host's sample rate, rounded down. It is the sub-frame placement the
/// per-write catch-up model exists to make accurate, so it is the right
/// quantity to diff against a reference core.
/// `part` is the port pair (0 = part I / channels 1-3, 1 = part II /
/// channels 4-6); `register`/`value` are the latched address and data byte.
/// `clock` is the chip's input clocks received when the write arrived,
/// counted from when it was built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FmWrite {
    pub sample_pos: u32,
    pub part: u8,
    pub register: u8,
    pub value: u8,
    pub clock: u64,
}

impl Ym2612 {
    /// Create a new YM2612 chip
    ///
    /// # Arguments
    /// * `clock_freq` - Chip clock frequency (typically 7,670,454 Hz)
    /// * `sample_rate` - The host's sample rate (typically 44,100 Hz)
    pub fn new(clock_freq: u32, sample_rate: u32) -> Self {
        let mut ym = Self {
            channels: [
                Channel::new(clock_freq),
                Channel::new(clock_freq),
                Channel::new(clock_freq),
                Channel::new(clock_freq),
                Channel::new(clock_freq),
                Channel::new(clock_freq),
            ],
            clock_freq,
            sample_rate,
            lfo: Lfo::new(),
            timers: Timers::new(),
            dac_enabled: false,
            dac_value: 0,
            unsampled: 0,
            position: 0,
            output: Vec::with_capacity(2048),
            sampler: None,
            timer_accumulator: 0.0,
            ch3_mode: 0,
            ch3_fnum: [0; 4],
            ch3_block: [0; 4],
            csm_key_state: 0,
            address_latch_part0: 0,
            address_latch_part1: 0,
            mute_flags: [false; 6],
            dac_model: FmDacModel::default(),
            output_level: 0.0,
            waveform_buffer: VecDeque::with_capacity(1024),
            max_waveform_samples: 1024,
            channel_outputs: [(0.0, 0.0); 6],
            dac_start_value: 0,
            eg_samples: 0,
            eg_counter: 0,
            busy_counter: 0,
            busy_end_cycle: 0,
            current_cpu_cycle: 0,
            sound_write_log: None,
            stems: None,
            input_clocks: 0,
            sound_log: None,
            sound_log_contents: SoundLogContents::default(),
        };
        ym.restart_output();
        ym
    }

    /// The record starts afresh where the chip stands, from the level it
    /// holds: its last sample, or silence when it has made none.
    fn restart_output(&mut self) {
        let held = self.output.last().map_or([0.0; 2], |entry| entry.1);
        self.output.clear();
        self.output.push((self.position, held));
        if let Some(stems) = self.stems.as_deref_mut() {
            stems.restart();
        }
    }

    /// Stand at input clock `position`, counted from master clock 0, with
    /// nothing changed: a reset and a load derive it from the master clock.
    /// A record that holds no sample starts again here; one a savestate
    /// brought back keeps its times.
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

    /// The chip's output up to input clock `until` into `into`, emptied
    /// first: the level the record started from and its input clock, then
    /// each sample made by then at its own (every sample the clocks
    /// received came to made first). A sample made after `until` (the chip
    /// follows the Z80, which can run past the frame's end) stays, for the
    /// record that goes on from `until` with the level the last one taken
    /// left. The stems staged for the samples taken go into `stems`.
    pub fn take_output(
        &mut self,
        until: u64,
        into: &mut Vec<(u64, YmLevels)>,
        stems: &mut Vec<f32>,
    ) {
        self.catch_up();
        into.clear();
        let taken = self.output.partition_point(|entry| entry.0 <= until).max(1);
        into.extend_from_slice(&self.output[..taken]);
        let held = into.last().map_or([0.0; 2], |entry| entry.1);
        self.output.drain(..taken);
        self.output.insert(0, (until.max(into[0].0), held));
        stems.clear();
        if let Some(staged) = self.stems.as_deref_mut() {
            staged.take(taken, stems);
        }
    }

    /// The record of a frame in progress, for the savestate's extension:
    /// empty when it holds nothing but the level it started from, which a
    /// load derives.
    pub fn frame_output(&self) -> Vec<(u64, YmLevels)> {
        if self.output.len() > 1 {
            self.output.clone()
        } else {
            Vec::new()
        }
    }

    /// Make the record a savestate's (after [`Self::from_savestate`]). With
    /// none, the record starts afresh, where the system next sets the
    /// chip's position, from the level the chip last held (silence for a
    /// chip that made none): a load cannot know the level of the chip's
    /// next sample before it is made.
    pub fn restore_frame_output(&mut self, output: &[(u64, YmLevels)]) {
        if output.is_empty() {
            self.restart_output();
        } else {
            self.output.clear();
            self.output.extend_from_slice(output);
        }
        self.align_stems();
    }

    /// Begin capturing FM data-port writes (see [`FmWrite`]), discarding any
    /// prior capture. Off by default; used only by the CLI sound-trace
    /// diagnostic, so the normal emulation path pays nothing.
    pub fn enable_write_log(&mut self) {
        self.sound_write_log = Some(Vec::new());
    }

    /// Drain the FM writes captured since the last call, leaving capture
    /// enabled. Returns empty if capture was never enabled.
    pub fn take_write_log(&mut self) -> Vec<FmWrite> {
        match self.sound_write_log.as_mut() {
            Some(log) => std::mem::take(log),
            None => Vec::new(),
        }
    }

    /// Record one resolved FM data write at its current in-frame sample
    /// position, and in the sound log when it logs writes. No-op unless a
    /// log is on.
    fn log_fm_write(&mut self, part: u8, register: u8, value: u8) {
        let since_start = self
            .position
            .saturating_sub(self.output.first().map_or(0, |e| e.0));
        let sample_pos = (u128::from(since_start) * u128::from(self.sample_rate)
            / u128::from(self.clock_freq.max(1))) as u32;
        let clock = self.input_clocks;
        if let Some(log) = self.sound_write_log.as_mut() {
            log.push(FmWrite {
                sample_pos,
                part,
                register,
                value,
                clock,
            });
        }
        if !self.sound_log_contents.writes {
            return;
        }
        if let Some(log) = self.sound_log.as_mut() {
            let register = u16::from(part) << 8 | u16::from(register);
            log.push((clock, SoundEvent::Write { register, value }));
        }
    }

    /// Log what `contents` asks for on the chip's input clock, dropping
    /// what was logged before: with `writes` each data write, and each
    /// reset of the /IC pin with the registers it leaves; with `keys` each
    /// channel's key-on and key-off. Contents of nothing stop the log. A
    /// host setting, not saved.
    pub fn set_sound_log(&mut self, contents: SoundLogContents) {
        self.sound_log = contents.any().then(Vec::new);
        self.sound_log_contents = contents;
    }

    /// Whether the sound log takes the channels' keys down.
    fn logs_keys(&self) -> bool {
        self.sound_log.is_some() && self.sound_log_contents.keys
    }

    /// Put `event` in the sound log at input clock `clock`.
    fn log_event(&mut self, clock: u64, event: SoundEvent) {
        if let Some(log) = self.sound_log.as_mut() {
            log.push((clock, event));
        }
    }

    /// What the sound log holds since the last take: each entry with the
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

    /// The input clock, in Hz.
    pub fn clock_freq(&self) -> u32 {
        self.clock_freq
    }

    /// Run on another input clock, as a console of another region gives it
    /// (the Mega Drive's YM2612 runs on the 68000's clock). The chip makes
    /// a sample each 144 of its clocks whatever they are, so what follows
    /// the clock is only what is shown or read in Hz: the operators'
    /// frequencies the debugger shows and the sample-and-hold's rate. What
    /// the chip holds is kept: the registers, the phases, their increments
    /// (the chip's own, which no clock changes) and envelopes, the timers
    /// (which count input clocks), the clocks received towards the next
    /// sample and the record. The same clock changes nothing.
    pub fn set_clock_freq(&mut self, clock_freq: u32) {
        if clock_freq == self.clock_freq {
            return;
        }
        self.clock_freq = clock_freq;
        self.sampler = None;
        for ch in &mut self.channels {
            ch.set_clock_freq(clock_freq);
        }
    }

    /// Reach the output through the DAC of `model`'s chip, from the next
    /// sample on.
    pub fn set_dac_model(&mut self, model: FmDacModel) {
        self.dac_model = model;
    }

    /// The DAC the channels reach the output through.
    pub fn dac_model(&self) -> FmDacModel {
        self.dac_model
    }

    /// Get a reference to a channel
    pub fn channel(&self, index: usize) -> Option<&Channel> {
        self.channels.get(index)
    }

    /// Get a mutable reference to a channel
    pub fn channel_mut(&mut self, index: usize) -> Option<&mut Channel> {
        self.channels.get_mut(index)
    }

    /// Reset the chip to initial state; the debugger's mutes stay.
    pub fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.reset();
        }

        self.lfo.reset();
        self.timers.reset();
        self.dac_enabled = false;
        self.dac_value = 0;
        self.unsampled = 0;
        self.timer_accumulator = 0.0;
        self.address_latch_part0 = 0;
        self.address_latch_part1 = 0;
        self.output_level = 0.0;
        self.waveform_buffer.clear();
        self.channel_outputs = [(0.0, 0.0); 6];
        self.eg_samples = 0;
        self.eg_counter = 0;
        self.busy_counter = 0;
        self.busy_end_cycle = 0;
        // The record starts again from silence, where the chip stands.
        self.output.clear();
        self.restart_output();
        self.align_stems();
        self.sampler = None;
        self.dac_start_value = 0;
        self.ch3_mode = 0;
        self.ch3_fnum = [0; 4];
        self.ch3_block = [0; 4];
        self.csm_key_state = 0;
    }

    /// Reset driven by the Z80 reset line (`$A11200`). On hardware the YM2612 `/IC`
    /// pin follows ZRES, so the FM chip is held cleared while the Z80 is in reset
    /// (as Genesis Plus GX and PicoDrive both do on that line). Unlike the
    /// full power-on [`reset`](Self::reset), this:
    ///   - first makes the samples produced up to this moment (so the reset
    ///     doesn't retroactively silence sound that already played this frame),
    ///   - clears FM synthesis + timer state, and
    ///   - preserves the record (`output`), the clocks towards the next sample
    ///     (`unsampled`), debugger mutes (`mute_flags`), and the sample-rate
    ///     config.
    pub fn ic_reset(&mut self) {
        self.catch_up();
        // The notes sounding end with the reset.
        let keyed_before: [u8; 6] = match self.logs_keys() {
            true => std::array::from_fn(|channel| self.keyed_operators(channel)),
            false => [0; 6],
        };

        for channel in &mut self.channels {
            channel.reset();
        }
        self.lfo.reset();
        self.timers.reset();
        self.dac_enabled = false;
        self.dac_value = 0;
        self.dac_start_value = 0;
        self.timer_accumulator = 0.0;
        self.ch3_mode = 0;
        self.ch3_fnum = [0; 4];
        self.ch3_block = [0; 4];
        self.csm_key_state = 0;
        self.address_latch_part0 = 0;
        self.address_latch_part1 = 0;
        self.eg_samples = 0;
        self.eg_counter = 0;
        self.busy_counter = 0;
        self.busy_end_cycle = 0;

        for (channel, keyed) in keyed_before.into_iter().enumerate() {
            if keyed != 0 {
                self.log_event(
                    self.input_clocks,
                    SoundEvent::KeyOff {
                        channel: channel as u8,
                    },
                );
            }
        }
        if self.sound_log.is_some() && self.sound_log_contents.writes {
            let clock = self.input_clocks;
            let image = self.register_image();
            if let Some(log) = self.sound_log.as_mut() {
                log.push((clock, SoundEvent::Reset));
                log.extend(
                    image
                        .into_iter()
                        .map(|(register, value)| (clock, SoundEvent::Write { register, value })),
                );
            }
        }
    }

    // ===== Z80 Communication Interface =====

    /// Read from YM2612 port (Z80 perspective)
    ///
    /// Z80 I/O mapping (writes):
    /// - Port 0 (0x4000): Address latch Part 0
    /// - Port 1 (0x4001): Data Part 0
    /// - Port 2 (0x4002): Address latch Part 1
    /// - Port 3 (0x4003): Data Part 1
    ///
    /// Reads from any of the four ports return the status register (busy +
    /// timer flags). Data ports are write-only for register values, but the
    /// status bus is mirrored: drivers such as E-Swat poll busy with
    /// `bit 7,(hl)` while HL points at `$4001`. Returning open-bus `0xFF`
    /// there leaves bit 7 stuck and soft-locks the PCM path.
    pub fn read_port(&self, port: u8) -> u8 {
        if port <= 3 {
            self.get_status()
        } else {
            0xFF
        }
    }

    /// Write to YM2612 port (Z80 perspective)
    ///
    /// Z80 I/O mapping:
    /// - Port 0 (0x4000): Address latch Part 0
    /// - Port 1 (0x4001): Data Part 0
    /// - Port 2 (0x4002): Address latch Part 1
    /// - Port 3 (0x4003): Data Part 1
    pub fn write_port(&mut self, port: u8, value: u8) {
        // Make the samples produced up to this write with the current register
        // state, so the write's effect begins at the correct sample. (Genesis
        // Plus GX and PicoDrive run the chip to the write's time the same way
        // before they apply a register change.)
        self.catch_up();
        match port {
            0 => {
                // Address latch Part 0
                self.address_latch_part0 = value;
            }
            1 => {
                // Data Part 0, logged before it acts, so a key it changes
                // follows the write in the log.
                self.log_fm_write(0, self.address_latch_part0, value);
                self.write_register(0, self.address_latch_part0, value);
                // Set busy flag for 32 * prescale(6) = 192 input clock cycles
                self.busy_counter = 192;
                self.busy_end_cycle = self.current_cpu_cycle + 192;
            }
            2 => {
                // Address latch Part 1
                self.address_latch_part1 = value;
            }
            3 => {
                // Data Part 1, logged before it acts
                self.log_fm_write(1, self.address_latch_part1, value);
                self.write_register(1, self.address_latch_part1, value);
                // Set busy flag for 32 * prescale(6) = 192 input clock cycles
                self.busy_counter = 192;
                self.busy_end_cycle = self.current_cpu_cycle + 192;
            }
            _ => {}
        }
    }

    /// Get YM2612 status register
    ///
    /// Bit layout:
    /// - Bit 0: Timer A overflow flag
    /// - Bit 1: Timer B overflow flag
    /// - Bit 2-6: Reserved (0)
    /// - Bit 7: Busy flag (0 = ready, 1 = busy)
    pub fn get_status(&self) -> u8 {
        // Busy is driven by `busy_counter`, which `clock()` decrements during
        // both 68K and Z80 bursts. The absolute `busy_end_cycle` /
        // `current_cpu_cycle` pair only advances at 68K instruction boundaries,
        // so a status poll inside a Z80 burst would otherwise see busy stuck
        // for the whole burst and starve PCM drivers that write -> poll -> write.
        let busy = if self.busy_counter > 0 { 0x80 } else { 0x00 };
        self.timers.get_status() | busy
    }

    /// Get LFO reference
    pub fn lfo(&self) -> &Lfo {
        &self.lfo
    }

    /// Get mutable LFO reference
    pub fn lfo_mut(&mut self) -> &mut Lfo {
        &mut self.lfo
    }

    /// Get Timers reference
    pub fn timers(&self) -> &Timers {
        &self.timers
    }

    /// Get mutable Timers reference
    pub fn timers_mut(&mut self) -> &mut Timers {
        &mut self.timers
    }

    /// Get DAC enabled status
    pub fn is_dac_enabled(&self) -> bool {
        self.dac_enabled
    }

    /// Get DAC value (signed, -128 to +127)
    pub fn get_dac_value(&self) -> i16 {
        self.dac_value
    }

    /// Get DAC sample (raw unsigned byte, 0-255)
    pub fn get_dac_sample(&self) -> u8 {
        // Convert from signed (-128..+127) to unsigned (0..255)
        ((self.dac_value as i32 + 128) & 0xFF) as u8
    }

    /// Set DAC value directly (for testing)
    pub fn set_dac_value(&mut self, value: i16) {
        self.dac_value = value;
    }
}
