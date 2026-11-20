//! The sound a command captures: the WAV it writes, and the loudness
//! summary it prints.

/// A WAV's samples are 16-bit PCM.
const BITS_PER_SAMPLE: u16 = 16;

/// A peak below this counts as silence: it keeps denormal or dither noise
/// from being reported as sound.
const SILENCE_PEAK: f32 = 1.0e-4;

/// A sample this loud or louder counts as clipped.
const CLIP_LEVEL: f32 = 0.999;

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

/// The loudness of a captured buffer: its peak, its RMS and how many of
/// its samples clipped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioStats {
    /// The loudest sample, as an absolute value.
    pub peak: f32,
    /// The root mean square of the samples.
    pub rms: f32,
    /// The samples at or above the clipping level.
    pub clipped: u64,
    /// The samples measured.
    pub samples: u64,
}

impl AudioStats {
    /// The loudness of `samples`.
    pub fn analyze(samples: &[f32]) -> Self {
        let mut peak = 0.0f32;
        let mut sum_sq = 0.0f64;
        let mut clipped = 0u64;
        for &s in samples {
            let level = s.abs();
            peak = peak.max(level);
            if level >= CLIP_LEVEL {
                clipped += 1;
            }
            sum_sq += f64::from(s) * f64::from(s);
        }
        let rms = if samples.is_empty() {
            0.0
        } else {
            (sum_sq / samples.len() as f64).sqrt() as f32
        };
        Self {
            peak,
            rms,
            clipped,
            samples: samples.len() as u64,
        }
    }

    /// Whether the buffer is silence.
    pub fn is_silent(&self) -> bool {
        self.peak < SILENCE_PEAK
    }

    /// The peak in decibels of full scale: `-9.1 dBFS`.
    pub fn peak_dbfs(&self) -> String {
        if self.peak > 0.0 {
            format!("{:.1} dBFS", 20.0 * self.peak.log10())
        } else {
            "-inf dBFS".to_string()
        }
    }

    /// The line that says how many samples clipped: `Clipping: none`, or
    /// their count and their share of the samples.
    pub fn clipping_line(&self) -> String {
        match self.clipped {
            0 => "Clipping: none".to_string(),
            n => format!(
                "Clipping samples: {n} ({:.2}%)",
                n as f64 / self.samples.max(1) as f64 * 100.0
            ),
        }
    }
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
    fn a_wav_quantises_and_clamps_its_samples() {
        // Full scale either way, and an over-range value that must clamp,
        // not wrap.
        let wav = encode_wav(&[1.0, -1.0, 2.0], 32_000, 1);
        let at = |i: usize| i16::from_le_bytes([wav[44 + 2 * i], wav[45 + 2 * i]]);
        assert_eq!(at(0), 32767);
        assert_eq!(at(1), -32767);
        assert_eq!(at(2), 32767);
    }

    #[test]
    fn the_stats_find_silence_the_peak_and_the_clipped_samples() {
        let silent = AudioStats::analyze(&[0.0, 0.0, 0.0, 0.0]);
        assert!(silent.is_silent());
        assert_eq!((silent.peak, silent.rms, silent.clipped), (0.0, 0.0, 0));
        assert_eq!(silent.peak_dbfs(), "-inf dBFS");
        assert_eq!(silent.clipping_line(), "Clipping: none");

        let loud = AudioStats::analyze(&[0.5, -0.8, 0.25, 1.0]);
        assert!(!loud.is_silent());
        assert!((loud.peak - 1.0).abs() < 1e-6);
        assert!(loud.rms > 0.0);
        assert_eq!(loud.clipped, 1);
        assert_eq!(loud.clipping_line(), "Clipping samples: 1 (25.00%)");
        assert_eq!(AudioStats::analyze(&[0.5]).peak_dbfs(), "-6.0 dBFS");
    }

    #[test]
    fn the_stats_of_no_samples_are_zero_not_nan() {
        let empty = AudioStats::analyze(&[]);
        assert_eq!((empty.peak, empty.rms, empty.clipped), (0.0, 0.0, 0));
        assert!(empty.is_silent());
        assert_eq!(empty.clipping_line(), "Clipping: none");
    }
}
