//! Unit tests of interlace mode 2 (double resolution): the mode a frame
//! draws with, both fields of every line drawn into the picture's rows,
//! cells 8 pixels by 16, the sprite table's 10-bit Y, the flags a game
//! reads, the picture drawn again and where its pixels came from, the
//! sprite boxes and images, and the vertical counter.

use super::*;
use retro_core::capture::{planes, tile_value, CaptureRecording};
use retro_core::video::{PictureRect, VideoDebugger};
use retro_core::PictureGrid;

/// The tables of [`double_resolution_vdp`]'s picture.
const PLANE_A: u32 = 0xC000;
const PLANE_B: u32 = 0xE000;
const WINDOW: u32 = 0xB000;
const SPRITES: u32 = 0xF000;

/// The picture's width and rows: 40 cells, 224 lines of two rows.
const WIDTH: u32 = 320;
const ROWS: u32 = 448;

/// The colour word of CRAM entry `index`, a different one for each.
fn colour_word(index: u32) -> u16 {
    (((index & 7) << 1) | (((index >> 3) & 7) << 5) | (((index * 3) & 7) << 9)) as u16
}

/// Cell 1's colour at row `row` (0-15) and column `column`.
fn plane_colour(row: u32, column: u32) -> u32 {
    1 + (row + column) % 15
}

/// Cell 4's colour, the window's.
fn window_colour(row: u32, column: u32) -> u32 {
    1 + (2 * row + 3 * column) % 15
}

/// The colour of cell `cell` of the sprites (2, 3, 5 to 8).
fn sprite_colour(cell: u32, row: u32, column: u32) -> u32 {
    1 + (3 * cell + row + 2 * column) % 15
}

/// Put cell `cell` (16 rows of 4 bytes, from byte `cell * 64`), each
/// pixel `colour(row, column)`.
fn put_tall_cell(vdp: &mut Vdp, cell: u32, colour: impl Fn(u32, u32) -> u32) {
    let mut bytes = [0u8; 64];
    for row in 0..16 {
        for column in 0..8 {
            let value = colour(row, column) as u8;
            let byte = &mut bytes[(row * 4 + column / 2) as usize];
            *byte |= if column % 2 == 0 { value << 4 } else { value };
        }
    }
    vdp.write_vram_debug(cell * 64, &bytes);
}

/// Put map entry `word` at (`column`, `row`) of the table at `table`, its
/// rows `columns` cells long.
fn put_entry(vdp: &mut Vdp, table: u32, columns: u32, (column, row): (u32, u32), word: u16) {
    vdp.write_vram_debug(table + (row * columns + column) * 2, &word.to_be_bytes());
}

/// Put sprite `slot` with its table words: `y` (the 10-bit Y), the size
/// and link word, the attributes and `x` (the 9-bit X).
fn put_sprite(vdp: &mut Vdp, slot: u32, words: [u16; 4]) {
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_be_bytes()).collect();
    vdp.write_vram_debug(SPRITES + slot * 8, &bytes);
}

