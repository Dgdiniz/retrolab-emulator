//! End-to-end PPU rendering tests for SNES Modes 0-7.
//!
//! Every scenario programs the PPU through the full A-bus ->
//! B-bus -> `Ppu::write_reg` chain (so tilemaps, character data,
//! palette entries, and control registers go in via
//! `bus.write(ppu_addr(0x21XX), ...)` rather than directly poking
//! `Ppu` state), then invokes the renderer with a stand-in
//! framebuffer and inspects the resulting RGBA8 pixels.
//!
//! Unit-level coverage of the decode helpers lives in
//! `ppu/tile.rs` and `ppu/render.rs`; the tests here pin the
//! seams that those modules can't reach on their own, register
//! writes, CGRAM packing, priority order across BGs, mosaic
//! replication across scanlines, and the framebuffer mapping.
//!
//! ## VRAM layout convention
//!
//! All tests share a fixed VRAM layout to keep the per-test setup
//! short:
//!
//! | Region     | Byte addr | Word addr  | `$2107-A` value |
//! |------------|-----------|------------|-----------------|
//! | Tile data  | 0x0000    | 0x0000     | (`$210B/C` = 0) |
//! | BG1 tmap   | 0x0800    | 0x0400     | `$2107 = 0x04`  |
//! | BG2 tmap   | 0x1000    | 0x0800     | `$2108 = 0x08`  |
//! | BG3 tmap   | 0x1800    | 0x0C00     | `$2109 = 0x0C`  |
//! | BG4 tmap   | 0x2000    | 0x1000     | `$210A = 0x10`  |
//!
//! `fresh_test_setup` parks every BG's vertical scroll at
//! `0xFFFF` (= `-1` mod 65536) so `scanline = 1` wraps to
//! `screen_y = 0` (and tile-row 0). Mode 2/4 tests that depend on
//! the BG3 offset-per-tile lookup landing at BG3 tile row 0 reset
//! `bg3_voffset` to 0 explicitly via `set_bg_voffset(bus, 2, 0)`.

use retro_core::cpu::Addressable;
use retro_core::Framebuffer;
use snes1_core::SnesBus;

// -------------------------------------------------------------
// Address + bus helpers
// -------------------------------------------------------------

/// PPU register address in bank `$00`.
fn ppu_addr(reg: u16) -> u32 {
    u32::from(reg) & 0x00FF_FFFF
}

/// Helper for a 256x224 framebuffer pre-filled with sentinel
/// bytes so untouched rows show up as `0xAB` instead of a happy
/// coincidence with the rendered black pixel.
fn fresh_fb() -> Framebuffer {
    let mut fb = Framebuffer::new(256, 224);
    fb.data.fill(0xAB);
    fb
}

/// Hires-sized framebuffer (512x480) for Mode 5/6 tests. 480 rows
/// cover the worst case (overscan + interlace = 240x2), matching the
/// emulator's real backing buffer. Pre-fills with `0xAB` like
/// [`fresh_fb`].
fn fresh_fb_hires() -> Framebuffer {
    let mut fb = Framebuffer::new(512, 480);
    fb.data.fill(0xAB);
    fb
}

/// RGBA pixel at `(x, y)` in a 512-wide framebuffer (host
/// coordinates). Same math as [`pixel_at`] but kept under its own
/// name so hires tests are easy to read.
fn pixel_at_512(fb: &Framebuffer, x: u32, y: u32) -> [u8; 4] {
    pixel_at(fb, x, y)
}

/// Standard test setup. Returns a bus where:
///
/// * The raster snapshot is parked at `v_scanline = 225` (vblank)
///   so VRAM / CGRAM / OAM writes that follow `$2100` clearing
///   forced-blank still pass the active-scan access gates.
/// * `$2100` is loaded so the display is enabled at `brightness`.
/// * Every BG's V scroll is pre-set to `0xFFFF`, that's `-1` mod
///   65536, which makes `scanline = 1` resolve to `screen_y = 0`
///   (and therefore tile row 0). Tests can write tile data at
///   `vram[0]` and the top-left tilemap entry at the slot
///   `vram[(screen_addr_byte >> 1) + 0]`, matching the natural
///   "I'm drawing the top of a tile" mental model.
///
/// Tests that need forced-blank or brightness-0 paths overrides
/// `$2100` after calling the helper.
fn fresh_test_setup(brightness: u8) -> (SnesBus, Framebuffer) {
    let mut bus = SnesBus::new();
    bus.set_raster_snapshot(0, 225);
    bus.write(ppu_addr(0x2100), brightness & 0x0F);
    for bg in 0..4 {
        set_bg_voffset(&mut bus, bg, 0xFFFF);
    }
    (bus, fresh_fb())
}

/// RGBA pixel at `(x, y)` in the framebuffer (host coordinates).
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

// -------------------------------------------------------------
// PPU register / state helpers
// -------------------------------------------------------------

/// `$2105` BGMODE: low 3 bits = mode, bit 3 = BG3 priority.
fn set_bgmode(bus: &mut SnesBus, mode: u8, bg3_priority: bool) {
    let mut val = mode & 0x07;
    if bg3_priority {
        val |= 0x08;
    }
    bus.write(ppu_addr(0x2105), val);
}

/// Set the BG `bg_id` (0..3) horizontal scroll via BGxHOFS
/// (`$210D + 2*bg`). Two writes are required (low byte then high)
/// because PPU1 uses a chained latch.
fn set_bg_hoffset(bus: &mut SnesBus, bg_id: u8, value: u16) {
    let reg = 0x210D + u16::from(bg_id) * 2;
    let lo = (value & 0xFF) as u8;
    let hi = ((value >> 8) & 0xFF) as u8;
    bus.write(ppu_addr(reg), lo);
    bus.write(ppu_addr(reg), hi);
}

/// Set BG `bg_id` vertical scroll. BGxVOFS sits at `$210E + 2*bg`.
fn set_bg_voffset(bus: &mut SnesBus, bg_id: u8, value: u16) {
    let reg = 0x210E + u16::from(bg_id) * 2;
    let lo = (value & 0xFF) as u8;
    let hi = ((value >> 8) & 0xFF) as u8;
    bus.write(ppu_addr(reg), lo);
    bus.write(ppu_addr(reg), hi);
}

/// Enable BG main-screen drawing via TM ($212C). Bits 0-3 = BG1-4,
/// bit 4 = OBJ.
fn set_main_enable(bus: &mut SnesBus, mask: u8) {
    bus.write(ppu_addr(0x212C), mask & 0x1F);
}

/// Enable BG sub-screen drawing via TS ($212D). Bits 0-3 = BG1-4,
/// bit 4 = OBJ. Used by Mode 5/6 hires tests to populate the
/// sub-screen plane that gets interleaved into the even columns
/// of the final 512-pixel output.
fn set_sub_enable(bus: &mut SnesBus, mask: u8) {
    bus.write(ppu_addr(0x212D), mask & 0x1F);
}

/// Write the SETINI register ($2133). Bit 2 = overscan (240
/// lines visible), bit 3 = pseudo-hires, bit 0 = interlace.
fn set_setini(bus: &mut SnesBus, value: u8) {
    bus.write(ppu_addr(0x2133), value);
}

/// Write one VRAM word at `word_addr` (16-bit address). Uses
/// auto-increment step 1 + high-side trigger.
fn write_vram_word(bus: &mut SnesBus, word_addr: u16, value: u16) {
    bus.write(ppu_addr(0x2115), 0x80);
    bus.write(ppu_addr(0x2116), (word_addr & 0xFF) as u8);
    bus.write(ppu_addr(0x2117), ((word_addr >> 8) & 0xFF) as u8);
    bus.write(ppu_addr(0x2118), (value & 0xFF) as u8);
    bus.write(ppu_addr(0x2119), ((value >> 8) & 0xFF) as u8);
}

/// Write one CGRAM 15-bit BGR color at `color_index` (0..255).
fn write_cgram(bus: &mut SnesBus, color_index: u8, bgr15: u16) {
    bus.write(ppu_addr(0x2121), color_index);
    bus.write(ppu_addr(0x2122), (bgr15 & 0xFF) as u8);
    bus.write(ppu_addr(0x2122), ((bgr15 >> 8) & 0x7F) as u8);
}

/// Set MOSAIC ($2106). `size` is 0..15 (encoded as `size + 1`
/// pixels by the hardware); `bg_mask` bits 0-3 enable BG1-4 mosaic.
fn set_mosaic(bus: &mut SnesBus, size: u8, bg_mask: u8) {
    let val = ((size & 0x0F) << 4) | (bg_mask & 0x0F);
    bus.write(ppu_addr(0x2106), val);
}

/// Set CGWSEL ($2130). Bit 0 = direct colour.
fn set_cgwsel(bus: &mut SnesBus, direct_color: bool) {
    let val: u8 = if direct_color { 0x01 } else { 0x00 };
    bus.write(ppu_addr(0x2130), val);
}

/// Convenience: pack 5-bit R/G/B into a 15-bit BGR word.
fn bgr15(r5: u8, g5: u8, b5: u8) -> u16 {
    u16::from(r5 & 0x1F) | (u16::from(g5 & 0x1F) << 5) | (u16::from(b5 & 0x1F) << 10)
}

/// Program the standard test layout: BG tile data at byte 0, BG1
/// tilemap at byte 0x0800, BG2 tilemap at byte 0x1000, etc. Tests
/// call this (or a subset) to avoid repeating the same `$2107-A`
/// writes everywhere.
fn install_standard_vram_layout(bus: &mut SnesBus) {
    bus.write(ppu_addr(0x210B), 0x00); // BG1/2 tile data at 0
    bus.write(ppu_addr(0x210C), 0x00); // BG3/4 tile data at 0
    bus.write(ppu_addr(0x2107), 0x04); // BG1 tilemap at byte 0x0800
    bus.write(ppu_addr(0x2108), 0x08); // BG2 tilemap at byte 0x1000
    bus.write(ppu_addr(0x2109), 0x0C); // BG3 tilemap at byte 0x1800
    bus.write(ppu_addr(0x210A), 0x10); // BG4 tilemap at byte 0x2000
}

