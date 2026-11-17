//! Audio: the YM2612 and the PSG mixed at a frame's end into the host's
//! ring, and the sound chips' debugger readers.

use super::GenesisEmulator;

impl GenesisEmulator {
    /// Get YM2612 channel state for debugging.
    pub fn get_ym2612_channel_state(
        &self,
        channel: u32,
    ) -> Option<retro_core::audio::ChannelState> {
        use retro_core::audio::AudioDebugger;
        self.ym2612().get_channel_state(channel)
    }

    /// Mute/unmute YM2612 channel.
    pub fn mute_ym2612_channel(&mut self, channel: u32, muted: bool) {
        use retro_core::audio::AudioDebugger;
        self.ym2612_mut().mute_channel(channel, muted);
    }

    /// Get YM2612 LFO state.
    pub fn get_ym2612_lfo_state(&self) -> serde_json::Value {
        let lfo = self.ym2612().lfo();
        serde_json::json!({
            "enabled": lfo.is_enabled(),
            "frequency": lfo.get_frequency(),
        })
    }

    /// Get YM2612 DAC state.
    pub fn get_ym2612_dac_state(&self) -> serde_json::Value {
        let ym = self.ym2612();
        serde_json::json!({
            "enabled": ym.is_dac_enabled(),
        })
    }

    /// Get YM2612 Timer A state.
    pub fn get_ym2612_timer_a_state(&self) -> serde_json::Value {
        let timers = self.ym2612().timers();
        serde_json::json!({
            "running": timers.is_timer_a_running(),
            "period": timers.get_timer_a_period(),
            "overflowed": (timers.get_status() & 0x01) != 0,
        })
    }

    /// Get YM2612 Timer B state.
    pub fn get_ym2612_timer_b_state(&self) -> serde_json::Value {
        let timers = self.ym2612().timers();
        serde_json::json!({
            "running": timers.is_timer_b_running(),
            "period": timers.get_timer_b_period(),
            "overflowed": (timers.get_status() & 0x02) != 0,
        })
    }

    /// Drain every available sample from the audio ring into a
    /// freshly-allocated `Vec<f32>`.
    ///
    /// Convenience API for callers (CLI, integration tests) that need
    /// an owned `Vec`. Hosts on the hot path should prefer
    /// [`Self::drain_audio_into`] (zero-alloc pull) and
    /// [`Self::audio_available`] (peek the queued count).
    pub fn get_audio_buffer(&mut self) -> Vec<f32> {
        let n = self.audio_ring.available();
        let mut out = vec![0.0; n];
        self.audio_ring.drain_into(&mut out);
        out
    }

    /// Drop every queued sample without delivering it. Used by reset
    /// paths and `set_audio_enabled(false)`.
    ///
    /// **Do not call from mid-stream code paths.** The ring is shared
    /// with the host's audio worklet (via repeated drains); clearing
    /// it while playback is live races the consumer and produces
    /// audible ticks. Hosts that want to skip ahead should simply
    /// stop pulling: overflow drops oldest samples automatically.
    pub fn clear_audio_buffer(&mut self) {
        self.audio_ring.clear();
    }

    /// Drop audio that has been generated but not yet handed to the host.
    ///
    /// Deliberately empty on this core, and that is the whole point: a
    /// headless frame here never queues anything. `generate_and_mix_audio`
    /// advances the converter and both chips, serialized state the desync
    /// hash covers, and then returns *before* the mix and the ring push, so
    /// a rollback re-simulation leaves nothing pending to discard. Clearing
    /// the host ring instead would throw away audio that was presented and
    /// is still on its way to the speaker.
    ///
    /// Exists so the netplay runner has one call it can make after a re-sim
    /// without asking which core it is talking to.
    pub fn discard_pending_audio(&mut self) {}

    /// Drain up to `out.len()` queued samples into `out`. Returns the
    /// number actually written; if fewer than `out.len()` are
    /// available the tail of `out` is left untouched (caller must
    /// `truncate` if they want the exact count).
    ///
    /// Zero-alloc pull path. Wired through the WASM bridge as
    /// `getAudioSamplesN(n)`.
    pub fn drain_audio_into(&mut self, out: &mut [f32]) -> usize {
        self.audio_ring.drain_into(out)
    }

