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

pub mod algorithms;
pub mod channel;
pub mod envelope;
pub mod lfo;
pub mod operator;
pub mod tables;
pub mod timers;
// pub mod dac;
// pub mod registers;

// Re-exports
pub use algorithms::Algorithm;
pub use channel::{Channel, StereoFlags};
pub use envelope::{EnvelopeGenerator, EnvelopeState};
pub use lfo::Lfo;
pub use operator::Operator;
pub use tables::SINE_TABLE;
pub use timers::Timers;

use retro_core::audio::{
    AudioChip, AudioChipInfo, AudioDebugger, ChannelState, ChannelType, EnvelopePhase,
    EnvelopeState as AudioEnvelopeState,
};
use retro_core::cpu::Clockable;
use std::collections::VecDeque;

/// Cap on `timer_accumulator`, in input clocks: eight timer ticks' worth.
/// It bounds the timer work of one `clock()` call; a longer call runs eight
/// ticks and drops the rest.
const MAX_TIMER_ACCUMULATOR: f32 = 144.0 * 8.0;

/// Upper bound on buffered, undrained stereo samples (L+R interleaved). If the
/// host stops consuming audio (disabled, debugger paused, tab backgrounded),
/// `catch_up()` trims the oldest samples instead of growing without bound.
const MAX_FRAME_SAMPLES: usize = 8192;

/// How many stereo pairs `generate_samples` may render ahead of this chip's own
/// clock accumulator to satisfy the count the system's converter asked for.
/// Two quantisations of the same elapsed time disagree by about a pair; beyond
/// that the caller is asking for audio the machine has not reached.
const MAX_BORROWED_PAIRS: usize = 2;

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

    /// Sample rate for audio output
    sample_rate: u32,

    /// Global LFO (Low Frequency Oscillator)
    lfo: Lfo,

    /// Timers A and B
    timers: Timers,

    /// DAC enable flag
    dac_enabled: bool,

    /// DAC value (9-bit signed)
    dac_value: i16,

    /// Clocks per output sample = clock_freq / sample_rate (~174 for typical values).
    clocks_per_sample: f32,

    /// Accumulated input clock cycles available for sample generation (pull model).
    /// Cycles are added by clock() and consumed by generate_samples().
    clock_accumulator: f32,

    /// Timer accumulator: ticks timers at the internal FM rate (clock / 144 ~ 53,267 Hz).
    /// Separate from clock_accumulator because timers must run at the hardware
    /// internal rate, not the output sample rate (44,100 Hz).
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
    /// Mute flags for each channel
    mute_flags: [bool; 6],

    /// Output level for visualization
    output_level: f32,

    /// Waveform buffer for visualization (last N samples) -- ring buffer
    waveform_buffer: VecDeque<f32>,

    /// Max waveform buffer size
    max_waveform_samples: usize,

    /// Last output samples (left, right) for each channel
    channel_outputs: [(f32, f32); 6],

    // ===== Incremental Rendering (timestamp catch-up) =====
    /// Stereo samples (L, R interleaved) already rendered this frame by
    /// `catch_up()`. The chip renders the audio produced *before* each register
    /// write using the state in effect at that moment, so sub-frame register
    /// changes (key-on/off, frequency, total level, DAC) land at their correct
    /// sample position instead of collapsing to end-of-frame state. Drained by
    /// `generate_samples()`; never serialized (transient per frame).
    frame_samples: Vec<f32>,
    /// Retained only for savestate-format compatibility. DAC continuity
    /// lives in `dac_value`, which is rendered live through `catch_up()`.
    dac_start_value: i16,

    // ===== Envelope Generator Timing =====
    /// EG timer, counting output samples. In real hardware, the EG updates every 3
    /// internal samples (clock/144/3 ~ 17,756 Hz).
    /// Since we generate samples at the output rate (not internal rate),
    /// we use a fractional accumulator to match the hardware EG rate.
    eg_accumulator: f32,

    /// How many output samples between EG ticks.
    /// = sample_rate / (clock_freq / 432)
    eg_samples_per_tick: f32,

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

    /// When false (host audio disabled), `catch_up()` skips FM synthesis and just
    /// keeps the clock accumulator bounded, so disabling audio saves the time FM
    /// synthesis takes. Timers keep running via `clock()`, so Timer A/B IRQ timing
    /// is unaffected. Not serialized: re-applied via `set_rendering_enabled`.
    rendering_enabled: bool,

    /// Optional capture of every FM *data-port* write, tagged with the
    /// in-frame output-sample offset at which it takes effect. `None` =
    /// disabled (zero overhead, no allocation). Transient diagnostic for the
    /// `sg1-cli sound-trace` cross-core timing diff, never serialized.
    sound_write_log: Option<Vec<FmWrite>>,
}

/// A single YM2612 data-port write captured by the optional sound-write log.
///
/// `sample_pos` is the output-sample offset *within the current frame* at
/// which the write takes audible effect, i.e. how many stereo samples
/// `catch_up()` had already rendered when the write arrived. It is exactly
/// the sub-frame placement the per-write catch-up model exists to make
/// accurate, so it is the right quantity to diff against a reference core.
/// `part` is the port pair (0 = part I / channels 1-3, 1 = part II /
/// channels 4-6); `register`/`value` are the latched address and data byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FmWrite {
    pub sample_pos: u32,
    pub part: u8,
    pub register: u8,
    pub value: u8,
}

