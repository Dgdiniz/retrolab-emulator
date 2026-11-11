//! Subcommand handlers for `snes1-cli`. Each module owns its clap
//! argument struct and a `run` entry point; `main.rs` only wires the
//! `Commands` enum to these.

pub mod apu_dump;
pub mod audio_dump;
pub mod benchmark;
pub mod compare;
pub mod determinism_check;
pub mod disasm;
pub mod golden;
pub mod info;
pub mod memory;
pub mod ppu_dump;
pub mod run;
pub mod savestate;
pub mod screenshot;
pub mod singlestep;
pub mod trace;

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use retro_core::cartridge::ExpansionChipState;
use snes1_core::{CoprocessorMode, SnesEmulator};

/// Open a buffered writer for an optional output path, falling back to
/// stdout when `path` is `None`.
pub(crate) fn open_output(path: Option<&PathBuf>) -> Result<Box<dyn Write>> {
    match path {
        Some(p) => {
            let f = File::create(p)
                .with_context(|| format!("failed to create output file: {}", p.display()))?;
            Ok(Box::new(BufWriter::new(f)))
        }
        None => Ok(Box::new(BufWriter::new(std::io::stdout()))),
    }
}

/// Parse a `--ignore A,X,DB` list into an upper-cased set.
pub(crate) fn parse_ignore(spec: Option<&str>) -> HashSet<String> {
    spec.map(|s| {
        s.split(',')
            .map(|r| r.trim().to_ascii_uppercase())
            .filter(|r| !r.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

/// Read a ROM file into memory with a contextual error.
pub(crate) fn read_rom(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("failed to read ROM: {}", path.display()))
}

/// When `force` is set, stage the threshold-free best-guess mapping as a
/// manifest override so a low-confidence ROM (blank title / bad checksum,
/// like Nintendo's SNES Test Program cartridge) loads with its vector-implied
/// preset instead of being rejected by the detector's confidence gate.
/// A no-op otherwise. Call before `load_rom`.
pub(crate) fn apply_force_detect(emu: &mut SnesEmulator, rom: &[u8], force: bool) {
    if force {
        let manifest = emu.detected_manifest(rom);
        emu.set_cartridge_manifest_override(Some(manifest));
    }
}

/// Print the installed coprocessor(s): a label summary followed by each
/// chip's live state. A plain cartridge prints `Coprocessor: none`; a
/// multi-chip cartridge enumerates every chip by `chip_id`. Shared by
/// `info` and `screenshot --coprocessor-state`.
pub(crate) fn print_coprocessors(emu: &SnesEmulator) {
    let labels = emu.coprocessor_labels();
    match labels.as_slice() {
        [] => {
            println!("Coprocessor: none");
            return;
        }
        [(_, label)] => println!("Coprocessor: {label}"),
        many => {
            println!("Coprocessors: {}", many.len());
            for (id, label) in many {
                println!("  [{id}] {label}");
            }
        }
    }
    for (id, state) in emu.coprocessors_debug_state() {
        print_coprocessor_state(id, &state);
    }
}

/// Print one coprocessor chip's live state (DSP-1 uPD7725 register file +
/// data RAM, the S-DD1 MMC latches + reconstructed DMA shadow, or the
/// SPC7110's decoder / ALU / data-port / mapping + RTC registers). Values
/// reflect however many frames the caller ran before dumping: `info`
/// defaults to a reset snapshot (`--frames 0`), while `info --frames N`
/// and `screenshot --coprocessor-state` show live state after N frames
/// (e.g. S-DD1 `DECOMPS > 0` once a game has driven a decompression, or the
/// SPC7110 `DEC_MODE`/`DX_OFF` after a decompression + bank switch).
/// Also consumed by the frontend debugger.
pub(crate) fn print_coprocessor_state(chip_id: u8, state: &ExpansionChipState) {
    println!();
    println!("Coprocessor chip state [{chip_id}]:");
    println!("  Type:           {:?}", state.chip_type);
    match state.program_counter {
        Some(pc) => println!("  PC:             ${pc:04x}"),
        None => println!("  PC:             (idle - no firmware loaded)"),
    }
    println!("  Cycle count:    {}", state.cycle_count);
    match &state.internal_ram {
        Some(ram) => println!(
            "  Internal RAM:   {} bytes ({} x 16-bit words)",
            ram.len(),
            ram.len() / 2
        ),
        None => println!("  Internal RAM:   (none)"),
    }
    if state.registers.is_empty() {
        println!("  Registers:      (none - chip present but not driven)");
    } else {
        println!("  Registers:");
        for r in &state.registers {
            println!("    {:<8} ${:04x}  {}", r.name, r.value, r.description);
        }
    }
}

/// Parse a comma-separated button list into the generic input bitmask
/// (`InputState::buttons`). The grammar and the button names live in
/// [`snes1_core::input_schedule`]; this wrapper only turns its error
/// into an `anyhow` one. Shared by the input-injection paths.
pub(crate) fn parse_button_mask(spec: Option<&str>) -> Result<Option<u32>> {
    snes1_core::input_schedule::parse_button_mask(spec).map_err(anyhow::Error::msg)
}

/// A frame-scheduled controller-input program, parsed from
/// `--press-at "F:buttons;F:buttons"` (e.g. `"180:start;186:;240:select"`).
pub(crate) use snes1_core::input_schedule::PressSchedule;

/// Parse a `--press-at` spec into a [`PressSchedule`]; `None` when no spec
/// was given.
pub(crate) fn parse_press_schedule(spec: Option<&str>) -> Result<Option<PressSchedule>> {
    snes1_core::input_schedule::parse_press_schedule(spec).map_err(anyhow::Error::msg)
}

/// Parse the `--reset-at` frame list (`"360,720"`) into a sorted set of
/// frame indices before which the console is reset.
pub(crate) fn parse_reset_frames(spec: Option<&str>) -> Result<std::collections::BTreeSet<u64>> {
    snes1_core::input_schedule::parse_reset_frames(spec).map_err(anyhow::Error::msg)
}

/// Shared coprocessor flags for the commands that boot a ROM (`run`,
/// `trace`, `compare-trace`, `screenshot`, `info`). Flattened into each
/// command's args via `#[command(flatten)]`.
#[derive(Args, Debug, Clone, Default)]
pub(crate) struct CoprocessorArgs {
    /// Path to a DSP-1 firmware dump (`dsp1b.rom` / `dsp1b.bin`, 8192
    /// bytes) enabling cycle-accurate low-level emulation of DSP-1 games
    /// such as Super Mario Kart (the `nec-lle` feature, on by default; a
    /// snes1-cli built without it ignores the dump, with a warning).
    /// Optional: without it those carts run the firmware-free
    /// high-level emulation (HLE), which renders the Mode-7 track
    /// correctly. Byte order (`.bin` BE / `.rom` LE) and revision are
    /// auto-detected by SHA-256.
    #[arg(long, value_name = "PATH")]
    pub dsp1_firmware: Option<PathBuf>,
    /// How a detected coprocessor is emulated: `auto` (LLE when firmware
    /// is present and built in, otherwise the firmware-free HLE), `lle`
    /// (force low-level, falls back to a non-hanging idle stub if
    /// firmware is missing/invalid; refused by a build without the
    /// `nec-lle` feature), or `hle` (force the firmware-free HLE).
    #[arg(
        long,
        value_name = "MODE",
        default_value = "auto",
        value_parser = parse_coprocessor_mode
    )]
    pub coprocessor_mode: CoprocessorMode,
}

fn parse_coprocessor_mode(s: &str) -> std::result::Result<CoprocessorMode, String> {
    match s.to_ascii_lowercase().as_str() {
        "auto" => Ok(CoprocessorMode::Auto),
        "lle" => Ok(CoprocessorMode::Lle),
        "hle" => Ok(CoprocessorMode::Hle),
        other => Err(format!(
            "unknown coprocessor mode '{other}' (expected auto|lle|hle)"
        )),
    }
}

impl CoprocessorArgs {
    /// Stage these flags on `emu` before `load_rom`: read the firmware
    /// file (if any) and set the emulation mode.
    pub(crate) fn apply(&self, emu: &mut SnesEmulator) -> Result<()> {
        if !snes1_core::NEC_LLE_BUILT {
            if self.coprocessor_mode == CoprocessorMode::Lle {
                anyhow::bail!(
                    "--coprocessor-mode lle: low-level emulation is not built into this \
                     snes1-cli (build it with `--features nec-lle`); DSP-1 games run on the HLE"
                );
            }
            if self.dsp1_firmware.is_some() {
                eprintln!(
                    "warning: --dsp1-firmware ignored: low-level emulation is not built into \
                     this snes1-cli (build it with `--features nec-lle`); the DSP-1 runs on the HLE"
                );
            }
        }
        emu.set_coprocessor_mode(self.coprocessor_mode);
        if let Some(path) = &self.dsp1_firmware {
            let fw = std::fs::read(path)
                .with_context(|| format!("failed to read DSP-1 firmware: {}", path.display()))?;
            emu.set_coprocessor_firmware(Some(fw));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_lle_is_refused_by_a_build_without_it() {
        let args = CoprocessorArgs {
            dsp1_firmware: None,
            coprocessor_mode: CoprocessorMode::Lle,
        };
        let result = args.apply(&mut SnesEmulator::new());
        assert_eq!(result.is_err(), !snes1_core::NEC_LLE_BUILT);
        if let Err(err) = result {
            assert!(err.to_string().contains("not built"), "{err}");
        }
    }
}
