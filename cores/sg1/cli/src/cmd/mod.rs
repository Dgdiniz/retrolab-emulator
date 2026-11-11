//! Subcommand handlers for `sg1-cli`. Each module owns its clap argument
//! struct and a `run` entry point; `main.rs` only wires the `Commands`
//! enum to these. Shared helpers (ROM loading, output sinks, screenshot
//! encoding, controller-input scheduling) live here.

pub mod audio_dump;
pub mod audio_info;
pub mod benchmark;
pub mod compare;
pub mod compare_trace;
pub mod determinism_check;
pub mod disasm;
pub mod dump_vdp;
pub mod fingerprint;
pub mod info;
pub mod memory;
pub mod profile;
pub mod run;
pub mod savestate;
pub mod screenshot;
pub mod sound_trace;
pub mod sram;
pub mod trace;
pub mod trace_to_text;
pub mod watch_ram;
pub mod watch_read;
pub mod watch_vram;

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use retro_core::cartridge::ExpansionChipState;
use retro_core::{EmulatorCore, InputState};
use sg1_core::GenesisEmulator;

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

/// Parse a comma-separated `--ignore PC,SR,A7` list into an upper-cased set.
pub(crate) fn parse_ignore(spec: Option<&str>) -> HashSet<String> {
    crate::trace_ref::parse_ignore(spec)
}

/// Read a ROM file into memory with a contextual error.
pub(crate) fn read_rom(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("failed to read ROM: {}", path.display()))
}

/// Read a ROM and boot a fresh emulator with it, returning both the raw
/// bytes (some callers report the size) and the loaded emulator.
pub(crate) fn load_rom(path: &Path) -> Result<(Vec<u8>, GenesisEmulator)> {
    let rom = read_rom(path)?;
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;
    Ok((rom, emu))
}

/// Print the parsed ROM title (best-effort) to stdout.
pub(crate) fn print_rom_title(emu: &GenesisEmulator) {
    if let Some(info) = emu.get_rom_info() {
        println!("Title: {}", info.title.trim());
    }
}

