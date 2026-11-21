//! Tiles: patterns and colours written through the bus, tiles decoded, tiles
//! drawn by the renderer the console runs, and the colour conversion of the
//! debugger's exports.

use super::draw;
use retro_core::cpu::Addressable;
use retro_core::video::VideoDebugger;
use sg1_core::bus::AddressBus;
use sg1_core::vdp::tiles::{build_rgba_palette, Tile};
use sg1_core::vdp::Vdp;

#[test]
fn test_tile_decode_from_vram() {
    let mut bus = AddressBus::new();

    // Write a tile pattern to VRAM: all 0xFF (all pixels = color 15)
    let vdp_base = 0xC00000;

    // Auto-increment 2, then VRAM write from address 0
    bus.write_word(vdp_base + 4, 0x8F02);
    bus.write_word(vdp_base + 4, 0x4000);
    bus.write_word(vdp_base + 4, 0x0000);

    // Write the 32 bytes of the tile, a word at a time
    for _ in 0..16 {
        bus.write_word(vdp_base, 0xFFFF);
    }

    let tile = Tile::decode(&bus.vdp().get_vram()[0..32]);
    assert!(tile.pixels.iter().all(|&pixel| pixel == 15));
}

#[test]
fn test_palette_write_to_cram() {
    let mut bus = AddressBus::new();
    let vdp_base = 0xC00000;

    // Set VDP to CRAM write mode
    bus.write_word(vdp_base + 4, 0x8F02); // Auto-increment 2
    bus.write_word(vdp_base + 4, 0xC000); // CRAM write, address 0
    bus.write_word(vdp_base + 4, 0x0000);

    // Write color values (9-bit format)
    // Black, Red, Green, Blue
    bus.write_word(vdp_base, 0x0000); // Black
    bus.write_word(vdp_base, 0x000E); // Red
    bus.write_word(vdp_base, 0x00E0); // Green
    bus.write_word(vdp_base, 0x0E00); // Blue

    assert_eq!(bus.vdp().get_cram()[0..4], [0x0000, 0x000E, 0x00E0, 0x0E00]);
}

/// The frame of tile 1, a pattern of `pattern`, drawn as plane A's top left
/// cell moved to (`x`, `y`) by the scrolls, colour 1 red and the backdrop
/// blue.
fn draw_tile_at(pattern: &[u8; 32], x: u16, y: u16) -> sg1_core::vdp::Vdp {
    let mut vdp = draw::vdp();
    draw::put_tile(&mut vdp, 1, pattern);
    draw::put_cell(
        &mut vdp,
        draw::PLANE_A,
        32,
        0,
        0,
        draw::entry(1, 0, false, false, false),
    );
    draw::set_colour(&mut vdp, 1, draw::RED);
    draw::set_colour(&mut vdp, 2, draw::BLUE);
    vdp.set_test_register(7, 0x02);
    // Plane A is 256 lines high: scrolling up by 256 - y moves it down by y.
    draw::scroll_across(&mut vdp, x, 0);
    draw::scroll_down(&mut vdp, (256 - y) & 0xFF, 0);
    draw::draw(&mut vdp);
    vdp
}

#[test]
fn a_tile_is_drawn_where_the_scrolls_put_it() {
    // A frame of colour 1, two pixels to a byte (the left one in the high
    // nibble): rows 0 and 7 whole, rows 1-6 their two edges.
    let mut pattern = [0u8; 32];
    pattern[0..4].copy_from_slice(&[0x11, 0x11, 0x11, 0x11]);
    for row in 1..7 {
        let base = row * 4;
        pattern[base..base + 4].copy_from_slice(&[0x10, 0x00, 0x00, 0x01]);
    }
    pattern[28..32].copy_from_slice(&[0x11, 0x11, 0x11, 0x11]);

    let vdp = draw_tile_at(&pattern, 10, 10);
    assert_eq!(
        draw::pixel(&vdp, 10, 10),
        draw::shown(draw::RED),
        "top left"
    );
    assert_eq!(
        draw::pixel(&vdp, 13, 13),
        draw::shown(draw::BLUE),
        "the middle is colour 0: the backdrop"
    );
    assert_eq!(
        draw::pixel(&vdp, 17, 17),
        draw::shown(draw::RED),
        "bottom right"
    );
    assert_eq!(draw::pixel(&vdp, 9, 10), draw::shown(draw::BLUE));
    assert_eq!(draw::pixel(&vdp, 18, 18), draw::shown(draw::BLUE));
}

