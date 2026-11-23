//! `stems`: run a ROM for a number of frames with the sound's stems armed,
//! and write each stem and the master as a WAV at the console's own rate,
//! with `stems.json` saying what each file holds, how the stems add up to
//! the master, and where they depart from it.

use std::path::PathBuf;

use anyhow::{Context, Result};
use retro_core::audio::{
    encode_wav_samples, render_frames, SampleSpan, SoundCore, SoundSamples, StemLayout, StemSet,
    StemTake,
};
use serde_json::json;

use crate::console::Console;
use crate::input::InputArgs;
use crate::progress;
use crate::rom::{load, write_banner, RomArgs};

#[derive(clap::Args, Debug)]
pub struct StemsArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Number of frames to run while recording, e.g. `300` (about 5 s at
    /// 60 fps).
    #[arg(long, short)]
    pub frames: Option<u64>,
    /// The folder the files go to; made when it is not there.
    #[arg(long, short, default_value = "stems")]
    pub output: PathBuf,
    /// The stems to write, comma-separated, e.g. `fm1,dac`; every stem by
    /// default. The master is always written.
    #[arg(long, value_delimiter = ',')]
    pub only: Vec<String>,
    /// Show a progress bar (on stderr).
    #[arg(long)]
    pub progress: bool,
    #[command(flatten)]
    pub input: InputArgs,
}

