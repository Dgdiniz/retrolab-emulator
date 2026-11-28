//! Band-limited steps: a level that changes at exact times, as a rate's
//! samples.

use std::sync::Arc;

use super::kernel::SincKernel;
use crate::audio::rate_converter::SampleTime;

/// A signal of `N` channels made of levels set at exact times, rendered on
/// a rate's samples as a band-limited signal: each change is a step whose
/// derivative is the kernel's band-limited impulse (the integrated impulse
/// train of Stilson and Smith), kept in Brandt's form: the level held
/// exactly from the first sample at or after the impulse's centre, and the
/// band-limited step's difference from that held step added on the samples
/// around the change.
///
/// A change at a time on the samples not yet rendered touches the
/// [`Self::span`] samples after it, is heard half-way `half_width - 1/2`
/// samples after its time, and then holds its level exactly. So what is
/// rendered depends on the levels and on the changes of the last span
/// alone: two of these that were given different pasts give the same
/// samples a span after a change that set them to the same level, and a
/// run rendered in pieces of any lengths gives the samples of the run
/// rendered at once. A change's samples are kept until they are rendered,
/// so a change far ahead of the samples rendered so far keeps every sample
/// up to it: render the samples before it first.
#[derive(Debug, Clone)]
pub struct BandLimitedSteps<const N: usize> {
    kernel: Arc<SincKernel>,
    /// The level last set.
    level: [f64; N],
    /// The level held at the next sample to render.
    held: [f64; N],
    /// From the next sample to render on: where the held level changes,
    /// and to what.
    switches: Vec<Option<[f64; N]>>,
    /// From the next sample to render on: the band-limited steps'
    /// difference from the held ones.
    residual: Vec<[f64; N]>,
}

impl<const N: usize> BandLimitedSteps<N> {
    /// Silence, its steps band-limited by `kernel`, laid on the rate the
    /// samples are rendered at.
    pub fn new(kernel: Arc<SincKernel>) -> Self {
        Self {
            kernel,
            level: [0.0; N],
            held: [0.0; N],
            switches: Vec::new(),
            residual: Vec::new(),
        }
    }

    /// The samples a change touches, from the one after its time: the
    /// kernel's taps.
    pub fn span(&self) -> usize {
        self.kernel.taps()
    }

    /// The level last set.
    pub fn level(&self) -> [f64; N] {
        self.level
    }

    /// The level becomes `level` at `at`, a time on the samples not yet
    /// rendered (`at.sample` -1 for the time before the first of them).
    /// Times are set in order, each at or after the one before; a change
    /// at a time already rendered is heard from the first sample not
    /// rendered, the part of its step before it left out.
    pub fn set_level(&mut self, at: SampleTime, level: [f64; N]) {
        let delta: [f64; N] = std::array::from_fn(|c| level[c] - self.level[c]);
        if delta.iter().all(|&d| d == 0.0) {
            return;
        }
        self.level = level;
        let taps = self.kernel.taps();
        // The held level changes at the first tap at or after the impulse's
        // centre, which lies `half_width - 1` taps in, plus the fraction.
        let held_from = if at.fraction == 0 {
            self.kernel.half_width() - 1
        } else {
            self.kernel.half_width()
        };
        let first = at.sample + 1;
        let skipped = first.min(0).unsigned_abs() as usize;
        let start = first.max(0) as usize;
        let switch = (first + held_from as i64).max(0) as usize;
        self.reach((start + taps - skipped.min(taps)).max(switch + 1));
        let (a, b, blend) = self.kernel.rows_at(at.fraction);
        // The band-limited step at each tap is the sum of the impulse's taps
        // up to it; past `held_from` the held step stands at 1.
        let mut step = 0.0;
        let mut slot = start;
        for k in 0..taps {
            step += a[k] + (b[k] - a[k]) * blend;
            if k < skipped {
                continue;
            }
            let difference = if k >= held_from { step - 1.0 } else { step };
            let residual = &mut self.residual[slot];
            for c in 0..N {
                residual[c] += delta[c] * difference;
            }
            slot += 1;
        }
        self.switches[switch] = Some(level);
    }

