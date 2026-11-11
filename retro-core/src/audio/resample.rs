//! Drift-free linear sample-rate conversion (interleaved stereo).
//!
//! Converts a stream of interleaved `i16` stereo frames at one rate into
//! interleaved `f32` frames at another (e.g. the SNES S-DSP's 32 kHz
//! output into a 44.1 kHz host buffer). Interpolation is linear: each
//! output frame is a blend of the two nearest input frames.
//!
//! The resampler is *stateful* and designed to be fed in arbitrary
//! chunks. It carries a fixed-point fractional phase and the previous
//! input frame (`prev_l`/`prev_r`) across calls, so:
//!
//! - successive calls produce no discontinuity at the chunk seam
//!   (the interpolation that straddles two chunks uses the last frame
//!   of the earlier chunk), and
//! - the long-run output/input frame ratio converges to exactly
//!   `output_rate / input_rate` with no cumulative drift, the carried
//!   phase is the only source of per-call rounding and it is bounded by
//!   a single output frame regardless of how many calls have happened.
//!
//! Linear interpolation is deliberately simple; a windowed-sinc /
//! polyphase upgrade is left for a future revision.

/// Fixed-point fractional bits for the phase accumulator.
const FRAC_BITS: u32 = 32;
/// One whole input frame in fixed-point phase units (`2^FRAC_BITS`).
const ONE: u64 = 1 << FRAC_BITS;

/// Stateful linear resampler for interleaved stereo audio.
#[derive(Debug, Clone)]
pub struct LinearResampler {
    input_rate: u32,
    output_rate: u32,
    /// Input-position advance per emitted output frame, fixed-point.
    /// Equals `ONE * input_rate / output_rate`.
    step: u64,
    /// Fractional position between `prev` and the current input frame,
    /// in `[0, ONE)` at every input-frame boundary.
    phase: u64,
    /// Previous input frame, used as the left anchor of the interpolation
    /// for output frames that fall before the current input frame.
    prev_l: i16,
    prev_r: i16,
}

