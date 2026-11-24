//! `vgm-log` and `gym-log`: run a ROM for a number of frames with the sound
//! log on, and write what the YM2612 and the PSG were told as a VGM 1.71
//! file (or a gzipped `.vgz`) or as a GYM file, which a player plays back.
//!
//! The log starts with the chips' registers as they stood, then every
//! write at its time on the master clock; a VGM keeps that time to the
//! sample, a GYM to the frame. A held note in a player starts again from
//! its attack: registers cannot hold an envelope's level.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use retro_cli::rom::{load, RomArgs};
use retro_cli::InputArgs;
use retro_core::audio::{
    encode_vgm, render_frames, DacForm, Gd3Tag, SoundCore, SoundEvent, SoundLog, SoundLogContents,
    VgmOptions,
};
use retro_core::EmulatorCore;
use sg1_core::sound::{encode_gym, GymHeader};
use sg1_core::GenesisEmulator;

use crate::console::{Genesis, GenesisBoot};

/// How `vgm-log` writes the DAC's samples.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum DacArg {
    /// As register writes, three bytes each.
    Raw,
    /// In a data bank at the file's start, one byte a write.
    Bank,
}

#[derive(Args, Debug)]
pub struct VgmLogArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Number of frames to run and log, e.g. `1800` (30 s at 60 fps).
    #[arg(long, short)]
    pub frames: Option<u64>,
    /// Output file path.
    #[arg(long, short, default_value = "sound.vgm")]
    pub output: PathBuf,
    /// Write the file gzipped, as a `.vgz`.
    #[arg(long)]
    pub vgz: bool,
    /// How the DAC's samples are written.
    #[arg(long, value_enum, default_value_t = DacArg::Raw)]
    pub dac: DacArg,
    /// Loop back to the start of this frame (counted from 0) when the file
    /// ends.
    #[arg(long)]
    pub loop_frame: Option<u64>,
    /// The track's name, in the file's GD3 tag.
    #[arg(long, default_value = "")]
    pub track: String,
    /// The game's name; the ROM header's title by default.
    #[arg(long)]
    pub game: Option<String>,
    /// The composer.
    #[arg(long, default_value = "")]
    pub author: String,
    /// The release date.
    #[arg(long, default_value = "")]
    pub date: String,
    /// Who made the file.
    #[arg(long, default_value = "")]
    pub ripper: String,
    /// Notes.
    #[arg(long, default_value = "")]
    pub notes: String,
    #[command(flatten)]
    pub input: InputArgs,
}

#[derive(Args, Debug)]
pub struct GymLogArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Number of frames to run and log, e.g. `1800` (30 s at 60 fps).
    #[arg(long, short)]
    pub frames: Option<u64>,
    /// Output file path.
    #[arg(long, short, default_value = "sound.gym")]
    pub output: PathBuf,
    /// Begin the file with a GYMX header holding the texts below.
    #[arg(long)]
    pub gymx: bool,
    /// The song's name.
    #[arg(long, default_value = "")]
    pub song: String,
    /// The game's name; the ROM header's title by default.
    #[arg(long)]
    pub game: Option<String>,
    /// The publisher.
    #[arg(long, default_value = "")]
    pub publisher: String,
    /// Who made the file.
    #[arg(long, default_value = "")]
    pub dumper: String,
    /// A comment.
    #[arg(long, default_value = "")]
    pub comment: String,
    #[command(flatten)]
    pub input: InputArgs,
}

/// Run `frames` frames of the player's loop with the sound log on, its
/// writes logged, `input` applied before each frame: the log of the whole
/// run, and the ROM's title.
fn record(
    rom: &RomArgs<GenesisBoot>,
    input: &InputArgs,
    frames: Option<u64>,
) -> Result<(SoundLog, GenesisEmulator, String)> {
    let Some(frames) = frames else {
        anyhow::bail!("specify --frames");
    };
    let plan = input.plan::<GenesisEmulator>()?;
    let (_, mut emu) = load::<Genesis>(rom)?;
    let title = emu
        .get_rom_info()
        .map(|info| info.title.trim().to_string())
        .unwrap_or_default();
    emu.start_sound_log(SoundLogContents {
        writes: true,
        keys: false,
        mailbox: false,
    });
    let mut whole: Option<SoundLog> = None;
    for frame in 0..frames {
        plan.before_frame(&mut emu, frame);
        render_frames(&mut emu, 1);
        let _ = emu.get_audio_samples();
        let take = emu
            .take_sound_log()
            .context("the log was on and took nothing")?;
        match whole.as_mut() {
            None => whole = Some(take),
            Some(whole) => {
                whole.entries.extend(take.entries);
                whole.interrupted |= take.interrupted;
            }
        }
    }
    emu.stop_sound_log();
    let log = whole.context("no frame was run")?;
    Ok((log, emu, title))
}

