//! Integration tests for VDP multi-layer rendering.
//!
//! Tests Window plane, Plane B, and layer priority system.

use sg1_core::vdp::planes::{NametableEntry, PlaneConfig, WindowConfig};

/// A tile of one colour. A pattern holds two pixels to a byte, the left one
/// in the high nibble.
fn create_solid_tile(color: u8) -> [u8; 32] {
    [(color << 4) | color; 32]
}

#[test]
fn test_plane_b_independent() {
    // Plane B should render independently just like Plane A
    let mut vram = vec![0u8; 0x10000];

    let tile = create_solid_tile(2); // Color 2
    vram[0..32].copy_from_slice(&tile);

    let nametable_addr = 0x2000; // Different from Plane A
    for i in 0..16 {
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
    cram[2] = 0x00E0; // Green

    let plane_b = PlaneConfig {
        nametable_addr: nametable_addr as u16,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 0,
        v_scroll: 0,
    };

    let mut fb = vec![0u8; 320 * 224 * 4];
    plane_b.render(&mut fb, &vram, &cram, 320, 224);

    // Should see green
    assert_eq!(fb[0], 0);
    assert_eq!(fb[1], 252);
    assert_eq!(fb[2], 0);
}

#[test]
fn test_two_planes_overlap() {
    // Test Plane A and Plane B overlapping
    let mut vram = vec![0u8; 0x10000];

    // Tile for Plane A (color 1)
    let tile_a = create_solid_tile(1);
    vram[0..32].copy_from_slice(&tile_a);

    // Tile for Plane B (color 2)
    let tile_b = create_solid_tile(2);
    vram[32..64].copy_from_slice(&tile_b);

    // Plane A nametable at 0x1000
    let entry_a = NametableEntry {
        pattern_index: 0,
        flip_h: false,
        flip_v: false,
        palette: 0,
        priority: false,
    };
    let word_a = entry_a.to_word();
    vram[0x1000] = (word_a >> 8) as u8;
    vram[0x1001] = word_a as u8;

    // Plane B nametable at 0x2000
    let entry_b = NametableEntry {
        pattern_index: 1,
        flip_h: false,
        flip_v: false,
        palette: 0,
        priority: false,
    };
    let word_b = entry_b.to_word();
    vram[0x2000] = (word_b >> 8) as u8;
    vram[0x2001] = word_b as u8;

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E; // Red (Plane A)
    cram[2] = 0x00E0; // Green (Plane B)

    let plane_a = PlaneConfig {
        nametable_addr: 0x1000,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 0,
        v_scroll: 0,
    };

    let plane_b = PlaneConfig {
        nametable_addr: 0x2000,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 0,
        v_scroll: 0,
    };

    let mut fb = vec![0u8; 320 * 224 * 4];

    // Render order: Plane B (background), then Plane A (foreground)
    plane_b.render(&mut fb, &vram, &cram, 320, 224);
    plane_a.render(&mut fb, &vram, &cram, 320, 224);

    // Should see Plane A (red) on top
    assert_eq!(fb[0], 252);
    assert_eq!(fb[1], 0);
}

#[test]
fn test_window_over_planes() {
    // The window takes plane A's place in its area (tiles 20 and on here)
    let mut vram = vec![0u8; 0x10000];

    // Plane A tile (color 1)
    let tile_plane = create_solid_tile(1);
    vram[0..32].copy_from_slice(&tile_plane);

    // Window tile (color 2)
    let tile_window = create_solid_tile(2);
    vram[32..64].copy_from_slice(&tile_window);

    // Plane A nametable
    for i in 0..64 {
        let entry = NametableEntry {
            pattern_index: 0,
            flip_h: false,
            flip_v: false,
            palette: 0,
            priority: false,
        };
        let word = entry.to_word();
        let addr = 0x1000 + (i * 2) as usize;
        vram[addr] = (word >> 8) as u8;
        vram[addr + 1] = word as u8;
    }

    // Window nametable
    for i in 0..64 {
        let entry = NametableEntry {
            pattern_index: 1,
            flip_h: false,
            flip_v: false,
            palette: 0,
            priority: false,
        };
        let word = entry.to_word();
        let addr = 0x3000 + (i * 2) as usize;
        vram[addr] = (word >> 8) as u8;
        vram[addr + 1] = word as u8;
    }

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E; // Red (Plane A)
    cram[2] = 0x00E0; // Green (Window)

    let plane_a = PlaneConfig {
        nametable_addr: 0x1000,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 0,
        v_scroll: 0,
    };

    let window = WindowConfig {
        nametable_addr: 0x3000,
        width_tiles: 32,
        height_tiles: 32,
        h_pos: 20,
        v_pos: 0,
        right_side: true,
        down_side: false,
    };

    let mut fb = vec![0u8; 320 * 224 * 4];

    // Render order: Plane A, then Window
    plane_a.render(&mut fb, &vram, &cram, 320, 224);
    window.render(&mut fb, &vram, &cram, 320, 224);

    // Left side (tile <20) should be red (Plane A)
    let idx_left = 0;
    assert_eq!(fb[idx_left], 252);
    assert_eq!(fb[idx_left + 1], 0);

    // Right side (tile >=20) should be green (Window)
    let idx_right = (160 * 4) as usize;
    assert_eq!(fb[idx_right], 0);
    assert_eq!(fb[idx_right + 1], 252);
}

#[test]
fn test_priority_flag_override() {
    // High priority on Plane B should show over low priority Plane A
    let mut vram = vec![0u8; 0x10000];

    let tile_a = create_solid_tile(1);
    let tile_b = create_solid_tile(2);
    vram[0..32].copy_from_slice(&tile_a);
    vram[32..64].copy_from_slice(&tile_b);

    // Plane A: low priority
    let entry_a = NametableEntry {
        pattern_index: 0,
        flip_h: false,
        flip_v: false,
        palette: 0,
        priority: false,
    };
    let word_a = entry_a.to_word();
    vram[0x1000] = (word_a >> 8) as u8;
    vram[0x1001] = word_a as u8;

    // Plane B: high priority
    let entry_b = NametableEntry {
        pattern_index: 1,
        flip_h: false,
        flip_v: false,
        palette: 0,
        priority: true,
    };
    let word_b = entry_b.to_word();
    vram[0x2000] = (word_b >> 8) as u8;
    vram[0x2001] = word_b as u8;

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E; // Red
    cram[2] = 0x00E0; // Green

    let plane_a = PlaneConfig {
        nametable_addr: 0x1000,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 0,
        v_scroll: 0,
    };

    let plane_b = PlaneConfig {
        nametable_addr: 0x2000,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 0,
        v_scroll: 0,
    };

    let mut fb = vec![0u8; 320 * 224 * 4];

    // Correct priority rendering:
    // 1. Low priority: B then A
    plane_b.render_with_priority(&mut fb, &vram, &cram, 320, 224, false);
    plane_a.render_with_priority(&mut fb, &vram, &cram, 320, 224, false);

    // 2. High priority: B then A
    plane_b.render_with_priority(&mut fb, &vram, &cram, 320, 224, true);
    plane_a.render_with_priority(&mut fb, &vram, &cram, 320, 224, true);

    // Should see Plane B (green) because it has high priority
    assert_eq!(fb[0], 0);
    assert_eq!(fb[1], 252);
}

#[test]
fn test_window_partial_screen() {
    // Window covering only bottom-right
    let mut vram = vec![0u8; 0x10000];

    let tile = create_solid_tile(1);
    vram[0..32].copy_from_slice(&tile);

    for i in 0..64 {
        let entry = NametableEntry {
            pattern_index: 0,
            flip_h: false,
            flip_v: false,
            palette: 0,
            priority: false,
        };
        let word = entry.to_word();
        let addr = 0x3000 + (i * 2) as usize;
        vram[addr] = (word >> 8) as u8;
        vram[addr + 1] = word as u8;
    }

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E;

    let window = WindowConfig {
        nametable_addr: 0x3000,
        width_tiles: 32,
        height_tiles: 32,
        h_pos: 20,
        v_pos: 15,
        right_side: true,
        down_side: true,
    };

    let mut fb = vec![0u8; 320 * 224 * 4];
    window.render(&mut fb, &vram, &cram, 320, 224);

    // Top-left should be black (not covered)
    assert_eq!(fb[0], 0);

    // Bottom-right should be red
    let idx = ((120 * 320 + 160) * 4) as usize;
    assert_eq!(fb[idx], 252);
}

#[test]
fn test_transparency_layering() {
    // Test that color 0 (transparent) allows lower layers to show
    let mut vram = vec![0u8; 0x10000];

    // Plane B tile: solid color 2 (green)
    let tile_b = create_solid_tile(2);
    vram[0..32].copy_from_slice(&tile_b);

    // Plane A tile: its first row is four transparent pixels, then four of
    // color 1 (red)
    let mut tile_a = [0u8; 32];
    tile_a[2] = 0x11;
    tile_a[3] = 0x11;
    vram[32..64].copy_from_slice(&tile_a);

    // Plane B nametable
    let entry_b = NametableEntry {
        pattern_index: 0,
        flip_h: false,
        flip_v: false,
        palette: 0,
        priority: false,
    };
    let word_b = entry_b.to_word();
    vram[0x2000] = (word_b >> 8) as u8;
    vram[0x2001] = word_b as u8;

    // Plane A nametable
    let entry_a = NametableEntry {
        pattern_index: 1,
        flip_h: false,
        flip_v: false,
        palette: 0,
        priority: false,
    };
    let word_a = entry_a.to_word();
    vram[0x1000] = (word_a >> 8) as u8;
    vram[0x1001] = word_a as u8;

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E; // Red
    cram[2] = 0x00E0; // Green

    let plane_b = PlaneConfig {
        nametable_addr: 0x2000,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 0,
        v_scroll: 0,
    };

    let plane_a = PlaneConfig {
        nametable_addr: 0x1000,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 0,
        v_scroll: 0,
    };

    let mut fb = vec![0u8; 320 * 224 * 4];

    // Render B then A
    plane_b.render(&mut fb, &vram, &cram, 320, 224);
    plane_a.render(&mut fb, &vram, &cram, 320, 224);

    // First pixel: transparent in A, should see B (green)
    assert_eq!(fb[0], 0);
    assert_eq!(fb[1], 252);

    // Fifth pixel: red in A
    let idx = (4 * 4) as usize;
    assert_eq!(fb[idx], 252);
    assert_eq!(fb[idx + 1], 0);
}

#[test]
fn test_window_with_scrolling_planes() {
    // Window is fixed, but planes scroll underneath
    let mut vram = vec![0u8; 0x10000];

    let tile_a = create_solid_tile(1);
    let tile_window = create_solid_tile(2);
    vram[0..32].copy_from_slice(&tile_a);
    vram[32..64].copy_from_slice(&tile_window);

    // Plane A nametable
    for i in 0..64 {
        let entry = NametableEntry {
            pattern_index: 0,
            flip_h: false,
            flip_v: false,
            palette: 0,
            priority: false,
        };
        let word = entry.to_word();
        let addr = 0x1000 + (i * 2) as usize;
        vram[addr] = (word >> 8) as u8;
        vram[addr + 1] = word as u8;
    }

    // Window nametable
    for i in 0..64 {
        let entry = NametableEntry {
            pattern_index: 1,
            flip_h: false,
            flip_v: false,
            palette: 0,
            priority: false,
        };
        let word = entry.to_word();
        let addr = 0x3000 + (i * 2) as usize;
        vram[addr] = (word >> 8) as u8;
        vram[addr + 1] = word as u8;
    }

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E; // Red
    cram[2] = 0x00E0; // Green

    let plane_a = PlaneConfig {
        nametable_addr: 0x1000,
        width_tiles: 32,
        height_tiles: 32,
        h_scroll: 16, // Scrolled
        v_scroll: 0,
    };

    let window = WindowConfig {
        nametable_addr: 0x3000,
        width_tiles: 32,
        height_tiles: 32,
        h_pos: 20,
        v_pos: 0,
        right_side: true,
        down_side: false,
    };

    let mut fb = vec![0u8; 320 * 224 * 4];

    plane_a.render(&mut fb, &vram, &cram, 320, 224);
    window.render(&mut fb, &vram, &cram, 320, 224);

    // Window region should be green regardless of plane scroll
    let idx_window = (160 * 4) as usize;
    assert_eq!(fb[idx_window], 0);
    assert_eq!(fb[idx_window + 1], 252);
}
