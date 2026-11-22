//! Where each pixel of a frame came from, the sprites each line met, the
//! chosen sprites drawn alone, the window masks and the offset-per-tile
//! columns, all worked out from the frame's line table.
//!
//! Each scene draws a frame line by line through the PPU, as the emulator
//! does, with the line table kept, and reads it back. The redraw that notes
//! where the pixels came from must draw the picture the frame left, byte for
//! byte, and its planes must say what drew each pixel and from where.

use retro_core::capture::{
    layer, line_flags, planes, tile_value, CaptureRecording, LineSprites, SpriteFate, NO_CELL,
};
use retro_core::cpu::Addressable;
use retro_core::Framebuffer;
use snes1_core::ppu::line_reads::{offset_replaced, WINDOW_MASKS_PER_LINE, WINDOW_MASK_BYTES};
use snes1_core::ppu::tile::compute_offset_per_tile;
use snes1_core::ppu::trace::{pixel_layer, MODE7_OUTSIDE};
use snes1_core::ppu::TracedFrame;
use snes1_core::SnesBus;

// -------------------------------------------------------------
// Driving the PPU
// -------------------------------------------------------------

/// Write PPU register `$21xx`.
fn reg(bus: &mut SnesBus, reg: u8, value: u8) {
    bus.write(0x2100 | u32::from(reg), value);
}

/// Write a register that takes two bytes, low first.
fn reg_word(bus: &mut SnesBus, register: u8, value: u16) {
    reg(bus, register, value as u8);
    reg(bus, register, (value >> 8) as u8);
}

/// Write `words` into VRAM from word `at` on.
fn vram(bus: &mut SnesBus, at: u16, words: &[u16]) {
    reg(bus, 0x15, 0x80);
    reg(bus, 0x16, at as u8);
    reg(bus, 0x17, (at >> 8) as u8);
    for &word in words {
        reg(bus, 0x18, word as u8);
        reg(bus, 0x19, (word >> 8) as u8);
    }
}

/// Write CGRAM colour `index`.
fn colour(bus: &mut SnesBus, index: u8, bgr: u16) {
    reg(bus, 0x21, index);
    reg(bus, 0x22, bgr as u8);
    reg(bus, 0x22, (bgr >> 8) as u8 & 0x7F);
}

/// Write the whole sprite table.
fn sprite_table(bus: &mut SnesBus, table: &[u8; 0x220]) {
    reg(bus, 0x02, 0);
    reg(bus, 0x03, 0);
    for &byte in table.iter() {
        reg(bus, 0x04, byte);
    }
}

/// A sprite table with every sprite parked below the visible lines.
fn parked_sprites() -> [u8; 0x220] {
    let mut table = [0u8; 0x220];
    for slot in 0..128 {
        table[slot * 4 + 1] = 0xF0;
    }
    table
}

/// Put sprite `slot` at `(x, y)` (x nine bits), character `character`,
/// attribute byte `attribute`, large when `large`.
fn place_sprite(
    table: &mut [u8; 0x220],
    slot: usize,
    x: u16,
    y: u8,
    character: u8,
    attribute: u8,
    large: bool,
) {
    table[slot * 4] = x as u8;
    table[slot * 4 + 1] = y;
    table[slot * 4 + 2] = character;
    table[slot * 4 + 3] = attribute;
    let shift = (slot & 3) * 2;
    let bits = u8::from(x >= 256) | (u8::from(large) << 1);
    let high = &mut table[0x200 + (slot >> 2)];
    *high = (*high & !(3 << shift)) | (bits << shift);
}

/// A character of `planes` bit planes whose pixel at `(row, column)` is
/// `pixel(row, column)`, as the tile format lays it out: planes 0-1 in the
/// first eight words, 2-3 in the next eight, and so on.
fn character(planes: usize, pixel: impl Fn(usize, usize) -> u8) -> Vec<u16> {
    let mut words = vec![0u16; planes * 4];
    for row in 0..8 {
        for column in 0..8 {
            let value = pixel(row, column);
            for plane in 0..planes {
                if (value >> plane) & 1 != 0 {
                    words[row + 8 * (plane / 2)] |= 1 << ((7 - column) + 8 * (plane % 2));
                }
            }
        }
    }
    words
}

/// A PPU on a bus with the line table kept, in forced blank in the
/// vertical blank, so every memory port is open for the scene's setup.
fn scene() -> SnesBus {
    let mut bus = SnesBus::new();
    bus.ppu_mut().set_capture_recording(CaptureRecording {
        line_states: true,
        ..Default::default()
    });
    bus.set_raster_snapshot(0, 225);
    reg(&mut bus, 0x00, 0x80);
    bus
}

/// The vertical scroll that puts BG line `y` on output row `y`: the
/// renderer adds the 1-based line to the scroll.
fn rows_from_zero(bus: &mut SnesBus, bg: u8) {
    reg_word(bus, 0x0E + 2 * bg, 0xFFFF);
}

/// The picture a frame left, cropped as the host crops it.
struct Frame {
    width: u32,
    height: u32,
    picture: Vec<u8>,
}

/// Draw a frame as the emulator does, every visible line in turn, with
/// `before(bus, line)` making the changes that land before each line;
/// then finish the picture.
fn draw_frame(bus: &mut SnesBus, mut before: impl FnMut(&mut SnesBus, u16)) -> Frame {
    let mut fb = Framebuffer::new(512, 480);
    for line in 1..240u16 {
        bus.set_raster_snapshot(300, line - 1);
        before(bus, line);
        bus.ppu_mut().render_scanline(line, &mut fb);
    }
    bus.set_raster_snapshot(0, 241);
    bus.ppu().finalize_frame(&mut fb);
    bus.ppu_mut().note_picture_finished();
    let (width, height) = bus.ppu().active_display_size();
    let stride = fb.width as usize * 4;
    let row = width as usize * 4;
    let mut picture = Vec::new();
    for y in 0..height as usize {
        picture.extend_from_slice(&fb.data[y * stride..y * stride + row]);
    }
    Frame {
        width,
        height,
        picture,
    }
}

/// Every plane of the frame's pixels, after checking the redraw drew the
/// picture the frame left.
fn read_sources(bus: &mut SnesBus, frame: &Frame) -> TracedFrame {
    let sources = bus
        .ppu_mut()
        .pixel_sources(planes::MAIN | planes::SUB_SCREEN);
    assert!(sources.from_line_states, "drawn from the line table");
    assert_eq!((sources.width, sources.height), (frame.width, frame.height));
    assert!(
        sources.picture == frame.picture,
        "the traced redraw draws the picture the frame left"
    );
    sources
}

