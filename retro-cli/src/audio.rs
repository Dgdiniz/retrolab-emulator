//! The loudness summary a command prints of the sound it captures (the WAV
//! it writes is retro-core's, `retro_core::audio::encode_wav`).

/// A peak below this counts as silence: it keeps denormal or dither noise
/// from being reported as sound.
const SILENCE_PEAK: f32 = 1.0e-4;

/// A sample this loud or louder counts as clipped.
const CLIP_LEVEL: f32 = 0.999;

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
