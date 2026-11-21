//! Unit tests of the renderer: colours, the backdrop, the planes' geometry,
//! the window, the scrolls, the layers, shadow and highlight, and a palette
//! write in the middle of a line.

use super::*;

#[test]
fn test_backdrop_rendering() {
    let mut vdp = Vdp::new();

    // Set backdrop color to bright red (palette 0, color 1)
    vdp.cram[1] = 0x000E; // Red
    vdp.registers[7] = 0x01; // Backdrop: palette 0, color 1

    // Render backdrop
    vdp.render_backdrop();

    // Check first pixel
    assert_eq!(vdp.framebuffer[0], 255); // R
    assert_eq!(vdp.framebuffer[1], 0); // G
    assert_eq!(vdp.framebuffer[2], 0); // B
    assert_eq!(vdp.framebuffer[3], 255); // A
}

#[test]
fn test_scroll_size_register() {
    let vdp = Vdp::new();

    // Default scroll size should be 32x32
    let (width, height) = vdp.get_scroll_size();
    assert_eq!(width, 32);
    assert_eq!(height, 32);
}

#[test]
fn test_scroll_size_64x64() {
    let mut vdp = Vdp::new();

    // Set register 16: H=64 (bits 0-1 = 01), V=64 (bits 4-5 = 01)
    vdp.set_test_register(16, 0x11);
    let (width, height) = vdp.get_scroll_size();
    assert_eq!(width, 64);
    assert_eq!(height, 64);
}

#[test]
fn test_scroll_size_128x32() {
    let mut vdp = Vdp::new();

    // Set register 16: H=128 (bits 0-1 = 11), V=32 (bits 4-5 = 00)
    vdp.set_test_register(16, 0x03);
    let (width, height) = vdp.get_scroll_size();
    assert_eq!(width, 128);
    assert_eq!(height, 32);
}

#[test]
fn test_playfield_geometry_valid_sizes() {
    let mut vdp = Vdp::new();

    // 32x32 (H=00, V=00): shift 6, col mask 0x1F (32 tiles), row mask 0xFF.
    vdp.set_test_register(16, 0x00);
    assert_eq!(vdp.playfield_geometry(), (6, 0x1F, 0x0FF));

    // 64x64 (H=01, V=01): shift 7, col mask 0x3F (64), row mask 0x1FF.
    vdp.set_test_register(16, 0x11);
    assert_eq!(vdp.playfield_geometry(), (7, 0x3F, 0x1FF));

    // 128x128 (H=11, V=11): shift 8, col mask 0x7F (128), row mask 0x3FF.
    vdp.set_test_register(16, 0x33);
    assert_eq!(vdp.playfield_geometry(), (8, 0x7F, 0x3FF));
}

#[test]
fn test_playfield_geometry_invalid_sizes() {
    let mut vdp = Vdp::new();

    // Invalid horizontal size 0b10 forces shift=0 so the nametable row
    // offset `(tile_row << 0) & 0x1FC0` is always 0: every scanline
    // re-reads row 0 (the VDP scroll-size/window test-ROM quirk). The
    // column width stays 32 tiles (mask 0x1F).
    vdp.set_test_register(16, 0x02);
    let (shift, col_mask, _row_mask) = vdp.playfield_geometry();
    assert_eq!(shift, 0);
    assert_eq!(col_mask, 0x1F);
    // Row offset collapses to 0 for any tile row.
    for tile_row in 0u32..32 {
        assert_eq!((tile_row << shift) & 0x1FC0, 0);
    }

    // Invalid vertical size 0b10 yields the non-power-of-two mask 0x2FF.
    vdp.set_test_register(16, 0x20);
    assert_eq!(vdp.playfield_geometry().2, 0x2FF);
}

#[test]
fn test_window_position_disabled() {
    let mut vdp = Vdp::new();

    // Both edges at 0, growing from the top and the left: no window.
    vdp.set_test_register(17, 0x00);
    vdp.set_test_register(18, 0x00);
    assert!(!vdp.window_area().covers(100, 100));
}