/// The value of `plane` at `(x, y)`.
fn at<T: Copy>(plane: &Option<Vec<T>>, sources: &TracedFrame, x: u32, y: u32) -> T {
    plane.as_ref().expect("the plane was asked for")[(y * sources.width + x) as usize]
}

/// The four main planes at `(x, y)`: layer, colour, tile, cell.
fn pixel(sources: &TracedFrame, x: u32, y: u32) -> (u16, u16, u32, u16) {
    (
        at(&sources.layer, sources, x, y),
        at(&sources.colour, sources, x, y),
        at(&sources.tile, sources, x, y),
        at(&sources.cell, sources, x, y),
    )
}

/// The `colour` plane's value: index, palette, bits-per-pixel code.
fn colour_value(index: u16, palette: u16, bpp_code: u16) -> u16 {
    index | (palette << 8) | (bpp_code << 11)
}

/// No colour math, in the `layer` plane.
const NO_MATH: u16 = pixel_layer::ADDEND_NONE << pixel_layer::ADDEND_SHIFT;

// -------------------------------------------------------------
// Backgrounds
// -------------------------------------------------------------

#[test]
fn a_background_pixel_names_its_character_its_map_entry_and_its_colour() {
    let mut bus = scene();
    reg(&mut bus, 0x05, 0x01); // Mode 1
    reg(&mut bus, 0x07, 0x04); // BG1 map at word $0400
    reg(&mut bus, 0x0B, 0x00); // BG1 characters at word 0
    rows_from_zero(&mut bus, 0);
    // Character 5: the pixel of column c is c + 1, on every row.
    vram(
        &mut bus,
        5 * 16,
        &character(4, |_, column| column as u8 + 1),
    );
    // Map row 2, column 3: character 5, palette 2, priority, flipped across.
    vram(
        &mut bus,
        0x0400 + 2 * 32 + 3,
        &[5 | (2 << 10) | 0x2000 | 0x4000],
    );
    // Map row 5, column 10: character 5, palette 1, flipped down.
    vram(&mut bus, 0x0400 + 5 * 32 + 10, &[5 | (1 << 10) | 0x8000]);
    colour(&mut bus, 0, 0x1234);
    for index in 1..64u8 {
        colour(
            &mut bus,
            index,
            u16::from(index).wrapping_mul(0x0421) & 0x7FFF,
        );
    }
    reg(&mut bus, 0x2C, 0x01);
    reg(&mut bus, 0x00, 0x0F);
    let frame = draw_frame(&mut bus, |_, _| {});
    let sources = read_sources(&mut bus, &frame);

    // (26, 20): two pixels into the flipped tile, read from its column 5,
    // pixel 6, palette 2: colour 38.
    assert_eq!(
        pixel(&sources, 26, 20),
        (
            1 | (1 << pixel_layer::PRIORITY_SHIFT) | NO_MATH,
            colour_value(38, 2, 2),
            tile_value(80, 5, 4, 5),
            0x0443,
        )
    );
    // (82, 41): a row flipped down, row 1 read from row 6; pixel 3 of
    // palette 1, colour 19.
    assert_eq!(
        pixel(&sources, 82, 41),
        (
            1 | NO_MATH,
            colour_value(19, 1, 2),
            tile_value(80, 5, 6, 2),
            0x0400 + 5 * 32 + 10,
        )
    );
    // The backdrop.
    assert_eq!(pixel(&sources, 0, 0), (NO_MATH, 0, 0, NO_CELL));
    // The sub screen took no part: no colour math, not hires.
    assert_eq!(at(&sources.sub_layer, &sources, 26, 20), 7);
}

#[test]
fn reading_the_sources_leaves_the_ppu_as_it_was() {
    let mut bus = scene();
    reg(&mut bus, 0x05, 0x01);
    reg(&mut bus, 0x07, 0x04);
    vram(
        &mut bus,
        16,
        &character(4, |row, column| (row + column) as u8 & 15),
    );
    vram(&mut bus, 0x0400, &[1; 1024]);
    reg(&mut bus, 0x2C, 0x11);
    reg(&mut bus, 0x00, 0x0F);
    let frame = draw_frame(&mut bus, |bus, line| reg_word(bus, 0x0D, line * 3));
    let memory = |bus: &SnesBus| {
        (
            bus.ppu().vram().to_vec(),
            bus.ppu().cgram().to_vec(),
            bus.ppu().oam().to_vec(),
        )
    };
    let before = memory(&bus);
    let state = bincode::serialize(&bus.ppu().to_savestate()).unwrap();
    let _ = read_sources(&mut bus, &frame);
    let _ = bus.ppu_mut().render_sprites(&[true; 128]);
    let _ = bus.ppu().line_sprites(1);
    let _ = bus.ppu().window_masks(1);
    let _ = bus.ppu().offset_per_tile_table(1);
    assert_eq!(memory(&bus), before);
    assert_eq!(
        bincode::serialize(&bus.ppu().to_savestate()).unwrap(),
        state
    );
    // And the picture drawn again is still the frame's.
    let image = bus.ppu_mut().picture_from_line_states().unwrap();
    assert_eq!(image.rgba, frame.picture);
}

// -------------------------------------------------------------
// Sprites
// -------------------------------------------------------------

/// Mode 1, BG1 opaque everywhere at low priority, two 8x8 sprites on lines
/// 31-38: slot 7 at x 40 with priority 3 and palette 1, slot 9 at x 100
/// with priority 0 and palette 0, both of character 2.
fn sprite_scene() -> (SnesBus, Frame) {
    let mut bus = scene();
    reg(&mut bus, 0x05, 0x01);
    reg(&mut bus, 0x07, 0x04);
    reg(&mut bus, 0x0B, 0x01); // BG1 characters at word $1000
    rows_from_zero(&mut bus, 0);
    vram(&mut bus, 0x1010, &character(4, |_, _| 1));
    vram(&mut bus, 0x0400, &[1; 1024]);
    reg(&mut bus, 0x01, 0x00); // 8x8 sprites, characters at word 0
    vram(&mut bus, 2 * 16, &character(4, |_, _| 1));
    let mut table = parked_sprites();
    place_sprite(&mut table, 7, 40, 30, 2, 0x32, false);
    place_sprite(&mut table, 9, 100, 30, 2, 0x00, false);
    sprite_table(&mut bus, &table);
    colour(&mut bus, 1, 0x0011);
    colour(&mut bus, 129, 0x0222);
    colour(&mut bus, 145, 0x3333);
    reg(&mut bus, 0x2C, 0x11);
    reg(&mut bus, 0x00, 0x0F);
    let frame = draw_frame(&mut bus, |_, _| {});
    (bus, frame)
}

