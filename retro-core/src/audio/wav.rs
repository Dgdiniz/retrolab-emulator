//! The WAV writer: interleaved samples as a RIFF WAVE file, and the chunks
//! a WAV carries after its samples, its marks (a loop, and cues with their
//! labels).

use serde::Deserialize;

use crate::audio::stems::{SampleKind, SoundSamples};

/// A WAV's samples are 16-bit PCM.
const BITS_PER_SAMPLE: u16 = 16;

/// The format tag of 32-bit float samples (`WAVE_FORMAT_IEEE_FLOAT`).
const FORMAT_FLOAT: u16 = 3;

/// Interleaved `f32` samples (nominally `-1.0..=1.0`) as a 16-bit PCM WAV:
/// the 44-byte canonical header, then the samples, each clamped to
/// `[-1, 1]` before it is quantised so an out-of-range mix cannot wrap.
pub fn encode_wav(samples: &[f32], sample_rate: u32, channels: u16) -> Vec<u8> {
    let bytes_per_sample = u32::from(BITS_PER_SAMPLE / 8);
    let byte_rate = sample_rate * u32::from(channels) * bytes_per_sample;
    let block_align = channels * (BITS_PER_SAMPLE / 8);
    let data_size = samples.len() as u32 * bytes_per_sample;

    let mut out = Vec::with_capacity(44 + data_size as usize);
    // The RIFF header.
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_size).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    // The format chunk: PCM.
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
    // The data chunk.
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_size.to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// The bytes one sample of `kind` takes in a WAV.
fn sample_bytes(kind: SampleKind) -> u16 {
    match kind {
        SampleKind::I16 => 2,
        SampleKind::F32 => 4,
    }
}

/// The bytes of the header of a WAV of `kind`'s samples past its first
/// eight, which its RIFF size counts with the data and the chunks after
/// it: the `WAVE` tag, the format chunk, for floats the `fact` chunk, and
/// the data chunk's own eight.
fn riff_header_bytes(kind: SampleKind) -> u32 {
    match kind {
        SampleKind::I16 => 36,
        SampleKind::F32 => 50,
    }
}

/// The header of a WAV of `data_size` bytes of `kind`'s samples, `frames`
/// sample frames of `channels` at `sample_rate`, followed by
/// `trailer_bytes` bytes of other chunks, up to the data chunk's size:
/// 16-bit integers as 16-bit PCM (the canonical 44 bytes), 32-bit floats
/// as 32-bit float samples (a format chunk of 18 bytes and the `fact`
/// chunk of the frames a non-PCM file carries, 58 bytes).
fn header(
    kind: SampleKind,
    sample_rate: u32,
    channels: u16,
    data_size: u32,
    frames: u32,
    trailer_bytes: u32,
) -> Vec<u8> {
    let block_align = channels * sample_bytes(kind);
    let mut out = Vec::with_capacity(58);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(riff_header_bytes(kind) + data_size + trailer_bytes).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    match kind {
        SampleKind::I16 => {
            out.extend_from_slice(&16u32.to_le_bytes());
            out.extend_from_slice(&1u16.to_le_bytes());
        }
        SampleKind::F32 => {
            out.extend_from_slice(&18u32.to_le_bytes());
            out.extend_from_slice(&FORMAT_FLOAT.to_le_bytes());
        }
    }
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * u32::from(block_align)).to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&(sample_bytes(kind) * 8).to_le_bytes());
    if kind == SampleKind::F32 {
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(b"fact");
        out.extend_from_slice(&4u32.to_le_bytes());
        out.extend_from_slice(&frames.to_le_bytes());
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_size.to_le_bytes());
    out
}

