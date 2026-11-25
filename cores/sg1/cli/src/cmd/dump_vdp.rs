//! `dump-vdp`: run a ROM for N frames and dump a full VDP state report
//! (registers + decoded settings, where the window plane is drawn, DMA
//! state, CRAM/VSRAM, VRAM usage statistics, timing, FIFO). Optionally also
//! writes a screenshot and/or the raw VRAM blob for external hex inspection.
//! The last frame stops at its capture boundary, as `screenshot
//! --render-layers` holds it: the report and the exports read the video
//! memory and the registers the picture was drawn from, before the game's
//! uploads in that frame's vertical blank.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use retro_cli::rom::{load, RomArgs};
use retro_cli::{InputArgs, RunEnd};
use retro_core::video::{PixelImage, VideoDebugger};
use retro_core::{DebuggerCore, ExecutionMode};
use sg1_core::vdp::window::WindowArea;
use sg1_core::vdp::{LAYER_PLANE_A, LAYER_PLANE_B, LAYER_WINDOW, TRANSPARENT_RGB};
use sg1_core::GenesisEmulator;

use crate::console::{Genesis, GenesisBoot};

#[derive(Args, Debug)]
pub struct DumpVdpArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Number of frames to run before dumping, e.g. `1`. The last stops at
    /// its capture boundary, its picture drawn and its vertical blank not
    /// begun: the dump reads what the picture was drawn from.
    #[arg(short, long)]
    pub frames: Option<u64>,
    #[command(flatten)]
    pub input: InputArgs,
    /// Also save a screenshot alongside the dump.
    #[arg(long)]
    pub screenshot: Option<PathBuf>,
    /// Save raw VRAM as a binary file for hex inspection.
    #[arg(long)]
    pub vram_bin: Option<PathBuf>,
    /// Write the report to a file instead of stdout.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Render CRAM as a 16x4 palette-swatch PNG (each cell 16x16 px).
    #[arg(long)]
    pub palette_png: Option<PathBuf>,
    /// Render the full VRAM tileset as a 16-tiles-wide PNG (palette 0;
    /// colour 0, transparent, in magenta).
    #[arg(long)]
    pub tiles_png: Option<PathBuf>,
    /// Render the plane A, plane B and window nametables as PNGs (the
    /// file's stem gets `_planeA` / `_planeB` / `_window` suffixes). Renders
    /// the raw tilemap with per-cell palette/flip, ignoring scroll,
    /// display-enable and where the window is drawn, so the scene shows
    /// even when the live composite is blanked. Transparent pixels are
    /// magenta, so black text stays visible.
    #[arg(long)]
    pub tilemap_png: Option<PathBuf>,
}

pub fn run(args: DumpVdpArgs<GenesisBoot>) -> Result<()> {
    let Some(frames) = args.frames else {
        anyhow::bail!("specify --frames");
    };
    let input = args.input.plan::<GenesisEmulator>()?;
    let (_, mut emu) = load::<Genesis>(&args.rom)?;
    super::print_rom_title(&emu);

    emu.debugger().set_execution_mode(ExecutionMode::Running);
    println!("Running {frames} frames...");
    input.run_frames(&mut emu, frames, RunEnd::CaptureBoundary);

    let report = build_vdp_report(&emu);
    if let Some(ref path) = args.output {
        std::fs::write(path, &report)
            .with_context(|| format!("failed to write report to: {}", path.display()))?;
        println!("VDP report saved to: {}", path.display());
    } else {
        println!("{report}");
    }

    if let Some(ref path) = args.screenshot {
        super::save_screenshot(&emu, path)?;
    }

    if let Some(ref path) = args.vram_bin {
        let vram = emu.vdp().get_vram();
        std::fs::write(path, vram)
            .with_context(|| format!("failed to write VRAM to: {}", path.display()))?;
        println!(
            "VRAM binary saved: {} ({} bytes)",
            path.display(),
            vram.len()
        );
    }

    if let Some(ref path) = args.palette_png {
        export_palette_png(&emu, path)?;
    }
    if let Some(ref path) = args.tiles_png {
        export_tiles_png(&emu, path)?;
    }
    if let Some(ref path) = args.tilemap_png {
        export_tilemap_png(&emu, path)?;
    }
    Ok(())
}