/// Print the installed coprocessor(s): a label summary followed by each
/// chip's live state. A plain cartridge prints `Coprocessor: none`; a
/// multi-chip cartridge enumerates every chip by `chip_id`. Shared by
/// `info` and `screenshot --coprocessor-state`, mirroring the snes1-cli
/// helper of the same name.
pub(crate) fn print_coprocessors(emu: &GenesisEmulator) {
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

/// Print one coprocessor chip's live state (for the SVP: the SSP1601
/// program counter, cycle count, and IRAM size). Values reflect however
/// many frames the caller ran before dumping, `info` defaults to a reset
/// snapshot (`--frames 0`), while `info --frames N` and
/// `screenshot --coprocessor-state` show live state after N frames (e.g.
/// the SVP's `Cycle count` climbing once Virtua Racing has driven it).
pub(crate) fn print_coprocessor_state(chip_id: u8, state: &ExpansionChipState) {
    println!();
    println!("Coprocessor chip state [{chip_id}]:");
    println!("  Type:           {:?}", state.chip_type);
    match state.program_counter {
        Some(pc) => println!("  PC:             ${pc:04x}"),
        None => println!("  PC:             (idle - no program loaded)"),
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

/// Force 6-button pads on both DE-9 ports.
///
/// `load_rom` already auto-selects 6-button when the cartridge header
/// advertises `'6'` in the I/O-support field; call this only for the
/// explicit `--six-button` override (or to force 6-button on a ROM that
/// omits the tag).
pub(crate) fn connect_controllers(emu: &mut GenesisEmulator, six_button: bool) {
    emu.connect_controllers(six_button);
}

/// Save the emulator's framebuffer as a PNG. The Genesis framebuffer is
/// already display-sized (no overscan crop needed), so the whole RGBA
/// buffer is converted to RGB and encoded as-is.
pub(crate) fn save_screenshot(emu: &GenesisEmulator, output_path: &Path) -> Result<()> {
    let fb = emu.get_framebuffer();
    let width = fb.width;
    let height = fb.height;

    if fb.data.is_empty() || width == 0 || height == 0 {
        anyhow::bail!("framebuffer is empty ({width}x{height})");
    }

    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for px in fb.data.as_chunks::<4>().0 {
        rgb.push(px[0]);
        rgb.push(px[1]);
        rgb.push(px[2]);
    }

    let img = image::RgbImage::from_raw(width, height, rgb).ok_or_else(|| {
        anyhow::anyhow!("failed to build {width}x{height} image from framebuffer")
    })?;
    img.save(output_path)
        .with_context(|| format!("failed to save screenshot: {}", output_path.display()))?;

    println!(
        "Screenshot saved: {} ({}x{})",
        output_path.display(),
        width,
        height
    );
    Ok(())
}

/// Genesis controller button bit positions, matching the
/// `Genesis{3,6}ButtonDevice` layout (`set_port_button_state` /
/// `InputState::buttons`). The 6-button extras (`X`/`Y`/`Z`/`MODE`) only
/// reach a game when 6-button pads are connected (`--six-button`).
const BUTTON_BITS: &[(&str, u32)] = &[
    ("up", 0),
    ("down", 1),
    ("left", 2),
    ("right", 3),
    ("b", 4),
    ("c", 5),
    ("a", 6),
    ("start", 7),
    ("z", 8),
    ("y", 9),
    ("x", 10),
    ("mode", 11),
];

/// Parse a comma- or `+`-separated button list into the controller
/// bitmask the core expects. Returns `None` when no spec was given;
/// errors on an unknown name so a typo fails loudly instead of silently
/// pressing nothing. Shared by the `screenshot`/`trace`/`compare-trace`
/// input-injection paths.
pub(crate) fn parse_button_mask(spec: Option<&str>) -> Result<Option<u32>> {
    let Some(spec) = spec else {
        return Ok(None);
    };
    let mut mask = 0u32;
    for name in spec
        .split(['+', ','])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let lower = name.to_ascii_lowercase();
        let bit = BUTTON_BITS
            .iter()
            .find(|(n, _)| *n == lower)
            .map(|(_, b)| *b)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown button '{name}' (expected up,down,left,right,a,b,c,start,x,y,z,mode)"
                )
            })?;
        mask |= 1 << bit;
    }
    Ok(Some(mask))
}

/// A frame-scheduled controller-input program for the `screenshot`,
/// `trace`, and `compare-trace` drivers. Parsed from
/// `--press-at "F:btns;F:btns"` (e.g. `"180:start;186:;240:a+c"`). Each
/// `F:buttons` set-point installs that player-1 mask starting on frame
/// `F` and holds it until the next set-point; an empty button list
/// (`F:`) releases all buttons. This reproduces timed human presses so a
/// game can be driven to a specific screen for a screenshot or trace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PressSchedule {
    /// Sorted `(frame, mask)` set-points.
    points: Vec<(u64, u32)>,
}

impl PressSchedule {
    /// The mask in effect on `frame` (the latest set-point with
    /// `set_frame <= frame`), or `None` before the first set-point, so
    /// the caller leaves input untouched until the schedule begins.
    pub(crate) fn mask_at(&self, frame: u64) -> Option<u32> {
        let mut current = None;
        for &(f, m) in &self.points {
            if f <= frame {
                current = Some(m);
            } else {
                break;
            }
        }
        current
    }
}

/// Parse a `--press-at "F:buttons;F:buttons"` spec into a
/// [`PressSchedule`]. Returns `None` when no spec was given. Errors on a
/// malformed segment, a non-numeric frame, or an unknown button name.
pub(crate) fn parse_press_schedule(spec: Option<&str>) -> Result<Option<PressSchedule>> {
    let Some(spec) = spec else {
        return Ok(None);
    };
    let mut points: Vec<(u64, u32)> = Vec::new();
    for seg in spec.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (frame_s, buttons_s) = seg.split_once(':').ok_or_else(|| {
            anyhow::anyhow!("invalid --press-at segment '{seg}' (expected 'frame:buttons')")
        })?;
        let frame: u64 = frame_s
            .trim()
            .parse()
            .with_context(|| format!("invalid frame number '{}' in --press-at", frame_s.trim()))?;
        // An empty button list releases all buttons on that frame.
        let mask = parse_button_mask(Some(buttons_s))?.unwrap_or(0);
        points.push((frame, mask));
    }
    if points.is_empty() {
        return Ok(None);
    }
    points.sort_by_key(|&(f, _)| f);
    Ok(Some(PressSchedule { points }))
}

