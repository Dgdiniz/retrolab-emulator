//! A background's whole map as one picture
//! ([`retro_core::video::VideoDebugger::layer_map`]):
//! the tilemap each background reads, every entry drawn as the renderer
//! draws it under the mode the PPU is in, or in Mode 7 the plane of 128 by
//! 128 characters BG1 (and BG2 under EXTBG) reads.

use retro_core::video::PixelImage;

use crate::ppu::debug::bgr15_to_rgba8;
use crate::ppu::render::{cgram_index, direct_color_word, is_hires_mode, palette_offset_for_bg};
use crate::ppu::tile::{
    bg_entry_character, decode_tile_2bpp_row, decode_tile_4bpp_row, decode_tile_8bpp_row,
    fetch_bg_tilemap_entry, mode7_sample, tilemap_masks,
};
use crate::ppu::{Ppu, LAYER_OBJ};

impl Ppu {
    /// The whole map of layer `layer`, background `layer` (0 BG1 to 3 BG4,
    /// [`Self::background_map`]); `None` for the sprites and for any layer
    /// past them.
    pub(super) fn map_image(&self, layer: u32) -> Option<PixelImage> {
        let bg = usize::try_from(layer).ok().filter(|&bg| bg < LAYER_OBJ)?;
        Some(self.background_map(bg))
    }

    /// Background `bg`'s (`0..=3`) whole tilemap as an RGBA image, each
    /// entry drawn as the renderer draws it under the current mode: its
    /// character at the mode's depth (2, 4 or 8 bits a pixel), coloured
    /// through its palette from the mode's first colour (Mode 0 gives each
    /// background 32 colours of its own), or in direct colour where the
    /// renderer takes it, with the entry's flips; 16 by 16 pixels where the
    /// background takes 16x16 tiles, and 16 dots wide in the hires Modes 5
    /// and 6, whose picture is 512 dots wide. Mode 7's plane (BG1, and BG2
    /// under EXTBG) is its 128 by 128 map of 8 by 8 characters, 1024 by
    /// 1024 pixels ([`Self::mode7_plane`]). Colour 0 of a character
    /// is transparent; a background the mode does not draw gives the map's
    /// size, transparent throughout.
    fn background_map(&self, bg: usize) -> PixelImage {
        match (self.bgmode & 0x07, bg) {
            (7, 0) => return self.mode7_plane(false),
            (7, 1) if self.mode7_extbg => return self.mode7_plane(true),
            _ => {}
        }
        let hires = is_hires_mode(self.bgmode);
        let tile16 = self.bg_tile_size[bg];
        let screen_size = self.bg_screen_size[bg] & 0x03;
        let (mask_x, mask_y) = tilemap_masks(screen_size, hires, tile16);
        let width = u32::from(mask_x) + 1;
        let height = u32::from(mask_y) + 1;
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        let bpp = self.bg_bpp(bg);
        let decode_row = match bpp {
            2 => decode_tile_2bpp_row,
            4 => decode_tile_4bpp_row,
            8 => decode_tile_8bpp_row,
            _ => {
                return PixelImage {
                    width,
                    height,
                    rgba,
                }
            }
        };
        let screen_addr = self.bg_screen_addr[bg];
        let tile_data = self.bg_tiledata_addr[bg];
        let palette_offset = palette_offset_for_bg(self.bgmode, bg);
        let direct = self.bg_direct_colour(bg);
        let vram = self.vram();
        let cgram = self.cgram();
        for y in 0..=mask_y {
            // Each 8-pixel run is one character's row: an entry covers a
            // whole number of them.
            for x in (0..=mask_x).step_by(8) {
                let entry =
                    fetch_bg_tilemap_entry(vram, screen_addr, screen_size, x, y, hires, tile16);
                let character = bg_entry_character(entry, x, y, hires, tile16);
                let pixels = decode_row(
                    vram,
                    character,
                    tile_data,
                    (y & 7) as u8,
                    entry.flip_h,
                    entry.flip_v,
                );
                let start = (u32::from(y) * width + u32::from(x)) as usize * 4;
                for (column, &pixel) in pixels.iter().enumerate() {
                    if pixel == 0 {
                        continue;
                    }
                    let colour = if direct {
                        direct_color_word(pixel, entry.palette)
                    } else {
                        cgram[usize::from(cgram_index(palette_offset, entry.palette, pixel, bpp))]
                    };
                    let at = start + column * 4;
                    rgba[at..at + 4].copy_from_slice(&bgr15_to_rgba8(colour));
                }
            }
        }
        PixelImage {
            width,
            height,
            rgba,
        }
    }