    /// Number of stereo-interleaved samples currently queued. Used
    /// by the host to right-size pulls, see `getAudioAvailable()` in
    /// the WASM bridge.
    pub fn audio_available(&self) -> usize {
        self.audio_ring.available()
    }

    /// Cumulative count of "silence fillers" the ring inserted on
    /// drain because the host asked for more than was available.
    /// A non-zero value means the producer is behind, the symptom
    /// the user hears as audio dropouts. Exposed for telemetry /
    /// diagnostics.
    pub fn audio_ring_underruns(&self) -> u64 {
        self.audio_ring.underrun_count()
    }

    /// Cumulative count of samples the ring dropped from the front
    /// because incoming data wouldn't fit. > 0 means the host is
    /// behind, usually a stalled animation frame loop. Useful for
    /// the same telemetry overlay as `audio_ring_underruns()`.
    pub fn audio_ring_overflows(&self) -> u64 {
        self.audio_ring.overflow_count()
    }

    /// Get the mixed waveform ring buffer for visualization.
    /// Returns the last N mono samples of the mixed PSG+YM2612 output.
    pub fn get_mixed_waveform(&self, samples: u32) -> Vec<f32> {
        let count = (samples as usize).min(self.mixed_waveform_buffer.len());
        if count == 0 {
            return vec![0.0; samples as usize];
        }
        let skip = self.mixed_waveform_buffer.len() - count;
        let mut result: Vec<f32> = self
            .mixed_waveform_buffer
            .iter()
            .skip(skip)
            .copied()
            .collect();
        // Pad front with zeros if not enough samples yet
        if result.len() < samples as usize {
            let mut padded = vec![0.0; samples as usize - result.len()];
            padded.append(&mut result);
            padded
        } else {
            result
        }
    }
}