/// Apply the player-1 input for `frame` from `schedule` (if any) as a
/// sticky direct override. Routed through `set_input` (not
/// `set_port_button_state`) so it survives `run_frame`'s per-frame input
/// resync. A `None` mask (before the first set-point) leaves input
/// untouched.
pub(crate) fn apply_press(emu: &mut GenesisEmulator, schedule: Option<&PressSchedule>, frame: u64) {
    if let Some(sched) = schedule {
        if let Some(buttons) = sched.mask_at(frame) {
            emu.set_input(0, InputState { buttons });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_button_mask_none_for_missing_spec() {
        assert_eq!(parse_button_mask(None).unwrap(), None);
    }

    #[test]
    fn parse_button_mask_maps_each_name_to_its_bit() {
        let cases = [
            ("up", 0),
            ("down", 1),
            ("left", 2),
            ("right", 3),
            ("b", 4),
            ("c", 5),
            ("a", 6),
            ("start", 7),
            ("z", 8),
            ("y", 9),
            ("x", 10),
            ("mode", 11),
        ];
        for (name, bit) in cases {
            assert_eq!(
                parse_button_mask(Some(name)).unwrap(),
                Some(1 << bit),
                "{name}"
            );
        }
    }

    #[test]
    fn parse_button_mask_accepts_plus_and_comma_and_is_case_insensitive() {
        let mask = parse_button_mask(Some("A+UP")).unwrap().unwrap();
        assert_eq!(mask, (1 << 6) | (1 << 0));
        let mask = parse_button_mask(Some(" Start , C ")).unwrap().unwrap();
        assert_eq!(mask, (1 << 7) | (1 << 5));
    }

    #[test]
    fn parse_button_mask_rejects_unknown_name() {
        assert!(parse_button_mask(Some("turbo")).is_err());
    }

    #[test]
    fn parse_button_mask_empty_string_is_zero() {
        assert_eq!(parse_button_mask(Some(" , ")).unwrap(), Some(0));
    }

    #[test]
    fn parse_press_schedule_none_for_missing_spec() {
        assert_eq!(parse_press_schedule(None).unwrap(), None);
    }

    #[test]
    fn parse_press_schedule_builds_sorted_setpoints() {
        let sched = parse_press_schedule(Some("186:;180:start"))
            .unwrap()
            .unwrap();
        assert_eq!(sched.points, vec![(180, 1 << 7), (186, 0)]);
    }

    #[test]
    fn press_schedule_mask_at_holds_until_next_setpoint() {
        let sched = parse_press_schedule(Some("180:start;186:"))
            .unwrap()
            .unwrap();
        assert_eq!(sched.mask_at(179), None);
        assert_eq!(sched.mask_at(180), Some(1 << 7));
        assert_eq!(sched.mask_at(185), Some(1 << 7));
        assert_eq!(sched.mask_at(186), Some(0));
        assert_eq!(sched.mask_at(10_000), Some(0));
    }

    #[test]
    fn parse_press_schedule_rejects_bad_frame_and_button() {
        assert!(parse_press_schedule(Some("notaframe:start")).is_err());
        assert!(parse_press_schedule(Some("12:bogus")).is_err());
        assert!(parse_press_schedule(Some("12start")).is_err());
    }

    #[test]
    fn parse_press_schedule_empty_spec_is_none() {
        assert_eq!(parse_press_schedule(Some(" ; ")).unwrap(), None);
    }
}
