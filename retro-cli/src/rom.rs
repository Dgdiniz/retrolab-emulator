//! The ROM a command loads, the cartridge manifest it may load it with,
//! and the line it prints about what it loaded.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use retro_core::golden::GoldenConsole;
use retro_core::EmulatorCore;

use crate::console::Console;

/// The ROM a command loads, the manifest `--manifest` names, and the
/// tool's start-up flags (`B`, the console's [`Console::BootArgs`]).
#[derive(clap::Args, Clone, Debug)]
pub struct RomArgs<B: clap::Args> {
    /// Path to the ROM file; the first argument of every command that
    /// takes one.
    #[arg(id = "rom", value_name = "ROM", index = 1)]
    pub path: PathBuf,
    /// Load the ROM with this cartridge manifest (`cartridge-manifest-v1`
    /// JSON, such as `manifest detect` writes) in place of the core's
    /// detection of its mapping and chips.
    #[arg(long, value_name = "FILE")]
    pub manifest: Option<PathBuf>,
    #[command(flatten)]
    pub boot: B,
}

impl<B: clap::Args + Default> RomArgs<B> {
    /// The ROM at `path`, with no manifest and the start-up flags at their
    /// defaults.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            manifest: None,
            boot: B::default(),
        }
    }
}

/// The bytes of the ROM file at `path`.
pub fn read_rom(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("failed to read ROM: {}", path.display()))
}

/// A cartridge manifest's file, read once: a command that makes several
/// machines parses it for each.
#[derive(Clone, Debug)]
pub struct ManifestFile {
    path: PathBuf,
    text: String,
}

impl ManifestFile {
    /// The manifest file at `path`.
    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read manifest: {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            text,
        })
    }

    /// The file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The manifest of `E`'s console the file holds, checked as the core
    /// checks a manifest a host stages; the error names the file.
    pub fn parse<E: EmulatorCore>(&self) -> Result<E::Manifest> {
        E::parse_manifest(&self.text).map_err(|e| anyhow::anyhow!("{}: {e}", self.path.display()))
    }

    /// The manifest as JSON, for the fields every console's has.
    pub fn json(&self) -> Result<serde_json::Value> {
        serde_json::from_str(&self.text)
            .with_context(|| format!("{}: invalid JSON", self.path.display()))
    }
}

/// The manifest `--manifest` names, read; `None` without one.
pub fn read_manifest(path: Option<&Path>) -> Result<Option<ManifestFile>> {
    path.map(ManifestFile::read).transpose()
}

/// Read the ROM and load it into a new machine, with the manifest
/// `--manifest` names and the start-up flags set around the load as the
/// console sets them; the ROM's bytes and the machine.
pub fn load<C: Console>(args: &RomArgs<C::BootArgs>) -> Result<(Vec<u8>, C::Emulator)> {
    let rom = read_rom(&args.path)?;
    let manifest = read_manifest(args.manifest.as_deref())?;
    let emu = load_image::<C>(&rom, &args.boot, manifest.as_ref())?;
    Ok((rom, emu))
}

/// A new machine with the ROM image `rom` loaded: the start-up flags set
/// as the console sets them, then `manifest` staged in place of the core's
/// detection, then the load.
pub fn load_image<C: Console>(
    rom: &[u8],
    boot: &C::BootArgs,
    manifest: Option<&ManifestFile>,
) -> Result<C::Emulator> {
    let mut emu = C::Emulator::new_for_golden();
    C::stage(&mut emu, rom, boot, manifest.is_some())?;
    if let Some(manifest) = manifest {
        let parsed = manifest.parse::<C::Emulator>()?;
        emu.set_cartridge_manifest_override(Some(parsed));
    }
    emu.load_rom(rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;
    C::after_load(&mut emu, boot)?;
    Ok(emu)
}

/// Write what was loaded: `Loaded "TITLE" [REGION]`, then the cartridge's
/// chips, `Coprocessor: LABEL` for one, `Coprocessors: L1, L2` for more.
pub fn write_banner<E: EmulatorCore>(emu: &mut E, out: &mut dyn Write) -> std::io::Result<()> {
    let title = emu.get_rom_info().map(|info| (info.title, info.region));
    let chips: Vec<String> = emu
        .cartridge_debugger()
        .get_chip_labels()
        .into_iter()
        .map(|(_, label)| label)
        .collect();
    for line in banner_lines(
        title.as_ref().map(|(t, r)| (t.as_str(), r.as_str())),
        &chips,
    ) {
        writeln!(out, "{line}")?;
    }
    Ok(())
}

/// The banner's lines for a ROM's title and region and its chips' labels.
fn banner_lines(title: Option<(&str, &str)>, chips: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some((title, region)) = title {
        lines.push(format!("Loaded {:?} [{region}]", title.trim()));
    }
    match chips {
        [] => {}
        [chip] => lines.push(format!("Coprocessor: {chip}")),
        many => lines.push(format!("Coprocessors: {}", many.join(", "))),
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_names_the_rom_and_its_chips() {
        assert_eq!(
            banner_lines(Some(("SUPER MARIOWORLD     ", "USA")), &[]),
            vec![r#"Loaded "SUPER MARIOWORLD" [USA]"#]
        );
        assert_eq!(
            banner_lines(Some(("STARFOX", "USA")), &["Super FX".to_string()]),
            vec![r#"Loaded "STARFOX" [USA]"#, "Coprocessor: Super FX"]
        );
        assert_eq!(
            banner_lines(None, &["SPC7110".to_string(), "RTC-4513".to_string()]),
            vec!["Coprocessors: SPC7110, RTC-4513"]
        );
    }
}
