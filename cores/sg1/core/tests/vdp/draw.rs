//! A small kit that draws pictures with the renderer the console runs:
//! set the registers, fill video memory, draw the frame again and read its
//! pixels.
//!
//! The layout every picture starts from: the display on in mode 5, 40 cells
//! wide, planes of 32 by 32 cells, plane A at `$C000`, plane B at `$E000`,
//! the window's table at `$B000` but no window on screen, the sprite table
//! at `$F000`, the horizontal scroll table at `$FC00`, both scrolls whole
//! screen, and the backdrop colour 0, black.

use sg1_core::vdp::Vdp;

/// Where plane A's table is.
pub const PLANE_A: u16 = 0xC000;
/// Where plane B's table is.
pub const PLANE_B: u16 = 0xE000;
/// Where the window's table is.
pub const WINDOW: u16 = 0xB000;
/// Where the sprite table is.
pub const SPRITES: u16 = 0xF000;
/// Where the horizontal scroll table is.
pub const HORIZONTAL_SCROLL: u16 = 0xFC00;

/// A VDP set up as the module's notes say.
pub fn vdp() -> Vdp {
    let mut vdp = Vdp::new();
    for (register, value) in [
        (1, 0x44),  // display on, mode 5
        (2, 0x30),  // plane A at $C000
        (3, 0x2C),  // window at $B000
        (4, 0x07),  // plane B at $E000
        (5, 0x78),  // sprite table at $F000
        (7, 0x00),  // backdrop: colour 0
        (11, 0x00), // both scrolls whole screen
        (12, 0x81), // 40 cells
        (13, 0x3F), // horizontal scroll table at $FC00
        (16, 0x00), // planes of 32 by 32 cells
        (17, 0x00), // no window columns
        (18, 0x00), // no window lines
    ] {
        vdp.set_test_register(register, value);
    }
    vdp
}

/// A pattern whose 64 pixels are all `colour`.
pub fn solid_tile(colour: u8) -> [u8; 32] {
    [(colour << 4) | colour; 32]
}

/// A pattern of colour 0 with the pixels `(x, y, colour)` set.
pub fn tile_with(pixels: &[(usize, usize, u8)]) -> [u8; 32] {
    let mut pattern = [0u8; 32];
    for &(x, y, colour) in pixels {
        let byte = &mut pattern[y * 4 + x / 2];
        if x % 2 == 0 {
            *byte = (*byte & 0x0F) | (colour << 4);
        } else {
            *byte = (*byte & 0xF0) | colour;
        }
    }
    pattern
}

/// Put `pattern` as tile number `tile`.
pub fn put_tile(vdp: &mut Vdp, tile: u16, pattern: &[u8; 32]) {
    vdp.write_vram_debug(u32::from(tile) * 32, pattern);
}

/// A nametable entry: the tile, its palette line, its flips and its
/// priority.
pub fn entry(tile: u16, palette: u16, flip_h: bool, flip_v: bool, high: bool) -> u16 {
    tile | (u16::from(flip_h) << 11)
        | (u16::from(flip_v) << 12)
        | (palette << 13)
        | (u16::from(high) << 15)
}

/// Put `value` in the cell at (`column`, `row`) of the table at `table`,
/// whose rows are `columns` cells long.
pub fn put_cell(vdp: &mut Vdp, table: u16, columns: u16, column: u16, row: u16, value: u16) {
    let address = u32::from(table) + u32::from((row * columns + column) * 2);
    vdp.write_vram_debug(address, &value.to_be_bytes());
}

/// Set colour `index` of CRAM to `word` (`0000 BBB0 GGG0 RRR0`).
pub fn set_colour(vdp: &mut Vdp, index: u32, word: u16) {
    vdp.write_cram_debug(index, &[word]);
}

/// Scroll the whole screen of plane A and of plane B by these numbers of
/// pixels to the right.
pub fn scroll_across(vdp: &mut Vdp, plane_a: u16, plane_b: u16) {
    let mut entry = [0u8; 4];
    entry[..2].copy_from_slice(&plane_a.to_be_bytes());
    entry[2..].copy_from_slice(&plane_b.to_be_bytes());
    vdp.write_vram_debug(u32::from(HORIZONTAL_SCROLL), &entry);
}

/// Scroll the whole screen of plane A and of plane B by these numbers of
/// lines up.
pub fn scroll_down(vdp: &mut Vdp, plane_a: u16, plane_b: u16) {
    vdp.write_vsram_debug(0, &[plane_a, plane_b]);
}

/// Put sprite `slot` of the sprite table at screen (`x`, `y`), `width` by
/// `height` cells, linking to `link`, with `attributes`: the pattern (bits
/// 0-10), the flips (11 across, 12 down), the palette line (13-14) and the
/// priority (15).
pub fn put_sprite(
    vdp: &mut Vdp,
    slot: u16,
    (x, y): (i16, i16),
    (width, height): (u16, u16),
    link: u16,
    attributes: u16,
) {
    let size_and_link = ((width - 1) << 10) | ((height - 1) << 8) | link;
    let words = [
        (y + 128) as u16,
        size_and_link,
        attributes,
        (x + 128) as u16,
    ];
    let mut bytes = [0u8; 8];
    for (i, word) in words.into_iter().enumerate() {
        bytes[i * 2..i * 2 + 2].copy_from_slice(&word.to_be_bytes());
    }
    vdp.write_vram_debug(u32::from(SPRITES) + u32::from(slot) * 8, &bytes);
}

/// Draw the frame again from video memory as it is now.
pub fn draw(vdp: &mut Vdp) {
    vdp.rerender_current_frame();
}

/// The colour of pixel (`x`, `y`) of the frame drawn.
pub fn pixel(vdp: &Vdp, x: usize, y: usize) -> (u8, u8, u8) {
    let at = (y * 320 + x) * 4;
    let framebuffer = vdp.framebuffer();
    (framebuffer[at], framebuffer[at + 1], framebuffer[at + 2])
}

/// The colour a CRAM word shows.
pub fn shown(word: u16) -> (u8, u8, u8) {
    Vdp::convert_genesis_color(word)
}

/// Red, green, blue and white, as CRAM words.
pub const RED: u16 = 0x000E;
pub const GREEN: u16 = 0x00E0;
pub const BLUE: u16 = 0x0E00;
pub const WHITE: u16 = 0x0EEE;
