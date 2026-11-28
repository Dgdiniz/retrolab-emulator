//! The spectra the module's tests measure against: a window and a power
//! spectrum by a radix-2 fast Fourier transform.

use std::f64::consts::PI;

/// The 4-term Blackman-Harris window of `length` samples: its sidelobes
/// sit near -92 dB, under what the tests ask of the kernels.
pub(super) fn blackman_harris(length: usize) -> Vec<f64> {
    (0..length)
        .map(|i| {
            let a = 2.0 * PI * i as f64 / (length - 1) as f64;
            0.35875 - 0.48829 * a.cos() + 0.14128 * (2.0 * a).cos() - 0.01168 * (3.0 * a).cos()
        })
        .collect()
}

/// The power at bins 0 to `signal.len() / 2 - 1` of `signal`, whose length
/// is a power of two.
pub(super) fn power_spectrum(signal: &[f64]) -> Vec<f64> {
    let n = signal.len();
    assert!(n.is_power_of_two(), "a length that is a power of two");
    let mut re = signal.to_vec();
    let mut im = vec![0.0; n];
    // Bit-reversed order, then the butterflies.
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            re.swap(i, j);
        }
    }
    let mut size = 2;
    while size <= n {
        let step = -2.0 * PI / size as f64;
        for start in (0..n).step_by(size) {
            for k in 0..size / 2 {
                let (wr, wi) = ((step * k as f64).cos(), (step * k as f64).sin());
                let (a, b) = (start + k, start + k + size / 2);
                let (xr, xi) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                re[b] = re[a] - xr;
                im[b] = im[a] - xi;
                re[a] += xr;
                im[a] += xi;
            }
        }
        size *= 2;
    }
    (0..n / 2).map(|k| re[k] * re[k] + im[k] * im[k]).collect()
}

#[test]
fn the_transform_finds_a_tone_in_its_bin() {
    let n = 1024;
    let signal: Vec<f64> = (0..n)
        .map(|i| (2.0 * PI * 37.0 * i as f64 / n as f64).cos())
        .collect();
    let power = power_spectrum(&signal);
    let peak = (0..n / 2)
        .max_by(|&a, &b| power[a].total_cmp(&power[b]))
        .expect("bins");
    assert_eq!(peak, 37);
    assert!((power[37] - (n as f64 / 2.0).powi(2)).abs() < 1e-6);
    assert!(power.iter().enumerate().all(|(k, &p)| k == 37 || p < 1e-12));
}
