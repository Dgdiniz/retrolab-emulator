//! Where each pixel came from, the sprites each line held and what became
//! of each, and the chosen sprites drawn alone: worked out on demand by
//! drawing the frame again from its line table with a trace.
//!
//! Each scene is put in video memory, then a frame is drawn line by line
//! with the line states recorded, as the frame loops draw one; the planes
//! and the sprite lists are then read from that frame.

#[allow(dead_code)]
#[path = "vdp/draw.rs"]
mod draw;

use draw::*;
use retro_core::capture::{layer, line_flags, planes, CaptureRecording, SpriteFate, NO_CELL};
use sg1_core::vdp::sources::{
    DRAWN_BY_PLANE_A, DRAWN_BY_PLANE_B, DRAWN_BY_SPRITE, DRAWN_BY_WINDOW, FLIPPED_ACROSS,
    FLIPPED_DOWN, HIGH_PRIORITY, OPERATOR_HIGHLIGHT, OPERATOR_SHADOW, OPERATOR_SHIFT, SHADE_SHIFT,
};
use sg1_core::vdp::{Shade, Vdp, LAYER_SPRITES};

/// Record the line states and draw frame 1 line by line, as the frame
/// loops draw a frame.
fn draw_frame(vdp: &mut Vdp) {
    vdp.set_capture_recording(CaptureRecording {
        line_states: true,
        ..CaptureRecording::default()
    });
    vdp.set_frame_being_drawn(1);
    for line in 0..224 {
        vdp.scanline(line);
    }
}

/// The value at pixel (`x`, `y`) of a plane of the 40-cell picture.
fn at<T: Copy>(plane: &[T], x: usize, y: usize) -> T {
    plane[y * 320 + x]
}

/// The `tile` plane's value for character `pattern` read at `row` and
/// `column`: the word where it starts, the character, the row, the column.
fn tile_of(pattern: u32, row: u32, column: u32) -> u32 {
    (pattern * 16) | (pattern << 15) | (row << 26) | (column << 29)
}

/// The shade bits of the `layer` plane.
fn shaded(shade: Shade) -> u16 {
    (shade as u16) << SHADE_SHIFT
}

/// A scene with a pixel of every kind: plane B, plane A, the window, a
/// sprite and the backdrop.
///
/// - Plane B, cell (0, 0): tile 1, colour 1 of palette line 1.
/// - Plane A, cell (2, 0): tile 2, whose one pixel (column 1, row 2) is
///   colour 3, on palette line 2, flipped both ways and of high priority,
///   so the pixel shows at (22, 5).
/// - The window, from line 200 down, cell (5, 26): tile 3, colour 4 of
///   palette line 3.
/// - Sprite 0: two cells by one at (100, 50), tiles 4 and 5 (colours 5
///   and 6), flipped across, so its left cell shows tile 5.
fn scene() -> Vdp {
    let mut vdp = vdp();
    put_tile(&mut vdp, 1, &solid_tile(1));
    put_tile(&mut vdp, 2, &tile_with(&[(1, 2, 3)]));
    put_tile(&mut vdp, 3, &solid_tile(4));
    put_tile(&mut vdp, 4, &solid_tile(5));
    put_tile(&mut vdp, 5, &solid_tile(6));
    put_cell(
        &mut vdp,
        PLANE_B,
        32,
        0,
        0,
        entry(1, 1, false, false, false),
    );
    put_cell(&mut vdp, PLANE_A, 32, 2, 0, entry(2, 2, true, true, true));
    vdp.set_test_register(18, 0x80 | 25);
    put_cell(
        &mut vdp,
        WINDOW,
        64,
        5,
        26,
        entry(3, 3, false, false, false),
    );
    put_sprite(&mut vdp, 0, (100, 50), (2, 1), 0, 4 | 0x0800);
    for (index, word) in [(17, GREEN), (35, RED), (52, BLUE), (5, WHITE), (6, RED)] {
        set_colour(&mut vdp, index, word);
    }
    vdp
}

