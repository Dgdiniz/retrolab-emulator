//! `watch-vram`: log VDP writes that land in a VRAM address window while
//! a ROM runs. The core prints each matching write (the watch is armed
//! via `Vdp::set_watch_vram`); this command just parses the range, arms
//! it, and advances the requested frames. Useful for finding which code
//! path scribbles a nametable / sprite-table region.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};

#[derive(Args, Debug)]
pub struct WatchVramArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// VRAM address range to watch, e.g. `C000-CFFF` (hex).
    #[arg(short, long)]
    pub range: String,
    /// Number of frames to run.
    #[arg(short, long, default_value_t = 60)]
    pub frames: u64,
    /// Render a PNG after running.
    #[arg(long)]
    pub screenshot: Option<PathBuf>,
}

pub fn run(args: WatchVramArgs) -> Result<()> {
    let (start, end) = parse_range(&args.range)?;

    let (_, mut emu) = super::load_rom(&args.rom)?;
    super::print_rom_title(&emu);

    emu.vdp_mut().set_watch_vram(start, end);
    println!("Watching VRAM writes in range 0x{start:04X}-0x{end:04X}");

    emu.debugger().set_execution_mode(ExecutionMode::Running);
    println!("Running {} frames...", args.frames);
    for _ in 0..args.frames {
        emu.run_frame();
    }
    println!("Done! Executed {} frames", args.frames);

    if let Some(ref path) = args.screenshot {
        super::save_screenshot(&emu, path)?;
    }
    Ok(())
}

/// Parse a `START-END` hex range (each side may carry a `0x`/`0X` prefix).
fn parse_range(range: &str) -> Result<(u16, u16)> {
    let (start_s, end_s) = range
        .split_once('-')
        .ok_or_else(|| anyhow::anyhow!("range must be START-END, e.g. C000-CFFF"))?;
    let parse = |s: &str| -> Result<u16> {
        let cleaned = s.trim().trim_start_matches("0x").trim_start_matches("0X");
        u16::from_str_radix(cleaned, 16).with_context(|| format!("invalid address: {s}"))
    };
    Ok((parse(start_s)?, parse(end_s)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_prefixed_ranges() {
        assert_eq!(parse_range("C000-CFFF").unwrap(), (0xC000, 0xCFFF));
        assert_eq!(parse_range("0x0000-0xFFFF").unwrap(), (0x0000, 0xFFFF));
    }

    #[test]
    fn rejects_malformed_ranges() {
        assert!(parse_range("C000").is_err());
        assert!(parse_range("ZZZZ-0000").is_err());
    }
}