#[test]
fn a_sprite_in_front_of_a_background_names_its_slot_and_one_behind_it_does_not_show() {
    let (mut bus, frame) = sprite_scene();
    let sources = read_sources(&mut bus, &frame);
    assert_eq!(
        pixel(&sources, 42, 32),
        (
            pixel_layer::SPRITES | (3 << pixel_layer::PRIORITY_SHIFT) | NO_MATH,
            colour_value(145, 1, 2),
            tile_value(2 << 4, 2, 2, 2),
            7,
        )
    );
    // Slot 9's priority 0 sits behind BG1 in Mode 1.
    assert_eq!(
        pixel(&sources, 102, 32),
        (
            1 | NO_MATH,
            colour_value(1, 0, 2),
            tile_value(0x1010, 1, 0, 6),
            0x0400 + 4 * 32 + 12,
        )
    );
}

#[test]
fn the_chosen_sprites_are_drawn_alone_after_priority() {
    let (mut bus, _) = sprite_scene();
    let alpha = |image: &retro_core::video::PixelImage, x: u32, y: u32| {
        image.rgba[((y * image.width + x) * 4 + 3) as usize]
    };
    let mut chosen = [false; 128];
    chosen[7] = true;
    let seven = bus.ppu_mut().render_sprites(&chosen);
    assert_eq!((seven.width, seven.height), (256, 224));
    assert_eq!(alpha(&seven, 42, 32), 0xFF);
    assert_eq!(alpha(&seven, 102, 32), 0);
    chosen[9] = true;
    let both = bus.ppu_mut().render_sprites(&chosen);
    // Slot 9 is behind BG1 in the picture, but drawn alone it shows.
    assert_eq!(alpha(&both, 102, 32), 0xFF);
    assert_eq!(
        both,
        bus.ppu_mut().render_layer(4).unwrap(),
        "both sprites chosen are the whole sprite layer"
    );
    let none = bus.ppu_mut().render_sprites(&[false; 128]);
    assert!(none.rgba.chunks(4).all(|pixel| pixel[3] == 0));

    // Two sprites on one place: only the one in front shows when the one
    // behind is chosen alone.
    let mut table = parked_sprites();
    place_sprite(&mut table, 3, 60, 50, 2, 0x30, false);
    place_sprite(&mut table, 4, 60, 50, 2, 0x30, false);
    bus.set_raster_snapshot(0, 225);
    sprite_table(&mut bus, &table);
    draw_frame(&mut bus, |_, _| {});
    let mut behind = [false; 128];
    behind[4] = true;
    let image = bus.ppu_mut().render_sprites(&behind);
    assert_eq!(alpha(&image, 61, 52), 0, "slot 3 wins the column");
    let mut front = [false; 128];
    front[3] = true;
    assert_eq!(alpha(&bus.ppu_mut().render_sprites(&front), 61, 52), 0xFF);
}

/// One entry of a line-sprite list: slot, fate, x, width, drawn width.
type Entry = (u8, u8, i16, u8, u8);

/// The flags and the entries of line `line` in a line-sprite list.
fn entries_of(sprites: &LineSprites, line: u16) -> (u16, Vec<Entry>) {
    let row = sprites
        .lines
        .chunks(4)
        .find(|row| row[0] == line)
        .unwrap_or_else(|| panic!("line {line} is listed"));
    let (first, count) = (usize::from(row[1]), usize::from(row[2]));
    let entries = (first..first + count)
        .map(|i| {
            (
                sprites.slots[i],
                sprites.fates[i],
                sprites.x[i],
                sprites.width[i],
                sprites.drawn_width[i],
            )
        })
        .collect();
    (row[3], entries)
}

#[test]
fn each_line_lists_the_sprites_it_met_and_what_became_of_them() {
    let mut bus = scene();
    // Selector 1: 8x8 small, 32x32 large.
    reg(&mut bus, 0x01, 0x20);
    let mut table = parked_sprites();
    // Forty small sprites on line 51 (row 50).
    for slot in 0..40 {
        place_sprite(&mut table, slot, (slot * 6) as u16, 50, 0, 0, false);
    }
    // Ten large sprites on lines 11-42 want 40 columns.
    for slot in 40..50 {
        place_sprite(&mut table, slot, ((slot - 40) * 20) as u16, 10, 0, 0, true);
    }
    // On line 101: one at x = 256, one wrapping in from the left (x = -7).
    place_sprite(&mut table, 60, 256, 100, 0, 0, false);
    place_sprite(&mut table, 61, 505, 100, 0, 0, false);
    sprite_table(&mut bus, &table);
    reg(&mut bus, 0x2C, 0x10);
    reg(&mut bus, 0x00, 0x0F);
    draw_frame(&mut bus, |bus, line| {
        if line == 150 {
            reg(bus, 0x2C, 0x00); // sprites on neither screen from line 150
        }
    });
    let sprites = bus.ppu().line_sprites(1);
    assert!(sprites.from_line_states);
    assert_eq!(sprites.lines.len(), 224 * 4);

    let (flags, entries) = entries_of(&sprites, 51);
    assert_eq!(flags, line_flags::OVER_LIMIT);
    assert_eq!(entries.len(), 40);
    assert!(entries[..32]
        .iter()
        .all(|e| e.1 == SpriteFate::DrawnWhole as u8 && e.4 == 8));
    assert!(entries[32..]
        .iter()
        .all(|e| e.1 == SpriteFate::OverLineLimit as u8 && e.4 == 0));
    assert_eq!(
        entries.iter().map(|e| e.0).collect::<Vec<_>>(),
        (0..40u8).collect::<Vec<_>>()
    );

    let (flags, entries) = entries_of(&sprites, 11);
    assert_eq!(flags, line_flags::OUT_OF_TIME);
    let fates: Vec<u8> = entries.iter().map(|e| e.1).collect();
    assert_eq!(
        fates[0],
        SpriteFate::LeftOut as u8,
        "slot 40 lost all its columns"
    );
    assert_eq!(fates[1], SpriteFate::DrawnInPart as u8);
    assert!(fates[2..]
        .iter()
        .all(|&fate| fate == SpriteFate::DrawnWhole as u8));
    assert_eq!(entries[1].4, 16, "slot 41 kept two of its four columns");
    assert_eq!(entries[1].3, 32);

    let (_, entries) = entries_of(&sprites, 101);
    assert_eq!(entries[0], (60, SpriteFate::OffScreen as u8, -256, 8, 0));
    assert_eq!(entries[1], (61, SpriteFate::DrawnWhole as u8, -7, 8, 1));

    let (flags, entries) = entries_of(&sprites, 150);
    assert_eq!(flags, line_flags::NOT_SHOWN);
    assert!(entries.is_empty());
}

