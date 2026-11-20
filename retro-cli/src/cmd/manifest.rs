//! `manifest detect` / `manifest validate`: a ROM's cartridge manifest
//! (`cartridge-manifest-v1`, the JSON that describes its mapping and its
//! chips) as the core's detection gives it, for a person to edit; and a
//! manifest checked, as the core checks one a host stages, and with
//! `--rom` the ROM loaded with it, so the core's builder runs on it, chips
//! included, and its `rom.sha256` held to the ROM image.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use retro_core::cartridge::manifest::MANIFEST_SCHEMA;
use retro_core::golden::GoldenConsole;
use retro_core::EmulatorCore;

use crate::console::Console;
use crate::report::write_memory_map;
use crate::rom::{load_image, read_rom, write_banner, ManifestFile};

#[derive(clap::Args, Debug)]
pub struct ManifestArgs<B: clap::Args> {
    #[command(subcommand)]
    pub action: ManifestAction<B>,
}

#[derive(clap::Subcommand, Debug)]
pub enum ManifestAction<B: clap::Args> {
    /// Write the cartridge manifest the core's detection gives a ROM, as
    /// JSON: its mapping and its chips, which `--manifest` loads it with
    /// once edited.
    Detect(DetectArgs),
    /// Check a cartridge manifest: its JSON, its version and the console's
    /// limits; with `--rom`, load that ROM with it, which builds its preset,
    /// its slots and its chips.
    Validate(ValidateArgs<B>),
}

#[derive(clap::Args, Debug)]
pub struct DetectArgs {
    /// Path to the ROM file.
    pub rom: PathBuf,
    /// Write the manifest to a file instead of stdout.
    #[arg(long, short)]
    pub output: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct ValidateArgs<B: clap::Args> {
    /// The manifest file (`cartridge-manifest-v1` JSON).
    pub manifest: PathBuf,
    /// Load this ROM with the manifest, the core's builder run on it,
    /// chips included, and hold the manifest's `rom.sha256` to the ROM
    /// image.
    #[arg(long, value_name = "ROM")]
    pub rom: Option<PathBuf>,
    #[command(flatten)]
    pub boot: B,
}

pub fn run<C: Console>(args: ManifestArgs<C::BootArgs>) -> Result<()> {
    match args.action {
        ManifestAction::Detect(args) => detect::<C>(args),
        ManifestAction::Validate(args) => validate::<C>(args),
    }
}

fn detect<C: Console>(args: DetectArgs) -> Result<()> {
    let rom = read_rom(&args.rom)?;
    let manifest = C::Emulator::new_for_golden().detected_manifest(&rom);
    let json = serde_json::to_string_pretty(&manifest).context("failed to write the manifest")?;
    match &args.output {
        Some(path) => {
            std::fs::write(path, format!("{json}\n"))
                .with_context(|| format!("failed to write {}", path.display()))?;
            eprintln!("Manifest written to: {}", path.display());
        }
        None => println!("{json}"),
    }
    Ok(())
}

fn validate<C: Console>(args: ValidateArgs<C::BootArgs>) -> Result<()> {
    let file = ManifestFile::read(&args.manifest)?;
    file.parse::<C::Emulator>()?;
    let json = file.json()?;
    let text = |value: &serde_json::Value| value.as_str().unwrap_or("").to_string();
    let count = |value: &serde_json::Value| value.as_array().map_or(0, Vec::len);
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "{}: {MANIFEST_SCHEMA} {}, preset {}, {} slot(s), {} chip(s)",
        args.manifest.display(),
        text(&json["version"]),
        text(&json["mapping"]["preset"]),
        count(&json["mapping"]["slots"]),
        count(&json["chips"]),
    )?;
    if let Some(rom_path) = &args.rom {
        let rom = read_rom(rom_path)?;
        let mut emu = load_image::<C>(&rom, &args.boot, Some(&file))?;
        write_banner(&mut emu, &mut out)?;
        // The SNES's builder skips the digest of a manifest a host stages,
        // and the Mega Drive's never reads it: the tool holds it to the
        // image the cartridge hashed (a copier header left out).
        let expected = text(&json["rom"]["sha256"]).to_ascii_lowercase();
        let actual = emu
            .cartridge_info()
            .map(|info| info.rom_hash.clone())
            .unwrap_or_default();
        if expected.is_empty() {
            writeln!(out, "sha256 not checked (the manifest gives none)")?;
        } else if expected != actual {
            anyhow::bail!("rom.sha256 {expected} is not the ROM image's {actual}");
        } else {
            writeln!(out, "rom.sha256 is the ROM image's")?;
        }
        write_memory_map(&mut emu, &mut out)?;
    } else {
        writeln!(
            out,
            "preset, slots and chips not built (give --rom to build them on a ROM)"
        )?;
    }
    writeln!(out, "valid")?;
    out.flush()?;
    Ok(())
}
