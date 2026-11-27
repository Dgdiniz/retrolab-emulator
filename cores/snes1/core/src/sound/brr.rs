//! A BRR sample as files: the `.brr` of its blocks, as they sit in ARAM
//! (whole 9-byte blocks, which BRRtools reads, its loop given with `-l`) or
//! after the 2-byte loop header AddmusicK reads (the loop block's place in
//! bytes from the sample's start, low byte first); the WAV of its values at
//! the S-DSP's 32 kHz, the rate a pitch of 0x1000 plays it at, its loop
//! played as a voice plays it until it repeats, the passes it repeats in a
//! `smpl` chunk; and what `sample.json` says of it.

use retro_core::audio::{encode_wav_samples, wav_with_loop, SoundSamples};
use serde::Serialize;

use crate::apu::{decode_sample, loop_passes, BrrEnd, BrrSample, DirectoryEntry, LoopPasses};

/// The S-DSP's sample rate.
const DSP_RATE: u32 = 32_000;

/// The sample's blocks as they sit in the 64 KiB `aram` from its start,
/// nine bytes each, the addresses wrapping at 64 KiB.
pub fn brr_blocks(aram: &[u8], start: u16, sample: &BrrSample) -> Vec<u8> {
    (0..sample.blocks * 9)
        .map(|i| aram[usize::from(start.wrapping_add(i as u16))])
        .collect()
}

/// The sample's `.brr` with the loop header: the loop block's place in
/// bytes from the start (0 when its loop names none of its blocks), then
/// its blocks.
pub fn brr_with_loop_header(aram: &[u8], start: u16, sample: &BrrSample) -> Vec<u8> {
    let offset = sample.loop_sample.map_or(0, |value| value / 16 * 9) as u16;
    let mut file = offset.to_le_bytes().to_vec();
    file.extend_from_slice(&brr_blocks(aram, start, sample));
    file
}

/// The sample's values as a mono 16-bit WAV at 32 kHz, each as the DSP
/// holds it (a 15-bit sample doubled, so at full scale). A sample whose end
/// block loops back to one of its blocks has `passes`, its loop as a voice
/// plays it ([`loop_passes`]): its later passes follow its values, and the
/// passes the voice repeats for ever are the loop in a `smpl` chunk, the
/// last pass when none had repeated.
pub fn sample_wav(sample: &BrrSample, passes: Option<&LoopPasses>) -> Vec<u8> {
    match (sample.loop_sample, passes) {
        (Some(start), Some(passes)) if sample.ends == BrrEnd::Loops => {
            let length = sample.pcm.len() - start;
            let mut values = sample.pcm.clone();
            values.extend_from_slice(&passes.later);
            let end = values.len() as u32;
            let repeated = (passes.cycle.unwrap_or(1) * length) as u32;
            let wav = encode_wav_samples(&SoundSamples::I16(values), DSP_RATE, 1);
            wav_with_loop(wav, DSP_RATE, end - repeated, end - 1)
        }
        _ => encode_wav_samples(&SoundSamples::I16(sample.pcm.clone()), DSP_RATE, 1),
    }
}

/// What `sample.json` says of a sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SampleInfo {
    /// The source number the voices name it by.
    pub source: u8,
    /// The address of the directory it was read from.
    pub directory: u16,
    pub start: u16,
    pub loop_start: u16,
    pub blocks: usize,
    pub ends: BrrEnd,
    /// The first value of the loop block, when the loop names one of the
    /// sample's blocks.
    pub loop_sample: Option<usize>,
    /// The passes of the loop its WAV holds: the first, which its own
    /// values end with, and those a voice plays after it until the loop
    /// repeats; 0 for a sample that does not loop back to one of its
    /// blocks.
    pub loop_passes: usize,
    /// How many of those passes a voice repeats for ever, the WAV's loop: 1
    /// when the last repeats itself, more for a cycle of passes; `None`
    /// when none had repeated within the decoder's cap (the WAV's loop is
    /// then its last pass) or the sample does not loop.
    pub loop_cycle: Option<usize>,
    /// The pitches it was keyed on at, each once, in order.
    pub pitches: Vec<u32>,
    /// The voices that keyed it on, each once, in order.
    pub voices: Vec<u8>,
}

impl SampleInfo {
    /// The text of `sample.json`: these fields as JSON, indented.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a sample's description is JSON")
    }

    /// The sample `entry` of the directory at `directory`, decoded as
    /// `sample`, its loop's `passes` as a voice plays it ([`loop_passes`]),
    /// keyed on at `pitches` by `voices`.
    pub fn of(
        directory: u16,
        entry: &DirectoryEntry,
        sample: &BrrSample,
        passes: Option<&LoopPasses>,
        pitches: Vec<u32>,
        voices: Vec<u8>,
    ) -> Self {
        Self {
            source: entry.source,
            directory,
            start: entry.start,
            loop_start: entry.loop_start,
            blocks: sample.blocks,
            ends: sample.ends,
            loop_sample: sample.loop_sample,
            loop_passes: passes.map_or(0, |passes| passes.passes),
            loop_cycle: passes.and_then(|passes| passes.cycle),
            pitches,
            voices,
        }
    }
}