/// Interleaved samples of a console's own kind as a WAV, neither
/// converted: 16-bit integers as 16-bit PCM (the canonical 44-byte header,
/// each sample as it is), 32-bit floats as 32-bit float samples (a format
/// chunk of 18 bytes and the `fact` chunk a non-PCM file carries). The
/// header is [`wav_header`]'s for samples that make whole frames.
pub fn encode_wav_samples(samples: &SoundSamples, sample_rate: u32, channels: u16) -> Vec<u8> {
    let kind = samples.kind();
    let data_size = samples.len() * usize::from(sample_bytes(kind));
    let frames = samples.len() as u32 / u32::from(channels.max(1));
    let mut out = header(kind, sample_rate, channels, data_size as u32, frames, 0);
    out.reserve(data_size);
    match samples {
        SoundSamples::I16(values) => {
            for value in values {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        SoundSamples::F32(values) => {
            for value in values {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
    }
    out
}

/// The header alone of a WAV of `frames` sample frames, each `channels`
/// interleaved samples of `kind`, at `sample_rate`, whose samples are
/// followed by `trailer_bytes` bytes of other chunks ([`wav_trailer`]'s; 0
/// for none): the bytes [`encode_wav_samples`] writes before such samples,
/// its RIFF size counting the chunks after them too. A file streamed to
/// disk is this header, then the samples' bytes, each little-endian, then
/// the trailer. `None` for no channel, or for a value the header's fields
/// cannot hold: the bytes of a sample frame in 16 bits; the bytes of a
/// second, of the data and of the file in 32.
pub fn wav_header(
    kind: SampleKind,
    sample_rate: u32,
    channels: u16,
    frames: u32,
    trailer_bytes: u32,
) -> Option<Vec<u8>> {
    let block_align = channels
        .checked_mul(sample_bytes(kind))
        .filter(|&bytes| bytes > 0)?;
    let data_size = frames.checked_mul(u32::from(block_align))?;
    let fits = sample_rate.checked_mul(u32::from(block_align)).is_some()
        && data_size
            .checked_add(riff_header_bytes(kind))
            .and_then(|bytes| bytes.checked_add(trailer_bytes))
            .is_some();
    fits.then(|| {
        header(
            kind,
            sample_rate,
            channels,
            data_size,
            frames,
            trailer_bytes,
        )
    })
}

/// The marks a WAV carries after its samples ([`wav_trailer`]): a loop,
/// and cues, each at a sample frame with a label. A host hands them as
/// `{ loop: [start, end], cues: [{ frame, label }] }`, either left out for
/// none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct WavMarks {
    /// The loop, played forward for ever: its first sample frame and its
    /// last.
    pub r#loop: Option<(u32, u32)>,
    /// The cues, in their order.
    pub cues: Vec<WavCue>,
}

/// A cue of a WAV: the sample frame it marks, and its label.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WavCue {
    pub frame: u32,
    pub label: String,
}

/// A RIFF chunk of `id` holding `body`, appended to `out`, with a pad byte
/// after a body of an odd length, which the chunk's size does not count.
fn put_chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}

/// The chunks a WAV carries after its samples for `marks`, at
/// `sample_rate`: with a loop, a `smpl` chunk naming it, played forward for
/// ever from its first sample frame to its last, its other fields saying
/// nothing (no maker, no product, middle C as the unity note, no SMPTE
/// offset); with cues, a `cue ` chunk of a point for each, numbered from 1
/// in their order, each at its sample frame of the data chunk, then a
/// `LIST` chunk of type `adtl` holding a `labl` for each, its label's
/// UTF-8 bytes ended by a zero. No mark, no byte. A file streamed to disk
/// ends with these bytes, its header counting them ([`wav_header`]).
pub fn wav_trailer(sample_rate: u32, marks: &WavMarks) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some((start, end)) = marks.r#loop {
        let period_ns = 1_000_000_000 / sample_rate.max(1);
        let body: Vec<u8> = [0, 0, period_ns, 60, 0, 0, 0, 1, 0, 0, 0, start, end, 0, 0]
            .iter()
            .flat_map(|value: &u32| value.to_le_bytes())
            .collect();
        put_chunk(&mut out, b"smpl", &body);
    }
    if !marks.cues.is_empty() {
        let mut points = (marks.cues.len() as u32).to_le_bytes().to_vec();
        let mut labels = b"adtl".to_vec();
        for (id, cue) in (1u32..).zip(&marks.cues) {
            points.extend_from_slice(&id.to_le_bytes());
            points.extend_from_slice(&cue.frame.to_le_bytes());
            points.extend_from_slice(b"data");
            for value in [0u32, 0, cue.frame] {
                points.extend_from_slice(&value.to_le_bytes());
            }
            let mut label = id.to_le_bytes().to_vec();
            label.extend_from_slice(cue.label.as_bytes());
            label.push(0);
            put_chunk(&mut labels, b"labl", &label);
        }
        put_chunk(&mut out, b"cue ", &points);
        put_chunk(&mut out, b"LIST", &labels);
    }
    out
}

