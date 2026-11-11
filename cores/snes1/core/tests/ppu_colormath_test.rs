//! End-to-end PPU colour-math / window / brightness tests.
//!
//! Every scenario programs the PPU through the full A-bus -> B-bus ->
//! `Ppu::write_reg` chain (BG tiles + palettes, OAM, and the
//! `$2123-$2132` colour-math / window register file) and then drives
//! [`Ppu::render_scanline`] with a stand-in framebuffer, inspecting
//! the resulting RGBA8 pixels.
//!
//! Unit-level coverage of the pure helpers lives in `ppu/window.rs`,
//! `ppu/colormath.rs`, and `ppu/render.rs`; the cases here pin the
//! seams those modules can't reach, register decode, the main/sub
//! composite, the fixed-vs-sub addend select, OBJ palette exemption,
//! the colour-window regions, per-layer window clipping, and the
//! hires colour-math interleave.
//!
//! Reference: bsnes-plus's alternative PPU renderer
//! (`snes/alt/ppu/render/`).

use retro_core::cpu::Addressable;
use retro_core::Framebuffer;
use snes1_core::SnesBus;

// -------------------------------------------------------------
// Address + framebuffer helpers
// -------------------------------------------------------------

fn ppu_addr(reg: u16) -> u32 {
    u32::from(reg) & 0x00FF_FFFF
}

fn fresh_fb() -> Framebuffer {
    let mut fb = Framebuffer::new(256, 224);
    fb.data.fill(0xAB);
    fb
}

fn fresh_fb_hires() -> Framebuffer {
    let mut fb = Framebuffer::new(512, 240);
    fb.data.fill(0xAB);
    fb
}

fn pixel_at(fb: &Framebuffer, x: u32, y: u32) -> [u8; 4] {
    let stride = (fb.width * 4) as usize;
    let base = stride * y as usize + (x * 4) as usize;
    [
        fb.data[base],
        fb.data[base + 1],
        fb.data[base + 2],
        fb.data[base + 3],
    ]
}

/// Pack 5-bit channels into a 15-bit BGR word (CGRAM format).
fn bgr15(r5: u8, g5: u8, b5: u8) -> u16 {
    u16::from(r5 & 0x1F) | (u16::from(g5 & 0x1F) << 5) | (u16::from(b5 & 0x1F) << 10)
}

/// 5->8 expansion + brightness ramp, mirroring the renderer's
/// `expand_channel_with_brightness`: `c8 = (c5<<3)|(c5>>2)`, then
/// `(c8 * (brightness + 1)) >> 4`.
fn expand(c5: u8, brightness: u8) -> u8 {
    let c5 = c5 & 0x1F;
    let c8 = (c5 << 3) | (c5 >> 2);
    ((u16::from(c8) * (u16::from(brightness) + 1)) >> 4) as u8
}

/// Expected RGBA8 for `bgr15(r5,g5,b5)` at full brightness (15).
fn rgba(r5: u8, g5: u8, b5: u8) -> [u8; 4] {
    rgba_b(r5, g5, b5, 15)
}

/// Expected RGBA8 for `bgr15(r5,g5,b5)` at an arbitrary brightness.
fn rgba_b(r5: u8, g5: u8, b5: u8, brightness: u8) -> [u8; 4] {
    [
        expand(r5, brightness),
        expand(g5, brightness),
        expand(b5, brightness),
        0xFF,
    ]
}

const BLACK: [u8; 4] = [0, 0, 0, 0xFF];

// -------------------------------------------------------------
// PPU register programming helpers
// -------------------------------------------------------------

/// Bus parked in VBlank (so setup writes pass the access gate) with
/// the display enabled at `brightness`. Every BG V-scroll is parked
/// at `-1` so `scanline = 1` resolves to `screen_y = 0` (tile row 0).
fn setup(brightness: u8) -> SnesBus {
    let mut bus = SnesBus::new();
    bus.set_raster_snapshot(0, 225);
    bus.write(ppu_addr(0x2100), brightness & 0x0F);
    for bg in 0..4 {
        set_bg_voffset(&mut bus, bg, 0xFFFF);
    }
    bus
}

