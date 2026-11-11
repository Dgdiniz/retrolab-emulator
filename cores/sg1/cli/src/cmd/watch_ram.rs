//! `watch-ram`: single-step a ROM through a window of frames and report
//! every write that changes a watched work-RAM cell, naming the exact
//! instruction (PC + disassembly) responsible.
//!
//! Unlike `watch-vram` (which arms a VDP-side hook for VRAM writes), this
//! works for *CPU* RAM and attributes each change to the writing 68000
//! instruction by snapshotting the live PC before each step and
//! re-reading the cell after. It is the tool for "who scheduled this task
//! pointer?" / "what scribbled this state variable?" questions, where the
//! address is reached through an indirect `(An)` write the disassembly
//! text alone would not reveal.
//!
//! To keep it cheap, the boot/attract prologue runs at full frame speed
//! (`--warmup-frames`); only the `--max-steps` window is single-
//! stepped. An optional `--report-pc` prints the watched value (and the
//! CPU registers) every time a given PC is about to execute, which
//! reproduces a known divergence point (e.g. a vblank-task dispatch
//! reading the cell) without scrolling the whole change log.

use std::path::PathBuf;

use anyhow::Result;
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};
use sg1_core::GenesisEmulator;

use super::{apply_press, connect_controllers, parse_press_schedule};

#[derive(clap::Args, Debug)]
pub struct WatchRamArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Work-RAM address to watch (24-bit hex, e.g. `FFA190`, `0xFFA190`).
    #[arg(long, value_parser = parse_hex_u32)]
    pub addr: u32,
    /// Width of the watched cell in bytes (1, 2 or 4).
    #[arg(long, default_value_t = 4)]
    pub width: u32,
    /// Frames to run at full speed before the single-stepped window.
    #[arg(long, default_value_t = 0)]
    pub warmup_frames: u64,
    /// Instructions to single-step while watching the cell (a Genesis
    /// frame is roughly 60k-130k 68000 instructions).
    #[arg(long, default_value_t = 200_000)]
    pub max_steps: u64,
    /// Stop at the first change instead of logging every change.
    #[arg(long)]
    pub until_change: bool,
    /// Also print the watched value every time this PC is about to
    /// execute (24-bit hex). Use to reproduce a known read/dispatch site.
    #[arg(long, value_parser = parse_hex_u32)]
    pub report_pc: Option<u32>,
    /// Connect 6-button pads instead of the default 3-button pads.
    #[arg(long)]
    pub six_button: bool,
    /// Timed player-1 input, e.g. `"180:start;186:"` (frame:buttons).
    #[arg(long)]
    pub press_at: Option<String>,
}

fn parse_hex_u32(s: &str) -> Result<u32, String> {
    let cleaned = s.trim().trim_start_matches("0x").trim_start_matches('$');
    u32::from_str_radix(cleaned, 16).map_err(|e| format!("invalid hex address '{s}': {e}"))
}

pub fn run(args: WatchRamArgs) -> Result<()> {
    if !matches!(args.width, 1 | 2 | 4) {
        anyhow::bail!("--width must be 1, 2 or 4");
    }
    let schedule = parse_press_schedule(args.press_at.as_deref())?;
    let hex_w = (args.width * 2) as usize;

    let (_, mut emu) = super::load_rom(&args.rom)?;
    super::print_rom_title(&emu);
    if args.six_button {
        connect_controllers(&mut emu, true);
    }

    // Boot/attract prologue at full frame speed.
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    for frame in 0..args.warmup_frames {
        apply_press(&mut emu, schedule.as_ref(), frame);
        emu.run_frame();
    }
    println!(
        "Warmed up {} frame(s); single-stepping up to {} instruction(s) watching ${:06X} ({}B)",
        args.warmup_frames, args.max_steps, args.addr, args.width
    );

    let mut prev = read_cell(&mut emu, args.addr, args.width);
    println!("  initial ${:06X} = {prev:0hex_w$X}", args.addr);

    // StepInto does not tick the frame counter, so the window is bounded by
    // stepped instructions; the warmup already positioned the run.
    emu.debugger().set_execution_mode(ExecutionMode::StepInto);
    let mut steps = 0u64;
    let mut changes = 0u64;
    let mut report_hits = 0u64;

    while steps < args.max_steps {
        let pre_pc = emu.cpu().pc & 0x00FF_FFFF;

        if args.report_pc == Some(pre_pc) {
            let val = read_cell(&mut emu, args.addr, args.width);
            report_hits += 1;
            // Only print the first few hits of a hot dispatch PC.
            if report_hits <= 4 {
                println!(
                    "  [s={steps}] >>> PC={pre_pc:06X}: ${:06X} = {val:0hex_w$X}   {}",
                    args.addr,
                    cpu_regs(&emu),
                );
            }
        }

        let r = emu.step();
        if !r.instruction_executed {
            continue;
        }
        steps += 1;

        let now = read_cell(&mut emu, args.addr, args.width);
        if now != prev {
            changes += 1;
            let disasm = disasm_one(&mut emu, pre_pc);
            println!(
                "  [s={steps}] PC={pre_pc:06X}  ${:06X}: {prev:0hex_w$X} -> {now:0hex_w$X}   | {disasm}",
                args.addr,
            );
            prev = now;
            if args.until_change {
                break;
            }
        }
    }

    println!(
        "Done: {changes} change(s), {report_hits} report-pc hit(s) over {steps} stepped instruction(s){}",
        if steps >= args.max_steps {
            " (hit --max-steps cap)"
        } else {
            ""
        }
    );
    Ok(())
}

/// Read the watched cell as a big-endian value of `width` bytes.
fn read_cell(emu: &mut GenesisEmulator, addr: u32, width: u32) -> u32 {
    let bytes = emu.read_memory_debug(addr, width);
    let mut v = 0u32;
    for &b in bytes.iter().take(width as usize) {
        v = (v << 8) | b as u32;
    }
    v
}

/// A compact `Dn=..`/`An=..` register line for the report-pc dump.
fn cpu_regs(emu: &GenesisEmulator) -> String {
    let cpu = emu.cpu();
    let mut out = String::new();
    for (i, v) in cpu.d.iter().enumerate() {
        out.push_str(&format!("D{i}={v:08X} "));
    }
    for (i, v) in cpu.a.iter().enumerate() {
        out.push_str(&format!("A{i}={v:08X} "));
    }
    out.trim_end().to_string()
}

/// Disassemble a single instruction at `pc` for change attribution.
fn disasm_one(emu: &mut GenesisEmulator, pc: u32) -> String {
    let instrs = emu.disassemble_at(pc, 1);
    if let Some(i) = instrs.first() {
        format!("{} {}", i.mnemonic, i.operands)
    } else {
        "??".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_addr_forms() {
        assert_eq!(parse_hex_u32("FFA190").unwrap(), 0xFF_A190);
        assert_eq!(parse_hex_u32("0xff0000").unwrap(), 0xFF_0000);
        assert_eq!(parse_hex_u32("$1").unwrap(), 1);
        assert!(parse_hex_u32("zzz").is_err());
    }
}