#[test]
fn each_pixel_says_which_layer_character_cell_and_colour_it_came_from() {
    let mut vdp = scene();
    draw_frame(&mut vdp);
    let sources = vdp.pixel_sources(planes::MAIN);
    assert!(sources.from_line_states && sources.matches_picture);
    assert_eq!((sources.width, sources.height), (320, 224));
    assert_eq!(sources.frame, 1);
    let layer = sources.layer.expect("asked for");
    let colour = sources.colour.expect("asked for");
    let tile = sources.tile.expect("asked for");
    let cell = sources.cell.expect("asked for");

    // Plane B, its map entry the first word of its table.
    assert_eq!(at(&layer, 3, 4), DRAWN_BY_PLANE_B);
    assert_eq!(at(&colour, 3, 4), 17);
    assert_eq!(at(&tile, 3, 4), tile_of(1, 4, 3));
    assert_eq!(at(&cell, 3, 4), 0xE000 / 2);

    // Plane A's one pixel: the row and column read are the flipped ones.
    assert_eq!(
        at(&layer, 22, 5),
        DRAWN_BY_PLANE_A | HIGH_PRIORITY | FLIPPED_ACROSS | FLIPPED_DOWN
    );
    assert_eq!(at(&colour, 22, 5), 2 * 16 + 3);
    assert_eq!(at(&tile, 22, 5), tile_of(2, 2, 1));
    assert_eq!(at(&cell, 22, 5), (0xC000 + 2 * 2) / 2);

    // The rest of that cell, and an empty place, are the backdrop.
    for (x, y) in [(21, 5), (300, 100)] {
        assert_eq!(at(&layer, x, y), layer::BACKDROP, "({x}, {y})");
        assert_eq!(at(&colour, x, y), 0);
        assert_eq!(at(&tile, x, y), 0);
        assert_eq!(at(&cell, x, y), NO_CELL);
    }

    // The window, whose rows are 64 cells long in the 40-cell mode.
    assert_eq!(at(&layer, 42, 210), DRAWN_BY_WINDOW);
    assert_eq!(at(&colour, 42, 210), 3 * 16 + 4);
    assert_eq!(at(&tile, 42, 210), tile_of(3, 2, 2));
    assert_eq!(at(&cell, 42, 210), (0xB000 + (26 * 64 + 5) * 2) / 2);

    // The sprite, flipped across: its first column is the last column of
    // its second cell, and the cell is its slot.
    assert_eq!(at(&layer, 100, 50), DRAWN_BY_SPRITE | FLIPPED_ACROSS);
    assert_eq!(at(&colour, 100, 50), 6);
    assert_eq!(at(&tile, 100, 50), tile_of(5, 0, 7));
    assert_eq!(at(&cell, 100, 50), 0);
    assert_eq!(at(&tile, 108, 50), tile_of(4, 0, 7));
    assert_eq!(at(&colour, 108, 50), 5);

    // The layer codes are the ones `renderLayer` names, plus one.
    let drawn_by = |x, y| at(&layer, x, y) & layer::WHAT_DREW;
    for (x, y, layer_id) in [(22, 5, 0), (3, 4, 1), (42, 210, 2), (100, 50, 3)] {
        let image = vdp.render_layer_image(layer_id).expect("a layer").image;
        assert_eq!(image.rgba[(y * 320 + x) * 4 + 3], 255, "layer {layer_id}");
        assert_eq!(drawn_by(x, y), 1 + layer_id as u16);
    }
}

#[test]
fn only_the_planes_asked_for_are_worked_out() {
    let mut vdp = scene();
    draw_frame(&mut vdp);
    let sources = vdp.pixel_sources(planes::LAYER | 16);
    assert!(sources.layer.is_some());
    assert!(sources.colour.is_none() && sources.tile.is_none() && sources.cell.is_none());
    assert_eq!(
        sources.layer,
        vdp.pixel_sources(planes::MAIN).layer,
        "the same layer plane"
    );
}

