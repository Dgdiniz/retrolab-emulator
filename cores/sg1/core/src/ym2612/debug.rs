//! The debugger's view of the chip: its channels as a host reads them,
//! the mute, and the waveforms.

use retro_core::audio::{
    AudioChip, AudioChipInfo, AudioDebugger, ChannelState, ChannelType, EnvelopePhase,
    EnvelopeState as AudioEnvelopeState,
};

use super::{EnvelopeState, Ym2612};

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
