//! The debugger's view of the chip: its channels as a host reads them,
//! each channel's patch, the keys the sound log takes down, the mute, and
//! the waveforms.

use retro_core::audio::{
    AudioChip, AudioChipInfo, AudioDebugger, ChannelState, ChannelType, EnvelopePhase,
    EnvelopeState as AudioEnvelopeState, SoundEvent,
};
use retro_core::hash::fnv1a64;
use serde::Serialize;

use super::algorithms::get_output_operators;
use super::registers::SLOT_OPERATORS;
use super::{EnvelopeState, Operator, Ym2612};

/// A channel's patch: what it plays, with no note. The operators are in
/// register-slot order, S1, S3, S2, S4, the order their registers sit in
/// (offsets 0, 4, 8 and 12 of each register group): the first modulator,
/// the second modulator, the first carrier and the second carrier, as the
/// chip's manual names them for algorithm 0. A format that orders them
/// otherwise maps them one by one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FmPatch {
    pub algorithm: u8,
    pub feedback: u8,
    /// The channel's left and right outputs.
    pub left: bool,
    pub right: bool,
    /// The sensitivities to the LFO: amplitude (0-3) and phase (0-7).
    pub ams: u8,
    pub pms: u8,
    pub operators: [FmOperator; 4],
}

/// One operator of a patch, its registers' fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FmOperator {
    pub detune: u8,
    pub multiple: u8,
    pub total_level: u8,
    pub rate_scaling: u8,
    pub attack_rate: u8,
    /// Whether the LFO's amplitude modulation reaches it.
    pub am: bool,
    pub decay_rate: u8,
    pub sustain_rate: u8,
    pub sustain_level: u8,
    pub release_rate: u8,
    pub ssg_eg: u8,
}

impl FmOperator {
    fn of(operator: &Operator) -> Self {
        let envelope = operator.envelope();
        Self {
            detune: operator.detune,
            multiple: operator.multiple,
            total_level: operator.total_level,
            rate_scaling: operator.rate_scaling,
            attack_rate: envelope.attack_rate,
            am: operator.am_enable,
            decay_rate: envelope.decay_rate,
            sustain_rate: envelope.sustain_rate,
            sustain_level: envelope.sustain_level,
            release_rate: envelope.release_rate,
            ssg_eg: envelope.get_ssg_eg(),
        }
    }
}

/// The carriers of `algorithm`, the operators the channel's output sums, in
/// this core's order.
fn carriers(algorithm: super::Algorithm) -> Vec<usize> {
    get_output_operators(algorithm)
        .into_iter()
        .map(|slot| SLOT_OPERATORS[slot])
        .collect()
}

impl Ym2612 {
    /// Channel `channel`'s patch (0-5); `None` past the sixth.
    pub fn patch(&self, channel: usize) -> Option<FmPatch> {
        let state = self.channels.get(channel)?;
        let stereo = state.get_stereo();
        Some(FmPatch {
            algorithm: state.get_algorithm().as_u8(),
            feedback: state.get_feedback(),
            left: stereo.left,
            right: stereo.right,
            ams: state.operator(0).get_ams(),
            pms: state.get_pms(),
            operators: std::array::from_fn(|i| FmOperator::of(state.operator(i))),
        })
    }

    /// What channel `channel` plays, as a key-on names it: the FNV-1a hash
    /// of its patch less its stereo and its carriers' total levels, which
    /// are where it plays and how loud, not what. The bytes hashed: the
    /// algorithm, the feedback, AMS and PMS, then each operator's eleven
    /// fields in register-slot order, a carrier's total level as 0xFF.
    pub fn instrument(&self, channel: usize) -> u64 {
        let Some(patch) = self.patch(channel) else {
            return 0;
        };
        let carriers = carriers(self.channels[channel].get_algorithm());
        let mut bytes = vec![patch.algorithm, patch.feedback, patch.ams, patch.pms];
        for (index, operator) in patch.operators.iter().enumerate() {
            let total_level = match carriers.contains(&index) {
                true => 0xFF,
                false => operator.total_level,
            };
            bytes.extend_from_slice(&[
                operator.detune,
                operator.multiple,
                total_level,
                operator.rate_scaling,
                operator.attack_rate,
                u8::from(operator.am),
                operator.decay_rate,
                operator.sustain_rate,
                operator.sustain_level,
                operator.release_rate,
                operator.ssg_eg,
            ]);
        }
        fnv1a64(&bytes)
    }

