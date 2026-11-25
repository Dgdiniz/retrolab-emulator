//! A layer's whole map as one picture ([`VideoDebugger::layer_map`]): plane
//! A's, plane B's or the window's map, every cell drawn as it stands in
//! VRAM, with no scroll and no window area.

use retro_core::video::{LayerKind, PixelImage, VideoDebugger};

use super::planes::NametableEntry;
use super::tiles::{build_rgba_palette, Tile};
use super::Vdp;

/// A cell's width and height in pixels, as [`VideoDebugger::layer_layout`]
/// gives them.
const CELL: u32 = 8;

/// Bytes of a character in VRAM: 8 rows of 4 bytes.
const CHARACTER_BYTES: usize = 32;

impl Vdp {
    /// The whole map of layer `layer` (0 plane A, 1 plane B, 2 the window),
    /// `None` for the sprites and for any other layer. The map is the cells
    /// [`VideoDebugger::layer_layout`] names: `map_width` by `map_height`
    /// of them from `map_base`, a row of `map_width` cells after another, a
    /// plane as large as register 16 makes it and the window 64 cells wide
    /// in the 40-cell mode (32 in the 32-cell mode) by 32 rows. Each cell is
    /// its entry's character, 8 by 8 pixels, flipped as the entry says,
    /// in the entry's palette line, in the colours the screen shows
    /// ([`build_rgba_palette`]); colour 0 is transparent. Interlace mode 2,
    /// whose picture draws a cell 16 rows tall from two characters, is
    /// drawn the same way, 8 by 8 cells of the character each entry names.
    pub(super) fn map_image(&self, layer: u32) -> Option<PixelImage> {
        let layout = self.layer_layout(layer)?;
        if !matches!(layout.kind, LayerKind::TileMap | LayerKind::Window) {
            return None;
        }
        let (columns, rows) = (layout.map_width, layout.map_height);
        let (width, height) = (columns * CELL, rows * CELL);
        let palettes: [[(u8, u8, u8, u8); 16]; 4] =
            std::array::from_fn(|line| build_rgba_palette(&self.cram, line as u8));
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for cell_y in 0..rows {
            for cell_x in 0..columns {
                let at = layout.map_base as usize + ((cell_y * columns + cell_x) * 2) as usize;
                let Some(word) = self.vram.get(at..at + 2) else {
                    continue;
                };
                let entry = NametableEntry::from_word(u16::from_be_bytes([word[0], word[1]]));
                let start = usize::from(entry.pattern_index) * CHARACTER_BYTES;
                let Some(pattern) = self.vram.get(start..start + CHARACTER_BYTES) else {
                    continue;
                };
                let tile = Tile::decode(pattern);
                let palette = &palettes[usize::from(entry.palette)];
                for y in 0..CELL {
                    for x in 0..CELL {
                        let index = tile.get_pixel(x as u8, y as u8, entry.flip_h, entry.flip_v);
                        if index == 0 {
                            continue;
                        }
                        let (r, g, b, a) = palette[usize::from(index)];
                        let pixel = ((cell_y * CELL + y) * width + cell_x * CELL + x) as usize * 4;
                        rgba[pixel..pixel + 4].copy_from_slice(&[r, g, b, a]);
                    }
                }
            }
        }
        Some(PixelImage {
            width,
            height,
            rgba,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{LAYER_PLANE_A, LAYER_PLANE_B, LAYER_SPRITES, LAYER_WINDOW};
    use super::*;

    /// VRAM and CRAM filled with values that look random and are the same
    /// on every run, so every map entry, character, flip and palette line
    /// is some value.
    fn scramble(vdp: &mut Vdp, seed: u32) {
        let mut state = seed;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for byte in vdp.vram.iter_mut() {
            *byte = next() as u8;
        }
        for colour in vdp.cram.iter_mut() {
            *colour = next() as u16 & 0x0EEE;
        }
        vdp.rebuild_cram_rgb();
    }

    /// A VDP with the display on, plane A at `$C000`, the window at `$B000`,
    /// plane B at `$E000`, the horizontal scroll table at `$AC00` read whole
    /// screen, register 12 and register 16 as given, and no window drawn.
    fn vdp_with(register_12: u8, register_16: u8, seed: u32) -> Vdp {
        let mut vdp = Vdp::new();
        scramble(&mut vdp, seed);
        vdp.registers[1] = 0x44; // display on, mode 5
        vdp.registers[2] = 0x30; // plane A at $C000
        vdp.registers[3] = 0x2C; // the window at $B000
        vdp.registers[4] = 0x07; // plane B at $E000
        vdp.registers[11] = 0x00; // whole-screen scrolls
        vdp.registers[12] = register_12;
        vdp.registers[13] = 0x2B; // horizontal scroll at $AC00
        vdp.registers[16] = register_16;
        vdp.registers[17] = 0x00;
        vdp.registers[18] = 0x00;
        vdp
    }

    /// Set plane `plane`'s scroll (0 plane A, 1 plane B): the horizontal in
    /// the scroll table, the vertical in its word of VSRAM.
    fn scroll(vdp: &mut Vdp, plane: usize, x: u16, y: u16) {
        let table = 0xAC00 + plane * 2;
        vdp.vram[table..table + 2].copy_from_slice(&x.to_be_bytes());
        vdp.vsram[plane] = y;
    }

    /// The pixel at `(x, y)` of `image`.
    fn pixel(image: &PixelImage, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * image.width + x) * 4) as usize;
        image.rgba[at..at + 4].try_into().expect("four bytes")
    }

    /// Every pixel the renderer draws of `layer` alone is the map's pixel
    /// the scroll puts there, and a pixel it leaves transparent is
    /// transparent there.
    fn assert_the_map_is_what_the_renderer_draws(
        vdp: &mut Vdp,
        layer: usize,
        (x0, y0): (u16, u16),
    ) {
        let map = vdp.map_image(layer as u32).expect("the layer's map");
        let drawn = vdp.render_layer_image(layer).expect("the layer").image;
        for y in 0..drawn.height {
            for x in 0..drawn.width {
                let map_x = (i64::from(x) - i64::from(x0)).rem_euclid(i64::from(map.width)) as u32;
                let map_y = (y + u32::from(y0)) % map.height;
                assert_eq!(
                    pixel(&drawn, x, y),
                    pixel(&map, map_x, map_y),
                    "layer {layer}, register 16 {:#04X}, scroll ({x0}, {y0}): picture ({x}, {y}), \
                     map ({map_x}, {map_y})",
                    vdp.registers[16]
                );
            }
        }
    }

    #[test]
    fn the_map_of_each_plane_is_what_the_renderer_draws_at_any_scroll() {
        // Every plane size the VDP takes, in both cell modes.
        for (seed, register_16) in [0x00u8, 0x01, 0x03, 0x10, 0x11, 0x30]
            .into_iter()
            .enumerate()
        {
            for register_12 in [0x81, 0x00] {
                let mut vdp = vdp_with(register_12, register_16, 7 + seed as u32);
                for (plane, layer) in [(0, LAYER_PLANE_A), (1, LAYER_PLANE_B)] {
                    for (x, y) in [(0, 0), (300, 290), (0x3FF, 1)] {
                        scroll(&mut vdp, plane, x, y);
                        assert_the_map_is_what_the_renderer_draws(&mut vdp, layer, (x, y));
                    }
                }
            }
        }
    }

    #[test]
    fn the_window_s_map_is_what_the_renderer_draws_where_it_covers_the_screen() {
        for register_12 in [0x81, 0x00] {
            let mut vdp = vdp_with(register_12, 0x01, 31);
            vdp.registers[17] = 0x80; // the window from column 0: the whole screen
            assert_the_map_is_what_the_renderer_draws(&mut vdp, LAYER_WINDOW, (0, 0));
        }
    }

    #[test]
    fn a_map_is_as_large_as_its_layout_names_it() {
        let size = |vdp: &Vdp, layer: usize| {
            let map = vdp.map_image(layer as u32).expect("a map");
            assert_eq!(map.rgba.len(), (map.width * map.height * 4) as usize);
            (map.width, map.height)
        };
        let vdp = vdp_with(0x81, 0x01, 1);
        assert_eq!(size(&vdp, LAYER_PLANE_A), (512, 256), "64 by 32 cells");
        assert_eq!(size(&vdp, LAYER_PLANE_B), (512, 256));
        assert_eq!(size(&vdp, LAYER_WINDOW), (512, 256), "64 cells in H40");
        let vdp = vdp_with(0x00, 0x30, 1);
        assert_eq!(size(&vdp, LAYER_PLANE_A), (256, 1024), "32 by 128 cells");
        assert_eq!(size(&vdp, LAYER_WINDOW), (256, 256), "32 cells in H32");
        for layer in [LAYER_PLANE_A, LAYER_WINDOW] {
            let layout = vdp.layer_layout(layer as u32).expect("a layout");
            assert_eq!(
                size(&vdp, layer),
                (layout.map_width * 8, layout.map_height * 8)
            );
        }
    }

    #[test]
    fn a_cell_is_its_character_in_its_palette_line_with_its_flips() {
        let mut vdp = Vdp::new();
        vdp.registers[2] = 0x30; // plane A at $C000, 32 by 32 cells

        // Character 1: colour 1 in its top-left pixel, colour 2 in its
        // top-right one, colour 0 elsewhere.
        vdp.vram[32] = 0x10;
        vdp.vram[35] = 0x02;
        vdp.cram[16 + 1] = 0x000E; // line 1, colour 1: red
        vdp.cram[16 + 2] = 0x0E00; // line 1, colour 2: blue

        // Cell (1, 0): character 1, palette line 1, flipped left to right.
        vdp.vram[0xC002..0xC004].copy_from_slice(&0x2801u16.to_be_bytes());
        let map = vdp.map_image(LAYER_PLANE_A as u32).expect("plane A");
        let red = build_rgba_palette(&vdp.cram, 1)[1];
        let blue = build_rgba_palette(&vdp.cram, 1)[2];
        assert_eq!(pixel(&map, 8, 0), [blue.0, blue.1, blue.2, 255], "flipped");
        assert_eq!(pixel(&map, 15, 0), [red.0, red.1, red.2, 255]);
        assert_eq!(pixel(&map, 9, 0), [0; 4], "colour 0 is transparent");
        assert_eq!(pixel(&map, 0, 0), [0; 4], "character 0 is empty");
    }

    #[test]
    fn the_sprites_and_a_layer_past_the_list_have_no_map() {
        let vdp = Vdp::new();
        assert_eq!(vdp.map_image(LAYER_SPRITES as u32), None);
        assert_eq!(vdp.map_image(4), None);
        assert_eq!(vdp.layer_map(LAYER_SPRITES as u32), None);
        assert_eq!(
            vdp.layer_map(LAYER_PLANE_B as u32),
            vdp.map_image(LAYER_PLANE_B as u32),
            "the trait's map is this one"
        );
    }
}