// -------------------------------------------------------------
// Colour math and the sub screen
// -------------------------------------------------------------

/// Mode 1: BG1 opaque on the left half (colour 1) on the main screen, BG2
/// opaque on the eight left tile columns of the top 64 lines (colour 2) on
/// the sub screen. Colour math changes down the frame: from line 1 halved
/// with the sub screen, from line 61 subtracting the fixed colour, from
/// line 121 the main screen black everywhere, and from line 181 none.
fn colour_math_scene(main: u8, sub: u8, math: bool) -> (SnesBus, Frame) {
    let mut bus = scene();
    reg(&mut bus, 0x05, 0x01);
    reg(&mut bus, 0x07, 0x04); // BG1 map at word $0400
    reg(&mut bus, 0x08, 0x08); // BG2 map at word $0800
    reg(&mut bus, 0x0B, 0x11); // BG1 and BG2 characters at word $1000
    rows_from_zero(&mut bus, 0);
    rows_from_zero(&mut bus, 1);
    vram(&mut bus, 0x1010, &character(4, |_, _| 1));
    vram(&mut bus, 0x1020, &character(4, |_, _| 2));
    let map1: Vec<u16> = (0..1024).map(|cell| u16::from(cell % 32 < 16)).collect();
    vram(&mut bus, 0x0400, &map1);
    let map2: Vec<u16> = (0..1024)
        .map(|cell| if cell % 32 < 8 && cell / 32 < 8 { 2 } else { 0 })
        .collect();
    vram(&mut bus, 0x0800, &map2);
    colour(&mut bus, 0, 0x0C63);
    colour(&mut bus, 1, 0x7FFF);
    colour(&mut bus, 2, 0x001F);
    reg(&mut bus, 0x32, 0x20 | 4); // fixed colour: red 4
    reg(&mut bus, 0x32, 0x40 | 6); // green 6
    reg(&mut bus, 0x32, 0x80 | 8); // blue 8
    reg(&mut bus, 0x2C, main);
    reg(&mut bus, 0x2D, sub);
    reg(&mut bus, 0x00, 0x0F);
    let frame = draw_frame(&mut bus, |bus, line| {
        if !math {
            return;
        }
        let (select, add_sub) = match line {
            1 => (0x02, 0x41),
            61 => (0x00, 0x81),
            121 => (0xC0, 0x00),
            181 => (0x00, 0x00),
            _ => return,
        };
        reg(bus, 0x30, select);
        reg(bus, 0x31, add_sub);
    });
    (bus, frame)
}

#[test]
fn colour_math_says_what_it_added_halved_subtracted_and_blacked() {
    let (mut bus, frame) = colour_math_scene(0x01, 0x02, true);
    let sources = read_sources(&mut bus, &frame);
    let layer = |x, y| at(&sources.layer, &sources, x, y);
    // Halved with BG2 from the sub screen.
    assert_eq!(
        layer(10, 10),
        1 | (2 << pixel_layer::ADDEND_SHIFT) | pixel_layer::MATH_APPLIED | pixel_layer::HALVED
    );
    // Added to the sub screen's backdrop, which halving skips.
    assert_eq!(layer(100, 10), 1 | pixel_layer::MATH_APPLIED);
    // The backdrop takes no part in colour math here.
    assert_eq!(layer(200, 10), NO_MATH);
    // Subtracting the fixed colour.
    assert_eq!(
        layer(10, 80),
        1 | (pixel_layer::ADDEND_FIXED_COLOUR << pixel_layer::ADDEND_SHIFT)
            | pixel_layer::MATH_APPLIED
            | pixel_layer::SUBTRACTED
    );
    // The main screen blacked, no colour math.
    assert_eq!(layer(10, 130), 1 | NO_MATH | pixel_layer::BLACKED);
    let rgba = |x: u32, y: u32| {
        let at = ((y * sources.width + x) * 4) as usize;
        sources.picture[at..at + 4].to_vec()
    };
    assert_eq!(rgba(10, 130), vec![0, 0, 0, 0xFF]);
    // No colour math: the sub screen takes no part.
    assert_eq!(layer(10, 190), 1 | NO_MATH);

    let sub_layer = |x, y| at(&sources.sub_layer, &sources, x, y);
    let sub_colour = |x, y| at(&sources.sub_colour, &sources, x, y);
    assert_eq!((sub_layer(10, 10), sub_colour(10, 10)), (2, 0x001F));
    // The sub screen's backdrop is the fixed colour in the normal modes.
    let fixed = 4 | (6 << 5) | (8 << 10);
    assert_eq!((sub_layer(10, 80), sub_colour(10, 80)), (0, fixed));
    assert_eq!((sub_layer(10, 190), sub_colour(10, 190)), (7, 0));
}

#[test]
fn the_sub_screen_planes_are_what_the_sub_screen_drew() {
    let (mut bus, frame) = colour_math_scene(0x01, 0x02, true);
    let with_math = read_sources(&mut bus, &frame);
    // The same scene with BG2 on the main screen and no colour math.
    let (mut bus, frame) = colour_math_scene(0x02, 0x00, false);
    let alone = read_sources(&mut bus, &frame);
    let expand = |c: u16| {
        let c = (c & 0x1F) as u8;
        (c << 3) | (c >> 2)
    };
    let mut compared = 0;
    for y in 0..120 {
        for x in 0..256 {
            if at(&with_math.sub_layer, &with_math, x, y) != 2 {
                continue;
            }
            compared += 1;
            assert_eq!(at(&alone.layer, &alone, x, y) & layer::WHAT_DREW, 2);
            let bgr = at(&with_math.sub_colour, &with_math, x, y);
            let at = ((y * alone.width + x) * 4) as usize;
            assert_eq!(
                &alone.picture[at..at + 3],
                &[expand(bgr), expand(bgr >> 5), expand(bgr >> 10)],
                "({x}, {y})"
            );
        }
    }
    assert_eq!(compared, 64 * 64, "BG2's 64 columns on its 64 lines");
}

// -------------------------------------------------------------
// Hires, interlace, Mode 7
// -------------------------------------------------------------

