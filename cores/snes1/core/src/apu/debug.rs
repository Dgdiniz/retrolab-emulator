//! [`AudioDebugger`] view over the S-DSP.
//!
//! The trait is implemented on [`Apu`] (not the `SDsp` directly)
//! because the eight-voice register/synthesis state lives in the
//! private `SDsp`, while the mixed 32 kHz output the debugger
//! visualises lives in the APU's host audio ring. `Apu` owns both, so
//! it is the natural home for the trait.
//!
//! Per the SG1 precedent, `DebuggerCore::audio_debugger()` returns
//! `None` (the debugger struct doesn't own the APU); the host reaches
//! this view through `SnesEmulator::audio_debugger()` instead.

use retro_core::audio::{AudioDebugger, ChannelState, ChannelType, EnvelopePhase, EnvelopeState};

use super::Apu;

/// SNES voices run at the DSP's 32 kHz output rate.
const DSP_OUTPUT_RATE_HZ: f32 = 32_000.0;
/// The S-DSP has exactly eight PCM voices.
const VOICE_COUNT: u32 = 8;

impl AudioDebugger for Apu {
    fn get_channel_count(&self) -> u32 {
        VOICE_COUNT
    }

    fn get_channel_state(&self, channel: u32) -> Option<ChannelState> {
        if channel >= VOICE_COUNT {
            return None;
        }
        let vd = self.state.dsp.voice_debug(channel as usize);

        // Pitch -> Hz: the DSP advances one source sample per
        // `pitch / 0x1000` output samples, so the playback frequency is
        // `32000 * pitch / 4096`.
        let frequency = DSP_OUTPUT_RATE_HZ * (vd.pitch as f32) / 4096.0;

        let left = (vd.voll as i32).unsigned_abs();
        let right = (vd.volr as i32).unsigned_abs();
        let volume = (left.max(right) as f32 / 127.0).min(1.0);
        let pan = if left + right == 0 {
            0.0
        } else {
            (right as f32 - left as f32) / (left + right) as f32
        };

        let envelope = Some(decode_envelope(&vd));

        // A voice counts as "live" while keyed on or still releasing
        // (envelope above zero), but a voice that has ended and decayed
        // to silence is not.
        let enabled = (vd.key_on || vd.env_level > 0) && !(vd.ended && vd.env_level == 0);

        Some(ChannelState {
            channel_id: channel,
            enabled,
            muted: vd.muted,
            frequency,
            frequency_raw: vd.pitch,
            period: 0,
            volume,
            // SNES voices have no single attenuation register; expose
            // the larger raw L/R magnitude as a stand-in.
            attenuation: left.max(right) as u8,
            pan,
            channel_type: if vd.noise {
                ChannelType::Noise
            } else {
                ChannelType::Sample
            },
            envelope,
            current_output: vd.outx as f32 / 128.0,
            // SNES noise is a shared LFSR with a global rate, not the
            // PSG-style per-channel taps `NoiseParams` models, so this
            // stays `None` even for noise-enabled voices.
            noise_params: None,
        })
    }

    fn mute_channel(&mut self, channel: u32, muted: bool) {
        if channel < VOICE_COUNT {
            self.state.dsp.set_voice_mute(channel as usize, muted);
        }
    }

    fn get_waveform(&self, channel: u32, samples: u32) -> Vec<f32> {
        let n = samples as usize;
        if channel >= VOICE_COUNT || n == 0 {
            return vec![0.0; n];
        }
        // The DSP sums all eight voices into a single ring before it is
        // captured, so a true per-voice tap isn't retained. Approximate
        // it by scaling the mixed output by this voice's instantaneous
        // normalized amplitude (envelope x peak volume); a muted or
        // keyed-off voice therefore reads as silence.
        let vd = self.state.dsp.voice_debug(channel as usize);
        if vd.muted {
            return vec![0.0; n];
        }
        let env = (vd.env_level as f32 / 2047.0).clamp(0.0, 1.0);
        let peak = ((vd.voll as i32)
            .unsigned_abs()
            .max((vd.volr as i32).unsigned_abs())) as f32
            / 127.0;
        let amp = env * peak;
        if amp == 0.0 {
            return vec![0.0; n];
        }
        self.get_output_waveform(samples)
            .into_iter()
            .map(|s| s * amp)
            .collect()
    }

    fn get_output_waveform(&self, samples: u32) -> Vec<f32> {
        let n = samples as usize;
        if n == 0 {
            return Vec::new();
        }
        // The ring is stereo-interleaved i16; fold each L/R pair to a
        // mono f32 in [-1, 1]. Peeking leaves the ring intact for the
        // host drain.
        let mut interleaved = vec![0i16; n * 2];
        self.audio_ring.peek_recent(&mut interleaved);
        interleaved
            .as_chunks::<2>()
            .0
            .iter()
            .map(|lr| {
                let l = lr[0] as f32 / 32768.0;
                let r = lr[1] as f32 / 32768.0;
                (l + r) * 0.5
            })
            .collect()
    }
}