impl LinearResampler {
    /// Build a resampler from `input_rate` to `output_rate` (both Hz,
    /// both must be positive). The ratio is region-independent for a
    /// given console, so the same instance is reused for the life of the
    /// emulator and only its state is `reset()`.
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        assert!(input_rate > 0, "input rate must be > 0");
        assert!(output_rate > 0, "output rate must be > 0");
        let step = ((input_rate as u64) << FRAC_BITS) / output_rate as u64;
        Self {
            input_rate,
            output_rate,
            step,
            phase: 0,
            prev_l: 0,
            prev_r: 0,
        }
    }

    /// Input sample rate in Hz.
    pub fn input_rate(&self) -> u32 {
        self.input_rate
    }

    /// Output sample rate in Hz.
    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }

    /// Clear the carried phase and previous-frame anchors. Call on
    /// console reset so the next chunk starts from a clean slate.
    pub fn reset(&mut self) {
        self.phase = 0;
        self.prev_l = 0;
        self.prev_r = 0;
    }

    /// Resample `input` (interleaved `i16` L/R, `input_rate` Hz) and
    /// **append** the interleaved `f32` result (`output_rate` Hz, range
    /// `[-1.0, 1.0]`) to `out`. The fractional phase and previous frame
    /// persist across calls, so feeding the same stream in one chunk or
    /// many produces the same (seam-free) output.
    pub fn resample_into(&mut self, input: &[i16], out: &mut Vec<f32>) {
        let frames = input.len() / 2;
        for j in 0..frames {
            let cur_l = input[2 * j];
            let cur_r = input[2 * j + 1];

            // Emit every output frame whose position falls within the
            // [prev, cur] interval (one input-frame period of time).
            while self.phase < ONE {
                let frac = self.phase as f32 / ONE as f32;
                let pl = self.prev_l as f32;
                let pr = self.prev_r as f32;
                let l = pl + (cur_l as f32 - pl) * frac;
                let r = pr + (cur_r as f32 - pr) * frac;
                out.push(l / 32768.0);
                out.push(r / 32768.0);
                self.phase += self.step;
            }

            // Consume one input frame of phase budget. The loop above
            // always exits with `phase >= ONE`, so this never underflows.
            self.phase -= ONE;
            self.prev_l = cur_l;
            self.prev_r = cur_r;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: count of stereo frames in an interleaved buffer.
    fn frames(buf: &[f32]) -> usize {
        buf.len() / 2
    }

    #[test]
    fn new_rejects_zero_rates() {
        let r = std::panic::catch_unwind(|| LinearResampler::new(0, 44_100));
        assert!(r.is_err());
        let r = std::panic::catch_unwind(|| LinearResampler::new(32_000, 0));
        assert!(r.is_err());
    }

    #[test]
    fn silence_in_silence_out() {
        let mut rs = LinearResampler::new(32_000, 44_100);
        let input = vec![0i16; 32_000 * 2 / 10]; // ~0.1 s of stereo silence
        let mut out = Vec::new();
        rs.resample_into(&input, &mut out);
        assert!(!out.is_empty(), "should emit output frames");
        assert!(out.iter().all(|&s| s == 0.0), "silence stays silent");
    }

    #[test]
    fn dc_level_is_preserved() {
        let mut rs = LinearResampler::new(32_000, 44_100);
        // Constant input: a steady DC level on both channels.
        let v: i16 = 16_384; // 0.5 of full scale
        let input = vec![v; 200 * 2];
        let mut out = Vec::new();
        rs.resample_into(&input, &mut out);

        // After the first input frame the previous-frame anchor equals
        // `v`, so the steady-state output is exactly `v / 32768`.
        let expected = v as f32 / 32768.0;
        let tail = &out[out.len() - 20..];
        for &s in tail {
            assert!((s - expected).abs() < 1e-6, "DC not preserved: {s}");
        }
    }

    #[test]
    fn output_stays_in_unit_range() {
        let mut rs = LinearResampler::new(32_000, 44_100);
        // Alternating extremes exercise the interpolation bounds.
        let mut input = Vec::new();
        for i in 0..1000 {
            let s = if i % 2 == 0 { i16::MIN } else { i16::MAX };
            input.push(s);
            input.push(s);
        }
        let mut out = Vec::new();
        rs.resample_into(&input, &mut out);
        for &s in &out {
            assert!((-1.0..=1.0).contains(&s), "sample {s} out of range");
        }
    }

    #[test]
    fn chunked_matches_oneshot() {
        // Phase continuity: feeding the same stream in many small chunks
        // must yield the identical output sequence as one big call.
        let mut input = Vec::new();
        for i in 0..5000i32 {
            // A slow ramp so any seam discontinuity would be visible.
            let s = ((i * 7) % 4000 - 2000) as i16;
            input.push(s);
            input.push(s.wrapping_add(13));
        }

        let mut oneshot = LinearResampler::new(32_000, 44_100);
        let mut out_one = Vec::new();
        oneshot.resample_into(&input, &mut out_one);

        let mut chunked = LinearResampler::new(32_000, 44_100);
        let mut out_chunk = Vec::new();
        // Irregular chunk sizes (in frames) to stress the carried phase.
        let chunk_frames = [1usize, 2, 3, 7, 50, 1, 200, 333];
        let mut fi = 0;
        let mut ci = 0;
        while fi < input.len() / 2 {
            let n = chunk_frames[ci % chunk_frames.len()].min(input.len() / 2 - fi);
            ci += 1;
            let slice = &input[fi * 2..(fi + n) * 2];
            chunked.resample_into(slice, &mut out_chunk);
            fi += n;
        }

        assert_eq!(
            out_one.len(),
            out_chunk.len(),
            "chunked and one-shot must emit the same number of samples"
        );
        for (a, b) in out_one.iter().zip(out_chunk.iter()) {
            assert_eq!(a, b, "chunked output diverged from one-shot (seam bug)");
        }
    }

    #[test]
    fn output_count_tracks_ratio_without_drift() {
        // Over a long run the output frame count must stay within a
        // single frame of input_frames * out_rate / in_rate, no matter
        // how the input is chunked, that is the no-drift guarantee.
        let mut rs = LinearResampler::new(32_000, 44_100);
        let mut out = Vec::new();
        let total_input_frames = 32_000usize * 5; // 5 seconds of audio
        let chunk = vec![0i16; 64 * 2]; // 64 frames per call
        let mut fed = 0;
        while fed < total_input_frames {
            rs.resample_into(&chunk, &mut out);
            fed += 64;
        }
        let expected = fed as f64 * 44_100.0 / 32_000.0;
        let got = frames(&out) as f64;
        assert!(
            (got - expected).abs() <= 2.0,
            "drift: got {got} output frames, expected ~{expected}"
        );
    }

    #[test]
    fn reset_clears_state() {
        let mut rs = LinearResampler::new(32_000, 44_100);
        let input = vec![12_345i16; 100 * 2];
        let mut out = Vec::new();
        rs.resample_into(&input, &mut out);
        rs.reset();
        // After reset the first output frame interpolates from a zero
        // anchor at phase 0, i.e. exactly silence again.
        let mut out2 = Vec::new();
        rs.resample_into(&[7000, -7000], &mut out2);
        assert_eq!(out2[0], 0.0, "first sample after reset starts from zero");
        assert_eq!(out2[1], 0.0);
    }
}
