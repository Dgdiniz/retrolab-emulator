//! `golden` / `golden-set` / `golden-compare`: record what the core
//! produces, frame by frame, and hold a later build to it.
//!
//! `golden` runs one ROM through [`retro_core::golden::run_golden`] and
//! writes a folder: `hashes.txt` (one line per frame: the state, picture
//! and audio hashes), `frame-NNNNN.png` at the listed shots, optionally
//! `audio.wav`, and `summary.txt`. `golden-set` does that for every entry
//! of a manifest into `<out>/<name>/`, skipping the ROMs that are not on
//! disk, each entry loaded as the golden tests load it
//! ([`GoldenConsole::load_for_golden`]), so a record it writes is the one
//! a test holds. `golden-compare` reads two such folders (or two set
//! folders) and reports where each run first parts from the other and how
//! many pixels differ on each shot. It is the gate every rewrite of a
//! module passes: the same output, frame for frame.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use retro_core::golden::{
    entry_spec_of, first_divergence, format_records_of, parse_manifest_of, parse_records_of,
    run_golden, FrameRecord, GoldenConsole, GoldenSpec,
};
use retro_core::{fnv1a64, EmulatorCore};

use crate::console::Console;
use crate::input::{parse_press_schedule, parse_reset_frames};
use crate::parse::parse_frame_list;
use crate::rom::{load, read_rom, RomArgs};

#[derive(clap::Args, Debug)]
pub struct GoldenArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Frames to run, e.g. `600`.
    #[arg(long)]
    pub frames: Option<u64>,
    /// Frame counts at which to write a PNG (comma-separated, e.g.
    /// `60,300,600`); default: the last frame.
    #[arg(long)]
    pub shots: Option<String>,
    /// Frame-scheduled player-1 input: `"F:buttons;F:buttons"`.
    #[arg(long)]
    pub press_at: Option<String>,
    /// Inject a console `reset()` before these frames (comma-separated).
    #[arg(long)]
    pub reset_at: Option<String>,
    /// Also write the captured audio as `audio.wav`.
    #[arg(long)]
    pub wav: bool,
    /// Output folder (created if missing).
    #[arg(long, short, visible_alias = "output")]
    pub out: PathBuf,
}

#[derive(clap::Args, Debug)]
pub struct GoldenSetArgs {
    /// The manifest: one ROM per line, as `name=... rom=... frames=...`.
    pub manifest: PathBuf,
    /// Output folder; each entry writes into `<out>/<name>/`.
    #[arg(long, short, visible_alias = "output")]
    pub out: PathBuf,
    /// Root the manifest's ROM paths are relative to (default: the
    /// current directory, i.e. run from the repository's root).
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Run only the entries whose name contains this text.
    #[arg(long)]
    pub only: Option<String>,
    /// Also write each entry's audio as `audio.wav`.
    #[arg(long)]
    pub wav: bool,
}

#[derive(clap::Args, Debug)]
pub struct GoldenCompareArgs {
    /// A run folder (holding `hashes.txt`) or a set folder of them.
    pub a: PathBuf,
    /// The folder to compare it with, of the same kind.
    pub b: PathBuf,
}

/// How a run was asked for, as its record's header names it.
struct RunHeader<'a> {
    press_at: Option<&'a str>,
    reset_at: Option<&'a str>,
    /// The console's golden flags that were set.
    flags: Vec<&'a str>,
    /// The cartridge manifest the ROM was loaded with.
    manifest: Option<&'a Path>,
}

pub fn run<C: Console>(args: GoldenArgs<C::BootArgs>) -> Result<()> {
    let Some(frames) = args.frames else {
        anyhow::bail!("specify --frames");
    };
    let mut shots = parse_frame_list(args.shots.as_deref().unwrap_or(""))
        .map_err(|e| anyhow::anyhow!("--shots: {e}"))?;
    if shots.is_empty() {
        shots.insert(frames);
    }
    let spec = GoldenSpec {
        frames,
        shots,
        presses: parse_press_schedule::<C::Emulator>(args.press_at.as_deref())?,
        resets: parse_reset_frames(args.reset_at.as_deref())?,
    };
    let name = args
        .rom
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rom".to_string());
    let (rom, emu) = load::<C>(&args.rom)?;
    let header = RunHeader {
        press_at: args.press_at.as_deref(),
        reset_at: args.reset_at.as_deref(),
        flags: C::golden_flags(&args.rom.boot),
        manifest: args.rom.manifest.as_deref(),
    };
    let outcome = record::<C>(
        emu,
        &name,
        &args.rom.path,
        &rom,
        &spec,
        &header,
        args.wav,
        &args.out,
    )?;
    println!("{outcome}");
    Ok(())
}