// -------------------------------------------------------------
// Mode 7 helpers
// -------------------------------------------------------------

/// Write a 16-bit value to a Mode 7 matrix/centre register through
/// its shared 8-bit latch (`$211B-$2120`): low byte first, then
/// high. Two writes are self-contained regardless of the leftover
/// latch byte from a previous register.
fn write_m7_pair(bus: &mut SnesBus, reg: u16, value: u16) {
    bus.write(ppu_addr(reg), (value & 0xFF) as u8);
    bus.write(ppu_addr(reg), ((value >> 8) & 0xFF) as u8);
}

/// Program the Mode 7 matrix coefficients A-D (`$211B-$211E`).
fn set_m7_matrix(bus: &mut SnesBus, a: u16, b: u16, c: u16, d: u16) {
    write_m7_pair(bus, 0x211B, a);
    write_m7_pair(bus, 0x211C, b);
    write_m7_pair(bus, 0x211D, c);
    write_m7_pair(bus, 0x211E, d);
}

/// Program the Mode 7 centre (M7X/M7Y, `$211F`/`$2120`).
fn set_m7_center(bus: &mut SnesBus, x: u16, y: u16) {
    write_m7_pair(bus, 0x211F, x);
    write_m7_pair(bus, 0x2120, y);
}

/// Program the Mode 7 H/V scroll offsets. These ride the BG1
/// scroll ports (`$210D`/`$210E`); the same writes feed the
/// `mode7_hoffset`/`voffset` latch chain.
fn set_m7_offset(bus: &mut SnesBus, h: u16, v: u16) {
    set_bg_hoffset(bus, 0, h);
    set_bg_voffset(bus, 0, v);
}

/// Write M7SEL (`$211A`): bits 6-7 screen-over (repeat), bit 1
/// vflip, bit 0 hflip.
fn set_m7sel(bus: &mut SnesBus, value: u8) {
    bus.write(ppu_addr(0x211A), value);
}

/// Identity transform: A = D = 1.0 (`0x0100` in 8.8 fixed point),
/// B = C = 0, centre + offset at the origin. Leaves screen-over at
/// wrap (0) and both flips off.
fn set_m7_identity(bus: &mut SnesBus) {
    set_m7_matrix(bus, 0x0100, 0x0000, 0x0000, 0x0100);
    set_m7_center(bus, 0, 0);
    set_m7_offset(bus, 0, 0);
}

/// Enable / disable EXTBG via SETINI (`$2133` bit 6). Writes the
/// whole register, so callers that also want overscan must OR it in
/// themselves.
fn set_extbg(bus: &mut SnesBus, on: bool) {
    let v: u8 = if on { 0x40 } else { 0x00 };
    bus.write(ppu_addr(0x2133), v);
}

/// Mode 7 base scene: vblank snapshot + brightness 15 (via
/// [`fresh_test_setup`]), Mode 7 selected, identity transform, BG1
/// main-screen enabled. The identity reset also zeroes the Mode 7
/// offset that `fresh_test_setup` parked at `0xFFFF` through the
/// shared BG1-scroll latch.
fn mode7_setup() -> (SnesBus, Framebuffer) {
    let (mut bus, fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 7, false);
    set_m7_identity(&mut bus);
    set_main_enable(&mut bus, 0x01);
    (bus, fb)
}

/// Lay out a Mode 7 plane that distinguishes all three screen-over
/// modes when screen column 0 maps just outside the plane:
///   * tile 0's row 1 is uniformly colour index 1 (red),
///   * tile 1 carries colour index 2 (green) at the wrap landing
///     pixel, reached only when the wrap path resolves world tile
///     column 127 -> tile 1.
///
/// Rendering happens on scanline 1, which samples world row 1
/// (the PPU's line counter is 1 on the first visible line), so the
/// character data sits in tile
/// row 1 (vram[8..16]), not row 0. With `mode7_hoffset = -1`,
/// screen x=0 lands at px=-1 (outside) and screen x=1 lands at px=0
/// (inside -> red).
fn install_mode7_screen_over_layout(bus: &mut SnesBus) {
    // Tile 0 row 1 (cy = 1): every character pixel = index 1 (high
    // byte 0x01). word 0's low byte stays 0 -> tilemap (0,0) = tile 0.
    for cx in 0u16..8 {
        write_vram_word(bus, 8 + cx, 0x0100);
    }
    // Tile 1 character (cx=7, cy=1) = index 2, the pixel the wrap
    // path lands on (px wraps to 1023 -> cx = 7), at vram[64+8+7].
    write_vram_word(bus, 64 + 8 + 7, 0x0200);
    // Tilemap (127, 0) = tile 1 so the wrap destination resolves to
    // tile 1, not tile 0.
    write_vram_word(bus, 127, 0x0001);
    write_cgram(bus, 1, bgr15(31, 0, 0)); // index 1 -> red (tile 0)
    write_cgram(bus, 2, bgr15(0, 31, 0)); // index 2 -> green (tile 1)
    set_m7_offset(bus, 0x1FFF, 0); // mode7_hoffset = -1 -> x=0 outside
}

// -------------------------------------------------------------
// Diagnostics, confirm the helpers actually land bytes
// -------------------------------------------------------------

#[test]
fn diag_cgram_helper_lands_color_word() {
    let (mut bus, _fb) = fresh_test_setup(15);
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));
    assert_eq!(
        bus.ppu().cgram()[1],
        0x001F,
        "CGRAM[1] should hold the red BGR15 we wrote"
    );
}

#[test]
fn diag_vram_helper_lands_word() {
    let (mut bus, _fb) = fresh_test_setup(15);
    write_vram_word(&mut bus, 0x0123, 0xABCD);
    assert_eq!(bus.ppu().vram()[0x0123], 0xABCD);
}

#[test]
fn diag_voffset_default_is_minus_one() {
    let (bus, _fb) = fresh_test_setup(15);
    let ppu = bus.ppu();
    for bg in 0..4 {
        assert_eq!(
            ppu.bg_voffset(bg),
            0xFFFF,
            "BG{} voffset should be -1u16 after fresh_test_setup",
            bg + 1
        );
    }
}

// -------------------------------------------------------------
// Bedrock: forced blank, brightness, framebuffer mapping
// -------------------------------------------------------------

#[test]
fn forced_blank_writes_pure_black_with_alpha_full() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    // Re-assert forced blank to verify the renderer's
    // display_disable short-circuit (the setup helper had cleared
    // bit 7 of `$2100`).
    bus.write(ppu_addr(0x2100), 0x80);
    bus.ppu_mut().render_scanline(1, &mut fb);
    let p = pixel_at(&fb, 0, 0);
    assert_eq!(p, [0, 0, 0, 0xFF], "forced blank line is black + alpha");
}

#[test]
fn brightness_zero_path_blanks_same_as_forced_blank() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    // Brightness 0 + display enabled is the spec'd "black even
    // though !blank" path; the renderer short-circuits before any
    // tile fetch.
    bus.write(ppu_addr(0x2100), 0x00);
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 7, 0), [0, 0, 0, 0xFF]);
}

#[test]
fn scanline_zero_is_skipped_renderer_does_not_touch_fb() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    bus.ppu_mut().render_scanline(0, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), [0xAB, 0xAB, 0xAB, 0xAB]);
}

#[test]
fn scanline_past_vblank_is_skipped() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    bus.ppu_mut().render_scanline(225, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 223), [0xAB, 0xAB, 0xAB, 0xAB]);
}

#[test]
fn scanline_one_maps_to_host_row_zero() {
    // Mode 1 with BG1 4bpp, single 8x8 tile at top-left, all
    // pixels = palette 0 colour 1 -> CGRAM[1].
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);

    // Tile 0 row 0 4bpp: plane0=0xFF, others 0 -> all pixels = 1.
    write_vram_word(&mut bus, 0x0000, 0x00FF);
    // BG1 tilemap entry at (0, 0) = vram[word 0x400]: tile 0.
    write_vram_word(&mut bus, 0x0400, 0x0000);
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), [0xFF, 0, 0, 0xFF]);
}

// -------------------------------------------------------------
// Mode 1, most common (SMW, DKC, ...)
// -------------------------------------------------------------

#[test]
fn mode1_bg1_4bpp_renders_tile_with_palette() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);

    // Tile 0 row 0: plane0=0b10000000, plane1=0b01000000 ->
    // col 0 = 1, col 1 = 2, rest 0.
    write_vram_word(&mut bus, 0x0000, 0x4080);
    // Tilemap entry: tile 0, palette 3 -> CGRAM base = 3*16 = 48.
    write_vram_word(&mut bus, 0x0400, 3 << 10);
    write_cgram(&mut bus, 49, bgr15(0, 31, 0));
    write_cgram(&mut bus, 50, bgr15(0, 0, 31));
    write_cgram(&mut bus, 0, bgr15(31, 31, 31));

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), [0, 0xFF, 0, 0xFF], "col 0 = green");
    assert_eq!(pixel_at(&fb, 1, 0), [0, 0, 0xFF, 0xFF], "col 1 = blue");
    assert_eq!(
        pixel_at(&fb, 2, 0),
        [0xFF, 0xFF, 0xFF, 0xFF],
        "col 2 = backdrop white"
    );
}

#[test]
fn mode1_bg3_priority_bit_lifts_bg3_above_bg1() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    // Mode 1 + BG3 priority gate ($2105 bit 3).
    set_bgmode(&mut bus, 1, true);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x05); // BG1 + BG3

    // Shared tile-0 data: plane0 = 0xFF -> all 8 cols carry pixel 1.
    // For BG1 (4bpp) this looks at vram[0..16]; for BG3 (2bpp)
    // only vram[0..8] matters.
    write_vram_word(&mut bus, 0x0000, 0x00FF);

    // BG1 tilemap entry (0, 0): tile 0, palette 0, low priority.
    write_vram_word(&mut bus, 0x0400, 0x0000);
    // BG3 tilemap entry (0, 0): tile 0, palette 1, HIGH priority.
    //   high prio (bit 13) | palette 1 (bit 10) | tile 0 = 0x2400.
    write_vram_word(&mut bus, 0x0C00, 0x2400);

    // BG1: palette 0 + pixel 1 -> CGRAM[1] = green.
    write_cgram(&mut bus, 1, bgr15(0, 31, 0));
    // BG3 2bpp: palette 1 + pixel 1 -> CGRAM[1 * 4 + 1] = CGRAM[5] = blue.
    write_cgram(&mut bus, 5, bgr15(0, 0, 31));

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0, 0, 0xFF, 0xFF],
        "BG3 hi-prio lifted above BG1 by $2105 bit 3"
    );
}

