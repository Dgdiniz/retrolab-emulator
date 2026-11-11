//! `disasm`: forward-disassemble a CPU's instruction stream.
//!
//! Decodes `--count` instructions starting at `--addr` (defaulting to the
//! selected CPU's live PC), optionally after running `--frames` frames so
//! the disassembly reflects a running program rather than the reset
//! vector.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode, Instruction};
use sg1_core::GenesisEmulator;

use crate::trace_ref::TraceCpu;

#[derive(Args, Debug)]
pub struct DisasmArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Which CPU to disassemble: `m68k` (68000) or `z80`.
    #[arg(long, default_value = "m68k")]
    pub cpu: String,
    /// Start address (hex). Defaults to the CPU's current PC.
    #[arg(long, value_parser = parse_hex_u32)]
    pub addr: Option<u32>,
    /// Number of instructions to decode.
    #[arg(long, default_value_t = 32)]
    pub count: u32,
    /// Run this many frames before decoding (so PC settles).
    #[arg(long, default_value_t = 0)]
    pub frames: u64,
    /// Write the listing here instead of stdout.
    #[arg(long, short)]
    pub output: Option<PathBuf>,
}

fn parse_hex_u32(s: &str) -> Result<u32, String> {
    let cleaned = s.trim().trim_start_matches("0x").trim_start_matches('$');
    u32::from_str_radix(cleaned, 16).map_err(|e| format!("invalid hex address '{s}': {e}"))
}

pub fn run(args: DisasmArgs) -> Result<()> {
    let cpu = TraceCpu::parse(&args.cpu)?;

    let (_, mut emu) = super::load_rom(&args.rom)?;
    // `run_frame` only advances the CPU in `Running` mode; otherwise the PC
    // never leaves the reset vector and `--frames` is a no-op.
    if args.frames > 0 {
        emu.debugger().set_execution_mode(ExecutionMode::Running);
    }
    for _ in 0..args.frames {
        emu.run_frame();
    }

    let addr = args.addr.unwrap_or_else(|| current_pc(&emu, cpu));
    let rows = emu.disassemble_cpu(cpu.cpu_id(), addr, args.count);

    let mut out = super::open_output(args.output.as_ref())?;
    writeln!(
        out,
        "Disassembly of {} at ${:06X} ({} instruction(s)):",
        cpu.label(),
        addr,
        rows.len()
    )?;
    for insn in &rows {
        writeln!(out, "{}", format_disasm_row(insn))?;
    }
    out.flush()?;
    Ok(())
}

/// Live PC of the selected CPU (68000: the 24-bit PC; Z80: the 16-bit PC).
fn current_pc(emu: &GenesisEmulator, cpu: TraceCpu) -> u32 {
    match cpu {
        TraceCpu::M68k => emu.cpu().pc & 0x00FF_FFFF,
        TraceCpu::Z80 => emu.z80().pc as u32,
    }
}

/// Render one disassembled instruction as `addr  bytes  mnemonic operands
/// ; comment`. The byte column is padded so mnemonics line up.
pub(crate) fn format_disasm_row(insn: &Instruction) -> String {
    let bytes: String = insn
        .bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    let text = if insn.operands.is_empty() {
        insn.mnemonic.clone()
    } else {
        format!("{} {}", insn.mnemonic, insn.operands)
    };
    let mut row = format!("{:06x}  {:<14}  {}", insn.address, bytes, text);
    if let Some(c) = &insn.comment {
        row.push_str("  ; ");
        row.push_str(c);
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insn(addr: u32, bytes: &[u8], m: &str, ops: &str, comment: Option<&str>) -> Instruction {
        Instruction {
            address: addr,
            bytes: bytes.to_vec(),
            mnemonic: m.to_string(),
            operands: ops.to_string(),
            comment: comment.map(|s| s.to_string()),
        }
    }

    #[test]
    fn row_has_addr_bytes_and_text() {
        let row = format_disasm_row(&insn(0x000400, &[0x4e, 0x71], "NOP", "", None));
        assert!(row.starts_with("000400"));
        assert!(row.contains("4e 71"));
        assert!(row.ends_with("NOP"));
    }

    #[test]
    fn row_with_operands_joins_mnemonic_and_operands() {
        let row = format_disasm_row(&insn(0x000200, &[0x4a, 0xb9], "TST.L", "$00A10008", None));
        assert!(row.ends_with("TST.L $00A10008"));
    }

    #[test]
    fn row_appends_comment() {
        let row = format_disasm_row(&insn(
            0x000100,
            &[0x60, 0x04],
            "BRA",
            "$000106",
            Some("(+4)"),
        ));
        assert!(row.ends_with("BRA $000106  ; (+4)"));
    }
}
