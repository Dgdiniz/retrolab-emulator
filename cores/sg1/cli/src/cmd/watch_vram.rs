//! `watch-vram`: log VDP writes that land in a VRAM address window while
//! a ROM runs. The core prints each matching write (the watch is armed
//! via `Vdp::set_watch_vram`); this command just parses the range, arms
//! it, and advances the requested frames. Useful for finding which code
//! path scribbles a nametable / sprite-table region.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use retro_cli::parse::parse_hex_range;
use retro_cli::rom::{load, RomArgs};
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};

use crate::console::{Genesis, GenesisBoot};

#[derive(Args, Debug)]
pub struct WatchVramArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// VRAM address range to watch, `START-END` inclusive or a single
    /// address (16-bit hex, e.g. `C000-CFFF`).
    #[arg(short, long)]
    pub range: String,
    /// Number of frames to run, e.g. `60`.
    #[arg(short, long)]
    pub frames: Option<u64>,
    /// Render a PNG after running.
    #[arg(long)]
    pub screenshot: Option<PathBuf>,
}

pub fn run(args: WatchVramArgs<GenesisBoot>) -> Result<()> {
    let Some(frames) = args.frames else {
        anyhow::bail!("specify --frames");
    };
    let (start, end) = parse_range(&args.range)?;

    let (_, mut emu) = load::<Genesis>(&args.rom)?;
    super::print_rom_title(&emu);

    emu.vdp_mut().set_watch_vram(start, end);
    println!("Watching VRAM writes in range 0x{start:04X}-0x{end:04X}");

    emu.debugger().set_execution_mode(ExecutionMode::Running);
    println!("Running {frames} frames...");
    for _ in 0..frames {
        emu.run_frame();
    }
    println!("Done! Executed {frames} frames");

    if let Some(ref path) = args.screenshot {
        super::save_screenshot(&emu, path)?;
    }
    Ok(())
}

/// The `--range` of VRAM addresses ([`parse_hex_range`], 16 bits wide).
fn parse_range(range: &str) -> Result<(u16, u16)> {
    let (start, end) = parse_hex_range(range, 16).map_err(|e| anyhow::anyhow!("--range: {e}"))?;
    Ok((start as u16, end as u16))
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
        assert_eq!(parse_range("C000").unwrap(), (0xC000, 0xC000));
        assert!(parse_range("ZZZZ-0000").is_err());
        assert!(parse_range("0000-10000").is_err());
    }
}
