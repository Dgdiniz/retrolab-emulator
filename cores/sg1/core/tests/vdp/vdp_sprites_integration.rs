//! Sprites: the attributes parsed from the sprite table, the list the
//! renderer walks, and sprites drawn by the renderer the console runs.

use super::draw::{self, entry, put_sprite, put_tile, set_colour, shown, solid_tile, tile_with};
use sg1_core::vdp::sprites::{Sprite, SpriteCache};

/// Where the parsing tests put the sprite attribute table.
const SAT: u16 = 0x100;

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
fn a_sprite_s_size_is_its_cells() {
    let mut vram = vec![0u8; 0x10000];
    write_sprite(&mut vram, 0, 150, (2, 2), 0, 0x0000, 160);
    let sprite = Sprite::from_sat(&vram, SAT, 0).unwrap();
    assert_eq!((sprite.width_tiles, sprite.height_tiles), (2, 2));
    let (x1, y1, x2, y2) = sprite.bounds();
    assert_eq!((x2 - x1, y2 - y1), (16, 16));

    write_sprite(&mut vram, 0, 150, (4, 4), 0, 0x0000, 160);
    let sprite = Sprite::from_sat(&vram, SAT, 0).unwrap();
    assert_eq!((sprite.width_tiles, sprite.height_tiles), (4, 4));
    let (x1, y1, x2, y2) = sprite.bounds();
    assert_eq!((x2 - x1, y2 - y1), (32, 32));
}

#[test]
fn the_attribute_word_gives_the_flips_the_palette_and_the_priority() {
    let mut vram = vec![0u8; 0x10000];
    write_sprite(&mut vram, 0, 150, (1, 1), 0, 0x1000, 160);
    assert!(Sprite::from_sat(&vram, SAT, 0).unwrap().flip_v);
    write_sprite(&mut vram, 0, 150, (1, 1), 0, 0x0800, 160);
    assert!(Sprite::from_sat(&vram, SAT, 0).unwrap().flip_h);

    for palette in 0..4u16 {
        write_sprite(
            &mut vram,
            palette as usize,
            150,
            (1, 1),
            0,
            palette << 13,
            160,
        );
        let sprite = Sprite::from_sat(&vram, SAT, palette as u8).unwrap();
        assert_eq!(sprite.palette, palette as u8);
    }

    write_sprite(&mut vram, 0, 150, (1, 1), 0, 0x8000, 160);
    assert!(Sprite::from_sat(&vram, SAT, 0).unwrap().priority);
}

#[test]
fn the_list_follows_the_links() {
    let mut vram = vec![0u8; 0x10000];
    for i in 0..3 {
        let link = if i < 2 { i as u16 + 1 } else { 0 };
        write_sprite(&mut vram, i, 150 + i as u16 * 32, (1, 1), link, 0x0000, 160);
    }
    let mut cache = SpriteCache::new();
    cache.build(&vram, SAT, 80);
    assert_eq!(cache.len(), 3);
}

#[test]
fn a_chain_that_never_ends_stops_at_the_sprite_limit() {
    let mut vram = vec![0u8; 0x10000];
    // 0 -> 1 -> 1 -> ...
    write_sprite(&mut vram, 0, 150, (1, 1), 1, 0x0000, 160);
    write_sprite(&mut vram, 1, 150, (1, 1), 1, 0x0000, 160);
    let mut cache = SpriteCache::new();
    cache.build(&vram, SAT, 80);
    assert_eq!(cache.len(), 80);
}

/// A VDP with the planes empty and tile 1 colour 1, red.
fn screen_with_red_tile() -> sg1_core::vdp::Vdp {
    let mut vdp = draw::vdp();
    put_tile(&mut vdp, 1, &solid_tile(1));
    set_colour(&mut vdp, 1, draw::RED);
    vdp
}