#[test]
fn mode1_bg1_priority_high_above_bg2_priority_low() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x03); // BG1 + BG2

    // BG1 + BG2 share tile-0 4bpp data (plane0 = 0xFF -> pixel 1).
    write_vram_word(&mut bus, 0x0000, 0x00FF);

    // BG1 tilemap (0, 0): tile 0, palette 0, HIGH priority.
    write_vram_word(&mut bus, 0x0400, 0x2000);
    // BG2 tilemap (0, 0): tile 0, palette 2, HIGH priority.
    write_vram_word(&mut bus, 0x0800, 0x2000 | (2 << 10));

    write_cgram(&mut bus, 1, bgr15(31, 0, 0)); // BG1 hi -> red
    write_cgram(&mut bus, 33, bgr15(0, 31, 0)); // BG2 (pal 2 + idx 1) -> green

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0xFF, 0, 0, 0xFF],
        "BG1 hi outranks BG2 hi in Mode 1"
    );
}

// -------------------------------------------------------------
// Mode 0, 4 BGs x 2bpp + per-BG palette offset
// -------------------------------------------------------------

#[test]
fn mode0_each_bg_uses_separate_palette_quadrant() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 0, false);
    install_standard_vram_layout(&mut bus);
    // Disable all but BG4 to isolate the palette offset test:
    // BG4 should pick palette quadrant base 96.
    set_main_enable(&mut bus, 0x08);

    // Tile 0 row 0 2bpp: plane0=0xFF, plane1=0 -> all cols = pixel 1.
    write_vram_word(&mut bus, 0x0000, 0x00FF);
    // BG4 tilemap entry (0, 0) at vram[0x1000]: tile 0, palette 0.
    // For 2bpp: cgram_index = palette_offset(96) + 0*4 + pixel(1) = 97.
    write_vram_word(&mut bus, 0x1000, 0x0000);
    // CGRAM[1] is a poison value, would be picked if palette_offset
    // were missed.
    write_cgram(&mut bus, 1, bgr15(0, 0, 31));
    write_cgram(&mut bus, 97, bgr15(0, 31, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0, 0xFF, 0, 0xFF],
        "BG4 uses CGRAM[96..] not CGRAM[0..]"
    );
}

// -------------------------------------------------------------
// Mode 3, BG1 8bpp + direct colour gate
// -------------------------------------------------------------

#[test]
fn mode3_bg1_8bpp_uses_full_256_color_cgram() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 3, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);

    // 8bpp tile 0 row 0: plane0=0xFF -> pixel 1 across all cols.
    // (Planes 2-7 stay zero so the high bits don't contribute.)
    write_vram_word(&mut bus, 0x0000, 0x00FF);
    // Tilemap entry: tile 0, palette 7 -> palette is masked away in
    // 8bpp + non-direct-colour -> CGRAM[1] used directly.
    write_vram_word(&mut bus, 0x0400, 7 << 10);
    write_cgram(&mut bus, 1, bgr15(0, 31, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), [0, 0xFF, 0, 0xFF]);
}

#[test]
fn mode3_direct_color_bypasses_cgram_when_cgwsel_bit0_set() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 3, false);
    set_cgwsel(&mut bus, true);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);

    // 8bpp pixel value across all cols = 0x07 (= 0b00000111).
    // Planes 0/1/2 are bit 7 = 1 across the whole row, planes 3-7 = 0.
    //   word 0  -> planes 0+1, plane0 = 0xFF, plane1 = 0xFF.
    //   word 8  -> planes 2+3, plane2 = 0xFF.
    //   word 16 -> planes 4+5 = 0.
    //   word 24 -> planes 6+7 = 0.
    write_vram_word(&mut bus, 0x0000, 0xFFFF);
    write_vram_word(&mut bus, 0x0008, 0x00FF);
    // Tilemap entry: tile 0, palette 0.
    write_vram_word(&mut bus, 0x0400, 0x0000);
    // Poison CGRAM[7] so accidental lookups show.
    write_cgram(&mut bus, 7, bgr15(0, 0, 31));

    bus.ppu_mut().render_scanline(1, &mut fb);
    let p = pixel_at(&fb, 0, 0);
    // Direct colour: pixel = 0x07 -> R bits 0-2 = 0b111. 5-bit R
    // assembled per bsnes-plus = (pixel R bits << 2) | (palette R
    // bits) = 0b11100 = 0x1C. Expanded 5->8: (0x1C << 3) | (0x1C >>
    // 2) = 0xE0 | 0x07 = 0xE7.
    assert_eq!(p[0], 0xE7, "direct colour red channel");
    assert_eq!(p[1], 0, "no green");
    assert_eq!(p[2], 0, "no blue");
    assert_eq!(p[3], 0xFF);
}

// -------------------------------------------------------------
// Mode 4, BG1 8bpp + BG2 2bpp + BG3 as offset-per-tile source
// -------------------------------------------------------------

#[test]
fn mode4_bg3_offset_shifts_bg1_h_origin() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 4, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01); // BG1 only

    // Reset BG3 voffset to 0 so the offset-per-tile lookup hits BG3
    // tile row 0 (the helper parked it at 0xFFFF otherwise -> ty=31).
    set_bg_voffset(&mut bus, 2, 0);

    // BG1 8bpp tiles. 8bpp tile N starts at vram[N * 32].
    //   tile 0 row 0 -> pixel 1 (plane0 = 0xFF): vram[0]    = 0x00FF
    //   tile 1 row 0 -> pixel 2 (plane1 = 0xFF): vram[0x20] = 0xFF00
    //   tile 2 row 0 -> pixel 4 (plane2 = 0xFF): vram[0x48] = 0x00FF
    write_vram_word(&mut bus, 0x0000, 0x00FF);
    write_vram_word(&mut bus, 0x0020, 0xFF00);
    write_vram_word(&mut bus, 0x0048, 0x00FF);

    // BG1 tilemap entries:
    //   (0, 0) at vram[0x400] = tile 0 -> red
    //   (1, 0) at vram[0x401] = tile 1 -> green (renders here WITHOUT
    //                                   offset-per-tile)
    //   (2, 0) at vram[0x402] = tile 2 -> blue  (where the H override
    //                                   redirects column 1)
    write_vram_word(&mut bus, 0x0400, 0x0000);
    write_vram_word(&mut bus, 0x0401, 0x0001);
    write_vram_word(&mut bus, 0x0402, 0x0002);

    // BG3 tilemap entry (0, 0) at vram[0xC00]:
    //   hval = 0x2008 -> valid bit 13 set for BG1 + bit 15 = 0 (H
    //   override) + magnitude (hval & ~7) = 0x2008. bsnes-plus treats
    //   the override as the ABSOLUTE tilemap coordinate (it feeds
    //   `tx = hoffset >> 3` directly, no second `+ x`). For BG1 tile
    //   column 1 (tile_left_x = 8) the renderer evaluates
    //   `screen_x = offset_x(8) + (hval & ~7 = 0x2008) = 0x2010`, so
    //   `tile_x = (0x2010 & 0xFF) >> 3 = 2` -> BG1 entry (2, 0) = tile 2
    //   = blue. The valid bit 0x2000 rides bit 13 of screen_x and is
    //   masked off by `& 0xFF` at the tilemap-address step.
    write_vram_word(&mut bus, 0x0C00, 0x2008);

    write_cgram(&mut bus, 1, bgr15(31, 0, 0));
    write_cgram(&mut bus, 2, bgr15(0, 31, 0));
    write_cgram(&mut bus, 4, bgr15(0, 0, 31));

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0xFF, 0, 0, 0xFF],
        "col 0 = red (no override, offset_x < 8)"
    );
    assert_eq!(
        pixel_at(&fb, 8, 0),
        [0, 0, 0xFF, 0xFF],
        "col 8 = blue (BG3 H override redirected from tile 1 to tile 2)"
    );
}

// -------------------------------------------------------------
// Mosaic
// -------------------------------------------------------------

#[test]
fn mosaic_h_replicates_leftmost_pixel_of_each_block() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);
    set_mosaic(&mut bus, 3, 0x01); // size 4 (encoded 3), BG1.

    // Tile 0 4bpp row 0:
    //   plane0 = 0xF0 (cols 0..3 -> +1)
    //   plane1 = 0x0F (cols 4..7 -> +2)
    // -> cols 0..3 = pixel 1, cols 4..7 = pixel 2.
    write_vram_word(&mut bus, 0x0000, 0x0FF0);
    write_vram_word(&mut bus, 0x0400, 0x0000);
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));
    write_cgram(&mut bus, 2, bgr15(0, 31, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);
    for x in 0..4 {
        assert_eq!(pixel_at(&fb, x, 0), [0xFF, 0, 0, 0xFF], "x={x} (block 0)");
    }
    for x in 4..8 {
        assert_eq!(pixel_at(&fb, x, 0), [0, 0xFF, 0, 0xFF], "x={x} (block 1)");
    }
}

