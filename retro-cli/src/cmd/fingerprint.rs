//! `fingerprint`: print a CRC-32 of a region of memory after every frame,
//! so a divergence against a reference emulator can be bisected cheaply.
//!
//! A trace from boot is enormous; a line a frame of the checksum of the
//! state that matters (the work RAM, or an object table in it) is tiny and
//! names the first frame the two emulators part. The region is read
//! without a side effect, in ascending address order, and hashed with the
//! zlib CRC-32, so the value compares with the same CRC another emulator
//! takes of the same bytes (an emulator built to print one a frame).

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use retro_core::EmulatorCore;

use crate::console::Console;
use crate::input::InputArgs;
use crate::output::open_output;
use crate::parse::parse_hex_u32;
use crate::report::crc32;
use crate::rom::{load, RomArgs};

#[derive(clap::Args, Debug)]
pub struct FingerprintArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Region start address (hex); by default the work RAM's.
    #[arg(long, value_parser = parse_hex_u32)]
    pub addr: Option<u32>,
    /// Region length in bytes (hex); by default the whole work RAM.
    #[arg(long, value_parser = parse_hex_u32)]
    pub len: Option<u32>,
    /// Number of frames to run and fingerprint, e.g. `2400` (40 s at 60
    /// fps).
    #[arg(long)]
    pub frames: Option<u64>,
    #[command(flatten)]
    pub input: InputArgs,
    /// Write the listing to a file instead of stdout.
    #[arg(long, short)]
    pub output: Option<PathBuf>,
}

pub fn run<C: Console>(args: FingerprintArgs<C::BootArgs>) -> Result<()> {
    let Some(frames) = args.frames else {
        anyhow::bail!("specify --frames");
    };
    let (work_ram, work_ram_size) = C::WORK_RAM;
    let addr = args.addr.unwrap_or(work_ram);
    let len = args.len.unwrap_or(work_ram_size);
    if len == 0 {
        anyhow::bail!("--len must be > 0");
    }
    let input = args.input.plan::<C::Emulator>()?;

    let (_, mut emu) = load::<C>(&args.rom)?;
    let mut out = open_output(args.output.as_ref())?;
    writeln!(
        out,
        "; per-frame CRC32 of ${:06X}..${:06X} ({} bytes)",
        addr,
        addr.wrapping_add(len),
        len
    )?;
    for frame in 0..frames {
        input.before_frame(&mut emu, frame);
        emu.run_frame();
        let crc = crc32(&emu.read_memory_debug(addr, len));
        writeln!(out, "F {frame} {crc:08X}")?;
    }
    out.flush()?;
    Ok(())
}