/// A VDP in interlace mode 2, 40 cells wide and 28 lines tall:
/// - planes of 32 by 32 cells; plane A at `$C000`, every cell cell 1 of
///   palette line 0 but those of column 0, empty, scrolled up 5 rows;
///   plane B at `$E000`, every cell cell 1 flipped down, palette line 3;
/// - the window at `$B000` over the lines from 208 (the rows from 416),
///   every cell cell 4 of palette line 2;
/// - sprite 0 at x 40 and row 101 (Y 357), one cell by two (8 pixels by
///   32 rows), cells 2 and 3, palette line 1; sprite 1, not in the chain,
///   two cells by two, flipped both ways, cells 5 to 8, palette line 1;
/// - CRAM entry i the colour [`colour_word`] of i.
fn double_resolution_vdp() -> Vdp {
    let mut vdp = Vdp::new();
    for (register, value) in [
        (1, 0x44),  // display on, mode 5
        (2, 0x30),  // plane A at $C000
        (3, 0x2C),  // window at $B000
        (4, 0x07),  // plane B at $E000
        (5, 0x78),  // sprite table at $F000
        (7, 0x00),  // backdrop: colour 0
        (11, 0x00), // whole-screen scrolls
        (12, 0x87), // 40 cells, interlace mode 2
        (13, 0x3F), // horizontal scroll at $FC00
        (16, 0x00), // planes of 32 by 32 cells
        (17, 0x00), // no window columns
        (18, 0x9A), // the window over the lines from 26 cells of 8 down
    ] {
        vdp.set_test_register(register, value);
    }
    put_tall_cell(&mut vdp, 1, plane_colour);
    put_tall_cell(&mut vdp, 4, window_colour);
    for cell in [2, 3, 5, 6, 7, 8] {
        put_tall_cell(&mut vdp, cell, |row, column| {
            sprite_colour(cell, row, column)
        });
    }
    for row in 0..32 {
        for column in 0..32 {
            let a = if column == 0 { 0x0000 } else { 0x0001 };
            put_entry(&mut vdp, PLANE_A, 32, (column, row), a);
            put_entry(&mut vdp, PLANE_B, 32, (column, row), 0x7001);
        }
        for column in 0..40 {
            put_entry(&mut vdp, WINDOW, 64, (column, row), 0x4004);
        }
    }
    vdp.write_vsram_debug(0, &[5, 0]);
    put_sprite(&mut vdp, 0, [256 + 101, 0x0100, 0x2002, 128 + 40]);
    put_sprite(&mut vdp, 1, [256 + 300, 0x0500, 0x3805, 128 + 200]);
    let colours: Vec<u16> = (0..64).map(colour_word).collect();
    vdp.write_cram_debug(0, &colours);
    vdp
}

/// The CRAM entry the picture of [`double_resolution_vdp`] shows at row
/// `row` and column `x`, worked out by hand from the module's rules.
fn expected_entry(row: u32, x: u32) -> u32 {
    if (101..133).contains(&row) && (40..48).contains(&x) {
        // Sprite 0, in front of plane A: its cell of the row, unflipped.
        let rows_down = row - 101;
        return 16 + sprite_colour(2 + rows_down / 16, rows_down % 16, x - 40);
    }
    if row / 2 >= 208 {
        // The window, from its cell row 26.
        return 32 + window_colour(row % 16, x % 8);
    }
    let column = (x / 8) % 32;
    if column != 0 {
        // Plane A, five rows down its cells.
        return plane_colour((row + 5) % 16, x % 8);
    }
    // Plane B, through plane A's empty column, its cell flipped down.
    48 + plane_colour(15 - row % 16, x % 8)
}

/// Draw the frame's 224 lines as the frame loop draws them.
fn draw_frame(vdp: &mut Vdp) {
    for line in 0..224 {
        vdp.scanline(line);
    }
}

/// The framebuffer's first `rows` rows, 320 pixels each.
fn picture(vdp: &Vdp, rows: u32) -> Vec<u8> {
    vdp.framebuffer[..(WIDTH * rows * 4) as usize].to_vec()
}

#[test]
fn the_interlace_mode_is_register_12_s_as_the_frame_began() {
    let mut vdp = Vdp::new();
    assert_eq!(vdp.interlace(), Interlace::Off);
    vdp.set_test_register(12, 0x87);
    assert_eq!(
        vdp.interlace(),
        Interlace::Off,
        "a change shows from the next frame"
    );
    assert_eq!(vdp.picture_grid(), PictureGrid::ONE);
    vdp.scanline(0);
    assert_eq!(vdp.interlace(), Interlace::Double);
    let woven = PictureGrid {
        columns_per_dot: 1,
        rows_per_line: 2,
        woven: true,
        first_dot: 0,
        first_line: 0,
    };
    assert_eq!(
        vdp.picture_grid(),
        woven,
        "starting at the first line drawn"
    );
    assert_eq!((vdp.drawn_height(), vdp.picture_height()), (224, 448));
    vdp.set_test_register(1, 0x4C);
    vdp.scanline(0);
    assert_eq!((vdp.drawn_height(), vdp.picture_height()), (240, 480));
    for (value, mode) in [
        (0x81, Interlace::Off),
        (0x83, Interlace::Normal),
        (0x85, Interlace::Off),
        (0x8B, Interlace::Normal),
        (0x8F, Interlace::Double),
    ] {
        vdp.set_test_register(12, value);
        vdp.scanline(0);
        assert_eq!(vdp.interlace(), mode, "register 12 {value:#04X}");
        let rows = if mode == Interlace::Double { 2 } else { 1 };
        assert_eq!(vdp.picture_grid().rows_per_line, rows);
    }

    // A load and a reset take it from register 12.
    vdp.set_test_register(12, 0x81);
    vdp.scanline(0);
    vdp.set_test_register(12, 0x87);
    let saved = vdp.to_savestate();
    let mut loaded = Vdp::new();
    loaded.from_savestate(&saved);
    assert_eq!(loaded.interlace(), Interlace::Double);
    vdp.scanline(0);
    vdp.reset();
    assert_eq!(
        vdp.interlace(),
        Interlace::Off,
        "a reset sets register 12 to $81"
    );
}

