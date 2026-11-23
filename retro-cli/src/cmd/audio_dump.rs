//! `audio-dump`: run a ROM for a number of frames with the sound captured,
//! and write it as a 16-bit PCM stereo WAV, with its loudness: the peak,
//! the RMS and the clipped samples, and a warning when it is silent, so a
//! sound driver that loaded but never played shows without opening the
//! file.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use retro_core::audio::encode_wav;
use retro_core::EmulatorCore;

use crate::audio::AudioStats;
use crate::console::Console;
use crate::input::InputArgs;
use crate::output::open_output;
use crate::progress;
use crate::rom::{load, write_banner, RomArgs};

/// The host's sound is stereo, its samples interleaved left and right.
const CHANNELS: u16 = 2;

#[derive(clap::Args, Debug)]
pub struct AudioDumpArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Number of frames to run while capturing, e.g. `300` (about 5 s at 60
    /// fps).
    #[arg(long, short)]
    pub frames: Option<u64>,
    /// Output WAV file path.
    #[arg(long, short, default_value = "audio_dump.wav")]
    pub output: PathBuf,
    /// The WAV's sample rate, in Hz: the rate the core produces; another is
    /// refused, since the samples are not resampled.
    #[arg(long)]
    pub sample_rate: Option<u32>,
    /// Show a progress bar (on stderr).
    #[arg(long)]
    pub progress: bool,
    #[command(flatten)]
    pub input: InputArgs,
}

pub fn run<C: Console>(args: AudioDumpArgs<C::BootArgs>) -> Result<()> {
    let Some(frames) = args.frames else {
        anyhow::bail!("specify --frames");
    };
    let input = args.input.plan::<C::Emulator>()?;

    let (_, mut emu) = load::<C>(&args.rom)?;
    let sample_rate = emu.get_audio_info().sample_rate;
    if let Some(asked) = args.sample_rate {
        if asked != sample_rate {
            anyhow::bail!(
                "--sample-rate {asked}: the core produces {sample_rate} Hz, and the samples are \
                 not resampled"
            );
        }
    }
    write_banner(&mut emu, &mut std::io::stdout())?;

    emu.set_audio_enabled(true);
    let bar = progress::bar(args.progress, frames, "frames");
    let mut samples: Vec<f32> = Vec::new();
    for frame in 0..frames {
        input.before_frame(&mut emu, frame);
        emu.run_frame();
        // The host's ring is bounded: taking it every frame keeps it from
        // dropping the oldest samples, which would leave gaps in the dump.
        samples.extend_from_slice(emu.get_audio_samples());
        if let Some(bar) = &bar {
            bar.inc(1);
        }
    }
    progress::finish(bar);

    let stats = AudioStats::analyze(&samples);
    let sample_frames = samples.len() / usize::from(CHANNELS);
    println!(
        "Captured {sample_frames} stereo sample frames ({:.2}s @ {sample_rate} Hz)",
        if sample_rate == 0 {
            0.0
        } else {
            sample_frames as f64 / f64::from(sample_rate)
        }
    );
    println!(
        "Peak: {:.4} ({})  RMS: {:.6}",
        stats.peak,
        stats.peak_dbfs(),
        stats.rms
    );
    println!("{}", stats.clipping_line());
    if stats.is_silent() {
        eprintln!(
            "warning: captured audio is silent (peak {:.6}) after {frames} frames - the sound \
             driver loaded but never keyed on a voice",
            stats.peak
        );
    }

    let wav = encode_wav(&samples, sample_rate, CHANNELS);
    let mut out = open_output(Some(&args.output))?;
    out.write_all(&wav)
        .with_context(|| format!("failed to write WAV: {}", args.output.display()))?;
    out.flush()
        .with_context(|| format!("failed to write WAV: {}", args.output.display()))?;
    println!("Wrote {} bytes to {}", wav.len(), args.output.display());
    Ok(())
}
