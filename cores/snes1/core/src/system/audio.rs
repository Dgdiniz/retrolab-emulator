//! Audio: the S-DSP's samples resampled at a frame's end into the host's
//! ring.

#[cfg(doc)]
use retro_core::EmulatorCore;

use super::SnesEmulator;

impl SnesEmulator {
    /// Times the host fell behind and the host audio ring dropped its
    /// oldest samples. Mirrors SG1's `audio_ring_overflows()` WASM export
    /// for the audio-backpressure telemetry overlay and the no-drift test.
    pub fn audio_overflows(&self) -> u64 {
        self.audio_ring.overflow_count()
    }

    /// Times the host pulled more samples than were queued (the ring ran
    /// dry). Mirrors SG1's `audio_ring_underruns()`.
    pub fn audio_underruns(&self) -> u64 {
        self.audio_ring.underrun_count()
    }

    /// Drop audio that has been generated but not yet handed to the host.
    ///
    /// Called after a rollback re-simulation: those frames' samples sit in
    /// the APU's own ring (a headless frame runs the DSP but skips the drain)
    /// and describe audio the player already heard in its mispredicted form.
    /// The host ring is deliberately left alone, it holds audio that *was*
    /// presented and is still on its way to the speaker.
    pub fn discard_pending_audio(&mut self) {
        self.bus.apu_mut().discard_pending_audio();
    }

    /// Interleaved L/R `f32` samples currently queued in the host audio
    /// ring (a value of `1470` means 735 stereo pairs ready). Lets a host
    /// worklet right-size a [`Self::drain_audio_into`] pull. Mirrors SG1's
    /// `audio_available()`.
    pub fn audio_available(&self) -> usize {
        self.audio_ring.available()
    }

    /// Drain up to `dst.len()` queued host samples into `dst`, returning
    /// how many were written. Destructive: each sample is yielded once.
    /// Mirrors SG1's `drain_audio_into()` so the WASM `getAudioSamplesN`
    /// export pulls a bounded, host-controlled window without over-draining
    /// the ring (unlike [`EmulatorCore::get_audio_samples`], which empties
    /// it). Size `dst` to `audio_available()` to avoid spurious underruns.
    pub fn drain_audio_into(&mut self, dst: &mut [f32]) -> usize {
        self.audio_ring.drain_into(dst)
    }

    /// Pull every queued 32 kHz `i16` sample out of the APU, resample it
    /// to the 44.1 kHz host rate, and push the `f32` result into the host
    /// audio ring. Called once per frame after the APU catch-up.
    ///
    /// Skipped entirely when audio is disabled; the APU's own 32 kHz ring
    /// then fills and bounds-overflows its oldest samples (benign), and
    /// resampling resumes from the live ring once audio is re-enabled.
    pub(super) fn drain_apu_to_host_audio(&mut self) {
        if !self.audio_enabled {
            return;
        }
        let available = self.bus.apu_mut().audio_samples_available();
        if available == 0 {
            return;
        }
        self.apu_drain_buf.resize(available, 0);
        let drained = self.bus.apu_mut().drain_audio(&mut self.apu_drain_buf);
        self.audio_resample_buf.clear();
        self.resampler
            .resample_into(&self.apu_drain_buf[..drained], &mut self.audio_resample_buf);
        // The host ring is bounded; if the host is far behind, `push_slice`
        // drops the oldest samples and bumps `overflow_count`, surfaced via
        // `audio_overflows()`.
        self.audio_ring.push_slice(&self.audio_resample_buf);
    }
}
