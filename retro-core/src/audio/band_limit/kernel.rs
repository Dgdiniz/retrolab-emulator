//! The kernel both forms of the band-limited output share: a
//! Kaiser-windowed sinc, tabulated at fractions of a sample.

use std::f64::consts::PI;

use super::math::{bessel_i0, kaiser_beta, sin_pi};

/// A Kaiser-windowed sinc low-pass laid on a rate's samples, tabulated at
/// `phases + 1` fractions of a sample and interpolated linearly between
/// them (Smith and Gossett's band-limited interpolation; the window's shape
/// and its beta from Kaiser's formulas).
///
/// Row `p` holds the `2 * half_width` taps of an impulse `p / phases` of a
/// sample past a whole sample `i`: tap `k` lands on sample `i + 1 + k`, so
/// the impulse's centre falls `half_width` samples after its time, a delay
/// that puts every tap on a later sample. Each row is normalised to sum to
/// 1, so a constant passes exactly.
#[derive(Debug, Clone, PartialEq)]
pub struct SincKernel {
    half_width: usize,
    phases: usize,
    rows: Vec<f64>,
}

impl SincKernel {
    /// A kernel spanning `half_width` samples each side of its centre, its
    /// cutoff at `cutoff` times the Nyquist frequency of the rate it is laid
    /// on (above 0, at most 1), its stopband `attenuation` dB down (over 50),
    /// tabulated at `phases` fractions of a sample.
    pub fn new(half_width: usize, cutoff: f64, attenuation: f64, phases: usize) -> Self {
        assert!(half_width > 0, "a kernel spans at least a sample each side");
        assert!(phases > 0, "a kernel holds at least one fraction");
        assert!(
            cutoff > 0.0 && cutoff <= 1.0,
            "a cutoff above 0 and at most the Nyquist frequency, not {cutoff}"
        );
        let beta = kaiser_beta(attenuation);
        let taps = 2 * half_width;
        let window_peak = bessel_i0(beta);
        let span = half_width as f64;
        let mut rows = vec![0.0; (phases + 1) * taps];
        for (p, row) in rows.chunks_exact_mut(taps).enumerate() {
            let fraction = p as f64 / phases as f64;
            for (k, tap) in row.iter_mut().enumerate() {
                // The tap's distance from the impulse's centre, in samples.
                let t = k as f64 - (span - 1.0) - fraction;
                let edge = t / span;
                let window = if edge.abs() >= 1.0 {
                    0.0
                } else {
                    bessel_i0(beta * (1.0 - edge * edge).sqrt()) / window_peak
                };
                let x = cutoff * t;
                let sinc = if x == 0.0 { 1.0 } else { sin_pi(x) / (PI * x) };
                *tap = cutoff * sinc * window;
            }
            let sum: f64 = row.iter().sum();
            for tap in row.iter_mut() {
                *tap /= sum;
            }
        }
        Self {
            half_width,
            phases,
            rows,
        }
    }

    /// The samples the kernel spans each side of its centre.
    pub fn half_width(&self) -> usize {
        self.half_width
    }

    /// The samples an impulse touches: twice the half width.
    pub fn taps(&self) -> usize {
        2 * self.half_width
    }

    /// The two tabulated rows nearest to an impulse `fraction` (in
    /// 1/2^32ths) of a sample past a whole sample, and how far towards the
    /// second the fraction lies (0 to 1): its taps are `a[k] + (b[k] -
    /// a[k]) * blend`.
    #[inline]
    pub fn rows_at(&self, fraction: u32) -> (&[f64], &[f64], f64) {
        let taps = self.taps();
        let scaled = u64::from(fraction) * self.phases as u64;
        let row = (scaled >> 32) as usize;
        let blend = (scaled & 0xFFFF_FFFF) as f64 / 4_294_967_296.0;
        (
            &self.rows[row * taps..(row + 1) * taps],
            &self.rows[(row + 1) * taps..(row + 2) * taps],
            blend,
        )
    }