#[test]
fn a_hires_line_shows_the_sub_screen_on_its_even_columns_and_interlace_weaves_two_rows() {
    let mut bus = scene();
    reg(&mut bus, 0x05, 0x05); // Mode 5
    reg(&mut bus, 0x07, 0x04); // BG1 map at word $0400
    reg(&mut bus, 0x08, 0x08); // BG2 map at word $0800
    reg(&mut bus, 0x0B, 0x21); // BG1 characters at $1000, BG2 at $2000
    vram(&mut bus, 0x1020, &character(4, |_, _| 1)); // characters 2 and 3
    vram(&mut bus, 0x1030, &character(4, |_, _| 2));
    vram(&mut bus, 0x2020, &character(2, |_, _| 1)); // BG2 character 4
    vram(&mut bus, 0x0400, &[2; 1024]);
    vram(&mut bus, 0x0800, &[4; 1024]);
    reg(&mut bus, 0x2C, 0x01);
    reg(&mut bus, 0x2D, 0x02);
    reg(&mut bus, 0x33, 0x01); // interlace
    reg(&mut bus, 0x00, 0x0F);
    let frame = draw_frame(&mut bus, |_, _| {});
    assert_eq!((frame.width, frame.height), (512, 448));
    let sources = read_sources(&mut bus, &frame);
    let layer = |x, y| at(&sources.layer, &sources, x, y);
    // Even columns: the sub screen's BG2; odd: the main screen's BG1.
    assert_eq!(layer(0, 0) & 0x0F, 2 | pixel_layer::SUB_SCREEN_SHOWN);
    assert_eq!(layer(1, 0) & 0x0F, 1);
    // The two rows of line 1 read BG lines 2 and 3, its two fields:
    // (1 << 1) | 0 and (1 << 1) | 1.
    let row_read = |x, y| (at(&sources.tile, &sources, x, y) >> 26) & 7;
    assert_eq!((row_read(1, 0), row_read(1, 1)), (2, 3));
    // Each 16-pixel slot is two characters, side by side.
    let character_read = |x, y| (at(&sources.tile, &sources, x, y) >> 15) & 0x7FF;
    assert_eq!((character_read(1, 0), character_read(9, 0)), (2, 3));
    assert_eq!(character_read(0, 0), 4);
}

#[test]
fn a_pseudo_hires_line_shows_the_sub_screen_on_the_even_column_of_each_dot() {
    // Mode 1 with pseudo-hires: BG1 on the main screen, a character whose
    // column `c` is colour `c + 1`; BG2 on the sub screen, colour 9, but
    // for its map's second entry, which is empty. Each dot is two columns:
    // the even one shows the sub screen, the odd one the main screen, and
    // the sub screen's empty dots show colour 0, not the fixed colour.
    let mut bus = scene();
    reg(&mut bus, 0x05, 0x01);
    reg(&mut bus, 0x07, 0x04); // BG1 map at word $0400
    reg(&mut bus, 0x08, 0x08); // BG2 map at word $0800
    reg(&mut bus, 0x0B, 0x21); // BG1 characters at $1000, BG2 at $2000
    rows_from_zero(&mut bus, 0);
    rows_from_zero(&mut bus, 1);
    vram(
        &mut bus,
        0x1010,
        &character(4, |_, column| column as u8 + 1),
    );
    vram(&mut bus, 0x2010, &character(4, |_, _| 9));
    vram(&mut bus, 0x0400, &[1; 1024]);
    let mut map = [1u16; 1024];
    map[1] = 0;
    vram(&mut bus, 0x0800, &map);
    for index in 0..10u8 {
        colour(&mut bus, index, (u16::from(index) * 3) | 0x2800);
    }
    reg(&mut bus, 0x32, 0xFF); // the fixed colour: white
    reg(&mut bus, 0x2C, 0x01);
    reg(&mut bus, 0x2D, 0x02);
    reg(&mut bus, 0x33, 0x08); // pseudo-hires
    reg(&mut bus, 0x00, 0x0F);
    let frame = draw_frame(&mut bus, |_, _| {});
    assert_eq!((frame.width, frame.height), (512, 448));

    let expand = |c: u16| ((c << 3) | (c >> 2)) as u8;
    let colour_of = |index: u16| [expand(index * 3), 0, expand(10)];
    let rgb = |x: u32, y: u32| {
        let at = ((y * frame.width + x) * 4) as usize;
        [
            frame.picture[at],
            frame.picture[at + 1],
            frame.picture[at + 2],
        ]
    };
    for dot in 0..16u32 {
        let main = colour_of(dot as u16 % 8 + 1);
        let sub = if dot < 8 { colour_of(9) } else { colour_of(0) };
        for y in 0..2 {
            assert_eq!(
                rgb(2 * dot + 1, y),
                main,
                "dot {dot}, row {y}: the main screen"
            );
            assert_eq!(rgb(2 * dot, y), sub, "dot {dot}, row {y}: the sub screen");
        }
    }

    // The planes note each column at its dot.
    let sources = read_sources(&mut bus, &frame);
    let drew = |x, y| at(&sources.layer, &sources, x, y) & 0x0F;
    let column_read = |x, y| (at(&sources.tile, &sources, x, y) >> 29) & 7;
    for dot in 0..16u32 {
        assert_eq!(drew(2 * dot + 1, 0), 1, "dot {dot}");
        assert_eq!(column_read(2 * dot + 1, 0), dot % 8, "dot {dot}");
        let sub = if dot < 8 { 2 } else { 0 };
        assert_eq!(
            drew(2 * dot, 0),
            sub | pixel_layer::SUB_SCREEN_SHOWN,
            "dot {dot}"
        );
    }

    // BG1 alone covers both columns of each dot.
    let alone = bus.ppu_mut().render_layer(0).unwrap();
    assert_eq!((alone.width, alone.height), (512, 448));
    for dot in 0..16usize {
        for x in [2 * dot, 2 * dot + 1] {
            let at = x * 4;
            assert_eq!(
                &alone.rgba[at..at + 3],
                &colour_of(dot as u16 % 8 + 1),
                "column {x}"
            );
            assert_eq!(alone.rgba[at + 3], 0xFF);
        }
    }
}

#[test]
fn a_pseudo_hires_line_of_an_interlaced_frame_gives_both_of_its_rows() {
    // Mode 1, pseudo-hires and interlace: a character whose row `r` is
    // colour `r + 1` on the main screen. The frame is woven, 512 by 448;
    // a pseudo-hires line's backgrounds keep their lines, so both rows of
    // each line show that line's background row.
    let mut bus = scene();
    reg(&mut bus, 0x05, 0x01);
    reg(&mut bus, 0x07, 0x04);
    reg(&mut bus, 0x0B, 0x01);
    rows_from_zero(&mut bus, 0);
    vram(&mut bus, 0x1010, &character(4, |row, _| row as u8 + 1));
    vram(&mut bus, 0x0400, &[1; 1024]);
    for index in 0..9u8 {
        colour(&mut bus, index, u16::from(index) * 3);
    }
    reg(&mut bus, 0x2C, 0x01);
    reg(&mut bus, 0x33, 0x09); // pseudo-hires and interlace
    reg(&mut bus, 0x00, 0x0F);
    let frame = draw_frame(&mut bus, |_, _| {});
    assert_eq!((frame.width, frame.height), (512, 448));
    let red = |x: u32, y: u32| frame.picture[((y * frame.width + x) * 4) as usize];
    let expand = |c: u32| ((c << 3) | (c >> 2)) as u8;
    for row in 0..32u32 {
        let line_row = row / 2;
        assert_eq!(red(1, row), expand((line_row % 8 + 1) * 3), "row {row}");
    }
    read_sources(&mut bus, &frame);
}