#[test]
fn mosaic_v_replays_previous_row_when_not_on_block_boundary() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);
    set_mosaic(&mut bus, 3, 0x01); // size 4, BG1 mosaic.

    // The helper parks voffset at 0xFFFF, so scanline 1 -> tile row 0
    // and scanline 2 -> tile row 1. Write distinct tile rows so the
    // test catches a regression where vmosaic falls through to the
    // natural-row draw instead of replaying the previous one.
    //   row 0 -> pixel 1 (red)   via plane0 byte at row 0
    //   row 1 -> pixel 2 (green) via plane1 byte at row 1
    write_vram_word(&mut bus, 0x0000, 0x00FF);
    write_vram_word(&mut bus, 0x0001, 0xFF00);

    write_vram_word(&mut bus, 0x0400, 0x0000);
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));
    write_cgram(&mut bus, 2, bgr15(0, 31, 0));

    // Scanline 1 is the V-mosaic fresh row (bsnes-plus initialises
    // `mosaic_vcounter = 1` at y=1 and `(1 - 1) % 4 == 0`).
    bus.ppu_mut().render_scanline(1, &mut fb);
    let row1 = pixel_at(&fb, 0, 0);
    assert_eq!(
        row1,
        [0xFF, 0, 0, 0xFF],
        "scanline 1 is fresh -> row 0 = red"
    );

    // Scanline 2 is inside the same mosaic block (`(2 - 1) % 4 ==
    // 1`), so vmosaic replays scanline 1's red even though tile row
    // 1 is green.
    bus.ppu_mut().render_scanline(2, &mut fb);
    let row2 = pixel_at(&fb, 0, 1);
    assert_eq!(row2, row1, "scanline 2 replays scanline 1");
}

#[test]
fn mosaic_size_one_is_a_noop_even_with_enable_bit_set() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);
    set_mosaic(&mut bus, 0, 0x0F); // size 1 (encoded 0), all BGs.

    write_vram_word(&mut bus, 0x0000, 0x0FF0);
    write_vram_word(&mut bus, 0x0400, 0x0000);
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));
    write_cgram(&mut bus, 2, bgr15(0, 31, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), [0xFF, 0, 0, 0xFF]);
    assert_eq!(pixel_at(&fb, 4, 0), [0, 0xFF, 0, 0xFF]);
}

// -------------------------------------------------------------
// Brightness
// -------------------------------------------------------------

#[test]
fn brightness_15_is_unattenuated() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);

    write_vram_word(&mut bus, 0x0000, 0x00FF);
    write_vram_word(&mut bus, 0x0400, 0x0000);
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0)[0], 0xFF);
}

#[test]
fn brightness_8_is_about_half() {
    let (mut bus, mut fb) = fresh_test_setup(8);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);

    write_vram_word(&mut bus, 0x0000, 0x00FF);
    write_vram_word(&mut bus, 0x0400, 0x0000);
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);
    // R5 = 31 -> expanded 0xFF. Brightness 8: scaled by (8+1)/16 =
    // 9/16. (0xFF * 9) >> 4 = 2295 >> 4 = 143 = 0x8F.
    assert_eq!(pixel_at(&fb, 0, 0)[0], 0x8F);
}

// -------------------------------------------------------------
// Tile flips
// -------------------------------------------------------------

#[test]
fn tile_flip_h_mirrors_row_pixels() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);

    // Tile 0 4bpp row 0: only col 0 = pixel 1 (plane0 bit 7).
    write_vram_word(&mut bus, 0x0000, 0x0080);
    // Tilemap entry with flip_h (bit 14).
    write_vram_word(&mut bus, 0x0400, 0x4000);
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));
    write_cgram(&mut bus, 0, bgr15(0, 0, 31));

    bus.ppu_mut().render_scanline(1, &mut fb);
    // After flip_h, original col 0 lands at col 7.
    assert_eq!(pixel_at(&fb, 7, 0), [0xFF, 0, 0, 0xFF]);
    assert_eq!(pixel_at(&fb, 0, 0), [0, 0, 0xFF, 0xFF], "col 0 backdrop");
}

#[test]
fn tile_flip_v_mirrors_row_selector_within_tile() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);

    // Tile 0 4bpp row 7 = pixel 1 across all cols (everything else 0).
    write_vram_word(&mut bus, 0x0007, 0x00FF);
    // Tilemap entry with flip_v (bit 15).
    write_vram_word(&mut bus, 0x0400, 0x8000);
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));
    write_cgram(&mut bus, 0, bgr15(0, 0, 31));

    // Scanline 1 lands at tile row 0; with flip_v that pulls
    // row 7 -> all cols = red.
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), [0xFF, 0, 0, 0xFF]);
    assert_eq!(pixel_at(&fb, 7, 0), [0xFF, 0, 0, 0xFF]);
}

// -------------------------------------------------------------
// Tilemap sizes + scroll
// -------------------------------------------------------------

#[test]
fn tilemap_size_1_wraps_horizontally_at_64_tiles() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    // Override BG1 tilemap to size 1 (64x32, two horizontal pages).
    //   addr base = 0x0800 + size bits = 0x04 | 0x01 = 0x05.
    bus.write(ppu_addr(0x2107), 0x05);
    set_main_enable(&mut bus, 0x01);

    // Tile 0 = pixel 1 (red), tile 1 = pixel 2 (green).
    write_vram_word(&mut bus, 0x0000, 0x00FF);
    write_vram_word(&mut bus, 0x0010, 0xFF00);
    // Tilemap base = vram[0x400]. Page 0 tile (0, 0) = entry 0 = tile 0.
    write_vram_word(&mut bus, 0x0400, 0x0000);
    // Page 1 tile (0, 0) sits at base + 0x400 word offset
    // (32 tiles x 32 rows / page).
    write_vram_word(&mut bus, 0x0800, 0x0001);
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));
    write_cgram(&mut bus, 2, bgr15(0, 31, 0));
    // H scroll = 256 -> screen_x for col 0 = 256 -> tile_x = 32 ->
    // tx & 0x20 -> page 1.
    set_bg_hoffset(&mut bus, 0, 256);

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0, 0xFF, 0, 0xFF],
        "wrap into page 1 -> tile 1 = green"
    );
}

#[test]
fn scroll_h_one_pixel_shifts_tile_left() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);

    // Tile 0 4bpp row 0: col 0 = pixel 1, rest 0.
    write_vram_word(&mut bus, 0x0000, 0x0080);
    write_vram_word(&mut bus, 0x0400, 0x0000);
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));
    write_cgram(&mut bus, 0, bgr15(0, 0, 31));
    set_bg_hoffset(&mut bus, 0, 1);

    bus.ppu_mut().render_scanline(1, &mut fb);
    // With H scroll = 1, screen col 0 fetches tile col 1 (= 0,
    // transparent -> backdrop blue).
    assert_eq!(pixel_at(&fb, 0, 0), [0, 0, 0xFF, 0xFF]);
}

// -------------------------------------------------------------
// Backdrop / transparency
// -------------------------------------------------------------

#[test]
fn backdrop_fills_pixels_left_transparent_by_every_bg() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x07);
    write_cgram(&mut bus, 0, bgr15(0, 0, 31));

    bus.ppu_mut().render_scanline(1, &mut fb);
    for x in 0..16 {
        assert_eq!(pixel_at(&fb, x, 0), [0, 0, 0xFF, 0xFF], "x={x}");
    }
}

#[test]
fn cgram_color_zero_in_palette_acts_transparent_not_backdrop() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);

    // Tile 0 4bpp row 0: only col 0 = pixel 1.
    write_vram_word(&mut bus, 0x0000, 0x0080);
    write_vram_word(&mut bus, 0x0400, 0x0000);
    write_cgram(&mut bus, 0, bgr15(0, 31, 0));
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0xFF, 0, 0, 0xFF],
        "col 0 = tile colour"
    );
    assert_eq!(
        pixel_at(&fb, 1, 0),
        [0, 0xFF, 0, 0xFF],
        "col 1 = backdrop (pixel value 0 -> transparent)"
    );
}

// -------------------------------------------------------------
// Disabled BG
// -------------------------------------------------------------

#[test]
fn bg_main_disable_hides_layer() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x00);

    write_vram_word(&mut bus, 0x0000, 0x00FF);
    write_vram_word(&mut bus, 0x0400, 0x0000);
    write_cgram(&mut bus, 0, bgr15(0, 0, 31));
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0, 0, 0xFF, 0xFF],
        "BG1 disabled -> backdrop fills"
    );
}

// -------------------------------------------------------------
// Mode 2, offset-per-tile path uses BG3 H + V slots
// -------------------------------------------------------------

#[test]
fn mode2_renders_bg1_4bpp_without_crashing_with_offset_path() {
    // Smoke test: Mode 2 walks the offset-per-tile branch for every
    // BG1 tile column >= 1, make sure pixel output remains sane
    // (= tile pixel colour) when the BG3 valid mask is clear (no
    // override should apply).
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 2, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);
    set_bg_voffset(&mut bus, 2, 0);

    // BG1 tile 0 4bpp row 0: all cols = pixel 1.
    write_vram_word(&mut bus, 0x0000, 0x00FF);
    // BG1 tilemap: tile 0 across columns 0..3 (vram[0x400..0x404]).
    for col in 0..4 {
        write_vram_word(&mut bus, 0x0400 + col, 0x0000);
    }
    // BG3 tilemap: leave zero -> valid bits clear -> no override.
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);
    for x in 0..32 {
        assert_eq!(pixel_at(&fb, x, 0), [0xFF, 0, 0, 0xFF], "x={x}");
    }
}