/// Render CRAM as a 16-wide x 4-tall swatch grid (each color a 16x16 px
/// block), so all four palettes are visible at a glance, each colour as the
/// screen shows it.
fn export_palette_png(emu: &GenesisEmulator, path: &std::path::Path) -> Result<()> {
    use sg1_core::vdp::Vdp;
    const CELL: u32 = 16;
    let cram = emu.vdp().get_cram();
    let (w, h) = (16 * CELL, 4 * CELL);
    let mut img = image::RgbImage::new(w, h);
    for (idx, &raw) in cram.iter().enumerate().take(64) {
        let (r, g, b) = Vdp::convert_genesis_color(raw);
        let (cx, cy) = ((idx % 16) as u32 * CELL, (idx / 16) as u32 * CELL);
        for dy in 0..CELL {
            for dx in 0..CELL {
                img.put_pixel(cx + dx, cy + dy, image::Rgb([r, g, b]));
            }
        }
    }
    img.save(path)
        .with_context(|| format!("failed to save palette PNG: {}", path.display()))?;
    println!("Palette PNG saved: {} ({}x{})", path.display(), w, h);
    Ok(())
}

/// Render every 8x8 tile in VRAM (2048 tiles) as a 16-tiles-wide sheet,
/// coloured with palette 0, with colour 0 (transparent) in
/// [`TRANSPARENT_RGB`]. Useful to confirm the pattern data is loaded.
fn export_tiles_png(emu: &GenesisEmulator, path: &std::path::Path) -> Result<()> {
    use sg1_core::vdp::tiles::{build_rgba_palette, Tile};
    let vram = emu.vdp().get_vram();
    let palette = build_rgba_palette(emu.vdp().get_cram(), 0);
    let total_tiles = vram.len() / 32;
    let cols = 16u32;
    let rows = total_tiles.div_ceil(cols as usize) as u32;
    let (w, h) = (cols * 8, rows * 8);
    let mut img = image::RgbImage::new(w, h);
    for t in 0..total_tiles {
        let tile = Tile::decode(&vram[t * 32..t * 32 + 32]);
        let (ox, oy) = ((t as u32 % cols) * 8, (t as u32 / cols) * 8);
        for py in 0..8u32 {
            for px in 0..8u32 {
                let pix = tile.get_pixel(px as u8, py as u8, false, false);
                img.put_pixel(ox + px, oy + py, pixel_rgb(&palette, pix));
            }
        }
    }
    img.save(path)
        .with_context(|| format!("failed to save tiles PNG: {}", path.display()))?;
    println!(
        "Tiles PNG saved: {} ({}x{}, {} tiles)",
        path.display(),
        w,
        h,
        total_tiles
    );
    Ok(())
}

/// Write the whole maps of plane A, plane B and the window as PNGs, as
/// the core draws them ([`VideoDebugger::layer_map`]): the raw tilemap with
/// each cell's palette and flips, with no scroll, no display-enable and no
/// window area, so the scene shows even when the live composite is blanked.
/// Writes three files with `_planeA` / `_planeB` / `_window` suffixes on
/// the given path's stem, their transparent pixels in [`TRANSPARENT_RGB`].
fn export_tilemap_png(emu: &GenesisEmulator, path: &Path) -> Result<()> {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "tilemap".to_string());
    let ext = path
        .extension()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "png".to_string());
    let parent = path.parent().unwrap_or_else(|| Path::new("."));

    for (name, layer) in [
        ("planeA", LAYER_PLANE_A),
        ("planeB", LAYER_PLANE_B),
        ("window", LAYER_WINDOW),
    ] {
        let out = parent.join(format!("{stem}_{name}.{ext}"));
        let map = emu
            .vdp()
            .layer_map(layer as u32)
            .ok_or_else(|| anyhow::anyhow!("the VDP drew no map of {name}"))?;
        save_map_png(&map, &out)?;
    }
    Ok(())
}

