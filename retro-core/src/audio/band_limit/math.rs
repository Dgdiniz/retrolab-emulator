//! The functions the kernels and the filter are built from, worked out by
//! series of additions, subtractions, multiplications and divisions: IEEE
//! 754 rounds those (and the square root) exactly on every target, where
//! the library's transcendental functions may differ by a unit in the last
//! place between a native build and a WebAssembly one, and so would every
//! sample made from them.

use std::f64::consts::PI;

/// sin(pi x).
pub(super) fn sin_pi(x: f64) -> f64 {
    // sin(pi x) has period 2: x into [-1, 1).
    let mut r = x - 2.0 * (x / 2.0).floor();
    if r >= 1.0 {
        r -= 2.0;
    }
    // Odd, and sin(pi r) = sin(pi (1 - r)): r into [0, 1/2].
    let (sign, mut r) = if r < 0.0 { (-1.0, -r) } else { (1.0, r) };
    if r > 0.5 {
        r = 1.0 - r;
    }
    let a = PI * r;
    let a2 = a * a;
    // The Taylor series of sin to its term in a^27: past it the terms are
    // below 1e-17 for a up to pi / 2.
    let mut term = a;
    let mut sum = a;
    let mut n = 1.0;
    for _ in 0..13 {
        term *= -a2 / ((n + 1.0) * (n + 2.0));
        sum += term;
        n += 2.0;
    }
    sign * sum
}

/// e^x: halved until small, its series there, then squared back.
pub(super) fn exp(x: f64) -> f64 {
    let mut halvings = 0;
    let mut y = x;
    while y.abs() > 0.5 {
        y /= 2.0;
        halvings += 1;
    }
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..30 {
        term *= y / f64::from(k);
        sum += term;
    }
    for _ in 0..halvings {
        sum *= sum;
    }
    sum
}

/// I0(x), the modified Bessel function of the first kind and of order 0:
/// the Kaiser window's shape. Its series has only positive terms.
pub(super) fn bessel_i0(x: f64) -> f64 {
    let half = x / 2.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut k = 1.0;
    loop {
        let ratio = half / k;
        term *= ratio * ratio;
        sum += term;
        if term < sum * 1e-21 {
            return sum;
        }
        k += 1.0;
    }
}

/// The Kaiser window's beta for a stopband `attenuation` dB down, by
/// Kaiser's formula for an attenuation over 50 dB, 0.1102 (A - 8.7); below
/// that his formula needs a power, which no kernel here asks for.
pub(super) fn kaiser_beta(attenuation: f64) -> f64 {
    assert!(
        attenuation > 50.0,
        "a stopband over 50 dB down, not {attenuation}"
    );
    0.1102 * (attenuation - 8.7)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_pi_matches_the_library_s_sine() {
        for i in -4000..=4000 {
            let x = f64::from(i) * 0.00137;
            let expected = (PI * x).sin();
            assert!((sin_pi(x) - expected).abs() < 4e-15, "sin_pi({x})");
        }
        assert_eq!(sin_pi(0.0), 0.0);
        assert_eq!(sin_pi(1.0), 0.0);
        assert_eq!(sin_pi(-3.0), 0.0);
    }

    #[test]
    fn exp_matches_the_library_s_exponential() {
        for i in -200..=200 {
            let x = f64::from(i) * 0.05;
            let expected = x.exp();
            assert!(
                (exp(x) - expected).abs() <= 4e-14 * expected,
                "exp({x}): {} against {expected}",
                exp(x)
            );
        }
        assert_eq!(exp(0.0), 1.0);
    }

    /// I0 against its values, given as e^-x I0(x) to ten places.
    #[test]
    fn bessel_i0_matches_its_table() {
        assert_eq!(bessel_i0(0.0), 1.0);
        for (x, scaled) in [
            (1.0, 0.465_759_607_6),
            (2.0, 0.308_508_322_6),
            (5.0, 0.183_540_812_6),
            (10.0, 0.127_833_337_2),
        ] {
            let value = bessel_i0(x) * (-x).exp();
            assert!((value - scaled).abs() < 1e-10, "I0({x}) e^-{x} = {value}");
        }
    }

    #[test]
    fn the_beta_is_kaisers() {
        assert!((kaiser_beta(80.0) - 7.857_26).abs() < 1e-12);
        assert!((kaiser_beta(60.0) - 5.653_26).abs() < 1e-12);
    }

    #[test]
    #[should_panic(expected = "over 50 dB")]
    fn the_beta_asks_for_more_than_50_db() {
        kaiser_beta(40.0);
    }
}
