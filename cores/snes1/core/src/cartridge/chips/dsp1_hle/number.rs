//! The numbers the DSP-1 computes with.
//!
//! Every word the chip handles is 1.15 fixed point: -1 at `0x8000`, just
//! under +1 at `0x7FFF`. Where a routine needs more range it carries a pair:
//! a 1.15 mantissa and the power of two that scales it ([`Scaled`]). The
//! chip has neither a shifter nor a divider. It moves a mantissa by
//! multiplying it with a power of two out of its data ROM, and it divides by
//! multiplying with a reciprocal it builds from a table of first guesses.
//! Its sine and its square root are tables too, read with a straight line
//! between entries.
//!
//! Every function here stops where the chip's word does: sums and products
//! wrap at sixteen bits (or at thirty-two, for the few values kept wide),
//! as they do in the chip's registers.

use super::rom;

/// The 1.15 product of two 1.15 numbers: the top of the 31-bit product,
/// rounded down and cut to a word.
#[inline]
pub(super) fn q15(a: i16, b: i16) -> i16 {
    q15_wide(i32::from(a), i32::from(b))
}

/// [`q15`] for factors already widened. The chip forms `-sin` in its wide
/// accumulator before multiplying, so the negation never overflows a word;
/// passing the negated factor here keeps that.
#[inline]
pub(super) fn q15_wide(a: i32, b: i32) -> i16 {
    (a.wrapping_mul(b) >> 15) as i16
}

/// How many bits under the sign bit repeat it, the places a word can move
/// up without losing anything. 15 for 0 and for -1.
fn spare_bits(value: i16) -> i16 {
    // Folding a negative word onto its complement leaves leading zeros
    // where the repeated sign bits were.
    let folded = (value ^ (value >> 15)) as u16;
    (folded.leading_zeros() - 1) as i16
}

/// `mantissa` moved down by the size of `places`, the way the chip does it:
/// one multiplication by the ROM's factor for that many places. The ROM
/// lists the factors on both sides of one word, so a count and its negative
/// move the mantissa the same way, a routine uses whichever it has, a
/// number of places or a negative exponent.
pub(super) fn moved_down(mantissa: i16, places: i16) -> i16 {
    q15_wide(
        i32::from(mantissa),
        rom::word(rom::FRACTIONS + i32::from(places)),
    )
}

/// A value kept wide, a difference of coordinates, a sum of products, as
/// a mantissa and the number of places it moved up to become one.
///
/// The chip holds such a value in two registers, fifteen low bits and the
/// rest, and normalises the pair: the high part moves up until its top bit
/// counts, the bits of the low part that fit move in under it, and when the
/// high part is nothing but sign the low part is normalised in its place.
pub(super) fn split_wide(value: i32) -> (i16, i16) {
    let low = (value & 0x7fff) as i16;
    let high = (value >> 15) as i16;
    let places = spare_bits(high);
    if places < 15 {
        let carried_in = if places == 0 {
            0
        } else {
            q15_wide(i32::from(low), rom::word(rom::CARRIES - i32::from(places)))
        };
        return ((high << places).wrapping_add(carried_in), places);
    }
    // `high` is 0 or -1. Go on counting into the low word, read with the
    // same sign.
    let further = spare_bits(if high < 0 { low | i16::MIN } else { low });
    let mantissa = if further == 0 {
        (high << 15).wrapping_add(low)
    } else {
        low << further
    };
    (mantissa, 15 + further)
}

/// A number as the chip's routines hand it on: a 1.15 mantissa and the
/// power of two that scales it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Scaled {
    pub(super) mantissa: i16,
    pub(super) exponent: i16,
}

impl Scaled {
    /// `mantissa x 2^exponent` with the mantissa moved up as far as it goes.
    pub(super) fn normalized(mantissa: i16, exponent: i16) -> Self {
        let places = spare_bits(mantissa);
        Self {
            mantissa: mantissa << places,
            exponent: exponent.wrapping_sub(places),
        }
    }