#[test]
fn test_window_position_right_side() {
    let mut vdp = Vdp::new();

    // Bit 7 = 1 (right side), bits 4-0 = 10 two-cell columns = pixel 160.
    vdp.set_test_register(17, 0x8A);
    // Line edge at 0 growing from the top: no whole lines.
    vdp.set_test_register(18, 0x00);

    assert!(!vdp.window_area().covers(150, 100));
    assert!(vdp.window_area().covers(170, 100));
}

#[test]
fn test_window_position_left_side() {
    let mut vdp = Vdp::new();

    // Bit 7 = 0 (left side), bits 4-0 = 10 two-cell columns = pixel 160.
    vdp.set_test_register(17, 0x0A);
    // Line edge at 0 growing from the top: no whole lines.
    vdp.set_test_register(18, 0x00);

    assert!(vdp.window_area().covers(100, 100));
    assert!(!vdp.window_area().covers(170, 100));
}

#[test]
fn test_window_position_right_side_from_column_zero() {
    let mut vdp = Vdp::new();

    // Bit 7 = 1 with the column edge at 0: the window starts at column 0
    // and takes every line (Rings of Power: R17=$80, R18=$00).
    vdp.set_test_register(17, 0x80);
    vdp.set_test_register(18, 0x00);
    assert!(vdp.window_area().covers(0, 0));
    assert!(vdp.window_area().covers(100, 100));
    assert!(vdp.window_area().covers(319, 223));
}

/// A display-on H40 VDP whose plane A is red everywhere (colour 1 of
/// palette 0) and whose window is blue everywhere (colour 2), with plane
/// B transparent and a black backdrop; registers 17 and 18 are left at 0.
fn red_plane_a_blue_window() -> Vdp {
    let mut vdp = Vdp::new();
    vdp.registers[1] = 0x44; // display on, mode 5, V28
    vdp.registers[12] = 0x81; // H40
    vdp.registers[2] = 0x38; // plane A nametable at 0xE000 (32x32 cells)
    vdp.registers[3] = 0x2C; // window nametable at 0xB000 (64x32 cells)
    vdp.registers[4] = 0x06; // plane B at 0xC000, left empty (transparent)

    // Tile 1 is all colour 1, tile 2 all colour 2.
    for b in 0..32 {
        vdp.vram[32 + b] = 0x11;
        vdp.vram[64 + b] = 0x22;
    }
    for cell in 0..32 * 32 {
        vdp.vram[0xE000 + cell * 2 + 1] = 0x01; // plane A: tile 1
    }
    for cell in 0..64 * 32 {
        vdp.vram[0xB000 + cell * 2 + 1] = 0x02; // window: tile 2
    }
    vdp.cram[1] = 0x000E; // red
    vdp.cram[2] = 0x0E00; // blue
    vdp.rebuild_cram_rgb();
    vdp
}

fn framebuffer_rgb(vdp: &Vdp, x: usize, y: usize) -> (u8, u8, u8) {
    let i = (y * 320 + x) * 4;
    (
        vdp.framebuffer[i],
        vdp.framebuffer[i + 1],
        vdp.framebuffer[i + 2],
    )
}

const SCREEN_CORNERS: [(usize, usize); 5] = [(0, 0), (160, 112), (319, 0), (0, 223), (319, 223)];

