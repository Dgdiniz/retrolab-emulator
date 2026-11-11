//! `ppu-dump`: run a ROM and summarise PPU state, with optional raw
//! memory dumps and viewer PNG exports.
//!
//! The default text summary answers the first rendering-diagnostic
//! questions: is the screen force-blanked, what BG mode is live, which
//! layers are enabled, and, crucially, *did the game upload any
//! graphics* (non-zero VRAM / CGRAM / OAM counts). When a screen is
//! black, an all-zero VRAM points at a stalled boot rather than a
//! renderer bug.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use retro_core::{EmulatorCore, InputState};
use snes1_core::{IoAccess, SnesEmulator};

#[derive(Args, Debug)]
pub struct PpuDumpArgs {
    /// Path to the ROM file (`.sfc` / `.smc`).
    pub rom: PathBuf,
    /// Run this many frames before dumping.
    #[arg(long, default_value_t = 300)]
    pub frames: u64,
    /// Dump raw VRAM (64 KiB, little-endian words) to this file.
    #[arg(long)]
    pub vram: Option<PathBuf>,
    /// Dump raw CGRAM (512 bytes, little-endian words) to this file.
    #[arg(long)]
    pub cgram: Option<PathBuf>,
    /// Dump raw OAM (544 bytes) to this file.
    #[arg(long)]
    pub oam: Option<PathBuf>,
    /// Export the 256-colour CGRAM as a 256x256 PNG swatch grid.
    #[arg(long)]
    pub palette_png: Option<PathBuf>,
    /// Export the BG tile set (16x16 4bpp tiles, palette 0) as a PNG.
    #[arg(long)]
    pub tiles_png: Option<PathBuf>,
    /// Export a BG tilemap as a PNG (see `--tilemap-bg` for which BG).
    #[arg(long)]
    pub tilemap_png: Option<PathBuf>,
    /// Which BG (`0..=3`) `--tilemap-png` exports.
    #[arg(long, default_value_t = 0)]
    pub tilemap_bg: usize,
    /// Write the text summary here instead of stdout.
    #[arg(long, short)]
    pub output: Option<PathBuf>,
    /// Capture the synchronisation-register I/O trace (RDNMI/TIMEUP/
    /// HVBJOY, APU mailbox `$2140-3`, INIDISP, the `$2137`/`$213C-F`
    /// H/V latch) and print its tail + a per-register tally after the
    /// run. Reveals the spin loop a stalled game is parked in and the
    /// register condition it polls.
    #[arg(long)]
    pub io_trace: bool,
    /// Ring-buffer size for `--io-trace`: keep the most recent N
    /// watched accesses leading up to the inspected frame.
    #[arg(long, default_value_t = 256)]
    pub io_trace_cap: usize,
    /// Capture the instruction-PC ring buffer and print the hottest
    /// PCs after the run. A tight main-thread spin loop (invisible to
    /// `--io-trace` because it touches no watched register) dominates
    /// this histogram.
    #[arg(long)]
    pub pc_trace: bool,
    /// Ring-buffer size for `--pc-trace`: keep the most recent N
    /// instruction PCs (~ one frame is tens of thousands).
    #[arg(long, default_value_t = 20000)]
    pub pc_trace_cap: usize,
    /// Watch a single WRAM cell (hex address, any alias, `$7E/$7F`
    /// direct or the low-8 KiB system-bank mirror) and print the tail of
    /// reads/writes to it, each with the PC + raster. Pins the exact code
    /// that touches a game-state variable (e.g. a stalled cutscene
    /// counter). Example: `--watch-addr 7E0158`.
    #[arg(long)]
    pub watch_addr: Option<String>,
    /// Ring-buffer size for `--watch-addr`: keep the most recent N
    /// accesses to the watched cell.
    #[arg(long, default_value_t = 256)]
    pub watch_addr_cap: usize,
    /// Print the per-scanline compositor configuration (window edges +
    /// per-layer enable/invert/combine, TM/TS/TMW/TSW screen designation,
    /// CGWSEL/CGADSUB/COLDATA colour-math). Diagnoses a render divergence
    /// when VRAM/CGRAM/OAM already match a reference.
    #[arg(long)]
    pub compositing: bool,
    /// Frame-scheduled player-1 input: `"F:buttons;F:buttons"` (e.g.
    /// `"250:start;262:"`). Reaches an input-gated test screen before the
    /// dump. Same grammar as `trace`/`screenshot --press-at`.
    #[arg(long)]
    pub press_at: Option<String>,
    /// Load low-confidence ROMs (blank title / bad checksum) by forcing
    /// the vector-implied mapping instead of failing the detector gate.
    #[arg(long)]
    pub force_detect: bool,
}