/// A tile pixel's colour in `palette`, colour 0 being transparent.
fn pixel_rgb(palette: &[(u8, u8, u8, u8); 16], pix: u8) -> image::Rgb<u8> {
    if pix == 0 {
        let (r, g, b) = TRANSPARENT_RGB;
        return image::Rgb([r, g, b]);
    }
    let (r, g, b, _) = palette[usize::from(pix & 0x0F)];
    image::Rgb([r, g, b])
}

/// Save a map to a PNG, its transparent pixels in [`TRANSPARENT_RGB`].
fn save_map_png(map: &PixelImage, path: &Path) -> Result<()> {
    let (r, g, b) = TRANSPARENT_RGB;
    let rgb: Vec<u8> = map
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[red, green, blue, alpha]| match alpha {
            0 => [r, g, b],
            _ => [red, green, blue],
        })
        .collect();
    let img = image::RgbImage::from_raw(map.width, map.height, rgb)
        .ok_or_else(|| anyhow::anyhow!("a {}x{} map", map.width, map.height))?;
    img.save(path)
        .with_context(|| format!("failed to save tilemap PNG: {}", path.display()))?;
    println!(
        "Tilemap PNG saved: {} ({}x{})",
        path.display(),
        map.width,
        map.height
    );
    Ok(())
}