#[test]
fn shadow_and_highlight_say_what_they_did_to_each_pixel() {
    let mut vdp = vdp();
    vdp.set_test_register(12, 0x89); // 40 cells, shadow and highlight
    put_tile(&mut vdp, 1, &solid_tile(1));
    put_tile(&mut vdp, 2, &solid_tile(2));
    put_tile(&mut vdp, 6, &solid_tile(14));
    put_tile(&mut vdp, 7, &solid_tile(15));
    // Plane B, cells 0 to 3 of row 0, low priority: shadowed.
    for column in 0..4 {
        put_cell(
            &mut vdp,
            PLANE_B,
            32,
            column,
            0,
            entry(1, 0, false, false, false),
        );
    }
    // Plane A, cell 5, high priority: as it is.
    put_cell(&mut vdp, PLANE_A, 32, 5, 0, entry(2, 0, false, false, true));
    // Over plane B's cell 0, the highlight operator (palette line 3,
    // colour 14); over its cell 2, the shadow operator (colour 15).
    put_sprite(&mut vdp, 0, (0, 0), (1, 1), 1, 6 | (3 << 13));
    put_sprite(&mut vdp, 1, (16, 0), (1, 1), 0, 7 | (3 << 13));
    set_colour(&mut vdp, 1, RED);
    set_colour(&mut vdp, 2, GREEN);
    draw_frame(&mut vdp);
    let sources = vdp.pixel_sources(planes::MAIN);
    assert!(sources.matches_picture);
    let layer = sources.layer.expect("asked for");
    let colour = sources.colour.expect("asked for");
    let cell = sources.cell.expect("asked for");

    // An operator is no pixel: the pixel is plane B's, the operator named.
    let highlight = (OPERATOR_HIGHLIGHT << OPERATOR_SHIFT) | shaded(Shade::Highlighted);
    assert_eq!(at(&layer, 1, 1), DRAWN_BY_PLANE_B | highlight);
    assert_eq!(at(&colour, 1, 1), 1);
    assert_eq!(at(&cell, 1, 1), 0xE000 / 2);
    assert_eq!(pixel(&vdp, 1, 1), highlighted(shown(RED)));

    assert_eq!(at(&layer, 9, 1), DRAWN_BY_PLANE_B | shaded(Shade::Shadowed));
    assert_eq!(pixel(&vdp, 9, 1), shadowed(shown(RED)));

    // This core shows the pixel under the shadow operator as it is.
    let shadow_operator = OPERATOR_SHADOW << OPERATOR_SHIFT;
    assert_eq!(at(&layer, 17, 1), DRAWN_BY_PLANE_B | shadow_operator);
    assert_eq!(pixel(&vdp, 17, 1), shown(RED));

    assert_eq!(at(&layer, 41, 1), DRAWN_BY_PLANE_A | HIGH_PRIORITY);
    assert_eq!(pixel(&vdp, 41, 1), shown(GREEN));

    // The backdrop is of low priority too.
    assert_eq!(
        at(&layer, 300, 100),
        layer::BACKDROP | shaded(Shade::Shadowed)
    );
}

/// Half of each level, as the renderer shadows.
fn shadowed((red, green, blue): (u8, u8, u8)) -> (u8, u8, u8) {
    (red / 2, green / 2, blue / 2)
}

/// Half again of each level, at most 255, as the renderer highlights.
fn highlighted((red, green, blue): (u8, u8, u8)) -> (u8, u8, u8) {
    let up = |level: u8| (u16::from(level) * 3 / 2).min(255) as u8;
    (up(red), up(green), up(blue))
}

/// One entry of a line of [`retro_core::capture::LineSprites`]: the slot, the
/// fate, the x, the width and the drawn width.
type Entry = (u8, u8, i16, u8, u8);

/// What one line of [`retro_core::capture::LineSprites`] holds: its flags and its
/// entries. The frame's lines were all evaluated from the table, and the
/// traced redraw draws the frame's picture.
fn line_entries(vdp: &mut Vdp, line: usize) -> (u16, Vec<Entry>) {
    let sources = vdp.pixel_sources(planes::LAYER);
    assert!(sources.from_line_states && sources.matches_picture);
    let sprites = vdp.line_sprites();
    assert!(sprites.from_line_states);
    let row = &sprites.lines[line * 4..line * 4 + 4];
    assert_eq!(usize::from(row[0]), line);
    let first = usize::from(row[1]);
    let entries = (first..first + usize::from(row[2]))
        .map(|at| {
            (
                sprites.slots[at],
                sprites.fates[at],
                sprites.x[at],
                sprites.width[at],
                sprites.drawn_width[at],
            )
        })
        .collect();
    (row[3], entries)
}

/// A chain of `count` sprites on line 100, one cell tall and `cells` cells
/// wide, of solid tiles from tile 1, sprite `n` in slot `n` at x
/// `n * step`.
fn chain_on_line_100(vdp: &mut Vdp, count: u16, cells: u16, step: i16) {
    for tile in 1..=cells {
        put_tile(vdp, tile, &solid_tile(1));
    }
    set_colour(vdp, 1, RED);
    for slot in 0..count {
        let link = if slot + 1 == count { 0 } else { slot + 1 };
        put_sprite(vdp, slot, (slot as i16 * step, 100), (cells, 1), link, 1);
    }
}

