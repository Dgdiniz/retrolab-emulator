//! Integration tests for VDP sprite rendering.

use sg1_core::vdp::sprites::{Sprite, SpriteCache};

/// Where the tests put the sprite attribute table.
const SAT: u16 = 0x100;

/// A tile of one colour. A pattern holds two pixels to a byte, the left one
/// in the high nibble.
fn create_test_tile(color: u8) -> [u8; 32] {
    [(color << 4) | color; 32]
}

/// Writes entry `index` of the sprite attribute table: its Y and X (128 is
/// the top or left edge of the screen), its width and height in tiles, the
/// entry it links to, and the word with the pattern (bits 0-10), the flips
/// (11 horizontal, 12 vertical), the palette (13-14) and the priority (15).
fn write_sprite(
    vram: &mut [u8],
    index: usize,
    y: u16,
    (width, height): (u16, u16),
    link: u16,
    attributes: u16,
    x: u16,
) {
    let size_and_link = ((width - 1) << 10) | ((height - 1) << 8) | link;
    let addr = SAT as usize + index * 8;
    for (i, word) in [y, size_and_link, attributes, x].into_iter().enumerate() {
        vram[addr + i * 2..addr + i * 2 + 2].copy_from_slice(&word.to_be_bytes());
    }
}

#[test]
fn test_sprite_render_single_1x1() {
    let mut vram = vec![0u8; 0x10000];

    // Create tile pattern
    let tile = create_test_tile(1); // Color 1
    vram[0..32].copy_from_slice(&tile);

    // One tile at y = 150, x = 160, pattern 0
    write_sprite(&mut vram, 0, 150, (1, 1), 0, 0x0000, 160);

    let sprite = Sprite::from_sat(&vram, SAT, 0).unwrap();

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E; // Red

    let mut framebuffer = vec![0u8; 320 * 224 * 4];
    sprite.render(&mut framebuffer, &vram, &cram, 320, 224);

    // Check pixel at sprite position
    let x = (160 - 128) as u32;
    let y = (150 - 128) as u32;
    let idx = ((y * 320 + x) * 4) as usize;

    assert_eq!(framebuffer[idx], 252); // Red
}

#[test]
fn test_sprite_render_2x2() {
    let mut vram = vec![0u8; 0x10000];

    // Create 4 tiles for 2x2 sprite
    for i in 0..4 {
        let tile = create_test_tile((i + 1) as u8);
        vram[i * 32..(i + 1) * 32].copy_from_slice(&tile);
    }

    write_sprite(&mut vram, 0, 150, (2, 2), 0, 0x0000, 160);

    let sprite = Sprite::from_sat(&vram, SAT, 0).unwrap();

    assert_eq!(sprite.width_tiles, 2);
    assert_eq!(sprite.height_tiles, 2);

    let (x1, y1, x2, y2) = sprite.bounds();
    assert_eq!(x2 - x1, 16); // 2 tiles wide
    assert_eq!(y2 - y1, 16); // 2 tiles tall
}

#[test]
fn test_sprite_render_4x4() {
    let mut vram = vec![0u8; 0x10000];

    // Create 16 tiles
    for i in 0..16 {
        let tile = create_test_tile(1);
        vram[i * 32..(i + 1) * 32].copy_from_slice(&tile);
    }

    write_sprite(&mut vram, 0, 150, (4, 4), 0, 0x0000, 160);

    let sprite = Sprite::from_sat(&vram, SAT, 0).unwrap();

    assert_eq!(sprite.width_tiles, 4);
    assert_eq!(sprite.height_tiles, 4);

    let (x1, y1, x2, y2) = sprite.bounds();
    assert_eq!(x2 - x1, 32);
    assert_eq!(y2 - y1, 32);
}

#[test]
fn test_sprite_flip_h() {
    let mut vram = vec![0u8; 0x10000];

    // Asymmetric tile (top-left pixel only, color 1)
    let mut tile = [0u8; 32];
    tile[0] = 0x10;
    vram[0..32].copy_from_slice(&tile);

    write_sprite(&mut vram, 0, 150, (1, 1), 0, 0x0800, 160);

    let sprite = Sprite::from_sat(&vram, SAT, 0).unwrap();
    assert!(sprite.flip_h);

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E; // Red

    let mut framebuffer = vec![0u8; 320 * 224 * 4];
    sprite.render(&mut framebuffer, &vram, &cram, 320, 224);

    // The pixel lands at the top right of the tile
    let red_at = |x: u32, y: u32| framebuffer[((y * 320 + x) * 4) as usize] == 252;
    assert!(red_at(32 + 7, 22));
    assert!(!red_at(32, 22));
}

#[test]
fn test_sprite_flip_v() {
    let mut vram = vec![0u8; 0x10000];

    let tile = create_test_tile(1);
    vram[0..32].copy_from_slice(&tile);

    write_sprite(&mut vram, 0, 150, (1, 1), 0, 0x1000, 160);

    let sprite = Sprite::from_sat(&vram, SAT, 0).unwrap();
    assert!(sprite.flip_v);
}

#[test]
fn test_sprite_different_palettes() {
    let mut vram = vec![0u8; 0x10000];
    let tile = create_test_tile(1);
    vram[0..32].copy_from_slice(&tile);

    // Test all 4 palettes
    for pal in 0..4 {
        write_sprite(&mut vram, pal, 150, (1, 1), 0, (pal as u16) << 13, 160);

        let sprite = Sprite::from_sat(&vram, SAT, pal as u8).unwrap();
        assert_eq!(sprite.palette, pal as u8);
    }
}

