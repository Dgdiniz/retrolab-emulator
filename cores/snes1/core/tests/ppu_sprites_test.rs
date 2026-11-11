//! End-to-end PPU sprite (OBJ) rendering tests.
//!
//! Every scenario programs the PPU through the full A-bus -> B-bus ->
//! `Ppu::write_reg` chain (OBSEL, OAM low/high tables, sprite
//! character data, palettes starting at CGRAM 128) and then drives
//! [`Ppu::render_scanline`] with a stand-in framebuffer, inspecting
//! the resulting RGBA8 pixels and the STAT77 (`$213E`) overflow flags.
//!
//! Unit-level coverage of the pure helpers lives in `ppu/sprite.rs`
//! and `ppu/render.rs`; the cases here pin the seams those modules
//! can't reach on their own, register decode, OAM streaming, the
//! one-line OBJ Y latency, the per-mode OBJ/BG priority interleave,
//! the 32-sprite / 34-tile overflow flags, and the decoded-tile cache
//! across scanlines.
//!
//! ## Sprite coordinate convention
//!
//! A sprite with OAM Y = N lands with its top row at output row N
//! (the renderer compares against `scanline - 1`, absorbing the
//! one-line OBJ pipeline latency since `scanline` is the 1-indexed
//! vcounter). So an OAM Y = 0 sprite occupies host rows 0..height-1,
//! rendered by scanlines 1..height.

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

/// Expected RGBA8 for a `bgr15(r5,g5,b5)` colour at full brightness
/// (the 5->8 expansion the renderer applies, with brightness 15 = no
/// attenuation).
fn rgba_full(r5: u8, g5: u8, b5: u8) -> [u8; 4] {
    let e = |c5: u8| -> u8 {
        let c5 = c5 & 0x1F;
        (c5 << 3) | (c5 >> 2)
    };
    [e(r5), e(g5), e(b5), 0xFF]
}

// -------------------------------------------------------------
// PPU / OAM / VRAM programming helpers
// -------------------------------------------------------------

/// Fresh bus parked in VBlank (so setup writes pass the access gate)
/// with the display enabled at full brightness.
fn sprite_setup() -> (SnesBus, Framebuffer) {
    let mut bus = SnesBus::new();
    bus.set_raster_snapshot(0, 225);
    bus.write(ppu_addr(0x2100), 0x0F);
    (bus, fresh_fb())
}

fn write_vram_word(bus: &mut SnesBus, word_addr: u16, value: u16) {
    bus.write(ppu_addr(0x2115), 0x80);
    bus.write(ppu_addr(0x2116), (word_addr & 0xFF) as u8);
    bus.write(ppu_addr(0x2117), ((word_addr >> 8) & 0xFF) as u8);
    bus.write(ppu_addr(0x2118), (value & 0xFF) as u8);
    bus.write(ppu_addr(0x2119), ((value >> 8) & 0xFF) as u8);
}

fn write_cgram(bus: &mut SnesBus, color_index: u8, color: u16) {
    bus.write(ppu_addr(0x2121), color_index);
    bus.write(ppu_addr(0x2122), (color & 0xFF) as u8);
    bus.write(ppu_addr(0x2122), ((color >> 8) & 0x7F) as u8);
}

/// OBSEL ($2101): size selector (bits 5-7), name-select (bits 3-4),
/// tile-data base (bits 0-2, each step = 0x4000 bytes of VRAM).
fn set_obsel(bus: &mut SnesBus, size_sel: u8, name_sel: u8, base: u8) {
    let v = ((size_sel & 0x07) << 5) | ((name_sel & 0x03) << 3) | (base & 0x07);
    bus.write(ppu_addr(0x2101), v);
}

/// Park the OAM byte pointer at `byte_addr` (must be word-aligned for
/// the low table; for the high table pass `0x200` and stream bytes).
fn set_oam_addr(bus: &mut SnesBus, byte_addr: u16) {
    bus.write(ppu_addr(0x2103), ((byte_addr >> 9) & 0x01) as u8);
    bus.write(ppu_addr(0x2102), ((byte_addr >> 1) & 0xFF) as u8);
}

/// Program sprite `idx`'s four low-table bytes.
fn write_oam_sprite(bus: &mut SnesBus, idx: u16, x: u8, y: u8, character: u8, attr: u8) {
    set_oam_addr(bus, idx * 4);
    bus.write(ppu_addr(0x2104), x);
    bus.write(ppu_addr(0x2104), y);
    bus.write(ppu_addr(0x2104), character);
    bus.write(ppu_addr(0x2104), attr);
}

