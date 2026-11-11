//! Integration tests for VDP plane rendering.
//!
//! Tests advanced rendering scenarios with scrolling, multiple tiles, and complex layouts.

use sg1_core::vdp::planes::{NametableEntry, PlaneConfig};

/// A tile of one colour. A pattern holds two pixels to a byte, the left one
/// in the high nibble.
fn create_solid_tile(color: u8) -> [u8; 32] {
    [(color << 4) | color; 32]
}

#[test]
fn test_plane_render_multiple_tiles() {
    // Create 4x4 grid of tiles
    let mut vram = vec![0u8; 0x10000];

    // Create 2 different tile patterns
    let tile1 = create_solid_tile(1); // Color 1
    let tile2 = create_solid_tile(2); // Color 2

    // Write patterns to VRAM
    vram[0..32].copy_from_slice(&tile1);
    vram[32..64].copy_from_slice(&tile2);

    // Create nametable (4x4 = 16 entries)
    let nametable_addr = 0x1000;
    for y in 0..4 {
        for x in 0..4 {
            let pattern = if (x + y) % 2 == 0 { 0 } else { 1 };
            let entry = NametableEntry {
                pattern_index: pattern,
                flip_h: false,
                flip_v: false,
                palette: 0,
                priority: false,
            };
            let word = entry.to_word();
            let addr = nametable_addr + ((y * 4 + x) * 2) as usize;
            vram[addr] = (word >> 8) as u8;
            vram[addr + 1] = word as u8;
        }
    }

    // Setup CRAM
    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E; // Red
    cram[2] = 0x00E0; // Green

    let config = PlaneConfig {
        nametable_addr: nametable_addr as u16,
        width_tiles: 4,
        height_tiles: 4,
        h_scroll: 0,
        v_scroll: 0,
    };

    let mut framebuffer = vec![0u8; 320 * 224 * 4];
    config.render(&mut framebuffer, &vram, &cram, 320, 224);

    // Verify some pixels
    let get_pixel = |x: u32, y: u32| -> (u8, u8, u8) {
        let idx = ((y * 320 + x) * 4) as usize;
        (framebuffer[idx], framebuffer[idx + 1], framebuffer[idx + 2])
    };

    // Top-left tile should be red (pattern 0)
    assert_eq!(get_pixel(0, 0), (252, 0, 0));

    // Second tile should be green (pattern 1)
    assert_eq!(get_pixel(8, 0), (0, 252, 0));
}

#[test]
fn test_plane_render_with_scroll_h() {
    let mut vram = vec![0u8; 0x10000];

    // Single tile pattern
    let tile = create_solid_tile(1);
    vram[0..32].copy_from_slice(&tile);

    // Nametable with multiple tiles
    let nametable_addr = 0x1000;
    for i in 0..10 {
        let entry = NametableEntry {
            pattern_index: 0,
            flip_h: false,
            flip_v: false,
            palette: 0,
            priority: false,
        };
        let word = entry.to_word();
        let addr = nametable_addr + (i * 2) as usize;
        vram[addr] = (word >> 8) as u8;
        vram[addr + 1] = word as u8;
    }

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E; // Red

    // Test without scroll
    let config_no_scroll = PlaneConfig {
        nametable_addr: nametable_addr as u16,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 0,
        v_scroll: 0,
    };

    let mut fb1 = vec![0u8; 320 * 224 * 4];
    config_no_scroll.render(&mut fb1, &vram, &cram, 320, 224);

    // Test with h_scroll = 8 pixels (1 tile)
    let config_scroll = PlaneConfig {
        nametable_addr: nametable_addr as u16,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 8,
        v_scroll: 0,
    };

    let mut fb2 = vec![0u8; 320 * 224 * 4];
    config_scroll.render(&mut fb2, &vram, &cram, 320, 224);

    // With scroll, pixel at (0, 0) should match pixel at (8, 0) without scroll
    let idx1 = (8 * 4) as usize;
    let idx2 = 0;
    assert_eq!(fb1[idx1], fb2[idx2]); // R
    assert_eq!(fb1[idx1 + 1], fb2[idx2 + 1]); // G
    assert_eq!(fb1[idx1 + 2], fb2[idx2 + 2]); // B
}

