//! `spc-dump`: run a ROM and write the APU's state as an SPC file, which an
//! SPC player plays: at the end of the frames run, or at the first key-on
//! after them.
//!
//! A note sounding when the file is taken is not in it (the file holds no
//! voice's place in its sample nor its envelope): a player starts from the
//! voices KON names and plays on from the program's next key-on. Taken at
//! a key-on, the file starts with that note.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use retro_cli::rom::{load, RomArgs};
use retro_cli::InputArgs;
use retro_core::audio::render_frames;
use retro_core::EmulatorCore;
use snes1_core::sound::Id666;
use snes1_core::SnesEmulator;

use crate::console::{Snes, SnesBoot};

#[derive(Args, Debug)]
pub struct SpcDumpArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Number of frames to run, e.g. `600` (10 s at 60 fps): the file is
    /// the APU at their end, or, with `--at-key-on`, at the first key-on
    /// after them.
    #[arg(long, short)]
    pub frames: Option<u64>,
    /// Take the file at the first key-on after the frames run: the APU as
    /// it stands after the SPC700 instruction that writes KON.
    #[arg(long)]
    pub at_key_on: bool,
    /// With `--at-key-on`, the frames to wait for the key-on before giving
    /// up.
    #[arg(long, default_value_t = 600)]
    pub within: u64,
    /// Output file path.
    #[arg(long, short, default_value = "sound.spc")]
    pub output: PathBuf,
    /// The song's title, in the file's ID666 tag.
    #[arg(long, default_value = "")]
    pub title: String,
    /// The game's name; the ROM header's title by default.
    #[arg(long)]
    pub game: Option<String>,
    /// The composer.
    #[arg(long, default_value = "")]
    pub artist: String,
    /// Who made the file.
    #[arg(long, default_value = "")]
    pub dumper: String,
    /// A comment.
    #[arg(long, default_value = "")]
    pub comment: String,
    /// The day the file was made, `MM/DD/YYYY`.
    #[arg(long, default_value = "")]
    pub date: String,
    /// The seconds a player plays before it fades out.
    #[arg(long, default_value_t = 0)]
    pub seconds: u32,
    /// The fade's length, in milliseconds.
    #[arg(long, default_value_t = 0)]
    pub fade_ms: u32,
    #[command(flatten)]
    pub input: InputArgs,
}

pub fn run(args: SpcDumpArgs<SnesBoot>) -> Result<()> {
    let Some(frames) = args.frames else {
        anyhow::bail!("specify --frames");
    };
    let plan = args.input.plan::<SnesEmulator>()?;
    let (_, mut emu) = load::<Snes>(&args.rom)?;
    let title = emu
        .get_rom_info()
        .map(|info| info.title.trim().to_string())
        .unwrap_or_default();
    let tag = Id666 {
        title: args.title,
        game: args.game.unwrap_or(title),
        dumper: args.dumper,
        comment: args.comment,
        date: args.date,
        seconds: args.seconds,
        fade_ms: args.fade_ms,
        artist: args.artist,
    };
    for frame in 0..frames {
        plan.before_frame(&mut emu, frame);
        render_frames(&mut emu, 1);
        let _ = emu.get_audio_samples();
    }
    let (bytes, when) = if args.at_key_on {
        emu.arm_spc_at_key_on();
        let mut kept = None;
        for frame in frames..frames + args.within {
            plan.before_frame(&mut emu, frame);
            render_frames(&mut emu, 1);
            let _ = emu.get_audio_samples();
            if let Some(bytes) = emu.take_key_on_spc(&tag) {
                kept = Some((bytes, format!("at the key-on in frame {frame}")));
                break;
            }
        }
        kept.with_context(|| {
            format!(
                "no voice was keyed on in the {} frames after frame {frames}",
                args.within
            )
        })?
    } else {
        (emu.export_spc(&tag), format!("after frame {frames}"))
    };
    std::fs::write(&args.output, &bytes)
        .with_context(|| format!("failed to write {}", args.output.display()))?;
    println!(
        "Wrote the APU {when}: {} bytes to {}",
        bytes.len(),
        args.output.display()
    );
    Ok(())
}