/// Stream the high table (32 bytes) starting at OAM byte 0x200.
fn write_oam_high_table(bus: &mut SnesBus, bytes: &[u8]) {
    set_oam_addr(bus, 0x200);
    for &b in bytes {
        bus.write(ppu_addr(0x2104), b);
    }
}

/// Park all 128 sprites off the visible region (Y = 240) so a test
/// only sees the sprites it explicitly programs afterwards.
fn park_all_sprites(bus: &mut SnesBus) {
    set_oam_addr(bus, 0);
    for _ in 0..128 {
        bus.write(ppu_addr(0x2104), 0x00); // X
        bus.write(ppu_addr(0x2104), 0xF0); // Y = 240
        bus.write(ppu_addr(0x2104), 0x00); // character
        bus.write(ppu_addr(0x2104), 0x00); // attribute
    }
}

/// Write a solid 4bpp tile (all 64 pixels = `index`) at VRAM tile slot
/// `tile` (word base `tile * 16`).
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

/// Write one 4bpp tile row (`index` across all 8 columns).
fn write_obj_tile_row(bus: &mut SnesBus, tile: u16, row: u16, index: u8) {
    let base = tile.wrapping_mul(16);
    let plane = |bit: u8| -> u16 {
        if index & bit != 0 {
            0x00FF
        } else {
            0x0000
        }
    };
    write_vram_word(bus, base + row, plane(1) | (plane(2) << 8));
    write_vram_word(bus, base + 8 + row, plane(4) | (plane(8) << 8));
}

fn set_bg_voffset(bus: &mut SnesBus, bg_id: u8, value: u16) {
    let reg = 0x210E + u16::from(bg_id) * 2;
    bus.write(ppu_addr(reg), (value & 0xFF) as u8);
    bus.write(ppu_addr(reg), ((value >> 8) & 0xFF) as u8);
}

fn read_stat77(bus: &mut SnesBus) -> u8 {
    bus.read(ppu_addr(0x213E))
}

// -------------------------------------------------------------
// Core cases
// -------------------------------------------------------------

#[test]
fn single_sprite_8x8_at_origin() {
    let (mut bus, mut fb) = sprite_setup();
    park_all_sprites(&mut bus);
    set_obsel(&mut bus, 0, 0, 0); // 8x8, tile data at word 0
    write_solid_obj_tile(&mut bus, 0, 1); // tile 0 solid index 1
    write_cgram(&mut bus, 129, bgr15(31, 0, 0)); // sprite pal 0, idx 1 = red
    write_oam_sprite(&mut bus, 0, 0, 0, 0, 0x00); // (0,0), char 0, prio 0
    bus.write(ppu_addr(0x212C), 0x10); // OBJ main enable

    // OAM Y = 0 -> host rows 0..7, drawn by scanlines 1..8.
    for line in 1..=9u16 {
        bus.ppu_mut().render_scanline(line, &mut fb);
    }
    for y in 0..8u32 {
        assert_eq!(pixel_at(&fb, 0, y), rgba_full(31, 0, 0), "sprite row {y}");
        assert_eq!(
            pixel_at(&fb, 7, y),
            rgba_full(31, 0, 0),
            "sprite col 7 row {y}"
        );
    }
    // Column 8 is past the 8-px sprite -> backdrop (CGRAM 0 = black).
    assert_eq!(pixel_at(&fb, 8, 0), [0, 0, 0, 0xFF]);
    // Row 8 (scanline 9) is past the sprite bottom -> backdrop.
    assert_eq!(pixel_at(&fb, 0, 8), [0, 0, 0, 0xFF]);
}

#[test]
fn sprite_horizontal_flip() {
    let (mut bus, mut fb) = sprite_setup();
    park_all_sprites(&mut bus);
    set_obsel(&mut bus, 0, 0, 0);
    // Tile 0 row pattern: column 0 = index 1, columns 1..7 = index 2.
    // plane0 (bit0) = 0x80 (col 0), plane1 (bit1) = 0x7F (cols 1..7).
    for row in 0..8u16 {
        write_vram_word(&mut bus, row, 0x80 | (0x7F << 8));
        write_vram_word(&mut bus, 8 + row, 0x0000);
    }
    write_cgram(&mut bus, 129, bgr15(31, 0, 0)); // index 1 = red
    write_cgram(&mut bus, 130, bgr15(0, 31, 0)); // index 2 = green
    bus.write(ppu_addr(0x212C), 0x10);

    // No flip: col 0 red, col 7 green.
    write_oam_sprite(&mut bus, 0, 0, 0, 0, 0x00);
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(pixel_at(&fb, 0, 0), rgba_full(31, 0, 0));
    assert_eq!(pixel_at(&fb, 7, 0), rgba_full(0, 31, 0));

    // Hflip (attr bit 6): the marker column moves to col 7.
    let mut fb2 = fresh_fb();
    write_oam_sprite(&mut bus, 0, 0, 0, 0, 0x40);
    bus.ppu_mut().render_scanline(1, &mut fb2);
    assert_eq!(
        pixel_at(&fb2, 7, 0),
        rgba_full(31, 0, 0),
        "hflip moves col0->col7"
    );
    assert_eq!(pixel_at(&fb2, 0, 0), rgba_full(0, 31, 0));
}