impl Ym2612 {
    /// Create a new YM2612 chip
    ///
    /// # Arguments
    /// * `clock_freq` - Chip clock frequency (typically 7,670,454 Hz)
    /// * `sample_rate` - Audio output sample rate (typically 44,100 Hz)
    pub fn new(clock_freq: u32, sample_rate: u32) -> Self {
        Self {
            channels: [
                Channel::with_sample_rate(clock_freq, sample_rate),
                Channel::with_sample_rate(clock_freq, sample_rate),
                Channel::with_sample_rate(clock_freq, sample_rate),
                Channel::with_sample_rate(clock_freq, sample_rate),
                Channel::with_sample_rate(clock_freq, sample_rate),
                Channel::with_sample_rate(clock_freq, sample_rate),
            ],
            clock_freq,
            sample_rate,
            lfo: Lfo::new(),
            timers: Timers::new(),
            dac_enabled: false,
            dac_value: 0,
            clocks_per_sample: clock_freq as f32 / sample_rate as f32,
            clock_accumulator: 0.0,
            timer_accumulator: 0.0,
            ch3_mode: 0,
            ch3_fnum: [0; 4],
            ch3_block: [0; 4],
            csm_key_state: 0,
            address_latch_part0: 0,
            address_latch_part1: 0,
            mute_flags: [false; 6],
            output_level: 0.0,
            waveform_buffer: VecDeque::with_capacity(1024),
            max_waveform_samples: 1024,
            channel_outputs: [(0.0, 0.0); 6],
            frame_samples: Vec::with_capacity(2048),
            dac_start_value: 0,
            eg_accumulator: 0.0,
            eg_samples_per_tick: sample_rate as f32 / (clock_freq as f32 / 432.0),
            eg_counter: 0,
            busy_counter: 0,
            busy_end_cycle: 0,
            current_cpu_cycle: 0,
            rendering_enabled: true,
            sound_write_log: None,
        }
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
    /// position. No-op unless capture is enabled.
    fn log_fm_write(&mut self, part: u8, register: u8, value: u8) {
        // Snapshot the position before the borrow so this stays a disjoint
        // field access (the log borrows `sound_write_log`, the position reads
        // `frame_samples`).
        let sample_pos = (self.frame_samples.len() / 2) as u32;
        if let Some(log) = self.sound_write_log.as_mut() {
            log.push(FmWrite {
                sample_pos,
                part,
                register,
                value,
            });
        }
    }

    /// Enable or disable audio rendering. When disabled (host audio off),
    /// `catch_up()` skips FM synthesis, so disabling audio saves the time
    /// synthesis takes. Timer clocking is unaffected.
    pub fn set_rendering_enabled(&mut self, enabled: bool) {
        self.rendering_enabled = enabled;
    }

    /// Write to a YM2612 register
    ///
    /// YM2612 has two register banks (part 0 and part 1), selected via address 0/2.
    /// Data is written via address 1/3.
    ///
    /// # Arguments
    /// * `part` - Register bank (0 or 1)
    /// * `register` - Register address (0x00-0xFF)
    /// * `value` - Value to write (8-bit)
    pub fn write_register(&mut self, part: u8, register: u8, value: u8) {
        match register {
            // Global registers (part 0 only)
            0x22 => {
                // LFO control
                self.lfo.set_control(value);
            }

            0x24 => {
                // Timer A period, upper eight of its ten bits
                self.timers.set_timer_a_high(value);
            }

            0x25 => {
                // Timer A period, lower two bits
                self.timers.set_timer_a_low(value);
            }

            0x26 => {
                // Timer B: TBL = (256 - value) << 4
                self.timers.set_timer_b_period(value);
            }

            0x27 => {
                // CH3 mode and timer control
                // Bits 7-6: CH3 mode (0x00=normal, 0x40=CH3 special, 0x80=CSM, 0xC0=CSM+CH3)
                // Bits 5-0: Timer control
                self.ch3_mode = value & 0xC0; // Extract bits 7-6
                                              // Update channel 2's special mode flag for per-operator LFO PM
                self.channels[2].ch3_special = (self.ch3_mode & 0x40) != 0;
                self.timers.set_control(value);
            }

            0x28 => {
                // Key On/Off (channel decoding checked against Genesis Plus GX)
                // Channel encoding: bits 0-1 = channel within bank (0-2, value 3 is INVALID)
                //                   bit 2 = bank select (0 = ch0-2, 1 = ch3-5)
                // Operator bits: bit 4=SLOT1(op0), bit 5=SLOT2(op2), bit 6=SLOT3(op1),
                // bit 7=SLOT4(op3)
                // Note: bits 5/6 are swapped due to YM2612's non-standard slot ordering
                let ch_low = (value & 0x03) as usize;
                if ch_low == 3 {
                    // Channel value 3 names no channel: the write is ignored
                    // Do nothing
                } else {
                    let channel_idx = if (value & 0x04) != 0 {
                        ch_low + 3
                    } else {
                        ch_low
                    };
                    if channel_idx < 6 {
                        // Key-on bits 4-7 name the chip's slots 1-4, and
                        // slots 2 and 3 are operators 3 and 2 in our order.
                        const BIT_TO_OP: [usize; 4] = [0, 2, 1, 3];
                        let op_bits = (value >> 4) & 0x0F;
                        for (i, &op_idx) in BIT_TO_OP.iter().enumerate() {
                            if (op_bits & (1 << i)) != 0 {
                                self.channels[channel_idx].operator_mut(op_idx).key_on();
                            } else {
                                self.channels[channel_idx].operator_mut(op_idx).key_off();
                            }
                        }
                    }
                }
            }

            0x2A => {
                // DAC data. `write_port` already rendered the audio up to this
                // write via `catch_up()`, so updating the DAC value live is
                // sample-accurate for the remainder of the frame.
                self.dac_value = (value as i16) - 128;
            }

            0x2B => {
                // DAC enable
                self.dac_enabled = (value & 0x80) != 0;
            }

            // Per-channel/operator registers
            0x30..=0xB6 => {
                let channel_idx = self.get_channel_index(part, register);

                if channel_idx < 6 {
                    self.write_channel_register(channel_idx, register, value);
                }
            }

            _ => {
                // Unimplemented or invalid register
            }
        }
    }

    /// Get channel index from register address and part
    fn get_channel_index(&self, part: u8, register: u8) -> usize {
        let channel_bits = register & 0x03;
        let base_channel = channel_bits as usize;

        // Part 1 addresses channels 3-5
        if part == 1 {
            base_channel + 3
        } else {
            base_channel
        }
    }

    /// Get operator index from register address
    fn get_operator_index(&self, register: u8) -> usize {
        // Operators are arranged in a specific pattern in registers
        // 0x30-0x3F: OP1, 0x40-0x4F: OP2, 0x50-0x5F: OP3, 0x60-0x6F: OP4 (for DT/MUL)
        // But not all register groups follow this pattern
        match register & 0xF0 {
            0x30 => ((register >> 2) & 0x03) as usize, // Detune/Multiple
            0x40 => ((register >> 2) & 0x03) as usize, // Total Level
            0x50 => ((register >> 2) & 0x03) as usize, // Rate Scaling/Attack Rate
            0x60 => ((register >> 2) & 0x03) as usize, // Amplitude Mod/Decay Rate
            0x70 => ((register >> 2) & 0x03) as usize, // Sustain Rate
            0x80 => ((register >> 2) & 0x03) as usize, // Sustain Level/Release Rate
            0x90 => ((register >> 2) & 0x03) as usize, // SSG-EG
            _ => 0,
        }
    }

    /// Write to a per-channel/operator register
    fn write_channel_register(&mut self, channel_idx: usize, register: u8, value: u8) {
        // Match against full register for frequency/algorithm registers,
        // and against register & 0xF0 for per-operator registers
        if matches!(register, 0xA0..=0xA2 | 0xA4..=0xA6 | 0xB0..=0xB2 | 0xB4..=0xB6) {
            // Per-channel registers (frequency, algorithm, stereo)
            self.write_perchannel_register(channel_idx, register, value);
            return;
        }

        // Get operator index for per-operator registers
        let operator_idx = self.get_operator_index(register);

        match register & 0xF0 {
            0x30 => {
                // Detune (DT) and Multiple (MUL) - per operator
                let detune = (value >> 4) & 0x07;
                let multiple = value & 0x0F;

                self.channels[channel_idx].operator_mut(operator_idx).detune = detune;
                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .multiple = multiple;
                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .update_phase_increment();
            }

            0x40 => {
                // Total Level (TL) - per operator
                let total_level = value & 0x7F;
                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .total_level = total_level;
            }

            0x50 => {
                // Rate Scaling (KS) and Attack Rate (AR) - per operator
                // Bits 7-6: Rate Scaling (KS, 0-3)
                // Bits 4-0: Attack Rate (AR, 0-31)
                let rate_scaling = (value >> 6) & 0x03;
                let attack_rate = value & 0x1F;

                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .set_rate_scaling(rate_scaling);
                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .envelope_mut()
                    .attack_rate = attack_rate;
            }

            0x60 => {
                // AM Enable and First Decay Rate (D1R) - per operator
                // Bit 7: AM Enable (1 = this operator responds to LFO AM)
                // Bits 6-5: Unused
                // Bits 4-0: D1R (First Decay Rate)
                let am_enable = (value & 0x80) != 0;
                let decay_rate = value & 0x1F;

                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .set_am_enable(am_enable);
                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .envelope_mut()
                    .decay_rate = decay_rate;
            }

            0x70 => {
                // Secondary Decay Rate (D2R / SR) - per operator
                let sustain_rate = value & 0x1F;
                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .envelope_mut()
                    .sustain_rate = sustain_rate;
            }

            0x80 => {
                // Sustain Level (SL) and Release Rate (RR) - per operator
                let sustain_level = (value >> 4) & 0x0F;
                let release_rate = value & 0x0F;

                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .envelope_mut()
                    .sustain_level = sustain_level;
                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .envelope_mut()
                    .release_rate = release_rate;
            }

            0x90 => {
                // SSG-EG (the envelope modes of the SSG, the OPN's Software-controlled
                // Sound Generator) - per operator
                // Bit 3: Enable SSG-EG
                // Bit 2: Attack
                // Bit 1: Alternate
                // Bit 0: Hold
                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .envelope_mut()
                    .set_ssg_eg(value);
            }

            _ => {}
        }
    }

    /// Write to per-channel register (frequency, algorithm, stereo)
    fn write_perchannel_register(&mut self, channel_idx: usize, register: u8, value: u8) {
        match register {
            // CH3 Special Mode registers (part 0 only)
            // The slots are Plutiedev's and Nuked OPN2's; the indices are what this
            // code writes. In this core's order slot 3 is index 1 and slot 2 is
            // index 2, so $A8/$AC and $AA/$AE drive each other's slot:
            //   $A8/$AC -> Slot 3 (S3) -> operator index 2 (C1)
            //   $A9/$AD -> Slot 1 (S1) -> operator index 0 (M1)
            //   $AA/$AE -> Slot 2 (S2) -> operator index 1 (M2)
            //   $A2/$A6 -> Slot 4 (S4) -> operator index 3 (C2) [normal channel reg]
            0xA8 => {
                // CH3 S3 (OP2) F-Number (low 8 bits)
                let f_num_low = value as u16;
                self.ch3_fnum[2] = (self.ch3_fnum[2] & 0x700) | f_num_low;
            }
            0xA9 => {
                // CH3 S1 (OP0) F-Number (low 8 bits)
                let f_num_low = value as u16;
                self.ch3_fnum[0] = (self.ch3_fnum[0] & 0x700) | f_num_low;
            }
            0xAA => {
                // CH3 S2 (OP1) F-Number (low 8 bits)
                let f_num_low = value as u16;
                self.ch3_fnum[1] = (self.ch3_fnum[1] & 0x700) | f_num_low;
            }
            0xAC => {
                // CH3 S3 (OP2) F-Number (high 3 bits) and Block
                let block = (value >> 3) & 0x07;
                let f_num_high = (value & 0x07) as u16;
                self.ch3_fnum[2] = (self.ch3_fnum[2] & 0x0FF) | (f_num_high << 8);
                self.ch3_block[2] = block;
                // Apply to CH3 OP2 if special mode is active
                if (self.ch3_mode & 0x40) != 0 {
                    self.channels[2].operator_mut(2).freq_number = self.ch3_fnum[2];
                    self.channels[2].operator_mut(2).block = self.ch3_block[2];
                    self.channels[2].operator_mut(2).update_phase_increment();
                    self.channels[2].operator_mut(2).update_key_code();
                    // Sync per-operator freq for LFO PM
                    self.channels[2].op_fnum[2] = self.ch3_fnum[2];
                    self.channels[2].op_block[2] = self.ch3_block[2];
                }
            }
            0xAD => {
                // CH3 S1 (OP0) F-Number (high 3 bits) and Block
                let block = (value >> 3) & 0x07;
                let f_num_high = (value & 0x07) as u16;
                self.ch3_fnum[0] = (self.ch3_fnum[0] & 0x0FF) | (f_num_high << 8);
                self.ch3_block[0] = block;
                // Apply to CH3 OP0 if special mode is active
                if (self.ch3_mode & 0x40) != 0 {
                    self.channels[2].operator_mut(0).freq_number = self.ch3_fnum[0];
                    self.channels[2].operator_mut(0).block = self.ch3_block[0];
                    self.channels[2].operator_mut(0).update_phase_increment();
                    self.channels[2].operator_mut(0).update_key_code();
                    // Sync per-operator freq for LFO PM
                    self.channels[2].op_fnum[0] = self.ch3_fnum[0];
                    self.channels[2].op_block[0] = self.ch3_block[0];
                }
            }
            0xAE => {
                // CH3 S2 (OP1) F-Number (high 3 bits) and Block
                let block = (value >> 3) & 0x07;
                let f_num_high = (value & 0x07) as u16;
                self.ch3_fnum[1] = (self.ch3_fnum[1] & 0x0FF) | (f_num_high << 8);
                self.ch3_block[1] = block;
                // Apply to CH3 OP1 if special mode is active
                if (self.ch3_mode & 0x40) != 0 {
                    self.channels[2].operator_mut(1).freq_number = self.ch3_fnum[1];
                    self.channels[2].operator_mut(1).block = self.ch3_block[1];
                    self.channels[2].operator_mut(1).update_phase_increment();
                    self.channels[2].operator_mut(1).update_key_code();
                    // Sync per-operator freq for LFO PM
                    self.channels[2].op_fnum[1] = self.ch3_fnum[1];
                    self.channels[2].op_block[1] = self.ch3_block[1];
                }
            }

            0xA0..=0xA2 => {
                // F-Number (low 8 bits) - per channel
                let f_num_low = value as u16;

                // Update all operators in the channel (or CH3 OP3 if CH3 special mode)
                if channel_idx == 2 && (self.ch3_mode & 0x40) != 0 {
                    // CH3 special mode: only update OP3 (OP0-2 use independent frequencies)
                    let current_f_num = self.ch3_fnum[3];
                    let new_f_num = (current_f_num & 0x700) | f_num_low;
                    self.ch3_fnum[3] = new_f_num;
                    // Also update the channel's operator
                    self.channels[channel_idx].operator_mut(3).freq_number = new_f_num;
                    self.channels[channel_idx]
                        .operator_mut(3)
                        .update_phase_increment();
                    self.channels[channel_idx].operator_mut(3).update_key_code();
                    // Sync per-operator freq for LFO PM
                    self.channels[2].op_fnum[3] = new_f_num;
                } else {
                    // Normal mode: update all operators
                    let current_f_num = self.channels[channel_idx].operator(0).freq_number;
                    let new_f_num = (current_f_num & 0x700) | f_num_low;
                    // Update channel base frequency for LFO PM calculation
                    let block = self.channels[channel_idx].operator(0).block;
                    self.channels[channel_idx].set_base_frequency(new_f_num, block);
                    for i in 0..4 {
                        self.channels[channel_idx].operator_mut(i).freq_number = new_f_num;
                        self.channels[channel_idx]
                            .operator_mut(i)
                            .update_phase_increment();
                        self.channels[channel_idx].operator_mut(i).update_key_code();
                    }
                }
            }

            0xA4..=0xA6 => {
                // F-Number (high 3 bits) and Block - per channel
                let block = (value >> 3) & 0x07;
                let f_num_high = (value & 0x07) as u16;

                // Update all operators in the channel (or CH3 OP3 if CH3 special mode)
                if channel_idx == 2 && (self.ch3_mode & 0x40) != 0 {
                    // CH3 special mode: only update OP3 (OP0-2 use independent frequencies)
                    let current_f_num = self.ch3_fnum[3];
                    let new_f_num = (current_f_num & 0xFF) | (f_num_high << 8);
                    self.ch3_fnum[3] = new_f_num;
                    self.ch3_block[3] = block;
                    // Also update the channel's operator
                    self.channels[channel_idx].operator_mut(3).freq_number = new_f_num;
                    self.channels[channel_idx].operator_mut(3).block = block;
                    self.channels[channel_idx]
                        .operator_mut(3)
                        .update_phase_increment();
                    self.channels[channel_idx].operator_mut(3).update_key_code();
                    // Sync per-operator freq for LFO PM
                    self.channels[2].op_fnum[3] = new_f_num;
                    self.channels[2].op_block[3] = block;
                } else {
                    // Normal mode: update all operators
                    let current_f_num = self.channels[channel_idx].operator(0).freq_number;
                    let new_f_num = (current_f_num & 0xFF) | (f_num_high << 8);
                    // Update channel base frequency for LFO PM calculation
                    self.channels[channel_idx].set_base_frequency(new_f_num, block);
                    for i in 0..4 {
                        self.channels[channel_idx].operator_mut(i).freq_number = new_f_num;
                        self.channels[channel_idx].operator_mut(i).block = block;
                        self.channels[channel_idx]
                            .operator_mut(i)
                            .update_phase_increment();
                        self.channels[channel_idx].operator_mut(i).update_key_code();
                    }
                }
            }

            0xB0..=0xB2 => {
                // Feedback (FB) and Algorithm (ALG)
                let feedback = (value >> 3) & 0x07;
                let algorithm = value & 0x07;

                self.channels[channel_idx].set_feedback(feedback);
                self.channels[channel_idx].set_algorithm(algorithm);
            }

            0xB4..=0xB6 => {
                // Stereo output (L/R), AMS and PMS - per channel
                // Bit 7: L (Left) output enable
                // Bit 6: R (Right) output enable
                // Bits 5-4: AMS (Amplitude Modulation Sensitivity, 0-3) - PER CHANNEL
                // Bits 2-0: PMS (Pitch Modulation Sensitivity, 0-7)
                let stereo = StereoFlags::from_register(value);
                let ams = (value >> 4) & 0x03; // Bits 5-4: AMS per channel
                let pms = value & 0x07; // Bits 2-0: PMS per channel

                self.channels[channel_idx].set_stereo(stereo);
                self.channels[channel_idx].set_pms(pms);

                // AMS is per-channel: set on all 4 operators
                // (AM enable is per-operator, set via register 0x60 bit 7)
                for i in 0..4 {
                    self.channels[channel_idx].operator_mut(i).set_ams(ams);
                }
            }

            _ => {}
        }
    }

    /// Render every whole output sample owed for the clocks accumulated since the
    /// last render point, appending them to `frame_samples` using the chip's
    /// current register state. Called before each register write (so the write's
    /// effect starts at the right sample) and once more at frame end by
    /// `generate_samples()`.
    fn catch_up(&mut self) {
        if !self.rendering_enabled {
            // Audio off: don't synthesize, just keep the accumulator bounded.
            if self.clock_accumulator >= self.clocks_per_sample {
                self.clock_accumulator %= self.clocks_per_sample;
            }
            return;
        }

        while self.clock_accumulator >= self.clocks_per_sample {
            self.clock_accumulator -= self.clocks_per_sample;
            let (left, right) = self.generate_one_sample();
            self.frame_samples.push(left);
            self.frame_samples.push(right);
        }
        // Bound the buffer if the host stops draining it, so it can't grow without limit.
        if self.frame_samples.len() > MAX_FRAME_SAMPLES {
            let excess = self.frame_samples.len() - MAX_FRAME_SAMPLES;
            self.frame_samples.drain(..excess);
        }
    }

    /// Generate one output sample by running the full FM pipeline once.
    /// Returns (left, right) in [-1.0, 1.0].
    /// Called ~735 times per frame (at 44.1 kHz / 60 fps) instead of ~128K times.
    fn generate_one_sample(&mut self) -> (f32, f32) {
        self.lfo.clock();

        // EG advance: hardware rate = clock_freq / 432 ~ 17.8 kHz.
        // eg_samples_per_tick = sample_rate / (clock_freq / 432) ~ 2.48
        self.eg_accumulator += 1.0;
        let advance_eg = self.eg_accumulator >= self.eg_samples_per_tick;
        if advance_eg {
            self.eg_accumulator -= self.eg_samples_per_tick;
            self.eg_counter += 1;
            if self.eg_counter >= 4096 {
                self.eg_counter = 1;
            }
        }

        let pm_step = self.lfo.get_pm_step();
        let am_value = self.lfo.get_am_value();

        let mut left_sum = 0.0f32;
        let mut right_sum = 0.0f32;

        for i in 0..6 {
            if self.mute_flags[i] {
                self.channel_outputs[i] = (0.0, 0.0);
                continue;
            }

            if i == 5 && self.dac_enabled {
                let dac_scaled = (self.dac_value as i32) << 6;
                let dac_f32 = dac_scaled as f32 / 8192.0;
                let stereo = self.channels[5].get_stereo();
                let left_dac = if stereo.left { dac_f32 } else { 0.0 };
                let right_dac = if stereo.right { dac_f32 } else { 0.0 };
                self.channel_outputs[i] = (left_dac, right_dac);
                left_sum += left_dac;
                right_sum += right_dac;
            } else {
                let (left, right) =
                    self.channels[i].clock_with_lfo_eg(pm_step, am_value, advance_eg);
                let left_f32 = left as f32 / 8192.0;
                let right_f32 = right as f32 / 8192.0;
                self.channel_outputs[i] = (left_f32, right_f32);
                left_sum += left_f32;
                right_sum += right_f32;
            }
        }

        left_sum /= 6.0;
        right_sum /= 6.0;

        let left_sample = left_sum.clamp(-1.0, 1.0);
        let right_sample = right_sum.clamp(-1.0, 1.0);

        let abs_l = left_sample.abs();
        let abs_r = right_sample.abs();
        self.output_level = if abs_l > abs_r { abs_l } else { abs_r };

        // Waveform visualization
        let mono = (left_sample + right_sample) * 0.5;
        if self.waveform_buffer.len() >= self.max_waveform_samples {
            self.waveform_buffer.pop_front();
        }
        self.waveform_buffer.push_back(mono);

        (left_sample, right_sample)
    }

    /// Run timers for the given number of input clock cycles.
    /// Timers tick at the internal FM rate (every 144 input clocks).
    fn clock_timers(&mut self, cycles: u32) {
        self.timer_accumulator += cycles as f32;
        if self.timer_accumulator > MAX_TIMER_ACCUMULATOR {
            self.timer_accumulator = MAX_TIMER_ACCUMULATOR;
        }
        let mut timer_ticks: i32 = 0;
        while self.timer_accumulator >= 144.0 {
            self.timer_accumulator -= 144.0;
            timer_ticks += 1;

            self.csm_key_state <<= 1;
            if self.csm_key_state & 0x02 != 0 {
                self.channels[2].key_off_all();
                self.csm_key_state &= !0x02;
            }

            let timer_a_overflow = self.timers.clock_timer_a();
            if timer_a_overflow && self.timers.is_csm_mode() {
                self.channels[2].key_on_all();
                self.csm_key_state |= 0x01;
            }
        }
        if timer_ticks > 0 {
            self.timers.clock_timer_b(timer_ticks);
        }
    }

    /// Get a reference to a channel
    pub fn channel(&self, index: usize) -> Option<&Channel> {
        self.channels.get(index)
    }

    /// Get a mutable reference to a channel
    pub fn channel_mut(&mut self, index: usize) -> Option<&mut Channel> {
        self.channels.get_mut(index)
    }

    /// Reset the chip to initial state
    pub fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.reset();
        }