#[test]
fn test_plane_render_with_scroll_v() {
    let mut vram = vec![0u8; 0x10000];

    let tile = create_solid_tile(1);
    vram[0..32].copy_from_slice(&tile);

    let nametable_addr = 0x1000;
    for y in 0..10 {
        for x in 0..10 {
            let entry = NametableEntry {
                pattern_index: 0,
                flip_h: false,
                flip_v: false,
                palette: 0,
                priority: false,
            };
            let word = entry.to_word();
            let addr = nametable_addr + ((y * 32 + x) * 2) as usize;
            vram[addr] = (word >> 8) as u8;
            vram[addr + 1] = word as u8;
        }
    }

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E;

    let config_no_scroll = PlaneConfig {
        nametable_addr: nametable_addr as u16,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 0,
        v_scroll: 0,
    };

    let mut fb1 = vec![0u8; 320 * 224 * 4];
    config_no_scroll.render(&mut fb1, &vram, &cram, 320, 224);

    // V-scroll by 8 pixels (1 tile)
    let config_scroll = PlaneConfig {
        nametable_addr: nametable_addr as u16,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 0,
        v_scroll: 8,
    };

    let mut fb2 = vec![0u8; 320 * 224 * 4];
    config_scroll.render(&mut fb2, &vram, &cram, 320, 224);

    // Verify scrolling worked
    let idx1 = ((8 * 320) * 4) as usize;
    let idx2 = 0;
    assert_eq!(fb1[idx1], fb2[idx2]);
}

#[test]
fn test_plane_render_wrap_around() {
    let mut vram = vec![0u8; 0x10000];

    // Two different tiles
    let tile1 = create_solid_tile(1);
    let tile2 = create_solid_tile(2);
    vram[0..32].copy_from_slice(&tile1);
    vram[32..64].copy_from_slice(&tile2);

    // Small plane (4x4) to test wrapping
    let nametable_addr = 0x1000;
    for i in 0..16 {
        let pattern = if i < 8 { 0 } else { 1 };
        let entry = NametableEntry {
            pattern_index: pattern,
            flip_h: false,
            flip_v: false,
            palette: 0,
            priority: false,
        };
        let word = entry.to_word();
        let addr = nametable_addr + (i * 2) as usize;
        vram[addr] = (word >> 8) as u8;
        vram[addr + 1] = word as u8;
    }

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E;
    cram[2] = 0x00E0;

    // Scroll by 32 pixels (4 tiles) - should wrap to beginning
    let config = PlaneConfig {
        nametable_addr: nametable_addr as u16,
        width_tiles: 4,
        height_tiles: 4,
        h_scroll: 32,
        v_scroll: 0,
    };

    let mut fb = vec![0u8; 320 * 224 * 4];
    config.render(&mut fb, &vram, &cram, 320, 224);

    // After wrapping, should see same pattern as start
    let idx = 0;
    assert!(fb[idx] > 0 || fb[idx + 1] > 0 || fb[idx + 2] > 0);
}

#[test]
fn test_plane_render_different_palettes() {
    let mut vram = vec![0u8; 0x10000];

    // One tile pattern
    let tile = create_solid_tile(1);
    vram[0..32].copy_from_slice(&tile);

    // Create nametable with different palettes
    let nametable_addr = 0x1000;
    for i in 0..4 {
        let entry = NametableEntry {
            pattern_index: 0,
            flip_h: false,
            flip_v: false,
            palette: i as u8,
            priority: false,
        };
        let word = entry.to_word();
        let addr = nametable_addr + (i * 2) as usize;
        vram[addr] = (word >> 8) as u8;
        vram[addr + 1] = word as u8;
    }

    // Setup 4 palettes with different colors
    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E; // Palette 0: Red
    cram[17] = 0x00E0; // Palette 1: Green
    cram[33] = 0x0E00; // Palette 2: Blue
    cram[49] = 0x0EEE; // Palette 3: White

    let config = PlaneConfig {
        nametable_addr: nametable_addr as u16,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 0,
        v_scroll: 0,
    };

    let mut fb = vec![0u8; 320 * 224 * 4];
    config.render(&mut fb, &vram, &cram, 320, 224);

    let get_pixel = |x: u32, y: u32| -> (u8, u8, u8) {
        let idx = ((y * 320 + x) * 4) as usize;
        (fb[idx], fb[idx + 1], fb[idx + 2])
    };

    // Tile 0 (palette 0) should be red
    assert_eq!(get_pixel(0, 0), (252, 0, 0));

    // Tile 1 (palette 1) should be green
    assert_eq!(get_pixel(8, 0), (0, 252, 0));

    // Tile 2 (palette 2) should be blue
    assert_eq!(get_pixel(16, 0), (0, 0, 252));

    // Tile 3 (palette 3) should be white
    assert_eq!(get_pixel(24, 0), (252, 252, 252));
}