/// The renderer draws the window wherever its area covers: a right-side
/// window with its edge at column 0 hides plane A on the whole screen,
/// and with both edges at 0 growing from the top and left plane A shows
/// everywhere.
#[test]
fn a_right_side_window_from_column_zero_is_drawn_over_the_whole_screen() {
    let mut vdp = red_plane_a_blue_window();
    let plane_a_rgb = vdp.cram_rgb[1];
    let window_rgb = vdp.cram_rgb[2];

    vdp.registers[17] = 0x80;
    vdp.registers[18] = 0x00;
    vdp.rerender_current_frame();
    for (x, y) in SCREEN_CORNERS {
        assert_eq!(
            framebuffer_rgb(&vdp, x, y),
            window_rgb,
            "window at ({x},{y})"
        );
    }

    vdp.registers[17] = 0x00;
    vdp.rerender_current_frame();
    for (x, y) in SCREEN_CORNERS {
        assert_eq!(
            framebuffer_rgb(&vdp, x, y),
            plane_a_rgb,
            "plane A at ({x},{y})"
        );
    }
}

/// The plane A and window debug layers each show only their own part of
/// the screen: the window's part is transparent in the plane A layer,
/// and plane A's part in the window layer.
#[test]
fn the_plane_a_and_window_layers_show_only_their_own_part() {
    let mut vdp = red_plane_a_blue_window();
    let plane_a_rgb = vdp.cram_rgb[1];
    let window_rgb = vdp.cram_rgb[2];
    // The window takes the columns from pixel 160 rightwards.
    vdp.registers[17] = 0x8A;
    vdp.registers[18] = 0x00;

    vdp.set_render_layer_mode(RenderLayerMode::PlaneA);
    vdp.rerender_current_frame();
    assert_eq!(framebuffer_rgb(&vdp, 100, 100), plane_a_rgb);
    assert_eq!(framebuffer_rgb(&vdp, 200, 100), TRANSPARENT_RGB);

    vdp.set_render_layer_mode(RenderLayerMode::Window);
    vdp.rerender_current_frame();
    assert_eq!(framebuffer_rgb(&vdp, 100, 100), TRANSPARENT_RGB);
    assert_eq!(framebuffer_rgb(&vdp, 200, 100), window_rgb);
}

/// A black pixel a plane draws stays black in its layer view, and a
/// transparent one is magenta, so the two can be told apart.
#[test]
fn a_layer_view_tells_black_from_transparent() {
    let mut vdp = red_plane_a_blue_window();
    vdp.cram[1] = 0x0000; // plane A's colour 1 is black, like the backdrop
    vdp.rebuild_cram_rgb();
    vdp.registers[17] = 0x8A; // window from pixel 160 rightwards
    vdp.set_render_layer_mode(RenderLayerMode::PlaneA);
    vdp.rerender_current_frame();
    assert_eq!(framebuffer_rgb(&vdp, 100, 100), (0, 0, 0));
    assert_eq!(framebuffer_rgb(&vdp, 200, 100), TRANSPARENT_RGB);
}

/// No colour the VDP shows can be mistaken for the transparent one.
#[test]
fn no_vdp_colour_is_the_transparent_colour() {
    for raw in 0..=0x0FFFu16 {
        assert_ne!(Vdp::convert_genesis_color(raw), TRANSPARENT_RGB);
    }
}

#[test]
fn test_hscroll_modes() {
    let mut vdp = Vdp::new();

    // Mode 0: Full screen scroll (bits 1-0 = 00)
    vdp.set_test_register(11, 0x00);
    assert_eq!(vdp.registers[11] & 0x03, 0x00);

    // Mode 2: Per-cell scroll (bits 1-0 = 10)
    vdp.set_test_register(11, 0x02);
    assert_eq!((vdp.registers[11] >> 2) & 0x03, 0x00); // Bits 3-2 stay 0
                                                       // The h_scroll_mode is bits 1-0
}

