//! `audio-dump`: run a ROM with audio enabled and write the mixed
//! output as a 16-bit PCM stereo WAV, reporting peak level and clipping.
//! Uses `run_frame_fast()` so capture runs at full speed with no debug
//! overhead; `get_audio_buffer()` drains the ring each frame.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};
use retro_core::EmulatorCore;

#[derive(Args, Debug)]
pub struct AudioDumpArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Number of frames to run (default: 300 = ~5 seconds).
    #[arg(short, long, default_value_t = 300)]
    pub frames: u64,
    /// Output WAV file path.
    #[arg(short, long, default_value = "audio_dump.wav")]
    pub output: PathBuf,
    /// Sample rate (default: 44100).
    #[arg(long, default_value_t = 44100)]
    pub sample_rate: u32,
    /// Frame-scheduled player-1 input `"F:buttons;F:buttons"`. Most games sit
    /// silent on a title screen until something is pressed, so a capture
    /// without this records the boot jingle at best.
    #[arg(long)]
    pub press_at: Option<String>,
    /// Connect 6-button pads so `x`/`y`/`z`/`mode` reach the game.
    #[arg(long)]
    pub six_button: bool,
}

pub fn run(args: AudioDumpArgs) -> Result<()> {
    let (_, mut emu) = super::load_rom(&args.rom)?;

    println!("=== SG1 Audio Dump ===");
    println!("ROM: {}", args.rom.display());
    super::print_rom_title(&emu);

    if args.six_button {
        super::connect_controllers(&mut emu, true);
    }
    let schedule = super::parse_press_schedule(args.press_at.as_deref())?;

    emu.set_audio_enabled(true);
    println!("Running {} frames with audio enabled...", args.frames);
    println!("Sample rate: {} Hz", args.sample_rate);

    let mut all_samples: Vec<f32> = Vec::new();

    let progress = ProgressBar::new(args.frames);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} frames ({eta})")
            .unwrap()
            .progress_chars("#>-"),
    );

    for frame in 0..args.frames {
        super::apply_press(&mut emu, schedule.as_ref(), frame);
        emu.run_frame_fast();
        let audio = emu.get_audio_buffer();
        all_samples.extend_from_slice(&audio);
        progress.set_position(frame + 1);
    }
    progress.finish_with_message("Complete");

    let total_samples = all_samples.len();
    let stereo_samples = total_samples / 2;
    let duration_secs = stereo_samples as f64 / args.sample_rate as f64;
    println!("Captured {stereo_samples} stereo samples ({duration_secs:.2}s)");

    let mut peak = 0.0f32;
    let mut clip_count = 0u64;
    for &s in &all_samples {
        let abs = s.abs();
        peak = peak.max(abs);
        if abs >= 0.999 {
            clip_count += 1;
        }
    }
    println!(
        "Peak level: {:.4} ({:.1} dB)",
        peak,
        if peak > 0.0 {
            20.0 * peak.log10()
        } else {
            f32::NEG_INFINITY
        }
    );
    if clip_count > 0 {
        println!(
            "Clipping samples: {} ({:.2}%)",
            clip_count,
            clip_count as f64 / total_samples.max(1) as f64 * 100.0
        );
    } else {
        println!("Clipping: none");
    }

    write_wav(&args.output, &all_samples, args.sample_rate, 2)?;
    println!("WAV written to: {}", args.output.display());
    Ok(())
}

/// Write `samples` (interleaved f32, clamped to [-1, 1]) as a 16-bit PCM
/// WAV with the given channel count.
fn write_wav(path: &Path, samples: &[f32], sample_rate: u32, channels: u16) -> Result<()> {
    let mut file = BufWriter::new(
        File::create(path)
            .with_context(|| format!("failed to create WAV file: {}", path.display()))?,
    );

    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * (bits_per_sample as u32 / 8);
    let block_align = channels * (bits_per_sample / 8);
    let data_size = (samples.len() * 2) as u32;
    let file_size = 36 + data_size;

    file.write_all(b"RIFF")?;
    file.write_all(&file_size.to_le_bytes())?;
    file.write_all(b"WAVE")?;
    file.write_all(b"fmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&channels.to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&bits_per_sample.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_size.to_le_bytes())?;

    for &sample in samples {
        let clamped = sample.clamp(-1.0, 1.0);
        let int_sample = (clamped * 32767.0) as i16;
        file.write_all(&int_sample.to_le_bytes())?;
    }
    file.flush()?;
    Ok(())
}
