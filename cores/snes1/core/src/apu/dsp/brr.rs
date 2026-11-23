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

/// How a sample's last block ends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum BrrEnd {
    /// Its end block loops: the voice goes on from the loop address.
    Loops,
    /// Its end block does not loop: the voice falls silent on it.
    Stops,
    /// No end block in the 64 KiB a sample can run through.
    Unended,
}

/// A sample decoded as the S-DSP decodes it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BrrSample {
    /// Its values, sixteen a block, the end block's included, in the DSP's
    /// own form: each a 15-bit sample doubled, as the interpolator reads
    /// it.
    pub pcm: Vec<i16>,
    /// The blocks read.
    pub blocks: usize,
    /// The first value of the block the loop address names, when that
    /// address is one of the sample's blocks.
    pub loop_sample: Option<usize>,
    pub ends: BrrEnd,
}

/// The most blocks a sample can run through: 64 KiB of nine-byte blocks.
const MAX_BLOCKS: usize = 0x10000 / 9 + 1;

/// Decode the sample at `start` of the 64 KiB `aram` (`loop_start` the
/// loop address its directory entry gives) as a voice decodes it, from
/// silence: block after block, each through the same `decode_four` a voice
/// uses, until an end block, the addresses wrapping at 64 KiB.
pub fn decode_sample(aram: &[u8], start: u16, loop_start: u16) -> BrrSample {
    let byte = |at: u16| aram[usize::from(at)];
    let mut ring = SampleRing::EMPTY;
    let mut sample = BrrSample {
        pcm: Vec::new(),
        blocks: 0,
        loop_sample: None,
        ends: BrrEnd::Unended,
    };
    let mut address = start;
    while sample.blocks < MAX_BLOCKS {
        if address == loop_start && sample.loop_sample.is_none() {
            sample.loop_sample = Some(sample.pcm.len());
        }
        let header = BlockHeader::decode(byte(address));
        for pair in 0..4u16 {
            let first = byte(address.wrapping_add(1 + pair * 2));
            let second = byte(address.wrapping_add(2 + pair * 2));
            decode_four(&mut ring, header, [first, second]);
            sample
                .pcm
                .extend((-4..0).map(|offset| ring.at(offset) as i16));
        }
        sample.blocks += 1;
        if header.ends {
            sample.ends = match header.loops {
                true => BrrEnd::Loops,
                false => BrrEnd::Stops,
            };
            break;
        }
        address = address.wrapping_add(9);
    }
    sample
}

/// One entry of the sample directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct DirectoryEntry {
    /// The source number a voice names it by.
    pub source: u8,
    /// Where its first block is.
    pub start: u16,
    /// Where a looping end block sends the voice.
    pub loop_start: u16,
    /// The voices whose source number names it, bit `n` for voice `n`.
    pub voices: u8,
}