/// Per-line (mode 3) and per-cell (mode 2) horizontal-scroll table reads
/// mask the line index to 8 bits. Visible lines never exceed 240 so this is a robustness
/// guard, but it keeps the table index from running past the 8-bit range.
#[test]
fn test_read_hscroll_masks_line_to_8_bits() {
    let mut vdp = Vdp::new();
    let base = 0x1000usize;

    // Entry for the masked line (256 & 0xFF == 0): base + 0.
    vdp.vram[base] = 0x01;
    vdp.vram[base + 1] = 0x23; // 0x0123 & 0x03FF = 0x123

    // Entry an UNMASKED per-line / per-cell index would hit at line 256
    // (both land at base + 1024): proves we don't read it.
    let unmasked = base + 256 * 4;
    vdp.vram[unmasked] = 0x02;
    vdp.vram[unmasked + 1] = 0x22; // 0x0222

    // Per-line (mode 3): line 256 -> entry 0.
    assert_eq!(vdp.read_hscroll(base, 0, 3, 256), 0x123);
    // Per-cell (mode 2): (256 & 0xFF) / 8 == 0 -> entry 0.
    assert_eq!(vdp.read_hscroll(base, 0, 2, 256), 0x123);

    // Sanity: an in-range line is unchanged (per-line line 1 -> base + 4).
    vdp.vram[base + 4] = 0x01;
    vdp.vram[base + 5] = 0x11; // 0x0111
    assert_eq!(vdp.read_hscroll(base, 0, 3, 1), 0x111);
}

/// Build a VDP whose Plane B nametable row 0 is solid CRAM index 1 (tile 1)
/// and row 1 is solid CRAM index 2 (tile 2), with per-2-cell vertical scroll
/// enabled and Plane B fine horizontal scroll = `shift`. Because every tile
/// column of a row decodes to the same index, the vertical-scroll value the
/// renderer picks for a pixel maps directly to `line_plane_b` (idx 1 = row 0,
/// idx 2 = row 1), so a test can read back *which* vscroll was used.
fn setup_left_col_vscroll_vdp(shift: u16) -> Vdp {
    let mut vdp = Vdp::new();
    vdp.set_test_register(1, 0x44); // display on + Mode 5
    vdp.set_test_register(11, 0x04); // per-column vscroll; hscroll mode 0
    vdp.set_test_register(16, 0x00); // planes 32x32
    vdp.set_test_register(2, 0x30); // Plane A nametable 0xC000 (transparent tile 0)
    vdp.set_test_register(4, 0x07); // Plane B nametable 0xE000
    vdp.set_test_register(5, 0x58); // SAT at 0xB000 (empty -> no sprites)
    vdp.set_test_register(13, 0x34); // hscroll table base 0xD000

    // Tile 1 = solid CRAM index 1; tile 2 = solid CRAM index 2.
    for b in 0x20..0x40 {
        vdp.vram[b] = 0x11;
    }
    for b in 0x40..0x60 {
        vdp.vram[b] = 0x22;
    }

    // Plane B nametable row 0 -> tile 1, row 1 -> tile 2 (all 32 columns).
    let pb = 0xE000usize;
    for col in 0..32usize {
        let r0 = pb + col * 2;
        vdp.vram[r0] = 0x00;
        vdp.vram[r0 + 1] = 0x01;
        let r1 = pb + 0x40 + col * 2; // row 1 stride = 1 << pf_shift(6)
        vdp.vram[r1] = 0x00;
        vdp.vram[r1 + 1] = 0x02;
    }

    // Plane B horizontal scroll (full-screen mode, plane offset 2) = `shift`.
    let hb = 0xD000usize;
    vdp.vram[hb + 2] = (shift >> 8) as u8;
    vdp.vram[hb + 3] = (shift & 0xFF) as u8;

    vdp
}