    /// This number normalised again, after a product, which leaves the
    /// mantissa short.
    pub(super) fn renormalized(self) -> Self {
        Self::normalized(self.mantissa, self.exponent)
    }

    /// The product of two scaled numbers, left as the multiplier gives it.
    pub(super) fn times(self, other: Self) -> Self {
        Self {
            mantissa: q15(self.mantissa, other.mantissa),
            exponent: self.exponent.wrapping_add(other.exponent),
        }
    }

    /// The number as one 1.15 word. Too large a number pins at +/-32 767; a
    /// negative exponent moves the mantissa down.
    pub(super) fn to_word(self) -> i16 {
        match self.exponent {
            1.. => 32767 * self.mantissa.signum(),
            0 => self.mantissa,
            places => moved_down(self.mantissa, places),
        }
    }

    /// `1 / self`.
    ///
    /// The mantissa's magnitude is normalised into 1/2..1, the ROM gives a
    /// first guess from its top seven bits, and two rounds of Newton's
    /// method (`g <- 2g - v*g^2`, arranged for a multiplier that keeps the top
    /// of each product) settle it. Exactly 1/2 skips the table: its
    /// reciprocal is the format's "1", or -1/2 one place up. 0 answers with
    /// the largest number the format holds.
    pub(super) fn reciprocal(self) -> Self {
        if self.mantissa == 0 {
            return Self {
                mantissa: 0x7fff,
                exponent: 0x2f,
            };
        }
        let negative = self.mantissa < 0;
        // -1 has no positive word; the chip takes it as just under 1.
        let magnitude = self.mantissa.checked_abs().unwrap_or(i16::MAX);
        let Self {
            mantissa: value,
            mut exponent,
        } = Self::normalized(magnitude, self.exponent);

        let mantissa = if value == 0x4000 {
            if negative {
                exponent = exponent.wrapping_sub(1);
                -0x4000
            } else {
                0x7fff
            }
        } else {
            let slot = (i32::from(value) - 0x4000) >> 7;
            let mut guess = rom::word(rom::RECIPROCAL_GUESSES + slot);
            for _ in 0..2 {
                let error = (i32::from(value) * guess) >> 15;
                guess = i32::from(((guess + ((-guess * error) >> 15)) << 1) as i16);
            }
            (if negative { -guess } else { guess }) as i16
        };
        Self {
            mantissa,
            exponent: 1i16.wrapping_sub(exponent),
        }
    }
}

/// The sine table at `step` 256ths of a turn, for the steps the two
/// functions below reach (0 to 191): the ROM holds half a turn, and the
/// second half is the first with the sign turned.
fn wave(step: i32) -> i32 {
    if step < 128 {
        rom::word(rom::HALF_WAVE + step)
    } else {
        -rom::word(rom::HALF_WAVE + step - 128)
    }
}

/// An angle below one table step (0 to 255), in radians as 1.15: the step
/// count times pi.
fn arc(part: i32) -> i32 {
    (part * rom::word(rom::PI)) >> 13
}

/// The table step an angle falls in, and how far into it. The angle is not
/// negative.
fn step_and_part(angle: i16) -> (i32, i32) {
    (i32::from(angle >> 8), i32::from(angle & 0xff))
}

/// A full turn's quarter, in table steps.
const QUARTER: i32 = 0x40;

/// `sin(angle)`, a full turn being 65 536: the table entry, plus the arc
/// past it times the cosine there.
pub(super) fn sine(angle: i16) -> i16 {
    if angle < 0 {
        // The half turn itself has no positive counterpart; its sine is 0.
        return if angle == i16::MIN {
            0
        } else {
            sine(-angle).wrapping_neg()
        };
    }
    let (step, part) = step_and_part(angle);
    let value = wave(step) + ((arc(part) * wave(step + QUARTER)) >> 15);
    value.min(32767) as i16
}