pub fn run(args: PpuDumpArgs) -> Result<()> {
    let rom = super::read_rom(&args.rom)?;
    let mut emu = SnesEmulator::new();
    super::apply_force_detect(&mut emu, &rom, args.force_detect);
    emu.load_rom(&rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;

    if args.io_trace {
        emu.enable_io_trace(args.io_trace_cap);
    }
    if args.pc_trace {
        emu.enable_pc_trace(args.pc_trace_cap);
    }
    let watch_addr = args.watch_addr.as_deref().map(parse_hex_u32).transpose()?;
    if let Some(addr) = watch_addr {
        emu.enable_addr_watch(addr, args.watch_addr_cap);
    }

    let schedule = super::parse_press_schedule(args.press_at.as_deref())?;
    for i in 0..args.frames {
        if let Some(sched) = &schedule {
            if let Some(buttons) = sched.mask_at(i) {
                emu.set_input(0, InputState { buttons });
            }
        }
        emu.run_frame();
    }

    // -- Immutable reads: raw state, counts, optional dumps/exports --
    let bgmode = emu.ppu().bgmode();
    let blank = emu.ppu().display_disable();
    let brightness = emu.ppu().display_brightness();
    let mode7 = emu.ppu().get_mode7_matrix();
    let vram_nonzero = nonzero_u16(emu.ppu().vram());
    let cgram_nonzero = nonzero_u16(emu.ppu().cgram());
    let oam_nonzero = nonzero_u8(emu.ppu().oam());

    if let Some(path) = &args.vram {
        write_bytes(path, &words_to_le_bytes(emu.ppu().vram()))?;
    }
    if let Some(path) = &args.cgram {
        write_bytes(path, &words_to_le_bytes(emu.ppu().cgram()))?;
    }
    if let Some(path) = &args.oam {
        write_bytes(path, emu.ppu().oam())?;
    }
    if let Some(path) = &args.palette_png {
        save_rgba_png(path, 256, 256, emu.export_palette_image())?;
    }
    if let Some(path) = &args.tiles_png {
        let img = emu.export_tile_image(snes1_core::TileSetSource::Bg, 0);
        save_rgba_png(path, img.width, img.height, img.rgba)?;
    }
    if let Some(path) = &args.tilemap_png {
        let img = emu.export_tilemap_image(args.tilemap_bg.min(3));
        save_rgba_png(path, img.width, img.height, img.rgba)?;
    }

    // -- Mutable read: TM/TS bytes + per-layer data via VideoDebugger --
    let (tm, ts, layers) = {
        let vd = emu.video_debugger();
        let regs = vd.get_registers();
        let find = |name: &str| {
            regs.iter()
                .find(|r| r.name == name)
                .map(|r| r.value)
                .unwrap_or(0)
        };
        let tm = find("TM");
        let ts = find("TS");
        let layers: Vec<(String, u32, u32, i32, i32, bool)> = (0..5)
            .filter_map(|id| vd.get_layer_data(id))
            .map(|l| (l.name, l.width, l.height, l.scroll_x, l.scroll_y, l.enabled))
            .collect();
        (tm, ts, layers)
    };

    let mut out = super::open_output(args.output.as_ref())?;
    writeln!(out, "PPU state after {} frame(s):", args.frames)?;
    writeln!(out, "  BG mode:           {bgmode}")?;
    writeln!(out, "  Forced blank:      {blank}")?;
    writeln!(out, "  Master brightness: {brightness}/15")?;
    writeln!(
        out,
        "  Main screen (TM):  {:<20} (${:02X})",
        decode_screen_layers(tm),
        tm
    )?;
    writeln!(
        out,
        "  Sub screen  (TS):  {:<20} (${:02X})",
        decode_screen_layers(ts),
        ts
    )?;
    writeln!(out, "  Layers:")?;
    for (name, w, h, sx, sy, enabled) in &layers {
        writeln!(
            out,
            "    {:<4} {:>4}x{:<4} scroll=({:>4},{:>4})  {}",
            name,
            w,
            h,
            sx,
            sy,
            if *enabled { "enabled" } else { "disabled" }
        )?;
    }
    writeln!(
        out,
        "  Mode7 matrix: A={} B={} C={} D={} X={} Y={}",
        mode7[0], mode7[1], mode7[2], mode7[3], mode7[4], mode7[5]
    )?;
    writeln!(out, "  VRAM:  {vram_nonzero}/32768 non-zero words")?;
    writeln!(out, "  CGRAM: {cgram_nonzero}/256 non-zero colors")?;
    writeln!(out, "  OAM:   {oam_nonzero}/544 non-zero bytes")?;

    if args.compositing {
        write!(out, "{}", emu.ppu().debug_compositing())?;
    }

    if args.io_trace {
        let trace = emu.take_io_trace();
        print_io_trace(&mut out, &trace)?;
    }
    if args.pc_trace {
        let trace = emu.take_pc_trace();
        print_pc_trace(&mut out, &trace)?;
    }
    if let Some(addr) = watch_addr {
        let trace = emu.take_addr_watch();
        print_addr_watch(&mut out, addr, &trace)?;
    }
    out.flush()?;
    Ok(())
}

/// Parse a hex address string (`7E0158`, `0x7E0158`, `$7E0158`).
fn parse_hex_u32(s: &str) -> Result<u32> {
    let t = s
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .trim_start_matches('$');
    u32::from_str_radix(t, 16).with_context(|| format!("invalid hex address: {s}"))
}

/// Print the captured address-watch tail: every read/write to the
/// watched WRAM cell, oldest first, with PC + raster. The PC of the
/// last write is the instruction that froze a stalled state variable.
fn print_addr_watch<W: Write>(out: &mut W, addr: u32, trace: &[IoAccess]) -> Result<()> {
    writeln!(
        out,
        "Address watch ${:06X} (last {} accesses):",
        addr,
        trace.len()
    )?;
    let (reads, writes) = trace.iter().fold((0usize, 0usize), |(r, w), a| {
        if a.is_write {
            (r, w + 1)
        } else {
            (r + 1, w)
        }
    });
    writeln!(out, "  {reads} reads, {writes} writes")?;
    for a in trace {
        writeln!(
            out,
            "  [v={:>3} h={:>3}] {} = ${:02X}  pc=${:06X}",
            a.v,
            a.h,
            if a.is_write { "W" } else { "R" },
            a.value,
            a.pc,
        )?;
    }
    Ok(())
}

/// Print the hottest program counters in the captured PC ring buffer.
/// A tight spin loop the main thread is parked in dominates the top of
/// the list; the contiguous span of hot PCs is the loop body.
fn print_pc_trace<W: Write>(out: &mut W, trace: &[u32]) -> Result<()> {
    use std::collections::HashMap;
    writeln!(out, "PC trace ({} instructions in window):", trace.len())?;
    let mut hist: HashMap<u32, usize> = HashMap::new();
    for &pc in trace {
        *hist.entry(pc).or_default() += 1;
    }
    let mut ranked: Vec<(u32, usize)> = hist.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let total = trace.len().max(1);
    for (pc, count) in ranked.into_iter().take(30) {
        writeln!(
            out,
            "  pc=${:06X}  {:>7} hits  ({:>5.1}%)",
            pc,
            count,
            100.0 * count as f64 / total as f64
        )?;
    }
    Ok(())
}

/// Human-readable name for a watched register low offset.
pub(crate) fn io_reg_name(addr: u32) -> &'static str {
    match addr & 0xFFFF {
        0x2100 => "INIDISP",
        0x2116 => "VMADDL",
        0x2117 => "VMADDH",
        0x2137 => "SLHV",
        0x213C => "OPHCT",
        0x213D => "OPVCT",
        0x213E => "STAT77",
        0x213F => "STAT78",
        0x2140 => "APUIO0",
        0x2141 => "APUIO1",
        0x2142 => "APUIO2",
        0x2143 => "APUIO3",
        0x4200 => "NMITIMEN",
        0x4207 => "HTIMEL",
        0x4208 => "HTIMEH",
        0x4209 => "VTIMEL",
        0x420A => "VTIMEH",
        0x420B => "MDMAEN",
        0x420C => "HDMAEN",
        0x4210 => "RDNMI",
        0x4211 => "TIMEUP",
        0x4212 => "HVBJOY",
        _ => "?",
    }
}

