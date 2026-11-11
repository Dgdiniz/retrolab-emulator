//! Sprite (OBJ) renderer benchmark.
//!
//! Renders a sprite-heavy Mode 1 frame (BG1 fill + 128 sprites
//! spread across the visible scanlines) through
//! [`Ppu::render_scanline`]. The scene is static and no VRAM write
//! lands inside the timing loop, so the `vram_generation` counter is
//! frozen and the decoded-sprite-tile cache is fully populated after
//! the first iteration, this benchmark is the steady-state
//! measurement for that cache (every later iteration is pure cache
//! hits on the per-scanline OAM eval + composite).

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

/// Park the OAM byte pointer at `byte_addr` (even, word-aligned).
fn set_oam_addr(bus: &mut SnesBus, byte_addr: u16) {
    bus.write(ppu_addr(0x2103), ((byte_addr >> 9) & 0x01) as u8);
    bus.write(ppu_addr(0x2102), ((byte_addr >> 1) & 0xFF) as u8);
}

/// Program sprite `idx`'s four low-table bytes (high table stays 0,
/// i.e. small size + X bit 8 clear).
fn write_oam_sprite(bus: &mut SnesBus, idx: u16, x: u8, y: u8, character: u8, attr: u8) {
    set_oam_addr(bus, idx * 4);
    bus.write(ppu_addr(0x2104), x);
    bus.write(ppu_addr(0x2104), y);
    bus.write(ppu_addr(0x2104), character);
    bus.write(ppu_addr(0x2104), attr);
}

/// Lay out a sprite-heavy Mode 1 scene: a small BG1 fill plus 128
/// 16x16 sprites spread across the screen, all drawing from a shared
/// pool of 4bpp character tiles so the decoded-tile cache sees heavy
/// reuse.
fn setup_sprite_scene() -> SnesBus {
    let mut bus = SnesBus::new();
    // VBlank snapshot so the setup writes pass the access gate, then
    // enable the display at full brightness.
    bus.set_raster_snapshot(0, 225);
    bus.write(ppu_addr(0x2100), 0x0F);

    // Mode 1; BG1 tilemap at byte 0x0800, BG/OBJ tile data at 0.
    bus.write(ppu_addr(0x2105), 0x01);
    bus.write(ppu_addr(0x210B), 0x00);
    bus.write(ppu_addr(0x2107), 0x04);

    // OBSEL: size selector 3 (small 16x16), tile base 0, name 0.
    bus.write(ppu_addr(0x2101), 0x60);

    // Character tiles 0..63 (4bpp, 16 words each) filled so each row
    // carries a non-zero colour index, enough to exercise the
    // decode + composite path for every sprite tile column.
    for tile in 0u16..64 {
        let base = tile * 16;
        let p = (tile & 0x0F) as u8 | 0x01;
        let plane = |bit: u8| -> u16 {
            if p & bit != 0 {
                0x00FF
            } else {
                0x0000
            }
        };
        for row in 0u16..8 {
            write_vram_word(&mut bus, base + row, plane(1) | (plane(2) << 8));
            write_vram_word(&mut bus, base + 8 + row, plane(4) | (plane(8) << 8));
        }
    }

    // BG1 tilemap fill (32x32) with a rolling tile index.
    for slot in 0u16..0x400 {
        write_vram_word(&mut bus, 0x0400 + slot, slot % 16);
    }

    // BG palette (indices 1..15) + sprite palette 0 (CGRAM 128..143).
    for idx in 1u8..16 {
        write_cgram(&mut bus, idx, u16::from(idx) * 0x0421);
    }
    for idx in 0u8..16 {
        write_cgram(&mut bus, 128 + idx, (0x0421 * u16::from(idx)) | 0x4000);
    }

    // 128 sprites spread across the visible field, sharing characters
    // 0..15 so the decoded-tile cache is exercised hard.
    for i in 0u16..128 {
        let x = ((i * 7) % 240) as u8;
        let y = ((i * 13) % 216) as u8;
        let character = (i % 16) as u8;
        write_oam_sprite(&mut bus, i, x, y, character, 0x00);
    }

    // Enable BG1 + OBJ on the main screen.
    bus.write(ppu_addr(0x212C), 0x11);
    bus
}

fn bench_sprite_heavy_frame(c: &mut Criterion) {
    let mut bus = setup_sprite_scene();
    let mut fb = Framebuffer::new(256, 224);
    c.bench_function("ppu_render_sprite_heavy_frame", |b| {
        b.iter(|| {
            let ppu = bus.ppu_mut();
            for line in 1u16..=224 {
                ppu.render_scanline(black_box(line), &mut fb);
            }
            black_box(fb.data[0]);
        });
    });
}

criterion_group!(benches, bench_sprite_heavy_frame);
criterion_main!(benches);