#[test]
fn a_woven_frame_draws_each_row_s_sprite_row_for_the_field_it_shows() {
    // Mode 5 with interlace and OBJ interlace: an 8x8 sprite whose row `r`
    // is colour `r + 1` covers four lines, each field showing every other
    // row. Each row of the woven frame is drawn for its own field, the
    // sprites as the backgrounds are, so the sprite's eight rows fall on
    // eight rows in order, from the first row of its first line, the row
    // of that line's even field.
    let mut bus = scene();
    reg(&mut bus, 0x05, 0x05);
    reg(&mut bus, 0x01, 0x00); // 8x8 sprites, characters at word 0
    vram(&mut bus, 0x0010, &character(4, |row, _| row as u8 + 1));
    for index in 0..9u8 {
        colour(&mut bus, 128 + index, u16::from(index) * 3);
    }
    let mut table = parked_sprites();
    place_sprite(&mut table, 0, 40, 10, 1, 0x30, false);
    sprite_table(&mut bus, &table);
    reg(&mut bus, 0x2C, 0x10); // the sprites on the main screen
    reg(&mut bus, 0x33, 0x03); // interlace and OBJ interlace
    reg(&mut bus, 0x00, 0x0F);
    let frame = draw_frame(&mut bus, |_, _| {});
    assert_eq!((frame.width, frame.height), (512, 448));
    // The main screen is on the odd columns: column 81 is dot 40.
    let red = |y: u32| frame.picture[((y * frame.width + 81) * 4) as usize];
    let expand = |c: u32| ((c << 3) | (c >> 2)) as u8;
    assert_eq!(red(19), 0, "the row above the sprite");
    for row in 0..8u32 {
        assert_eq!(red(20 + row), expand((row + 1) * 3), "sprite row {row}");
    }
    assert_eq!(red(28), 0, "the row below the sprite");
    read_sources(&mut bus, &frame);
}

/// Mode 1 with pseudo-hires, which shows the sub screen on its even
/// columns: BG2 on the sub screen alone, a character whose pixel at (row,
/// column) is colour `row + column + 1`, under a mosaic of four set before
/// line `mosaic_from` is drawn. Line `n` shows background row `n - 1`.
fn sub_screen_mosaic_scene(mosaic_from: u16) -> (SnesBus, Frame) {
    let mut bus = scene();
    reg(&mut bus, 0x05, 0x01);
    reg(&mut bus, 0x08, 0x08); // BG2 map at word $0800
    reg(&mut bus, 0x0B, 0x20); // BG2 characters at $2000
    rows_from_zero(&mut bus, 1);
    vram(
        &mut bus,
        0x2010,
        &character(4, |row, column| (row + column + 1) as u8),
    );
    vram(&mut bus, 0x0800, &[1; 1024]);
    for index in 0..16u8 {
        colour(&mut bus, index, u16::from(index) * 2);
    }
    reg(&mut bus, 0x2D, 0x02); // BG2 on the sub screen
    reg(&mut bus, 0x33, 0x08); // pseudo-hires
    reg(&mut bus, 0x00, 0x0F);
    let frame = draw_frame(&mut bus, |bus, line| {
        if line == mosaic_from {
            reg(bus, 0x06, 0x32); // a mosaic of four on BG2
        }
    });
    (bus, frame)
}

/// The red of the sub screen's pixel at `(dot, line)` of
/// [`sub_screen_mosaic_scene`]'s frame: line `line`'s first row is
/// `2 * (line - 1)`, and dot `dot`'s sub screen pixel column `2 * dot`.
fn sub_screen_red(frame: &Frame, dot: u32, line: u32) -> u8 {
    frame.picture[((2 * (line - 1) * frame.width + 2 * dot) * 4) as usize]
}

#[test]
fn the_sub_screen_s_backgrounds_are_mosaicked_as_the_main_screen_s_are() {
    // Each four-by-four block of the sub screen shows its first pixel, as
    // the main screen's would, and the planes name that pixel.
    let (mut bus, frame) = sub_screen_mosaic_scene(1);
    let expand = |c: u32| ((c << 3) | (c >> 2)) as u8;
    for line in 1..=8u32 {
        for dot in 0..8u32 {
            let first = ((line - 1) / 4 * 4) + (dot / 4 * 4) + 1;
            assert_eq!(
                sub_screen_red(&frame, dot, line),
                expand(first * 2),
                "dot {dot}, line {line}"
            );
        }
    }
    let sources = read_sources(&mut bus, &frame);
    let layer = |dot: u32, line: u32| at(&sources.layer, &sources, 2 * dot, 2 * (line - 1));
    let tile = |dot: u32, line: u32| at(&sources.tile, &sources, 2 * dot, 2 * (line - 1));
    let repeats = |dot, line| layer(dot, line) & pixel_layer::MOSAIC_REPEAT != 0;
    assert_eq!(layer(0, 1) & 0x0F, 2 | pixel_layer::SUB_SCREEN_SHOWN);
    assert!(!repeats(0, 1) && !repeats(4, 1) && !repeats(0, 5));
    assert!(repeats(1, 1) && repeats(3, 1) && repeats(0, 2) && repeats(5, 7));
    // The row and the column read are the block's first pixel's.
    let read = |dot, line| ((tile(dot, line) >> 26) & 7, tile(dot, line) >> 29);
    assert_eq!(read(3, 3), (0, 0));
    assert_eq!(read(6, 6), (4, 4));

    // Set before line 3, inside the block of lines 1-4, the mosaic finds no
    // sub screen rows kept by line 2, drawn without it: line 3 starts a
    // block of its own, line 4 repeats it, and line 5 starts the next.
    let (mut bus, frame) = sub_screen_mosaic_scene(3);
    for (line, background_row) in [(1, 0), (2, 1), (3, 2), (4, 2), (5, 4)] {
        for dot in 0..8u32 {
            let first = if line < 3 {
                background_row + dot + 1
            } else {
                background_row + (dot / 4 * 4) + 1
            };
            assert_eq!(
                sub_screen_red(&frame, dot, line),
                expand(first * 2),
                "mosaic from line 3: dot {dot}, line {line}"
            );
        }
    }
    read_sources(&mut bus, &frame);
}

