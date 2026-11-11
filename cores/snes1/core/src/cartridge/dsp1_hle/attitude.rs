//! The attitude commands: three rotation matrices and the vectors taken
//! through them.
//!
//! The chip keeps three 3 x 3 matrices, A, B and C. `Attitude` builds one
//! from a scale and three angles, turns about the Z, Y and X axes, applied
//! in that order. `Objective` takes a vector into the turned frame (the
//! matrix times the vector), `Subjective` brings one back (the transpose
//! times the vector), and `Scalar` gives only the first component of the
//! turned vector, summed wide before it is cut to a word. `Gyrate` advances
//! three such angles by a turn rate given in the turned frame, and `Polar`
//! turns a vector by three angles without a matrix.

use super::number::{cosine, dot, q15, sine, split_wide, Scaled, Turn};

/// A rotation matrix, by rows.
pub(super) type Matrix = [[i16; 3]; 3];

/// `first` times each of `factors` in turn, as 1.15 products.
fn chain(first: i16, factors: &[i16]) -> i16 {
    factors.iter().fold(first, |product, &f| q15(product, f))
}

/// `Attitude`: the matrix of a turn about Z, then Y, then X, times half the
/// scale (the half leaves room for a sum of two terms).
pub(super) fn attitude(scale: i16, about_z: i16, about_y: i16, about_x: i16) -> Matrix {
    let (z, y, x) = (Turn::of(about_z), Turn::of(about_y), Turn::of(about_x));
    let m = scale >> 1;
    [
        [
            chain(m, &[z.cos, y.cos]),
            chain(m, &[z.sin, y.cos]).wrapping_neg(),
            q15(m, y.sin),
        ],
        [
            chain(m, &[z.sin, x.cos]).wrapping_add(chain(m, &[z.cos, x.sin, y.sin])),
            chain(m, &[z.cos, x.cos]).wrapping_sub(chain(m, &[z.sin, x.sin, y.sin])),
            chain(m, &[x.sin, y.cos]).wrapping_neg(),
        ],
        [
            chain(m, &[z.sin, x.sin]).wrapping_sub(chain(m, &[z.cos, x.cos, y.sin])),
            chain(m, &[z.cos, x.sin]).wrapping_add(chain(m, &[z.sin, x.cos, y.sin])),
            chain(m, &[x.cos, y.cos]),
        ],
    ]
}

/// `Objective`: the matrix times the vector.
pub(super) fn objective(matrix: &Matrix, v: [i16; 3]) -> [i16; 3] {
    matrix.map(|row| dot(v, row))
}

/// `Subjective`: the transposed matrix times the vector.
pub(super) fn subjective(matrix: &Matrix, v: [i16; 3]) -> [i16; 3] {
    [0, 1, 2].map(|column| dot(v, matrix.map(|row| row[column])))
}

/// `Scalar`: the vector against the matrix's first row, the three products
/// summed at full width and cut to a word once.
pub(super) fn scalar(matrix: &Matrix, v: [i16; 3]) -> i16 {
    let sum = (0..3).fold(0i32, |sum, i| {
        sum.wrapping_add(i32::from(v[i]) * i32::from(matrix[0][i]))
    });
    (sum >> 15) as i16
}

/// `Rotate`: the point `(x, y)` turned by `angle` in its plane.
pub(super) fn rotate(angle: i16, x: i16, y: i16) -> [i16; 2] {
    let (turned_x, turned_y) = Turn::of(angle).turned(y, x);
    [turned_x, turned_y]
}

/// `Polar`: the vector turned about Z, then Y, then X.
pub(super) fn polar(about: [i16; 3], v: [i16; 3]) -> [i16; 3] {
    let [about_z, about_y, about_x] = about;
    let [x, y, z] = v;
    let (x, y) = Turn::of(about_z).turned(y, x);
    let (z, x) = Turn::of(about_y).turned(x, z);
    let (y, z) = Turn::of(about_x).turned(z, y);
    [x, y, z]
}