/// Build the full VDP state text report.
fn build_vdp_report(emu: &GenesisEmulator) -> String {
    use sg1_core::vdp::Vdp;
    let vdp = emu.vdp();
    let regs = vdp.registers();
    let mut report = String::new();

    report.push_str("=== VDP Registers ===\n");
    let reg_names = [
        "R00: Mode 1",
        "R01: Mode 2",
        "R02: Plane A Name Table",
        "R03: Window Name Table",
        "R04: Plane B Name Table",
        "R05: Sprite Table",
        "R06: (unused)",
        "R07: Backdrop Color",
        "R08: (unused)",
        "R09: (unused)",
        "R10: H-Int Counter",
        "R11: Mode 3",
        "R12: Mode 4",
        "R13: H-Scroll Table",
        "R14: (unused)",
        "R15: Auto-Increment",
        "R16: Scroll Size",
        "R17: Window H Position",
        "R18: Window V Position",
        "R19: DMA Length Low",
        "R20: DMA Length High",
        "R21: DMA Source Low",
        "R22: DMA Source Mid",
        "R23: DMA Source High/Type",
    ];
    for (i, name) in reg_names.iter().enumerate() {
        report.push_str(&format!(
            "  {} = 0x{:02X} ({:08b})\n",
            name, regs[i], regs[i]
        ));
    }

    report.push_str("\n=== Decoded Settings ===\n");
    report.push_str(&format!("  Display Enabled: {}\n", (regs[1] & 0x40) != 0));
    report.push_str(&format!("  DMA Enabled: {}\n", (regs[1] & 0x10) != 0));
    report.push_str(&format!(
        "  VBlank Interrupt Enabled: {}\n",
        (regs[1] & 0x20) != 0
    ));
    report.push_str(&format!(
        "  HBlank Interrupt Enabled: {}\n",
        (regs[0] & 0x10) != 0
    ));

    let h40 = (regs[12] & 0x81) != 0;
    let v30 = (regs[1] & 0x08) != 0;
    report.push_str(&format!(
        "  Display Mode: {}x{} ({}x{})\n",
        if h40 { 320 } else { 256 },
        if v30 { 240 } else { 224 },
        if h40 { "H40" } else { "H32" },
        if v30 { "V30" } else { "V28" }
    ));
    report.push_str(&format!(
        "  Backdrop: palette {}, color {}\n",
        (regs[7] >> 4) & 0x3,
        regs[7] & 0x0F
    ));
    report.push_str(&format!("  Auto-Increment: {} bytes\n", regs[15]));

    let scroll_size = |bits: u8| match bits & 0x03 {
        0 => "32 cells",
        1 => "64 cells",
        3 => "128 cells",
        _ => "invalid",
    };
    report.push_str(&format!(
        "  Scroll Size: H={}, V={}\n",
        scroll_size(regs[16]),
        scroll_size(regs[16] >> 4)
    ));

    report.push_str(&format!(
        "  Plane A Nametable: 0x{:04X}\n",
        vdp.get_plane_a_nametable()
    ));
    report.push_str(&format!(
        "  Plane B Nametable: 0x{:04X}\n",
        vdp.get_plane_b_nametable()
    ));
    report.push_str(&format!(
        "  Window Nametable:  0x{:04X}\n",
        vdp.get_window_nametable()
    ));
    report.push_str(&format!(
        "  Window Area:       {} (R17=0x{:02X}, R18=0x{:02X})\n",
        describe_window_area(
            vdp.window_area(),
            vdp.display_width() as u16,
            vdp.display_height() as u16
        ),
        regs[17],
        regs[18]
    ));
    report.push_str(&format!(
        "  Sprite Table:      0x{:04X}\n",
        (regs[5] as u16 & 0x7F) << 9
    ));
    report.push_str(&format!(
        "  HScroll Table:     0x{:04X}\n",
        (regs[13] as u16 & 0x3F) << 10
    ));

    report.push_str("\n=== DMA State ===\n");
    let dma_state = vdp.dma_state();
    report.push_str(&format!("  Active: {}\n", dma_state.is_active()));
    report.push_str(&format!("  CPU Blocked: {}\n", dma_state.cpu_blocked));
    report.push_str(&format!(
        "  Cycles Remaining: {}\n",
        dma_state.cycles_remaining
    ));
    if let Some(ref op) = dma_state.operation {
        report.push_str(&format!("  Operation: {op:?}\n"));
    } else {
        report.push_str("  Operation: None (idle)\n");
    }
    report.push_str(&format!(
        "  Pending (Memory-to-VRAM): {}\n",
        vdp.is_dma_pending()
    ));

    let dma_length = ((regs[20] as u32) << 8) | (regs[19] as u32);
    // For a 68000-memory DMA, bit 6 of register 23 is source address bit 23
    // (work RAM at 0xFF0000 and up), so seven bits belong to the address.
    let dma_source_raw =
        ((regs[23] as u32 & 0x7F) << 16) | ((regs[22] as u32) << 8) | (regs[21] as u32);
    let dma_mode = (regs[23] >> 6) & 0x03;
    let dma_mode_name = match dma_mode {
        0 | 1 => "Memory-to-VRAM",
        2 => "VRAM Fill",
        3 => "VRAM Copy",
        _ => "Unknown",
    };
    report.push_str(&format!(
        "  DMA Registers: length={} (0x{:04X}), source=0x{:06X}, mode={} ({})\n",
        dma_length,
        dma_length,
        dma_source_raw << 1,
        dma_mode,
        dma_mode_name
    ));

    let (access_addr, access_code) = vdp.current_access();
    let access_target = match access_code & 0x0F {
        0x01 => "VRAM",
        0x03 => "CRAM",
        0x05 => "VSRAM",
        _ => "Unknown",
    };
    report.push_str(&format!(
        "  Current Access: addr=0x{access_addr:04X}, code=0x{access_code:02X} ({access_target})\n"
    ));

    report.push_str("\n=== CRAM (Color RAM) - 4 palettes x 16 colors ===\n");
    let cram = vdp.get_cram();
    for pal in 0..4 {
        report.push_str(&format!("  Palette {pal}:"));
        let mut all_zero = true;
        for col in 0..16 {
            let raw = cram[pal * 16 + col];
            if raw != 0 {
                all_zero = false;
            }
            let (r, g, b) = Vdp::convert_genesis_color(raw);
            if col % 8 == 0 && col > 0 {
                report.push_str("\n            ");
            }
            report.push_str(&format!(" {raw:04X}(#{r:02X}{g:02X}{b:02X})"));
        }
        if all_zero {
            report.push_str(" [ALL ZERO]");
        }
        report.push('\n');
    }

    report.push_str("\n=== VSRAM (Vertical Scroll RAM) ===\n");
    let vsram = vdp.get_vsram();
    report.push_str("  ");
    let mut vsram_non_zero = 0;
    for (i, &val) in vsram.iter().enumerate() {
        if val != 0 {
            vsram_non_zero += 1;
        }
        if i > 0 && i % 10 == 0 {
            report.push_str("\n  ");
        }
        report.push_str(&format!("{val:04X} "));
    }
    report.push_str(&format!(
        "\n  Non-zero entries: {}/{}\n",
        vsram_non_zero,
        vsram.len()
    ));

    report.push_str("\n=== VRAM Statistics ===\n");
    let vram = vdp.get_vram();
    report.push_str(&format!(
        "  Total size: {} bytes ({} KB)\n",
        vram.len(),
        vram.len() / 1024
    ));
    let non_zero_bytes = vram.iter().filter(|&&b| b != 0).count();
    report.push_str(&format!(
        "  Non-zero bytes: {} ({:.1}%)\n",
        non_zero_bytes,
        (non_zero_bytes as f64 / vram.len() as f64) * 100.0
    ));

    let tile_size = 32;
    let total_tiles = vram.len() / tile_size;
    let non_zero_tiles = (0..total_tiles)
        .filter(|&t| {
            vram[t * tile_size..t * tile_size + tile_size]
                .iter()
                .any(|&b| b != 0)
        })
        .count();
    report.push_str(&format!(
        "  Non-zero tiles: {}/{} ({:.1}%)\n",
        non_zero_tiles,
        total_tiles,
        (non_zero_tiles as f64 / total_tiles as f64) * 100.0
    ));

    report.push_str("  Non-zero VRAM regions:\n");
    let mut in_region = false;
    let mut region_start = 0;
    let mut region_count = 0;
    for i in (0..vram.len()).step_by(32) {
        let end = (i + 32).min(vram.len());
        let has_data = vram[i..end].iter().any(|&b| b != 0);
        if has_data && !in_region {
            region_start = i;
            in_region = true;
        } else if !has_data && in_region {
            if region_count < 20 {
                report.push_str(&format!(
                    "    0x{:04X} - 0x{:04X} ({} bytes)\n",
                    region_start,
                    i - 1,
                    i - region_start
                ));
            }
            region_count += 1;
            in_region = false;
        }
    }
    if in_region {
        if region_count < 20 {
            report.push_str(&format!(
                "    0x{:04X} - 0x{:04X} ({} bytes)\n",
                region_start,
                vram.len() - 1,
                vram.len() - region_start
            ));
        }
        region_count += 1;
    }
    if region_count > 20 {
        report.push_str(&format!("    ... and {} more regions\n", region_count - 20));
    }

    report.push_str("\n=== Timing State ===\n");
    let timing = vdp.timing();
    report.push_str(&format!("  Frame Count: {}\n", timing.frame_count));
    report.push_str(&format!("  Scanline: {}\n", timing.scanline));
    report.push_str(&format!("  H Counter: {}\n", timing.h_counter));
    report.push_str(&format!("  In VBlank: {}\n", timing.in_vblank));
    report.push_str(&format!("  In HBlank: {}\n", timing.in_hblank));
    report.push_str(&format!("  VBlank Pending: {}\n", timing.vblank_pending));
    report.push_str(&format!("  HBlank Pending: {}\n", timing.hblank_pending));

    report.push_str("\n=== FIFO State ===\n");
    report.push_str(&format!("  Empty: {}\n", vdp.is_fifo_empty()));
    report.push_str(&format!("  Full: {}\n", vdp.is_fifo_full()));

    report
}

