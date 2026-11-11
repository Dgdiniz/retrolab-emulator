//! Background renderer benchmarks.
//!
//! Renders a full Mode 1 frame (BG1 4bpp + BG2 4bpp + BG3 2bpp, 224
//! visible scanlines) through [`Ppu::render_scanline`]. The scene is
//! static, so the per-BG tilemap-row cache hits for the 7 trailing
//! scanlines of every 8-line tile band, this benchmark is the
//! steady-state measurement for that cache.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use retro_core::cpu::Addressable;
use retro_core::Framebuffer;
use snes1_core::SnesBus;

/// PPU register address in bank `$00`.
fn ppu_addr(reg: u16) -> u32 {
    u32::from(reg) & 0x00FF_FFFF
}

/// Write one VRAM word at `word_addr` via auto-increment step 1.
fn write_vram_word(bus: &mut SnesBus, word_addr: u16, value: u16) {
    bus.write(ppu_addr(0x2115), 0x80);
    bus.write(ppu_addr(0x2116), (word_addr & 0xFF) as u8);
    bus.write(ppu_addr(0x2117), ((word_addr >> 8) & 0xFF) as u8);
    bus.write(ppu_addr(0x2118), (value & 0xFF) as u8);
    bus.write(ppu_addr(0x2119), ((value >> 8) & 0xFF) as u8);
}

/// Write one CGRAM 15-bit BGR colour at `color_index`.
fn write_cgram(bus: &mut SnesBus, color_index: u8, bgr15: u16) {
    bus.write(ppu_addr(0x2121), color_index);
    bus.write(ppu_addr(0x2122), (bgr15 & 0xFF) as u8);
    bus.write(ppu_addr(0x2122), ((bgr15 >> 8) & 0x7F) as u8);
}

/// Lay out a static Mode 1 scene with a fully-populated 32x32 tilemap
/// for each of the three visible backgrounds plus a handful of 4bpp /
/// 2bpp character tiles and a 16-colour palette.
fn setup_mode1_scene() -> SnesBus {
    let mut bus = SnesBus::new();
    // VBlank snapshot so the setup VRAM / CGRAM writes pass the access
    // gate, then enable the display at full brightness.
    bus.set_raster_snapshot(0, 225);
    bus.write(ppu_addr(0x2100), 0x0F);

    // Mode 1 (BG1/BG2 4bpp, BG3 2bpp).
    bus.write(ppu_addr(0x2105), 0x01);
    // Tile data base 0 for every BG; tilemaps at 0x0800 / 0x1000 /
    // 0x1800 (byte addresses).
    bus.write(ppu_addr(0x210B), 0x00);
    bus.write(ppu_addr(0x210C), 0x00);
    bus.write(ppu_addr(0x2107), 0x04);
    bus.write(ppu_addr(0x2108), 0x08);
    bus.write(ppu_addr(0x2109), 0x0C);

    // Sixteen 4bpp character tiles (16 words each), each filled with a
    // single colour index so the decoded rows are cheap but distinct.
    for tile in 0u16..16 {
        let base = tile * 16;
        let p = (tile & 0x0F) as u8;
        let plane = |bit: u8| -> u16 {
            if p & bit != 0 {
                0x00FF
            } else {
                0x0000
            }
        };
        for row in 0u16..8 {
            let lo = plane(1) | (plane(2) << 8);
            let hi = plane(4) | (plane(8) << 8);
            write_vram_word(&mut bus, base + row, lo);
            write_vram_word(&mut bus, base + 8 + row, hi);
        }
    }

    // Fill the three tilemaps (32x32 = 0x400 entries each) with a
    // rolling tile index so adjacent columns differ.
    for slot in 0u16..0x400 {
        let tile = slot % 16;
        write_vram_word(&mut bus, 0x0400 + slot, tile); // BG1
        write_vram_word(&mut bus, 0x0800 + slot, (tile + 5) % 16); // BG2
        write_vram_word(&mut bus, 0x0C00 + slot, (tile + 9) % 16); // BG3
    }

    // A 16-colour palette (non-zero so opaque pixels show).
    for idx in 1u8..16 {
        write_cgram(&mut bus, idx, u16::from(idx) * 0x0421);
    }

    // Enable BG1 + BG2 + BG3 on the main screen.
    bus.write(ppu_addr(0x212C), 0x07);
    bus
}

fn bench_mode1_frame(c: &mut Criterion) {
    let mut bus = setup_mode1_scene();
    let mut fb = Framebuffer::new(256, 224);
    c.bench_function("ppu_render_mode1_frame", |b| {
        b.iter(|| {
            let ppu = bus.ppu_mut();
            for line in 1u16..=224 {
                ppu.render_scanline(black_box(line), &mut fb);
            }
            black_box(fb.data[0]);
        });
    });
}

criterion_group!(benches, bench_mode1_frame);
criterion_main!(benches);