pub fn run_set<C: Console>(args: GoldenSetArgs) -> Result<()> {
    let text = fs::read_to_string(&args.manifest)
        .with_context(|| format!("failed to read manifest: {}", args.manifest.display()))?;
    let entries = parse_manifest_of::<C::Emulator>(&text).map_err(anyhow::Error::msg)?;
    let root = args.root.clone().unwrap_or_else(|| PathBuf::from("."));
    let (mut ran, mut skipped, mut failed) = (0usize, 0usize, 0usize);
    for entry in &entries {
        if let Some(only) = &args.only {
            if !entry.name.contains(only.as_str()) {
                continue;
            }
        }
        let rom_path = root.join(&entry.rom);
        if !rom_path.is_file() {
            println!(
                "{:<28} skipped - ROM not on disk ({})",
                entry.name,
                rom_path.display()
            );
            skipped += 1;
            continue;
        }
        let spec = entry_spec_of::<C::Emulator>(entry).map_err(anyhow::Error::msg)?;
        let header = RunHeader {
            press_at: entry.press_at.as_deref(),
            reset_at: entry.reset_at.as_deref(),
            flags: <C::Emulator as GoldenConsole>::FLAGS
                .iter()
                .copied()
                .filter(|flag| entry.has_flag(flag))
                .collect(),
            manifest: None,
        };
        let outcome = read_rom(&rom_path).and_then(|rom| {
            let mut emu = C::Emulator::new_for_golden();
            emu.load_for_golden(entry, &rom)
                .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;
            record::<C>(
                emu,
                &entry.name,
                &rom_path,
                &rom,
                &spec,
                &header,
                args.wav,
                &args.out.join(&entry.name),
            )
        });
        match outcome {
            Ok(outcome) => {
                println!("{outcome}");
                ran += 1;
            }
            Err(e) => {
                println!("{:<28} FAILED - {e:#}", entry.name);
                failed += 1;
            }
        }
    }
    println!(
        "golden-set: {ran} ran, {skipped} skipped, {failed} failed -> {}",
        args.out.display()
    );
    if failed > 0 {
        anyhow::bail!(
            "{failed} entr{} failed",
            if failed == 1 { "y" } else { "ies" }
        );
    }
    Ok(())
}

/// Run a loaded ROM through `spec` and write its folder. Returns the
/// one-line summary.
#[allow(clippy::too_many_arguments)]
fn record<C: Console>(
    mut emu: C::Emulator,
    name: &str,
    rom_path: &Path,
    rom: &[u8],
    spec: &GoldenSpec,
    asked: &RunHeader<'_>,
    wav: bool,
    out: &Path,
) -> Result<String> {
    let title = emu
        .get_rom_info()
        .map(|i| i.title.trim().to_string())
        .unwrap_or_default();

    fs::create_dir_all(out).with_context(|| format!("failed to create {}", out.display()))?;
    let mut shot_errors: Vec<String> = Vec::new();
    let mut samples: Vec<f32> = Vec::new();
    let records = run_golden(
        &mut emu,
        spec,
        |frame, w, h, rgb| {
            let path = out.join(format!("frame-{frame:05}.png"));
            match image::RgbImage::from_raw(w, h, rgb.to_vec()) {
                Some(img) => {
                    if let Err(e) = img.save(&path) {
                        shot_errors.push(format!("{}: {e}", path.display()));
                    }
                }
                None => shot_errors.push(format!("{}: {w}x{h} buffer mismatch", path.display())),
            }
        },
        |s| {
            if wav {
                samples.extend_from_slice(s);
            }
        },
    );
    if let Some(e) = shot_errors.first() {
        anyhow::bail!("failed to write a shot: {e}");
    }
    if wav {
        let rate = emu.get_audio_info().sample_rate;
        let bytes = crate::audio::encode_wav(&samples, rate, 2);
        fs::write(out.join("audio.wav"), bytes)
            .with_context(|| format!("failed to write {}", out.join("audio.wav").display()))?;
    }

    let header = header_lines(
        rom_path,
        rom,
        &title,
        spec,
        asked,
        <C::Emulator as GoldenConsole>::FLAGS,
    );
    fs::write(
        out.join("hashes.txt"),
        format_records_of::<C::Emulator>(&header, &records),
    )
    .with_context(|| format!("failed to write {}", out.join("hashes.txt").display()))?;

    let last = records.last();
    let total_samples: u64 = records.iter().map(|r| u64::from(r.samples)).sum();
    let summary = format!(
        "{name:<28} {} frames  state {}  video {}  samples {total_samples}  {}x{}",
        records.len(),
        last.map(|r| format!("{:016x}", r.state))
            .unwrap_or_default(),
        last.map(|r| format!("{:016x}", r.video))
            .unwrap_or_default(),
        last.map(|r| r.width).unwrap_or(0),
        last.map(|r| r.height).unwrap_or(0),
    );
    fs::write(out.join("summary.txt"), format!("{summary}\n"))
        .with_context(|| format!("failed to write {}", out.join("summary.txt").display()))?;
    Ok(summary)
}