    /// Mode 7's plane, 1024 by 1024 pixels: its 128 by 128 map of 8 by 8
    /// characters, read as the renderer samples it; each pixel BG1's 8-bit
    /// colour through CGRAM, or in direct colour while CGWSEL's direct
    /// colour is on, or (`extbg`) BG2's low 7 bits through CGRAM. Colour 0
    /// is transparent.
    fn mode7_plane(&self, extbg: bool) -> PixelImage {
        const SIDE: u32 = 1024;
        let vram = self.vram();
        let cgram = self.cgram();
        let mut rgba = vec![0u8; (SIDE * SIDE * 4) as usize];
        for y in 0..SIDE {
            for x in 0..SIDE {
                // Inside the plane every screen-over setting reads its map.
                let Some(byte) = mode7_sample(vram, x as i32, y as i32, 0) else {
                    continue;
                };
                let colour = if extbg {
                    match byte & 0x7F {
                        0 => continue,
                        index => cgram[usize::from(index)],
                    }
                } else if byte == 0 {
                    continue;
                } else if self.direct_color {
                    direct_color_word(byte, 0)
                } else {
                    cgram[usize::from(byte)]
                };
                let at = ((y * SIDE + x) * 4) as usize;
                rgba[at..at + 4].copy_from_slice(&bgr15_to_rgba8(colour));
            }
        }
        PixelImage {
            width: SIDE,
            height: SIDE,
            rgba,
        }
    }
}

#[cfg(test)]
mod tests {
    use retro_core::video::VideoDebugger;

    use super::*;

    /// Background `bg`'s map, through the trait.
    fn map_of(ppu: &Ppu, bg: usize) -> PixelImage {
        ppu.layer_map(bg as u32).expect("a background has a map")
    }

    #[test]
    fn a_map_is_32_by_32_entries_by_default() {
        let ppu = Ppu::new();
        // Default screen size selector (0) -> 32x32 tiles -> 256x256 px.
        let img = map_of(&ppu, 0);
        assert_eq!((img.width, img.height), (256, 256));
        assert_eq!(img.rgba.len(), 256 * 256 * 4);
    }

    // -- The map and the renderer -------------------------------------

    /// VRAM and CGRAM filled with values that look random and are the same on
    /// every run, so every map entry, character and colour is some value.
    fn scramble(ppu: &mut Ppu, seed: u32) {
        let mut state = seed;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for word in ppu.vram.iter_mut() {
            *word = next() as u16;
        }
        for colour in ppu.cgram.iter_mut() {
            *colour = next() as u16 & 0x7FFF;
        }
    }

    /// Every pixel the renderer draws of background `bg` on 224 lines, with
    /// the scroll at each of `scrolls`, is the map's pixel at the same place
    /// of the map, and a pixel it leaves transparent is transparent there.
    fn assert_the_map_is_what_the_renderer_draws(ppu: &mut Ppu, bg: usize, scrolls: &[(u16, u16)]) {
        use crate::ppu::render::{BgRowPixel, MAX_SCANLINE_WIDTH};
        use crate::ppu::trace::FrameTrace;
        use retro_core::capture::NoTrace;
        let map = map_of(ppu, bg);
        let hires = is_hires_mode(ppu.bgmode);
        let width: u16 = if hires { 512 } else { 256 };
        for &(hoffset, voffset) in scrolls {
            ppu.bg_hoffset[bg] = hoffset;
            ppu.bg_voffset[bg] = voffset;
            ppu.invalidate_tilemap_caches();
            let h_scroll = if hires { hoffset << 1 } else { hoffset };
            for scanline in 1..=224u16 {
                let mut row = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
                ppu.render_bg_row(
                    bg,
                    scanline,
                    &mut row,
                    hires,
                    &mut NoTrace::<FrameTrace>::new(),
                );
                let map_y = u32::from(scanline.wrapping_add(voffset)) % map.height;
                for x in 0..width {
                    let map_x = u32::from(x.wrapping_add(h_scroll)) % map.width;
                    let at = ((map_y * map.width + map_x) * 4) as usize;
                    let pixel = row[usize::from(x)];
                    let expected = if pixel.opaque {
                        bgr15_to_rgba8(pixel.color)
                    } else {
                        [0; 4]
                    };
                    assert_eq!(
                        map.rgba[at..at + 4],
                        expected,
                        "mode {} BG{} scroll ({hoffset}, {voffset}) line {scanline} x {x}",
                        ppu.bgmode,
                        bg + 1
                    );
                }
            }
        }
    }

