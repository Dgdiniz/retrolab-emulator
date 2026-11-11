//! Tile rendering and decoding for VDP.
//!
//! Handles 8x8 tile patterns (4bpp) and conversion to indexed pixels.

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

    /// Render tile to a framebuffer at specific position.
    ///
    /// # Arguments
    /// * `framebuffer` - RGBA framebuffer (stride = width * 4)
    /// * `x` - X position in framebuffer
    /// * `y` - Y position in framebuffer
    /// * `width` - Framebuffer width
    /// * `palette` - 16-color palette (RGBA format)
    /// * `flip_h` - Horizontal flip
    /// * `flip_v` - Vertical flip
    #[allow(clippy::too_many_arguments)]
    pub fn render_to_framebuffer(
        &self,
        framebuffer: &mut [u8],
        x: i32,
        y: i32,
        width: u32,
        palette: &[(u8, u8, u8, u8); 16],
        flip_h: bool,
        flip_v: bool,
    ) {
        for ty in 0..8 {
            for tx in 0..8 {
                let pixel = self.get_pixel(tx, ty, flip_h, flip_v);

                // Color 0 is transparent (don't draw)
                if pixel == 0 {
                    continue;
                }

                let screen_x = x + tx as i32;
                let screen_y = y + ty as i32;

                // Clip to the framebuffer's width and to 224 lines
                if screen_x < 0 || screen_y < 0 {
                    continue;
                }
                if screen_x >= width as i32 || screen_y >= 224 {
                    continue;
                }

                let idx = ((screen_y * width as i32 + screen_x) * 4) as usize;
                let color = palette[pixel as usize];

                framebuffer[idx] = color.0; // R
                framebuffer[idx + 1] = color.1; // G
                framebuffer[idx + 2] = color.2; // B
                framebuffer[idx + 3] = color.3; // A
            }
        }
    }
}

/// Convert Genesis 9-bit color to 24-bit RGB.
///
/// Genesis color format (CRAM):
/// - Bits 1-3: Red (3 bits)
/// - Bits 5-7: Green (3 bits)
/// - Bits 9-11: Blue (3 bits)
/// - Bit 0, 4, 8: Unused (not stored; shadow/highlight is register 12 bit 3)
///
/// # Arguments
/// * `genesis_color` - 9-bit color value (stored as u16)
///
/// # Returns
/// (R, G, B) tuple, each 0-255
pub fn convert_genesis_color_to_rgb(genesis_color: u16) -> (u8, u8, u8) {
    let r = ((genesis_color >> 1) & 0x07) as u8;
    let g = ((genesis_color >> 5) & 0x07) as u8;
    let b = ((genesis_color >> 9) & 0x07) as u8;

    // Convert 3-bit (0-7) to 8-bit (0-255)
    // Simple method: multiply by 36 (approx 255/7)
    // More accurate: r * 255 / 7, but 36 is faster
    let r = r * 36;
    let g = g * 36;
    let b = b * 36;

    (r, g, b)
}

/// Build RGBA palette from CRAM palette.
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
        let (r, g, b) = convert_genesis_color_to_rgb(cram[base + i]);
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
    fn test_color_convert_black() {
        let (r, g, b) = convert_genesis_color_to_rgb(0x000);
        assert_eq!((r, g, b), (0, 0, 0));
    }

    #[test]
    fn test_color_convert_white() {
        // Max values: R=7, G=7, B=7
        // Encoded as: (7 << 1) | (7 << 5) | (7 << 9) = 0x0EEE
        let (r, g, b) = convert_genesis_color_to_rgb(0x0EEE);
        assert_eq!(r, 252); // 7 * 36 = 252
        assert_eq!(g, 252);
        assert_eq!(b, 252);
    }

    #[test]
    fn test_color_convert_red() {
        // Red = 7, Green = 0, Blue = 0
        // Encoded as: (7 << 1) = 0x000E
        let (r, g, b) = convert_genesis_color_to_rgb(0x000E);
        assert_eq!(r, 252);
        assert_eq!(g, 0);
        assert_eq!(b, 0);
    }

    #[test]
    fn test_color_convert_green() {
        // Red = 0, Green = 7, Blue = 0
        // Encoded as: (7 << 5) = 0x00E0
        let (r, g, b) = convert_genesis_color_to_rgb(0x00E0);
        assert_eq!(r, 0);
        assert_eq!(g, 252);
        assert_eq!(b, 0);
    }

    #[test]
    fn test_color_convert_blue() {
        // Red = 0, Green = 0, Blue = 7
        // Encoded as: (7 << 9) = 0x0E00
        let (r, g, b) = convert_genesis_color_to_rgb(0x0E00);
        assert_eq!(r, 0);
        assert_eq!(g, 0);
        assert_eq!(b, 252);
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
        assert_eq!(palette[1], (252, 0, 0, 255)); // Red
        assert_eq!(palette[15], (252, 252, 252, 255)); // White
    }
}