#[test]
fn sprite_palette_select() {
    let (mut bus, mut fb) = sprite_setup();
    park_all_sprites(&mut bus);
    set_obsel(&mut bus, 0, 0, 0);
    write_solid_obj_tile(&mut bus, 0, 1); // index 1 in every sprite's palette
                                          // Palette 0 index 1 -> CGRAM 129 = red; palette 3 index 1 -> CGRAM
                                          // 128 + 48 + 1 = 177 = blue.
    write_cgram(&mut bus, 129, bgr15(31, 0, 0));
    write_cgram(&mut bus, 177, bgr15(0, 0, 31));
    write_oam_sprite(&mut bus, 0, 0, 0, 0, 0x00); // palette 0
    write_oam_sprite(&mut bus, 1, 16, 0, 0, 0x06); // palette 3 (bits 1-3)
    bus.write(ppu_addr(0x212C), 0x10);

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        rgba_full(31, 0, 0),
        "sprite 0 palette 0"
    );
    assert_eq!(
        pixel_at(&fb, 16, 0),
        rgba_full(0, 0, 31),
        "sprite 1 palette 3"
    );
}

#[test]
fn sprite_priority_over_bg() {
    // Mode 1: a full-screen BG1 (low priority) with one sprite on top.
    // OBJ priority 2/3 sit above BG1-low; OBJ 0 sits below it.
    let setup = || -> SnesBus {
        let mut bus = SnesBus::new();
        bus.set_raster_snapshot(0, 225);
        bus.write(ppu_addr(0x2100), 0x0F);
        park_all_sprites(&mut bus);
        bus.write(ppu_addr(0x2105), 0x01); // Mode 1
        bus.write(ppu_addr(0x2107), 0x04); // BG1 tilemap @ word 0x400
        bus.write(ppu_addr(0x210B), 0x00); // BG1/2 tile data @ word 0
        set_bg_voffset(&mut bus, 0, 0xFFFF); // scanline 1 -> tile row 0
                                             // BG1 tile 1 = solid index 1 (blue), tilemap (0,0) -> tile 1.
        write_solid_obj_tile(&mut bus, 1, 1);
        write_vram_word(&mut bus, 0x0400, 0x0001);
        write_cgram(&mut bus, 1, bgr15(0, 0, 31)); // BG = blue

        // Sprite tile data lives in a separate VRAM page (OBSEL base 1
        // -> byte 0x4000 -> char 0 resolves to tile 512 / word 0x2000).
        set_obsel(&mut bus, 0, 0, 1);
        write_solid_obj_tile(&mut bus, 512, 1);
        write_cgram(&mut bus, 129, bgr15(31, 0, 0)); // sprite = red
        bus.write(ppu_addr(0x212C), 0x11); // BG1 + OBJ main enable
        bus
    };

    // Sprite priority 3 -> above BG1-low -> red wins.
    let mut bus = setup();
    write_oam_sprite(&mut bus, 0, 0, 0, 0, 0x30); // priority 3
    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        rgba_full(31, 0, 0),
        "OBJ3 beats BG1-low"
    );

    // Sprite priority 0 -> below BG1-low -> BG (blue) wins.
    let mut bus = setup();
    write_oam_sprite(&mut bus, 0, 0, 0, 0, 0x00); // priority 0
    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        rgba_full(0, 0, 31),
        "OBJ0 loses to BG1-low"
    );
}

#[test]
fn range_overflow_flag() {
    let (mut bus, mut fb) = sprite_setup();
    park_all_sprites(&mut bus);
    set_obsel(&mut bus, 0, 0, 0);
    write_solid_obj_tile(&mut bus, 0, 1);
    write_cgram(&mut bus, 129, bgr15(31, 0, 0));
    // 33 sprites all on line 0 -> the 33rd trips range-over.
    for i in 0..33u16 {
        write_oam_sprite(&mut bus, i, 0, 0, 0, 0x00);
    }
    bus.write(ppu_addr(0x212C), 0x10);

    bus.ppu_mut().render_scanline(1, &mut fb);
    let stat = read_stat77(&mut bus);
    assert_ne!(
        stat & 0x40,
        0,
        "STAT77 bit 6 (range over) set by 33 sprites"
    );
    assert_eq!(
        stat & 0x80,
        0,
        "32 single-tile sprites stay under the 34-tile cap"
    );
}