#[test]
fn the_21st_sprite_of_a_line_in_40_cells_and_the_17th_in_32_are_over_the_limit() {
    for (register_12, shown_per_line) in [(0x81u8, 20u16), (0x00, 16)] {
        let mut vdp = vdp();
        vdp.set_test_register(12, register_12);
        chain_on_line_100(&mut vdp, shown_per_line + 1, 1, 12);
        draw_frame(&mut vdp);
        assert!(vdp.line_sprites().from_line_states);

        let (flags, entries) = line_entries(&mut vdp, 100);
        assert_eq!(
            flags,
            line_flags::OVER_LIMIT,
            "register 12 ${register_12:02X}"
        );
        assert_eq!(entries.len(), usize::from(shown_per_line) + 1);
        for (n, &(slot, fate, x, width, drawn)) in entries.iter().enumerate() {
            assert_eq!(usize::from(slot), n);
            assert_eq!(x, n as i16 * 12);
            assert_eq!(width, 8);
            if n < usize::from(shown_per_line) {
                assert_eq!(
                    (fate, drawn),
                    (SpriteFate::DrawnWhole as u8, 8),
                    "sprite {n}"
                );
            } else {
                assert_eq!((fate, drawn), (SpriteFate::OverLineLimit as u8, 0));
            }
        }
        // The picture agrees: the last one is not drawn.
        let last = usize::from(shown_per_line) * 12;
        let layer = vdp.pixel_sources(planes::MAIN).layer.expect("asked for");
        assert_eq!(
            at_width(&layer, &vdp, last - 12, 100) & layer::WHAT_DREW,
            DRAWN_BY_SPRITE
        );
        assert_eq!(
            at_width(&layer, &vdp, last + 4, 100) & layer::WHAT_DREW,
            layer::BACKDROP
        );

        // A line with none of them lists none.
        assert_eq!(line_entries(&mut vdp, 99), (0, Vec::new()));
    }
}

/// The value at pixel (`x`, `y`) of a plane of the picture as wide as the
/// VDP's mode.
fn at_width<T: Copy>(plane: &[T], vdp: &Vdp, x: usize, y: usize) -> T {
    plane[y * vdp.display_width() as usize + x]
}

#[test]
fn the_sprite_the_dots_run_out_in_is_drawn_in_part_and_the_ones_after_left_out() {
    // Sixteen sprites three cells wide on line 100, 16 pixels apart: the
    // first thirteen use 312 of the 320 dots, the 14th crosses with one
    // cell's dots left, and the last two come after.
    let mut vdp = vdp();
    chain_on_line_100(&mut vdp, 16, 3, 16);
    draw_frame(&mut vdp);
    let (flags, entries) = line_entries(&mut vdp, 100);
    assert_eq!(flags, line_flags::OUT_OF_TIME);
    let fates: Vec<u8> = entries.iter().map(|entry| entry.1).collect();
    let mut expected = vec![SpriteFate::DrawnWhole as u8; 13];
    expected.push(SpriteFate::DrawnInPart as u8);
    expected.extend([SpriteFate::LeftOut as u8; 2]);
    assert_eq!(fates, expected);
    let drawn: Vec<u8> = entries.iter().map(|entry| entry.4).collect();
    let mut expected_drawn = vec![24u8; 13];
    expected_drawn.extend([8, 0, 0]);
    assert_eq!(drawn, expected_drawn);

    // The picture shows the clip: the 14th stops after its first cell.
    let sources = vdp.pixel_sources(planes::MAIN);
    let (layer, cell) = (sources.layer.unwrap(), sources.cell.unwrap());
    assert_eq!(at(&layer, 212, 100) & layer::WHAT_DREW, DRAWN_BY_SPRITE);
    assert_eq!(at(&cell, 212, 100), 12, "the 13th is in front");
    assert_eq!(at(&layer, 220, 100) & layer::WHAT_DREW, layer::BACKDROP);

    // The line below gets the sprite mask carried in.
    let (flags, _) = line_entries(&mut vdp, 101);
    assert_eq!(flags, line_flags::OUT_OF_TIME | line_flags::MASK_CARRIED_IN);
}