fn set_bgmode(bus: &mut SnesBus, mode: u8, bg3_priority: bool) {
    let mut val = mode & 0x07;
    if bg3_priority {
        val |= 0x08;
    }
    bus.write(ppu_addr(0x2105), val);
}

fn set_bg_voffset(bus: &mut SnesBus, bg_id: u8, value: u16) {
    let reg = 0x210E + u16::from(bg_id) * 2;
    bus.write(ppu_addr(reg), (value & 0xFF) as u8);
    bus.write(ppu_addr(reg), ((value >> 8) & 0xFF) as u8);
}

fn write_vram_word(bus: &mut SnesBus, word_addr: u16, value: u16) {
    bus.write(ppu_addr(0x2115), 0x80);
    bus.write(ppu_addr(0x2116), (word_addr & 0xFF) as u8);
    bus.write(ppu_addr(0x2117), ((word_addr >> 8) & 0xFF) as u8);
    bus.write(ppu_addr(0x2118), (value & 0xFF) as u8);
    bus.write(ppu_addr(0x2119), ((value >> 8) & 0xFF) as u8);
}

fn write_cgram(bus: &mut SnesBus, index: u8, color: u16) {
    bus.write(ppu_addr(0x2121), index);
    bus.write(ppu_addr(0x2122), (color & 0xFF) as u8);
    bus.write(ppu_addr(0x2122), ((color >> 8) & 0x7F) as u8);
}

fn install_standard_vram_layout(bus: &mut SnesBus) {
    bus.write(ppu_addr(0x210B), 0x00);
    bus.write(ppu_addr(0x210C), 0x00);
    bus.write(ppu_addr(0x2107), 0x04); // BG1 tilemap @ word 0x400
    bus.write(ppu_addr(0x2108), 0x08); // BG2 tilemap @ word 0x800
}

fn set_main_enable(bus: &mut SnesBus, mask: u8) {
    bus.write(ppu_addr(0x212C), mask & 0x1F);
}

fn set_sub_enable(bus: &mut SnesBus, mask: u8) {
    bus.write(ppu_addr(0x212D), mask & 0x1F);
}

/// CGWSEL ($2130): main/sub colour-window masks + add-sub mode.
fn set_cgwsel(bus: &mut SnesBus, main_mask: u8, sub_mask: u8, addsub_mode: bool) {
    let mut v = ((main_mask & 0x3) << 6) | ((sub_mask & 0x3) << 4);
    if addsub_mode {
        v |= 0x02;
    }
    bus.write(ppu_addr(0x2130), v);
}

/// CGADSUB ($2131): subtract / halve toggles + per-layer enables.
/// `bg_mask` bits 0-3 = BG1-4.
fn set_cgadsub(bus: &mut SnesBus, subtract: bool, halve: bool, back: bool, oam: bool, bg_mask: u8) {
    let mut v = bg_mask & 0x0F;
    if subtract {
        v |= 0x80;
    }
    if halve {
        v |= 0x40;
    }
    if back {
        v |= 0x20;
    }
    if oam {
        v |= 0x10;
    }
    bus.write(ppu_addr(0x2131), v);
}

/// COLDATA ($2132): set the fixed colour via three channel-selective
/// writes (R, then G, then B).
fn set_fixed_color(bus: &mut SnesBus, r5: u8, g5: u8, b5: u8) {
    bus.write(ppu_addr(0x2132), 0x20 | (r5 & 0x1F));
    bus.write(ppu_addr(0x2132), 0x40 | (g5 & 0x1F));
    bus.write(ppu_addr(0x2132), 0x80 | (b5 & 0x1F));
}

/// Encode a per-layer window-enable nibble.
fn win_nibble(w1_enable: bool, w1_invert: bool, w2_enable: bool, w2_invert: bool) -> u8 {
    let mut n = 0u8;
    if w1_invert {
        n |= 0x01;
    }
    if w1_enable {
        n |= 0x02;
    }
    if w2_invert {
        n |= 0x04;
    }
    if w2_enable {
        n |= 0x08;
    }
    n
}

