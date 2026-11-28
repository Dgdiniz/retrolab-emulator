//! A console's analog output stage: a one-pole low-pass and a one-pole
//! DC-blocking high-pass.

use std::f64::consts::PI;

use super::math::exp;

/// A first-order low-pass and a first-order DC-blocking high-pass on each
/// side of interleaved stereo samples, the pole of each analog filter's
/// corner `fc` mapped to `e^(-2 pi fc / rate)` (Oppenheim and Schafer): the
/// low-pass `y += a (x - y)` with `a = 1 - e^(-2 pi fc / rate)`, unit gain
/// at DC; the high-pass `y = b (y + x - x')` with `b = e^(-2 pi fc /
/// rate)`, its zero at DC.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputFilter {
    low_pass: Option<f64>,
    high_pass: Option<f64>,
    low: [f64; 2],
    high_in: [f64; 2],
    high_out: [f64; 2],
}

impl OutputFilter {
    /// A low-pass with its corner at `low_pass_hz` and a high-pass with its
    /// corner at `high_pass_hz`, on samples at `rate` Hz; `None` leaves that
    /// filter out.
    pub fn new(low_pass_hz: Option<f64>, high_pass_hz: Option<f64>, rate: f64) -> Self {
        Self {
            low_pass: low_pass_hz.map(|corner| 1.0 - exp(-2.0 * PI * corner / rate)),
            high_pass: high_pass_hz.map(|corner| exp(-2.0 * PI * corner / rate)),
            low: [0.0; 2],
            high_in: [0.0; 2],
            high_out: [0.0; 2],
        }
    }

    /// Filter interleaved stereo `samples` in place, the low-pass first.
    pub fn process(&mut self, samples: &mut [f32]) {
        for frame in samples.as_chunks_mut::<2>().0 {
            for (side, sample) in frame.iter_mut().enumerate() {
                let mut x = f64::from(*sample);
                if let Some(a) = self.low_pass {
                    self.low[side] += a * (x - self.low[side]);
                    x = self.low[side];
                }
                if let Some(b) = self.high_pass {
                    let y = b * (self.high_out[side] + x - self.high_in[side]);
                    self.high_in[side] = x;
                    self.high_out[side] = y;
                    x = y;
                }
                *sample = x as f32;
            }
        }
    }

    /// Both filters at rest: what follows starts from silence.
    pub fn reset(&mut self) {
        self.low = [0.0; 2];
        self.high_in = [0.0; 2];
        self.high_out = [0.0; 2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The steady-state amplitude of a sine of `frequency` through
    /// `filter`, on its left side.
    fn gain(mut filter: OutputFilter, frequency: f64, rate: f64) -> f64 {
        let length = 4 * rate as usize;
        let mut samples: Vec<f32> = (0..length)
            .flat_map(|i| {
                let v = (2.0 * PI * frequency * i as f64 / rate).sin() as f32;
                [v, 0.0]
            })
            .collect();
        filter.process(&mut samples);
        samples[length..]
            .iter()
            .step_by(2)
            .fold(0.0f64, |m, &s| m.max(f64::from(s).abs()))
    }

    /// The low-pass's gain at a frequency is its difference equation's,
    /// |a / (1 - (1 - a) e^-jw)|; a constant passes at full level.
    #[test]
    fn the_low_pass_has_its_equation_s_gain() {
        let rate = 44_100.0;
        let a = 1.0 - (-2.0 * PI * 2_840.0 / rate).exp();
        for frequency in [200.0, 2_840.0, 9_000.0] {
            let w = 2.0 * PI * frequency / rate;
            let (re, im) = (1.0 - (1.0 - a) * w.cos(), (1.0 - a) * w.sin());
            let expected = a / (re * re + im * im).sqrt();
            let got = gain(
                OutputFilter::new(Some(2_840.0), None, rate),
                frequency,
                rate,
            );
            assert!(
                (got - expected).abs() < 2e-3,
                "{frequency} Hz: {got} against {expected}"
            );
        }
        let mut constant = vec![0.5f32; 2 * 44_100];
        OutputFilter::new(Some(2_840.0), None, rate).process(&mut constant);
        assert!((constant[constant.len() - 2] - 0.5).abs() < 1e-6);
    }

    /// The high-pass's gain is b |1 - e^-jw| / |1 - b e^-jw|; a constant
    /// dies away.
    #[test]
    fn the_high_pass_has_its_equation_s_gain_and_blocks_a_constant() {
        let rate = 44_100.0;
        let b = (-2.0 * PI * 20.0 / rate).exp();
        for frequency in [20.0, 100.0, 5_000.0] {
            let w = 2.0 * PI * frequency / rate;
            let top = ((1.0 - w.cos()).powi(2) + w.sin().powi(2)).sqrt();
            let bottom = ((1.0 - b * w.cos()).powi(2) + (b * w.sin()).powi(2)).sqrt();
            let expected = b * top / bottom;
            let got = gain(OutputFilter::new(None, Some(20.0), rate), frequency, rate);
            assert!(
                (got - expected).abs() < 2e-3,
                "{frequency} Hz: {got} against {expected}"
            );
        }
        let mut constant = vec![0.5f32; 2 * 44_100];
        OutputFilter::new(None, Some(20.0), rate).process(&mut constant);
        assert!(constant[constant.len() - 2].abs() < 1e-4);
    }

    #[test]
    fn a_reset_forgets_the_filters_state() {
        let mut filter = OutputFilter::new(Some(2_840.0), Some(20.0), 44_100.0);
        let fresh = filter.clone();
        let mut samples = vec![0.7f32; 200];
        filter.process(&mut samples);
        filter.reset();
        assert_eq!(filter, fresh);
    }
}