#[test]
fn a_sprite_at_x_0_of_the_table_hides_itself_and_the_sprites_after_it() {
    // Sprite 0 at x 50 arms the mask; sprite 1 at the table's x 0 (screen
    // -128) then hides itself and sprite 2.
    let mut vdp = vdp();
    put_tile(&mut vdp, 1, &solid_tile(1));
    set_colour(&mut vdp, 1, RED);
    put_sprite(&mut vdp, 0, (50, 100), (1, 1), 1, 1);
    put_sprite(&mut vdp, 1, (-128, 100), (1, 1), 2, 1);
    put_sprite(&mut vdp, 2, (80, 100), (1, 1), 0, 1);
    draw_frame(&mut vdp);
    let (flags, entries) = line_entries(&mut vdp, 100);
    assert_eq!(flags, 0);
    assert_eq!(
        entries,
        [
            (0, SpriteFate::DrawnWhole as u8, 50, 8, 8),
            (1, SpriteFate::Masked as u8, -128, 8, 0),
            (2, SpriteFate::Masked as u8, 80, 8, 0),
        ]
    );
    let layer = vdp.pixel_sources(planes::MAIN).layer.unwrap();
    assert_eq!(at(&layer, 52, 100) & layer::WHAT_DREW, DRAWN_BY_SPRITE);
    assert_eq!(at(&layer, 82, 100) & layer::WHAT_DREW, layer::BACKDROP);
}

#[test]
fn a_sprite_off_the_screen_is_listed_off_screen() {
    let mut vdp = vdp();
    put_tile(&mut vdp, 1, &solid_tile(1));
    put_sprite(&mut vdp, 0, (330, 100), (1, 1), 1, 1);
    put_sprite(&mut vdp, 1, (-16, 100), (2, 1), 0, 1);
    draw_frame(&mut vdp);
    let (_, entries) = line_entries(&mut vdp, 100);
    assert_eq!(
        entries,
        [
            (0, SpriteFate::OffScreen as u8, 330, 8, 0),
            (1, SpriteFate::OffScreen as u8, -16, 16, 0),
        ]
    );
}

/// Two sprites of two cells by two: slot 0 (colour 1) at (100, 100) in
/// front of slot 1 (colour 2) at (108, 104).
fn overlapping_sprites() -> Vdp {
    let mut vdp = vdp();
    for tile in 1..=4 {
        put_tile(&mut vdp, tile, &solid_tile(1));
        put_tile(&mut vdp, tile + 4, &solid_tile(2));
    }
    set_colour(&mut vdp, 1, RED);
    set_colour(&mut vdp, 2, GREEN);
    put_sprite(&mut vdp, 0, (100, 100), (2, 2), 1, 1);
    put_sprite(&mut vdp, 1, (108, 104), (2, 2), 0, 5);
    draw_frame(&mut vdp);
    vdp
}

#[test]
fn every_sprite_chosen_is_the_sprite_layer_and_one_chosen_shows_what_it_drew() {
    let mut every_kind = scene();
    draw_frame(&mut every_kind);
    let mut scenes = vec![overlapping_sprites(), every_kind];
    let mut limited = vdp();
    chain_on_line_100(&mut limited, 21, 1, 12);
    draw_frame(&mut limited);
    scenes.push(limited);
    let mut clipped = vdp();
    chain_on_line_100(&mut clipped, 16, 3, 16);
    draw_frame(&mut clipped);
    scenes.push(clipped);
    for vdp in &mut scenes {
        let layer = vdp.render_layer_image(LAYER_SPRITES).expect("the sprites");
        let all = vdp.render_sprites(&[1; 80]);
        assert!(all.from_line_states);
        assert_eq!(all.image, layer.image);
    }

    // The sprite behind shows only where the one in front does not cover.
    let vdp = &mut scenes[0];
    let rgba = |image: &[u8], x: usize, y: usize| {
        let at = (y * 320 + x) * 4;
        [image[at], image[at + 1], image[at + 2], image[at + 3]]
    };
    let green = shown(GREEN);
    let red = shown(RED);
    let behind = vdp.render_sprites(&[0, 1]).image.rgba;
    assert_eq!(rgba(&behind, 110, 106), [0; 4], "under the one in front");
    assert_eq!(rgba(&behind, 120, 106), [green.0, green.1, green.2, 255]);
    assert_eq!(rgba(&behind, 101, 101), [0; 4]);
    let in_front = vdp.render_sprites(&[1]).image.rgba;
    assert_eq!(rgba(&in_front, 110, 106), [red.0, red.1, red.2, 255]);
    assert_eq!(rgba(&in_front, 120, 106), [0; 4]);
    assert!(vdp
        .render_sprites(&[])
        .image
        .rgba
        .iter()
        .all(|&byte| byte == 0));
}

