//! A BRR sample as files: the `.brr` of its blocks, as they sit in ARAM or
//! after the 2-byte loop header AddmusicK and BRRtools read (the loop
//! block's place in bytes from the sample's start, low byte first); the WAV
//! of its values at the S-DSP's 32 kHz, the rate a pitch of 0x1000 plays it
//! at, its loop in a `smpl` chunk; and what `sample.json` says of it.

use retro_core::audio::{encode_wav_samples, wav_with_loop, SoundSamples};
use serde::Serialize;

use crate::apu::{decode_sample, BrrEnd, BrrSample, DirectoryEntry};

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
/// holds it (a 15-bit sample doubled, so at full scale), with its loop in a
/// `smpl` chunk when its end block loops back to one of its blocks.
pub fn sample_wav(sample: &BrrSample) -> Vec<u8> {
    let wav = encode_wav_samples(&SoundSamples::I16(sample.pcm.clone()), DSP_RATE, 1);
    match (sample.ends, sample.loop_sample) {
        (BrrEnd::Loops, Some(start)) if !sample.pcm.is_empty() => {
            wav_with_loop(wav, DSP_RATE, start as u32, sample.pcm.len() as u32 - 1)
        }
        _ => wav,
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
    /// `sample`, keyed on at `pitches` by `voices`.
    pub fn of(
        directory: u16,
        entry: &DirectoryEntry,
        sample: &BrrSample,
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
    /// The values as a WAV at 32 kHz with its loop ([`sample_wav`]).
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
        Self {
            brr: brr_blocks(aram, entry.start, &sample),
            brr_looped: brr_with_loop_header(aram, entry.start, &sample),
            wav: sample_wav(&sample),
            info: SampleInfo::of(directory, entry, &sample, pitches, voices),
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
        assert_eq!(files.brr, brr_blocks(&aram, 0xFFF7, &sample));
        assert_eq!(
            files.brr_looped,
            brr_with_loop_header(&aram, 0xFFF7, &sample)
        );
        assert_eq!(files.wav, sample_wav(&sample));
        assert_eq!(
            files.info,
            SampleInfo::of(0x0400, &entry, &sample, vec![0x1000, 0x0800], vec![0, 3])
        );
        assert_eq!(
            (files.info.source, files.info.directory, files.info.blocks),
            (7, 0x0400, 2)
        );
    }

    #[test]
    fn the_wav_holds_the_values_and_its_loop() {
        let sample = decode_sample(&aram(), 0xFFF7, 0x0000);
        let wav = sample_wav(&sample);
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
        assert_eq!(sample_wav(&stops).len(), 44 + 64, "no loop, no smpl");
    }
}
