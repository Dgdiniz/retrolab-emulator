//! BRR, the S-DSP's sample format, and the ring each voice decodes into.
//!
//! A sample is a chain of 9-byte blocks: a header byte, then eight bytes
//! holding sixteen 4-bit values, high nibble first. The header carries
//! the block's shift (bits 4-7), its filter (bits 2-3), a loop flag
//! (bit 1) and an end flag (bit 0). Each value is sign-extended, shifted
//! left by the shift (a shift of 13-15 is invalid and collapses the
//! value to 0 or -2048), then one of four predictors adds a weighted
//! sum of the two samples decoded before it, the chip's own fractions,
//! in the chip's own integer arithmetic:
//!
//! | filter | adds                          |
//! |--------|-------------------------------|
//! | 0      | nothing                       |
//! | 1      | 15/16 of the last sample      |
//! | 2      | 61/32 last - 15/16 before it  |
//! | 3      | 115/64 last - 13/16 before it |
//!
//! The result is clamped to 16 bits and then doubled with 16-bit
//! wrap-around, which leaves a 15-bit sample in a 16-bit slot with bit 0
//! clear; the predictors read their inputs from that form. Four values
//! are decoded per call (one call per voice per sample, when the voice
//! has used up the four it had), into a ring of twelve that the
//! interpolator reads from.
//!
//! Written from fullsnes's description of the BRR format; its output was
//! checked against bsnes-plus.

/// The sixteen-bit sample values a voice interpolates between: a ring
/// of twelve, read at any offset around the write head, the predictors
/// look two samples back, the interpolator up to three ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SampleRing {
    samples: [i16; 12],
    /// Where the next decoded sample goes.
    head: u8,
}

impl SampleRing {
    pub(super) const LEN: usize = 12;

    pub(super) const EMPTY: SampleRing = SampleRing {
        samples: [0; 12],
        head: 0,
    };

    /// The sample `offset` slots from the write head, in either direction.
    #[inline]
    pub(super) fn at(&self, offset: i32) -> i32 {
        let index = (i32::from(self.head) + offset).rem_euclid(Self::LEN as i32);
        i32::from(self.samples[index as usize])
    }

    /// Append one decoded sample and advance the head.
    #[inline]
    pub(super) fn push(&mut self, sample: i16) {
        self.samples[usize::from(self.head)] = sample;
        self.head = if usize::from(self.head) + 1 == Self::LEN {
            0
        } else {
            self.head + 1
        };
    }

    /// Point the head at slot 0 without clearing anything, a key-on
    /// restarts the ring but the stale samples stay readable.
    pub(super) fn rewind(&mut self) {
        self.head = 0;
    }

    /// The raw slots and head, for the savestate.
    pub(super) fn parts(&self) -> ([i16; 12], u8) {
        (self.samples, self.head)
    }

    pub(super) fn from_parts(samples: [i16; 12], head: u8) -> Self {
        Self {
            samples,
            head: head % Self::LEN as u8,
        }
    }
}

/// A block header, decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BlockHeader {
    /// Bits 4-7: how far left each 4-bit value is shifted.
    pub shift: u8,
    /// Bits 2-3: which predictor the block uses.
    pub filter: u8,
    /// Bit 1: at the end of this block, continue from the loop address.
    pub loops: bool,
    /// Bit 0: this is the sample's last block.
    pub ends: bool,
}

impl BlockHeader {
    pub(super) fn decode(byte: u8) -> Self {
        Self {
            shift: byte >> 4,
            filter: (byte >> 2) & 3,
            loops: byte & 2 != 0,
            ends: byte & 1 != 0,
        }
    }

    /// An end block that does not loop: the voice falls silent on it.
    pub(super) fn ends_without_loop(self) -> bool {
        self.ends && !self.loops
    }
}