/// `cos(angle)`: the same table a quarter turn on, less the arc times the
/// sine.
pub(super) fn cosine(angle: i16) -> i16 {
    if angle == i16::MIN {
        return i16::MIN;
    }
    let (step, part) = step_and_part(angle.abs());
    let value = wave(step + QUARTER) - ((arc(part) * wave(step)) >> 15);
    if value < -32768 {
        -32767
    } else {
        value as i16
    }
}

/// The sine and cosine of one angle, as the commands keep them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Turn {
    pub(super) sin: i16,
    pub(super) cos: i16,
}

impl Turn {
    pub(super) fn of(angle: i16) -> Self {
        Self {
            sin: sine(angle),
            cos: cosine(angle),
        }
    }

    /// `(a*sin + b*cos, a*cos - b*sin)`: the pair `(b, a)` turned by this
    /// angle, which is how the chip writes every rotation in a plane.
    pub(super) fn turned(self, a: i16, b: i16) -> (i16, i16) {
        (
            q15(a, self.sin).wrapping_add(q15(b, self.cos)),
            q15(a, self.cos).wrapping_sub(q15(b, self.sin)),
        )
    }
}

/// The sum of the three 1.15 products of two vectors' components.
pub(super) fn dot(a: [i16; 3], b: [i16; 3]) -> i16 {
    q15(a[0], b[0])
        .wrapping_add(q15(a[1], b[1]))
        .wrapping_add(q15(a[2], b[2]))
}

/// `x^2 + y^2 + z^2`, kept wide.
pub(super) fn sum_of_squares(v: [i16; 3]) -> i32 {
    v.iter().fold(0i32, |sum, &part| {
        sum.wrapping_add(i32::from(part) * i32::from(part))
    })
}