pub fn run<C: Console>(args: StemsArgs<C::BootArgs>) -> Result<()> {
    let Some(frames) = args.frames else {
        anyhow::bail!("specify --frames");
    };
    let input = args.input.plan::<C::Emulator>()?;
    let (_, mut emu) = load::<C>(&args.rom)?;
    let Some(layout) = emu.stem_layout() else {
        anyhow::bail!("{} takes no stems", C::TOOL);
    };
    let armed = chosen(&layout, &args.only)?;
    write_banner(&mut emu, &mut std::io::stdout())?;

    let take = record(
        &mut emu,
        armed,
        frames,
        |emu, frame| input.before_frame(emu, frame),
        args.progress,
    )?;

    std::fs::create_dir_all(&args.output)
        .with_context(|| format!("failed to create {}", args.output.display()))?;
    let mut files = Vec::new();
    for (stem, index) in take.stems.iter().zip(take.armed.iter()) {
        let channel = &layout.channels[index];
        let file = format!("{}.wav", channel.name);
        write_wav(&args.output, &file, stem, layout.rate, channel.width())?;
        files.push(json!({
            "file": file,
            "name": channel.name,
            "chip": channel.chip,
            "channel": channel.channel,
            "stereo": channel.stereo,
            "in_master": channel.in_master,
        }));
    }
    write_wav(&args.output, "master.wav", &take.master, layout.rate, 2)?;
    let departures: Vec<[u32; 2]> = take
        .departures
        .iter()
        .map(|span| [span.start, span.frames])
        .collect();
    let departed: u64 = take
        .departures
        .iter()
        .map(|span| u64::from(span.frames))
        .sum();
    let summary = json!({
        "rate": layout.rate,
        "kind": layout.kind,
        "frames": take.frames,
        "sum": layout.sum,
        "stems": files,
        "master": { "file": "master.wav", "stereo": true },
        "departures": departures,
        "dropped": take.dropped,
    });
    let path = args.output.join("stems.json");
    let text = serde_json::to_string_pretty(&summary)?;
    std::fs::write(&path, format!("{text}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;

    println!(
        "Recorded {} sample frames ({:.2}s @ {} Hz) of {} stem(s) and the master into {}",
        take.frames,
        f64::from(take.frames) / f64::from(layout.rate.max(1)),
        layout.rate,
        take.armed.len(),
        args.output.display()
    );
    println!(
        "Sum: {:?}; {departed} sample frame(s) depart from it in {} run(s)",
        layout.sum,
        take.departures.len()
    );
    Ok(())
}

/// The stems `only` names, every stem when it names none.
pub fn chosen(layout: &StemLayout, only: &[String]) -> Result<StemSet> {
    if only.is_empty() {
        return Ok(layout.all());
    }
    only.iter().try_fold(StemSet::EMPTY, |set, name| {
        let Some(index) = layout.index_of(name.trim()) else {
            let names: Vec<&str> = layout.channels.iter().map(|stem| stem.name).collect();
            anyhow::bail!("no stem `{name}`: the stems are {}", names.join(", "));
        };
        Ok(set.with(index))
    })
}

/// Run `frames` frames of the player's loop with `armed` recorded, the
/// sound on, `before_frame` called before each: every frame's take joined
/// into one, its departures counted from the first frame.
pub fn record<E: SoundCore>(
    emu: &mut E,
    armed: StemSet,
    frames: u64,
    mut before_frame: impl FnMut(&mut E, u64),
    show_progress: bool,
) -> Result<StemTake> {
    emu.arm_stems(armed);
    let bar = progress::bar(show_progress, frames, "frames");
    let mut whole: Option<StemTake> = None;
    for frame in 0..frames {
        before_frame(emu, frame);
        render_frames(emu, 1);
        // The host's ring is not what this records: it is emptied so that
        // it never fills.
        let _ = emu.get_audio_samples();
        let take = emu
            .take_stems()
            .context("the stems were armed and the machine took none")?;
        whole = Some(match whole {
            None => take,
            Some(whole) => join(whole, take),
        });
        if let Some(bar) = &bar {
            bar.inc(1);
        }
    }
    progress::finish(bar);
    emu.arm_stems(StemSet::EMPTY);
    whole.context("no frame was run")
}

/// `later` after `earlier`: the buffers joined, the departures counted on.
fn join(mut earlier: StemTake, later: StemTake) -> StemTake {
    for (into, from) in earlier.stems.iter_mut().zip(later.stems) {
        extend(into, from);
    }
    extend(&mut earlier.master, later.master);
    let offset = earlier.frames;
    for span in later.departures {
        let start = span.start + offset;
        match earlier.departures.last_mut() {
            Some(last) if last.end() == start => last.frames += span.frames,
            _ => earlier.departures.push(SampleSpan {
                start,
                frames: span.frames,
            }),
        }
    }
    earlier.frames += later.frames;
    earlier.dropped += later.dropped;
    earlier
}

fn extend(into: &mut SoundSamples, from: SoundSamples) {
    match (into, from) {
        (SoundSamples::I16(into), SoundSamples::I16(from)) => into.extend(from),
        (SoundSamples::F32(into), SoundSamples::F32(from)) => into.extend(from),
        _ => unreachable!("a machine's stems are of one kind"),
    }
}

fn write_wav(
    folder: &std::path::Path,
    file: &str,
    samples: &SoundSamples,
    rate: u32,
    channels: usize,
) -> Result<()> {
    let path = folder.join(file);
    std::fs::write(&path, encode_wav_samples(samples, rate, channels as u16))
        .with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::audio::{SampleKind, StemChannel, StemSum};

    fn layout() -> StemLayout {
        let stem = |name| StemChannel {
            name,
            chip: Some(0),
            channel: Some(0),
            stereo: false,
            in_master: true,
        };
        StemLayout {
            rate: 1000,
            kind: SampleKind::I16,
            channels: vec![stem("fm1"), stem("dac")],
            master: stem("master"),
            sum: StemSum::Exact,
        }
    }

    #[test]
    fn only_names_stems_of_the_layout() {
        let layout = layout();
        assert_eq!(chosen(&layout, &[]).unwrap(), layout.all());
        let dac = chosen(&layout, &["dac".to_string()]).unwrap();
        assert_eq!(dac.iter().collect::<Vec<_>>(), [1]);
        let error = chosen(&layout, &["fm7".to_string()]).unwrap_err();
        assert_eq!(error.to_string(), "no stem `fm7`: the stems are fm1, dac");
    }

    #[test]
    fn takes_join_with_their_departures_counted_on() {
        let take = |frames, start, samples: Vec<i16>| StemTake {
            frames,
            armed: StemSet::all(1),
            stems: vec![SoundSamples::I16(samples.clone())],
            master: SoundSamples::I16(samples.iter().flat_map(|&s| [s, s]).collect()),
            departures: vec![SampleSpan { start, frames: 1 }],
            dropped: 0,
        };
        let joined = join(take(2, 1, vec![1, 2]), take(2, 0, vec![3, 4]));
        assert_eq!(joined.frames, 4);
        assert_eq!(joined.stems, [SoundSamples::I16(vec![1, 2, 3, 4])]);
        assert_eq!(
            joined.departures,
            [SampleSpan {
                start: 1,
                frames: 2
            }]
        );
        assert_eq!(joined.master.len(), 8);
    }
}