#[test]
fn working_them_out_moves_neither_the_picture_nor_the_state() {
    for mut vdp in [scene(), overlapping_sprites()] {
        draw_frame(&mut vdp);
        let picture = vdp.framebuffer().to_vec();
        let state = serde_json::to_string(&vdp.to_savestate()).expect("serializes");
        let sources = vdp.pixel_sources(planes::MAIN);
        assert!(sources.matches_picture);
        vdp.line_sprites();
        vdp.render_sprites(&[1; 80]);
        assert_eq!(vdp.framebuffer(), &picture[..]);
        assert_eq!(
            serde_json::to_string(&vdp.to_savestate()).expect("serializes"),
            state
        );
        // The picture drawn again from the table is the same one.
        assert_eq!(
            vdp.picture_from_line_states().expect("armed").rgba,
            picture[..320 * 224 * 4]
        );
    }
}

#[test]
fn without_a_table_the_state_now_and_a_line_with_no_record_reads_nothing() {
    // No table: the frame drawn from the state as it is now.
    let mut vdp = scene();
    draw(&mut vdp);
    let sources = vdp.pixel_sources(planes::MAIN);
    assert!(!sources.from_line_states && sources.matches_picture);
    let layer = sources.layer.expect("asked for");
    assert_eq!(at(&layer, 22, 5) & layer::WHAT_DREW, DRAWN_BY_PLANE_A);
    assert!(!vdp.line_sprites().from_line_states);

    // Armed part of the way down: the lines above have no record.
    let mut vdp = scene();
    for line in 0..100 {
        vdp.scanline(line);
    }
    vdp.set_capture_recording(CaptureRecording {
        pixel_sources: true,
        ..CaptureRecording::default()
    });
    vdp.set_frame_being_drawn(1);
    for line in 100..224 {
        vdp.scanline(line);
    }
    let sources = vdp.pixel_sources(planes::MAIN);
    assert!(!sources.from_line_states);
    assert!(
        sources.matches_picture,
        "the state now draws the same lines"
    );
    let layer = sources.layer.expect("asked for");
    let cell = sources.cell.expect("asked for");
    for y in 0..100 {
        assert!(
            (0..320).all(|x| at(&layer, x, y) == layer::NOTHING),
            "line {y}"
        );
        assert!((0..320).all(|x| at(&cell, x, y) == NO_CELL));
    }
    assert_eq!(at(&layer, 42, 210) & layer::WHAT_DREW, DRAWN_BY_WINDOW);
}

#[test]
fn a_line_drawn_with_the_display_off_reads_nothing_and_its_backdrop() {
    let mut vdp = scene();
    vdp.set_test_register(7, 0x21); // backdrop: palette line 2, colour 1
    vdp.set_capture_recording(CaptureRecording {
        line_states: true,
        ..CaptureRecording::default()
    });
    vdp.set_frame_being_drawn(1);
    for line in 0..224 {
        // The display off from line 100 to line 149.
        let register_1 = if (100..150).contains(&line) {
            0x04
        } else {
            0x44
        };
        vdp.set_test_register(1, register_1);
        vdp.scanline(line);
    }
    let sources = vdp.pixel_sources(planes::MAIN);
    assert!(sources.from_line_states && sources.matches_picture);
    let (layer, colour) = (sources.layer.unwrap(), sources.colour.unwrap());
    assert!((0..320).all(|x| at(&layer, x, 120) == layer::NOTHING));
    assert!((0..320).all(|x| at(&colour, x, 120) == 2 * 16 + 1));
    assert_eq!(at(&layer, 300, 160), layer::BACKDROP);
    let (flags, entries) = line_entries(&mut vdp, 120);
    assert_eq!((flags, entries.len()), (0, 0), "no sprite evaluated");
}