#[test]
fn tiles_in_a_row_do_not_overlap() {
    let mut vdp = draw::vdp();
    draw::put_tile(&mut vdp, 1, &draw::solid_tile(1));
    for column in 0..3 {
        let value = draw::entry(1, 0, false, false, false);
        draw::put_cell(&mut vdp, draw::PLANE_A, 32, column, 0, value);
    }
    draw::set_colour(&mut vdp, 1, draw::RED);
    draw::draw(&mut vdp);
    for x in [0, 8, 16, 23] {
        assert_eq!(
            draw::pixel(&vdp, x, 0),
            draw::shown(draw::RED),
            "column {x}"
        );
    }
    assert_eq!(draw::pixel(&vdp, 24, 0), draw::shown(0));
}

#[test]
fn one_tile_takes_the_colours_of_the_palette_line_its_cell_names() {
    let mut vdp = draw::vdp();
    draw::put_tile(&mut vdp, 1, &draw::solid_tile(1));
    draw::put_cell(
        &mut vdp,
        draw::PLANE_A,
        32,
        0,
        0,
        draw::entry(1, 0, false, false, false),
    );
    draw::put_cell(
        &mut vdp,
        draw::PLANE_A,
        32,
        1,
        0,
        draw::entry(1, 1, false, false, false),
    );
    draw::set_colour(&mut vdp, 1, draw::RED);
    draw::set_colour(&mut vdp, 17, draw::BLUE);
    draw::draw(&mut vdp);
    assert_eq!(draw::pixel(&vdp, 0, 0), draw::shown(draw::RED));
    assert_eq!(draw::pixel(&vdp, 8, 0), draw::shown(draw::BLUE));
}

#[test]
fn colour_0_of_a_tile_lets_the_backdrop_through() {
    let mut pattern = [0u8; 32];
    pattern[0] = 0x10; // only the first pixel, colour 1
    let vdp = draw_tile_at(&pattern, 10, 10);
    assert_eq!(draw::pixel(&vdp, 10, 10), draw::shown(draw::RED));
    assert_eq!(draw::pixel(&vdp, 11, 10), draw::shown(draw::BLUE));
}

#[test]
fn test_color_conversion_all_values() {
    // Every colour a CRAM word holds, three bits each of red, green and
    // blue, goes into the exports' palette on the levels the debugger
    // names, as the screen shows it.
    let levels = Vdp::new().colour_levels().levels;
    let mut cram = [0u16; 64];
    for r in 0u16..8 {
        for g in 0u16..8 {
            for b in 0u16..8 {
                let genesis_color = (r << 1) | (g << 5) | (b << 9);
                cram[0] = genesis_color;
                let (r_out, g_out, b_out, alpha) = build_rgba_palette(&cram, 0)[0];

                let expected = (levels[r as usize], levels[g as usize], levels[b as usize]);
                assert_eq!((r_out, g_out, b_out), expected, "{genesis_color:#06X}");
                assert_eq!(Vdp::convert_genesis_color(genesis_color), expected);
                assert_eq!(alpha, 255);
            }
        }
    }
}

#[test]
fn test_build_palette_all_palettes() {
    let mut cram = [0u16; 64];

    // Initialize CRAM with pattern
    for (i, color) in cram.iter_mut().enumerate() {
        *color = (i as u16) << 1; // Simple pattern
    }

    // Test all 4 palettes
    for pal_idx in 0..4 {
        let palette = build_rgba_palette(&cram, pal_idx);

        // Verify palette has 16 colors
        assert_eq!(palette.len(), 16);

        // Verify alpha channel is always 255
        for color in &palette {
            assert_eq!(color.3, 255, "Alpha should always be 255");
        }
    }
}