/// The record's header: the ROM file (its name, size, FNV-1a and title,
/// and the manifest it was loaded with), then how the run was asked for,
/// each of the console's golden flags (`all_flags`) with whether it was
/// set.
fn header_lines(
    rom_path: &Path,
    rom: &[u8],
    title: &str,
    spec: &GoldenSpec,
    asked: &RunHeader<'_>,
    all_flags: &[&str],
) -> Vec<String> {
    let mut file = format!(
        "rom={} size={} fnv={:016x} title={title:?}",
        rom_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        rom.len(),
        fnv1a64(rom),
    );
    if let Some(manifest) = asked.manifest {
        file.push_str(&format!(" manifest={:?}", manifest.display().to_string()));
    }
    let mut run = format!(
        "frames={} shots={} press-at={} reset-at={}",
        spec.frames,
        spec.shots
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join(","),
        asked.press_at.unwrap_or("-"),
        asked.reset_at.unwrap_or("-"),
    );
    for flag in all_flags {
        run.push_str(&format!(" {flag}={}", asked.flags.contains(flag)));
    }
    vec![file, run]
}

pub fn run_compare<C: Console>(args: GoldenCompareArgs) -> Result<()> {
    let pairs = if args.a.join("hashes.txt").is_file() {
        vec![(
            args.a
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "run".into()),
            args.a.clone(),
            args.b.clone(),
        )]
    } else {
        let mut pairs = Vec::new();
        let mut names: Vec<String> = fs::read_dir(&args.a)
            .with_context(|| format!("failed to read {}", args.a.display()))?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("hashes.txt").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for n in names {
            pairs.push((n.clone(), args.a.join(&n), args.b.join(&n)));
        }
        if pairs.is_empty() {
            anyhow::bail!(
                "{} holds no run (no hashes.txt in it or below it)",
                args.a.display()
            );
        }
        pairs
    };

    let mut differing = 0usize;
    for (name, a, b) in pairs {
        match compare_runs::<C::Emulator>(&a, &b) {
            Ok(report) => {
                if !report.same {
                    differing += 1;
                }
                println!("{:<28} {}", name, report.line);
            }
            Err(e) => {
                differing += 1;
                println!("{name:<28} ERROR - {e:#}");
            }
        }
    }
    if differing > 0 {
        anyhow::bail!("{differing} run(s) differ");
    }
    println!("golden-compare: identical");
    Ok(())
}

struct CompareReport {
    same: bool,
    line: String,
}