/// Print the captured I/O trace: a per-register read/write tally (a
/// tight poll loop shows up as a hot register) followed by the raw
/// access tail, oldest first.
fn print_io_trace<W: Write>(out: &mut W, trace: &[IoAccess]) -> Result<()> {
    use std::collections::BTreeMap;
    writeln!(out, "I/O trace (last {} watched accesses):", trace.len())?;
    let mut reads: BTreeMap<&str, usize> = BTreeMap::new();
    let mut writes: BTreeMap<&str, usize> = BTreeMap::new();
    for a in trace {
        let name = io_reg_name(a.addr);
        let tally = if a.is_write { &mut writes } else { &mut reads };
        *tally.entry(name).or_default() += 1;
    }
    writeln!(out, "  reads:  {reads:?}")?;
    writeln!(out, "  writes: {writes:?}")?;
    for a in trace {
        writeln!(
            out,
            "  [v={:>3} h={:>3}] {} ${:06X} = ${:02X}  pc=${:06X}  {}",
            a.v,
            a.h,
            if a.is_write { "W" } else { "R" },
            a.addr,
            a.value,
            a.pc,
            io_reg_name(a.addr),
        )?;
    }
    Ok(())
}

/// Count non-zero 16-bit words in a slice.
pub(crate) fn nonzero_u16(slice: &[u16]) -> usize {
    slice.iter().filter(|&&w| w != 0).count()
}

