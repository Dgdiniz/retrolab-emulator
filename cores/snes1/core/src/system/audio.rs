//! Audio: the S-DSP's samples resampled at a frame's end into the host's
//! ring.

use super::SnesEmulator;

impl SnesEmulator {
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
        // drops the oldest samples and counts them, which
        // `host_audio().overflows()` reports.
        self.host_audio.push_slice(&self.audio_resample_buf);
    }
}
