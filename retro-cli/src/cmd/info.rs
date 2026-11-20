//! `info`: load a ROM and print what it is: the file and its header (the
//! console's lines), the cartridge's coprocessors with their state, and
//! the memory map the cartridge gives the main CPU.

use std::io::Write;

use anyhow::Result;
use retro_core::EmulatorCore;

use crate::console::Console;
use crate::input::InputArgs;
use crate::report::{write_chips, write_memory_map};
use crate::rom::{load, RomArgs};

#[derive(clap::Args, Debug)]
pub struct InfoArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Run this many frames after loading before dumping coprocessor +
    /// memory state (0 = static snapshot at reset).
    #[arg(long, default_value_t = 0)]
    pub frames: u64,
    #[command(flatten)]
    pub input: InputArgs,
}

pub fn run<C: Console>(args: InfoArgs<C::BootArgs>) -> Result<()> {
    let input = args.input.plan::<C::Emulator>()?;
    let (rom, mut emu) = load::<C>(&args.rom)?;
    // The frames let the chips' state and the memory map show the game at
    // work rather than at reset.
    for frame in 0..args.frames {
        input.before_frame(&mut emu, frame);
        emu.run_frame();
    }

    let mut out = std::io::stdout().lock();
    C::write_rom_header(&emu, &rom, &args.rom.path, &mut out)?;
    writeln!(out)?;
    write_chips(&mut emu, &mut out)?;
    write_memory_map(&mut emu, &mut out)?;
    out.flush()?;
    Ok(())
}
