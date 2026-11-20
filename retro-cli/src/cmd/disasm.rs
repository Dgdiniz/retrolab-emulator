//! `disasm`: forward-disassemble a CPU's instruction stream: `--count`
//! instructions from `--addr`, by default the CPU's program counter (with
//! its bank, for a CPU that runs in banks), optionally after `--frames`
//! frames so the listing shows a running program rather than the reset.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use retro_core::EmulatorCore;

use crate::console::{find_cpu, program_counter, Console};
use crate::input::InputArgs;
use crate::output::open_output;
use crate::parse::parse_hex_u32;
use crate::report::disassembly_row;
use crate::rom::{load, RomArgs};
use crate::trace::check_cpu;

#[derive(clap::Args, Debug)]
pub struct DisasmArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Which CPU to disassemble.
    #[arg(long, default_value = "")]
    pub cpu: String,
    /// Start address (hex). Defaults to the CPU's current PC.
    #[arg(long, value_parser = parse_hex_u32)]
    pub addr: Option<u32>,
    /// Number of instructions to decode.
    #[arg(long, default_value_t = 32)]
    pub count: u32,
    /// Run this many frames before decoding.
    #[arg(long, default_value_t = 0)]
    pub frames: u64,
    /// Write the listing here instead of stdout.
    #[arg(long, short)]
    pub output: Option<PathBuf>,
    #[command(flatten)]
    pub input: InputArgs,
}

pub fn run<C: Console>(args: DisasmArgs<C::BootArgs>) -> Result<()> {
    let cpu = find_cpu::<C>(&args.cpu)?;
    cpu.check_available()?;
    let input = args.input.plan::<C::Emulator>()?;

    let (_, mut emu) = load::<C>(&args.rom)?;
    check_cpu::<C>(&emu, cpu)?;
    for frame in 0..args.frames {
        input.before_frame(&mut emu, frame);
        emu.run_frame();
    }

    let addr = match args.addr {
        Some(addr) => addr,
        None => program_counter(&mut emu, cpu.id),
    };
    let rows = emu.disassemble_cpu(cpu.id, addr, args.count);

    let mut out = open_output(args.output.as_ref())?;
    writeln!(
        out,
        "Disassembly of {} at ${:06X} ({} instruction(s)):",
        cpu.label,
        addr,
        rows.len()
    )?;
    for insn in &rows {
        writeln!(out, "{}", disassembly_row(insn, cpu.disassembly_bytes))?;
    }
    out.flush()?;
    Ok(())
}