#[test]
fn test_plane_render_flipped_tiles() {
    let mut vram = vec![0u8; 0x10000];

    // Asymmetric tile pattern for flip testing
    let mut tile = [0u8; 32];
    // Top-left pixel only, color 15
    tile[0] = 0xF0;
    vram[0..32].copy_from_slice(&tile);

    let nametable_addr = 0x1000;

    // Four tiles: normal, flip_h, flip_v, both
    let entries = [
        NametableEntry {
            pattern_index: 0,
            flip_h: false,
            flip_v: false,
            palette: 0,
            priority: false,
        },
        NametableEntry {
            pattern_index: 0,
            flip_h: true,
            flip_v: false,
            palette: 0,
            priority: false,
        },
        NametableEntry {
            pattern_index: 0,
            flip_h: false,
            flip_v: true,
            palette: 0,
            priority: false,
        },
        NametableEntry {
            pattern_index: 0,
            flip_h: true,
            flip_v: true,
            palette: 0,
            priority: false,
        },
    ];

    for (i, entry) in entries.iter().enumerate() {
        let word = entry.to_word();
        let addr = nametable_addr + (i * 2);
        vram[addr] = (word >> 8) as u8;
        vram[addr + 1] = word as u8;
    }

    let mut cram = vec![0u16; 64];
    cram[15] = 0x0EEE; // White

    let config = PlaneConfig {
        nametable_addr: nametable_addr as u16,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 0,
        v_scroll: 0,
    };

    let mut fb = vec![0u8; 320 * 224 * 4];
    config.render(&mut fb, &vram, &cram, 320, 224);

    let is_white = |x: u32, y: u32| -> bool {
        let idx = ((y * 320 + x) * 4) as usize;
        fb[idx] > 200 && fb[idx + 1] > 200 && fb[idx + 2] > 200
    };

    // Tile 0: top-left should be white
    assert!(is_white(0, 0));
    assert!(!is_white(7, 0));

    // Tile 1 (h-flip): top-right should be white
    assert!(is_white(15, 0));
    assert!(!is_white(8, 0));

    // Tile 2 (v-flip): bottom-left should be white
    assert!(is_white(16, 7));
    assert!(!is_white(16, 0));

    // Tile 3 (both): bottom-right should be white
    assert!(is_white(31, 7));
    assert!(!is_white(24, 0));
}

#[test]
fn test_plane_size_64x32() {
    let mut vram = vec![0u8; 0x10000];

    let tile = create_solid_tile(1);
    vram[0..32].copy_from_slice(&tile);

    // Large nametable (64x32)
    let nametable_addr = 0x2000;
    for i in 0..(64 * 32) {
        let entry = NametableEntry {
            pattern_index: 0,
            flip_h: false,
            flip_v: false,
            palette: 0,
            priority: false,
        };
        let word = entry.to_word();
        let addr = nametable_addr + (i * 2) as usize;
        vram[addr] = (word >> 8) as u8;
        vram[addr + 1] = word as u8;
    }

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E;

    let config = PlaneConfig {
        nametable_addr: nametable_addr as u16,
        width_tiles: 64,
        height_tiles: 32,
        h_scroll: 0,
        v_scroll: 0,
    };

    let mut fb = vec![0u8; 320 * 224 * 4];
    config.render(&mut fb, &vram, &cram, 320, 224);

    // Should render without panic
    assert!(fb[0] > 0 || fb[1] > 0 || fb[2] > 0);
}