#[test]
fn a_mode7_pixel_names_its_character_and_its_plane_cell_and_says_when_it_lies_outside() {
    let mut bus = scene();
    reg(&mut bus, 0x05, 0x07);
    reg(&mut bus, 0x1A, 0xC0); // character 0 outside the plane
    reg_word(&mut bus, 0x1B, 0x0100); // the identity matrix
    reg_word(&mut bus, 0x1C, 0x0000);
    reg_word(&mut bus, 0x1D, 0x0000);
    reg_word(&mut bus, 0x1E, 0x0100);
    reg_word(&mut bus, 0x1F, 0x0000);
    reg_word(&mut bus, 0x20, 0x0000);
    reg_word(&mut bus, 0x0D, 0x1FF0); // scrolled 16 to the left
    reg_word(&mut bus, 0x0E, 0x0000);
    // Character 0's pixels are 0x11; the map's row 0 names character 0.
    vram(&mut bus, 0, &[0x1100; 64]);
    // Map cell (2, 1) names character 7.
    vram(&mut bus, 130, &[0x0007]);
    // Character 7's pixel at (row, column) is 0x40 + row * 8 + column.
    let pixels: Vec<u16> = (0..64u16).map(|i| (0x40 + i) << 8).collect();
    vram(&mut bus, 7 * 64, &pixels);
    for index in 1..=255u8 {
        colour(&mut bus, index, u16::from(index) * 0x21);
    }
    reg(&mut bus, 0x2C, 0x01);
    reg(&mut bus, 0x00, 0x0F);
    let frame = draw_frame(&mut bus, |_, _| {});
    let sources = read_sources(&mut bus, &frame);
    // (34, 10): world (18, 11), cell (2, 1), character 7, row 3, column 2.
    assert_eq!(
        pixel(&sources, 34, 10),
        (
            1 | NO_MATH,
            colour_value(0x5A, 0, 3),
            tile_value(7 * 64, 7, 3, 2),
            130,
        )
    );
    // (3, 10): world (-13, 11), outside the plane: character 0 forced.
    assert_eq!(
        pixel(&sources, 3, 10),
        (
            1 | NO_MATH,
            colour_value(0x11, 0, 3),
            tile_value(0, 0, 3, 3),
            MODE7_OUTSIDE,
        )
    );
}

// -------------------------------------------------------------
// Mosaic, offset-per-tile, direct colour
// -------------------------------------------------------------

#[test]
fn mosaic_offset_per_tile_and_direct_colour_are_flagged() {
    // Mosaic of four on BG1.
    let mut bus = scene();
    reg(&mut bus, 0x05, 0x01);
    reg(&mut bus, 0x07, 0x04);
    reg(&mut bus, 0x0B, 0x01);
    rows_from_zero(&mut bus, 0);
    vram(
        &mut bus,
        0x1010,
        &character(4, |row, column| ((row + column) % 15 + 1) as u8),
    );
    vram(&mut bus, 0x0400, &[1; 1024]);
    reg(&mut bus, 0x06, 0x31);
    reg(&mut bus, 0x2C, 0x01);
    reg(&mut bus, 0x00, 0x0F);
    let frame = draw_frame(&mut bus, |_, _| {});
    let sources = read_sources(&mut bus, &frame);
    let repeats = |x, y| at(&sources.layer, &sources, x, y) & pixel_layer::MOSAIC_REPEAT != 0;
    assert!(!repeats(0, 0) && !repeats(4, 0) && !repeats(4, 4));
    assert!(repeats(1, 0) && repeats(3, 0) && repeats(0, 1) && repeats(5, 6));

    // Offset-per-tile in Mode 2: BG3's entry at column 4 moves BG1's
    // column 5 (dots 40-47) eight pixels.
    let mut bus = scene();
    reg(&mut bus, 0x05, 0x02);
    reg(&mut bus, 0x07, 0x04);
    reg(&mut bus, 0x09, 0x0C); // BG3 map at word $0C00
    reg(&mut bus, 0x0B, 0x01);
    vram(
        &mut bus,
        0x1010,
        &character(4, |_, column| column as u8 + 1),
    );
    vram(&mut bus, 0x0400, &[1; 1024]);
    vram(&mut bus, 0x0C04, &[0x2000 | 8]);
    reg(&mut bus, 0x2C, 0x01);
    reg(&mut bus, 0x00, 0x0F);
    let frame = draw_frame(&mut bus, |_, _| {});
    let sources = read_sources(&mut bus, &frame);
    let replaced = |x, y| at(&sources.layer, &sources, x, y) & pixel_layer::OFFSET_REPLACED != 0;
    assert!((40..48).all(|x| replaced(x, 5)));
    assert!(!replaced(39, 5) && !replaced(48, 5) && !replaced(0, 5));

    // Direct colour in Mode 3: the pixel's byte is its colour.
    let mut bus = scene();
    reg(&mut bus, 0x05, 0x03);
    reg(&mut bus, 0x07, 0x04);
    reg(&mut bus, 0x0B, 0x01);
    rows_from_zero(&mut bus, 0);
    vram(&mut bus, 0x1000 + 32, &character(8, |_, _| 0x5B));
    vram(&mut bus, 0x0400, &[1 | (5 << 10); 1024]);
    reg(&mut bus, 0x30, 0x01);
    reg(&mut bus, 0x2C, 0x01);
    reg(&mut bus, 0x00, 0x0F);
    let frame = draw_frame(&mut bus, |_, _| {});
    let sources = read_sources(&mut bus, &frame);
    assert_eq!(
        pixel(&sources, 5, 6),
        (
            1 | pixel_layer::DIRECT_COLOUR | NO_MATH,
            colour_value(0x5B, 5, 3),
            tile_value(0x1000 + 32, 1, 6, 5),
            0x0400,
        )
    );
}

// -------------------------------------------------------------
// Normalisation
// -------------------------------------------------------------