    #[test]
    fn the_map_of_each_background_is_what_the_renderer_draws() {
        // Mode 0: four backgrounds of 2 bits, each with 32 colours of its own;
        // every screen size.
        for bg in 0..4 {
            let mut ppu = Ppu::new();
            scramble(&mut ppu, 11 + bg as u32);
            ppu.bgmode = 0;
            ppu.bg_screen_addr[bg] = 0x2000 * bg as u32;
            ppu.bg_tiledata_addr[bg] = 0x4000;
            ppu.bg_screen_size[bg] = bg as u8;
            assert_the_map_is_what_the_renderer_draws(&mut ppu, bg, &[(0, 0), (300, 290)]);
        }
        // Mode 1: 4 bits on BG1 and BG2, 2 on BG3; 16x16 tiles on BG2.
        let mut ppu = Ppu::new();
        scramble(&mut ppu, 21);
        ppu.bgmode = 1;
        ppu.bg_screen_size[0] = 3;
        ppu.bg_tiledata_addr[1] = 0x8000;
        ppu.bg_tile_size[1] = true;
        ppu.bg_screen_size[1] = 1;
        for bg in 0..3 {
            assert_the_map_is_what_the_renderer_draws(&mut ppu, bg, &[(0, 0), (700, 500)]);
        }
        // Mode 3: BG1's 8 bits through CGRAM, then in direct colour.
        let mut ppu = Ppu::new();
        scramble(&mut ppu, 31);
        ppu.bgmode = 3;
        ppu.bg_tile_size[0] = true;
        assert_the_map_is_what_the_renderer_draws(&mut ppu, 0, &[(0, 0), (100, 200)]);
        ppu.direct_color = true;
        assert!(ppu.bg_direct_colour(0));
        assert_the_map_is_what_the_renderer_draws(&mut ppu, 0, &[(0, 0)]);
        // Mode 5: hires, its entries 16 dots wide; 16x16 tiles on BG2.
        let mut ppu = Ppu::new();
        scramble(&mut ppu, 41);
        ppu.bgmode = 5;
        ppu.bg_screen_size[0] = 1;
        ppu.bg_tile_size[1] = true;
        for bg in 0..2 {
            assert_the_map_is_what_the_renderer_draws(&mut ppu, bg, &[(0, 0), (130, 70)]);
        }
    }

    #[test]
    fn a_map_s_size_follows_the_tiles_and_the_hires_modes() {
        let mut ppu = Ppu::new();
        let size = |ppu: &Ppu, bg: usize| {
            let map = map_of(ppu, bg);
            assert_eq!(map.rgba.len(), (map.width * map.height * 4) as usize);
            (map.width, map.height)
        };
        ppu.bgmode = 1;
        ppu.bg_screen_size[0] = 1;
        assert_eq!(size(&ppu, 0), (512, 256), "64 by 32 entries of 8 by 8");
        ppu.bg_tile_size[0] = true;
        assert_eq!(size(&ppu, 0), (1024, 512), "of 16 by 16");
        ppu.bgmode = 5;
        ppu.bg_tile_size[0] = false;
        assert_eq!(size(&ppu, 0), (1024, 256), "hires: 16 dots by 8");
        // A background the mode does not draw: the map's size, transparent.
        scramble(&mut ppu, 51);
        let off = map_of(&ppu, 2);
        assert_eq!((off.width, off.height), (512, 256));
        assert!(off.rgba.iter().all(|&b| b == 0));
    }

    #[test]
    fn the_map_of_mode_7_is_its_plane() {
        let mut ppu = Ppu::new();
        ppu.bgmode = 7;
        // Map cell (1, 0) names character 2; its pixel (3, 4) is colour 0x85,
        // its pixel (4, 4) colour 0x80, and the rest of the map character 0,
        // all colour 0 but its pixel (0, 0), colour 0x05.
        ppu.vram[1] = 0x0002;
        ppu.vram[2 * 64 + 4 * 8 + 3] |= 0x8500;
        ppu.vram[2 * 64 + 4 * 8 + 4] |= 0x8000;
        ppu.vram[0] |= 0x0500;
        ppu.cgram[0x05] = 0x001F;
        ppu.cgram[0x80] = 0x03E0;
        ppu.cgram[0x85] = 0x7C00;
        let pixel = |map: &PixelImage, x: u32, y: u32| {
            let at = ((y * map.width + x) * 4) as usize;
            [
                map.rgba[at],
                map.rgba[at + 1],
                map.rgba[at + 2],
                map.rgba[at + 3],
            ]
        };
        let plane = map_of(&ppu, 0);
        assert_eq!((plane.width, plane.height), (1024, 1024));
        assert_eq!(pixel(&plane, 11, 4), bgr15_to_rgba8(0x7C00), "8 bits");
        assert_eq!(pixel(&plane, 12, 4), bgr15_to_rgba8(0x03E0));
        assert_eq!(pixel(&plane, 16, 0), bgr15_to_rgba8(0x001F), "character 0");
        assert_eq!(pixel(&plane, 17, 0), [0; 4], "colour 0 is transparent");
        ppu.direct_color = true;
        let direct = map_of(&ppu, 0);
        assert_eq!(
            pixel(&direct, 11, 4),
            bgr15_to_rgba8(direct_color_word(0x85, 0))
        );
        // BG2 under EXTBG: the low 7 bits, colour 0 transparent.
        ppu.mode7_extbg = true;
        let extbg = map_of(&ppu, 1);
        assert_eq!(pixel(&extbg, 11, 4), bgr15_to_rgba8(0x001F), "0x85 as 0x05");
        assert_eq!(pixel(&extbg, 12, 4), [0; 4], "0x80 as 0");
    }

    #[test]
    fn the_sprites_and_a_layer_past_them_have_no_map() {
        let ppu = Ppu::new();
        assert_eq!(ppu.layer_map(LAYER_OBJ as u32), None);
        assert_eq!(ppu.layer_map(LAYER_OBJ as u32 + 1), None);
        assert_eq!(
            ppu.map_image(0),
            Some(map_of(&ppu, 0)),
            "the trait's map is this one"
        );
    }
}
