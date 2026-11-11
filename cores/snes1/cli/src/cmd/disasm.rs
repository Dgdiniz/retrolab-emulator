//! `disasm`: forward-disassemble a CPU's instruction stream.
//!
//! Decodes `--count` instructions starting at `--addr` (defaulting to
//! the selected CPU's live PC), optionally after running `--frames`
//! frames so the 65C816 M/X/E flags, which set immediate operand
//! widths, reflect a running program rather than the reset state.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use retro_core::{EmulatorCore, Instruction};
use snes1_core::SnesEmulator;

use crate::trace_ref::TraceCpu;

#[derive(Args, Debug)]
pub struct DisasmArgs {
    /// Path to the ROM file (`.sfc` / `.smc`).
    pub rom: PathBuf,
    /// Which CPU to disassemble: `snes` (65C816), `spc700`, `gsu` (Super FX),
    /// or `cx4` (Cx4 HG51B169).
    #[arg(long, default_value = "snes")]
    pub cpu: String,
    /// Start address (hex). Defaults to the CPU's current PC.
    #[arg(long, value_parser = parse_hex_u32)]
    pub addr: Option<u32>,
    /// Number of instructions to decode.
    #[arg(long, default_value_t = 32)]
    pub count: u32,
    /// Run this many frames before decoding (so PC + flags settle).
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

    let rom = super::read_rom(&args.rom)?;
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;

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

/// Live PC of the selected CPU as a flat address (65C816: `PBR:PC`;
/// SPC700: the 16-bit `PC`; Super FX GSU: `R15` / Cx4 HG51B169: the 23-bit
/// program counter, both from the side-effect-free coprocessor debug
/// state's `program_counter`; `0` when no chip is installed).
fn current_pc(emu: &SnesEmulator, cpu: TraceCpu) -> u32 {
    match cpu {
        TraceCpu::Snes => {
            let c = emu.cpu();
            ((c.pbr as u32) << 16) | c.pc as u32
        }
        TraceCpu::Spc700 => emu.bus().apu().spc700().pc as u32,
        TraceCpu::Gsu | TraceCpu::Hg51b => emu
            .coprocessor_debug_state()
            .and_then(|s| s.program_counter)
            .unwrap_or(0),
    }
}

/// Render one disassembled instruction as `addr  bytes  mnemonic
/// operands  ; comment`. The byte column is padded so mnemonics line up
/// for the common <=4-byte instructions.
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
    let mut row = format!("{:06x}  {:<11}  {}", insn.address, bytes, text);
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
        let row = format_disasm_row(&insn(0x00_804a, &[0xe2, 0x30], "sep", "#$30", None));
        assert!(row.starts_with("00804a"));
        assert!(row.contains("e2 30"));
        assert!(row.ends_with("sep #$30"));
    }

    #[test]
    fn row_without_operands_has_no_trailing_space() {
        let row = format_disasm_row(&insn(0x00_8000, &[0x78], "sei", "", None));
        assert!(row.ends_with("sei"));
        assert!(!row.ends_with("sei "));
    }

    #[test]
    fn row_appends_comment() {
        let row = format_disasm_row(&insn(
            0x00_8002,
            &[0x80, 0x05],
            "bra",
            "$8009",
            Some("(+5)"),
        ));
        assert!(row.ends_with("bra $8009  ; (+5)"));
    }
}