/// A sample's files: its blocks as a `.brr`, raw and after the loop
/// header; its values as a WAV with its loop; and what `sample.json` says
/// of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleFiles {
    /// The blocks as they sit in ARAM ([`brr_blocks`]).
    pub brr: Vec<u8>,
    /// The blocks after the loop header ([`brr_with_loop_header`]).
    pub brr_looped: Vec<u8>,
    /// The values as a WAV at 32 kHz with its loop as a voice plays it
    /// ([`sample_wav`]).
    pub wav: Vec<u8>,
    /// What `sample.json` says of it ([`SampleInfo::to_json`] writes it).
    pub info: SampleInfo,
}

impl SampleFiles {
    /// The files of the sample `entry` of the directory at `directory`
    /// names in the 64 KiB `aram`, decoded as a voice decodes it, keyed on
    /// at `pitches` by `voices`.
    pub fn of(
        aram: &[u8],
        directory: u16,
        entry: &DirectoryEntry,
        pitches: Vec<u32>,
        voices: Vec<u8>,
    ) -> Self {
        let sample = decode_sample(aram, entry.start, entry.loop_start);
        let passes = loop_passes(aram, entry.loop_start, &sample);
        Self {
            brr: brr_blocks(aram, entry.start, &sample),
            brr_looped: brr_with_loop_header(aram, entry.start, &sample),
            wav: sample_wav(&sample, passes.as_ref()),
            info: SampleInfo::of(directory, entry, &sample, passes.as_ref(), pitches, voices),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aram() -> Vec<u8> {
        let mut aram = vec![0u8; 0x10000];
        aram[0xFFF7] = 0xB0;
        aram[0xFFF8..0x10000].copy_from_slice(&[0x11; 8]);
        aram[0x0000] = 0xB3;
        aram[0x0001..0x0009].copy_from_slice(&[0x77; 8]);
        aram
    }

    #[test]
    fn the_brr_files_hold_the_blocks_as_they_sit_in_aram() {
        let aram = aram();
        let sample = decode_sample(&aram, 0xFFF7, 0x0000);
        assert_eq!((sample.blocks, sample.loop_sample), (2, Some(16)));
        let blocks = brr_blocks(&aram, 0xFFF7, &sample);
        assert_eq!(blocks.len(), 18);
        assert_eq!(
            &blocks[..9],
            &aram[0xFFF7..0x10000],
            "up to the end of ARAM"
        );
        assert_eq!(&blocks[9..], &aram[0x0000..0x0009], "then wrapping");
        let headed = brr_with_loop_header(&aram, 0xFFF7, &sample);
        assert_eq!(&headed[..2], &[9, 0], "the loop block 9 bytes in");
        assert_eq!(&headed[2..], &blocks[..]);
    }

    #[test]
    fn the_files_are_the_writers_of_the_sample_the_entry_names() {
        let aram = aram();
        let entry = DirectoryEntry {
            source: 7,
            start: 0xFFF7,
            loop_start: 0x0000,
            voices: 0b1001,
        };
        let files = SampleFiles::of(&aram, 0x0400, &entry, vec![0x1000, 0x0800], vec![0, 3]);
        let sample = decode_sample(&aram, 0xFFF7, 0x0000);
        let passes = loop_passes(&aram, 0x0000, &sample);
        assert_eq!(files.brr, brr_blocks(&aram, 0xFFF7, &sample));
        assert_eq!(
            files.brr_looped,
            brr_with_loop_header(&aram, 0xFFF7, &sample)
        );
        assert_eq!(files.wav, sample_wav(&sample, passes.as_ref()));
        assert_eq!(
            files.info,
            SampleInfo::of(
                0x0400,
                &entry,
                &sample,
                passes.as_ref(),
                vec![0x1000, 0x0800],
                vec![0, 3]
            )
        );
        assert_eq!(
            (files.info.source, files.info.directory, files.info.blocks),
            (7, 0x0400, 2)
        );
        assert_eq!(
            (files.info.loop_passes, files.info.loop_cycle),
            (1, Some(1))
        );
    }

    #[test]
    fn the_wav_holds_the_values_and_its_loop() {
        // A loop of filter 0, which a voice plays the same on every pass.
        let aram = aram();
        let sample = decode_sample(&aram, 0xFFF7, 0x0000);
        let wav = sample_wav(&sample, loop_passes(&aram, 0x0000, &sample).as_ref());
        assert_eq!(i16::from_le_bytes([wav[46], wav[47]]), sample.pcm[1]);
        let smpl = 44 + 64;
        assert_eq!(&wav[smpl..smpl + 4], b"smpl");
        assert_eq!(
            &wav[smpl + 52..smpl + 60],
            &[16, 0, 0, 0, 31, 0, 0, 0],
            "loop 16 to 31"
        );
        assert_eq!(
            u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize,
            wav.len() - 8
        );
        let mut stops = sample.clone();
        stops.ends = BrrEnd::Stops;
        assert_eq!(sample_wav(&stops, None).len(), 44 + 64, "no loop, no smpl");
    }

    /// A sample at `$1000`: a block of filter 0 whose values are all
    /// 0x7000, then a looping block of `header` and `data`, the loop.
    fn entered_at_0x7000(header: u8, data: [u8; 8]) -> (Vec<u8>, BrrSample) {
        let mut aram = vec![0u8; 0x10000];
        aram[0x1000] = 0xC0;
        aram[0x1001..0x1009].copy_from_slice(&[0x77; 8]);
        aram[0x1009] = header | 0x03;
        aram[0x100A..0x1012].copy_from_slice(&data);
        let sample = decode_sample(&aram, 0x1000, 0x1009);
        (aram, sample)
    }

    /// The values a WAV of this module holds, its loop's first and last
    /// sample frame, and its RIFF size against its length.
    fn read_wav(wav: &[u8]) -> (Vec<i16>, u32, u32) {
        let word = |at: usize| u32::from_le_bytes(wav[at..at + 4].try_into().unwrap());
        assert_eq!(word(4) as usize, wav.len() - 8);
        let data = word(40) as usize;
        let values = wav[44..44 + data]
            .chunks(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let smpl = 44 + data;
        assert_eq!(&wav[smpl..smpl + 4], b"smpl");
        (values, word(smpl + 52), word(smpl + 56))
    }

    #[test]
    fn the_wav_plays_the_loop_as_a_voice_plays_it() {
        // Filter 1 over silence, entered at 0x7000: nine passes, the ninth
        // the one the voice repeats.
        let (aram, sample) = entered_at_0x7000(0x04, [0; 8]);
        let passes = loop_passes(&aram, 0x1009, &sample).unwrap();
        let (values, first, last) = read_wav(&sample_wav(&sample, Some(&passes)));
        assert_eq!(&values[..32], &sample.pcm[..]);
        assert_eq!(&values[32..], &passes.later[..]);
        assert_eq!(values.len(), 16 + 9 * 16);
        assert_eq!((first, last), (16 + 8 * 16, 16 + 9 * 16 - 1));
        // A cycle of two passes: the loop is both.
        let (aram, sample) =
            entered_at_0x7000(0xC4, [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0]);
        let passes = loop_passes(&aram, 0x1009, &sample).unwrap();
        assert_eq!((passes.passes, passes.cycle), (9, Some(2)));
        let (values, first, last) = read_wav(&sample_wav(&sample, Some(&passes)));
        assert_eq!((first, last), (16 + 7 * 16, 16 + 9 * 16 - 1));
        assert_eq!(values.len(), 16 + 9 * 16);
        // Passes that had not repeated: the loop is the last of them.
        let unsettled = LoopPasses {
            cycle: None,
            ..passes.clone()
        };
        let (_, first, last) = read_wav(&sample_wav(&sample, Some(&unsettled)));
        assert_eq!((first, last), (16 + 8 * 16, 16 + 9 * 16 - 1));
    }

    #[test]
    fn the_description_says_the_passes_and_the_cycle() {
        // Shift 10 and filter 1: eight passes, the last three a cycle.
        let (aram, _) = entered_at_0x7000(0xA4, [0x7F; 8]);
        let entry = DirectoryEntry {
            source: 2,
            start: 0x1000,
            loop_start: 0x1009,
            voices: 0,
        };
        let files = SampleFiles::of(&aram, 0x0200, &entry, vec![0x1000], vec![5]);
        assert_eq!(
            (files.info.loop_passes, files.info.loop_cycle),
            (8, Some(3))
        );
        let json = files.info.to_json();
        assert!(json.contains("\"loop_sample\": 16,\n  \"loop_passes\": 8,\n  \"loop_cycle\": 3,"));
        // A loop to none of its blocks: no passes.
        let outside = SampleInfo::of(
            0x0200,
            &entry,
            &decode_sample(&aram, 0x1000, 0x2000),
            None,
            Vec::new(),
            Vec::new(),
        );
        assert_eq!((outside.loop_passes, outside.loop_cycle), (0, None));
        assert!(outside
            .to_json()
            .contains("\"loop_passes\": 0,\n  \"loop_cycle\": null,"));
    }
}