/// `Gyrate`: three angles `[z, x, y]` advanced by a turn `[u, f, l]` given
/// in the turned frame. With `c` and `s` the cosine and sine of the Y angle:
///
/// - Z gains `(u*c - f*s) / cos x`;
/// - X gains `u*s + f*c`;
/// - Y gains `l - (u*c + f*s) * tan x`.
pub(super) fn gyrate(angles: [i16; 3], turn: [i16; 3]) -> [i16; 3] {
    let [z, x, y] = angles;
    let [u, f, l] = turn;
    let about_y = Turn::of(y);
    let secant = Scaled {
        mantissa: cosine(x),
        exponent: 0,
    }
    .reciprocal();
    let wide = |a: i16, b: i16| i32::from(a) * i32::from(b);

    let (across, places) = split_wide(wide(u, about_y.cos).wrapping_sub(wide(f, about_y.sin)));
    let z_gain = Scaled::normalized(
        q15(across, secant.mantissa),
        secant.exponent.wrapping_sub(places),
    )
    .to_word();

    let x_gain = q15(u, about_y.sin).wrapping_add(q15(f, about_y.cos));

    let (along, places) = split_wide(wide(u, about_y.cos).wrapping_add(wide(f, about_y.sin)));
    let sine_x = Scaled::normalized(sine(x), secant.exponent.wrapping_sub(places));
    let tangent = q15(secant.mantissa, sine_x.mantissa);
    let y_loss = Scaled::normalized(q15(along, tangent).wrapping_neg(), sine_x.exponent).to_word();

    [
        z.wrapping_add(z_gain),
        x.wrapping_add(x_gain),
        y.wrapping_add(y_loss).wrapping_add(l),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No turn at all, at full scale: half the scale down the diagonal, less
    /// the bit each of its two products with "1" (`0x7FFF`) rounds away.
    const LEVEL: Matrix = [[0x3ffd, 0, 0], [0, 0x3ffd, 0], [0, 0, 0x3ffd]];

    #[test]
    fn no_turn_gives_a_scaled_identity() {
        assert_eq!(attitude(0x7fff, 0, 0, 0), LEVEL);
    }

    #[test]
    fn a_quarter_turn_about_z_swaps_x_and_y() {
        let m = attitude(0x7fff, 0x4000, 0, 0);
        // Row 0 looks along -Y, row 1 along +X.
        assert_eq!(m[0][0], 0);
        assert!(m[0][1] < -0x3ff0);
        assert!(m[1][0] > 0x3ff0);
        assert_eq!(m[2][2], 0x3ffd);
    }

    #[test]
    fn objective_and_subjective_are_each_others_transpose() {
        let m = attitude(0x7fff, 0x1234, 0x0abc, 0x2000);
        let v = [0x0400, -0x0200, 0x0100];
        let transposed: Matrix = [0, 1, 2].map(|i| [m[0][i], m[1][i], m[2][i]]);
        assert_eq!(subjective(&m, v), objective(&transposed, v));
        // Scalar is Objective's first word, but for the rounding of one sum
        // against three.
        let first = objective(&m, v)[0];
        assert!((i32::from(scalar(&m, v)) - i32::from(first)).abs() <= 2);
    }

    #[test]
    fn rotate_and_polar_turn_a_vector() {
        // A quarter turn takes (x, y) to (y, -x); "1" being 0x7FFF, a word
        // comes through a bit short.
        assert_eq!(rotate(0x4000, 0x1000, 0), [0, -0x0fff]);
        assert_eq!(rotate(0, 0x1000, 0x0800), [0x0fff, 0x07ff]);
        // Polar with only the Z angle is Rotate with z carried through.
        let [x, y, z] = polar([0x4000, 0, 0], [0x1000, 0, 0x0700]);
        assert_eq!((x, y), (0, -0x0fff));
        assert!((i32::from(z) - 0x0700).abs() <= 2);
    }

    #[test]
    fn gyrate_without_a_turn_leaves_the_angles() {
        assert_eq!(
            gyrate([0x1000, 0x0800, 0x2000], [0, 0, 0]),
            [0x1000, 0x0800, 0x2000]
        );
        // A turn about the third axis alone moves only the Y angle.
        assert_eq!(
            gyrate([0x1000, 0x0800, 0x2000], [0, 0, 0x0100]),
            [0x1000, 0x0800, 0x2100]
        );
    }
}
