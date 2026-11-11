//! Colour-math + window renderer benchmark.
//!
//! Renders a Mode 1 frame with the full colour-math slow path active:
//! BG1 on the main screen, BG2 on the sub screen, sub-screen add-math
//! enabled, a per-layer window clip on BG1, and a colour-window region
//! gating the main screen. This forces [`Ppu::render_scanline`] to
//! build the sub plane, the per-layer clip tables, and the COL region
//! tables, then run the per-pixel `resolve_pixel` pass for every
//! visible scanline, i.e. it measures the path the background and
//! sprite benchmarks (`ppu_render`, `ppu_sprites`) deliberately skip.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use retro_core::cpu::Addressable;
use retro_core::Framebuffer;
use snes1_core::SnesBus;

fn ppu_addr(reg: u16) -> u32 {
    u32::from(reg) & 0x00FF_FFFF
}

fn write_vram_word(bus: &mut SnesBus, word_addr: u16, value: u16) {
    bus.write(ppu_addr(0x2115), 0x80);
    bus.write(ppu_addr(0x2116), (word_addr & 0xFF) as u8);
    bus.write(ppu_addr(0x2117), ((word_addr >> 8) & 0xFF) as u8);
    bus.write(ppu_addr(0x2118), (value & 0xFF) as u8);
    bus.write(ppu_addr(0x2119), ((value >> 8) & 0xFF) as u8);
}

fn write_cgram(bus: &mut SnesBus, index: u8, bgr15: u16) {
    bus.write(ppu_addr(0x2121), index);
    bus.write(ppu_addr(0x2122), (bgr15 & 0xFF) as u8);
    bus.write(ppu_addr(0x2122), ((bgr15 >> 8) & 0x7F) as u8);
}

/// Build a Mode 1 scene that drives the colour-math + window slow
/// path on every scanline.
fn setup_color_math_scene() -> SnesBus {
    let mut bus = SnesBus::new();
    bus.set_raster_snapshot(0, 225);
    bus.write(ppu_addr(0x2100), 0x0F);

    // Mode 1; BG1 tilemap @ word 0x400, BG2 tilemap @ word 0x800,
    // BG/OBJ tile data @ 0.
    bus.write(ppu_addr(0x2105), 0x01);
    bus.write(ppu_addr(0x210B), 0x00);
    bus.write(ppu_addr(0x2107), 0x04);
    bus.write(ppu_addr(0x2108), 0x08);

    // Two 4bpp tiles: tile 0 -> index 1 (BG1), tile 1 -> index 2 (BG2).
    for row in 0u16..8 {
        write_vram_word(&mut bus, row, 0x00FF); // tile 0 plane0 = 0xFF
        write_vram_word(&mut bus, 16 + row, 0xFF00); // tile 1 plane1 = 0xFF
    }
    // Tilemap fills: BG1 -> tile 0, BG2 -> tile 1 across the field.
    for slot in 0u16..0x400 {
        write_vram_word(&mut bus, 0x0400 + slot, 0x0000);
        write_vram_word(&mut bus, 0x0800 + slot, 0x0001);
    }
    write_cgram(&mut bus, 1, 0x001F); // BG1 red
    write_cgram(&mut bus, 2, 0x03E0); // BG2 green

    // Main BG1, sub BG2.
    bus.write(ppu_addr(0x212C), 0x01);
    bus.write(ppu_addr(0x212D), 0x02);

    // Window 1 = [40, 200]; BG1 clipped inside it (TMW); COL window
    // also uses window 1, main colour-window mask = "inside".
    bus.write(ppu_addr(0x2123), 0x02); // W12SEL: BG1 window 1 enable
    bus.write(ppu_addr(0x2125), 0x20); // WOBJSEL: COL window 1 enable
    bus.write(ppu_addr(0x2126), 40); // WH0 left
    bus.write(ppu_addr(0x2127), 200); // WH1 right
    bus.write(ppu_addr(0x212E), 0x01); // TMW: BG1 main window-disable

    // CGWSEL: col_main_mask = inside (1), addsub_mode = sub-screen.
    bus.write(ppu_addr(0x2130), (1 << 6) | 0x02);
    // CGADSUB: add, no halve, BG1 colour-math enable.
    bus.write(ppu_addr(0x2131), 0x01);
    bus
}

fn bench_color_math_frame(c: &mut Criterion) {
    let mut bus = setup_color_math_scene();
    let mut fb = Framebuffer::new(256, 224);
    c.bench_function("ppu_render_color_math_frame", |b| {
        b.iter(|| {
            let ppu = bus.ppu_mut();
            for line in 1u16..=224 {
                ppu.render_scanline(black_box(line), &mut fb);
            }
            black_box(fb.data[0]);
        });
    });
}

criterion_group!(benches, bench_color_math_frame);
criterion_main!(benches);