/// A Mode 2 BG3 vertical override must shift the column's sample point by
/// exactly `scanline + vval` (1x), NOT `scanline + (scanline + vval)` (2x).
///
/// `compute_offset_per_tile` already returns an absolute V position, so
/// feeding it back through `screen_y = scanline + voffset` would count the
/// scanline twice: the BG would be sampled at 2x vertical speed (half
/// height) and wrap the tilemap, and Chrono Trigger's "TRIGGER" title
/// would appear twice (top + bottom).
///
/// Setup: BG1 column 1 (output x = 8) gets a V override with magnitude
/// 0 (`vval = 0x2000`, just the BG1 valid bit). With base voffset 0 the
/// correct sample row equals the scanline: scanline 4 -> screen_y 4 ->
/// BG1 tile row 0 (entry (1,0) = red); scanline 12 -> screen_y 12 -> BG1
/// tile row 1 (entry (1,1) = green). A 2x path would instead
/// land on row 1 (green) at scanline 4 and row 3 (empty -> backdrop) at
/// scanline 12.
#[test]
fn mode2_v_override_tracks_scanline_not_doubled() {
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 2, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);
    // Clean math: BG1 + BG3 vertical scroll both 0 (the helper parks
    // them at 0xFFFF otherwise).
    set_bg_voffset(&mut bus, 0, 0);
    set_bg_voffset(&mut bus, 2, 0);

    // BG1 4bpp solid tiles: tile 1 = pixel value 1 (red), tile 2 =
    // pixel value 2 (green). 4bpp tile N occupies words [N*16, N*16+16);
    // rows 0..7 hold plane0/plane1 in the low/high byte.
    for r in 0..8 {
        write_vram_word(&mut bus, 0x10 + r, 0x00FF); // tile 1: plane0 set
        write_vram_word(&mut bus, 0x20 + r, 0xFF00); // tile 2: plane1 set
    }
    // BG1 tilemap: (1,0) = tile 1 (red), (1,1) = tile 2 (green). Base
    // word 0x400; row stride 0x20.
    write_vram_word(&mut bus, 0x0401, 0x0001);
    write_vram_word(&mut bus, 0x0421, 0x0002);

    // BG3 V slot for the x=8 column sits at tile (0,1) (the V slot is
    // read at `bg3_voffset + 8` -> BG3 tile row 1). Word = 0xC00 + 0x20.
    // vval = 0x2000 -> BG1 valid bit, magnitude 0. The H slot (BG3 tile
    // (0,0), word 0xC00) stays 0 so the column keeps screen_x = 8.
    write_vram_word(&mut bus, 0x0C20, 0x2000);

    write_cgram(&mut bus, 1, bgr15(31, 0, 0)); // red
    write_cgram(&mut bus, 2, bgr15(0, 31, 0)); // green

    bus.ppu_mut().render_scanline(4, &mut fb);
    bus.ppu_mut().render_scanline(12, &mut fb);

    assert_eq!(
        pixel_at(&fb, 8, 3),
        [0xFF, 0, 0, 0xFF],
        "scanline 4 -> row 0 = red (1x V override, not the 2x green)"
    );
    assert_eq!(
        pixel_at(&fb, 8, 11),
        [0, 0xFF, 0, 0xFF],
        "scanline 12 -> row 1 = green (override advances at 1x)"
    );
}

// -------------------------------------------------------------
// Mode 5, Hi-res 512-pixel BG1 4bpp + BG2 2bpp
// -------------------------------------------------------------

/// Mode 5 layout: BG1 is 4bpp (16 colours), BG2 is 2bpp (4
/// colours). The renderer walks 512 display pixels per scanline,
/// each tilemap entry covers 16 BG-pixels (paired adjacent-tile
/// fetch), and the final composite interleaves main (odd display
/// pixels) with sub (even display pixels).

#[test]
fn mode5_bg1_4bpp_renders_into_hires_512_buffer() {
    let (mut bus, _) = fresh_test_setup(15);
    let mut fb = fresh_fb_hires();
    set_bgmode(&mut bus, 5, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01); // BG1 on main only

    // BG1 4bpp tile 0 row 0: all cols = pixel 1.
    write_vram_word(&mut bus, 0x0000, 0x00FF);
    // BG1 tilemap (0, 0): tile 0, palette 0.
    write_vram_word(&mut bus, 0x0400, 0x0000);
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);

    // Hires output: main occupies odd display columns. Since sub
    // is disabled, even columns fall through to backdrop (= 0).
    // Display col 1 (= main pixel 0) should be red.
    assert_eq!(
        pixel_at_512(&fb, 1, 0),
        [0xFF, 0, 0, 0xFF],
        "display col 1 (odd -> main) = BG1 red"
    );
    // Display col 0 (= sub pixel 0) is backdrop (sub plane empty,
    // CGRAM[0] = 0 by default).
    assert_eq!(
        pixel_at_512(&fb, 0, 0),
        [0, 0, 0, 0xFF],
        "display col 0 (even -> sub backdrop)"
    );
}

#[test]
fn mode5_main_and_sub_screens_are_interleaved() {
    let (mut bus, _) = fresh_test_setup(15);
    let mut fb = fresh_fb_hires();
    set_bgmode(&mut bus, 5, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01); // BG1 -> main
    set_sub_enable(&mut bus, 0x02); // BG2 -> sub

    // BG1 + BG2 share tile 0 row 0. BG1 is 4bpp, BG2 is 2bpp.
    // BG1 4bpp row 0: plane0 = 0xFF -> all cols = pixel 1.
    // BG2 2bpp row 0: same plane0 = 0xFF -> all cols = pixel 1.
    write_vram_word(&mut bus, 0x0000, 0x00FF);
    // BG1 tilemap: tile 0.
    write_vram_word(&mut bus, 0x0400, 0x0000);
    // BG2 tilemap: tile 0.
    write_vram_word(&mut bus, 0x0800, 0x0000);
    // CGRAM: BG1 red, BG2 green.
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));
    // BG2 is 2bpp -> cgram_index = palette 0 * 4 + pixel 1 = 1.
    // Both BG1 and BG2 collide on CGRAM[1] -> use a different
    // BG2 palette so the colour disambiguates.
    write_vram_word(&mut bus, 0x0800, 1 << 10); // BG2 palette 1
    write_cgram(&mut bus, 5, bgr15(0, 31, 0)); // palette 1 + pixel 1

    bus.ppu_mut().render_scanline(1, &mut fb);

    // Display col 0 = sub (BG2 green).
    assert_eq!(pixel_at_512(&fb, 0, 0), [0, 0xFF, 0, 0xFF], "sub col 0");
    // Display col 1 = main (BG1 red).
    assert_eq!(pixel_at_512(&fb, 1, 0), [0xFF, 0, 0, 0xFF], "main col 1");
    // The pattern repeats across the row.
    assert_eq!(pixel_at_512(&fb, 2, 0), [0, 0xFF, 0, 0xFF], "sub col 2");
    assert_eq!(pixel_at_512(&fb, 3, 0), [0xFF, 0, 0, 0xFF], "main col 3");
}

#[test]
fn mode5_hires_writes_beyond_column_256() {
    let (mut bus, _) = fresh_test_setup(15);
    let mut fb = fresh_fb_hires();
    set_bgmode(&mut bus, 5, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);

    // In hires Mode 5, each tilemap entry covers 16 BG-pixels by
    // pairing the entry's tile with `tile + 1` (adjacent-tile
    // fetch). Both tiles 0 AND 1 must carry data or the right
    // 8-px half of every 16-px slot reads as transparent.
    //   tile 0 row 0 (4bpp): plane0 = 0xFF -> all cols pixel 1
    //   tile 1 row 0 (4bpp): plane0 = 0xFF (4bpp tile stride = 16 words)
    write_vram_word(&mut bus, 0x0000, 0x00FF);
    write_vram_word(&mut bus, 0x0010, 0x00FF);
    // BG1 tilemap row covers every 16-px slot with tile 0.
    // Tilemap row 0 spans cols 0..32 (32 entries x 16 px = 512 px).
    for col in 0..32 {
        write_vram_word(&mut bus, 0x0400 + col, 0x0000);
    }
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);

    // Hires writes the full 512-pixel line. Odd columns show the
    // main screen; with only TM enabled the BG is red there. Display
    // column k maps 1:1 to BG column k (no 2x stretch). This uniform
    // fill is a smoke test, `mode5_hires_renders_full_512_width_
    // distinct_halves` is the column-mapping regression guard.
    assert_eq!(
        pixel_at_512(&fb, 301, 0),
        [0xFF, 0, 0, 0xFF],
        "odd column 301 (main screen) is the BG1 colour"
    );
    assert_eq!(
        pixel_at_512(&fb, 511, 0),
        [0xFF, 0, 0, 0xFF],
        "last hires column is the BG1 colour"
    );
}

#[test]
fn mode5_hires_renders_full_512_width_distinct_halves() {
    // All 512 BG columns of a hires line reach the screen: columns
    // 0..255 are not stretched 2x, and 256..511 are not dropped.
    // Paint the left half of the 512-wide line red and the
    // right half green, then assert every display column shows its own
    // BG position's colour. BG1 is enabled on BOTH main ($212C) and
    // sub ($212D) so even (sub) and odd (main) columns both carry it.
    let (mut bus, _) = fresh_test_setup(15);
    let mut fb = fresh_fb_hires();
    set_bgmode(&mut bus, 5, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);
    set_sub_enable(&mut bus, 0x01);

    // Red pair (tiles 0 & 1, index 1 via plane0) and green pair
    // (tiles 2 & 3, index 2 via plane1). Each hires 16-px slot pairs
    // tile T with T+1, so both halves of every slot need data.
    write_vram_word(&mut bus, 0x0000, 0x00FF); // tile 0 row 0 -> idx 1
    write_vram_word(&mut bus, 0x0010, 0x00FF); // tile 1 row 0 -> idx 1
    write_vram_word(&mut bus, 0x0020, 0xFF00); // tile 2 row 0 -> idx 2
    write_vram_word(&mut bus, 0x0030, 0xFF00); // tile 3 row 0 -> idx 2

    // Tilemap row 0: entries 0..15 (BG cols 0..255) -> tile 0 (red),
    // entries 16..31 (BG cols 256..511) -> tile 2 (green).
    for tx in 0..16 {
        write_vram_word(&mut bus, 0x0400 + tx, 0x0000);
    }
    for tx in 16..32 {
        write_vram_word(&mut bus, 0x0400 + tx, 0x0002);
    }
    write_cgram(&mut bus, 1, bgr15(31, 0, 0)); // red
    write_cgram(&mut bus, 2, bgr15(0, 31, 0)); // green

    bus.ppu_mut().render_scanline(1, &mut fb);

    // Left half: display cols 0..255 map 1:1 to BG cols 0..255 -> red.
    // Check an even (sub) and an odd (main) column.
    assert_eq!(
        pixel_at_512(&fb, 10, 0),
        [0xFF, 0, 0, 0xFF],
        "left-half even column is red"
    );
    assert_eq!(
        pixel_at_512(&fb, 11, 0),
        [0xFF, 0, 0, 0xFF],
        "left-half odd column is red"
    );
    // Right half: BG cols 256..511 must NOT be dropped. A renderer
    // that stretched the left half would sample BG col display/2 = 200
    // here and show red.
    assert_eq!(
        pixel_at_512(&fb, 400, 0),
        [0, 0xFF, 0, 0xFF],
        "right-half even column is green (BG col 400, not dropped)"
    );
    assert_eq!(
        pixel_at_512(&fb, 401, 0),
        [0, 0xFF, 0, 0xFF],
        "right-half odd column is green"
    );
    assert_eq!(
        pixel_at_512(&fb, 511, 0),
        [0, 0xFF, 0, 0xFF],
        "last hires column is green"
    );
}

