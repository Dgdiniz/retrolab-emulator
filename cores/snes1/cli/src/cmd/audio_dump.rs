//! `audio-dump`: boot a ROM, run it for a window of frames with audio
//! capture enabled, and write the S-DSP output to a 16-bit PCM stereo
//! WAV.
//!
//! The host audio ring stays empty unless capture is armed via
//! [`EmulatorCore::set_audio_enabled`]; the S-DSP still runs either way,
//! but [`EmulatorCore::get_audio_samples`] only yields the resampled
//! 44.1 kHz stereo stream when it is set. The command also prints
//! peak/RMS/clip stats and warns on silence, so a sound driver that is
//! loaded-but-not-playing (e.g. a music-synced intro that never started)
//! is obvious without opening the file.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};
use retro_core::EmulatorCore;
use snes1_core::SnesEmulator;

/// WAV is written as 16-bit PCM; the SNES host mix is stereo-interleaved
/// (L, R, L, R, ...) at the rate reported by `get_audio_info()`.
const WAV_CHANNELS: u16 = 2;
const BITS_PER_SAMPLE: u16 = 16;
/// Peak below this counts as "silent", guards against denormal/dither
/// noise being reported as audible output.
const SILENCE_PEAK: f32 = 1.0e-4;

#[derive(Args, Debug)]
pub struct AudioDumpArgs {
    /// Path to the ROM file (`.sfc` / `.smc`).
    pub rom: PathBuf,
    /// Number of frames to run while capturing (~5 s at 60 fps).
    #[arg(long, default_value_t = 300)]
    pub frames: u64,
    /// Output WAV path.
    #[arg(long, short, default_value = "audio_dump.wav")]
    pub output: PathBuf,
    /// Show a progress bar (on stderr).
    #[arg(long)]
    pub progress: bool,
}

pub fn run(args: AudioDumpArgs) -> Result<()> {
    let rom = super::read_rom(&args.rom)?;
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;

    if let Some(info) = emu.get_rom_info() {
        println!("Loaded {:?} [{}]", info.title.trim(), info.region);
    }

    emu.set_audio_enabled(true);
    let sample_rate = emu.get_audio_info().sample_rate;

    let pb = make_bar(args.progress, args.frames);
    let mut samples: Vec<f32> = Vec::new();
    for _ in 0..args.frames {
        emu.run_frame();
        // `get_audio_samples` drains the host ring each frame; not
        // draining would let the bounded ring overflow and silently drop
        // the oldest samples, leaving gaps in the dump.
        samples.extend_from_slice(emu.get_audio_samples());
        if let Some(pb) = &pb {
            pb.inc(1);
        }
    }
    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    let stats = AudioStats::analyze(&samples);
    let frame_pairs = samples.len() / WAV_CHANNELS as usize;
    println!(
        "Captured {} stereo sample frames ({:.2}s @ {} Hz)",
        frame_pairs,
        if sample_rate == 0 {
            0.0
        } else {
            frame_pairs as f64 / sample_rate as f64
        },
        sample_rate
    );
    println!(
        "Peak: {:.4} ({})  RMS: {:.6}",
        stats.peak,
        stats.peak_dbfs(),
        stats.rms
    );
    if stats.is_silent() {
        eprintln!(
            "warning: captured audio is silent (peak {:.6}) after {} frames - \
             the sound driver loaded but never keyed on a voice",
            stats.peak, args.frames
        );
    }

    let wav = encode_wav(&samples, sample_rate, WAV_CHANNELS);
    let mut out = super::open_output(Some(&args.output))?;
    out.write_all(&wav)
        .with_context(|| format!("failed to write WAV: {}", args.output.display()))?;
    out.flush().ok();
    println!("Wrote {} bytes to {}", wav.len(), args.output.display());
    Ok(())
}

/// Encode interleaved `f32` samples (nominally `-1.0..=1.0`) as a 16-bit
/// PCM WAV byte stream.
///
/// Pure (no I/O) so the 44-byte canonical header layout is unit-testable
/// without touching the filesystem. Samples are clamped to `[-1, 1]`
/// before the `i16` quantisation so out-of-range mix values can't wrap.
pub(crate) fn encode_wav(samples: &[f32], sample_rate: u32, channels: u16) -> Vec<u8> {
    let bytes_per_sample = (BITS_PER_SAMPLE / 8) as u32;
    let byte_rate = sample_rate * channels as u32 * bytes_per_sample;
    let block_align = channels * (BITS_PER_SAMPLE / 8);
    let data_size = samples.len() as u32 * bytes_per_sample;

    let mut out = Vec::with_capacity(44 + data_size as usize);
    // RIFF header.
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_size).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    // fmt chunk.
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
    // data chunk.
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_size.to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Loudness summary of a captured buffer, used to flag a silent dump.
pub(crate) struct AudioStats {
    pub peak: f32,
    pub rms: f32,
}