#[test]
fn the_planes_are_normalised_as_the_picture_is() {
    // Lines 1-100 in Mode 1, the rest in Mode 5: a hires frame, its lo-res
    // rows widened and every row doubled down.
    let mut bus = scene();
    reg(&mut bus, 0x05, 0x01);
    reg(&mut bus, 0x07, 0x04);
    reg(&mut bus, 0x0B, 0x01);
    vram(
        &mut bus,
        0x1010,
        &character(4, |_, column| column as u8 + 1),
    );
    vram(
        &mut bus,
        0x1020,
        &character(4, |_, column| 8 - column as u8),
    );
    vram(&mut bus, 0x0400, &[1; 1024]);
    reg(&mut bus, 0x2C, 0x01);
    reg(&mut bus, 0x00, 0x0F);
    let frame = draw_frame(&mut bus, |bus, line| {
        if line == 101 {
            reg(bus, 0x05, 0x05);
        }
    });
    assert_eq!((frame.width, frame.height), (512, 448));
    let sources = read_sources(&mut bus, &frame);
    let tile = |x, y| at(&sources.tile, &sources, x, y);
    for y in [0, 1, 50, 199] {
        for x in 0..256 {
            assert_eq!(tile(2 * x, y), tile(2 * x + 1, y), "({x}, {y}) widened");
        }
    }
    // Rows 2k and 2k + 1 are one line, doubled down.
    assert_eq!(
        sources.tile.as_ref().unwrap()[300 * 512..301 * 512],
        sources.tile.as_ref().unwrap()[301 * 512..302 * 512]
    );

    // An overscan frame is cropped to the TV window: its first row is line
    // 8, which reads BG line 7, map row 0; its second, map row 1.
    let mut bus = scene();
    reg(&mut bus, 0x05, 0x01);
    reg(&mut bus, 0x07, 0x04);
    reg(&mut bus, 0x0B, 0x01);
    rows_from_zero(&mut bus, 0);
    vram(&mut bus, 0x1010, &character(4, |_, _| 1));
    vram(&mut bus, 0x0400, &[1; 1024]);
    reg(&mut bus, 0x33, 0x04);
    reg(&mut bus, 0x2C, 0x01);
    reg(&mut bus, 0x00, 0x0F);
    let frame = draw_frame(&mut bus, |_, _| {});
    assert_eq!((frame.width, frame.height), (256, 224));
    let sources = read_sources(&mut bus, &frame);
    assert_eq!(at(&sources.cell, &sources, 0, 0), 0x0400);
    assert_eq!(at(&sources.cell, &sources, 0, 1), 0x0420);
}

// -------------------------------------------------------------
// Window masks and the offset-per-tile table
// -------------------------------------------------------------

#[test]
fn the_window_masks_follow_each_lines_windows() {
    let mut bus = scene();
    reg(&mut bus, 0x23, 0x02); // window 1 on BG1
    reg(&mut bus, 0x25, 0x20); // and on the colour window
    reg(&mut bus, 0x30, 0x60); // main screen kept inside it, math outside
    reg(&mut bus, 0x00, 0x0F);
    let edges = |line: u16| (line as u8, (line + 50) as u8);
    draw_frame(&mut bus, |bus, line| {
        let (left, right) = edges(line);
        reg(bus, 0x26, left);
        reg(bus, 0x27, right);
    });
    let masks = bus.ppu().window_masks(1);
    assert!(masks.from_line_states);
    assert_eq!(masks.lines.len(), 224);
    let per_line = WINDOW_MASKS_PER_LINE * WINDOW_MASK_BYTES;
    assert_eq!(masks.masks.len(), 224 * per_line);
    for (row, &line) in masks.lines.iter().enumerate() {
        let line_masks = &masks.masks[row * per_line..(row + 1) * per_line];
        let bit = |mask: usize, x: usize| {
            line_masks[mask * WINDOW_MASK_BYTES + (x >> 3)] & (1 << (x & 7)) != 0
        };
        let (left, right) = edges(line);
        for x in 0..256 {
            let inside = x >= usize::from(left) && x <= usize::from(right);
            assert_eq!(bit(0, x), inside, "line {line} BG1 dot {x}");
            assert!(!bit(1, x) && !bit(2, x) && !bit(3, x) && !bit(4, x));
            assert_eq!(bit(5, x), inside, "the colour window");
            assert_eq!(bit(6, x), inside, "the main screen kept inside");
            assert_eq!(bit(7, x), !inside, "colour math outside");
        }
    }
}

#[test]
fn the_offset_per_tile_table_is_what_the_fetch_reads() {
    for (mode, entries) in [
        // Mode 2: horizontal entries on BG3's row 0, vertical on row 1.
        (0x02u8, vec![(4u16, 0x2000 | 8), (32 + 6, 0x6000 | 24)]),
        // Mode 4: one entry per column, bit 15 the axis.
        (0x04, vec![(4, 0x2000 | 8), (6, 0xA000 | 24)]),
    ] {
        let mut bus = scene();
        reg(&mut bus, 0x05, mode);
        reg(&mut bus, 0x09, 0x0C); // BG3 map at word $0C00
        for (cell, entry) in entries {
            vram(&mut bus, 0x0C00 + cell, &[entry]);
        }
        reg(&mut bus, 0x2C, 0x01);
        reg(&mut bus, 0x00, 0x0F);
        draw_frame(&mut bus, |bus, line| {
            reg_word(bus, 0x0D, line * 5);
            reg_word(bus, 0x0F, line * 3);
        });
        let table = bus.ppu().offset_per_tile_table(1);
        assert!(table.from_line_states);
        assert_eq!(table.columns, 33);
        assert_eq!(table.lines, (1..=224).collect::<Vec<u16>>());
        let columns = table.columns as usize;
        let mut replaced_somewhere = [false; 2];
        for (row, &line) in table.lines.iter().enumerate() {
            for bg in 0..2usize {
                let hoffset = line * if bg == 0 { 5 } else { 3 };
                let fine = hoffset & 7;
                for k in 0..columns {
                    let x = if k == 0 { 0 } else { (k as u16) * 8 - fine };
                    let wanted = compute_offset_per_tile(
                        bus.ppu().vram(),
                        0x1800,
                        0,
                        0,
                        0,
                        mode,
                        bg as u8,
                        x,
                        hoffset,
                        0,
                        line,
                    );
                    let at = (row * 2 + bg) * columns + k;
                    assert_eq!(
                        (table.scroll[at * 2], table.scroll[at * 2 + 1]),
                        wanted,
                        "mode {mode} line {line} BG{} column {k}",
                        bg + 1
                    );
                    let bits = table.replaced[at];
                    if bits & (offset_replaced::HORIZONTAL | offset_replaced::VERTICAL) != 0 {
                        replaced_somewhere[usize::from(bits & offset_replaced::VERTICAL != 0)] =
                            true;
                    }
                    assert_eq!(bits & offset_replaced::PAST_THE_LINE != 0, x >= 256);
                }
            }
        }
        assert_eq!(replaced_somewhere, [true, true], "mode {mode}");
    }
}