#[test]
fn mode5_interlace_renders_full_448_height() {
    // Mode 5/6 interlace ($2133 bit 0) doubles the vertical output: the
    // height doubles to 448 and each scanline emits two woven rows, so
    // the full image is built in one frame, not just the 224 rows of
    // the top field.
    let (mut bus, _) = fresh_test_setup(15);
    let mut fb = fresh_fb_hires();
    set_bgmode(&mut bus, 5, false);
    set_setini(&mut bus, 0x01); // interlace (bit 0)
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);
    set_sub_enable(&mut bus, 0x01);

    // Solid red BG: tile 0 + adjacent tile 1, every row = index 1.
    // VRAM (and thus the tilemap) defaults to all-zero = tile 0, so
    // the whole plane is red across every 512x448 position.
    for row in 0..8 {
        write_vram_word(&mut bus, row, 0x00FF);
        write_vram_word(&mut bus, 0x0010 + row, 0x00FF);
    }
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));

    // Each scanline emits two woven rows -> 224 scanlines fill 448
    // framebuffer rows.
    for sl in 1..=224 {
        bus.ppu_mut().render_scanline(sl, &mut fb);
    }

    // Interlace doubles the reported height. The size is frame-global,
    // so it reads back correctly only once the frame's first hires
    // scanline has latched `render_interlace` (at scanline 1).
    assert_eq!(bus.ppu().active_display_size(), (512, 448));

    // Top half renders red.
    assert_eq!(
        pixel_at_512(&fb, 100, 10),
        [0xFF, 0, 0, 0xFF],
        "top-half row is red"
    );
    // Bottom half (rows 224..447) is only reached by the interlace
    // weave; without it these rows would keep the 0xAB sentinel.
    assert_eq!(
        pixel_at_512(&fb, 100, 300),
        [0xFF, 0, 0, 0xFF],
        "bottom-half row 300 rendered red (interlace 448)"
    );
    assert_eq!(
        pixel_at_512(&fb, 100, 447),
        [0xFF, 0, 0, 0xFF],
        "last interlace row 447 rendered red"
    );
}

#[test]
fn mode5_hires_horizontal_scroll_is_doubled() {
    // In Mode 5/6 the BG horizontal scroll lives in the 512-dot hires
    // space, so it must be doubled (bsnes-plus
    // `hscroll = regs.bg_hofs[bg] << hires`). With BGHOFS = 8 the
    // red/green boundary at BG column 256 must move LEFT by 16 display
    // columns (2x8) to display col 240. An undoubled scroll would move it
    // only 8 columns (to 248), so display col 245 would read red instead
    // of green; the BG would run at half speed while sprites stay in
    // 256-dot space, which breaks RPM Racing's demo track as it scrolls
    // (the cars slide onto the grass).
    let (mut bus, _) = fresh_test_setup(15);
    let mut fb = fresh_fb_hires();
    set_bgmode(&mut bus, 5, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);
    set_sub_enable(&mut bus, 0x01);
    set_bg_hoffset(&mut bus, 0, 8);

    // Same red-left / green-right tile layout as the full-width test:
    // tiles 0/1 -> index 1 (red), tiles 2/3 -> index 2 (green). Hires
    // pairs tile T with T+1, so both halves of every 16-px slot have
    // data.
    write_vram_word(&mut bus, 0x0000, 0x00FF);
    write_vram_word(&mut bus, 0x0010, 0x00FF);
    write_vram_word(&mut bus, 0x0020, 0xFF00);
    write_vram_word(&mut bus, 0x0030, 0xFF00);
    for tx in 0..16 {
        write_vram_word(&mut bus, 0x0400 + tx, 0x0000); // red half
    }
    for tx in 16..32 {
        write_vram_word(&mut bus, 0x0400 + tx, 0x0002); // green half
    }
    write_cgram(&mut bus, 1, bgr15(31, 0, 0)); // red
    write_cgram(&mut bus, 2, bgr15(0, 31, 0)); // green

    bus.ppu_mut().render_scanline(1, &mut fb);

    // BGHOFS 8 doubled to 16: boundary (BG col 256) lands at display
    // col 240. Display col 245 is past it -> BG col ~261 -> green. An
    // undoubled (half-speed) scroll would sample BG col ~253 there -> red.
    assert_eq!(
        pixel_at_512(&fb, 245, 0),
        [0, 0xFF, 0, 0xFF],
        "doubled scroll: display col 245 shows BG col ~261 (green)"
    );
    // Display col 239 is before the doubled boundary -> still red.
    assert_eq!(
        pixel_at_512(&fb, 239, 0),
        [0xFF, 0, 0, 0xFF],
        "display col 239 shows BG col ~255 (red)"
    );
}

#[test]
fn hires_interlace_latched_for_whole_frame() {
    // The interlace decision (the weave row count) must be latched at the
    // frame top, not re-sampled per scanline: ASP: Air Strike Patrol
    // toggles $2133 bit 0 around its Mode 5 menus, and if the weave
    // followed the live bit mid-frame the rendered rows would disagree
    // across the frame (only the top half, or duplicated text). Since
    // *any* hires frame reports 448 (non-interlace reaches it via
    // finalize doubling), the latch is
    // observed through `frame_interlaced`, which distinguishes a true
    // weave from a line-doubled non-interlace frame, while the reported
    // height stays a stable 448 throughout.
    let (mut bus, _) = fresh_test_setup(15);
    let mut fb = fresh_fb_hires();
    set_bgmode(&mut bus, 5, false);
    set_setini(&mut bus, 0x01); // interlace ON
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);
    set_sub_enable(&mut bus, 0x01);

    // Scanline 1 latches render_interlace = true for this frame.
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        bus.ppu().active_display_size(),
        (512, 448),
        "hires frame -> 448"
    );
    assert!(
        bus.ppu().frame_interlaced(),
        "interlace latched at scanline 1 -> true weave"
    );

    // The game clears the interlace bit mid-frame. The latched decision
    // must NOT change for the rest of THIS frame.
    set_setini(&mut bus, 0x00); // live interlace bit OFF
    bus.ppu_mut().render_scanline(2, &mut fb);
    assert_eq!(
        bus.ppu().active_display_size(),
        (512, 448),
        "height stays 448 across the whole frame"
    );
    assert!(
        bus.ppu().frame_interlaced(),
        "mid-frame interlace toggle ignored until the next frame top"
    );

    // The next frame re-latches at scanline 1 with interlace now off.
    // Still a hires frame -> 448, but now a line-doubled (non-weave)
    // frame, so `frame_interlaced` flips to false.
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        bus.ppu().active_display_size(),
        (512, 448),
        "non-interlace hires still reports 448"
    );
    assert!(
        !bus.ppu().frame_interlaced(),
        "next frame re-latches interlace=off -> line-doubled, not woven"
    );
}

// -------------------------------------------------------------
// Mode 6, Hi-res BG1 4bpp + BG3 offset-per-tile, no BG2/3/4 drawn
// -------------------------------------------------------------

#[test]
fn mode6_bg1_only_other_bgs_invisible() {
    let (mut bus, _) = fresh_test_setup(15);
    let mut fb = fresh_fb_hires();
    set_bgmode(&mut bus, 6, false);
    install_standard_vram_layout(&mut bus);
    // Enable BG1 + BG2 + BG3, only BG1 should render. BG3 is the
    // offset table (invisible by definition in Mode 6); BG2 is
    // not part of Mode 6 at all.
    set_main_enable(&mut bus, 0x07);
    set_bg_voffset(&mut bus, 2, 0);

    // BG1 4bpp tile 0 + tile 1: pixel 1 in row 0 of both. Hires
    // adjacent-tile fetch needs both tiles populated or the right
    // 8-px half of every 16-px slot would render transparent.
    write_vram_word(&mut bus, 0x0000, 0x00FF);
    write_vram_word(&mut bus, 0x0010, 0x00FF);
    for col in 0..32 {
        write_vram_word(&mut bus, 0x0400 + col, 0x0000);
    }
    // BG2 tilemap entry that WOULD render green if BG2 were
    // visible in Mode 6.
    write_vram_word(&mut bus, 0x0800, 1 << 10);
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));
    write_cgram(&mut bus, 5, bgr15(0, 31, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);
    // Col 1 (main) = BG1 red. BG2 is silenced even though TM
    // enabled it.
    assert_eq!(pixel_at_512(&fb, 1, 0), [0xFF, 0, 0, 0xFF]);
}