    /// The next `frames` samples, `N` interleaved values each, into `out`;
    /// what the changes left for later samples is kept for them.
    pub fn render(&mut self, frames: usize, out: &mut [f32]) {
        assert!(
            out.len() >= N * frames,
            "room for {frames} samples of {N} channels"
        );
        let buffered = self.residual.len().min(frames);
        for i in 0..buffered {
            if let Some(level) = self.switches[i].take() {
                self.held = level;
            }
            let residual = std::mem::replace(&mut self.residual[i], [0.0; N]);
            for c in 0..N {
                out[N * i + c] = (self.held[c] + residual[c]) as f32;
            }
        }
        for i in buffered..frames {
            for c in 0..N {
                out[N * i + c] = self.held[c] as f32;
            }
        }
        if buffered == self.residual.len() {
            self.residual.clear();
            self.switches.clear();
        } else {
            // The samples rendered were emptied: they go to the end.
            self.residual.rotate_left(buffered);
            self.switches.rotate_left(buffered);
        }
    }

    /// Silence from the next sample on, nothing of the changes kept.
    pub fn clear(&mut self) {
        self.level = [0.0; N];
        self.held = [0.0; N];
        self.switches.clear();
        self.residual.clear();
    }

    /// The buffers reach `samples` samples from the next to render.
    fn reach(&mut self, samples: usize) {
        if self.residual.len() < samples {
            self.residual.resize(samples, [0.0; N]);
            self.switches.resize(samples, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::spectrum::{blackman_harris, power_spectrum};
    use super::*;

    fn full() -> Arc<SincKernel> {
        Arc::new(SincKernel::new(32, 0.92, 80.0, 128))
    }

    fn light() -> Arc<SincKernel> {
        Arc::new(SincKernel::new(16, 0.9, 60.0, 128))
    }

    /// A time `samples` (whole and fraction) on the samples not rendered.
    fn at(samples: f64) -> SampleTime {
        let whole = samples.floor();
        SampleTime {
            sample: whole as i64,
            fraction: ((samples - whole) * 4_294_967_296.0) as u32,
        }
    }

    #[test]
    fn a_step_touches_nothing_before_its_window_and_then_holds_its_level() {
        let mut steps = BandLimitedSteps::<2>::new(light());
        steps.set_level(at(10.25), [1.0, -0.5]);
        let mut out = vec![0.0f32; 2 * 80];
        steps.render(80, &mut out);
        for i in 0..=10 {
            assert_eq!(out[2 * i], 0.0, "sample {i}, before the step's window");
        }
        for i in 10 + steps.span() + 1..80 {
            assert_eq!(out[2 * i], 1.0, "sample {i}");
            assert_eq!(out[2 * i + 1], -0.5, "sample {i}");
        }
        // Its middle half a sample before the delay of the half width.
        let middle: f64 = 10.25 + 16.0 - 0.5;
        let (below, above) = (
            out[2 * middle.floor() as usize],
            out[2 * middle.ceil() as usize],
        );
        assert!(below < 0.5 && above > 0.5, "{below} {above}");
    }

    /// A run rendered in pieces is the run rendered at once, sample for
    /// sample, each change given on the samples not rendered at its time.
    #[test]
    fn a_run_in_pieces_is_the_run_at_once() {
        let changes: Vec<(f64, f64)> = (0..300)
            .map(|i| {
                (
                    f64::from(i) * 3.71 + 0.13,
                    if i % 3 == 0 {
                        0.7
                    } else {
                        -0.2 * f64::from(i % 5)
                    },
                )
            })
            .collect();
        let mut whole = BandLimitedSteps::<1>::new(light());
        for &(time, level) in &changes {
            whole.set_level(at(time), [level]);
        }
        let mut at_once = vec![0.0f32; 1200];
        whole.render(1200, &mut at_once);

        let mut pieces = BandLimitedSteps::<1>::new(light());
        let mut rendered = 0usize;
        let mut out = vec![0.0f32; 1200];
        let mut next = changes.iter().peekable();
        for length in [1usize, 7, 64, 3, 200, 333, 0, 92, 500] {
            let end = (rendered + length).min(1200);
            // Every change before this piece's end goes in first.
            while let Some(&&(time, level)) = next.peek() {
                if time >= end as f64 {
                    break;
                }
                pieces.set_level(at(time - rendered as f64), [level]);
                next.next();
            }
            pieces.render(end - rendered, &mut out[rendered..end]);
            rendered = end;
        }
        assert_eq!(rendered, 1200);
        assert_eq!(out, at_once);
    }

    /// Two given different pasts give the same samples a span after a
    /// change that sets both to one level, whatever came before.
    #[test]
    fn the_samples_forget_every_change_older_than_the_span() {
        let mut a = BandLimitedSteps::<2>::new(full());
        let mut b = BandLimitedSteps::<2>::new(full());
        for i in 0..40 {
            a.set_level(at(f64::from(i) * 2.3), [f64::from(i % 7) * 0.1, -0.3]);
            b.set_level(at(f64::from(i) * 1.9 + 0.5), [0.25, f64::from(i % 3) * 0.2]);
        }
        let mut out_a = vec![0.0f32; 2 * 400];
        let mut out_b = out_a.clone();
        for steps in [&mut a, &mut b] {
            steps.set_level(at(100.6), [0.4, 0.1]);
            for i in 0..50 {
                steps.set_level(
                    at(130.0 + f64::from(i) * 4.4),
                    [f64::from(i % 4) * 0.3, -0.6],
                );
            }
        }
        a.render(400, &mut out_a);
        b.render(400, &mut out_b);
        let from = 100 + 1 + a.span();
        assert_ne!(out_a[..2 * from], out_b[..2 * from]);
        assert_eq!(out_a[2 * from..], out_b[2 * from..]);
    }

    /// The steps of a sum are the sum of the steps.
    #[test]
    fn the_steps_of_a_sum_are_the_sum_of_the_steps() {
        let (mut a, mut b, mut sum) = (
            BandLimitedSteps::<1>::new(full()),
            BandLimitedSteps::<1>::new(full()),
            BandLimitedSteps::<1>::new(full()),
        );
        let mut levels = (0.0, 0.0);
        for i in 0..200 {
            let time = f64::from(i) * 1.37;
            if i % 2 == 0 {
                levels.0 = f64::from(i % 9) * 0.05 - 0.2;
                a.set_level(at(time), [levels.0]);
            } else {
                levels.1 = 0.3 - f64::from(i % 5) * 0.11;
                b.set_level(at(time), [levels.1]);
            }
            sum.set_level(at(time), [levels.0 + levels.1]);
        }
        let mut out = [vec![0.0f32; 400], vec![0.0f32; 400], vec![0.0f32; 400]];
        for (steps, out) in [&mut a, &mut b, &mut sum].into_iter().zip(out.iter_mut()) {
            steps.render(400, out);
        }
        let [a, b, sum] = &out;
        for (i, ((&a, &b), &sum)) in a.iter().zip(b).zip(sum).enumerate() {
            let parts = f64::from(a) + f64::from(b);
            assert!((parts - f64::from(sum)).abs() < 1e-6, "sample {i}");
        }
    }

    /// A square wave of steps at its half periods' exact times holds only
    /// its harmonics: the power outside them is what folding and the
    /// kernel's stopband leave (an ideal band-limited square has none).
    #[test]
    fn a_square_wave_keeps_its_harmonics_alone() {
        let (rate, f0) = (44_100.0, 4_660.866);
        let length = 8192;
        let mut steps = BandLimitedSteps::<1>::new(full());
        let half_period = rate / (2.0 * f0);
        let mut level = 1.0;
        steps.set_level(at(-0.5), [level]);
        let mut time = 0.37;
        while time < 2.0 * length as f64 {
            level = -level;
            steps.set_level(at(time), [level]);
            time += half_period;
        }
        let mut out = vec![0.0f32; 2 * length];
        steps.render(2 * length, &mut out);
        let window = blackman_harris(length);
        let segment: Vec<f64> = out[length / 2..length / 2 + length]
            .iter()
            .zip(&window)
            .map(|(&s, &w)| f64::from(s) * w)
            .collect();
        let power = power_spectrum(&segment);
        let bin = rate / length as f64;
        let (mut total, mut outside) = (0.0, 0.0);
        for (k, &p) in power.iter().enumerate().skip(9) {
            total += p;
            let f = k as f64 * bin;
            let harmonic = (1..400)
                .step_by(2)
                .any(|h| (f - f64::from(h) * f0).abs() <= 8.0 * bin);
            if !harmonic {
                outside += p;
            }
        }
        let db = 10.0 * (outside / total).log10();
        assert!(db < -70.0, "{db} dB outside the harmonics");
    }

    /// A tone made at a chip's own rate and held from sample to sample is
    /// the tone, less the hold's droop: the hold's images above the host's
    /// band, which would fold into it, are gone.
    #[test]
    fn samples_held_at_another_rate_keep_their_tone_alone() {
        let (rate, own, f0) = (44_100.0, 53_267.0, 1_000.0);
        let length = 8192;
        let mut steps = BandLimitedSteps::<1>::new(full());
        let mut n = 0u32;
        loop {
            let time = f64::from(n) * rate / own;
            if time >= 2.0 * length as f64 {
                break;
            }
            let value = (2.0 * std::f64::consts::PI * f0 * f64::from(n) / own).sin() * 0.5;
            steps.set_level(at(time), [value]);
            n += 1;
        }
        let mut out = vec![0.0f32; 2 * length];
        steps.render(2 * length, &mut out);
        let window = blackman_harris(length);
        let segment: Vec<f64> = out[length / 2..length / 2 + length]
            .iter()
            .zip(&window)
            .map(|(&s, &w)| f64::from(s) * w)
            .collect();
        let power = power_spectrum(&segment);
        let bin = rate / length as f64;
        let (mut total, mut outside) = (0.0, 0.0);
        for (k, &p) in power.iter().enumerate().skip(9) {
            total += p;
            if (k as f64 * bin - f0).abs() > 8.0 * bin {
                outside += p;
            }
        }
        let db = 10.0 * (outside / total).log10();
        assert!(db < -85.0, "{db} dB outside the tone");
        // The tone's amplitude, against a sine of 0.5 held: sin(x) / x of
        // the hold's x = pi f0 / own.
        let x = std::f64::consts::PI * f0 / own;
        let held = 0.5 * x.sin() / x;
        let peak = out[length..2 * length]
            .iter()
            .fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(
            (f64::from(peak) - held).abs() < 1e-3,
            "{peak} against {held}"
        );
    }

    #[test]
    fn clearing_leaves_silence() {
        let mut steps = BandLimitedSteps::<1>::new(light());
        steps.set_level(at(3.0), [0.8]);
        steps.clear();
        let mut out = vec![1.0f32; 64];
        steps.render(64, &mut out);
        assert!(out.iter().all(|&s| s == 0.0));
        assert_eq!(steps.level(), [0.0]);
    }

    /// A change a span or more before the first sample not rendered is
    /// its level from that sample on; one less far back is heard from it
    /// with what is left of its step.
    #[test]
    fn a_change_in_the_past_comes_in_at_the_next_sample() {
        let mut steps = BandLimitedSteps::<1>::new(light());
        let mut out = vec![0.0f32; 64];
        steps.render(8, &mut out);
        steps.set_level(at(-40.0), [0.6]);
        steps.render(64, &mut out);
        assert!(out.iter().all(|&s| s == 0.6), "{:?}", &out[..4]);

        steps.set_level(at(-20.0), [0.2]);
        steps.render(64, &mut out);
        assert!(
            out[..12].iter().all(|&s| (s - 0.2).abs() < 0.05),
            "{:?}",
            &out[..12]
        );
        assert!(out[12..].iter().all(|&s| s == 0.2), "{:?}", &out[12..16]);
    }

    /// Past the samples a change touched, a long render is the held level
    /// alone: the buffers hold only what the changes left.
    #[test]
    fn a_long_render_holds_the_level() {
        let mut steps = BandLimitedSteps::<1>::new(light());
        steps.set_level(at(2.0), [0.3]);
        let mut out = vec![0.0f32; 100_000];
        steps.render(100_000, &mut out);
        assert!(out[40..].iter().all(|&s| s == 0.3));
        assert!(steps.residual.is_empty());
    }
}