/// W12SEL ($2123) low nibble = BG1 window enable bits.
fn set_window_bg1(
    bus: &mut SnesBus,
    w1_enable: bool,
    w1_invert: bool,
    w2_enable: bool,
    w2_invert: bool,
) {
    bus.write(
        ppu_addr(0x2123),
        win_nibble(w1_enable, w1_invert, w2_enable, w2_invert),
    );
}

/// WOBJSEL ($2125) high nibble = COL window enable bits.
fn set_window_col(
    bus: &mut SnesBus,
    w1_enable: bool,
    w1_invert: bool,
    w2_enable: bool,
    w2_invert: bool,
) {
    bus.write(
        ppu_addr(0x2125),
        win_nibble(w1_enable, w1_invert, w2_enable, w2_invert) << 4,
    );
}

/// WH0-WH3 ($2126-$2129): window 1 + window 2 left/right edges.
fn set_window_edges(bus: &mut SnesBus, w1l: u8, w1r: u8, w2l: u8, w2r: u8) {
    bus.write(ppu_addr(0x2126), w1l);
    bus.write(ppu_addr(0x2127), w1r);
    bus.write(ppu_addr(0x2128), w2l);
    bus.write(ppu_addr(0x2129), w2r);
}

/// WBGLOG ($212A) BG1 combine (bits 0-1).
fn set_window_combine_bg1(bus: &mut SnesBus, combine: u8) {
    bus.write(ppu_addr(0x212A), combine & 0x3);
}

/// TMW ($212E): main-screen per-layer window-disable mask.
fn set_tmw(bus: &mut SnesBus, mask: u8) {
    bus.write(ppu_addr(0x212E), mask & 0x1F);
}

// -- Sprite (OBJ) helpers --------------------------------------

fn set_obsel(bus: &mut SnesBus, size_sel: u8, name_sel: u8, base: u8) {
    let v = ((size_sel & 0x07) << 5) | ((name_sel & 0x03) << 3) | (base & 0x07);
    bus.write(ppu_addr(0x2101), v);
}

fn set_oam_addr(bus: &mut SnesBus, byte_addr: u16) {
    bus.write(ppu_addr(0x2103), ((byte_addr >> 9) & 0x01) as u8);
    bus.write(ppu_addr(0x2102), ((byte_addr >> 1) & 0xFF) as u8);
}

fn write_oam_sprite(bus: &mut SnesBus, idx: u16, x: u8, y: u8, character: u8, attr: u8) {
    set_oam_addr(bus, idx * 4);
    bus.write(ppu_addr(0x2104), x);
    bus.write(ppu_addr(0x2104), y);
    bus.write(ppu_addr(0x2104), character);
    bus.write(ppu_addr(0x2104), attr);
}

fn park_all_sprites(bus: &mut SnesBus) {
    set_oam_addr(bus, 0);
    for _ in 0..128 {
        bus.write(ppu_addr(0x2104), 0x00);
        bus.write(ppu_addr(0x2104), 0xF0); // Y = 240, off-screen
        bus.write(ppu_addr(0x2104), 0x00);
        bus.write(ppu_addr(0x2104), 0x00);
    }
}

/// Solid 4bpp OBJ tile (all 64 pixels = `index`) at VRAM tile slot.
fn write_solid_obj_tile(bus: &mut SnesBus, tile: u16, index: u8) {
    let base = tile.wrapping_mul(16);
    let plane = |bit: u8| -> u16 {
        if index & bit != 0 {
            0x00FF
        } else {
            0x0000
        }
    };
    for row in 0..8u16 {
        write_vram_word(bus, base + row, plane(1) | (plane(2) << 8));
        write_vram_word(bus, base + 8 + row, plane(4) | (plane(8) << 8));
    }
}

// -- BG fill helpers -------------------------------------------

/// Mode 1, BG1 on main, tile 0 = colour index 1 = `color` everywhere.
fn install_main_bg1(bus: &mut SnesBus, color: u16) {
    set_bgmode(bus, 1, false);
    install_standard_vram_layout(bus);
    set_main_enable(bus, 0x01);
    write_vram_word(bus, 0x0000, 0x00FF); // tile 0 row 0 -> index 1
    write_vram_word(bus, 0x0400, 0x0000); // BG1 tilemap (0,0) = tile 0
    write_cgram(bus, 1, color);
}