#[test]
fn mode6_offset_per_tile_uses_bg3_lookup() {
    // Mode 6 reuses the Mode 2/4 offset-per-tile algorithm. We
    // run a BG3 H-override through it and check only that the
    // renderer still draws a pixel there (full alpha). Hires column
    // granularity means we look at display col 17 (= main pixel 8)
    // which is in the second tile slot.
    let (mut bus, _) = fresh_test_setup(15);
    let mut fb = fresh_fb_hires();
    set_bgmode(&mut bus, 6, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);
    set_bg_voffset(&mut bus, 2, 0);

    // BG1 4bpp tiles: tile 0 = pixel 1 (red), tile 4 = pixel 2
    // (green) at vram[0x40] = 0xFF00 (tile 4 plane1 = 0xFF).
    write_vram_word(&mut bus, 0x0000, 0x00FF);
    write_vram_word(&mut bus, 0x0040, 0xFF00);
    // BG1 tilemap entries covering 32 cols.
    for col in 0..32 {
        write_vram_word(&mut bus, 0x0400 + col, 0x0000);
    }
    // BG3 offset entry at tile (0, 0): hval = 0x2040 -> valid bit
    // 13 set + bit 15 = 0 (H override) + magnitude (hval & 0x3FF,
    // low three bits dropped) = 0x040. For a non-leftmost tile, the
    // renderer should bump BG1's hoffset. Smoke-test that the
    // renderer does NOT crash and still produces an opaque pixel.
    write_vram_word(&mut bus, 0x0C00, 0x2040);
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));
    write_cgram(&mut bus, 2, bgr15(0, 31, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);
    // Test passes if rendering doesn't panic and produces a valid
    // RGBA pixel (alpha = 0xFF) past the first 16-px slot.
    let p = pixel_at_512(&fb, 17, 0);
    assert_eq!(p[3], 0xFF, "Mode 6 offset path renders valid alpha");
}

// -------------------------------------------------------------
// Overscan (240 visible lines via $2133 bit 2)
// -------------------------------------------------------------

#[test]
fn overscan_enabled_renders_lines_225_to_239() {
    // With overscan on, the PPU active region extends to scanlines
    // 1..=239 (the last visible NTSC overscan line). Bsnes-plus
    // gates vblank starting at line 240; matching that, scanline
    // 239 -> host row 238 must be touched, but scanline 240 is
    // already vblank and stays clipped.
    let (mut bus, _) = fresh_test_setup(15);
    let mut fb = fresh_fb_hires();
    set_setini(&mut bus, 0x04); // bit 2 = overscan

    // Forced-blank scenario: even with display enabled and
    // brightness 0 the renderer writes black + alpha.
    bus.write(ppu_addr(0x2100), 0x00); // brightness 0 -> black
    bus.ppu_mut().render_scanline(239, &mut fb);
    assert_eq!(
        pixel_at_512(&fb, 0, 238),
        [0, 0, 0, 0xFF],
        "scanline 239 -> row 238 black + alpha (last overscan line)"
    );

    // Scanline 240 is the first non-visible line even with
    // overscan, must NOT be touched.
    bus.ppu_mut().render_scanline(240, &mut fb);
    assert_eq!(
        pixel_at_512(&fb, 0, 239),
        [0xAB, 0xAB, 0xAB, 0xAB],
        "scanline 240 is past the overscan visible region"
    );
}

#[test]
fn overscan_disabled_skips_lines_225_to_239() {
    // Without overscan the PPU active region stops at scanline 224
    // (host row 223). Scanlines 225..=239 must be no-ops so host
    // rows 224..=238 keep the sentinel byte (0xAB).
    let (mut bus, _) = fresh_test_setup(15);
    let mut fb = fresh_fb_hires();
    // Overscan OFF (bit 2 clear).
    set_setini(&mut bus, 0x00);

    bus.ppu_mut().render_scanline(225, &mut fb);
    bus.ppu_mut().render_scanline(239, &mut fb);
    assert_eq!(
        pixel_at_512(&fb, 0, 224),
        [0xAB, 0xAB, 0xAB, 0xAB],
        "scanline 225 with overscan off -> host row 224 untouched"
    );
    assert_eq!(
        pixel_at_512(&fb, 0, 238),
        [0xAB, 0xAB, 0xAB, 0xAB],
        "scanline 239 with overscan off -> host row 238 untouched"
    );
}

// -------------------------------------------------------------
// PPU active_display_size + field
// -------------------------------------------------------------

#[test]
fn active_display_size_reports_hires_dimensions() {
    // `active_display_size` keys off the FRAME-GLOBAL hires/interlace
    // state, which is (re)built each frame at scanline 1; render one
    // scanline after each register change so the flags reflect the
    // current mode before sampling the size.
    let (mut bus, _) = fresh_test_setup(15);
    let mut fb = fresh_fb_hires();
    // Expose the full overscan frame so this test exercises the raw
    // dimension reporting (the default 224-crop is covered separately).
    bus.ppu_mut().set_show_overscan(true);

    // Mode 0 + no overscan -> 256x224.
    set_bgmode(&mut bus, 0, false);
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(bus.ppu().active_display_size(), (256, 224));

    // Mode 5 (non-interlace hires) -> 512x448. bsnes displays
    // non-interlace hires line-doubled, so any hires frame reports the
    // doubled height regardless of the interlace bit.
    set_bgmode(&mut bus, 5, false);
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(bus.ppu().active_display_size(), (512, 448));

    // Mode 5 + overscan (non-interlace) -> 512x480.
    set_setini(&mut bus, 0x04);
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(bus.ppu().active_display_size(), (512, 480));

    // Mode 5 + interlace ($2133 bit 0) -> 512x448 (two woven fields,
    // same height as the line-doubled non-interlace case).
    set_setini(&mut bus, 0x01);
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(bus.ppu().active_display_size(), (512, 448));

    // Mode 5 + overscan + interlace -> 512x480 (worst case).
    set_setini(&mut bus, 0x05);
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(bus.ppu().active_display_size(), (512, 480));

    // Interlace is ignored outside hires: lo-res stays single-field.
    set_bgmode(&mut bus, 0, false);
    set_setini(&mut bus, 0x01);
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(bus.ppu().active_display_size(), (256, 224));

    // Mode 0 + overscan -> 256x240.
    set_setini(&mut bus, 0x04);
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(bus.ppu().active_display_size(), (256, 240));
}

#[test]
fn active_display_size_crops_overscan_to_tv_window_by_default() {
    // Default host behavior (no show_overscan toggle) matches
    // bsnes-plus / snes9x NTSC output: an overscan frame is reported at
    // the 224-line (448 hires) TV window, not the full 240/480.
    let (mut bus, _) = fresh_test_setup(15);
    let mut fb = fresh_fb_hires();

    // Mode 0 + overscan -> cropped to 256x224.
    set_bgmode(&mut bus, 0, false);
    set_setini(&mut bus, 0x04);
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(bus.ppu().active_display_size(), (256, 224));

    // Mode 5 (hires) + overscan -> cropped to 512x448.
    set_bgmode(&mut bus, 5, false);
    set_setini(&mut bus, 0x04);
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(bus.ppu().active_display_size(), (512, 448));

    // Enabling the toggle restores the full overscan dimensions.
    bus.ppu_mut().set_show_overscan(true);
    assert_eq!(bus.ppu().active_display_size(), (512, 480));
}

#[test]
fn non_hires_modes_use_only_first_256_columns_of_hires_buffer() {
    // Rendering Mode 1 into a 512-wide framebuffer
    // must leave columns 256..511 untouched. The host clamps via
    // `display_info.width = 256` so the right half is reserved for
    // a later hires frame.
    let (mut bus, _) = fresh_test_setup(15);
    let mut fb = fresh_fb_hires();
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);

    write_vram_word(&mut bus, 0x0000, 0x00FF);
    write_vram_word(&mut bus, 0x0400, 0x0000);
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);

    // Col 0 = BG1 red, col 256 = untouched (sentinel 0xAB).
    assert_eq!(pixel_at_512(&fb, 0, 0), [0xFF, 0, 0, 0xFF]);
    assert_eq!(
        pixel_at_512(&fb, 256, 0),
        [0xAB, 0xAB, 0xAB, 0xAB],
        "non-hires mode must not write into the second half"
    );
}

// -------------------------------------------------------------
// Mode 7, affine plane + EXTBG
// -------------------------------------------------------------

#[test]
fn mode7_identity_renders_tile_pixel_at_origin() {
    let (mut bus, mut fb) = mode7_setup();
    // Scanline 1 + identity samples world (px=0, py=1) for screen
    // (0,0): tilemap (0,0) = vram[0] low byte (tile 0, the default
    // zero), and the character pixel (cx=0, cy=1) = vram[8] high
    // byte = 0x42.
    write_vram_word(&mut bus, 0x0008, 0x4200);
    write_cgram(&mut bus, 0x42, bgr15(31, 0, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), [0xFF, 0, 0, 0xFF]);
    // The neighbouring world pixel is empty -> backdrop (black).
    assert_eq!(pixel_at(&fb, 1, 0), [0, 0, 0, 0xFF]);
}

#[test]
fn mode7_identity_maps_screen_x_to_world_x() {
    let (mut bus, mut fb) = mode7_setup();
    // Scanline 1 samples world row 1 (cy = 1). Tile 0 row 1
    // horizontal ramp: character pixel at cx = cx + 1 (high byte of
    // word 8 + cx). The low byte of word 0 stays 0 -> tilemap (0,0) =
    // tile 0.
    for cx in 0u16..3 {
        write_vram_word(&mut bus, 8 + cx, (cx + 1) << 8);
    }
    write_cgram(&mut bus, 1, bgr15(31, 0, 0)); // world x0 -> red
    write_cgram(&mut bus, 2, bgr15(0, 31, 0)); // world x1 -> green
    write_cgram(&mut bus, 3, bgr15(0, 0, 31)); // world x2 -> blue

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0xFF, 0, 0, 0xFF],
        "screen x0 -> world x0"
    );
    assert_eq!(
        pixel_at(&fb, 1, 0),
        [0, 0xFF, 0, 0xFF],
        "screen x1 -> world x1"
    );
    assert_eq!(
        pixel_at(&fb, 2, 0),
        [0, 0, 0xFF, 0xFF],
        "screen x2 -> world x2"
    );
}

#[test]
fn mode7_hflip_mirrors_world_x() {
    let (mut bus, mut fb) = mode7_setup();
    // M7SEL bit 0 = hflip; red character pixel at world (0,1), the
    // row scanline 1 samples -> vram[8] high byte.
    set_m7sel(&mut bus, 0x01);
    write_vram_word(&mut bus, 0x0008, 0x4200);
    write_cgram(&mut bus, 0x42, bgr15(31, 0, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);
    // With hflip, screen x=255 samples world x=0 (the red origin);
    // screen x=0 samples world x=255 (empty -> backdrop).
    assert_eq!(
        pixel_at(&fb, 255, 0),
        [0xFF, 0, 0, 0xFF],
        "hflip puts the origin at the right edge"
    );
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0, 0, 0, 0xFF],
        "screen x0 samples world x255 (empty)"
    );
}