#[test]
fn a_frame_in_interlace_mode_2_draws_both_fields_of_every_line() {
    let mut vdp = double_resolution_vdp();
    draw_frame(&mut vdp);
    for row in 0..ROWS {
        for x in 0..WIDTH {
            let at = ((row * WIDTH + x) * 4) as usize;
            let shown = (
                vdp.framebuffer[at],
                vdp.framebuffer[at + 1],
                vdp.framebuffer[at + 2],
            );
            let entry = expected_entry(row, x);
            assert_eq!(
                shown,
                Vdp::convert_genesis_color(colour_word(entry)),
                "row {row}, x {x}: CRAM entry {entry}"
            );
        }
    }
}

#[test]
fn the_picture_drawn_again_is_the_frame_s_with_both_rows_of_each_line() {
    let mut vdp = double_resolution_vdp();
    vdp.set_capture_recording(CaptureRecording {
        line_states: true,
        ..CaptureRecording::default()
    });
    vdp.set_frame_being_drawn(1);
    draw_frame(&mut vdp);
    let drawn = picture(&vdp, ROWS);

    let table = vdp.line_state_table().expect("the line table is armed");
    assert!(table.complete);
    assert_eq!(table.rows_per_line, 2);
    let again = vdp
        .picture_from_line_states()
        .expect("the table holds the frame");
    assert_eq!((again.width, again.height), (WIDTH, ROWS));
    assert!(again.rgba == drawn, "the picture from the line states");
    assert!(vdp.rerender_current_frame());
    assert!(picture(&vdp, ROWS) == drawn, "the repaint");

    let sources = vdp.pixel_sources(planes::LAYER | planes::TILE | planes::CELL);
    assert_eq!((sources.width, sources.height), (WIDTH, ROWS));
    assert!(sources.from_line_states && sources.matches_picture);
    let tile = sources.tile.expect("asked for");
    let cell = sources.cell.expect("asked for");
    let at = |row: u32, x: u32| (row * WIDTH + x) as usize;
    // Plane A at row 6 reads row 11 of cell 1: its second character (3),
    // the character's row 3.
    assert_eq!(tile[at(6, 9)], tile_value(3 * 16, 3, 3, 1));
    // Plane B at row 2, flipped down, reads row 13: character 3, row 5.
    assert_eq!(tile[at(2, 0)], tile_value(3 * 16, 3, 5, 0));
    // Sprite 0's first row is cell 2's (characters 4 and 5) row 0, and
    // its 18th row is cell 3's row 1: character 6.
    assert_eq!(tile[at(101, 40)], tile_value(4 * 16, 4, 0, 0));
    assert_eq!(tile[at(118, 41)], tile_value(6 * 16, 6, 1, 1));
    assert_eq!(cell[at(118, 41)], 0, "the sprite's slot");
}