/// Compare one run folder with another: the hash records, then every
/// shot present on both sides.
fn compare_runs<E: GoldenConsole>(a: &Path, b: &Path) -> Result<CompareReport> {
    let read = |dir: &Path| -> Result<Vec<FrameRecord>> {
        let path = dir.join("hashes.txt");
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let (_, records) = parse_records_of::<E>(&text).map_err(anyhow::Error::msg)?;
        Ok(records)
    };
    let ra = read(a)?;
    let rb = read(b)?;
    let mut parts: Vec<String> = Vec::new();
    let mut same = true;
    match first_divergence(&ra, &rb) {
        None => parts.push(format!("{} frames identical", ra.len())),
        Some(d) => {
            same = false;
            parts.push(format!("differs from frame {} ({})", d.frame, d.describe()));
        }
    }
    for (frame, pa) in shots_in(a)? {
        let pb = b.join(pa.file_name().unwrap_or_default());
        if !pb.is_file() {
            continue;
        }
        let ia = image::open(&pa)
            .with_context(|| format!("failed to read {}", pa.display()))?
            .to_rgb8();
        let ib = image::open(&pb)
            .with_context(|| format!("failed to read {}", pb.display()))?
            .to_rgb8();
        if ia.dimensions() != ib.dimensions() {
            same = false;
            parts.push(format!(
                "shot {frame}: {}x{} vs {}x{}",
                ia.width(),
                ia.height(),
                ib.width(),
                ib.height()
            ));
            continue;
        }
        let diff = differing_pixels(ia.as_raw(), ib.as_raw());
        if diff > 0 {
            same = false;
            parts.push(format!("shot {frame}: {diff} px differ"));
        }
    }
    Ok(CompareReport {
        same,
        line: parts.join("; "),
    })
}

/// The `frame-NNNNN.png` shots of a run folder, by frame.
fn shots_in(dir: &Path) -> Result<Vec<(u64, PathBuf)>> {
    let mut shots = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let path = entry?.path();
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(num) = name
            .strip_prefix("frame-")
            .and_then(|s| s.strip_suffix(".png"))
        {
            if let Ok(frame) = num.parse::<u64>() {
                shots.push((frame, path));
            }
        }
    }
    shots.sort();
    Ok(shots)
}

/// Count the pixels whose RGB bytes differ between two same-sized buffers.
fn differing_pixels(a: &[u8], b: &[u8]) -> usize {
    a.as_chunks::<3>()
        .0
        .iter()
        .zip(b.as_chunks::<3>().0)
        .filter(|(pa, pb)| pa != pb)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_names_the_rom_the_run_and_every_flag() {
        let spec = GoldenSpec {
            frames: 600,
            shots: [300, 600].into(),
            presses: None,
            resets: Default::default(),
        };
        let mut asked = RunHeader {
            press_at: None,
            reset_at: Some("360"),
            flags: Vec::new(),
            manifest: None,
        };
        let lines = |asked: &RunHeader<'_>| {
            header_lines(
                Path::new("roms/smw.sfc"),
                b"abc",
                "SUPER MARIOWORLD",
                &spec,
                asked,
                &["force-detect"],
            )
        };
        assert_eq!(
            lines(&asked),
            vec![
                format!(
                    "rom=smw.sfc size=3 fnv={:016x} title=\"SUPER MARIOWORLD\"",
                    fnv1a64(b"abc")
                ),
                "frames=600 shots=300,600 press-at=- reset-at=360 force-detect=false".to_string(),
            ]
        );
        asked.flags = vec!["force-detect"];
        asked.manifest = Some(Path::new("smw.json"));
        let with = lines(&asked);
        assert!(with[0].ends_with(" manifest=\"smw.json\""), "{}", with[0]);
        assert!(with[1].ends_with(" force-detect=true"), "{}", with[1]);
    }

    #[test]
    fn differing_pixels_counts_whole_pixels() {
        let a = [0u8, 0, 0, 10, 10, 10, 20, 20, 20];
        let mut b = a;
        b[4] = 11; // one channel of pixel 1
        b[6] = 21; // one channel of pixel 2
        assert_eq!(differing_pixels(&a, &b), 2);
        assert_eq!(differing_pixels(&a, &a), 0);
    }

    #[test]
    fn shots_in_reads_only_frame_pngs_in_order() {
        let dir =
            std::env::temp_dir().join(format!("retro-cli-golden-shots-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for n in [
            "frame-00300.png",
            "frame-00060.png",
            "hashes.txt",
            "frame-x.png",
        ] {
            fs::write(dir.join(n), b"").unwrap();
        }
        let shots = shots_in(&dir).unwrap();
        assert_eq!(
            shots.iter().map(|(f, _)| *f).collect::<Vec<_>>(),
            vec![60, 300]
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