#[test]
fn mode7_vflip_mirrors_world_y() {
    let (mut bus, mut fb) = mode7_setup();
    // Red character pixel at world (0,1), the row scanline 1
    // samples without vflip (vram[8] high byte, tile 0 cy=1).
    write_vram_word(&mut bus, 0x0008, 0x4200);
    write_cgram(&mut bus, 0x42, bgr15(31, 0, 0));

    // No vflip: scanline 1 -> world y 1 (the line counter is 1 on the
    // first visible line) shows the pixel.
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0xFF, 0, 0, 0xFF],
        "no vflip: pixel at host row 0"
    );

    // vflip mirrors the line around 255, so scanline 1 now samples
    // world y=254 (empty) and the pixel disappears from row 0.
    set_m7sel(&mut bus, 0x02); // bit 1 = vflip
    let mut fb2 = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb2);
    assert_eq!(
        pixel_at(&fb2, 0, 0),
        [0, 0, 0, 0xFF],
        "vflip: host row 0 samples world y254 (empty)"
    );
}

#[test]
fn mode7_screen_over_transparent_mode2() {
    let (mut bus, mut fb) = mode7_setup();
    install_mode7_screen_over_layout(&mut bus);
    set_m7sel(&mut bus, 0x80); // repeat = 2 (bits 7-6 = 10)

    bus.ppu_mut().render_scanline(1, &mut fb);
    // Screen x=0 maps outside -> palette 0 / transparent -> backdrop.
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0, 0, 0, 0xFF],
        "repeat 2 outside -> transparent backdrop"
    );
    // Screen x=1 maps to world (0,1) -> tile 0 colour 1 = red.
    assert_eq!(pixel_at(&fb, 1, 0), [0xFF, 0, 0, 0xFF], "inside -> red");
}

#[test]
fn mode7_screen_over_wrap_mode0() {
    let (mut bus, mut fb) = mode7_setup();
    install_mode7_screen_over_layout(&mut bus);
    set_m7sel(&mut bus, 0x00); // repeat = 0 (wrap)

    bus.ppu_mut().render_scanline(1, &mut fb);
    // Screen x=0 (px=-1) wraps to world x=1023 -> tile 1 colour 2.
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0, 0xFF, 0, 0xFF],
        "repeat 0 wraps to tile 1 (green)"
    );
}

#[test]
fn mode7_screen_over_tile0_mode3() {
    let (mut bus, mut fb) = mode7_setup();
    install_mode7_screen_over_layout(&mut bus);
    set_m7sel(&mut bus, 0xC0); // repeat = 3 (bits 7-6 = 11)

    bus.ppu_mut().render_scanline(1, &mut fb);
    // Screen x=0 (px=-1) outside -> forced tile 0, character pixel
    // (cx=7, cy=1) -> tile 0 colour 1 = red (NOT the wrap's green).
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0xFF, 0, 0, 0xFF],
        "repeat 3 forces tile 0 (red)"
    );
}

#[test]
fn mode7_extbg_off_bg1_uses_full_256_colors() {
    let (mut bus, mut fb) = mode7_setup();
    set_extbg(&mut bus, false);
    // Scanline 1 samples world (0,1) -> tile 0 pixel at vram[8].
    // Character byte 0x81 (bit 7 set). Without EXTBG, BG1 treats it
    // as a full 8-bit index -> CGRAM[0x81].
    write_vram_word(&mut bus, 0x0008, 0x8100);
    write_cgram(&mut bus, 0x81, bgr15(31, 0, 0));
    // Poison the would-be EXTBG split index so a regression shows.
    write_cgram(&mut bus, 0x01, bgr15(0, 31, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0xFF, 0, 0, 0xFF],
        "BG1 uses the full 0x81 index, not the split 0x01"
    );
}

#[test]
fn mode7_extbg_bg2_high_priority_renders_above_bg1() {
    let (mut bus, mut fb) = mode7_setup();
    set_extbg(&mut bus, true);
    set_main_enable(&mut bus, 0x03);
    // Both BG1 and BG2 enabled on the main screen. Scanline 1
    // samples world (0,1) -> tile 0 pixel at vram[8]. Character byte
    // 0x81 has bit 7 set -> BG2 high priority, index 1:
    //   BG1 sees the full 0x81 -> CGRAM[0x81] (red).
    //   BG2 sees index 1 at high priority -> CGRAM[1] (green).
    // Priority order BG2-high > BG1 -> green wins.
    write_vram_word(&mut bus, 0x0008, 0x8100);
    write_cgram(&mut bus, 0x81, bgr15(31, 0, 0));
    write_cgram(&mut bus, 0x01, bgr15(0, 31, 0));

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0, 0xFF, 0, 0xFF],
        "EXTBG BG2 high-priority pixel sits above BG1"
    );
}

#[test]
fn mode7_direct_color_bg1_decodes_pixel_directly() {
    let (mut bus, mut fb) = mode7_setup();
    // CGWSEL bit 0 enables direct colour. Scanline 1 samples world
    // (0,1) -> tile 0 pixel at vram[8]. Character byte 0x07 then
    // decodes directly (matches the Mode 3 direct-colour test: the
    // R channel expands to 0xE7).
    set_cgwsel(&mut bus, true);
    write_vram_word(&mut bus, 0x0008, 0x0700);
    // Poison CGRAM[0x07] so an accidental palette lookup shows.
    write_cgram(&mut bus, 0x07, bgr15(0, 0, 31));

    bus.ppu_mut().render_scanline(1, &mut fb);
    let p = pixel_at(&fb, 0, 0);
    assert_eq!(p[0], 0xE7, "direct-colour red channel");
    assert_eq!(p[1], 0, "no green");
    assert_eq!(p[2], 0, "no blue");
    assert_eq!(p[3], 0xFF);
}

#[test]
fn mode7_active_display_size_is_256_by_224() {
    let (mut bus, _fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 7, false);
    // Mode 7 is non-hires -> 256 wide, 224 tall by default.
    assert_eq!(bus.ppu().active_display_size(), (256, 224));
    // Overscan stays cropped to 224 by default; width stays 256.
    set_setini(&mut bus, 0x04);
    assert_eq!(bus.ppu().active_display_size(), (256, 224));
    // The toggle exposes the full 240-line frame.
    bus.ppu_mut().set_show_overscan(true);
    assert_eq!(bus.ppu().active_display_size(), (256, 240));
}

// -------------------------------------------------------------
// Tilemap-row cache, parity + invalidation through the bus
// -------------------------------------------------------------

#[test]
fn tilemap_cache_static_band_uniform_across_scanlines() {
    // A static Mode 1 screen: tile 0 is uniformly colour index 1, the
    // tilemap is all zeros (every column = tile 0). The whole first
    // tile band (scanlines 1..=8 -> host rows 0..=7) must be red. Lines
    // 2..8 are served from the tilemap row the cache built on line 1,
    // so this proves the cached entries render identically to the
    // freshly-built row.
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);

    // Tile 0 (4bpp): plane0 = 0xFF on every row -> pixel index 1.
    for row in 0u16..8 {
        write_vram_word(&mut bus, row, 0x00FF);
    }
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));

    for line in 1u16..=8 {
        bus.ppu_mut().render_scanline(line, &mut fb);
    }
    for row in 0u32..8 {
        assert_eq!(
            pixel_at(&fb, 0, row),
            [0xFF, 0, 0, 0xFF],
            "band row {row} col 0 must be red"
        );
        assert_eq!(
            pixel_at(&fb, 200, row),
            [0xFF, 0, 0, 0xFF],
            "band row {row} col 200 must be red"
        );
    }
}

#[test]
fn tilemap_cache_vblank_vram_change_reflected_on_next_render() {
    // Render once to warm the cache, then rewrite the tilemap entry
    // during VBlank (the fresh snapshot parks at scanline 225, so the
    // write lands and bumps the VRAM generation). The next render of
    // the same scanline must reflect the new tile, proving the
    // generation bump invalidates the cached row end-to-end.
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 1, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);

    // Tile 1 -> pixel index 1 (red); tile 2 -> pixel index 2 (green).
    for row in 0u16..8 {
        write_vram_word(&mut bus, 16 + row, 0x00FF); // tile 1, plane0
        write_vram_word(&mut bus, 32 + row, 0xFF00); // tile 2, plane1
    }
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));
    write_cgram(&mut bus, 2, bgr15(0, 31, 0));

    // Tilemap (0,0) = tile 1.
    write_vram_word(&mut bus, 0x0400, 0x0001);
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0xFF, 0, 0, 0xFF],
        "first render shows tile 1 (red)"
    );

    // Repoint tilemap (0,0) to tile 2 during VBlank -> generation bump.
    write_vram_word(&mut bus, 0x0400, 0x0002);
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0, 0xFF, 0, 0xFF],
        "VRAM change must invalidate the cache -> tile 2 (green)"
    );
}

#[test]
fn tilemap_cache_offset_mode_render_unchanged() {
    // Mode 2 (offset-per-tile) bypasses the tilemap-row cache. The
    // first tile column always uses the base scroll (no BG3 lookup),
    // so its pixel must still resolve correctly through the direct
    // fetch path.
    let (mut bus, mut fb) = fresh_test_setup(15);
    set_bgmode(&mut bus, 2, false);
    install_standard_vram_layout(&mut bus);
    set_main_enable(&mut bus, 0x01);

    // Tile 1 (4bpp): plane0 = 0xFF -> pixel index 1.
    for row in 0u16..8 {
        write_vram_word(&mut bus, 16 + row, 0x00FF);
    }
    write_cgram(&mut bus, 1, bgr15(31, 0, 0));
    write_vram_word(&mut bus, 0x0400, 0x0001); // tilemap (0,0) = tile 1

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        [0xFF, 0, 0, 0xFF],
        "offset-mode BG1 column 0 renders via the direct fetch path"
    );
}