/// Where the window replaces plane A on a `width` x `height` display, in
/// words: `whole screen`, `none`, or the lines it takes whole and the
/// columns it takes on the other lines. Read through the renderer's own
/// [`WindowArea`], so the report cannot disagree with the picture.
fn describe_window_area(area: WindowArea, width: u16, height: u16) -> String {
    let lines = span(height, |y| area.takes_line(y));
    let columns = span(width, |x| area.takes_column(x));
    if lines == Some((0, height - 1)) || columns == Some((0, width - 1)) {
        return "whole screen".to_string();
    }
    match (lines, columns) {
        (None, None) => "none".to_string(),
        (Some((first, last)), None) => format!("lines {first}-{last}"),
        (None, Some((first, last))) => format!("columns {first}-{last}"),
        (Some((l0, l1)), Some((c0, c1))) => {
            format!("lines {l0}-{l1}, and columns {c0}-{c1} of the other lines")
        }
    }
}

/// The first and last of `0..n` where `takes` holds, or `None` when it
/// never does. The window's lines and columns are each one run from an
/// edge, so the pair names all of them.
fn span(n: u16, takes: impl Fn(u16) -> bool) -> Option<(u16, u16)> {
    let first = (0..n).find(|&i| takes(i))?;
    let last = (0..n).rev().find(|&i| takes(i))?;
    Some((first, last))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn describe(reg17: u8, reg18: u8) -> String {
        describe_window_area(WindowArea::from_registers(reg17, reg18), 320, 224)
    }

    #[test]
    fn describes_the_window_area_in_words() {
        // Rings of Power's town: the window covers everything.
        assert_eq!(describe(0x80, 0x00), "whole screen");
        assert_eq!(describe(0x00, 0x80), "whole screen");
        assert_eq!(describe(0x00, 0x00), "none");
        assert_eq!(describe(0x8A, 0x00), "columns 160-319");
        assert_eq!(describe(0x0A, 0x00), "columns 0-159");
        assert_eq!(describe(0x00, 0x85), "lines 40-223");
        assert_eq!(describe(0x00, 0x05), "lines 0-39");
        assert_eq!(
            describe(0x04, 0x99),
            "lines 200-223, and columns 0-63 of the other lines"
        );
    }

    #[test]
    fn colour_zero_is_painted_transparent() {
        let mut palette = [(0u8, 0u8, 0u8, 255u8); 16];
        palette[0] = (36, 73, 109, 255); // what colour 0 holds does not matter
        palette[1] = (0, 0, 0, 255); // a black the tile really draws
        let (r, g, b) = TRANSPARENT_RGB;
        assert_eq!(pixel_rgb(&palette, 0), image::Rgb([r, g, b]));
        assert_eq!(pixel_rgb(&palette, 1), image::Rgb([0, 0, 0]));
    }

    /// A machine with no game whose VDP registers are set through the
    /// control port, mode 5 first (the registers past 10 take no write in
    /// mode 4), plane A at `$C000`, the window at `$B000` and plane B at
    /// `$E000`, register 12 and register 16 as given, and whose VRAM and CRAM
    /// are filled through the debugger's writes with values that look random
    /// and are the same on every run.
    fn scrambled(register_12: u8, register_16: u8, seed: u32) -> GenesisEmulator {
        let mut emu = GenesisEmulator::new();
        let mut state = seed;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        let vram: Vec<u8> = (0..0x1_0000).map(|_| next() as u8).collect();
        let cram: Vec<u16> = (0..64).map(|_| next() as u16 & 0x0EEE).collect();
        emu.vdp_mut().write_vram_debug(0, &vram);
        emu.vdp_mut().write_cram_debug(0, &cram);
        for (register, value) in [
            (1u16, 0x04u8),
            (2, 0x30),
            (3, 0x2C),
            (4, 0x07),
            (12, register_12),
            (16, register_16),
        ] {
            emu.vdp_mut()
                .write_word_port(0xC0_0004, 0x8000 | (register << 8) | u16::from(value));
        }
        emu
    }

    /// The hashes of the three files `export_tilemap_png` writes for a
    /// machine, plane A's, plane B's and the window's, and their sizes.
    fn tilemap_files(emu: &GenesisEmulator, case: &str) -> [(u64, (u32, u32)); 3] {
        let dir =
            std::env::temp_dir().join(format!("sg1-cli-tilemap-{}-{case}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        export_tilemap_png(emu, &dir.join("map.png")).unwrap();
        let files = ["planeA", "planeB", "window"].map(|name| {
            let path = dir.join(format!("map_{name}.png"));
            let bytes = std::fs::read(&path).unwrap();
            let size = image::open(&path).unwrap().to_rgb8().dimensions();
            (retro_core::fnv1a64(&bytes), size)
        });
        std::fs::remove_dir_all(&dir).unwrap();
        files
    }

    /// The files `--tilemap-png` writes, held to the bytes they had when the
    /// maps were drawn in this tool: every plane size the VDP takes, both
    /// cell modes and interlace mode 2, over scrambled video memory.
    #[test]
    fn the_tilemap_pngs_keep_the_bytes_they_had() {
        const PINNED: [(&str, u8, u8, [u64; 3]); 7] = [
            (
                "h40-64x32",
                0x81,
                0x01,
                [
                    0xC91F_F6DD_4173_38CA,
                    0xB1CE_D057_CD97_98B4,
                    0xE03F_620D_D9D7_8D5C,
                ],
            ),
            (
                "h32-32x32",
                0x00,
                0x00,
                [
                    0x1779_C8A8_1C1D_CFEB,
                    0xE8FB_AA10_4245_024A,
                    0xF936_8F33_9F8D_9ED3,
                ],
            ),
            (
                "h40-128x32",
                0x81,
                0x03,
                [
                    0xC7A1_898C_ED0F_9494,
                    0xBE54_FE7D_0FF7_C96B,
                    0x8D9C_AE18_E5ED_245D,
                ],
            ),
            (
                "h32-32x64",
                0x00,
                0x10,
                [
                    0x63A6_0634_4D95_60CA,
                    0x600B_42A0_E27C_6102,
                    0xBB94_7BFC_6A4B_1AD9,
                ],
            ),
            (
                "h40-64x64",
                0x81,
                0x11,
                [
                    0xF6DB_D0A3_8EEA_4DA6,
                    0x3986_C991_CCF6_CDB5,
                    0x9C4F_A4A0_2914_A170,
                ],
            ),
            (
                "h40-32x128",
                0x81,
                0x30,
                [
                    0x6C60_3442_9E92_9592,
                    0x7FA4_36EF_A76D_4F55,
                    0xFED9_ECD6_45A5_E528,
                ],
            ),
            (
                "interlace-64x32",
                0x87,
                0x01,
                [
                    0xF83E_188F_2B07_9B3A,
                    0x6748_1859_F43A_49F4,
                    0x6CFE_78E9_EB08_BD54,
                ],
            ),
        ];
        for (seed, (case, register_12, register_16, pinned)) in PINNED.into_iter().enumerate() {
            let emu = scrambled(register_12, register_16, 101 + seed as u32);
            let hashes = tilemap_files(&emu, case).map(|(hash, _)| hash);
            assert_eq!(hashes, pinned, "{case}: {hashes:#018X?}");
        }
    }

    /// A register 16 the VDP does not take draws the map at the size the
    /// layer list reports for it: 64 by 64 cells for each value here.
    #[test]
    fn an_invalid_plane_size_draws_the_map_at_the_size_the_layers_report() {
        for register_16 in [0x02, 0x22, 0x33] {
            let emu = scrambled(0x81, register_16, 7);
            let sizes =
                tilemap_files(&emu, &format!("invalid-{register_16:02x}")).map(|(_, size)| size);
            assert_eq!(
                sizes,
                [(512, 512), (512, 512), (512, 256)],
                "register 16 {register_16:#04X}"
            );
        }
    }

    #[test]
    fn an_edge_past_the_display_gives_all_or_nothing() {
        assert_eq!(describe(0x1F, 0x00), "whole screen");
        assert_eq!(describe(0x9F, 0x00), "none");
        // H32 is 256 pixels wide: an edge at pixel 256 from the right is
        // past it, from the left it takes every column.
        let area = |r17| WindowArea::from_registers(r17, 0x00);
        assert_eq!(describe_window_area(area(0x90), 256, 224), "none");
        assert_eq!(describe_window_area(area(0x10), 256, 224), "whole screen");
    }
}
