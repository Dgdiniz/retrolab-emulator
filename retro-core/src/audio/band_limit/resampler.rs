//! A chip's own-rate samples at the host's rate, by band-limited
//! interpolation.

use std::sync::Arc;

use super::kernel::SincKernel;
use crate::audio::rate_converter::gcd;

/// Interleaved stereo samples at one rate made into samples at a higher
/// one: each output sample is the band-limited signal the input samples
/// stand for, read at its time (Smith and Gossett's band-limited
/// interpolation, the kernel laid on the input's rate, its taps gathered
/// at each output sample), on the exact ratio of the two rates, so the
/// output's count never drifts from it.
///
/// Fed in chunks of any size, it gives the samples it gives fed at once:
/// it carries the last `taps` input samples and its place between them.
/// An output sample stands for the input's signal up to `half_width`
/// input samples before the input sample whose coming completed it, so the
/// first ones stand for the silence carried at the start. The output keeps
/// the input's full scale: a value the interpolation puts past it (a fast
/// swing near full scale overshoots between samples) is held at it, as a
/// 16-bit output would.
#[derive(Debug, Clone)]
pub struct SincResampler {
    kernel: Arc<SincKernel>,
    /// The rates, in lowest terms.
    input_rate: u64,
    output_rate: u64,
    /// The last `taps` input samples, each written twice, `taps` apart, so
    /// they always lie in order from `head` on.
    history: Vec<[f64; 2]>,
    head: usize,
    /// The next output sample's place past the input sample `half_width`
    /// before the last one taken: whole input samples, and the remainder
    /// over `output_rate`.
    whole: u64,
    remainder: u64,
}

impl SincResampler {
    /// From `input_rate` to `output_rate` (Hz, the input's the lower),
    /// interpolated by `kernel`, laid on the input's rate.
    pub fn new(kernel: Arc<SincKernel>, input_rate: u32, output_rate: u32) -> Self {
        assert!(input_rate > 0, "input rate must be > 0");
        assert!(output_rate > 0, "output rate must be > 0");
        assert!(
            input_rate <= output_rate,
            "a resampler up to a higher rate, not from {input_rate} down to {output_rate}"
        );
        let g = gcd(u64::from(input_rate), u64::from(output_rate));
        let taps = kernel.taps();
        Self {
            kernel,
            input_rate: u64::from(input_rate) / g,
            output_rate: u64::from(output_rate) / g,
            history: vec![[0.0; 2]; 2 * taps],
            head: 0,
            whole: 0,
            remainder: 0,
        }
    }

    /// Resample `input` (interleaved `i16` left and right) and append the
    /// result (interleaved `f32`, full scale 1) to `out`.
    pub fn resample_into(&mut self, input: &[i16], out: &mut Vec<f32>) {
        let taps = self.kernel.taps();
        for frame in input.as_chunks::<2>().0 {
            let sample = [f64::from(frame[0]), f64::from(frame[1])];
            self.history[self.head] = sample;
            self.history[self.head + taps] = sample;
            self.head = (self.head + 1) % taps;
            // Every output sample whose place lies between the middle two
            // of the samples carried: the kernel is even, so its row for a
            // place `fraction` past input sample `i`, read as weights,
            // gathers samples `i - (half_width - 1)` to `i + half_width`.
            while self.whole == 0 {
                let fraction = ((self.remainder << 32) / self.output_rate) as u32;
                let (a, b, blend) = self.kernel.rows_at(fraction);
                let window = &self.history[self.head..self.head + taps];
                let (mut left, mut right) = (0.0, 0.0);
                for ((sample, &a), &b) in window.iter().zip(a).zip(b) {
                    let weight = a + (b - a) * blend;
                    left += sample[0] * weight;
                    right += sample[1] * weight;
                }
                out.push((left / 32768.0).clamp(-1.0, 1.0) as f32);
                out.push((right / 32768.0).clamp(-1.0, 1.0) as f32);
                self.remainder += self.input_rate;
                self.whole += self.remainder / self.output_rate;
                self.remainder %= self.output_rate;
            }
            self.whole -= 1;
        }
    }

