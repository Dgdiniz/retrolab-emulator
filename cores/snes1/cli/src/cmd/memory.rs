//! `memory`: boot a ROM, advance it, and dump a region of CPU-visible
//! memory as a classic offset + hex + ASCII listing.
//!
//! Addresses are 24-bit 65C816 bus addresses (e.g. `7e0000` = WRAM start,
//! `00ffc0` = the cartridge header copy). The dump reads through the *live*
//! bus, so reading hardware register pages (`$2100-$21ff`, `$4200-$43ff`) can
//! have side effects, point it at WRAM/ROM/SRAM for a faithful snapshot.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use retro_core::EmulatorCore;
use snes1_core::SnesEmulator;

#[derive(clap::Args, Debug)]
pub struct MemoryArgs {
    /// Path to the ROM file (`.sfc` / `.smc`).
    pub rom: PathBuf,
    /// Start address (24-bit hex, e.g. `7e0000`, `0x7e0000`, `$7E0000`).
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

    let rom = super::read_rom(&args.rom)?;
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;

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
/// row, with an extra space splitting the row into two 8-byte halves. The
/// address column is the absolute 24-bit address (`base + row*16`).
pub fn format_hex_dump(base_addr: u32, data: &[u8]) -> String {
    // 16 bytes x "XX " (3 chars) + 1 extra space after the 8th byte.
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
            hex.push_str(&format!("{:02x} ", b));
            ascii.push(if (0x20..=0x7e).contains(&b) {
                b as char
            } else {
                '.'
            });
        }
        while hex.len() < HEX_COL_WIDTH {
            hex.push(' ');
        }

        out.push_str(&format!("{:06X}  {}|{}|\n", addr, hex, ascii));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses() {
        assert_eq!(parse_hex_u32("7e0000").unwrap(), 0x7E_0000);
        assert_eq!(parse_hex_u32("0x00FFC0").unwrap(), 0x00_FFC0);
        assert_eq!(parse_hex_u32("$1").unwrap(), 1);
        assert!(parse_hex_u32("nope").is_err());
    }

    #[test]
    fn dumps_full_row_with_ascii_and_split() {
        let data: Vec<u8> = (0..16).collect();
        let dump = format_hex_dump(0x7E_0000, &data);
        let line = dump.lines().next().unwrap();
        // Address column + the two-half split (extra space before byte 8).
        assert!(line.starts_with("7E0000  00 01 02 03 04 05 06 07  08 09 0a 0b 0c 0d 0e 0f "));
        // Non-printable low bytes render as dots; the ASCII gutter is wrapped.
        assert!(line.ends_with("|................|"));
    }

    #[test]
    fn renders_printable_ascii() {
        let dump = format_hex_dump(0, b"Hello, SNES!");
        let line = dump.lines().next().unwrap();
        assert!(line.ends_with("|Hello, SNES!|"));
        // "Hello, S", byte 7 ('S') sits just before the two-half split.
        assert!(line.starts_with("000000  48 65 6c 6c 6f 2c 20 53"));
    }

    #[test]
    fn wraps_multiple_rows_with_absolute_addresses() {
        let data: Vec<u8> = vec![0xAA; 20];
        let dump = format_hex_dump(0x00_FFF8, &data);
        let mut lines = dump.lines();
        assert!(lines.next().unwrap().starts_with("00FFF8  aa"));
        // Second row is base + 16, wrapping the low 24 bits is not needed here.
        assert!(lines.next().unwrap().starts_with("010008  aa aa aa aa "));
    }

    #[test]
    fn short_row_is_padded_so_ascii_aligns() {
        let dump = format_hex_dump(0, &[0x41, 0x42, 0x43]);
        let line = dump.lines().next().unwrap();
        assert!(line.contains("41 42 43"));
        assert!(line.ends_with("|ABC|"));
        // The ascii gutter must start at the same column as a full row.
        let full = format_hex_dump(0, &[0u8; 16]);
        let full_bar = full.lines().next().unwrap().find('|').unwrap();
        assert_eq!(line.find('|').unwrap(), full_bar);
    }
}
