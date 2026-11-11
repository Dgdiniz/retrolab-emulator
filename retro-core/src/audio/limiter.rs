//! Soft clipping utilities for audio output.
//!
//! Hard clamping (`x.clamp(-1.0, 1.0)`) introduces harsh harmonic
//! distortion whenever the mixed signal exceeds full-scale. A soft
//! clipper rounds the corner so brief overshoots colour the sound
//! rather than ringing.
//!
//! The transfer function used here is `tanh`, which is monotone,
//! C-infinity smooth, and bounded to `(-1, 1)`. Cheap on modern
//! hardware (one libm call or SIMD intrinsic per sample) and
//! perceptually pleasing.

/// Soft-clip a single sample to `[-1, 1]`.
///
/// Values well inside `[-1, 1]` are returned almost unchanged
/// (`tanh(x) ~ x` for `|x| << 1`). Larger values are smoothly
/// compressed and saturate exactly at `+/-1.0` in `f32` precision
/// (because `tanh(+/-inf) = +/-1.0` and `tanh` saturates inside `f32`
/// resolution well before that for `|x|` above about 9).
#[inline]
pub fn soft_clip(sample: f32) -> f32 {
    sample.tanh()
}

/// Apply [`soft_clip`] in-place to every element of `buffer`.
pub fn soft_clip_buffer(buffer: &mut [f32]) {
    for sample in buffer.iter_mut() {
        *sample = sample.tanh();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_clip_passes_zero_through() {
        assert_eq!(soft_clip(0.0), 0.0);
    }

    #[test]
    fn soft_clip_is_bounded_to_unit_interval() {
        for &x in &[-10.0f32, -2.0, -1.0, 1.0, 2.0, 10.0, 100.0] {
            let y = soft_clip(x);
            assert!(
                (-1.0..=1.0).contains(&y),
                "soft_clip({x}) = {y} should be in [-1, 1]"
            );
        }
    }

    #[test]
    fn soft_clip_compresses_overshoot_below_unity() {
        // For mild overshoot (just past full-scale) tanh should
        // soften the corner: where a hard clip would output exactly
        // 1.0, the output stays strictly below unity, so the signal
        // stays inside the safe range.
        let y = soft_clip(1.2);
        assert!(y < 1.0, "soft_clip(1.2) = {y} must compress below unity");
        assert!(
            y > 0.8,
            "soft_clip(1.2) = {y} should still be close to unity"
        );
    }

    #[test]
    fn soft_clip_is_odd_symmetric() {
        for &x in &[0.5f32, 1.0, 2.5, 7.7] {
            let y_pos = soft_clip(x);
            let y_neg = soft_clip(-x);
            assert!(
                (y_pos + y_neg).abs() < 1e-6,
                "soft_clip({x}) + soft_clip(-{x}) = {} should be ~0",
                y_pos + y_neg
            );
        }
    }

    #[test]
    fn soft_clip_is_near_linear_for_small_inputs() {
        for &x in &[-0.01f32, -0.001, 0.001, 0.01] {
            let y = soft_clip(x);
            assert!(
                (y - x).abs() < 1e-4,
                "soft_clip({x}) = {y} should be very close to x for small x"
            );
        }
    }

    #[test]
    fn soft_clip_buffer_matches_per_sample() {
        let mut buf = [-2.0f32, -0.5, 0.0, 0.5, 2.0, 10.0];
        let expected: Vec<f32> = buf.iter().map(|&x| soft_clip(x)).collect();
        soft_clip_buffer(&mut buf);
        for (got, want) in buf.iter().zip(expected.iter()) {
            assert!((got - want).abs() < 1e-9);
        }
    }

    #[test]
    fn soft_clip_buffer_empty_is_noop() {
        let mut buf: [f32; 0] = [];
        soft_clip_buffer(&mut buf);
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn soft_clip_handles_nan_and_inf() {
        assert!(soft_clip(f32::NAN).is_nan());
        // tanh(+inf) = 1.0, tanh(-inf) = -1.0
        assert_eq!(soft_clip(f32::INFINITY), 1.0);
        assert_eq!(soft_clip(f32::NEG_INFINITY), -1.0);
    }
}
