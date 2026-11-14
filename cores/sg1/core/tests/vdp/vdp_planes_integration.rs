//! A plane drawn by the renderer the console runs: tiles, scrolling,
//! wrapping, palettes, flips, plane sizes and transparency.
//!
//! A horizontal scroll moves the plane right: column `x` of the screen
//! shows column `x - scroll` of the plane, wrapped to its width. A vertical
//! scroll moves it up: line `y` shows line `y + scroll`, wrapped to its
//! height.

use super::draw::*;

#[test]
fn tiles_side_by_side_are_drawn_each_in_its_cell() {
    let mut vdp = vdp();
    put_tile(&mut vdp, 1, &solid_tile(1));
    put_tile(&mut vdp, 2, &solid_tile(2));
    for row in 0..4 {
        for column in 0..4 {
            let tile = if (column + row) % 2 == 0 { 1 } else { 2 };
            let value = entry(tile, 0, false, false, false);
            put_cell(&mut vdp, PLANE_A, 32, column, row, value);
        }
    }
    set_colour(&mut vdp, 1, RED);
    set_colour(&mut vdp, 2, GREEN);
    draw(&mut vdp);

    assert_eq!(pixel(&vdp, 0, 0), shown(RED));
    assert_eq!(pixel(&vdp, 8, 0), shown(GREEN));
    assert_eq!(pixel(&vdp, 8, 8), shown(RED));
    assert_eq!(pixel(&vdp, 31, 31), shown(RED));
    assert_eq!(
        pixel(&vdp, 32, 0),
        shown(0),
        "cell 4 is tile 0: the backdrop"
    );
}

#[test]
fn a_horizontal_scroll_moves_the_plane_right() {
    let mut vdp = vdp();
    put_tile(&mut vdp, 1, &solid_tile(1));
    for column in 0..10 {
        put_cell(
            &mut vdp,
            PLANE_A,
            32,
            column,
            0,
            entry(1, 0, false, false, false),
        );
    }
    set_colour(&mut vdp, 1, RED);

    draw(&mut vdp);
    assert_eq!(pixel(&vdp, 0, 0), shown(RED));
    assert_eq!(pixel(&vdp, 79, 0), shown(RED), "cell 9");
    assert_eq!(pixel(&vdp, 80, 0), shown(0), "cell 10 is empty");

    scroll_across(&mut vdp, 8, 0);
    draw(&mut vdp);
    assert_eq!(
        pixel(&vdp, 0, 0),
        shown(0),
        "the plane's last column, 31, comes in from the left"
    );
    assert_eq!(pixel(&vdp, 8, 0), shown(RED), "cell 0 is one cell right");
    assert_eq!(pixel(&vdp, 87, 0), shown(RED), "cell 9");
    assert_eq!(pixel(&vdp, 88, 0), shown(0), "cell 10");
}

#[test]
fn a_vertical_scroll_moves_the_plane_up() {
    let mut vdp = vdp();
    put_tile(&mut vdp, 1, &solid_tile(1));
    for row in 0..10 {
        for column in 0..10 {
            put_cell(
                &mut vdp,
                PLANE_A,
                32,
                column,
                row,
                entry(1, 0, false, false, false),
            );
        }
    }
    set_colour(&mut vdp, 1, RED);

    draw(&mut vdp);
    assert_eq!(pixel(&vdp, 0, 79), shown(RED), "row 9");
    assert_eq!(pixel(&vdp, 0, 80), shown(0), "row 10 is empty");

    scroll_down(&mut vdp, 8, 0);
    draw(&mut vdp);
    assert_eq!(pixel(&vdp, 0, 71), shown(RED), "row 9, one row higher");
    assert_eq!(pixel(&vdp, 0, 72), shown(0), "row 10");
}

#[test]
fn a_scroll_wraps_at_the_planes_width() {
    let mut vdp = vdp();
    put_tile(&mut vdp, 1, &solid_tile(1));
    put_tile(&mut vdp, 2, &solid_tile(2));
    put_cell(
        &mut vdp,
        PLANE_A,
        32,
        0,
        0,
        entry(1, 0, false, false, false),
    );
    for column in 1..32 {
        put_cell(
            &mut vdp,
            PLANE_A,
            32,
            column,
            0,
            entry(2, 0, false, false, false),
        );
    }
    set_colour(&mut vdp, 1, RED);
    set_colour(&mut vdp, 2, GREEN);

    // A plane of 32 cells is 256 pixels wide: a scroll of 256 is none.
    scroll_across(&mut vdp, 256, 0);
    draw(&mut vdp);
    assert_eq!(pixel(&vdp, 0, 0), shown(RED));
    assert_eq!(pixel(&vdp, 8, 0), shown(GREEN));
    assert_eq!(pixel(&vdp, 256, 0), shown(RED), "the plane again");

    scroll_across(&mut vdp, 256 + 8, 0);
    draw(&mut vdp);
    assert_eq!(pixel(&vdp, 0, 0), shown(GREEN), "column 31 comes in");
    assert_eq!(pixel(&vdp, 8, 0), shown(RED));
}

