//! Planes and the window drawn together by the renderer the console runs:
//! which one shows where they overlap.

use super::draw::*;

#[test]
fn plane_b_is_drawn_on_its_own() {
    let mut vdp = vdp();
    put_tile(&mut vdp, 1, &solid_tile(2));
    for column in 0..16 {
        put_cell(
            &mut vdp,
            PLANE_B,
            32,
            column,
            0,
            entry(1, 0, false, false, false),
        );
    }
    set_colour(&mut vdp, 2, GREEN);
    draw(&mut vdp);
    assert_eq!(pixel(&vdp, 0, 0), shown(GREEN));
    assert_eq!(pixel(&vdp, 127, 7), shown(GREEN));
    assert_eq!(pixel(&vdp, 128, 0), shown(0));
}

#[test]
fn plane_a_is_drawn_over_plane_b() {
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
    put_cell(
        &mut vdp,
        PLANE_B,
        32,
        0,
        0,
        entry(2, 0, false, false, false),
    );
    set_colour(&mut vdp, 1, RED);
    set_colour(&mut vdp, 2, GREEN);
    draw(&mut vdp);
    assert_eq!(pixel(&vdp, 0, 0), shown(RED));
}

/// Plane A all tile 1 (red) and the window's table all tile 2 (green).
fn plane_a_and_window() -> sg1_core::vdp::Vdp {
    let mut vdp = vdp();
    put_tile(&mut vdp, 1, &solid_tile(1));
    put_tile(&mut vdp, 2, &solid_tile(2));
    for row in 0..32 {
        for column in 0..32 {
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
    // The window's rows are 64 cells long in the 40-cell mode.
    for row in 0..32 {
        for column in 0..64 {
            put_cell(
                &mut vdp,
                WINDOW,
                64,
                column,
                row,
                entry(2, 0, false, false, false),
            );
        }
    }
    set_colour(&mut vdp, 1, RED);
    set_colour(&mut vdp, 2, GREEN);
    vdp
}

#[test]
fn the_window_takes_plane_as_place_in_its_columns() {
    let mut vdp = plane_a_and_window();
    // From column 160 (ten units of 16 pixels) rightwards.
    vdp.set_test_register(17, 0x8A);
    draw(&mut vdp);
    assert_eq!(pixel(&vdp, 0, 0), shown(RED));
    assert_eq!(pixel(&vdp, 159, 100), shown(RED));
    assert_eq!(pixel(&vdp, 160, 0), shown(GREEN));
    assert_eq!(pixel(&vdp, 319, 223), shown(GREEN));
}

#[test]
fn a_high_priority_plane_b_shows_over_a_low_plane_a() {
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
    put_cell(&mut vdp, PLANE_B, 32, 0, 0, entry(2, 0, false, false, true));
    set_colour(&mut vdp, 1, RED);
    set_colour(&mut vdp, 2, GREEN);
    draw(&mut vdp);
    assert_eq!(pixel(&vdp, 0, 0), shown(GREEN));
}

#[test]
fn the_window_takes_its_lines_whole_and_its_columns_on_the_rest() {
    let mut vdp = plane_a_and_window();
    // Columns from 160 rightwards, and lines from 120 down.
    vdp.set_test_register(17, 0x8A);
    vdp.set_test_register(18, 0x8F);
    draw(&mut vdp);
    assert_eq!(pixel(&vdp, 0, 0), shown(RED), "neither");
    assert_eq!(pixel(&vdp, 160, 0), shown(GREEN), "a column of the window");
    assert_eq!(pixel(&vdp, 0, 120), shown(GREEN), "a line of the window");
    assert_eq!(pixel(&vdp, 160, 120), shown(GREEN), "both");
}

#[test]
fn colour_0_of_plane_a_shows_plane_b_behind_it() {
    let mut vdp = vdp();
    put_tile(&mut vdp, 1, &solid_tile(2));
    // Four pixels of colour 0, then four of colour 1, on every row.
    let mut pattern = [0u8; 32];
    for row in 0..8 {
        pattern[row * 4 + 2] = 0x11;
        pattern[row * 4 + 3] = 0x11;
    }
    put_tile(&mut vdp, 2, &pattern);
    put_cell(
        &mut vdp,
        PLANE_B,
        32,
        0,
        0,
        entry(1, 0, false, false, false),
    );
    put_cell(
        &mut vdp,
        PLANE_A,
        32,
        0,
        0,
        entry(2, 0, false, false, false),
    );
    set_colour(&mut vdp, 1, RED);
    set_colour(&mut vdp, 2, GREEN);
    draw(&mut vdp);
    assert_eq!(pixel(&vdp, 0, 0), shown(GREEN), "plane B through plane A");
    assert_eq!(pixel(&vdp, 4, 0), shown(RED));
}

#[test]
fn the_window_does_not_scroll_with_plane_a() {
    let mut vdp = plane_a_and_window();
    vdp.set_test_register(17, 0x8A);
    scroll_across(&mut vdp, 16, 0);
    scroll_down(&mut vdp, 16, 0);
    draw(&mut vdp);
    assert_eq!(pixel(&vdp, 160, 0), shown(GREEN));
    assert_eq!(pixel(&vdp, 0, 0), shown(RED));
}