/// Add BG2 on the sub-screen: tile 1 = colour index 2 = `color`.
fn install_sub_bg2(bus: &mut SnesBus, color: u16) {
    set_sub_enable(bus, 0x02);
    write_vram_word(bus, 0x0010, 0xFF00); // tile 1 row 0 -> index 2
    write_vram_word(bus, 0x0800, 0x0001); // BG2 tilemap (0,0) = tile 1
    write_cgram(bus, 2, color);
}

// ===============================================================
// Colour-math arithmetic (fixed-colour addend, addsub_mode = 0)
// ===============================================================

#[test]
fn fixed_color_add_clips_at_31() {
    // 20 + 20 = 40 -> clip to 31 on red.
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(20, 0, 0));
    set_cgwsel(&mut bus, 0, 0, false);
    set_fixed_color(&mut bus, 20, 0, 0);
    set_cgadsub(&mut bus, false, false, false, false, 0x01);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), rgba(31, 0, 0), "add saturates at 31");
}

#[test]
fn fixed_color_add_half_max_is_31() {
    // (31 + 31) / 2 = 31.
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(31, 31, 31));
    set_cgwsel(&mut bus, 0, 0, false);
    set_fixed_color(&mut bus, 31, 31, 31);
    set_cgadsub(&mut bus, false, true, false, false, 0x01);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        rgba(31, 31, 31),
        "max + max halved = max"
    );
}

#[test]
fn fixed_color_add_half_is_average() {
    // (20 + 10) / 2 = 15 on red.
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(20, 0, 0));
    set_cgwsel(&mut bus, 0, 0, false);
    set_fixed_color(&mut bus, 10, 0, 0);
    set_cgadsub(&mut bus, false, true, false, false, 0x01);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), rgba(15, 0, 0), "halved sum");
}

#[test]
fn fixed_color_subtract_clamps_to_zero() {
    // 5 - 10 = 0 (no wrap) -> black.
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(5, 0, 0));
    set_cgwsel(&mut bus, 0, 0, false);
    set_fixed_color(&mut bus, 10, 0, 0);
    set_cgadsub(&mut bus, true, false, false, false, 0x01);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), BLACK, "subtract clamps at 0");
}

#[test]
fn fixed_color_subtract_basic() {
    // 20 - 6 = 14 on red.
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(20, 0, 0));
    set_cgwsel(&mut bus, 0, 0, false);
    set_fixed_color(&mut bus, 6, 0, 0);
    set_cgadsub(&mut bus, true, false, false, false, 0x01);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), rgba(14, 0, 0));
}

#[test]
fn fixed_color_subtract_half() {
    // (20 - 6) / 2 = 7 on red.
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(20, 0, 0));
    set_cgwsel(&mut bus, 0, 0, false);
    set_fixed_color(&mut bus, 6, 0, 0);
    set_cgadsub(&mut bus, true, true, false, false, 0x01);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), rgba(7, 0, 0));
}

// ===============================================================
// Sub-screen addend (addsub_mode = 1)
// ===============================================================

#[test]
fn subscreen_add_combines_main_and_sub() {
    // main BG1 (10,0,0) + sub BG2 (0,10,0) = (10,10,0).
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(10, 0, 0));
    install_sub_bg2(&mut bus, bgr15(0, 10, 0));
    set_cgwsel(&mut bus, 0, 0, true); // addsub_mode = sub-screen
    set_cgadsub(&mut bus, false, false, false, false, 0x01);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), rgba(10, 10, 0), "main + sub");
}

#[test]
fn subscreen_subtract_per_channel() {
    // main BG1 (20,10,0) - sub BG2 (5,3,0) = (15,7,0).
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(20, 10, 0));
    install_sub_bg2(&mut bus, bgr15(5, 3, 0));
    set_cgwsel(&mut bus, 0, 0, true);
    set_cgadsub(&mut bus, true, false, false, false, 0x01);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), rgba(15, 7, 0), "main - sub");
}