/// The sample directory at page `page` of the 64 KiB `aram`: each of the
/// 256 source numbers' start and loop addresses (four bytes an entry, low
/// byte first, the addresses wrapping at 64 KiB), and the voices `sources`
/// (each voice's source number) name it.
pub fn directory(aram: &[u8], page: u8, sources: [u8; 8]) -> Vec<DirectoryEntry> {
    let word = |at: u16| {
        u16::from_le_bytes([aram[usize::from(at)], aram[usize::from(at.wrapping_add(1))]])
    };
    (0..=255u8)
        .map(|source| {
            let at = (u16::from(page) << 8).wrapping_add(u16::from(source) << 2);
            DirectoryEntry {
                source,
                start: word(at),
                loop_start: word(at.wrapping_add(2)),
                voices: sources
                    .iter()
                    .enumerate()
                    .filter(|(_, &named)| named == source)
                    .fold(0, |voices, (voice, _)| voices | 1 << voice),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sample of three blocks at `$1000`: filter 0 shift 12, filter 1
    /// shift 4 (the loop block), and filter 3 shift 8, the end, its loop
    /// flag `end`'s.
    fn three_blocks(end: u8) -> Vec<u8> {
        let mut aram = vec![0u8; 0x10000];
        let blocks: [(u8, [u8; 8]); 3] = [
            (0xC0, [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0]),
            (0x44, [0x7F, 0x80, 0x01, 0xFF, 0x70, 0x0F, 0x88, 0x77]),
            (0x8C | end, [0x11, 0x22, 0x33, 0x44, 0xCC, 0xDD, 0xEE, 0x99]),
        ];
        for (i, (header, data)) in blocks.iter().enumerate() {
            let at = 0x1000 + i * 9;
            aram[at] = *header;
            aram[at + 1..at + 9].copy_from_slice(data);
        }
        aram
    }

    #[test]
    fn a_sample_decodes_block_by_block_as_a_voice_decodes_it() {
        let aram = three_blocks(0x03);
        let sample = decode_sample(&aram, 0x1000, 0x1009);
        assert_eq!(sample.blocks, 3);
        assert_eq!(sample.pcm.len(), 48);
        assert_eq!(sample.loop_sample, Some(16), "the second block");
        assert_eq!(sample.ends, BrrEnd::Loops);
        // The first block, filter 0 shift 12: each nibble << 12 >> 1, then
        // doubled: 1 -> 0x1000, 2 -> 0x2000, 7 -> 0x7000, 8 (-8) -> -0x8000,
        // F (-1) -> -0x1000.
        assert_eq!(&sample.pcm[..4], &[0x1000, 0x2000, 0x3000, 0x4000]);
        assert_eq!(&sample.pcm[6..8], &[0x7000, -0x8000]);
        assert_eq!(&sample.pcm[14..16], &[-0x1000, 0]);
        // Every value is the DSP's doubled form.
        assert!(sample.pcm.iter().all(|v| v & 1 == 0));
        // The same values a voice's ring holds, decoding the same bytes.
        let mut ring = SampleRing::EMPTY;
        let mut voice = Vec::new();
        for block in 0..3 {
            let at = 0x1000 + block * 9;
            let header = BlockHeader::decode(aram[at]);
            for pair in 0..4 {
                decode_four(
                    &mut ring,
                    header,
                    [aram[at + 1 + pair * 2], aram[at + 2 + pair * 2]],
                );
                voice.extend((-4..0).map(|o| ring.at(o) as i16));
            }
        }
        assert_eq!(sample.pcm, voice);
        assert_eq!(
            retro_core::hash::fnv1a64(
                &sample
                    .pcm
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<u8>>()
            ),
            0xFF4F_49DA_8385_F1C8,
            "pinned"
        );
    }

    #[test]
    fn a_sample_stops_on_an_end_without_loop_and_names_no_outside_loop() {
        let aram = three_blocks(0x01);
        let sample = decode_sample(&aram, 0x1000, 0x2000);
        assert_eq!(
            (sample.blocks, sample.ends, sample.loop_sample),
            (3, BrrEnd::Stops, None)
        );
        let silent = vec![0u8; 0x10000];
        let unended = decode_sample(&silent, 0x0000, 0x0000);
        assert_eq!(unended.ends, BrrEnd::Unended);
        assert_eq!(unended.blocks, MAX_BLOCKS);
        assert_eq!(unended.loop_sample, Some(0));
    }

    #[test]
    fn every_shift_and_filter_follows_the_chip_s_arithmetic() {
        // One block a shift and filter, the nibbles 7, -8, 1, -1 ...: the
        // values pinned as the chip's arithmetic gives them (shifts 13-15
        // collapse to 0 or -2048 before the filter).
        let mut aram = vec![0u8; 0x10000];
        let mut at = 0x2000usize;
        for shift in 0..16u8 {
            for filter in 0..4u8 {
                aram[at] = shift << 4 | filter << 2;
                aram[at + 1..at + 9]
                    .copy_from_slice(&[0x78, 0x1F, 0x7F, 0x80, 0x08, 0xF7, 0x31, 0xC4]);
                at += 9;
            }
        }
        aram[at - 9] |= 0x01;
        let sample = decode_sample(&aram, 0x2000, 0x2000);
        assert_eq!(sample.blocks, 64);
        assert_eq!(sample.ends, BrrEnd::Stops);
        // Shift 13, filter 0 (block 52): 7 collapses to 0, -8 to -2048,
        // doubled to -0x1000; shift 12, filter 0 (block 48): 7 << 12 >> 1,
        // doubled, is 0x7000.
        assert_eq!(&sample.pcm[52 * 16..52 * 16 + 2], &[0, -0x1000]);
        assert_eq!(sample.pcm[48 * 16], 0x7000);
        assert_eq!(
            retro_core::hash::fnv1a64(
                &sample
                    .pcm
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<u8>>()
            ),
            0x5854_B461_281B_725B,
            "pinned"
        );
    }

    #[test]
    fn the_directory_gives_each_source_s_addresses_and_its_voices() {
        let mut aram = vec![0u8; 0x10000];
        aram[0x0300..0x0308].copy_from_slice(&[0x00, 0x10, 0x09, 0x10, 0x34, 0x12, 0x78, 0x56]);
        // Page 0xFF wraps: source 0x40's entry is at 0xFF00 + 0x100 = 0x0000.
        aram[0x0000..0x0004].copy_from_slice(&[0xCD, 0xAB, 0x01, 0xEF]);
        let entries = directory(&aram, 0x03, [1, 0, 1, 7, 0, 0, 0, 2]);
        assert_eq!(entries.len(), 256);
        assert_eq!(
            entries[0],
            DirectoryEntry {
                source: 0,
                start: 0x1000,
                loop_start: 0x1009,
                voices: 0b0111_0010
            }
        );
        assert_eq!(
            (entries[1].start, entries[1].loop_start, entries[1].voices),
            (0x1234, 0x5678, 0b101)
        );
        let wrapped = directory(&aram, 0xFF, [0; 8]);
        assert_eq!(
            (wrapped[0x40].start, wrapped[0x40].loop_start),
            (0xABCD, 0xEF01)
        );
    }

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