/// One 4-bit value through the shift and the predictor, given the two
/// samples decoded before it (`last` as stored, `before` as stored).
#[inline]
fn predict(value: i32, header: BlockHeader, last: i32, before: i32) -> i16 {
    let mut s = if header.shift <= 12 {
        (value << header.shift) >> 1
    } else {
        value & !0x7ff
    };
    // The predictors work on the stored form: `last` is the doubled
    // sample, `before` is halved back to its 15 bits first.
    let before = before >> 1;
    match header.filter {
        0 => {}
        1 => {
            s += last >> 1;
            s += (-last) >> 5;
        }
        2 => {
            s += last;
            s -= before;
            s += before >> 4;
            s += (last * -3) >> 6;
        }
        _ => {
            s += last;
            s -= before;
            s += (last * -13) >> 7;
            s += (before * 3) >> 4;
        }
    }
    (s.clamp(-0x8000, 0x7fff) << 1) as i16
}

/// Decode four values, two data bytes, high nibble first, into the
/// ring, each through the header's shift and predictor.
pub(super) fn decode_four(ring: &mut SampleRing, header: BlockHeader, bytes: [u8; 2]) {
    let mut nibbles = u16::from_be_bytes(bytes);
    for _ in 0..4 {
        // The top nibble, sign-extended.
        let value = i32::from(nibbles as i16) >> 12;
        nibbles <<= 4;
        let sample = predict(value, header, ring.at(-1), ring.at(-2));
        ring.push(sample);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(header: u8, bytes: [u8; 2]) -> [i32; 4] {
        let mut ring = SampleRing::EMPTY;
        decode_four(&mut ring, BlockHeader::decode(header), bytes);
        [ring.at(-4), ring.at(-3), ring.at(-2), ring.at(-1)]
    }

    #[test]
    fn the_header_splits_into_its_fields() {
        let h = BlockHeader::decode(0b1011_0111);
        assert_eq!(h.shift, 11);
        assert_eq!(h.filter, 1);
        assert!(h.loops);
        assert!(h.ends);
        assert!(!h.ends_without_loop());
        assert!(BlockHeader::decode(0x01).ends_without_loop());
        assert!(!BlockHeader::decode(0x02).ends_without_loop());
    }

    #[test]
    fn filter_0_only_shifts() {
        // Shift 8: 7 << 8 = 1792, halved to 896, doubled back to 1792.
        assert_eq!(decoded(0x80, [0x77, 0x77]), [1792; 4]);
        // Negative values sign-extend: -1 << 8 = -256 -> -128 -> -256.
        assert_eq!(decoded(0x80, [0xff, 0xff]), [-256; 4]);
    }

    #[test]
    fn an_invalid_shift_collapses_the_value() {
        assert_eq!(decoded(0xf0, [0x77, 0x77]), [0; 4]);
        assert_eq!(decoded(0xd0, [0x88, 0x88]), [-4096; 4]);
    }

    #[test]
    fn filter_1_adds_fifteen_sixteenths_of_the_last_sample() {
        let mut ring = SampleRing::EMPTY;
        ring.push(64);
        // Zero input: 64 >> 1 = 32, plus (-64) >> 5 = -2, doubled -> 60.
        decode_four(&mut ring, BlockHeader::decode(0x04), [0, 0]);
        assert_eq!(ring.at(-4), 60);
    }

    #[test]
    fn the_result_is_clamped_before_it_is_doubled() {
        // Shift 12, value 7: 7 << 12 = 28672, halved to 14336. Filter 2
        // over a last sample of 32766 and a sample before it of -32768
        // (read halved, -16384) adds 32766 + 16384 - 1024 - 1536: 60926,
        // past 16 bits. Clamped to 0x7fff and doubled, that is -2 in a
        // 16-bit slot.
        let mut ring = SampleRing::EMPTY;
        ring.push(-32768);
        ring.push(32766);
        decode_four(&mut ring, BlockHeader::decode(0xc8), [0x77, 0x77]);
        assert_eq!(ring.at(-4), -2);
    }

    #[test]
    fn the_ring_wraps_both_ways() {
        let mut ring = SampleRing::EMPTY;
        for i in 0..14 {
            ring.push(i);
        }
        assert_eq!(ring.parts().1, 2);
        assert_eq!(ring.at(-1), 13);
        assert_eq!(ring.at(-2), 12);
        assert_eq!(ring.at(0), 2);
        assert_eq!(ring.at(3), 5);
        assert_eq!(ring.at(10), 12);
        let (samples, head) = ring.parts();
        assert_eq!(SampleRing::from_parts(samples, head), ring);
    }
}