#[test]
fn subscreen_add_backdrop_addend_when_sub_empty() {
    // addsub_mode with no sub layer enabled -> the sub addend is the
    // sub backdrop (fixed colour in normal modes). main (10,0,0) +
    // fixed (5,0,0) = (15,0,0).
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(10, 0, 0));
    set_cgwsel(&mut bus, 0, 0, true);
    set_fixed_color(&mut bus, 5, 0, 0);
    set_cgadsub(&mut bus, false, false, false, false, 0x01);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    // `addend_is_backdrop` (which would suppress halve, but halve is
    // off here): a plain add against fixed.
    assert_eq!(pixel_at(&fb, 0, 0), rgba(15, 0, 0));
}

// ===============================================================
// OBJ colour-math exemption (palette 0-3 vs 4-7)
// ===============================================================

/// Program sprite 0 at the origin in `palette` with colour index 1 =
/// `(8,0,0)`, then render row 0 (scanline 1).
fn render_obj_with_palette(palette: u8) -> [u8; 4] {
    let mut bus = setup(15);
    park_all_sprites(&mut bus);
    set_obsel(&mut bus, 0, 0, 0);
    write_solid_obj_tile(&mut bus, 0, 1);
    let cgram_idx = 128 + palette * 16 + 1;
    write_cgram(&mut bus, cgram_idx, bgr15(8, 0, 0));
    let attr = (palette & 0x07) << 1; // priority 0
    write_oam_sprite(&mut bus, 0, 0, 0, 0, attr);
    set_main_enable(&mut bus, 0x10); // OBJ main

    // Colour math: OBJ enable, add the fixed colour (4,0,0).
    set_cgwsel(&mut bus, 0, 0, false);
    set_fixed_color(&mut bus, 4, 0, 0);
    set_cgadsub(&mut bus, false, false, false, true, 0x00);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    pixel_at(&fb, 0, 0)
}

#[test]
fn obj_palette_0_3_is_color_math_exempt() {
    // Palette 2 (CGRAM index < 192) -> no colour math: shows (8,0,0).
    assert_eq!(render_obj_with_palette(2), rgba(8, 0, 0));
}

#[test]
fn obj_palette_4_7_takes_color_math() {
    // Palette 5 (CGRAM index >= 192) -> add fixed: (8+4,0,0)=(12,0,0).
    assert_eq!(render_obj_with_palette(5), rgba(12, 0, 0));
}

// ===============================================================
// Backdrop colour math
// ===============================================================

#[test]
fn backdrop_color_math_adds_fixed() {
    // No layer enabled -> backdrop (CGRAM[0]) shows; back-enable adds
    // the fixed colour. (5,0,0) + (5,0,0) = (10,0,0).
    let mut bus = setup(15);
    set_bgmode(&mut bus, 1, false);
    write_cgram(&mut bus, 0, bgr15(5, 0, 0));
    set_cgwsel(&mut bus, 0, 0, false);
    set_fixed_color(&mut bus, 5, 0, 0);
    set_cgadsub(&mut bus, false, false, true, false, 0x00);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), rgba(10, 0, 0));
}

#[test]
fn backdrop_without_back_enable_is_unchanged() {
    // Same setup but back-enable off -> backdrop stays (5,0,0).
    let mut bus = setup(15);
    set_bgmode(&mut bus, 1, false);
    write_cgram(&mut bus, 0, bgr15(5, 0, 0));
    set_cgwsel(&mut bus, 0, 0, false);
    set_fixed_color(&mut bus, 5, 0, 0);
    // back-enable off, but bg1 enable on so colour math is "possible"
    // and the sub plane / `resolve_pixel` pass still runs.
    set_cgadsub(&mut bus, false, false, false, false, 0x01);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), rgba(5, 0, 0));
}

// ===============================================================
// Windows, per-layer clip (TMW) + colour-window (col_main_mask)
// ===============================================================

