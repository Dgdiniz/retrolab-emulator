//! `memory`: boot a ROM, run it, and dump a region of the main CPU's
//! memory as an address + hex + ASCII listing.
//!
//! The addresses are the main CPU's bus. The bytes are read without a
//! side effect, as the debugger's memory view reads them: a register page
//! shows what a read would give, and no read changes the machine.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use retro_core::EmulatorCore;

use crate::console::Console;
use crate::input::InputArgs;
use crate::output::open_output;
use crate::parse::parse_hex_u32;
use crate::report::hex_dump;
use crate::rom::{load, RomArgs};

#[derive(clap::Args, Debug)]
pub struct MemoryArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Start address (hex).
    #[arg(value_parser = parse_hex_u32, index = 2)]
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
    #[command(flatten)]
    pub input: InputArgs,
}

pub fn run<C: Console>(args: MemoryArgs<C::BootArgs>) -> Result<()> {
    if args.len == 0 {
        anyhow::bail!("--len must be > 0");
    }
    let input = args.input.plan::<C::Emulator>()?;

    let (_, mut emu) = load::<C>(&args.rom)?;
    for frame in 0..args.frames {
        input.before_frame(&mut emu, frame);
        emu.run_frame();
    }

    let data = emu.read_memory_debug(args.addr, args.len);
    let mut out = open_output(args.output.as_ref())?;
    writeln!(
        out,
        "; {} bytes @ ${:06X} after {} frame(s)",
        args.len, args.addr, args.frames
    )?;
    out.write_all(hex_dump(args.addr, &data).as_bytes())?;
    out.flush()?;
    Ok(())
}