#[test]
fn a_sprite_s_box_and_image_follow_the_woven_grid() {
    let mut vdp = double_resolution_vdp();
    draw_frame(&mut vdp);
    // Sprite 0 covers rows 101 to 132: lines 50 to 66, rows 100 to 133.
    let boxed = PictureRect {
        x: 40,
        y: 100,
        width: 8,
        height: 34,
    };
    let list = vdp.get_sprite_data();
    let table = vdp.sprite_table();
    let listed = list.iter().find(|sprite| sprite.index == 0).unwrap();
    assert_eq!(
        (listed.x, listed.y, listed.width, listed.height),
        (40, 50, 8, 16),
        "dots and lines"
    );
    assert_eq!(listed.picture, boxed);
    assert_eq!(table[0].picture, boxed);
    assert_eq!((table[0].y, table[0].tile_address), (50, 2 * 64));
    // A sprite on an even row is boxed by its rows alone: sprite 1 at row
    // 300, 32 rows.
    let second = list.iter().find(|sprite| sprite.index == 1).unwrap();
    assert_eq!(
        second.picture,
        PictureRect {
            x: 200,
            y: 300,
            width: 16,
            height: 32,
        }
    );

    // Sprite 0's image is the picture's pixels under it.
    let image = vdp.export_sprite_image(0).expect("slot 0");
    assert_eq!((image.width, image.height), (8, 32));
    for y in 0..32 {
        for x in 0..8 {
            let at = ((y * 8 + x) * 4) as usize;
            let screen = (((101 + y) * WIDTH + 40 + x) * 4) as usize;
            assert_eq!(
                image.rgba[at..at + 3],
                vdp.framebuffer[screen..screen + 3],
                "x {x}, y {y}"
            );
            assert_eq!(image.rgba[at + 3], 255);
        }
    }
    // Sprite 1, two cells by two flipped both ways, by hand: its pixel at
    // (x, y) is the unflipped sprite's at (15 - x, 31 - y), cell 5 + its
    // column of cells times two plus its row of cells.
    let image = vdp.export_sprite_image(1).expect("slot 1");
    assert_eq!((image.width, image.height), (16, 32));
    for y in 0..32 {
        for x in 0..16 {
            let (across, down) = (15 - x, 31 - y);
            let cell = 5 + (across / 8) * 2 + down / 16;
            let entry = 16 + sprite_colour(cell, down % 16, across % 8);
            let (red, green, blue) = Vdp::convert_genesis_color(colour_word(entry));
            let at = ((y * 16 + x) * 4) as usize;
            assert_eq!(
                image.rgba[at..at + 4],
                [red, green, blue, 255],
                "x {x}, y {y}"
            );
        }
    }
}

#[test]
fn the_flags_a_game_reads_are_those_of_the_line_s_own_field() {
    for odd in [false, true] {
        let mut vdp = Vdp::new();
        for (register, value) in [(1, 0x44), (5, 0x78), (12, 0x87)] {
            vdp.set_test_register(register, value);
        }
        put_tall_cell(&mut vdp, 1, |_, _| 1);
        // 21 sprites of one cell on rows 1 to 16: on line 0's odd row, not
        // on its even row. Only 20 fit a row.
        for slot in 0..21 {
            let link = if slot < 20 { slot + 1 } else { 0 };
            put_sprite(&mut vdp, slot as u32, [257, link, 0x0001, 128 + 8 * slot]);
        }
        vdp.odd_frame = odd;
        vdp.scanline(0);
        assert_eq!(
            vdp.sprite_overflow,
            odd,
            "the overflow of the line's own field, the {} one",
            if odd { "odd" } else { "even" }
        );
        assert_eq!(vdp.field_drawn, u8::from(odd));
    }
}

#[test]
fn the_vertical_counter_follows_the_interlace_mode() {
    let mut vdp = Vdp::new();
    // NTSC, 28 lines: the counter jumps from $EA to $E5 at line 235.
    let lines = [100, 101, 129, 234, 235, 240];
    for (register_12, expected) in [
        (0x81, [0x64, 0x65, 0x81, 0xEA, 0xE5, 0xEA]),
        // Interlace mode 1: bit 0 shows bit 8, set past the jump.
        (0x83, [0x64, 0x64, 0x80, 0xEA, 0xE5, 0xEB]),
        // Interlace mode 2: the count doubled, its bit 8 in bit 0.
        (0x87, [0xC8, 0xCA, 0x03, 0xD5, 0xCB, 0xD5]),
    ] {
        vdp.set_test_register(12, register_12);
        vdp.scanline(0);
        for (line, value) in lines.into_iter().zip(expected) {
            vdp.timing.scanline = line;
            let read = (vdp.read_word_port(0x08) >> 8) as u8;
            assert_eq!(
                read, value,
                "register 12 {register_12:#04X}, line {line}: {read:#04X}"
            );
        }
    }
}