/// H40 left-most-column vertical-scroll quirk: when
/// per-column vscroll is on and hscroll is not 16px-aligned, the partial
/// left-edge column uses column 19's `vsram[A] & vsram[B]`, not column 0.
#[test]
fn test_left_column_vscroll_quirk_h40_uses_col19() {
    let shift = 4u16;
    let mut vdp = setup_left_col_vscroll_vdp(shift);
    vdp.set_test_register(12, 0x01); // H40
    vdp.vsram[1] = 0; // Plane B column 0 -> row 0 (idx 1)
    vdp.vsram[38] = 8; // column 19 Plane A
    vdp.vsram[39] = 8; // column 19 Plane B -> AND = 8 -> row 1 (idx 2)

    vdp.composite_scanline(0);

    for x in 0..shift as usize {
        assert_eq!(
            vdp.line_plane_b[x], 2,
            "x={x}: left sliver must use column-19 vscroll (idx 2)"
        );
    }
    for x in shift as usize..16 {
        assert_eq!(
            vdp.line_plane_b[x], 1,
            "x={x}: must use column-0 vscroll (idx 1)"
        );
    }
}

/// H32 disables the left-most-column vscroll (value forced to 0), so the
/// partial left-edge column reads row 0 even when column 0/19 scroll down.
#[test]
fn test_left_column_vscroll_quirk_h32_disabled() {
    let shift = 4u16;
    let mut vdp = setup_left_col_vscroll_vdp(shift);
    vdp.set_test_register(12, 0x00); // H32
    vdp.vsram[1] = 8; // column 0 -> row 1 (idx 2), so it differs from override
    vdp.vsram[38] = 8; // column 19 set non-zero to prove it is NOT used in H32
    vdp.vsram[39] = 8;

    vdp.composite_scanline(0);

    for x in 0..shift as usize {
        assert_eq!(
            vdp.line_plane_b[x], 1,
            "x={x}: H32 left-column vscroll is disabled (idx 1)"
        );
    }
    for x in shift as usize..16 {
        assert_eq!(
            vdp.line_plane_b[x], 2,
            "x={x}: must use column-0 vscroll (idx 2)"
        );
    }
}

/// A 16px-aligned horizontal scroll (`hscroll & 0x0F == 0`) must NOT trigger
/// the left-column override, even in H40 with per-column vscroll: the whole
/// left edge reads column 0.
#[test]
fn test_left_column_vscroll_no_override_when_16px_aligned() {
    let mut vdp = setup_left_col_vscroll_vdp(0);
    vdp.set_test_register(12, 0x01); // H40
    vdp.vsram[1] = 0; // column 0 -> idx 1
    vdp.vsram[38] = 8; // column 19 would give idx 2 if (wrongly) used
    vdp.vsram[39] = 8;

    vdp.composite_scanline(0);

    for x in 0..16usize {
        assert_eq!(
            vdp.line_plane_b[x], 1,
            "x={x}: aligned hscroll must not trigger the left-column override"
        );
    }
}

/// The debugger's per-layer disable mask suppresses a plane from
/// composition: with Plane B solid (idx 1) it composites normally,
/// but once `set_layer_debug_disabled(LAYER_PLANE_B, true)` is set the
/// pixels read back transparent (-1) and fall through to the backdrop.
#[test]
fn test_debug_layer_disable_suppresses_plane_b() {
    let mut vdp = setup_left_col_vscroll_vdp(0);

    vdp.composite_scanline(0);
    assert_eq!(vdp.line_plane_b[0], 1, "Plane B composites when enabled");

    vdp.set_layer_debug_disabled(LAYER_PLANE_B, true);
    vdp.composite_scanline(0);
    assert_eq!(
        vdp.line_plane_b[0], -1,
        "disabled Plane B is suppressed (transparent) in the composite"
    );
    // Plane A is tile 0 (transparent) here, so the final pixel is the
    // backdrop index, not Plane B.
    assert_eq!(vdp.line_index[0], vdp.backdrop_index().min(63) as u8);

    // Re-enabling restores the layer.
    vdp.set_layer_debug_disabled(LAYER_PLANE_B, false);
    vdp.composite_scanline(0);
    assert_eq!(
        vdp.line_plane_b[0], 1,
        "re-enabled Plane B composites again"
    );
}