/// The length of a vector: the square root of its sum of squares.
///
/// The sum is normalised to a mantissa and an even number of places (an odd
/// count halves the mantissa once more), the ROM's nodes on either side of
/// the mantissa's top six bits are joined by a straight line read at its
/// low nine bits, and half the places come back off the result.
pub(super) fn length(v: [i16; 3]) -> i16 {
    let squares = sum_of_squares(v);
    if squares == 0 {
        return 0;
    }
    let (mut mantissa, places) = split_wide(squares);
    if places & 1 != 0 {
        mantissa = q15(mantissa, 0x4000);
    }
    let node = rom::ROOT_NODES + i32::from(q15(mantissa, 0x0040));
    let (below, above) = (rom::word(node), rom::word(node + 1));
    let root = (((above - below) * (i32::from(mantissa) & 0x1ff)) >> 9) + below;
    (root as i16) >> (places >> 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_product_keeps_the_top_of_31_bits() {
        assert_eq!(q15(0x4000, 0x4000), 0x2000);
        assert_eq!(q15(-32768, 0x4000), -0x4000);
        // -1 x -1 has no 1.15 word: the product's top bit falls off.
        assert_eq!(q15(-32768, -32768), -32768);
        // Rounded down, not toward zero.
        assert_eq!(q15(-1, 1), -1);
    }

    #[test]
    fn normalising_moves_the_mantissa_up_to_its_top_bit() {
        assert_eq!(
            Scaled::normalized(0x0001, 0),
            Scaled {
                mantissa: 0x4000,
                exponent: -14
            }
        );
        assert_eq!(
            Scaled::normalized(-2, 3),
            Scaled {
                mantissa: -32768,
                exponent: -11
            }
        );
        // 0 and -1 have fifteen spare bits each.
        assert_eq!(Scaled::normalized(0, 0).exponent, -15);
        assert_eq!(
            Scaled::normalized(-1, 0),
            Scaled {
                mantissa: -32768,
                exponent: -15
            }
        );
        // An already normalised mantissa stays.
        assert_eq!(Scaled::normalized(0x4000, 5).exponent, 5);
        assert_eq!(Scaled::normalized(-32768, 5).exponent, 5);
    }

    #[test]
    fn a_wide_value_splits_into_a_mantissa_and_places() {
        // A value already filling the high part.
        assert_eq!(split_wide(0x2000_0000), (0x4000, 0));
        // Bits of the low part move in under the high part.
        assert_eq!(split_wide(0x0000_ffff), (0x7fff, 14));
        // The high part is all sign: the low part is normalised instead.
        assert_eq!(split_wide(1), (0x4000, 29));
        assert_eq!(split_wide(-1), (-32768, 30));
        assert_eq!(split_wide(0), (0, 30));
        // Three places, four and five: the low part's top bits move in,
        // but not at four, where the ROM's slip drops them.
        assert_eq!(split_wide(0x0400_7fff), (0x4007, 3));
        assert_eq!(split_wide(0x0200_7fff), (0x4000, 4));
        assert_eq!(split_wide(0x0100_7fff), (0x401f, 5));
    }

    #[test]
    fn a_scaled_number_becomes_a_word() {
        let word = |mantissa, exponent| Scaled { mantissa, exponent }.to_word();
        assert_eq!(word(0x1234, 0), 0x1234);
        assert_eq!(word(0x4000, -1), 0x2000);
        assert_eq!(word(-0x4000, -2), -0x1000);
        // Too large: pinned, by sign; 0 stays 0.
        assert_eq!(word(0x4000, 1), 32767);
        assert_eq!(word(-0x4000, 9), -32767);
        assert_eq!(word(0, 3), 0);
        assert_eq!(word(0x4000, -4), 0x0400);
        // The same count from either side of the ROM's table.
        assert_eq!(moved_down(0x4000, 10), 0x0010);
        assert_eq!(moved_down(0x4000, -10), 0x0010);
        // Eleven places, read on the upper side, meet the ROM's slip.
        assert_eq!(moved_down(0x4000, 11), 0);
        assert_eq!(moved_down(0x4000, -11), 0x0008);
        // No places at all is a multiplication by just under 1.
        assert_eq!(moved_down(0x4000, 0), 0x3fff);
    }

    #[test]
    fn the_reciprocal_of_simple_numbers() {
        let reciprocal = |mantissa, exponent| Scaled { mantissa, exponent }.reciprocal();
        // 1 / 1/2 = "1" one place up.
        assert_eq!(
            reciprocal(0x4000, 0),
            Scaled {
                mantissa: 0x7fff,
                exponent: 1
            }
        );
        // 1 / -1/2 = -2, written -1/2 x 2^2.
        assert_eq!(
            reciprocal(-0x4000, 0),
            Scaled {
                mantissa: -0x4000,
                exponent: 2
            }
        );
        // 1 / 0: the largest number there is.
        assert_eq!(
            reciprocal(0, 0),
            Scaled {
                mantissa: 0x7fff,
                exponent: 0x2f
            }
        );
        // 1 / 3/4 = 4/3 = 2/3 x 2, to the table's accuracy.
        let two_thirds = reciprocal(0x6000, 0);
        assert_eq!(two_thirds.exponent, 1);
        assert!((i32::from(two_thirds.mantissa) - 0x5555).abs() <= 2);
    }

    #[test]
    fn sine_and_cosine_at_the_quarter_turns() {
        assert_eq!((sine(0), cosine(0)), (0, 0x7fff));
        assert_eq!((sine(0x4000), cosine(0x4000)), (0x7fff, 0));
        assert_eq!(sine(-0x2000), -sine(0x2000));
        assert_eq!(cosine(-0x2000), cosine(0x2000));
        // The half turn: sine 0, cosine -1.
        assert_eq!((sine(i16::MIN), cosine(i16::MIN)), (0, i16::MIN));
        // Between two table entries the value moves with the arc.
        assert!(sine(0x0080) > sine(0) && sine(0x0080) < sine(0x0100));
    }

    #[test]
    fn the_length_of_a_vector() {
        assert_eq!(length([0, 0, 0]), 0);
        // 3-4-5, scaled up.
        let five = length([0x0300, 0x0400, 0]);
        assert!((i32::from(five) - 0x0500).abs() <= 2, "{five:#x}");
        // A single component comes back a bit short of itself.
        assert_eq!(length([0x1000, 0, 0]), 0x0fff);
    }
}
