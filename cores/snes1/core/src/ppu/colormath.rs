//! Pure colour-math arithmetic for the compositor.
//!
//! The SNES colour-math unit adds or subtracts two 15-bit BGR colours
//! channel-by-channel, optionally halving the result. Addition
//! saturates each 5-bit channel at 31; subtraction clamps at 0.
//!
//! These free functions do the arithmetic channel by channel, which
//! is how fullsnes describes it; an emulator can do all three channels
//! in one machine word with carry masks, and the two forms give the
//! same results (checked against bsnes-plus): `(x + y) >> 1` already
//! floors, and `saturating_sub(..) >> 1` is the masked-then-shifted
//! result.

#[inline]
fn channels(c: u16) -> (u16, u16, u16) {
    (c & 0x1F, (c >> 5) & 0x1F, (c >> 10) & 0x1F)
}

#[inline]
fn pack(r: u16, g: u16, b: u16) -> u16 {
    (r & 0x1F) | ((g & 0x1F) << 5) | ((b & 0x1F) << 10)
}

/// `a + b` per 5-bit BGR channel. With `halve` the per-channel sum is
/// shifted right by one (no clamp is needed, `(31 + 31) >> 1 = 31`);
/// without it each channel saturates at 31.
#[must_use]
pub fn color_add(a: u16, b: u16, halve: bool) -> u16 {
    let (ar, ag, ab) = channels(a);
    let (br, bg, bb) = channels(b);
    let add = |x: u16, y: u16| {
        let sum = x + y;
        if halve {
            sum >> 1
        } else {
            sum.min(31)
        }
    };
    pack(add(ar, br), add(ag, bg), add(ab, bb))
}

/// `a - b` per 5-bit BGR channel, clamped at 0. With `halve` the
/// clamped per-channel difference is shifted right by one.
#[must_use]
pub fn color_sub(a: u16, b: u16, halve: bool) -> u16 {
    let (ar, ag, ab) = channels(a);
    let (br, bg, bb) = channels(b);
    let sub = |x: u16, y: u16| {
        let diff = x.saturating_sub(y);
        if halve {
            diff >> 1
        } else {
            diff
        }
    };
    pack(sub(ar, br), sub(ag, bg), sub(ab, bb))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bgr15(r: u16, g: u16, b: u16) -> u16 {
        (r & 0x1F) | ((g & 0x1F) << 5) | ((b & 0x1F) << 10)
    }

    #[test]
    fn add_no_overflow_clips_at_31() {
        // Two maxed colours add to 0x7FFF (each channel saturates).
        assert_eq!(color_add(0x7FFF, 0x7FFF, false), 0x7FFF);
        // Mixed: 20 + 15 = 35 -> 31.
        assert_eq!(
            color_add(bgr15(20, 0, 0), bgr15(15, 0, 0), false),
            bgr15(31, 0, 0)
        );
    }

    #[test]
    fn add_plain_sums_channels_independently() {
        assert_eq!(
            color_add(bgr15(10, 4, 1), bgr15(5, 6, 2), false),
            bgr15(15, 10, 3)
        );
    }

    #[test]
    fn add_half_divides_sum_by_two() {
        // (31 + 31) / 2 = 31.
        assert_eq!(color_add(0x7FFF, 0x7FFF, true), 0x7FFF);
        // (10 + 6) / 2 = 8 on red only.
        assert_eq!(
            color_add(bgr15(10, 0, 0), bgr15(6, 0, 0), true),
            bgr15(8, 0, 0)
        );
        // Odd sum floors: (5 + 4) / 2 = 4.
        assert_eq!(
            color_add(bgr15(5, 0, 0), bgr15(4, 0, 0), true),
            bgr15(4, 0, 0)
        );
    }

    #[test]
    fn sub_clamps_at_zero() {
        // 5 - 10 = 0 (no wrap).
        assert_eq!(
            color_sub(bgr15(5, 0, 0), bgr15(10, 0, 0), false),
            bgr15(0, 0, 0)
        );
        // 20 - 6 = 14.
        assert_eq!(
            color_sub(bgr15(20, 0, 0), bgr15(6, 0, 0), false),
            bgr15(14, 0, 0)
        );
    }

    #[test]
    fn sub_half_divides_clamped_difference() {
        // (20 - 6) / 2 = 7.
        assert_eq!(
            color_sub(bgr15(20, 0, 0), bgr15(6, 0, 0), true),
            bgr15(7, 0, 0)
        );
        // Clamp-then-halve: (5 - 10) clamps to 0, /2 = 0.
        assert_eq!(
            color_sub(bgr15(5, 0, 0), bgr15(10, 0, 0), true),
            bgr15(0, 0, 0)
        );
    }

    #[test]
    fn channels_are_independent() {
        // Each operation only touches its own channel.
        assert_eq!(
            color_sub(bgr15(31, 31, 31), bgr15(1, 2, 3), false),
            bgr15(30, 29, 28)
        );
    }
}