/// The index of the first entry of frame `frame`: the entry after the
/// frame's end before it.
fn entry_of_frame(log: &SoundLog, frame: u64) -> Option<usize> {
    if frame == 0 {
        return Some(0);
    }
    log.entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.event == SoundEvent::FrameEnd)
        .nth(frame as usize - 1)
        .map(|(index, _)| index + 1)
        .filter(|&index| index < log.entries.len())
}

fn write(path: &PathBuf, bytes: &[u8], log: &SoundLog) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))?;
    let writes = log
        .entries
        .iter()
        .filter(|entry| matches!(entry.event, SoundEvent::Write { .. }))
        .count();
    println!(
        "Logged {writes} write(s), {} of them the chips' registers at the start; wrote {} bytes \
         to {}",
        log.image_len,
        bytes.len(),
        path.display()
    );
    if log.interrupted {
        eprintln!("note: a reset came during the run; the log goes on after it");
    }
    Ok(())
}

pub fn run_vgm(args: VgmLogArgs<GenesisBoot>) -> Result<()> {
    let (log, emu, title) = record(&args.rom, &args.input, args.frames)?;
    let loop_entry = match args.loop_frame {
        None => None,
        Some(frame) => Some(
            entry_of_frame(&log, frame)
                .with_context(|| format!("--loop-frame {frame}: the run logged no such frame"))?,
        ),
    };
    let options = VgmOptions {
        tag: Gd3Tag {
            track: args.track,
            game: args.game.unwrap_or(title),
            system: "Sega Mega Drive / Genesis".to_string(),
            author: args.author,
            date: args.date,
            ripper: args.ripper,
            notes: args.notes,
        },
        dac: match args.dac {
            DacArg::Raw => DacForm::Raw,
            DacArg::Bank => DacForm::Bank,
        },
        loop_entry,
        frame_rate: emu.get_display_info().refresh_rate.round() as u32,
    };
    let vgm = encode_vgm(&log, &emu.sound_log_chips(), &options)?;
    let bytes = match args.vgz {
        true => retro_core::audio::vgm::gzip(&vgm),
        false => vgm,
    };
    write(&args.output, &bytes, &log)
}

pub fn run_gym(args: GymLogArgs<GenesisBoot>) -> Result<()> {
    let (log, emu, title) = record(&args.rom, &args.input, args.frames)?;
    let header = args.gymx.then(|| GymHeader {
        song: args.song,
        game: args.game.unwrap_or(title),
        publisher: args.publisher,
        emulator: "sg1".to_string(),
        dumper: args.dumper,
        comment: args.comment,
        loop_frame: 0,
    });
    let gym =
        encode_gym(&log, &emu.sound_log_chips(), header.as_ref()).map_err(anyhow::Error::msg)?;
    write(&args.output, &gym, &log)
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::audio::SoundLogEntry;

    #[test]
    fn a_frame_starts_after_the_end_of_the_one_before() {
        let entry = |event| SoundLogEntry {
            time: 0,
            chip: 0,
            event,
        };
        let write = SoundEvent::Write {
            register: 0,
            value: 0,
        };
        let log = SoundLog {
            clock_hz: 1,
            entries: vec![
                entry(write),
                entry(SoundEvent::FrameEnd),
                entry(write),
                entry(SoundEvent::FrameEnd),
            ],
            image_len: 0,
            interrupted: false,
        };
        assert_eq!(entry_of_frame(&log, 0), Some(0));
        assert_eq!(entry_of_frame(&log, 1), Some(2));
        assert_eq!(
            entry_of_frame(&log, 2),
            None,
            "nothing after the last frame's end"
        );
    }
}