#[test]
fn a_one_cell_sprite_is_drawn_where_its_position_says() {
    let mut vdp = screen_with_red_tile();
    put_sprite(
        &mut vdp,
        0,
        (32, 22),
        (1, 1),
        0,
        entry(1, 0, false, false, false),
    );
    draw::draw(&mut vdp);
    assert_eq!(draw::pixel(&vdp, 32, 22), shown(draw::RED));
    assert_eq!(draw::pixel(&vdp, 39, 29), shown(draw::RED));
    assert_eq!(draw::pixel(&vdp, 40, 22), shown(0));
    assert_eq!(draw::pixel(&vdp, 32, 30), shown(0));
}

#[test]
fn a_sprite_flipped_across_draws_its_left_pixel_on_the_right() {
    let mut vdp = draw::vdp();
    put_tile(&mut vdp, 1, &tile_with(&[(0, 0, 1)]));
    set_colour(&mut vdp, 1, draw::RED);
    put_sprite(
        &mut vdp,
        0,
        (32, 22),
        (1, 1),
        0,
        entry(1, 0, true, false, false),
    );
    draw::draw(&mut vdp);
    assert_eq!(draw::pixel(&vdp, 39, 22), shown(draw::RED));
    assert_eq!(draw::pixel(&vdp, 32, 22), shown(0));
}

#[test]
fn a_sprite_off_the_screen_draws_nothing_and_one_half_off_draws_its_visible_columns() {
    let mut vdp = screen_with_red_tile();
    // Slot 0 at the top left corner of the sprite space, off the screen;
    // slot 1 four pixels past the left edge; slot 2 past the bottom right.
    put_sprite(
        &mut vdp,
        0,
        (-128, -128),
        (1, 1),
        1,
        entry(1, 0, false, false, false),
    );
    put_sprite(
        &mut vdp,
        1,
        (-4, 100),
        (1, 1),
        2,
        entry(1, 0, false, false, false),
    );
    put_sprite(
        &mut vdp,
        2,
        (320, 224),
        (1, 1),
        0,
        entry(1, 0, false, false, false),
    );
    draw::draw(&mut vdp);

    let red = shown(draw::RED);
    for x in 0..4 {
        assert_eq!(draw::pixel(&vdp, x, 100), red, "column {x}: its last four");
    }
    assert_eq!(draw::pixel(&vdp, 4, 100), shown(0));
    let covered = (0..224)
        .flat_map(|y| (0..320).map(move |x| (x, y)))
        .filter(|&(x, y)| draw::pixel(&vdp, x, y) == red)
        .count();
    assert_eq!(covered, 4 * 8, "nothing else of any sprite is drawn");
}

#[test]
fn two_sprites_side_by_side_are_both_drawn() {
    let mut vdp = screen_with_red_tile();
    put_sprite(
        &mut vdp,
        0,
        (32, 22),
        (1, 1),
        1,
        entry(1, 0, false, false, false),
    );
    put_sprite(
        &mut vdp,
        1,
        (48, 22),
        (1, 1),
        0,
        entry(1, 0, false, false, false),
    );
    draw::draw(&mut vdp);
    assert_eq!(draw::pixel(&vdp, 32, 22), shown(draw::RED));
    assert_eq!(draw::pixel(&vdp, 48, 22), shown(draw::RED));
    assert_eq!(draw::pixel(&vdp, 40, 22), shown(0), "the gap between them");
}

#[test]
fn colour_0_of_a_sprite_shows_what_is_behind_it() {
    let mut vdp = draw::vdp();
    // Four pixels of colour 0, then four of colour 1, on every row.
    let mut pattern = [0u8; 32];
    for row in 0..8 {
        pattern[row * 4 + 2] = 0x11;
        pattern[row * 4 + 3] = 0x11;
    }
    put_tile(&mut vdp, 1, &pattern);
    set_colour(&mut vdp, 1, draw::RED);
    set_colour(&mut vdp, 2, draw::BLUE);
    vdp.set_test_register(7, 0x02); // the backdrop is colour 2, blue
    put_sprite(
        &mut vdp,
        0,
        (32, 22),
        (1, 1),
        0,
        entry(1, 0, false, false, false),
    );
    draw::draw(&mut vdp);
    assert_eq!(draw::pixel(&vdp, 32, 22), shown(draw::BLUE));
    assert_eq!(draw::pixel(&vdp, 36, 22), shown(draw::RED));
}