#[test]
fn tile_overflow_flag() {
    let (mut bus, mut fb) = sprite_setup();
    park_all_sprites(&mut bus);
    // Size selector 2: large = 64x64 (8 tile columns). Five large
    // sprites overlapping at x=0 -> 40 tile columns > 34.
    set_obsel(&mut bus, 2, 0, 0);
    write_oam_high_table(&mut bus, &[0xAA, 0x02]); // sprites 0-4 large
    for i in 0..5u16 {
        write_oam_sprite(&mut bus, i, 0, 0, 0, 0x00);
    }
    bus.write(ppu_addr(0x212C), 0x10);

    bus.ppu_mut().render_scanline(1, &mut fb);
    let stat = read_stat77(&mut bus);
    assert_ne!(stat & 0x80, 0, "STAT77 bit 7 (time over) set by >34 tiles");
}

#[test]
fn oam_rotation_affects_priority() {
    // Two overlapping same-priority sprites; whichever the evaluator
    // starts at becomes the highest-priority sprite and wins.
    let build = || -> SnesBus {
        let mut bus = SnesBus::new();
        bus.set_raster_snapshot(0, 225);
        bus.write(ppu_addr(0x2100), 0x0F);
        park_all_sprites(&mut bus);
        set_obsel(&mut bus, 0, 0, 0);
        write_solid_obj_tile(&mut bus, 0, 1);
        write_cgram(&mut bus, 129, bgr15(31, 0, 0)); // sprite 0: pal 0 = red
        write_cgram(&mut bus, 145, bgr15(0, 31, 0)); // sprite 1: pal 1 = green
        write_oam_sprite(&mut bus, 0, 0, 0, 0, 0x00); // pal 0
        write_oam_sprite(&mut bus, 1, 0, 0, 0, 0x02); // pal 1 (bits 1-3)
        bus.write(ppu_addr(0x212C), 0x10);
        bus
    };

    // No rotation: sprite 0 is first -> highest -> red wins.
    let mut bus = build();
    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        rgba_full(31, 0, 0),
        "sprite 0 wins without rotation"
    );

    // Rotation start = sprite 1 (baseaddr >> 2 = 1): $2103 bit 7 +
    // baseaddr 4 ($2102 = 2). Written last so it doesn't disturb the
    // OAM data writes above.
    let mut bus = build();
    bus.write(ppu_addr(0x2103), 0x80);
    bus.write(ppu_addr(0x2102), 0x02);
    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        rgba_full(0, 31, 0),
        "rotation lifts sprite 1"
    );
}

// -------------------------------------------------------------
// Extra coverage: flag clear, tall vflip, cache, Mode 7 OBJ
// -------------------------------------------------------------

#[test]
fn overflow_flags_clear_at_frame_top() {
    let (mut bus, mut fb) = sprite_setup();
    park_all_sprites(&mut bus);
    set_obsel(&mut bus, 0, 0, 0);
    write_solid_obj_tile(&mut bus, 0, 1);
    write_cgram(&mut bus, 129, bgr15(31, 0, 0));
    for i in 0..33u16 {
        write_oam_sprite(&mut bus, i, 0, 0, 0, 0x00);
    }
    bus.write(ppu_addr(0x212C), 0x10);

    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_ne!(read_stat77(&mut bus) & 0x40, 0, "range over set this frame");

    // Remove the overflow (leave only sprite 0 on the line), then
    // render the next frame's top scanline: the frame-top reset drops
    // last frame's flag before the (now non-overflowing) eval.
    for i in 1..33u16 {
        write_oam_sprite(&mut bus, i, 0, 0xF0, 0, 0x00);
    }
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        read_stat77(&mut bus) & 0x40,
        0,
        "frame top cleared the stale flag"
    );
}