impl AudioStats {
    pub(crate) fn analyze(samples: &[f32]) -> Self {
        let mut peak = 0.0f32;
        let mut sum_sq = 0.0f64;
        for &s in samples {
            peak = peak.max(s.abs());
            sum_sq += (s as f64) * (s as f64);
        }
        let rms = if samples.is_empty() {
            0.0
        } else {
            (sum_sq / samples.len() as f64).sqrt() as f32
        };
        Self { peak, rms }
    }

    pub(crate) fn is_silent(&self) -> bool {
        self.peak < SILENCE_PEAK
    }

    fn peak_dbfs(&self) -> String {
        if self.peak > 0.0 {
            format!("{:.1} dBFS", 20.0 * self.peak.log10())
        } else {
            "-inf dBFS".to_string()
        }
    }
}

fn make_bar(show: bool, len: u64) -> Option<ProgressBar> {
    if !show {
        return None;
    }
    let pb = ProgressBar::new(len);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} frames ({eta})")
            .unwrap()
            .progress_chars("#>-"),
    );
    Some(pb)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a little-endian `u32` out of `bytes` at `off`.
    fn u32_at(bytes: &[u8], off: usize) -> u32 {
        u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
    }
    fn u16_at(bytes: &[u8], off: usize) -> u16 {
        u16::from_le_bytes([bytes[off], bytes[off + 1]])
    }

    #[test]
    fn wav_header_is_canonical_44_bytes() {
        // Two stereo sample frames -> 4 f32 -> 8 data bytes.
        let samples = [0.0f32, 0.0, 0.0, 0.0];
        let wav = encode_wav(&samples, 44_100, 2);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u32_at(&wav, 16), 16); // fmt chunk size
        assert_eq!(u16_at(&wav, 20), 1); // PCM
        assert_eq!(u16_at(&wav, 22), 2); // channels
        assert_eq!(u32_at(&wav, 24), 44_100); // sample rate
        assert_eq!(u32_at(&wav, 28), 44_100 * 2 * 2); // byte rate
        assert_eq!(u16_at(&wav, 32), 4); // block align (2ch * 2 bytes)
        assert_eq!(u16_at(&wav, 34), 16); // bits per sample
        assert_eq!(&wav[36..40], b"data");
        let data_size = u32_at(&wav, 40);
        assert_eq!(data_size, 8); // 4 samples * 2 bytes
                                  // RIFF size is 36 + data, and the whole buffer is header + data.
        assert_eq!(u32_at(&wav, 4), 36 + data_size);
        assert_eq!(wav.len(), 44 + data_size as usize);
    }

    #[test]
    fn wav_quantises_and_clamps_samples() {
        // Full-scale positive, full-scale negative, and an over-range
        // value that must clamp (not wrap) to +32767.
        let wav = encode_wav(&[1.0, -1.0, 2.0], 32_000, 1);
        let s0 = i16::from_le_bytes([wav[44], wav[45]]);
        let s1 = i16::from_le_bytes([wav[46], wav[47]]);
        let s2 = i16::from_le_bytes([wav[48], wav[49]]);
        assert_eq!(s0, 32767);
        assert_eq!(s1, -32767);
        assert_eq!(s2, 32767); // clamped, did not wrap to a negative
    }

    #[test]
    fn stats_flag_silence_and_measure_peak() {
        let silent = AudioStats::analyze(&[0.0, 0.0, 0.0, 0.0]);
        assert!(silent.is_silent());
        assert_eq!(silent.peak, 0.0);
        assert_eq!(silent.rms, 0.0);

        let loud = AudioStats::analyze(&[0.5, -0.8, 0.25]);
        assert!(!loud.is_silent());
        assert!((loud.peak - 0.8).abs() < 1e-6);
        assert!(loud.rms > 0.0);
    }

    #[test]
    fn stats_on_empty_buffer_are_zero_not_nan() {
        let empty = AudioStats::analyze(&[]);
        assert_eq!(empty.peak, 0.0);
        assert_eq!(empty.rms, 0.0);
        assert!(empty.is_silent());
    }
}
