//! `memory`: boot a ROM, advance it, and dump a region of CPU-visible
//! memory as a classic offset + hex + ASCII listing.
//!
//! Addresses are 24-bit 68000 bus addresses (e.g. `FF0000` = Work RAM
//! start, `000000` = the ROM header). The dump reads through the *live*
//! bus, so reading hardware register pages (VDP `$C00000`, I/O
//! `$A10000`) can have side effects, point it at Work RAM / ROM / SRAM
//! for a faithful snapshot.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};

#[derive(clap::Args, Debug)]
pub struct MemoryArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Start address (24-bit hex, e.g. `ff0000`, `0xFF0000`, `$FF0000`).
    #[arg(value_parser = parse_hex_u32)]
    pub addr: u32,
    /// Number of bytes to dump.
    #[arg(long, default_value_t = 256)]
    pub len: u32,
    /// Frames to run before snapshotting (0 = dump the post-reset state).
    #[arg(long, default_value_t = 0)]
    pub frames: u64,
    /// Write the dump to a file instead of stdout.
    #[arg(long, short)]
    pub output: Option<PathBuf>,
}

fn parse_hex_u32(s: &str) -> Result<u32, String> {
    let cleaned = s.trim().trim_start_matches("0x").trim_start_matches('$');
    u32::from_str_radix(cleaned, 16).map_err(|e| format!("invalid hex address '{s}': {e}"))
}

pub fn run(args: MemoryArgs) -> Result<()> {
    if args.len == 0 {
        anyhow::bail!("--len must be > 0");
    }

    let (_, mut emu) = super::load_rom(&args.rom)?;
    // `run_frame` only advances the CPU in `Running` mode; without this the
    // snapshot would be the boot state regardless of `--frames`.
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    for _ in 0..args.frames {
        emu.run_frame();
    }

    let data = emu.read_memory_debug(args.addr, args.len);
    let dump = format_hex_dump(args.addr, &data);

    let header = format!(
        "; {} bytes @ ${:06X} after {} frame(s)\n",
        args.len, args.addr, args.frames
    );
    let mut out = super::open_output(args.output.as_ref())?;
    out.write_all(header.as_bytes())?;
    out.write_all(dump.as_bytes())?;
    out.flush()?;
    Ok(())
}

/// Format `data` as a hex dump: `AAAAAA  hex bytes  |ascii|`, 16 bytes per
/// row, with an extra space splitting the row into two 8-byte halves.
pub fn format_hex_dump(base_addr: u32, data: &[u8]) -> String {
    const HEX_COL_WIDTH: usize = 16 * 3 + 1;

    let mut out = String::new();
    for (row, chunk) in data.chunks(16).enumerate() {
        let addr = base_addr.wrapping_add((row * 16) as u32);

        let mut hex = String::new();
        let mut ascii = String::new();
        for (i, &b) in chunk.iter().enumerate() {
            if i == 8 {
                hex.push(' ');
            }
            hex.push_str(&format!("{b:02x} "));
            ascii.push(if (0x20..=0x7e).contains(&b) {
                b as char
            } else {
                '.'
            });
        }
        while hex.len() < HEX_COL_WIDTH {
            hex.push(' ');
        }

        out.push_str(&format!("{addr:06X}  {hex}|{ascii}|\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses() {
        assert_eq!(parse_hex_u32("ff0000").unwrap(), 0xFF_0000);
        assert_eq!(parse_hex_u32("0x00FFC0").unwrap(), 0x00_FFC0);
        assert_eq!(parse_hex_u32("$1").unwrap(), 1);
        assert!(parse_hex_u32("nope").is_err());
    }

    #[test]
    fn dumps_full_row_with_ascii_and_split() {
        let data: Vec<u8> = (0..16).collect();
        let dump = format_hex_dump(0xFF_0000, &data);
        let line = dump.lines().next().unwrap();
        assert!(line.starts_with("FF0000  00 01 02 03 04 05 06 07  08 09 0a 0b 0c 0d 0e 0f "));
        assert!(line.ends_with("|................|"));
    }

    #[test]
    fn renders_printable_ascii() {
        let dump = format_hex_dump(0, b"Hello, SG1!");
        let line = dump.lines().next().unwrap();
        assert!(line.ends_with("|Hello, SG1!|"));
        assert!(line.starts_with("000000  48 65 6c 6c 6f 2c 20 53"));
    }

    #[test]
    fn short_row_is_padded_so_ascii_aligns() {
        let dump = format_hex_dump(0, &[0x41, 0x42, 0x43]);
        let line = dump.lines().next().unwrap();
        assert!(line.contains("41 42 43"));
        assert!(line.ends_with("|ABC|"));
        let full = format_hex_dump(0, &[0u8; 16]);
        let full_bar = full.lines().next().unwrap().find('|').unwrap();
        assert_eq!(line.find('|').unwrap(), full_bar);
    }
}