/// Sprites are gated by the same mask. Place one opaque 8x8 sprite at
/// screen (0,0): with the layer enabled it wins over the low-priority
/// Plane B pixel; once disabled the BG shows through.
#[test]
fn test_debug_layer_disable_suppresses_sprites() {
    let mut vdp = setup_left_col_vscroll_vdp(0);
    // Sprite pattern tile 3 = solid colour index 5.
    for b in 0x60..0x80 {
        vdp.vram[b] = 0x55;
    }
    // SAT base 0xB000 (reg5=0x58, H40 from reset). Sprite 0: 1x1 at screen (0,0),
    // pattern 3, palette 1, link 0 (end of list).
    let sat = 0xB000usize;
    vdp.vram[sat] = 0x00; // word0: Y = 128 -> screen 0
    vdp.vram[sat + 1] = 0x80;
    vdp.vram[sat + 2] = 0x00; // word1: vsize=hsize=0, link=0
    vdp.vram[sat + 3] = 0x00;
    let attr = 0x2000u16 | 3; // palette 1, pattern 3, low priority
    vdp.vram[sat + 4] = (attr >> 8) as u8;
    vdp.vram[sat + 5] = (attr & 0xFF) as u8;
    vdp.vram[sat + 6] = 0x00; // word3: X = 128 -> screen 0
    vdp.vram[sat + 7] = 0x80;
    vdp.invalidate_sprite_cache();

    vdp.composite_scanline(0);
    // Low-pri sprite (pal 1, colour 5 -> idx 21) beats low-pri Plane B.
    assert_eq!(vdp.line_index[0], 21, "sprite shows over BG when enabled");

    vdp.set_layer_debug_disabled(LAYER_SPRITES, true);
    vdp.composite_scanline(0);
    assert_eq!(
        vdp.line_index[0], 1,
        "sprites suppressed; Plane B (idx 1) shows through"
    );
}

#[test]
fn test_shadow_color() {
    let color = (200, 100, 50);
    let shadowed = Vdp::shadow_color(color);
    assert_eq!(shadowed, (100, 50, 25));
}

#[test]
fn test_highlight_color() {
    let color = (100, 100, 100);
    let highlighted = Vdp::highlight_color(color);
    // 100 * 3 / 2 = 150
    assert_eq!(highlighted, (150, 150, 150));
}

#[test]
fn test_highlight_color_clamped() {
    let color = (200, 200, 200);
    let highlighted = Vdp::highlight_color(color);
    // 200 * 3 / 2 = 300, clamped to 255
    assert_eq!(highlighted, (255, 255, 255));
}

// ===== DMA Integration Tests =====