#[test]
fn window_clip_removes_main_bg_inside_region() {
    // BG1 red everywhere; window 1 = [0,7]; TMW clips BG1 inside it.
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(31, 0, 0));
    set_window_bg1(&mut bus, true, false, false, false);
    set_window_edges(&mut bus, 0, 7, 0, 0);
    set_tmw(&mut bus, 0x01); // BG1 main window-disable

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), BLACK, "inside window -> BG1 clipped");
    assert_eq!(pixel_at(&fb, 7, 0), BLACK, "right edge inclusive");
    assert_eq!(
        pixel_at(&fb, 8, 0),
        rgba(31, 0, 0),
        "outside window -> BG1 shows"
    );
}

#[test]
fn window1_region_matches_inclusive_edges() {
    // WH0=80, WH1=160, X=100 inside.
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(31, 0, 0));
    set_window_bg1(&mut bus, true, false, false, false);
    set_window_edges(&mut bus, 80, 160, 0, 0);
    set_tmw(&mut bus, 0x01);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 100, 0),
        BLACK,
        "X=100 inside [80,160] -> clipped"
    );
    assert_eq!(pixel_at(&fb, 79, 0), rgba(31, 0, 0), "just left of window");
    assert_eq!(
        pixel_at(&fb, 161, 0),
        rgba(31, 0, 0),
        "just right of window"
    );
}

#[test]
fn window_invert_flips_clip_region() {
    // Inverted window 1 = [0,7] -> clip everything *outside* [0,7].
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(31, 0, 0));
    set_window_bg1(&mut bus, true, true, false, false);
    set_window_edges(&mut bus, 0, 7, 0, 0);
    set_tmw(&mut bus, 0x01);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        rgba(31, 0, 0),
        "inside stays (inverted)"
    );
    assert_eq!(pixel_at(&fb, 8, 0), BLACK, "outside is clipped (inverted)");
}

#[test]
fn window_combine_or_clips_union() {
    // W1=[0,7] OR W2=[100,107] -> both bands clipped.
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(31, 0, 0));
    set_window_bg1(&mut bus, true, false, true, false);
    set_window_edges(&mut bus, 0, 7, 100, 107);
    set_window_combine_bg1(&mut bus, 0); // OR
    set_tmw(&mut bus, 0x01);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), BLACK, "in W1");
    assert_eq!(pixel_at(&fb, 103, 0), BLACK, "in W2");
    assert_eq!(pixel_at(&fb, 50, 0), rgba(31, 0, 0), "in neither");
}

#[test]
fn window_combine_and_clips_intersection() {
    // W1=[0,120] AND W2=[100,200] -> clip only [100,120].
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(31, 0, 0));
    set_window_bg1(&mut bus, true, false, true, false);
    set_window_edges(&mut bus, 0, 120, 100, 200);
    set_window_combine_bg1(&mut bus, 1); // AND
    set_tmw(&mut bus, 0x01);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 110, 0), BLACK, "in both -> clipped");
    assert_eq!(pixel_at(&fb, 50, 0), rgba(31, 0, 0), "W1 only");
    assert_eq!(pixel_at(&fb, 150, 0), rgba(31, 0, 0), "W2 only");
}

#[test]
fn window_combine_xor_clips_symmetric_difference() {
    // W1=[0,120] XOR W2=[100,200] -> clip [0,99] and [121,200].
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(31, 0, 0));
    set_window_bg1(&mut bus, true, false, true, false);
    set_window_edges(&mut bus, 0, 120, 100, 200);
    set_window_combine_bg1(&mut bus, 2); // XOR
    set_tmw(&mut bus, 0x01);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 50, 0), BLACK, "W1 only -> clipped");
    assert_eq!(pixel_at(&fb, 110, 0), rgba(31, 0, 0), "overlap -> kept");
    assert_eq!(pixel_at(&fb, 150, 0), BLACK, "W2 only -> clipped");
}

#[test]
fn window_combine_xnor_clips_complement_of_xor() {
    // W1=[0,120] XNOR W2=[100,200] -> clip the overlap [100,120] (and
    // the neither region), keep the W1-only / W2-only bands.
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(31, 0, 0));
    set_window_bg1(&mut bus, true, false, true, false);
    set_window_edges(&mut bus, 0, 120, 100, 200);
    set_window_combine_bg1(&mut bus, 3); // XNOR
    set_tmw(&mut bus, 0x01);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 50, 0), rgba(31, 0, 0), "W1 only -> kept");
    assert_eq!(pixel_at(&fb, 110, 0), BLACK, "overlap -> clipped");
    assert_eq!(pixel_at(&fb, 150, 0), rgba(31, 0, 0), "W2 only -> kept");
}