#[test]
fn test_plane_size_128x32() {
    let mut vram = vec![0u8; 0x10000];

    let tile = create_solid_tile(2);
    vram[0..32].copy_from_slice(&tile);

    // Very large nametable (128x32) - tests edge cases
    let nametable_addr = 0x4000;
    // Write 40 * 28 entries in a row: with 128 tiles to a plane row they fill
    // rows 0-7 and part of row 8 (the rest is zero, the same entry)
    for i in 0..(40 * 28) {
        let entry = NametableEntry {
            pattern_index: 0,
            flip_h: false,
            flip_v: false,
            palette: 0, // Use palette 0, not 1
            priority: false,
        };
        let word = entry.to_word();
        let addr = nametable_addr + (i * 2) as usize;
        if addr < vram.len() - 1 {
            vram[addr] = (word >> 8) as u8;
            vram[addr + 1] = word as u8;
        }
    }

    let mut cram = vec![0u16; 64];
    cram[2] = 0x00E0; // Palette 0, color 2 = Green

    let config = PlaneConfig {
        nametable_addr: nametable_addr as u16,
        width_tiles: 128,
        height_tiles: 32,
        h_scroll: 0,
        v_scroll: 0,
    };

    let mut fb = vec![0u8; 320 * 224 * 4];
    config.render(&mut fb, &vram, &cram, 320, 224);

    // Should render without panic
    let idx = 0;
    assert!(fb[idx] > 0 || fb[idx + 1] > 0 || fb[idx + 2] > 0);
}

#[test]
fn test_plane_partial_scroll() {
    let mut vram = vec![0u8; 0x10000];

    let tile = create_solid_tile(1);
    vram[0..32].copy_from_slice(&tile);

    let nametable_addr = 0x1000;
    for i in 0..64 {
        let entry = NametableEntry {
            pattern_index: 0,
            flip_h: false,
            flip_v: false,
            palette: 0,
            priority: false,
        };
        let word = entry.to_word();
        let addr = nametable_addr + (i * 2) as usize;
        vram[addr] = (word >> 8) as u8;
        vram[addr + 1] = word as u8;
    }

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E;

    // Scroll by 3 pixels (partial tile)
    let config = PlaneConfig {
        nametable_addr: nametable_addr as u16,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 3,
        v_scroll: 5,
    };

    let mut fb = vec![0u8; 320 * 224 * 4];
    config.render(&mut fb, &vram, &cram, 320, 224);

    // Should render without artifacts
    assert!(fb[0] > 0 || fb[1] > 0 || fb[2] > 0);
}

#[test]
fn test_plane_transparency() {
    let mut vram = vec![0u8; 0x10000];

    // Tile with color 0 (transparent) and color 1
    let mut tile = [0u8; 32];
    // First row: four pixels of color 0 (transparent), then four of color 1
    tile[2] = 0x11;
    tile[3] = 0x11;
    vram[0..32].copy_from_slice(&tile);

    let nametable_addr = 0x1000;
    let entry = NametableEntry {
        pattern_index: 0,
        flip_h: false,
        flip_v: false,
        palette: 0,
        priority: false,
    };
    let word = entry.to_word();
    vram[nametable_addr] = (word >> 8) as u8;
    vram[nametable_addr + 1] = word as u8;

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E;

    let config = PlaneConfig {
        nametable_addr: nametable_addr as u16,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 0,
        v_scroll: 0,
    };

    // Fill framebuffer with blue (background)
    let mut fb = vec![0u8; 320 * 224 * 4];
    for i in 0..(320 * 224) {
        let idx = i * 4;
        fb[idx] = 0;
        fb[idx + 1] = 0;
        fb[idx + 2] = 255;
        fb[idx + 3] = 255;
    }

    config.render(&mut fb, &vram, &cram, 320, 224);

    // First 4 pixels should be transparent (blue background)
    assert_eq!(fb[0], 0);
    assert_eq!(fb[1], 0);
    assert_eq!(fb[2], 255);

    // Next 4 pixels should be red (color 1)
    let idx = (4 * 4) as usize;
    assert_eq!(fb[idx], 252);
}
