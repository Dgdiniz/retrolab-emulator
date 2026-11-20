//! `trace-to-text`: turn a binary trace (the entries a host saved, as the
//! debugger hands them out) into the console's text format. A CPU whose
//! lines disassemble its memory needs the ROM the trace was made from for
//! its whole text; without one, the console says what its lines can show.

use std::path::PathBuf;

use anyhow::{Context, Result};
use retro_core::golden::GoldenConsole;

use crate::console::{find_cpu, Console};
use crate::rom::{load, RomArgs};

#[derive(clap::Args, Debug)]
pub struct TraceToTextArgs {
    /// Input binary trace file.
    pub input: PathBuf,
    /// Output text trace file.
    #[arg(short, long)]
    pub output: PathBuf,
    /// Which CPU produced the trace.
    #[arg(long, default_value = "")]
    pub cpu: String,
    /// The ROM the trace was made from, for lines that disassemble its memory.
    #[arg(short, long)]
    pub rom: Option<PathBuf>,
}

pub fn run<C: Console>(args: TraceToTextArgs) -> Result<()> {
    let cpu = find_cpu::<C>(&args.cpu)?;
    cpu.check_available()?;
    let data = std::fs::read(&args.input)
        .with_context(|| format!("failed to read input: {}", args.input.display()))?;

    let text = match &args.rom {
        Some(rom) => {
            let (_, mut emu) = load::<C>(&RomArgs::new(rom))?;
            cpu.check_slot::<C>(&emu)?;
            C::trace_text(&mut emu, cpu, &data, false)?
        }
        None => match C::trace_text_without_rom(cpu, &data) {
            Some(text) => {
                eprintln!(
                    "note: no --rom given; {} instruction column omitted",
                    cpu.label
                );
                text
            }
            None => C::trace_text(&mut C::Emulator::new_for_golden(), cpu, &data, false)?,
        },
    };

    std::fs::write(&args.output, &text)
        .with_context(|| format!("failed to write output: {}", args.output.display()))?;
    println!("Text trace written to: {}", args.output.display());
    Ok(())
}