    /// The taps of an impulse `fraction` (in 1/2^32ths) of a sample past a
    /// whole sample, written into `out` (at least [`Self::taps`] long).
    #[inline]
    pub fn taps_at(&self, fraction: u32, out: &mut [f64]) {
        let (a, b, blend) = self.rows_at(fraction);
        for ((tap, &a), &b) in out.iter_mut().zip(a).zip(b) {
            *tap = a + (b - a) * blend;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kernel() -> SincKernel {
        SincKernel::new(16, 0.9, 60.0, 64)
    }

    #[test]
    fn every_fraction_passes_a_constant_exactly() {
        let kernel = kernel();
        let mut taps = vec![0.0; kernel.taps()];
        for fraction in [0u32, 1, 1 << 31, 3 << 30, u32::MAX] {
            kernel.taps_at(fraction, &mut taps);
            let sum: f64 = taps.iter().sum();
            assert!((sum - 1.0).abs() < 1e-14, "fraction {fraction}: {sum}");
        }
    }

    /// The kernel is even about its centre, so the impulse a fraction `f`
    /// past a sample is the one `1 - f` past it, reversed.
    #[test]
    fn a_fraction_and_its_complement_are_mirrors() {
        let kernel = kernel();
        let (mut left, mut right) = (vec![0.0; 32], vec![0.0; 32]);
        for p in 1..64u64 {
            kernel.taps_at(((p << 32) / 64) as u32, &mut left);
            kernel.taps_at((((64 - p) << 32) / 64) as u32, &mut right);
            for k in 0..32 {
                assert!((left[k] - right[31 - k]).abs() < 1e-15, "{p}/64, tap {k}");
            }
        }
    }

    /// The tables of the two kernels the cores' sound is made with, to the
    /// bit: a change to the series or to the tables moves every sample
    /// they make, and shows here first.
    #[test]
    fn the_tables_the_cores_use_are_as_recorded() {
        for (cutoff, recorded) in [
            (0.92, 0x3b0d_d5ba_40e1_01e0u64),
            (1.0, 0x3629_c234_7af3_1cd4),
        ] {
            let kernel = SincKernel::new(32, cutoff, 80.0, 128);
            let bytes: Vec<u8> = kernel
                .rows
                .iter()
                .flat_map(|tap| tap.to_bits().to_le_bytes())
                .collect();
            assert_eq!(crate::hash::fnv1a64(&bytes), recorded, "cutoff {cutoff}");
        }
    }

    /// At a cutoff of the full Nyquist frequency and a whole sample, every
    /// tap but the centre's lands on a zero of the sinc: the kernel passes
    /// a sample through as it is.
    #[test]
    fn at_the_nyquist_cutoff_a_whole_sample_passes_as_it_is() {
        let kernel = SincKernel::new(16, 1.0, 80.0, 128);
        let mut taps = vec![0.0; 32];
        kernel.taps_at(0, &mut taps);
        for (k, &tap) in taps.iter().enumerate() {
            let expected = if k == 15 { 1.0 } else { 0.0 };
            assert!((tap - expected).abs() < 1e-15, "tap {k}: {tap}");
        }
    }

    /// The impulse's spectrum, every tabulated fraction taken together (the
    /// kernel sampled at `1 / phases` of a sample): flat to the passband's
    /// edge, and from the stopband's edge on, past the rate's Nyquist
    /// frequency where folding would come from, under the attenuation asked
    /// for; the transition is Kaiser's, (A - 8) / (2.285 (taps - 1))
    /// radians a sample wide, centred on the cutoff.
    #[test]
    fn the_spectrum_holds_its_passband_and_its_stopband() {
        let (half_width, cutoff, attenuation, phases) = (32, 0.92, 80.0, 128);
        let kernel = SincKernel::new(half_width, cutoff, attenuation, phases);
        let taps = kernel.taps();
        // (time from the centre in samples, tap) for every row but the last,
        // which repeats the first one a sample on.
        let mut dense = Vec::with_capacity(taps * phases);
        for p in 0..phases {
            let row = &kernel.rows[p * taps..(p + 1) * taps];
            for (k, &tap) in row.iter().enumerate() {
                let t = k as f64 - (half_width as f64 - 1.0) - p as f64 / phases as f64;
                dense.push((t, tap / phases as f64));
            }
        }
        // `nyquists` in Nyquist frequencies of the rate the kernel is laid on.
        let gain = |nyquists: f64| {
            let w = PI * nyquists;
            let (re, im) = dense.iter().fold((0.0, 0.0), |(re, im), &(t, tap)| {
                (re + tap * (w * t).cos(), im - tap * (w * t).sin())
            });
            (re * re + im * im).sqrt()
        };
        let transition = (attenuation - 8.0) / (2.285 * (taps - 1) as f64) / PI;
        let ripple = 10f64.powf(-attenuation / 20.0);
        for i in 0..=60 {
            let f = (cutoff - transition / 2.0) * f64::from(i) / 60.0;
            assert!((gain(f) - 1.0).abs() < 4.0 * ripple, "passband at {f}");
        }
        for i in 0..=120 {
            let f = cutoff + transition / 2.0 + 5.0 * f64::from(i) / 120.0;
            let db = 20.0 * gain(f).log10();
            assert!(db < -(attenuation - 6.0), "stopband at {f}: {db} dB");
        }
    }
}
