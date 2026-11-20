//! The ROM a command loads, and the line it prints about what it loaded.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use retro_core::golden::GoldenConsole;
use retro_core::EmulatorCore;

use crate::console::Console;

/// The ROM a command loads, and the tool's start-up flags (`B`, the
/// console's [`Console::BootArgs`]).
#[derive(clap::Args, Clone, Debug)]
pub struct RomArgs<B: clap::Args> {
    /// Path to the ROM file; the first argument of every command that
    /// takes one.
    #[arg(id = "rom", value_name = "ROM", index = 1)]
    pub path: PathBuf,
    #[command(flatten)]
    pub boot: B,
}

impl<B: clap::Args + Default> RomArgs<B> {
    /// The ROM at `path`, with the start-up flags at their defaults.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            boot: B::default(),
        }
    }
}

/// The bytes of the ROM file at `path`.
pub fn read_rom(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("failed to read ROM: {}", path.display()))
}

/// Read the ROM and load it into a new machine, the start-up flags set
/// around the load as the console sets them; the ROM's bytes and the
/// machine.
pub fn load<C: Console>(args: &RomArgs<C::BootArgs>) -> Result<(Vec<u8>, C::Emulator)> {
    let rom = read_rom(&args.path)?;
    let emu = load_image::<C>(&rom, &args.boot)?;
    Ok((rom, emu))
}

/// A new machine with the ROM image `rom` loaded, the start-up flags set
/// around the load as the console sets them.
pub fn load_image<C: Console>(rom: &[u8], boot: &C::BootArgs) -> Result<C::Emulator> {
    let mut emu = C::Emulator::new_for_golden();
    C::stage(&mut emu, rom, boot)?;
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