/// The WAV `wav` (as [`encode_wav`] or [`encode_wav_samples`] write one)
/// with the chunks of `marks` after its data ([`wav_trailer`]) at
/// `sample_rate`; the RIFF size grows to hold them.
pub fn wav_with_marks(mut wav: Vec<u8>, sample_rate: u32, marks: &WavMarks) -> Vec<u8> {
    wav.extend(wav_trailer(sample_rate, marks));
    let riff = (wav.len() - 8) as u32;
    wav[4..8].copy_from_slice(&riff.to_le_bytes());
    wav
}

/// The WAV `wav` (as [`encode_wav`] or [`encode_wav_samples`] write one)
/// with a `smpl` chunk after its data naming one loop, played forward for
/// ever, from sample frame `start` to sample frame `end` (the last it
/// plays) at `sample_rate` ([`wav_trailer`] of that loop alone); the RIFF
/// size grows to hold it.
pub fn wav_with_loop(wav: Vec<u8>, sample_rate: u32, start: u32, end: u32) -> Vec<u8> {
    let marks = WavMarks {
        r#loop: Some((start, end)),
        cues: Vec::new(),
    };
    wav_with_marks(wav, sample_rate, &marks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(bytes: &[u8], off: usize) -> u32 {
        u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
    }

    fn u16_at(bytes: &[u8], off: usize) -> u16 {
        u16::from_le_bytes([bytes[off], bytes[off + 1]])
    }

    #[test]
    fn a_loop_is_a_smpl_chunk_after_the_data() {
        let wav = encode_wav_samples(&SoundSamples::I16(vec![1, 2, 3, 4]), 32_000, 1);
        let looped = wav_with_loop(wav.clone(), 32_000, 1, 3);
        assert_eq!(&looped[..4], b"RIFF");
        assert_eq!(u32_at(&looped, 4) as usize, looped.len() - 8);
        assert_eq!(&looped[8..wav.len()], &wav[8..]);
        let smpl = wav.len();
        assert_eq!(&looped[smpl..smpl + 4], b"smpl");
        assert_eq!(u32_at(&looped, smpl + 4), 60);
        assert_eq!(
            u32_at(&looped, smpl + 16),
            31_250,
            "the sample period in ns"
        );
        assert_eq!(u32_at(&looped, smpl + 20), 60, "middle C");
        assert_eq!(u32_at(&looped, smpl + 36), 1, "one loop");
        assert_eq!(u32_at(&looped, smpl + 48), 0, "forward");
        assert_eq!(
            (u32_at(&looped, smpl + 52), u32_at(&looped, smpl + 56)),
            (1, 3)
        );
        assert_eq!(u32_at(&looped, smpl + 64), 0, "for ever");
        assert_eq!(looped.len(), smpl + 68);
    }

    /// The `smpl` chunk of a loop from sample frame 1 to sample frame 3 at
    /// 32 kHz, byte for byte.
    #[rustfmt::skip]
    const LOOP_1_3_AT_32K: [u8; 68] = [
        b's', b'm', b'p', b'l', 60, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0,             // no maker, no product
        0x12, 0x7A, 0, 0, 60, 0, 0, 0,      // 31,250 ns a sample; middle C
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // no pitch fraction, no SMPTE
        1, 0, 0, 0, 0, 0, 0, 0,             // one loop, no sampler data
        0, 0, 0, 0, 0, 0, 0, 0,             // loop 0, played forward
        1, 0, 0, 0, 3, 0, 0, 0,             // from frame 1 to frame 3
        0, 0, 0, 0, 0, 0, 0, 0,             // no fraction, for ever
    ];

    #[test]
    fn a_looped_wav_is_its_samples_then_its_smpl_chunk_byte_for_byte() {
        let wav = encode_wav_samples(&SoundSamples::I16(vec![1, 2, 3, 4]), 32_000, 1);
        let mut expected = wav.clone();
        expected[4..8].copy_from_slice(&112u32.to_le_bytes());
        expected.extend_from_slice(&LOOP_1_3_AT_32K);
        assert_eq!(wav_with_loop(wav, 32_000, 1, 3), expected);
    }

    #[test]
    fn a_wav_header_is_the_canonical_44_bytes() {
        // Two stereo sample frames: 4 samples, 8 data bytes.
        let wav = encode_wav(&[0.0f32, 0.0, 0.0, 0.0], 44_100, 2);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u32_at(&wav, 16), 16, "the format chunk's size");
        assert_eq!(u16_at(&wav, 20), 1, "PCM");
        assert_eq!(u16_at(&wav, 22), 2, "channels");
        assert_eq!(u32_at(&wav, 24), 44_100, "sample rate");
        assert_eq!(u32_at(&wav, 28), 44_100 * 2 * 2, "byte rate");
        assert_eq!(u16_at(&wav, 32), 4, "block align: 2 channels of 2 bytes");
        assert_eq!(u16_at(&wav, 34), 16, "bits per sample");
        assert_eq!(&wav[36..40], b"data");
        let data_size = u32_at(&wav, 40);
        assert_eq!(data_size, 8);
        assert_eq!(u32_at(&wav, 4), 36 + data_size);
        assert_eq!(wav.len(), 44 + data_size as usize);
    }

    #[test]
    fn integer_samples_are_written_as_they_are() {
        let wav = encode_wav_samples(&SoundSamples::I16(vec![-32768, 1, 32767, -2]), 32_000, 2);
        assert_eq!(wav.len(), 44 + 8);
        assert_eq!(u16_at(&wav, 20), 1, "PCM");
        assert_eq!(u32_at(&wav, 28), 32_000 * 4, "byte rate");
        assert_eq!(u16_at(&wav, 34), 16);
        assert_eq!(u32_at(&wav, 40), 8);
        assert_eq!(&wav[44..], &[0x00, 0x80, 1, 0, 0xFF, 0x7F, 0xFE, 0xFF]);
        assert_eq!(
            encode_wav_samples(&SoundSamples::I16(vec![1, -1]), 44_100, 2)[..44],
            encode_wav(&[0.0, 0.0], 44_100, 2)[..44],
            "the same header as the PCM writer's"
        );
    }

    #[test]
    fn float_samples_are_written_as_floats_with_their_fact_chunk() {
        let wav = encode_wav_samples(&SoundSamples::F32(vec![0.5, -1.25, 2.0]), 44_100, 1);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32_at(&wav, 4) as usize, wav.len() - 8);
        assert_eq!(u32_at(&wav, 16), 18, "the format chunk's size");
        assert_eq!(u16_at(&wav, 20), 3, "IEEE float");
        assert_eq!(u16_at(&wav, 22), 1);
        assert_eq!(u32_at(&wav, 28), 44_100 * 4);
        assert_eq!(u16_at(&wav, 32), 4);
        assert_eq!(u16_at(&wav, 34), 32);
        assert_eq!(u16_at(&wav, 36), 0, "no extension");
        assert_eq!(&wav[38..42], b"fact");
        assert_eq!(u32_at(&wav, 46), 3, "sample frames");
        assert_eq!(&wav[50..54], b"data");
        assert_eq!(u32_at(&wav, 54), 12);
        let at = |i: usize| f32::from_le_bytes(wav[58 + 4 * i..62 + 4 * i].try_into().unwrap());
        assert_eq!([at(0), at(1), at(2)], [0.5, -1.25, 2.0], "nothing clamped");
    }

    /// The samples' bytes as a host streaming a file writes them after the
    /// header, each little-endian.
    fn bytes_of(samples: &SoundSamples) -> Vec<u8> {
        match samples {
            SoundSamples::I16(values) => values.iter().flat_map(|v| v.to_le_bytes()).collect(),
            SoundSamples::F32(values) => values.iter().flat_map(|v| v.to_le_bytes()).collect(),
        }
    }

    #[test]
    fn the_header_and_then_the_samples_are_the_whole_file() {
        for (samples, empty, header_bytes) in [
            (
                SoundSamples::I16(vec![1, -2, 32767, -32768, 0, 5]),
                SoundSamples::I16(Vec::new()),
                44,
            ),
            (
                SoundSamples::F32(vec![0.5, -1.25, 2.0, 0.0, -0.0, 1.0e-3]),
                SoundSamples::F32(Vec::new()),
                58,
            ),
        ] {
            let kind = samples.kind();
            let whole = encode_wav_samples(&samples, 44_100, 2);
            let header = wav_header(kind, 44_100, 2, 3, 0).unwrap();
            assert_eq!(header.len(), header_bytes, "{kind:?}");
            let mut streamed = header.clone();
            streamed.extend(bytes_of(&samples));
            assert_eq!(streamed, whole, "{kind:?}");
            // The header a streamed file starts with, before its first frame.
            assert_eq!(
                wav_header(kind, 44_100, 2, 0, 0).unwrap(),
                encode_wav_samples(&empty, 44_100, 2),
                "{kind:?}"
            );
            // With a loop: the same header, its RIFF size grown by the loop's
            // chunk, the samples, then the chunk.
            let looped = wav_with_loop(whole.clone(), 44_100, 1, 2);
            assert_eq!(u32_at(&looped, 4), u32_at(&header, 4) + 68, "{kind:?}");
            assert_eq!(&looped[..4], &header[..4]);
            assert_eq!(&looped[8..header.len()], &header[8..], "{kind:?}");
            assert_eq!(&looped[header.len()..whole.len()], &bytes_of(&samples)[..]);
            assert_eq!(&looped[whole.len()..whole.len() + 4], b"smpl");
            assert_eq!(looped.len(), whole.len() + 68);
        }
    }

    #[test]
    fn a_header_whose_fields_cannot_hold_it_is_refused() {
        assert_eq!(
            wav_header(SampleKind::I16, 32_000, 0, 1, 0),
            None,
            "no channel"
        );
        // A sample frame's bytes in 16 bits.
        assert!(wav_header(SampleKind::I16, 32_000, 32_767, 1, 0).is_some());
        assert_eq!(wav_header(SampleKind::I16, 32_000, 32_768, 1, 0), None);
        assert_eq!(wav_header(SampleKind::F32, 32_000, 16_384, 1, 0), None);
        // The bytes of a second in 32.
        assert_eq!(wav_header(SampleKind::I16, u32::MAX, 1, 1, 0), None);
        // The data's size, and the file's after its header, in 32.
        let most = (u32::MAX - 36) / 2;
        assert!(wav_header(SampleKind::I16, 32_000, 1, most, 0).is_some());
        assert_eq!(wav_header(SampleKind::I16, 32_000, 1, most + 1, 0), None);
        let most = (u32::MAX - 50) / 4;
        assert!(wav_header(SampleKind::F32, 32_000, 1, most, 0).is_some());
        assert_eq!(wav_header(SampleKind::F32, 32_000, 1, most + 1, 0), None);
        // The file's with the chunks after its samples, in 32: the most
        // frames, with the bytes left after the data a trailer may take.
        let most = (u32::MAX - 36) / 2;
        let left = u32::MAX - 36 - 2 * most;
        assert!(wav_header(SampleKind::I16, 32_000, 1, most, left).is_some());
        assert_eq!(wav_header(SampleKind::I16, 32_000, 1, most, left + 1), None);
        assert_eq!(wav_header(SampleKind::I16, 32_000, 1, 0, u32::MAX), None);
    }

    /// A loop and two cues: the first one's label of four letters, whose
    /// `labl` body of 9 bytes takes a pad byte, the second one's of three.
    fn loop_and_two_cues() -> WavMarks {
        WavMarks {
            r#loop: Some((1, 3)),
            cues: vec![
                WavCue {
                    frame: 2,
                    label: "jump".into(),
                },
                WavCue {
                    frame: 0x0102,
                    label: "end".into(),
                },
            ],
        }
    }

    #[test]
    fn a_trailer_is_the_loop_s_smpl_then_the_cues_and_their_labels_byte_for_byte() {
        #[rustfmt::skip]
        let cue: [u8; 60] = [
            b'c', b'u', b'e', b' ', 52, 0, 0, 0,
            2, 0, 0, 0,                                     // two points
            1, 0, 0, 0, 2, 0, 0, 0,                         // point 1, at frame 2,
            b'd', b'a', b't', b'a', 0, 0, 0, 0, 0, 0, 0, 0, // of the data chunk,
            2, 0, 0, 0,                                     // its frame there
            2, 0, 0, 0, 0x02, 0x01, 0, 0,                   // point 2, at 0x0102
            b'd', b'a', b't', b'a', 0, 0, 0, 0, 0, 0, 0, 0,
            0x02, 0x01, 0, 0,
        ];
        #[rustfmt::skip]
        let list: [u8; 46] = [
            b'L', b'I', b'S', b'T', 38, 0, 0, 0, b'a', b'd', b't', b'l',
            b'l', b'a', b'b', b'l', 9, 0, 0, 0, 1, 0, 0, 0, // label 1: "jump",
            b'j', b'u', b'm', b'p', 0, 0,                   // its zero, a pad byte
            b'l', b'a', b'b', b'l', 8, 0, 0, 0, 2, 0, 0, 0, // label 2: "end"
            b'e', b'n', b'd', 0,                            // and its zero
        ];
        let mut expected = LOOP_1_3_AT_32K.to_vec();
        expected.extend_from_slice(&cue);
        expected.extend_from_slice(&list);
        assert_eq!(wav_trailer(32_000, &loop_and_two_cues()), expected);
    }

    #[test]
    fn a_loop_alone_is_its_smpl_chunk_and_no_mark_no_byte() {
        let marks = WavMarks {
            r#loop: Some((1, 3)),
            cues: Vec::new(),
        };
        assert_eq!(wav_trailer(32_000, &marks), LOOP_1_3_AT_32K);
        assert!(wav_trailer(32_000, &WavMarks::default()).is_empty());
        let wav = encode_wav_samples(&SoundSamples::I16(vec![1, 2]), 32_000, 1);
        assert_eq!(
            wav_with_marks(wav.clone(), 32_000, &WavMarks::default()),
            wav
        );
    }

    #[test]
    fn a_streamed_wav_with_marks_is_the_header_the_samples_then_the_trailer() {
        for samples in [
            SoundSamples::I16(vec![1, -2, 32767, -32768, 0, 5, 7, 9]),
            SoundSamples::F32(vec![0.5, -1.25, 2.0, 0.0, -0.0, 1.0e-3, 0.25, 1.0]),
        ] {
            let kind = samples.kind();
            let marks = loop_and_two_cues();
            let trailer = wav_trailer(44_100, &marks);
            let mut streamed = wav_header(kind, 44_100, 2, 4, trailer.len() as u32).unwrap();
            streamed.extend(bytes_of(&samples));
            streamed.extend(&trailer);
            let whole = wav_with_marks(encode_wav_samples(&samples, 44_100, 2), 44_100, &marks);
            assert_eq!(streamed, whole, "{kind:?}");
            assert_eq!(u32_at(&whole, 4) as usize, whole.len() - 8, "{kind:?}");
            // The header with a trailer is the one without, its RIFF size
            // grown by the trailer's bytes.
            let plain = wav_header(kind, 44_100, 2, 4, 0).unwrap();
            assert_eq!(&streamed[8..plain.len()], &plain[8..], "{kind:?}");
            assert_eq!(
                u32_at(&streamed, 4),
                u32_at(&plain, 4) + trailer.len() as u32
            );
        }
    }

    #[test]
    fn a_wav_quantises_and_clamps_its_samples() {
        // Full scale either way, and an over-range value that must clamp,
        // not wrap.
        let wav = encode_wav(&[1.0, -1.0, 2.0], 32_000, 1);
        let at = |i: usize| i16::from_le_bytes([wav[44 + 2 * i], wav[45 + 2 * i]]);
        assert_eq!(at(0), 32767);
        assert_eq!(at(1), -32767);
        assert_eq!(at(2), 32767);
    }
}
