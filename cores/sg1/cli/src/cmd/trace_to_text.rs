//! `trace-to-text`: convert a raw binary trace blob (e.g. captured by
//! the frontend or a test harness) into the MAME text format.
//!
//! For the 68000 a ROM may be supplied so each entry is disassembled at
//! its PC; without one, the instruction column falls back to the raw
//! opcode word. The Z80 always disassembles from the bytes captured in
//! each entry, so it needs no ROM.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;

use crate::trace_driver::{convert_m68k_trace_to_text, convert_z80_trace_to_text};
use crate::trace_ref::TraceCpu;

#[derive(Args, Debug)]
pub struct TraceToTextArgs {
    /// Input binary trace file.
    pub input: PathBuf,
    /// Output text trace file.
    #[arg(short, long)]
    pub output: PathBuf,
    /// Which CPU produced the trace: `m68k` (68000) or `z80`.
    #[arg(long, default_value = "m68k")]
    pub cpu: String,
    /// ROM file, for 68000 disassembly (ignored for Z80).
    #[arg(short, long)]
    pub rom: Option<PathBuf>,
}

pub fn run(args: TraceToTextArgs) -> Result<()> {
    let cpu = TraceCpu::parse(&args.cpu)?;
    let data = std::fs::read(&args.input)
        .with_context(|| format!("failed to read input: {}", args.input.display()))?;

    let text = match cpu {
        TraceCpu::Z80 => convert_z80_trace_to_text(&data),
        TraceCpu::M68k => {
            let mut emu = match &args.rom {
                Some(rom) => Some(super::load_rom(rom)?.1),
                None => None,
            };
            match emu.as_mut() {
                Some(e) => convert_m68k_trace_to_text(&data, e),
                None => {
                    eprintln!("note: no --rom given; 68000 instruction column omitted");
                    convert_m68k_trace_to_text_opcode_only(&data)
                }
            }
        }
    };

    std::fs::write(&args.output, &text)
        .with_context(|| format!("failed to write output: {}", args.output.display()))?;
    println!("Text trace written to: {}", args.output.display());
    Ok(())
}

/// Render the 68000 trace without disassembly (opcode word only), for the
/// no-ROM case.
fn convert_m68k_trace_to_text_opcode_only(data: &[u8]) -> String {
    use sg1_core::debugger::{TraceEntryBinary, TRACE_ENTRY_SIZE};
    let mut out = String::with_capacity((data.len() / TRACE_ENTRY_SIZE.max(1)) * 200);
    for chunk in data.as_chunks::<TRACE_ENTRY_SIZE>().0 {
        if let Some(e) = TraceEntryBinary::from_bytes(chunk) {
            out.push_str(&format!(
                "PC:{:06X} SR:{:04X} D0:{:08X} D1:{:08X} D2:{:08X} D3:{:08X} D4:{:08X} D5:{:08X} D6:{:08X} D7:{:08X} A0:{:08X} A1:{:08X} A2:{:08X} A3:{:08X} A4:{:08X} A5:{:08X} A6:{:08X} A7:{:08X} | DC.W ${:04X}\n",
                e.pc, e.sr,
                e.d[0], e.d[1], e.d[2], e.d[3], e.d[4], e.d[5], e.d[6], e.d[7],
                e.a[0], e.a[1], e.a[2], e.a[3], e.a[4], e.a[5], e.a[6], e.a[7],
                e.opcode
            ));
        }
    }
    out
}