#[test]
fn each_cell_takes_the_palette_line_its_entry_names() {
    let mut vdp = vdp();
    put_tile(&mut vdp, 1, &solid_tile(1));
    for line in 0..4 {
        put_cell(
            &mut vdp,
            PLANE_A,
            32,
            line,
            0,
            entry(1, line, false, false, false),
        );
    }
    set_colour(&mut vdp, 1, RED);
    set_colour(&mut vdp, 17, GREEN);
    set_colour(&mut vdp, 33, BLUE);
    set_colour(&mut vdp, 49, WHITE);
    draw(&mut vdp);

    assert_eq!(pixel(&vdp, 0, 0), shown(RED));
    assert_eq!(pixel(&vdp, 8, 0), shown(GREEN));
    assert_eq!(pixel(&vdp, 16, 0), shown(BLUE));
    assert_eq!(pixel(&vdp, 24, 0), shown(WHITE));
}

#[test]
fn a_cell_is_flipped_across_down_or_both() {
    let mut vdp = vdp();
    // One pixel, at the tile's top left.
    put_tile(&mut vdp, 1, &tile_with(&[(0, 0, 15)]));
    for (column, (flip_h, flip_v)) in [(false, false), (true, false), (false, true), (true, true)]
        .into_iter()
        .enumerate()
    {
        let value = entry(1, 0, flip_h, flip_v, false);
        put_cell(&mut vdp, PLANE_A, 32, column as u16, 0, value);
    }
    set_colour(&mut vdp, 15, WHITE);
    draw(&mut vdp);

    let white = shown(WHITE);
    assert_eq!(pixel(&vdp, 0, 0), white);
    assert_ne!(pixel(&vdp, 7, 0), white);
    assert_eq!(pixel(&vdp, 15, 0), white, "across: the top right");
    assert_ne!(pixel(&vdp, 8, 0), white);
    assert_eq!(pixel(&vdp, 16, 7), white, "down: the bottom left");
    assert_ne!(pixel(&vdp, 16, 0), white);
    assert_eq!(pixel(&vdp, 31, 7), white, "both: the bottom right");
    assert_ne!(pixel(&vdp, 24, 0), white);
}

#[test]
fn a_plane_64_cells_wide_wraps_at_512_pixels() {
    let mut vdp = vdp();
    vdp.set_test_register(16, 0x01); // 64 by 32 cells
    put_tile(&mut vdp, 1, &solid_tile(1));
    // Rows of 64 cells: the last cell of row 0.
    put_cell(
        &mut vdp,
        PLANE_A,
        64,
        63,
        0,
        entry(1, 0, false, false, false),
    );
    set_colour(&mut vdp, 1, RED);

    scroll_across(&mut vdp, 8, 0);
    draw(&mut vdp);
    assert_eq!(
        pixel(&vdp, 0, 0),
        shown(RED),
        "column 63 comes in from the left"
    );
    assert_eq!(pixel(&vdp, 8, 0), shown(0));
    assert_eq!(pixel(&vdp, 0, 8), shown(0), "row 1 is the next 128 bytes");
}

#[test]
fn a_plane_128_cells_wide_wraps_at_1024_pixels() {
    let mut vdp = vdp();
    vdp.set_test_register(16, 0x03); // 128 by 32 cells
    put_tile(&mut vdp, 2, &solid_tile(2));
    put_cell(
        &mut vdp,
        PLANE_A,
        128,
        127,
        0,
        entry(2, 0, false, false, false),
    );
    set_colour(&mut vdp, 2, GREEN);

    scroll_across(&mut vdp, 8, 0);
    draw(&mut vdp);
    assert_eq!(pixel(&vdp, 0, 0), shown(GREEN), "column 127 comes in");
    assert_eq!(pixel(&vdp, 8, 0), shown(0));
}

#[test]
fn a_scroll_of_part_of_a_cell_moves_by_pixels() {
    let mut vdp = vdp();
    // One pixel, at (0, 5) of the tile.
    put_tile(&mut vdp, 1, &tile_with(&[(0, 5, 1)]));
    put_cell(
        &mut vdp,
        PLANE_A,
        32,
        0,
        0,
        entry(1, 0, false, false, false),
    );
    set_colour(&mut vdp, 1, RED);

    scroll_across(&mut vdp, 3, 0);
    scroll_down(&mut vdp, 5, 0);
    draw(&mut vdp);
    assert_eq!(pixel(&vdp, 3, 0), shown(RED), "three right, five up");
    assert_eq!(pixel(&vdp, 2, 0), shown(0));
    assert_eq!(pixel(&vdp, 4, 0), shown(0));
    assert_eq!(pixel(&vdp, 3, 1), shown(0));
}

#[test]
fn colour_0_of_a_tile_shows_what_is_behind_it() {
    let mut vdp = vdp();
    // Four pixels of colour 0, then four of colour 1, on every row.
    let mut pattern = [0u8; 32];
    for row in 0..8 {
        pattern[row * 4 + 2] = 0x11;
        pattern[row * 4 + 3] = 0x11;
    }
    put_tile(&mut vdp, 1, &pattern);
    put_cell(
        &mut vdp,
        PLANE_A,
        32,
        0,
        0,
        entry(1, 0, false, false, false),
    );
    set_colour(&mut vdp, 1, RED);
    // The backdrop is colour 2, blue.
    set_colour(&mut vdp, 2, BLUE);
    vdp.set_test_register(7, 0x02);
    draw(&mut vdp);

    assert_eq!(pixel(&vdp, 0, 0), shown(BLUE));
    assert_eq!(pixel(&vdp, 3, 0), shown(BLUE));
    assert_eq!(pixel(&vdp, 4, 0), shown(RED));
}