#[test]
fn main_screen_black_outside_color_window() {
    // CGWSEL col_main_mask = 1 ("inside only"); COL window 1 = [0,7].
    // Inside the window the main screen shows; outside it is forced
    // black.
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(31, 0, 0));
    set_window_col(&mut bus, true, false, false, false);
    set_window_edges(&mut bus, 0, 7, 0, 0);
    set_cgwsel(&mut bus, 1, 0, false); // col_main_mask = inside

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        rgba(31, 0, 0),
        "inside col window -> shown"
    );
    assert_eq!(
        pixel_at(&fb, 8, 0),
        BLACK,
        "outside col window -> main black"
    );
}

// ===============================================================
// Master brightness + forced blank
// ===============================================================

#[test]
fn master_brightness_zero_is_black() {
    // Brightness 0 with display enabled -> black.
    let mut bus = setup(0);
    install_main_bg1(&mut bus, bgr15(31, 0, 0));

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), BLACK);
}

#[test]
fn forced_blank_overrides_everything() {
    // $2100 = 0x80 (forced blank) even at brightness 15 -> black.
    let mut bus = setup(15);
    install_main_bg1(&mut bus, bgr15(31, 0, 0));
    bus.write(ppu_addr(0x2100), 0x80 | 0x0F);

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), BLACK);
}

#[test]
fn master_brightness_half_dims_color() {
    // Brightness 7 dims (31,0,0): r = (255 * 8) >> 4 = 127.
    let mut bus = setup(7);
    install_main_bg1(&mut bus, bgr15(31, 0, 0));

    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), rgba_b(31, 0, 0, 7));
    assert_eq!(pixel_at(&fb, 0, 0)[0], 127, "half-brightness red");
}

// ===============================================================
// Hires (Mode 5) colour math
// ===============================================================

#[test]
fn hires_mode5_color_math_on_main_odd_columns() {
    // Mode 5: odd display columns are the main screen (normal pixel),
    // even columns the sub screen (swap pixel). With BG1 on main only
    // and a fixed-colour add, the odd columns gain the fixed colour
    // and the even columns stay the (empty) sub backdrop.
    let mut bus = setup(15);
    set_bgmode(&mut bus, 5, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);
    write_vram_word(&mut bus, 0x0000, 0x00FF); // BG1 tile 0 row 0 -> index 1
    write_vram_word(&mut bus, 0x0400, 0x0000);
    write_cgram(&mut bus, 1, bgr15(8, 0, 0));
    write_cgram(&mut bus, 0, bgr15(0, 0, 0)); // backdrop = black

    set_cgwsel(&mut bus, 0, 0, false);
    set_fixed_color(&mut bus, 4, 0, 0);
    set_cgadsub(&mut bus, false, false, false, false, 0x01);

    let mut fb = fresh_fb_hires();
    bus.ppu_mut().render_scanline(1, &mut fb);
    // Odd column 1 = main pixel 0 = (8+4,0,0) = (12,0,0).
    assert_eq!(
        pixel_at(&fb, 1, 0),
        rgba(12, 0, 0),
        "main odd column + fixed"
    );
    // Even column 0 = sub backdrop (CGRAM[0] in hires) -> black.
    assert_eq!(pixel_at(&fb, 0, 0), BLACK, "sub even column backdrop");
}

#[test]
fn hires_mode5_baseline_without_color_math_unchanged() {
    // Same scene, no colour math -> odd column shows the raw BG1
    // colour.
    let mut bus = setup(15);
    set_bgmode(&mut bus, 5, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);
    write_vram_word(&mut bus, 0x0000, 0x00FF);
    write_vram_word(&mut bus, 0x0400, 0x0000);
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));

    let mut fb = fresh_fb_hires();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 1, 0), rgba(31, 0, 0), "odd column = BG1 red");
    assert_eq!(pixel_at(&fb, 0, 0), BLACK, "even column = sub backdrop");
}
