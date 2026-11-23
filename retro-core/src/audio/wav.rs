//! The WAV writer: interleaved samples as a RIFF WAVE file.

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
/// eight, which its RIFF size counts with the data: the `WAVE` tag, the
/// format chunk, for floats the `fact` chunk, and the data chunk's own
/// eight.
fn riff_header_bytes(kind: SampleKind) -> u32 {
    match kind {
        SampleKind::I16 => 36,
        SampleKind::F32 => 50,
    }
}

/// The header of a WAV of `data_size` bytes of `kind`'s samples, `frames`
/// sample frames of `channels` at `sample_rate`, up to the data chunk's
/// size: 16-bit integers as 16-bit PCM (the canonical 44 bytes), 32-bit
/// floats as 32-bit float samples (a format chunk of 18 bytes and the
/// `fact` chunk of the frames a non-PCM file carries, 58 bytes).
fn header(
    kind: SampleKind,
    sample_rate: u32,
    channels: u16,
    data_size: u32,
    frames: u32,
) -> Vec<u8> {
    let block_align = channels * sample_bytes(kind);
    let mut out = Vec::with_capacity(58);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(riff_header_bytes(kind) + data_size).to_le_bytes());
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
    let mut out = header(kind, sample_rate, channels, data_size as u32, frames);
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
/// interleaved samples of `kind`, at `sample_rate`: the bytes
/// [`encode_wav_samples`] writes before such samples. A file streamed to
/// disk is this header, then the samples' bytes, each little-endian. `None`
/// for no channel, or for a value the header's fields cannot hold: the
/// bytes of a sample frame in 16 bits; the bytes of a second, of the data
/// and of the file in 32.
pub fn wav_header(
    kind: SampleKind,
    sample_rate: u32,
    channels: u16,
    frames: u32,
) -> Option<Vec<u8>> {
    let block_align = channels
        .checked_mul(sample_bytes(kind))
        .filter(|&bytes| bytes > 0)?;
    let data_size = frames.checked_mul(u32::from(block_align))?;
    let fits = sample_rate.checked_mul(u32::from(block_align)).is_some()
        && data_size.checked_add(riff_header_bytes(kind)).is_some();
    fits.then(|| header(kind, sample_rate, channels, data_size, frames))
}

/// The WAV `wav` (as [`encode_wav`] or [`encode_wav_samples`] write one)
/// with a `smpl` chunk after its data naming one loop, played forward for
/// ever, from sample frame `start` to sample frame `end` (the last it
/// plays) at `sample_rate`; the RIFF size grows to hold it. The chunk's
/// other fields say nothing (no maker, no product, middle C as the unity
/// note, no SMPTE offset).
pub fn wav_with_loop(mut wav: Vec<u8>, sample_rate: u32, start: u32, end: u32) -> Vec<u8> {
    let period_ns = 1_000_000_000 / sample_rate.max(1);
    wav.extend_from_slice(b"smpl");
    wav.extend_from_slice(&60u32.to_le_bytes());
    for value in [0, 0, period_ns, 60, 0, 0, 0, 1, 0, 0, 0, start, end, 0, 0] {
        wav.extend_from_slice(&u32::to_le_bytes(value));
    }
    let riff = (wav.len() - 8) as u32;
    wav[4..8].copy_from_slice(&riff.to_le_bytes());
    wav
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
            let header = wav_header(kind, 44_100, 2, 3).unwrap();
            assert_eq!(header.len(), header_bytes, "{kind:?}");
            let mut streamed = header.clone();
            streamed.extend(bytes_of(&samples));
            assert_eq!(streamed, whole, "{kind:?}");
            // The header a streamed file starts with, before its first frame.
            assert_eq!(
                wav_header(kind, 44_100, 2, 0).unwrap(),
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
            wav_header(SampleKind::I16, 32_000, 0, 1),
            None,
            "no channel"
        );
        // A sample frame's bytes in 16 bits.
        assert!(wav_header(SampleKind::I16, 32_000, 32_767, 1).is_some());
        assert_eq!(wav_header(SampleKind::I16, 32_000, 32_768, 1), None);
        assert_eq!(wav_header(SampleKind::F32, 32_000, 16_384, 1), None);
        // The bytes of a second in 32.
        assert_eq!(wav_header(SampleKind::I16, u32::MAX, 1, 1), None);
        // The data's size, and the file's after its header, in 32.
        let most = (u32::MAX - 36) / 2;
        assert!(wav_header(SampleKind::I16, 32_000, 1, most).is_some());
        assert_eq!(wav_header(SampleKind::I16, 32_000, 1, most + 1), None);
        let most = (u32::MAX - 50) / 4;
        assert!(wav_header(SampleKind::F32, 32_000, 1, most).is_some());
        assert_eq!(wav_header(SampleKind::F32, 32_000, 1, most + 1), None);
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