/// Decode a voice's ADSR/GAIN registers + live stage into the generic
/// [`EnvelopeState`]. The four rate floats are normalized 0..1 hints
/// for the UI, not exact times.
fn decode_envelope(vd: &super::dsp::VoiceDebug) -> EnvelopeState {
    let (attack, decay, sustain, release) = if vd.adsr_enabled {
        let ar = (vd.adsr0 & 0x0f) as f32 / 15.0;
        let dr = ((vd.adsr0 >> 4) & 0x07) as f32 / 7.0;
        let sl = ((vd.adsr1 >> 5) & 0x07) as f32 / 7.0;
        let sr = (vd.adsr1 & 0x1f) as f32 / 31.0;
        (ar, dr, sl, sr)
    } else {
        // GAIN mode: a single level/rate byte drives the envelope.
        (0.0, 0.0, (vd.gain & 0x7f) as f32 / 127.0, 0.0)
    };

    let current_phase = if vd.env_level == 0 {
        EnvelopePhase::Off
    } else {
        match vd.env_mode {
            1 => EnvelopePhase::Attack,
            2 => EnvelopePhase::Decay,
            3 => EnvelopePhase::Sustain,
            _ => EnvelopePhase::Release,
        }
    };

    EnvelopeState {
        attack,
        decay,
        sustain,
        release,
        current_phase,
    }
}

#[cfg(test)]
mod tests {
    use crate::apu::Apu;
    use retro_core::audio::{AudioDebugger, ChannelType};

    /// Write S-DSP register `reg` = `val` through the real `$F2`/`$F3`
    /// address/data port pair (no CPU stepping required).
    fn write_dsp(apu: &mut Apu, reg: u8, val: u8) {
        apu.spc_write(0x00F2, reg);
        apu.spc_write(0x00F3, val);
    }

    #[test]
    fn channel_count_is_eight() {
        assert_eq!(Apu::new().get_channel_count(), 8);
    }

    /// Voice volume / pitch registers decode into the generic
    /// `ChannelState` (frequency in Hz, normalized volume, L/R pan).
    #[test]
    fn channel_state_decodes_volume_and_pitch() {
        let mut apu = Apu::new();
        write_dsp(&mut apu, 0x00, 0x7F); // V0 VOLL = +127
        write_dsp(&mut apu, 0x01, 0x40); // V0 VOLR = +64
        write_dsp(&mut apu, 0x02, 0x00); // V0 PITCHL
        write_dsp(&mut apu, 0x03, 0x10); // V0 PITCHH -> pitch = $1000

        let st = apu.get_channel_state(0).expect("voice 0 in range");
        assert_eq!(st.channel_id, 0);
        assert_eq!(st.frequency_raw, 0x1000);
        // 32 kHz x pitch/4096 = 32000 Hz at pitch $1000.
        assert!(
            (st.frequency - 32_000.0).abs() < 0.5,
            "freq {}",
            st.frequency
        );
        assert!((st.volume - 1.0).abs() < 1e-6, "vol {}", st.volume);
        // VOLR < VOLL -> the field pans left (negative).
        assert!(st.pan < 0.0 && st.pan > -1.0, "pan {}", st.pan);
        assert!(matches!(st.channel_type, ChannelType::Sample));
        assert!(!st.muted);

        // Out-of-range voice index has no state.
        assert!(apu.get_channel_state(8).is_none());
    }

    /// `mute_channel` flips the per-voice mute flag, which the channel
    /// state reports and the per-voice waveform tap honours (silence).
    #[test]
    fn mute_channel_sets_muted_flag_and_silences_waveform() {
        let mut apu = Apu::new();
        write_dsp(&mut apu, 0x00, 0x7F); // give voice 0 some volume
        assert!(!apu.get_channel_state(0).unwrap().muted);

        apu.mute_channel(0, true);
        assert!(apu.get_channel_state(0).unwrap().muted);
        let wf = apu.get_waveform(0, 16);
        assert_eq!(wf.len(), 16);
        assert!(wf.iter().all(|&s| s == 0.0), "muted voice is silent");

        apu.mute_channel(0, false);
        assert!(!apu.get_channel_state(0).unwrap().muted);
    }

    /// The output-waveform tap returns exactly `samples` mono values and
    /// peeks the ring non-destructively (empty ring -> silence).
    #[test]
    fn output_waveform_length_and_empty_ring_silence() {
        let apu = Apu::new();
        let wf = apu.get_output_waveform(64);
        assert_eq!(wf.len(), 64);
        assert!(wf.iter().all(|&s| s == 0.0), "fresh ring is silent");
        assert!(apu.get_output_waveform(0).is_empty());
    }
}
