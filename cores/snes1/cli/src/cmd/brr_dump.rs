//! `brr-dump`: run a ROM and write its BRR samples as files: each sample's
//! blocks as a `.brr` (raw, or after the 2-byte loop header AddmusicK and
//! BRRtools read), its values as a 32 kHz WAV with its loop, and a `.json`
//! of what it is (its source number, the directory, its start and loop, its
//! blocks, how it ends, and the pitches and voices it was keyed on at).
//!
//! The samples are read from ARAM at the end of the run, through the
//! directory then in force; the pitches and voices are the key-ons the
//! sound log took down during the run.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use retro_cli::rom::{load, RomArgs};
use retro_cli::InputArgs;
use retro_core::audio::{render_frames, SoundCore, SoundEvent, SoundLogContents};
use retro_core::EmulatorCore;
use snes1_core::sound::{brr_blocks, brr_with_loop_header, sample_wav, SampleInfo};
use snes1_core::SnesEmulator;

use crate::console::{Snes, SnesBoot};

#[derive(Args, Debug)]
pub struct BrrDumpArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Number of frames to run, e.g. `600` (10 s at 60 fps).
    #[arg(long, short)]
    pub frames: Option<u64>,
    /// A sample to write, by its source number (repeat for more).
    #[arg(long = "source", value_name = "S")]
    pub sources: Vec<u8>,
    /// Write every sample a voice was keyed on at during the run.
    #[arg(long)]
    pub all: bool,
    /// Begin each `.brr` with the 2-byte loop header.
    #[arg(long)]
    pub loop_header: bool,
    /// The folder the files go to (made if missing): `sample-SS.brr`,
    /// `sample-SS.wav` and `sample-SS.json`, SS the source number in hex.
    #[arg(long, short, default_value = "samples")]
    pub output: PathBuf,
    #[command(flatten)]
    pub input: InputArgs,
}

/// The pitches and the voices a sample was keyed on at, each once, in the
/// order they came.
#[derive(Default)]
struct Keyed {
    pitches: Vec<u32>,
    voices: Vec<u8>,
}

pub fn run(args: BrrDumpArgs<SnesBoot>) -> Result<()> {
    let Some(frames) = args.frames else {
        anyhow::bail!("specify --frames");
    };
    if args.sources.is_empty() && !args.all {
        anyhow::bail!("name a sample with --source, or write every one keyed with --all");
    }
    let plan = args.input.plan::<SnesEmulator>()?;
    let (_, mut emu) = load::<Snes>(&args.rom)?;
    emu.start_sound_log(SoundLogContents {
        writes: false,
        keys: true,
    });
    let mut keyed: BTreeMap<u8, Keyed> = BTreeMap::new();
    for frame in 0..frames {
        plan.before_frame(&mut emu, frame);
        render_frames(&mut emu, 1);
        let _ = emu.get_audio_samples();
        let log = emu
            .take_sound_log()
            .context("the log was on and took nothing")?;
        for entry in log.entries {
            if let SoundEvent::KeyOn {
                channel,
                instrument,
                pitch,
                ..
            } = entry.event
            {
                let sample = keyed.entry(instrument as u8).or_default();
                if !sample.pitches.contains(&pitch) {
                    sample.pitches.push(pitch);
                }
                if !sample.voices.contains(&channel) {
                    sample.voices.push(channel);
                }
            }
        }
    }
    emu.stop_sound_log();

    let mut sources = args.sources.clone();
    if args.all {
        sources.extend(keyed.keys().copied());
    }
    sources.sort_unstable();
    sources.dedup();
    std::fs::create_dir_all(&args.output)
        .with_context(|| format!("failed to make {}", args.output.display()))?;
    let directory = emu.sample_directory();
    let page = emu.bus().apu().sample_directory_page();
    let aram = emu.bus().apu().aram().to_vec();
    println!("Source  Start  Loop   Blocks  Ends     Voices    Pitches");
    for source in sources {
        let entry = &directory[usize::from(source)];
        let sample = emu.decode_brr(entry.start, entry.loop_start);
        let seen = keyed.remove(&source).unwrap_or_default();
        let info = SampleInfo::of(
            u16::from(page) << 8,
            entry,
            &sample,
            seen.pitches.clone(),
            seen.voices.clone(),
        );
        let stem = args.output.join(format!("sample-{source:02X}"));
        let brr = match args.loop_header {
            true => brr_with_loop_header(&aram, entry.start, &sample),
            false => brr_blocks(&aram, entry.start, &sample),
        };
        let write = |extension: &str, bytes: &[u8]| -> Result<()> {
            let path = stem.with_extension(extension);
            std::fs::write(&path, bytes)
                .with_context(|| format!("failed to write {}", path.display()))
        };
        write("brr", &brr)?;
        write("wav", &sample_wav(&sample))?;
        write("json", info.to_json().as_bytes())?;
        println!(
            "${source:02X}     ${:04X}  ${:04X}  {:>6}  {:<8} {:<9} {}",
            entry.start,
            entry.loop_start,
            sample.blocks,
            format!("{:?}", sample.ends),
            format!("{:?}", seen.voices),
            seen.pitches
                .iter()
                .map(|p| format!("${p:04X}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    println!("Wrote the samples to {}", args.output.display());
    Ok(())
}