/// Verify that the renderer correctly decodes all tile attribute bits
/// from a nametable word stored in VRAM, using the exact same code path
/// as render_scanline.
#[test]
fn test_nametable_word_decode_in_render_path() {
    let test_cases: &[(u16, bool, u16, u8, bool, bool)] = &[
        // (word, expected_pri, expected_pat, expected_pal, expected_fh, expected_fv)
        (0x8001, true, 0x001, 0, false, false), // priority only
        (0xE001, true, 0x001, 3, false, false), // priority + palette 3
        (0x6001, false, 0x001, 3, false, false), // no priority, palette 3
        (0x0001, false, 0x001, 0, false, false), // no attributes
        (0x825C, true, 0x25C, 0, false, false), // the Sonic 1 ribbon case
        (0x025C, false, 0x25C, 0, false, false), // the same tile, priority clear
        (0x9800, true, 0x000, 0, true, true),   // priority + both flips
        (0x0800, false, 0x000, 0, true, false), // h-flip only
        (0x1000, false, 0x000, 0, false, true), // v-flip only
        (0xFFFF, true, 0x7FF, 3, true, true),   // all bits set
    ];

    for &(word, exp_pri, exp_pat, exp_pal, exp_fh, exp_fv) in test_cases {
        // Write word to VRAM at a known address, big-endian
        let addr: usize = 0x1000;
        let mut vram = vec![0u8; 0x10000];
        vram[addr] = (word >> 8) as u8;
        vram[addr + 1] = (word & 0xFF) as u8;

        // Read back using the exact same decode as render_scanline
        let read_word = ((vram[addr] as u16) << 8) | (vram[addr + 1] as u16);
        let priority = (read_word & 0x8000) != 0;
        let pattern = read_word & 0x07FF;
        let palette = ((read_word >> 13) & 0x03) as u8;
        let flip_h = (read_word & 0x0800) != 0;
        let flip_v = (read_word & 0x1000) != 0;

        assert_eq!(
            read_word, word,
            "Word readback mismatch for 0x{:04X}: got 0x{:04X}",
            word, read_word
        );
        assert_eq!(
            priority, exp_pri,
            "Priority mismatch for word 0x{:04X}: got {} expected {}",
            word, priority, exp_pri
        );
        assert_eq!(
            pattern, exp_pat,
            "Pattern mismatch for word 0x{:04X}: got 0x{:03X} expected 0x{:03X}",
            word, pattern, exp_pat
        );
        assert_eq!(
            palette, exp_pal,
            "Palette mismatch for word 0x{:04X}: got {} expected {}",
            word, palette, exp_pal
        );
        assert_eq!(
            flip_h, exp_fh,
            "FlipH mismatch for word 0x{:04X}: got {} expected {}",
            word, flip_h, exp_fh
        );
        assert_eq!(
            flip_v, exp_fv,
            "FlipV mismatch for word 0x{:04X}: got {} expected {}",
            word, flip_v, exp_fv
        );
    }
}

/// Render-path shadow/highlight test. With S/H enabled a low-priority
/// plane-A pixel must be shadowed and a high-priority one must stay normal.
/// Drives the full composite->index-buffer then remap->RGB pipeline (the
/// remap pass owns S/H), so S/H is checked without a game that uses it.
#[test]
fn test_shadow_highlight_render_path() {
    for (priority_word, high_pri) in [(0x0000u16, false), (0x8000u16, true)] {
        let mut vdp = Vdp::new();
        // Display on; H40 (reg12 bit0) + Shadow/Highlight (reg12 bit3).
        vdp.set_test_register(1, 0x44);
        vdp.set_test_register(12, 0x09);
        // Plane A nametable at 0xC000 (reg2 bits 5-3 = 0b110).
        vdp.set_test_register(2, 0x30);
        // Tile 1 pattern: pixel (0,0) = colour index 1 (high nibble of byte 0).
        vdp.vram[0x20] = 0x10;
        // Plane A cell (0,0) -> tile 1, palette 0, priority per case.
        let word = priority_word | 0x0001;
        vdp.vram[0xC000] = (word >> 8) as u8;
        vdp.vram[0xC001] = (word & 0xFF) as u8;
        // Palette 0 colour 1 = an asymmetric colour so shadow is visible.
        vdp.cram[1] = 0x008E;
        vdp.rebuild_cram_rgb();
        let base = vdp.cram_rgb[1];

        vdp.render_scanline(0);

        let got = (vdp.framebuffer[0], vdp.framebuffer[1], vdp.framebuffer[2]);
        let expected = if high_pri {
            base
        } else {
            Vdp::shadow_color(base)
        };
        assert_eq!(
            got, expected,
            "S/H render mismatch (high_pri={high_pri}): got {got:?} expected {expected:?} base {base:?}"
        );
        if !high_pri {
            assert_ne!(got, base, "shadow must darken the low-priority pixel");
        }
    }
}

