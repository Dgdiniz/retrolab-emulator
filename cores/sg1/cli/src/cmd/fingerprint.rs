//! `fingerprint`: emit a per-frame CRC32 of a work-RAM region so a
//! divergence against a reference emulator can be bisected cheaply.
//!
//! A full from-boot instruction trace is enormous; a one-line-per-frame
//! checksum of the state region that actually matters (e.g. the in-RAM
//! sprite/object buffer at `$FF0000`) is tiny and pinpoints the *first*
//! frame the two emulators part ways. The region is read through
//! `read_memory_debug` in ascending 68000-address order and hashed with
//! the standard zlib CRC-32 (poly `0xEDB88320`, init/`xorout`
//! `0xFFFFFFFF`), so the value is directly comparable to the same CRC
//! taken over the same address-ordered bytes in another emulator, such as
//! a Genesis Plus GX build made to print one per frame.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};

use super::{apply_press, connect_controllers, parse_press_schedule};

#[derive(clap::Args, Debug)]
pub struct FingerprintArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Region start address (24-bit hex, default `FF0000` = Work RAM).
    #[arg(long, value_parser = parse_hex_u32, default_value = "FF0000")]
    pub addr: u32,
    /// Region length in bytes (default `10000` = the whole 64 KB Work RAM).
    #[arg(long, value_parser = parse_hex_u32, default_value = "10000")]
    pub len: u32,
    /// Number of frames to run and fingerprint.
    #[arg(long, default_value_t = 2400)]
    pub frames: u64,
    /// Connect 6-button pads instead of the default 3-button pads.
    #[arg(long)]
    pub six_button: bool,
    /// Timed player-1 input, e.g. `"180:start;186:"` (frame:buttons).
    #[arg(long)]
    pub press_at: Option<String>,
    /// Write the listing to a file instead of stdout.
    #[arg(long, short)]
    pub output: Option<PathBuf>,
}

fn parse_hex_u32(s: &str) -> Result<u32, String> {
    let cleaned = s.trim().trim_start_matches("0x").trim_start_matches('$');
    u32::from_str_radix(cleaned, 16).map_err(|e| format!("invalid hex value '{s}': {e}"))
}

pub fn run(args: FingerprintArgs) -> Result<()> {
    if args.len == 0 {
        anyhow::bail!("--len must be > 0");
    }
    let schedule = parse_press_schedule(args.press_at.as_deref())?;

    let (_, mut emu) = super::load_rom(&args.rom)?;
    // `load_rom` already picks 6-button from the ROM header; `--six-button`
    // only forces pads on ROMs that omit the tag.
    if args.six_button {
        connect_controllers(&mut emu, true);
    }
    // Without this, `run_frame` does not advance the CPU (the default
    // execution mode is not `Running`), so every fingerprint would be the
    // boot state. `screenshot` sets the same mode for the same reason.
    emu.debugger().set_execution_mode(ExecutionMode::Running);

    let table = crc32_table();
    let mut out = super::open_output(args.output.as_ref())?;
    writeln!(
        out,
        "; per-frame CRC32 of ${:06X}..${:06X} ({} bytes)",
        args.addr,
        args.addr + args.len,
        args.len
    )?;

    for frame in 0..args.frames {
        apply_press(&mut emu, schedule.as_ref(), frame);
        emu.run_frame();
        let data = emu.read_memory_debug(args.addr, args.len);
        let crc = crc32(&table, &data);
        writeln!(out, "F {frame} {crc:08X}")?;
    }
    out.flush()?;
    Ok(())
}

/// Build the standard reflected CRC-32 lookup table (poly `0xEDB88320`).
fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut c = i as u32;
        let mut j = 0;
        while j < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            j += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

/// zlib CRC-32 of `data` seeded at 0 (init/`xorout` = `0xFFFFFFFF`).
fn crc32(table: &[u32; 256], data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_forms() {
        assert_eq!(parse_hex_u32("FF0000").unwrap(), 0xFF_0000);
        assert_eq!(parse_hex_u32("0x10000").unwrap(), 0x1_0000);
        assert!(parse_hex_u32("zz").is_err());
    }

    #[test]
    fn crc32_matches_known_zlib_vectors() {
        let t = crc32_table();
        // Canonical zlib CRC-32 test vectors.
        assert_eq!(crc32(&t, b""), 0x0000_0000);
        assert_eq!(crc32(&t, b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(&t, b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    #[test]
    fn crc32_all_zero_block_is_stable() {
        let t = crc32_table();
        // A 4 KB zero page has a fixed, well-known CRC, handy as a sanity
        // check when bisecting.
        let zeros = [0u8; 0x1000];
        let c = crc32(&t, &zeros);
        assert_eq!(c, crc32(&t, &zeros));
        assert_ne!(c, 0);
    }
}
