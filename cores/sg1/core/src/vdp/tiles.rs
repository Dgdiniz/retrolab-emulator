//! Tile decoding for the VDP's debugger views and exports.
//!
//! Handles 8x8 tile patterns (4bpp) and conversion to indexed pixels, and
//! a palette line's colours as the screen shows them
//! ([`build_rgba_palette`]). The renderer reads its pixels straight from
//! VRAM as it draws (`Vdp::read_tile_pixel_from_vram`); this decode is for
//! the views and exports that want a whole tile.

use super::Vdp;

/// A decoded 8x8 tile (64 pixels, 4-bit indexed color per pixel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    /// Pixel data: 64 pixels (8x8), each pixel is 4-bit palette index (0-15)
    pub pixels: [u8; 64],
}

impl Tile {
    /// Decode a tile from VRAM pattern data.
    ///
    /// Genesis tiles are stored in packed nibble format:
    /// - 32 bytes per tile (8 rows x 4 bytes)
    /// - Each byte contains 2 pixels: high nibble = left pixel, low nibble = right pixel
    /// - Each pixel is a 4-bit palette index (0-15)
    ///
    /// Row layout (4 bytes = 8 pixels):
    ///   byte 0: pixel 0 (hi nibble), pixel 1 (lo nibble)
    ///   byte 1: pixel 2 (hi nibble), pixel 3 (lo nibble)
    ///   byte 2: pixel 4 (hi nibble), pixel 5 (lo nibble)
    ///   byte 3: pixel 6 (hi nibble), pixel 7 (lo nibble)
    ///
    /// # Arguments
    /// * `pattern_data` - 32-byte slice from VRAM
    ///
    /// # Returns
    /// Decoded tile with 64 indexed pixels
    pub fn decode(pattern_data: &[u8]) -> Self {
        assert_eq!(pattern_data.len(), 32, "Tile pattern must be 32 bytes");

        let mut pixels = [0u8; 64];

        for row in 0..8 {
            let base = row * 4; // 4 bytes per row

            // Each byte holds 2 pixels (high nibble = left, low nibble = right)
            for byte_idx in 0..4 {
                let byte = pattern_data[base + byte_idx];
                let col = byte_idx * 2;

                pixels[row * 8 + col] = (byte >> 4) & 0x0F; // High nibble = left pixel
                pixels[row * 8 + col + 1] = byte & 0x0F; // Low nibble = right pixel
            }
        }

        Self { pixels }
    }

    /// Get pixel at (x, y) with optional flipping.
    ///
    /// # Arguments
    /// * `x` - X coordinate (0-7)
    /// * `y` - Y coordinate (0-7)
    /// * `flip_h` - Horizontal flip
    /// * `flip_v` - Vertical flip
    ///
    /// # Returns
    /// 4-bit palette index (0-15)
    pub fn get_pixel(&self, x: u8, y: u8, flip_h: bool, flip_v: bool) -> u8 {
        let x = if flip_h { 7 - x } else { x };
        let y = if flip_v { 7 - y } else { y };
        self.pixels[(y * 8 + x) as usize]
    }
}

/// Build RGBA palette from CRAM palette: each colour as the screen shows
/// it ([`Vdp::convert_genesis_color`]), opaque.
///
/// # Arguments
/// * `cram` - CRAM array (64 colors)
/// * `palette_index` - Which palette to use (0-3)
///
/// # Returns
/// 16-color RGBA palette
pub fn build_rgba_palette(cram: &[u16], palette_index: u8) -> [(u8, u8, u8, u8); 16] {
    let base = (palette_index as usize) * 16;
    let mut palette = [(0u8, 0u8, 0u8, 255u8); 16];

    for i in 0..16 {
        let (r, g, b) = Vdp::convert_genesis_color(cram[base + i]);
        palette[i] = (r, g, b, 255);
    }

    palette
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tile_decode_all_zeros() {
        let pattern = [0u8; 32];
        let tile = Tile::decode(&pattern);

        for pixel in tile.pixels.iter() {
            assert_eq!(*pixel, 0, "All pixels should be color 0");
        }
    }

    #[test]
    fn test_tile_decode_all_ones() {
        // All bits set = color 15 (0xF)
        let pattern = [0xFFu8; 32];
        let tile = Tile::decode(&pattern);

        for pixel in tile.pixels.iter() {
            assert_eq!(*pixel, 15, "All pixels should be color 15");
        }
    }

    #[test]
    fn test_tile_decode_nibble_pattern() {
        // In packed nibble format, 0xAB means pixel=0xA (left), pixel=0xB (right)
        let mut pattern = [0u8; 32];
        // Row 0: byte 0 = 0x12 -> pixels 0,1 = (1, 2)
        //         byte 1 = 0x34 -> pixels 2,3 = (3, 4)
        //         byte 2 = 0x00 -> pixels 4,5 = (0, 0)
        //         byte 3 = 0x00 -> pixels 6,7 = (0, 0)
        pattern[0] = 0x12;
        pattern[1] = 0x34;

        let tile = Tile::decode(&pattern);

        assert_eq!(tile.pixels[0], 1);
        assert_eq!(tile.pixels[1], 2);
        assert_eq!(tile.pixels[2], 3);
        assert_eq!(tile.pixels[3], 4);
        assert_eq!(tile.pixels[4], 0);
        assert_eq!(tile.pixels[5], 0);
    }

    #[test]
    fn test_tile_flip_horizontal() {
        let mut pattern = [0u8; 32];
        // Row 0: first pixel = color 0xF (nibble format: 0xF0 = pixel 0xF, pixel 0x0)
        pattern[0] = 0xF0; // pixel 0 = 0xF, pixel 1 = 0x0

        let tile = Tile::decode(&pattern);

        // Without flip: first pixel is color 15
        assert_eq!(tile.get_pixel(0, 0, false, false), 15);
        assert_eq!(tile.get_pixel(7, 0, false, false), 0);

        // With H-flip: first pixel becomes last
        assert_eq!(tile.get_pixel(0, 0, true, false), 0);
        assert_eq!(tile.get_pixel(7, 0, true, false), 15);
    }

    #[test]
    fn test_tile_flip_vertical() {
        let mut pattern = [0u8; 32];
        // Row 0: first pixel = color 0xF
        pattern[0] = 0xF0; // pixel 0 = 0xF, pixel 1 = 0x0

        let tile = Tile::decode(&pattern);

        // Without flip: top-left is color 15
        assert_eq!(tile.get_pixel(0, 0, false, false), 15);
        assert_eq!(tile.get_pixel(0, 7, false, false), 0);

        // With V-flip: top becomes bottom
        assert_eq!(tile.get_pixel(0, 0, false, true), 0);
        assert_eq!(tile.get_pixel(0, 7, false, true), 15);
    }

    #[test]
    fn test_build_rgba_palette() {
        let mut cram = [0u16; 64];
        // Palette 0: first 16 colors
        cram[0] = 0x000; // Black
        cram[1] = 0x00E; // Red
        cram[15] = 0x0EEE; // White

        let palette = build_rgba_palette(&cram, 0);

        assert_eq!(palette[0], (0, 0, 0, 255)); // Black
        assert_eq!(palette[1], (255, 0, 0, 255)); // Red
        assert_eq!(palette[15], (255, 255, 255, 255)); // White
    }
}
