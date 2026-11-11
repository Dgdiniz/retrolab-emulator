//! `golden` / `golden-set` / `golden-compare`: record what the core
//! produces, frame by frame, and hold a later build to it.
//!
//! `golden` runs one ROM through [`snes1_core::golden::run_golden`] and
//! writes a folder: `hashes.txt` (one line per frame: the state, picture
//! and audio hashes), `frame-NNNNN.png` at the listed shots, optionally
//! `audio.wav`, and `summary.txt`. `golden-set` does that for every entry
//! of a manifest into `<out>/<name>/`, skipping
//! the ROMs that are not on disk. `golden-compare` reads two such folders
//! (or two set folders) and reports where each run first parts from the
//! other and how many pixels differ on each shot. It is the gate every
//! rewrite of a module passes: the same output, frame for frame.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use retro_core::{fnv1a64, EmulatorCore};
use snes1_core::golden::{
    entry_spec, first_divergence, format_records, parse_manifest, parse_records, run_golden,
    FrameRecord, GoldenSpec, FORCE_DETECT,
};
use snes1_core::SnesEmulator;

use super::{
    apply_force_detect, parse_press_schedule, parse_reset_frames, read_rom, CoprocessorArgs,
};

#[derive(Args, Debug)]
pub struct GoldenArgs {
    /// Path to the ROM file (`.sfc` / `.smc`).
    pub rom: PathBuf,
    /// Frames to run.
    #[arg(long, default_value_t = 600)]
    pub frames: u64,
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
    /// Load a low-confidence ROM by forcing its vector-implied mapping.
    #[arg(long)]
    pub force_detect: bool,
    /// Also write the captured audio as `audio.wav`.
    #[arg(long)]
    pub wav: bool,
    /// Output folder (created if missing).
    #[arg(long, short)]
    pub out: PathBuf,
    /// DSP-1 firmware + coprocessor-mode flags.
    #[command(flatten)]
    pub coprocessor: CoprocessorArgs,
}

#[derive(Args, Debug)]
pub struct GoldenSetArgs {
    /// The manifest: one ROM per line, as `name=... rom=... frames=...`.
    pub manifest: PathBuf,
    /// Output folder; each entry writes into `<out>/<name>/`.
    #[arg(long, short)]
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

#[derive(Args, Debug)]
pub struct GoldenCompareArgs {
    /// A run folder (holding `hashes.txt`) or a set folder of them.
    pub a: PathBuf,
    /// The folder to compare it with, of the same kind.
    pub b: PathBuf,
}

/// Parse `--shots 60,300` into a set; `None` -> empty (the run uses its
/// last frame).
fn parse_shots(spec: Option<&str>) -> Result<BTreeSet<u64>> {
    let mut shots = BTreeSet::new();
    for s in spec
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        shots.insert(
            s.parse::<u64>()
                .with_context(|| format!("invalid --shots frame {s:?} (want an integer)"))?,
        );
    }
    Ok(shots)
}

pub fn run(args: GoldenArgs) -> Result<()> {
    let mut shots = parse_shots(args.shots.as_deref())?;
    if shots.is_empty() {
        shots.insert(args.frames);
    }
    let spec = GoldenSpec {
        frames: args.frames,
        shots,
        presses: parse_press_schedule(args.press_at.as_deref())?,
        resets: parse_reset_frames(args.reset_at.as_deref())?,
    };
    let name = args
        .rom
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rom".to_string());
    let outcome = run_one(
        &name,
        &args.rom,
        &spec,
        args.press_at.as_deref(),
        args.reset_at.as_deref(),
        args.force_detect,
        &args.coprocessor,
        args.wav,
        &args.out,
    )?;
    println!("{outcome}");
    Ok(())
}

pub fn run_set(args: GoldenSetArgs) -> Result<()> {
    let text = fs::read_to_string(&args.manifest)
        .with_context(|| format!("failed to read manifest: {}", args.manifest.display()))?;
    let entries = parse_manifest(&text).map_err(anyhow::Error::msg)?;
    let root = args.root.clone().unwrap_or_else(|| PathBuf::from("."));
    let coprocessor = CoprocessorArgs::default();
    let (mut ran, mut skipped, mut failed) = (0usize, 0usize, 0usize);
    for entry in &entries {
        if let Some(only) = &args.only {
            if !entry.name.contains(only.as_str()) {
                continue;
            }
        }
        let rom = root.join(&entry.rom);
        if !rom.is_file() {
            println!(
                "{:<28} skipped - ROM not on disk ({})",
                entry.name,
                rom.display()
            );
            skipped += 1;
            continue;
        }
        let spec = entry_spec(entry).map_err(anyhow::Error::msg)?;
        match run_one(
            &entry.name,
            &rom,
            &spec,
            entry.press_at.as_deref(),
            entry.reset_at.as_deref(),
            entry.has_flag(FORCE_DETECT),
            &coprocessor,
            args.wav,
            &args.out.join(&entry.name),
        ) {
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

/// Run one ROM and write its folder. Returns the one-line summary.
#[allow(clippy::too_many_arguments)]
fn run_one(
    name: &str,
    rom_path: &Path,
    spec: &GoldenSpec,
    press_at: Option<&str>,
    reset_at: Option<&str>,
    force_detect: bool,
    coprocessor: &CoprocessorArgs,
    wav: bool,
    out: &Path,
) -> Result<String> {
    let rom = read_rom(rom_path)?;
    let mut emu = SnesEmulator::new();
    coprocessor.apply(&mut emu)?;
    apply_force_detect(&mut emu, &rom, force_detect);
    emu.load_rom(&rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;
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
        let bytes = super::audio_dump::encode_wav(&samples, rate, 2);
        fs::write(out.join("audio.wav"), bytes)
            .with_context(|| format!("failed to write {}", out.join("audio.wav").display()))?;
    }

    let header = vec![
        format!(
            "rom={} size={} fnv={:016x} title={:?}",
            rom_path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            rom.len(),
            fnv1a64(&rom),
            title
        ),
        format!(
            "frames={} shots={} press-at={} reset-at={} force-detect={}",
            spec.frames,
            spec.shots
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join(","),
            press_at.unwrap_or("-"),
            reset_at.unwrap_or("-"),
            force_detect
        ),
    ];
    fs::write(out.join("hashes.txt"), format_records(&header, &records))
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

pub fn run_compare(args: GoldenCompareArgs) -> Result<()> {
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
        match compare_runs(&a, &b) {
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
fn compare_runs(a: &Path, b: &Path) -> Result<CompareReport> {
    let read = |dir: &Path| -> Result<Vec<FrameRecord>> {
        let path = dir.join("hashes.txt");
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let (_, records) = parse_records(&text).map_err(anyhow::Error::msg)?;
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
pub(crate) fn differing_pixels(a: &[u8], b: &[u8]) -> usize {
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
    fn shots_parse_and_default_to_empty() {
        assert_eq!(parse_shots(None).unwrap(), BTreeSet::new());
        assert_eq!(
            parse_shots(Some(" 60, 300 ,60")).unwrap(),
            BTreeSet::from([60, 300])
        );
        assert!(parse_shots(Some("60,x")).is_err());
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
        let dir = std::env::temp_dir().join(format!("snes1-golden-shots-{}", std::process::id()));
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