#[test]
fn tall_sprite_vertical_flip() {
    let (mut bus, mut fb) = sprite_setup();
    park_all_sprites(&mut bus);
    set_obsel(&mut bus, 3, 0, 0); // selector 3 small = 16x16, no large bit
                                  // 16x16 char 0 occupies tiles 0 (top-left) and 16 (bottom-left).
    write_solid_obj_tile(&mut bus, 0, 1); // top = index 1 (red)
    write_solid_obj_tile(&mut bus, 16, 2); // bottom = index 2 (green)
    write_cgram(&mut bus, 129, bgr15(31, 0, 0));
    write_cgram(&mut bus, 130, bgr15(0, 31, 0));
    bus.write(ppu_addr(0x212C), 0x10);

    // No vflip: top row shows the top tile (red).
    write_oam_sprite(&mut bus, 0, 0, 0, 0, 0x00);
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        rgba_full(31, 0, 0),
        "top row = top tile"
    );

    // Vflip (attr bit 7): the top row now shows the bottom tile (green).
    let mut fb2 = fresh_fb();
    write_oam_sprite(&mut bus, 0, 0, 0, 0, 0x80);
    bus.ppu_mut().render_scanline(1, &mut fb2);
    assert_eq!(
        pixel_at(&fb2, 0, 0),
        rgba_full(0, 31, 0),
        "vflip lifts the bottom tile"
    );
}

#[test]
fn cache_coherent_across_scanlines() {
    let (mut bus, mut fb) = sprite_setup();
    park_all_sprites(&mut bus);
    set_obsel(&mut bus, 0, 0, 0);
    // Tile 0: row r carries colour index r+1 (1..8), so each of the
    // sprite's eight scanlines must read a distinct decoded row.
    for r in 0..8u16 {
        write_obj_tile_row(&mut bus, 0, r, (r + 1) as u8);
        write_cgram(&mut bus, 129 + r as u8, bgr15((r + 1) as u8, 0, 0));
    }
    write_oam_sprite(&mut bus, 0, 0, 0, 0, 0x00);
    bus.write(ppu_addr(0x212C), 0x10);

    // First pass populates the decoded-tile cache; each host row shows
    // its own colour.
    for line in 1..=8u16 {
        bus.ppu_mut().render_scanline(line, &mut fb);
    }
    for r in 0..8u32 {
        let idx = (r + 1) as u8;
        assert_eq!(pixel_at(&fb, 0, r), rgba_full(idx, 0, 0), "row {r} colour");
    }

    // Second pass renders entirely from the cache (generation frozen,
    // no VRAM writes) and must reproduce the identical output.
    let mut fb2 = fresh_fb();
    for line in 1..=8u16 {
        bus.ppu_mut().render_scanline(line, &mut fb2);
    }
    for r in 0..8u32 {
        let idx = (r + 1) as u8;
        assert_eq!(pixel_at(&fb2, 0, r), rgba_full(idx, 0, 0), "cached row {r}");
    }
}

#[test]
fn obj_over_mode7_bg1() {
    // Mode 7: OBJ priority 1-3 sit above BG1, OBJ 0 below.
    let build = || -> SnesBus {
        let mut bus = SnesBus::new();
        bus.set_raster_snapshot(0, 225);
        bus.write(ppu_addr(0x2100), 0x0F);
        park_all_sprites(&mut bus);
        bus.write(ppu_addr(0x2105), 0x07); // Mode 7
                                           // Identity matrix (A = D = 1.0 in 8.8 fixed point).
        bus.write(ppu_addr(0x211B), 0x00);
        bus.write(ppu_addr(0x211B), 0x01);
        bus.write(ppu_addr(0x211E), 0x00);
        bus.write(ppu_addr(0x211E), 0x01);
        // Scanline 1 samples world (0,1): char pixel at vram[8] high
        // byte = colour index 0x21; tilemap[0] low byte = tile 0.
        write_vram_word(&mut bus, 8, 0x21 << 8);
        write_cgram(&mut bus, 0x21, bgr15(0, 0, 31)); // BG = blue

        // Sprite tile data in its own page (OBSEL base 1 -> tile 512).
        set_obsel(&mut bus, 0, 0, 1);
        write_solid_obj_tile(&mut bus, 512, 1);
        write_cgram(&mut bus, 129, bgr15(31, 0, 0)); // sprite = red
        bus.write(ppu_addr(0x212C), 0x11); // BG1 + OBJ main enable
        bus
    };

    // Sprite priority 1 -> above Mode 7 BG1 -> red.
    let mut bus = build();
    write_oam_sprite(&mut bus, 0, 0, 0, 0, 0x10);
    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        rgba_full(31, 0, 0),
        "OBJ1 above Mode 7 BG1"
    );

    // Sprite priority 0 -> below BG1 -> blue.
    let mut bus = build();
    write_oam_sprite(&mut bus, 0, 0, 0, 0, 0x00);
    let mut fb = fresh_fb();
    bus.ppu_mut().render_scanline(1, &mut fb);
    assert_eq!(
        pixel_at(&fb, 0, 0),
        rgba_full(0, 0, 31),
        "OBJ0 below Mode 7 BG1"
    );
}