    /// Silence carried, the place at its start: the next chunk starts from
    /// a clean slate.
    pub fn reset(&mut self) {
        self.history.iter_mut().for_each(|s| *s = [0.0; 2]);
        self.head = 0;
        self.whole = 0;
        self.remainder = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn kernel() -> Arc<SincKernel> {
        Arc::new(SincKernel::new(32, 1.0, 80.0, 128))
    }

    /// Every input sample gives the output samples whose places it
    /// completes: `n` input samples, `ceil(n * 441 / 320)` of 44.1 kHz from
    /// 32 kHz, whatever the chunks.
    #[test]
    fn the_count_is_the_ratio_s_exactly() {
        let mut rs = SincResampler::new(kernel(), 32_000, 44_100);
        let mut out = Vec::new();
        let mut fed = 0u64;
        for chunk in [1usize, 2, 533, 534, 7, 1000, 31_999]
            .iter()
            .cycle()
            .take(40)
        {
            rs.resample_into(&vec![0i16; 2 * chunk], &mut out);
            fed += *chunk as u64;
            assert_eq!(
                out.len() as u64 / 2,
                (fed * 441).div_ceil(320),
                "after {fed}"
            );
        }
    }

    #[test]
    fn a_constant_passes_exactly() {
        let mut rs = SincResampler::new(kernel(), 32_000, 44_100);
        let mut out = Vec::new();
        rs.resample_into(&vec![16_384i16; 2 * 4000], &mut out);
        for &s in &out[2 * 200..] {
            assert_eq!(s, 0.5);
        }
    }

    /// A tone in the passband comes out as the tone read at the output
    /// samples' times, `half_width` input samples late: the error is the
    /// input's 16-bit rounding and the kernel's ripple, near -90 dB.
    #[test]
    fn a_tone_is_read_at_the_output_s_times() {
        let (input_rate, output_rate, f0, amplitude) = (32_000.0, 44_100.0, 3_000.0, 12_000.0);
        let input: Vec<i16> = (0..16_000)
            .flat_map(|i| {
                let v = (amplitude * (2.0 * PI * f0 * f64::from(i) / input_rate).sin()).round();
                [v as i16, -(v as i16)]
            })
            .collect();
        let mut rs = SincResampler::new(kernel(), 32_000, 44_100);
        let mut out = Vec::new();
        rs.resample_into(&input, &mut out);
        let (mut signal, mut error) = (0.0, 0.0);
        for n in 1000..out.len() / 2 - 1000 {
            let place = n as f64 * input_rate / output_rate - 32.0;
            let ideal = amplitude / 32768.0 * (2.0 * PI * f0 * place / input_rate).sin();
            let got = f64::from(out[2 * n]);
            signal += ideal * ideal;
            error += (got - ideal) * (got - ideal);
            assert!(
                (f64::from(out[2 * n + 1]) + got).abs() < 1e-6,
                "the right side"
            );
        }
        let db = 10.0 * (error / signal).log10();
        assert!(db < -80.0, "{db} dB of error");
    }

    #[test]
    fn chunks_give_the_samples_one_call_gives() {
        let input: Vec<i16> = (0..9000).map(|i| ((i * 37) % 4001 - 2000) as i16).collect();
        let mut once = SincResampler::new(kernel(), 32_000, 44_100);
        let mut expected = Vec::new();
        once.resample_into(&input, &mut expected);
        let mut pieces = SincResampler::new(kernel(), 32_000, 44_100);
        let mut out = Vec::new();
        let mut from = 0;
        for length in [1usize, 3, 64, 2, 900, 7].iter().cycle() {
            let to = (from + 2 * length).min(input.len());
            pieces.resample_into(&input[from..to], &mut out);
            from = to;
            if from == input.len() {
                break;
            }
        }
        assert_eq!(out, expected);
    }

    /// A full-scale square overshoots between its samples; the output
    /// stops at full scale.
    #[test]
    fn the_output_keeps_the_input_s_full_scale() {
        let input: Vec<i16> = (0..4000)
            .flat_map(|i| {
                let v = if (i / 5) % 2 == 0 { i16::MAX } else { i16::MIN };
                [v, v]
            })
            .collect();
        let mut rs = SincResampler::new(kernel(), 32_000, 44_100);
        let mut out = Vec::new();
        rs.resample_into(&input, &mut out);
        assert!(out.iter().all(|&s| (-1.0..=1.0).contains(&s)));
        assert!(out.contains(&1.0), "the overshoot held at 1");
    }

    #[test]
    fn a_reset_starts_from_silence() {
        let mut rs = SincResampler::new(kernel(), 32_000, 44_100);
        let mut out = Vec::new();
        rs.resample_into(&vec![9_000i16; 2 * 500], &mut out);
        rs.reset();
        out.clear();
        rs.resample_into(&[9_000, 9_000], &mut out);
        assert_eq!(out.len(), 4);
        assert_eq!(out[..2], [0.0; 2], "a whole sample's place, in the silence");
        assert!(out[2..].iter().all(|s| s.abs() < 1e-4), "{out:?}");
    }
}