/// Count non-zero bytes in a slice.
pub(crate) fn nonzero_u8(slice: &[u8]) -> usize {
    slice.iter().filter(|&&b| b != 0).count()
}

/// Flatten a `u16` slice to little-endian bytes (the native VRAM/CGRAM
/// byte order a hex viewer expects).
pub(crate) fn words_to_le_bytes(words: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(words.len() * 2);
    for &w in words {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

/// Decode a TM/TS screen-designation byte into a layer-name list.
/// Bits 0-3 are BG1-BG4, bit 4 is OBJ (`$212C`/`$212D` layout).
pub(crate) fn decode_screen_layers(byte: u32) -> String {
    let mut parts = Vec::new();
    for bg in 0..4 {
        if byte & (1 << bg) != 0 {
            parts.push(format!("BG{}", bg + 1));
        }
    }
    if byte & 0x10 != 0 {
        parts.push("OBJ".to_string());
    }
    if parts.is_empty() {
        "(none)".to_string()
    } else {
        parts.join(" ")
    }
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)
        .with_context(|| format!("failed to write dump: {}", path.display()))?;
    eprintln!("wrote {} bytes to {}", bytes.len(), path.display());
    Ok(())
}

fn save_rgba_png(path: &Path, w: u32, h: u32, rgba: Vec<u8>) -> Result<()> {
    let img = image::RgbaImage::from_raw(w, h, rgba)
        .ok_or_else(|| anyhow::anyhow!("failed to build {w}x{h} RGBA image"))?;
    img.save(path)
        .with_context(|| format!("failed to save PNG: {}", path.display()))?;
    eprintln!("wrote {}x{} PNG to {}", w, h, path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonzero_counts() {
        assert_eq!(nonzero_u16(&[0, 1, 0, 2, 0]), 2);
        assert_eq!(nonzero_u8(&[0, 0, 0]), 0);
        assert_eq!(nonzero_u8(&[1, 2, 3]), 3);
    }

    #[test]
    fn words_to_le_bytes_is_little_endian() {
        assert_eq!(
            words_to_le_bytes(&[0x1234, 0x00FF]),
            vec![0x34, 0x12, 0xFF, 0x00]
        );
    }

    #[test]
    fn decode_screen_layers_bits() {
        assert_eq!(decode_screen_layers(0x00), "(none)");
        assert_eq!(decode_screen_layers(0x01), "BG1");
        assert_eq!(decode_screen_layers(0x13), "BG1 BG2 OBJ");
        assert_eq!(decode_screen_layers(0x10), "OBJ");
        assert_eq!(decode_screen_layers(0x0F), "BG1 BG2 BG3 BG4");
    }

    #[test]
    fn io_reg_name_decodes_watched_registers() {
        // Mirror bank bits are ignored, the low offset names the reg.
        assert_eq!(io_reg_name(0x00_2100), "INIDISP");
        assert_eq!(io_reg_name(0x80_4210), "RDNMI");
        assert_eq!(io_reg_name(0x00_4212), "HVBJOY");
        assert_eq!(io_reg_name(0x00_2140), "APUIO0");
        assert_eq!(io_reg_name(0x00_213D), "OPVCT");
        assert_eq!(io_reg_name(0x00_4200), "NMITIMEN");
        assert_eq!(io_reg_name(0x00_4209), "VTIMEL");
        assert_eq!(io_reg_name(0x00_2116), "VMADDL");
        assert_eq!(io_reg_name(0x00_420B), "MDMAEN");
        assert_eq!(io_reg_name(0x00_420C), "HDMAEN");
        // Anything outside the watch set falls back to "?".
        assert_eq!(io_reg_name(0x00_2118), "?");
    }
}
