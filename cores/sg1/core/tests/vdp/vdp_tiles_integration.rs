//! Integration tests for VDP tile rendering: patterns and colors written
//! through the bus, tiles decoded and drawn into a framebuffer, and the color
//! conversion.

use retro_core::cpu::Addressable;
use sg1_core::bus::AddressBus;
use sg1_core::vdp::tiles::{build_rgba_palette, convert_genesis_color_to_rgb, Tile};

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

#[test]
fn test_tile_render_to_framebuffer() {
    // Create a simple test tile
    let mut pattern = [0u8; 32];

    // A frame of color 1, two pixels to a byte (the left one in the high
    // nibble):
    // Row 0: 11111111
    // Rows 1-6: 10000001 (edges only)
    // Row 7: 11111111
    pattern[0..4].copy_from_slice(&[0x11, 0x11, 0x11, 0x11]);
    for row in 1..7 {
        let base = row * 4;
        pattern[base..base + 4].copy_from_slice(&[0x10, 0x00, 0x00, 0x01]);
    }
    pattern[28..32].copy_from_slice(&[0x11, 0x11, 0x11, 0x11]);

    let tile = Tile::decode(&pattern);

    // Create a simple palette (color 1 = red)
    let mut palette = [(0u8, 0u8, 0u8, 255u8); 16];
    palette[1] = (255, 0, 0, 255); // Red

    // Create framebuffer
    let mut framebuffer = vec![0u8; 320 * 224 * 4];

    // Render tile at position (10, 10)
    tile.render_to_framebuffer(&mut framebuffer, 10, 10, 320, &palette, false, false);

    // Verify some pixels
    let get_pixel = |x: u32, y: u32| -> (u8, u8, u8) {
        let idx = ((y * 320 + x) * 4) as usize;
        (framebuffer[idx], framebuffer[idx + 1], framebuffer[idx + 2])
    };

    // Top-left of tile (10, 10) should be red
    assert_eq!(get_pixel(10, 10), (255, 0, 0), "Top-left should be red");

    // Center of tile should be black (color 0 = transparent)
    assert_eq!(get_pixel(13, 13), (0, 0, 0), "Center should be black");

    // Bottom-right corner should be red
    assert_eq!(get_pixel(17, 17), (255, 0, 0), "Bottom-right should be red");
}

#[test]
fn test_multiple_tiles_no_overlap() {
    // All pixels = color 1
    let pattern = [0x11u8; 32];

    let tile = Tile::decode(&pattern);

    let mut palette = [(0u8, 0u8, 0u8, 255u8); 16];
    palette[1] = (255, 0, 0, 255); // Red

    let mut framebuffer = vec![0u8; 320 * 224 * 4];

    // Render 3 tiles in a row
    tile.render_to_framebuffer(&mut framebuffer, 0, 0, 320, &palette, false, false);
    tile.render_to_framebuffer(&mut framebuffer, 8, 0, 320, &palette, false, false);
    tile.render_to_framebuffer(&mut framebuffer, 16, 0, 320, &palette, false, false);

    let get_pixel = |x: u32, y: u32| -> (u8, u8, u8) {
        let idx = ((y * 320 + x) * 4) as usize;
        (framebuffer[idx], framebuffer[idx + 1], framebuffer[idx + 2])
    };

    // Verify pixels in each tile
    assert_eq!(get_pixel(0, 0), (255, 0, 0));
    assert_eq!(get_pixel(8, 0), (255, 0, 0));
    assert_eq!(get_pixel(16, 0), (255, 0, 0));
}

#[test]
fn test_tile_with_different_palettes() {
    // All pixels = color 1
    let pattern = [0x11u8; 32];

    let tile = Tile::decode(&pattern);

    // Palette 0: color 1 = red
    let mut palette0 = [(0u8, 0u8, 0u8, 255u8); 16];
    palette0[1] = (255, 0, 0, 255);

    // Palette 1: color 1 = blue
    let mut palette1 = [(0u8, 0u8, 0u8, 255u8); 16];
    palette1[1] = (0, 0, 255, 255);

    let mut framebuffer = vec![0u8; 320 * 224 * 4];

    // Render same tile with different palettes
    tile.render_to_framebuffer(&mut framebuffer, 0, 0, 320, &palette0, false, false);
    tile.render_to_framebuffer(&mut framebuffer, 8, 0, 320, &palette1, false, false);

    let get_pixel = |x: u32, y: u32| -> (u8, u8, u8) {
        let idx = ((y * 320 + x) * 4) as usize;
        (framebuffer[idx], framebuffer[idx + 1], framebuffer[idx + 2])
    };

    // First tile should be red
    assert_eq!(get_pixel(0, 0), (255, 0, 0));

    // Second tile should be blue
    assert_eq!(get_pixel(8, 0), (0, 0, 255));
}

#[test]
fn test_transparency_color_0() {
    let mut pattern = [0u8; 32];
    // Create a tile with color 0 (transparent) and color 1
    // Only first pixel is color 1, rest are color 0
    pattern[0] = 0x10; // First pixel, high nibble

    let tile = Tile::decode(&pattern);

    let mut palette = [(0u8, 0u8, 0u8, 255u8); 16];
    palette[1] = (255, 0, 0, 255); // Red

    // Fill framebuffer with blue background
    let mut framebuffer = vec![0u8; 320 * 224 * 4];
    for i in 0..(320 * 224) {
        let idx = i * 4;
        framebuffer[idx] = 0; // R
        framebuffer[idx + 1] = 0; // G
        framebuffer[idx + 2] = 255; // B (blue background)
        framebuffer[idx + 3] = 255; // A
    }

    // Render tile
    tile.render_to_framebuffer(&mut framebuffer, 10, 10, 320, &palette, false, false);

    let get_pixel = |x: u32, y: u32| -> (u8, u8, u8) {
        let idx = ((y * 320 + x) * 4) as usize;
        (framebuffer[idx], framebuffer[idx + 1], framebuffer[idx + 2])
    };

    // First pixel should be red (tile color)
    assert_eq!(get_pixel(10, 10), (255, 0, 0));

    // Second pixel should still be blue (background shows through)
    assert_eq!(get_pixel(11, 10), (0, 0, 255));
}

#[test]
fn test_color_conversion_all_values() {
    // Test all possible 3-bit values for each channel
    for r in 0u16..8 {
        for g in 0u16..8 {
            for b in 0u16..8 {
                let genesis_color = (r << 1) | (g << 5) | (b << 9);
                let (r_out, g_out, b_out) = convert_genesis_color_to_rgb(genesis_color);

                // Each 3-bit value should map to approximately value * 36
                let r_expected = (r * 36) as u8;
                let g_expected = (g * 36) as u8;
                let b_expected = (b * 36) as u8;

                assert!(r_out >= r_expected && r_out <= r_expected.saturating_add(36));
                assert!(g_out >= g_expected && g_out <= g_expected.saturating_add(36));
                assert!(b_out >= b_expected && b_out <= b_expected.saturating_add(36));
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