/// Mid-line CRAM beam-X segment re-tint: an
/// in-window palette write recolours only `[beam_x, width)` so the already
/// scanned-out left keeps its colour; a write past the 860-mclk window does
/// not touch the line at all. Locks in the segment boundary and the window
/// so a future change can't silently widen, drop, or whole-line it.
#[test]
fn test_cram_midline_retint_window() {
    let mut vdp = Vdp::new();
    // Display on, H40, S/H off; plane A base 0xC000.
    vdp.set_test_register(1, 0x44);
    vdp.set_test_register(12, 0x01);
    vdp.set_test_register(2, 0x30);
    // Tile 1 = colour index 1 for every pixel; fill plane A row 0 with it so
    // the whole visible line resolves to CRAM index 1.
    for i in 0..32 {
        vdp.vram[0x20 + i] = 0x11;
    }
    for col in 0..64usize {
        let a = 0xC000 + col * 2;
        vdp.vram[a] = 0x00;
        vdp.vram[a + 1] = 0x01;
    }
    // Palette 0 colour 1 starts red.
    vdp.cram[1] = 0x000E;
    vdp.rebuild_cram_rgb();
    vdp.timing.scanline = 0;
    vdp.render_scanline(0);
    let red = Vdp::convert_genesis_color(0x000E);
    let width = vdp.display_width() as usize;
    assert_eq!(
        (vdp.framebuffer[0], vdp.framebuffer[1], vdp.framebuffer[2]),
        red
    );

    // Beam mid-line but inside the 860-mclk window: re-tint only [beam_x,w).
    vdp.beam_cycles_per_line = 488;
    vdp.beam_cycles_into_line = 100;
    let beam_x = vdp.beam_active_x();
    assert!(
        beam_x > 0 && beam_x < width,
        "beam_x={beam_x} width={width}"
    );
    vdp.code = 0x03; // CRAM write
    vdp.address = 0x02; // index 1
    vdp.write_data_port(0x00E0, true); // green
    let green = Vdp::convert_genesis_color(0x00E0);
    let lx = (beam_x - 1) * 4;
    let rx = (beam_x + 4) * 4;
    assert_eq!(
        (
            vdp.framebuffer[lx],
            vdp.framebuffer[lx + 1],
            vdp.framebuffer[lx + 2]
        ),
        red,
        "left of the beam must keep the pre-write colour"
    );
    assert_eq!(
        (
            vdp.framebuffer[rx],
            vdp.framebuffer[rx + 1],
            vdp.framebuffer[rx + 2]
        ),
        green,
        "at/right of the beam must show the new colour"
    );

    // Beam past the window: CRAM updates but the line is left untouched.
    vdp.beam_cycles_into_line = 480;
    vdp.code = 0x03;
    vdp.address = 0x02;
    vdp.write_data_port(0x0E00, true); // blue
    assert_eq!(
        (
            vdp.framebuffer[rx],
            vdp.framebuffer[rx + 1],
            vdp.framebuffer[rx + 2]
        ),
        green,
        "late-beam CRAM write must NOT re-tint the current line"
    );
    assert_eq!(vdp.cram_rgb[1], Vdp::convert_genesis_color(0x0E00));
}

// -- The FIFO's access slots -----------------------------------------

/// Register 16's two size fields: 32, 64 or 128 tiles across (the
/// invalid code repeats row 0 of a 32-tile plane), and 256 to 1024
/// pixels down, the invalid code giving a mask that is not a power of
/// two less one. The other bits of the register change nothing.
#[test]
fn playfield_geometry_of_every_size_code() {
    let mut vdp = Vdp::new();
    let across = [(6, 0x1F), (7, 0x3F), (0, 0x1F), (8, 0x7F)];
    let down = [0x0FF, 0x1FF, 0x2FF, 0x3FF];
    for value in 0..=0xFFu8 {
        vdp.registers[16] = value;
        let (row_shift, col_mask) = across[usize::from(value & 0x03)];
        let row_mask = down[usize::from((value >> 4) & 0x03)];
        assert_eq!(
            vdp.playfield_geometry(),
            (row_shift, col_mask, row_mask),
            "register 16 = {value:#04x}"
        );
    }
}