#[test]
fn test_sprite_priority_flag() {
    let mut vram = vec![0u8; 0x10000];

    write_sprite(&mut vram, 0, 150, (1, 1), 0, 0x8000, 160);

    let sprite = Sprite::from_sat(&vram, SAT, 0).unwrap();
    assert!(sprite.priority);
}

#[test]
fn test_sprite_offscreen_positions() {
    // Sprite at x=-128, y=-128 (offscreen top-left)
    let sprite = Sprite {
        x: -128,
        y: -128,
        width_tiles: 1,
        height_tiles: 1,
        pattern_base: 0,
        palette: 0,
        priority: false,
        flip_h: false,
        flip_v: false,
        link: 0,
    };

    assert!(!sprite.is_visible(320, 224));

    // Sprite at x=320, y=224 (offscreen bottom-right)
    let sprite2 = Sprite {
        x: 320,
        y: 224,
        width_tiles: 1,
        height_tiles: 1,
        pattern_base: 0,
        palette: 0,
        priority: false,
        flip_h: false,
        flip_v: false,
        link: 0,
    };

    assert!(!sprite2.is_visible(320, 224));
}

#[test]
fn test_sprite_partially_offscreen_left() {
    let mut vram = vec![0u8; 0x10000];
    let tile = create_test_tile(1);
    vram[0..32].copy_from_slice(&tile);

    let sprite = Sprite {
        x: -4, // Partially offscreen
        y: 100,
        width_tiles: 1,
        height_tiles: 1,
        pattern_base: 0,
        palette: 0,
        priority: false,
        flip_h: false,
        flip_v: false,
        link: 0,
    };

    assert!(sprite.is_visible(320, 224));

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E;

    let mut framebuffer = vec![0u8; 320 * 224 * 4];
    sprite.render(&mut framebuffer, &vram, &cram, 320, 224);

    // Should render visible part only
    let idx = ((100 * 320) * 4) as usize;
    assert_eq!(framebuffer[idx], 252); // Red visible
}

#[test]
fn test_sprite_cache_multiple_sprites() {
    let mut vram = vec![0u8; 0x10000];

    // Create 3 sprites in chain: 0 -> 1 -> 2, at different y positions
    for i in 0..3 {
        let link = if i < 2 { i as u16 + 1 } else { 0 };
        write_sprite(&mut vram, i, 150 + i as u16 * 32, (1, 1), link, 0x0000, 160);
    }

    let mut cache = SpriteCache::new();
    cache.build(&vram, SAT, 80);

    assert_eq!(cache.len(), 3);
}

#[test]
fn test_sprite_cache_render_multiple() {
    let mut vram = vec![0u8; 0x10000];

    // Create tile
    let tile = create_test_tile(1);
    vram[0..32].copy_from_slice(&tile);

    // Create 2 sprites side by side: 0 -> 1
    for i in 0..2 {
        let link = if i == 0 { 1 } else { 0 };
        write_sprite(&mut vram, i, 150, (1, 1), link, 0x0000, 160 + i as u16 * 16);
    }

    let mut cache = SpriteCache::new();
    cache.build(&vram, SAT, 80);

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E;

    let mut framebuffer = vec![0u8; 320 * 224 * 4];
    cache.render_all(&mut framebuffer, &vram, &cram, 320, 224);

    // Both sprites should be rendered
    assert_eq!(cache.len(), 2);
    let red_at = |x: u32, y: u32| framebuffer[((y * 320 + x) * 4) as usize] == 252;
    assert!(red_at(32, 22));
    assert!(red_at(48, 22));
}

#[test]
fn test_sprite_transparency() {
    let mut vram = vec![0u8; 0x10000];

    // Tile whose first row is four pixels of color 0 (transparent), then
    // four of color 1
    let mut tile = [0u8; 32];
    tile[2] = 0x11;
    tile[3] = 0x11;
    vram[0..32].copy_from_slice(&tile);

    write_sprite(&mut vram, 0, 150, (1, 1), 0, 0x0000, 160);

    let sprite = Sprite::from_sat(&vram, SAT, 0).unwrap();

    let mut cram = vec![0u16; 64];
    cram[1] = 0x000E;

    // Fill background with blue
    let mut framebuffer = vec![0u8; 320 * 224 * 4];
    for i in 0..(320 * 224) {
        let idx = i * 4;
        framebuffer[idx + 2] = 255; // Blue
        framebuffer[idx + 3] = 255; // Alpha
    }

    sprite.render(&mut framebuffer, &vram, &cram, 320, 224);

    let x = (160 - 128) as u32;
    let y = (150 - 128) as u32;

    // First pixel should be blue (transparent)
    let idx1 = ((y * 320 + x) * 4) as usize;
    assert_eq!(framebuffer[idx1 + 2], 255);

    // Fifth pixel should be red (color 1)
    let idx2 = ((y * 320 + x + 4) * 4) as usize;
    assert_eq!(framebuffer[idx2], 252);
}

#[test]
fn test_sprite_link_chain_circular_protection() {
    let mut vram = vec![0u8; 0x10000];

    // A chain that never ends: 0 -> 1 -> 1 -> ...
    write_sprite(&mut vram, 0, 150, (1, 1), 1, 0x0000, 160);
    write_sprite(&mut vram, 1, 150, (1, 1), 1, 0x0000, 160);

    let mut cache = SpriteCache::new();
    cache.build(&vram, SAT, 80);

    // The walk stops at the sprite limit
    assert_eq!(cache.len(), 80);
}
