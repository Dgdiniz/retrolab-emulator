//! `brr-dump`: run a ROM and write its BRR samples as files: each sample's
//! blocks as a `.brr` (raw, whole 9-byte blocks, which BRRtools reads, its
//! loop given with `-l`; or after the 2-byte loop header AddmusicK reads),
//! its values as a 32 kHz WAV with its loop as a voice plays it (pass after
//! pass until the loop repeats, the passes it repeats marked as the loop),
//! and a `.json` of what it is (its source number, the directory, its start
//! and loop, its blocks, how it ends, the passes of its loop the WAV holds
//! and how many of them repeat, and the pitches and voices it was keyed on
//! at).
//!
//! The samples are read from ARAM at the end of the run, through the
//! directory then in force; the pitches and voices are the key-ons the
//! sound log took down during the run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use retro_cli::rom::{load, RomArgs};
use retro_cli::InputArgs;
use retro_core::audio::{render_frames, SoundCore, SoundEvent, SoundLogContents};
use retro_core::EmulatorCore;
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
        mailbox: false,
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
    write_samples(&emu, keyed, &sources, &args.output, args.loop_header)
}

/// Write the files of each sample of `sources` to the folder `output`
/// (made if missing), each with the pitches and the voices `keyed` holds
/// for it, read through the directory the S-DSP's registers name, and
/// print a line of each.
fn write_samples(
    emu: &SnesEmulator,
    mut keyed: BTreeMap<u8, Keyed>,
    sources: &[u8],
    output: &Path,
    loop_header: bool,
) -> Result<()> {
    std::fs::create_dir_all(output)
        .with_context(|| format!("failed to make {}", output.display()))?;
    println!("Source  Start  Loop   Blocks  Ends     Voices    Pitches");
    for &source in sources {
        let seen = keyed.remove(&source).unwrap_or_default();
        let files = emu.sample_files(source, seen.pitches, seen.voices);
        let stem = output.join(format!("sample-{source:02X}"));
        let brr = match loop_header {
            true => &files.brr_looped,
            false => &files.brr,
        };
        let write = |extension: &str, bytes: &[u8]| -> Result<()> {
            let path = stem.with_extension(extension);
            std::fs::write(&path, bytes)
                .with_context(|| format!("failed to write {}", path.display()))
        };
        write("brr", brr)?;
        write("wav", &files.wav)?;
        write("json", files.info.to_json().as_bytes())?;
        let info = &files.info;
        println!(
            "${source:02X}     ${:04X}  ${:04X}  {:>6}  {:<8} {:<9} {}",
            info.start,
            info.loop_start,
            info.blocks,
            format!("{:?}", info.ends),
            format!("{:?}", info.voices),
            info.pitches
                .iter()
                .map(|p| format!("${p:04X}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    println!("Wrote the samples to {}", output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A machine whose ARAM holds source 1 of a directory at `$0200`: three
    /// blocks at `$0300`, the end block looping back to the second.
    fn machine() -> SnesEmulator {
        let mut emu = SnesEmulator::new();
        let apu = emu.bus_mut().apu_mut();
        for (i, &byte) in [0x00, 0x03, 0x09, 0x03].iter().enumerate() {
            apu.spc_write(0x0204 + i as u16, byte);
        }
        let blocks: [(u8, [u8; 8]); 3] = [
            (0xC0, [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0]),
            (0x44, [0x7F, 0x80, 0x01, 0xFF, 0x70, 0x0F, 0x88, 0x77]),
            (0x8F, [0x11, 0x22, 0x33, 0x44, 0xCC, 0xDD, 0xEE, 0x99]),
        ];
        for (n, (header, data)) in blocks.iter().enumerate() {
            let at = 0x0300 + 9 * n as u16;
            apu.spc_write(at, *header);
            for (i, &byte) in data.iter().enumerate() {
                apu.spc_write(at + 1 + i as u16, byte);
            }
        }
        apu.spc_write(0x00F2, 0x5D);
        apu.spc_write(0x00F3, 0x02);
        emu
    }

    #[test]
    fn the_files_written_are_the_core_s_for_each_sample() {
        let emu = machine();
        let folder =
            std::env::temp_dir().join(format!("snes1-cli-brr-dump-{}", std::process::id()));
        for loop_header in [false, true] {
            let keyed = BTreeMap::from([(
                1,
                Keyed {
                    pitches: vec![0x1000, 0x0800],
                    voices: vec![0, 3],
                },
            )]);
            write_samples(&emu, keyed, &[1, 2], &folder, loop_header).unwrap();
            let read = |name: &str| std::fs::read(folder.join(name)).unwrap();
            let files = emu.sample_files(1, vec![0x1000, 0x0800], vec![0, 3]);
            let brr = match loop_header {
                true => &files.brr_looped,
                false => &files.brr,
            };
            assert_eq!(&read("sample-01.brr"), brr);
            assert_eq!(read("sample-01.wav"), files.wav);
            assert_eq!(read("sample-01.json"), files.info.to_json().into_bytes());
            // A source nobody keyed: its files, with no pitch and no voice.
            let unkeyed = emu.sample_files(2, Vec::new(), Vec::new());
            assert_eq!(read("sample-02.wav"), unkeyed.wav);
            assert_eq!(read("sample-02.json"), unkeyed.info.to_json().into_bytes());
        }
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