        self.lfo.reset();
        self.timers.reset();
        self.dac_enabled = false;
        self.dac_value = 0;
        self.clock_accumulator = 0.0;
        self.timer_accumulator = 0.0;
        self.address_latch_part0 = 0;
        self.address_latch_part1 = 0;
        self.mute_flags = [false; 6];
        self.output_level = 0.0;
        self.waveform_buffer.clear();
        self.channel_outputs = [(0.0, 0.0); 6];
        self.eg_accumulator = 0.0;
        self.eg_counter = 0;
        self.busy_counter = 0;
        self.busy_end_cycle = 0;
        self.frame_samples.clear();
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
    ///   - first renders audio produced up to this moment (so the reset doesn't
    ///     retroactively silence sound that already played this frame),
    ///   - clears FM synthesis + timer state, and
    ///   - preserves the transient output buffer (`frame_samples`), the sub-sample
    ///     clock phase (`clock_accumulator`), debugger mutes (`mute_flags`), the
    ///     host render gate (`rendering_enabled`), and the sample-rate config.
    pub fn ic_reset(&mut self) {
        self.catch_up();

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
        self.eg_accumulator = 0.0;
        self.eg_counter = 0;
        self.busy_counter = 0;
        self.busy_end_cycle = 0;
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
        // Render the audio produced up to this write using the current register
        // state, so the write's effect begins at the correct sample position.
        // (Genesis Plus GX and PicoDrive run the chip to the write's time the
        // same way before they apply a register change.)
        self.catch_up();
        match port {
            0 => {
                // Address latch Part 0
                self.address_latch_part0 = value;
            }
            1 => {
                // Data Part 0
                self.write_register(0, self.address_latch_part0, value);
                self.log_fm_write(0, self.address_latch_part0, value);
                // Set busy flag for 32 * prescale(6) = 192 input clock cycles
                self.busy_counter = 192;
                self.busy_end_cycle = self.current_cpu_cycle + 192;
            }
            2 => {
                // Address latch Part 1
                self.address_latch_part1 = value;
            }
            3 => {
                // Data Part 1
                self.write_register(1, self.address_latch_part1, value);
                self.log_fm_write(1, self.address_latch_part1, value);
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

// ===== TRAIT IMPLEMENTATIONS =====

impl Clockable for Ym2612 {
    fn clock(&mut self, cycles: u32) -> u32 {
        // Accumulate input clocks and run timers. Audio is rendered incrementally
        // by `catch_up()` at each register write and flushed by `generate_samples()`,
        // so register changes land at their true sub-frame sample position.
        self.busy_counter = self.busy_counter.saturating_sub(cycles);
        self.clock_timers(cycles);
        self.clock_accumulator += cycles as f32;
        cycles
    }
}

impl AudioChip for Ym2612 {
    fn chip_name(&self) -> &'static str {
        "YM2612"
    }

    fn get_info(&self) -> AudioChipInfo {
        AudioChipInfo {
            name: "YM2612",
            manufacturer: "Yamaha",
            channels: 6,
            sample_rate: self.sample_rate,
            stereo: true,
            channel_types: vec![
                ChannelType::FM,
                ChannelType::FM,
                ChannelType::FM,
                ChannelType::FM,
                ChannelType::FM,
                if self.dac_enabled {
                    ChannelType::PCM
                } else {
                    ChannelType::FM
                }, // Channel 6: FM or DAC
            ],
        }
    }

    /// `address` is the port, 0-3, as at $4000-$4003: port 0 (or 2) latches
    /// a register number of part 0 (or 1), port 1 (or 3) writes its data.
    /// A single byte cannot hold both a part and a register number, so the
    /// chip is driven here as a program drives it.
    fn write_register(&mut self, address: u8, data: u8) {
        self.write_port(address & 0x03, data);
    }

    /// `address` is the port, 0-3: every port reads the status register.
    fn read_register(&mut self, address: u8) -> u8 {
        self.read_port(address & 0x03)
    }

    fn generate_samples(&mut self, output: &mut [f32], sample_rate: u32) -> usize {
        if sample_rate != self.sample_rate {
            self.sample_rate = sample_rate;
            self.clocks_per_sample = self.clock_freq as f32 / sample_rate as f32;
            self.eg_samples_per_tick = sample_rate as f32 / (self.clock_freq as f32 / 432.0);
            for ch in &mut self.channels {
                ch.set_sample_rate(sample_rate);
            }
        }

        // Flush the samples owed for the clocks accumulated since the last write
        // (rendered at the current register state), then hand the caller what it
        // asked for. The drift-free sample *count* over time is owned by the
        // system's cycles->samples converter (see `system.rs`), not by this buffer,
        // so any whole samples beyond what the caller drains are a sub-sample
        // rounding tail that must NOT persist: carrying them would replay stale
        // audio on the next call after a register write has since changed the chip
        // state. Drop the remainder so every `generate_samples` reflects state up
        // to *now*. Sub-frame accuracy is provided by `catch_up()` running at each
        // `write_port`, not by cross-call buffering.
        self.catch_up();

        let want = output.len() & !1; // copy whole stereo pairs only

        // Fill a one-pair shortfall rather than letting it reach the mixer as
        // silence. The system's cycles->samples converter owns the frame's
        // sample count, and this chip's `clock_accumulator` is a second,
        // floating-point quantisation of the same elapsed time -- so on a few
        // frames in a hundred it lands exactly one stereo pair short of what
        // the caller asked for. The caller's buffer is pre-zeroed, so an unfilled
        // pair would reach the mixer as a sample of silence in the middle of a
        // sustained note: an audible tick a few times a second, loudest where
        // the waveform sits furthest from zero.
        //
        // Each borrowed pair is charged to the accumulator, so the chip gives
        // the time back on a later frame and its long-run rate is unchanged.
        // The borrow is capped because two quantisations of one elapsed time
        // can only disagree by about a pair: a caller handing over an oversized
        // scratch buffer is asking for audio the machine has not lived through
        // yet, and that must not drag the chip ahead of it.
        if self.rendering_enabled {
            let mut borrowed = 0;
            while self.frame_samples.len() < want && borrowed < MAX_BORROWED_PAIRS {
                self.clock_accumulator -= self.clocks_per_sample;
                let (left, right) = self.generate_one_sample();
                self.frame_samples.push(left);
                self.frame_samples.push(right);
                borrowed += 1;
            }
        }

        let n = want.min(self.frame_samples.len() & !1);
        output[..n].copy_from_slice(&self.frame_samples[..n]);
        self.frame_samples.clear();

        n
    }

    fn reset(&mut self) {
        Ym2612::reset(self);
    }

    fn get_output_level(&self) -> f32 {
        self.output_level
    }
}

impl AudioDebugger for Ym2612 {
    fn get_channel_count(&self) -> u32 {
        6
    }

    fn get_channel_state(&self, channel: u32) -> Option<ChannelState> {
        if channel >= 6 {
            return None;
        }

        let ch_idx = channel as usize;
        let ch = &self.channels[ch_idx];

        // Get operator 1 (slot 1; a carrier only in algorithm 7) info for channel-level data
        let op = ch.operator(0);

        // Convert YM2612 envelope state to retro-core envelope state
        let envelope = {
            let env = op.envelope();
            Some(AudioEnvelopeState {
                attack: env.attack_rate as f32 / 31.0,
                decay: env.decay_rate as f32 / 31.0,
                sustain: (15 - env.sustain_level) as f32 / 15.0, // Invert: 0=loud, 15=silent
                release: env.release_rate as f32 / 15.0,
                current_phase: match env.get_state() {
                    EnvelopeState::Off => EnvelopePhase::Off,
                    EnvelopeState::Attack => EnvelopePhase::Attack,
                    EnvelopeState::Decay => EnvelopePhase::Decay,
                    EnvelopeState::Sustain => EnvelopePhase::Sustain,
                    EnvelopeState::Release => EnvelopePhase::Release,
                },
            })
        };

        // Calculate pan from stereo flags
        let stereo = ch.get_stereo();
        let pan = if stereo.left && stereo.right {
            0.0 // Center
        } else if stereo.left {
            -1.0 // Left
        } else if stereo.right {
            1.0 // Right
        } else {
            0.0 // Muted (use center)
        };

        // Get current output
        let (left_out, right_out) = self.channel_outputs[ch_idx];
        let current_output = (left_out + right_out) / 2.0;

        // Check if channel is enabled (any operator has key on)
        let enabled = (0..4).any(|i| ch.operator(i).envelope().is_key_on());

        Some(ChannelState {
            channel_id: channel,
            enabled,
            muted: self.mute_flags[ch_idx],
            frequency: op.get_frequency_hz() as f32,
            frequency_raw: op.freq_number,
            period: 0, // TODO: calculate from phase increment
            volume: 1.0 - (op.total_level as f32 / 127.0), // TL is attenuation
            attenuation: op.total_level,
            pan,
            channel_type: if ch_idx == 5 && self.dac_enabled {
                ChannelType::PCM
            } else {
                ChannelType::FM
            },
            envelope,
            current_output,
            noise_params: None,
        })
    }

    fn mute_channel(&mut self, channel: u32, muted: bool) {
        if channel < 6 {
            self.mute_flags[channel as usize] = muted;
        }
    }

    fn get_waveform(&self, channel: u32, samples: u32) -> Vec<f32> {
        if channel >= 6 {
            return vec![0.0; samples as usize];
        }

        // TODO: per-channel waveform buffers; without them this returns silence.
        vec![0.0; samples as usize]
    }

    fn get_output_waveform(&self, samples: u32) -> Vec<f32> {
        let samples = samples as usize;

        if self.waveform_buffer.len() >= samples {
            // Return last N samples from ring buffer
            self.waveform_buffer
                .iter()
                .skip(self.waveform_buffer.len() - samples)
                .copied()
                .collect()
        } else {
            // Pad with zeros if we don't have enough samples yet
            let mut result = vec![0.0; samples - self.waveform_buffer.len()];
            result.extend(self.waveform_buffer.iter());
            result
        }
    }
}

// Savestate support
use crate::system::savestate::Ym2612State;

impl Ym2612 {
    pub fn to_savestate(&self) -> Ym2612State {
        let (lfo_enabled, lfo_freq, lfo_phase, lfo_counter) = self.lfo.to_savestate();
        Ym2612State {
            channels: [
                self.channels[0].to_savestate(),
                self.channels[1].to_savestate(),
                self.channels[2].to_savestate(),
                self.channels[3].to_savestate(),
                self.channels[4].to_savestate(),
                self.channels[5].to_savestate(),
            ],
            lfo_enabled,
            lfo_freq,
            lfo_phase,
            lfo_counter,
            dac_enabled: self.dac_enabled,
            dac_value: self.dac_value,
            dac_start_value: self.dac_start_value,
            clock_accumulator: self.clock_accumulator,
            timer_accumulator: self.timer_accumulator,
            ch3_mode: self.ch3_mode,
            ch3_fnum: self.ch3_fnum,
            ch3_block: self.ch3_block,
            csm_key_state: self.csm_key_state,
            address_latch_part0: self.address_latch_part0,
            address_latch_part1: self.address_latch_part1,
            eg_accumulator: self.eg_accumulator,
            eg_counter: self.eg_counter,
            busy_end_cycle: self.busy_end_cycle,
            current_cpu_cycle: self.current_cpu_cycle,
            timer: self.timers.to_savestate(),
        }
    }

    pub fn from_savestate(&mut self, s: &Ym2612State) {
        for i in 0..6 {
            self.channels[i].from_savestate(&s.channels[i]);
        }
        self.lfo
            .from_savestate(s.lfo_enabled, s.lfo_freq, s.lfo_phase, s.lfo_counter);
        self.dac_enabled = s.dac_enabled;
        self.dac_value = s.dac_value;
        self.dac_start_value = s.dac_start_value;
        self.clock_accumulator = s.clock_accumulator;
        self.timer_accumulator = s.timer_accumulator;
        self.ch3_mode = s.ch3_mode;
        self.ch3_fnum = s.ch3_fnum;
        self.ch3_block = s.ch3_block;
        self.csm_key_state = s.csm_key_state;
        self.address_latch_part0 = s.address_latch_part0;
        self.address_latch_part1 = s.address_latch_part1;
        self.eg_accumulator = s.eg_accumulator;
        self.eg_counter = s.eg_counter;
        self.busy_end_cycle = s.busy_end_cycle;
        self.current_cpu_cycle = s.current_cpu_cycle;
        self.timers.from_savestate(&s.timer);
        self.frame_samples.clear();
    }
}

#[cfg(test)]
mod tests {
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
}