impl GenesisEmulator {
    /// Advance the audio subsystem one frame, and, when `present`, mix and
    /// push the samples to the output ring. Uses pre-allocated buffers to avoid
    /// per-frame heap allocations.
    ///
    /// The first half (advancing `audio_rate_converter` and pulling each chip's
    /// samples, which drains its `clock_accumulator`) mutates *serialized
    /// machine state*, state the netplay desync hash (`state_hash`) covers, so
    /// it MUST run on every frame, audible or headless. If a headless
    /// rollback/catch-up frame skipped it, a re-simulated span would leave the
    /// converter + chip phase frozen where a live audible run advanced them, the
    /// two peers' hashes for that frame would differ, and the netplay desync
    /// check would fire a spurious full resync. The second half (the mix, the
    /// waveform-visualization ring, and the host audio ring) is pure
    /// presentation (none of it is serialized), so a headless frame returns
    /// before it: audio is generated but not rendered, exactly as rollback
    /// netcode requires.
    pub(super) fn generate_and_mix_audio(&mut self, present: bool) {
        /// Soft limiter using tanh-like curve.
        /// Below threshold (0.8), signal passes through linearly.
        /// Above threshold, applies smooth compression to avoid hard clipping.
        /// Output is always in [-1.0, 1.0].
        #[inline]
        fn soft_limit(x: f32) -> f32 {
            const THRESHOLD: f32 = 0.8;
            if x.abs() <= THRESHOLD {
                x
            } else {
                let sign = x.signum();
                let excess = x.abs() - THRESHOLD;
                let compressed = THRESHOLD + (1.0 - THRESHOLD) * (1.0 - (-excess * 3.0).exp());
                sign * compressed.min(1.0)
            }
        }

        if !self.audio_enabled {
            return;
        }

        // Drift-free cycles->samples mapping. The converter is anchored
        // on `master_cycles`, so the delta between the previous
        // anchor's cumulative count and this frame's tells us exactly
        // how many stereo pairs to emit, bit-for-bit identical to a
        // single big-int compute done at the end of the session.
        let cumulative = self
            .audio_rate_converter
            .cycles_to_samples(self.master_cycles);
        let prev = self.audio_rate_converter.samples_at_reference();
        let samples_per_frame = cumulative.saturating_sub(prev) as usize;
        // Re-anchor immediately. `cumulative` already includes the
        // fractional remainder; carrying it forward inside `advance`
        // keeps the next frame's computation drift-free.
        self.audio_rate_converter
            .advance(self.master_cycles, cumulative);

        // Edge case: a paused / stepped emulator may report 0 delta
        // (no cycle advance since last call). Skip the heavy mix path
        // entirely: pushing an empty slice would be harmless but
        // wastes the chip generation work below.
        if samples_per_frame == 0 {
            return;
        }

        // Ensure pre-allocated buffers are large enough (resize only if needed)
        if self.psg_sample_buf.len() < samples_per_frame {
            self.psg_sample_buf.resize(samples_per_frame, 0.0);
        }
        if self.ym_sample_buf.len() < samples_per_frame * 2 {
            self.ym_sample_buf.resize(samples_per_frame * 2, 0.0);
        }
        if self.mix_sample_buf.len() < samples_per_frame * 2 {
            self.mix_sample_buf.resize(samples_per_frame * 2, 0.0);
        }

        // Zero the used portion of buffers
        for v in &mut self.psg_sample_buf[..samples_per_frame] {
            *v = 0.0;
        }
        for v in &mut self.ym_sample_buf[..samples_per_frame * 2] {
            *v = 0.0;
        }

        // Generate samples from PSG and YM2612.
        // Both buffers were pre-zeroed so any samples beyond what each chip
        // produces are already silence (0.0).
        use retro_core::audio::AudioChip;
        let _psg_count = self.bus.psg_mut().generate_samples(
            &mut self.psg_sample_buf[..samples_per_frame],
            self.audio_sample_rate,
        );

        let ym_count = self.bus.ym2612_mut().generate_samples(
            &mut self.ym_sample_buf[..samples_per_frame * 2],
            self.audio_sample_rate,
        );
        let _ym_stereo_count = ym_count / 2;

        // Everything above advanced serialized machine state (the converter and
        // each chip's `clock_accumulator`), so it ran on this frame regardless of
        // presentation. A headless frame stops here: the mix, the waveform ring,
        // and the host audio ring below are presentation-only and never enter the
        // netplay desync hash. See the fn doc for why this ordering is
        // load-bearing for rollback determinism.
        if !present {
            return;
        }

        // Mix PSG + YM2612 (stereo interleaved: L, R, L, R, ...)
        // Always output exactly samples_per_frame to keep the audio buffer fill
        // rate matched to the video frame rate. Both sample buffers are pre-zeroed,
        // so if either chip produced slightly fewer samples, the tail is silence.
        let mix_count = samples_per_frame;

        for i in 0..mix_count {
            // PSG is mono - duplicate to L and R
            let psg = self.psg_sample_buf[i];

            // YM2612 is stereo interleaved
            let ym_left = self.ym_sample_buf[i * 2];
            let ym_right = self.ym_sample_buf[i * 2 + 1];

            // Mix: PSG (mono->both) + YM2612 (stereo)
            // PSG/FM balance: the PSG at full scale is about a third of the
            // FM at full scale (0.34, the ratio Genesis Plus GX's default
            // levels give). 0.25/0.75 gives 0.333, with max output = 1.0.
            let mixed_left = soft_limit(psg * 0.25 + ym_left * 0.75);
            let mixed_right = soft_limit(psg * 0.25 + ym_right * 0.75);

            // The stereo-interleaved mixed output, staged for the ring.
            self.mix_sample_buf[i * 2] = mixed_left;
            self.mix_sample_buf[i * 2 + 1] = mixed_right;

            // Store mono mix for waveform visualization
            let mono = (mixed_left + mixed_right) * 0.5;
            if self.mixed_waveform_buffer.len() >= 2048 {
                self.mixed_waveform_buffer.pop_front();
            }
            self.mixed_waveform_buffer.push_back(mono);
        }

        // The ring is bounded; if the host is way behind (debugger
        // paused, tab backgrounded) `push_slice` quietly drops the
        // oldest samples and bumps `overflow_count`. Callers diagnose
        // backpressure through `audio_ring_overflows()`.
        self.audio_ring
            .push_slice(&self.mix_sample_buf[..mix_count * 2]);
    }
}