    /// Channel `channel`'s loudest carrier: the one with the least total
    /// level (attenuation), the first of them at a tie.
    fn loudest_carrier(&self, channel: usize) -> &Operator {
        let state = &self.channels[channel];
        carriers(state.get_algorithm())
            .into_iter()
            .map(|index| state.operator(index))
            .min_by_key(|operator| operator.total_level)
            .expect("every algorithm has a carrier")
    }

    /// The operators of channel `channel` keyed on, bit `i` for operator
    /// `i` in this core's order.
    pub(super) fn keyed_operators(&self, channel: usize) -> u8 {
        let state = &self.channels[channel];
        (0..4)
            .filter(|&index| state.operator(index).is_key_on())
            .fold(0, |keyed, index| keyed | 1 << index)
    }

    /// Channel `channel`'s note as a key-on names it: its instrument, its
    /// F-number with the block above it (`fnum | block << 11`), and its
    /// loudest carrier's total level.
    fn key_on(&self, channel: usize) -> SoundEvent {
        let (number, block) = self.channel_frequency(channel);
        SoundEvent::KeyOn {
            channel: channel as u8,
            instrument: self.instrument(channel),
            pitch: u32::from(number) | u32::from(block) << 11,
            level: u16::from(self.loudest_carrier(channel).total_level),
        }
    }

    /// Log, at input clock `clock`, what a change of channel `channel`'s
    /// keys from the operators `before` keyed did: a key-on when an
    /// operator started, a key-off when the last one stopped.
    pub(super) fn log_key_change(&mut self, clock: u64, channel: usize, before: u8) {
        let after = self.keyed_operators(channel);
        if after & !before != 0 {
            let event = self.key_on(channel);
            self.log_event(clock, event);
        } else if after == 0 && before != 0 {
            self.log_event(
                clock,
                SoundEvent::KeyOff {
                    channel: channel as u8,
                },
            );
        }
    }

    /// A key-on for each channel keyed now: the notes a log of the keys
    /// starts with.
    pub fn key_image(&self) -> Vec<SoundEvent> {
        (0..self.channels.len())
            .filter(|&channel| self.keyed_operators(channel) != 0)
            .map(|channel| self.key_on(channel))
            .collect()
    }
}

impl AudioDebugger for Ym2612 {
    fn chip_info(&self) -> AudioChipInfo {
        AudioChip::get_info(self)
    }

    fn get_channel_count(&self) -> u32 {
        6
    }

    fn get_channel_state(&self, channel: u32) -> Option<ChannelState> {
        if channel >= 6 {
            return None;
        }

        let ch_idx = channel as usize;
        let ch = &self.channels[ch_idx];

        // Operator 0 (slot 1, a carrier only in algorithm 7) for the
        // envelope and the frequency, which every operator shares outside
        // channel 3's special mode; the loudest carrier for the level.
        let op = ch.operator(0);
        let loudest = self.loudest_carrier(ch_idx);

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
            volume: 1.0 - (loudest.total_level as f32 / 127.0), // TL is attenuation
            attenuation: loudest.total_level,
            pan,
            channel_type: if ch_idx == 5 && self.dac_enabled {
                ChannelType::PCM
            } else {
                ChannelType::FM
            },
            envelope,
            current_output,
            noise_params: None,
            sample: None,
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

        // The stems' tap keeps each channel's last samples while the host
        // takes stems; otherwise there are none to show.
        match &self.stems {
            Some(stems) => stems.scope(channel as usize, samples),
            None => vec![0.0; samples as usize],
        }
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
