//! Plane rendering for VDP (Plane A, Plane B, Window).
//!
//! Genesis has 3 background planes: Plane A and Plane B scroll, the window does not.

use super::tiles::{build_rgba_palette, Tile};
use super::window::WindowArea;

/// Nametable entry (2 bytes in VRAM).
///
/// Format (big-endian):
/// ```text
/// Bit 15: Priority
/// Bit 14-13: Palette (0-3)
/// Bit 12: Vertical flip
/// Bit 11: Horizontal flip
/// Bit 10-0: Pattern index (0-2047)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NametableEntry {
    pub pattern_index: u16, // bits 0-10 (2048 patterns)
    pub flip_h: bool,       // bit 11
    pub flip_v: bool,       // bit 12
    pub palette: u8,        // bits 13-14 (4 palettes)
    pub priority: bool,     // bit 15
}

impl NametableEntry {
    /// Decode from 16-bit VRAM word.
    pub fn from_word(word: u16) -> Self {
        Self {
            pattern_index: word & 0x07FF,
            flip_h: (word & 0x0800) != 0,
            flip_v: (word & 0x1000) != 0,
            palette: ((word >> 13) & 0x03) as u8,
            priority: (word & 0x8000) != 0,
        }
    }

    /// Encode to 16-bit VRAM word.
    pub fn to_word(&self) -> u16 {
        self.pattern_index
            | if self.flip_h { 0x0800 } else { 0 }
            | if self.flip_v { 0x1000 } else { 0 }
            | ((self.palette as u16) << 13)
            | if self.priority { 0x8000 } else { 0 }
    }
}

/// Plane configuration.
#[derive(Debug, Clone)]
pub struct PlaneConfig {
    /// Base address of nametable in VRAM.
    pub nametable_addr: u16,

    /// Plane size in tiles (width).
    pub width_tiles: u16,

    /// Plane size in tiles (height).
    pub height_tiles: u16,

    /// Horizontal scroll offset (pixels).
    pub h_scroll: i16,

    /// Vertical scroll offset (pixels).
    pub v_scroll: i16,
}

impl PlaneConfig {
    /// Get nametable entry at (tile_x, tile_y).
    ///
    /// # Arguments
    /// * `vram` - VRAM array
    /// * `tile_x` - X coordinate in tiles
    /// * `tile_y` - Y coordinate in tiles
    ///
    /// # Returns
    /// Nametable entry, or None if out of bounds
    pub fn get_nametable_entry(
        &self,
        vram: &[u8],
        tile_x: u16,
        tile_y: u16,
    ) -> Option<NametableEntry> {
        if tile_x >= self.width_tiles || tile_y >= self.height_tiles {
            return None;
        }

        let offset = (tile_y * self.width_tiles + tile_x) * 2;
        let addr = self.nametable_addr + offset;

        if addr as usize + 1 >= vram.len() {
            return None;
        }

        // Big-endian read
        let word = ((vram[addr as usize] as u16) << 8) | (vram[addr as usize + 1] as u16);
        Some(NametableEntry::from_word(word))
    }

    /// Render plane to framebuffer.
    ///
    /// # Arguments
    /// * `framebuffer` - RGBA framebuffer (320x224 or larger)
    /// * `vram` - VRAM containing patterns and nametable
    /// * `cram` - CRAM (color palette)
    /// * `screen_width` - Width of screen in pixels
    /// * `screen_height` - Height of screen in pixels
    pub fn render(
        &self,
        framebuffer: &mut [u8],
        vram: &[u8],
        cram: &[u16],
        screen_width: u32,
        screen_height: u32,
    ) {
        // Calculate visible tile range with scrolling
        let start_tile_x = (self.h_scroll / 8) as u16;
        let start_tile_y = (self.v_scroll / 8) as u16;
        let pixel_offset_x = (self.h_scroll % 8) as i32;
        let pixel_offset_y = (self.v_scroll % 8) as i32;

        // Render visible tiles (with extra for partial tiles)
        let tiles_x = (screen_width / 8) + 2;
        let tiles_y = (screen_height / 8) + 2;

        for ty in 0..tiles_y {
            for tx in 0..tiles_x {
                // Wrap around plane size
                let tile_x = (start_tile_x + tx as u16) % self.width_tiles;
                let tile_y = (start_tile_y + ty as u16) % self.height_tiles;

                // Get nametable entry
                let entry = match self.get_nametable_entry(vram, tile_x, tile_y) {
                    Some(e) => e,
                    None => continue,
                };

                // Get tile pattern
                let pattern_addr = (entry.pattern_index as usize) * 32;
                if pattern_addr + 32 > vram.len() {
                    continue;
                }

                let tile = Tile::decode(&vram[pattern_addr..pattern_addr + 32]);

                // Get palette
                let palette = build_rgba_palette(cram, entry.palette);

                // Calculate screen position
                let screen_x = (tx as i32) * 8 - pixel_offset_x;
                let screen_y = (ty as i32) * 8 - pixel_offset_y;

                // Render tile
                tile.render_to_framebuffer(
                    framebuffer,
                    screen_x,
                    screen_y,
                    screen_width,
                    &palette,
                    entry.flip_h,
                    entry.flip_v,
                );
            }
        }
    }

    /// Render plane with priority check (for layering).
    ///
    /// Only renders pixels with matching priority flag.
    ///
    /// # Arguments
    /// * `framebuffer` - RGBA framebuffer
    /// * `vram` - VRAM containing patterns and nametable
    /// * `cram` - CRAM (color palette)
    /// * `screen_width` - Width of screen in pixels
    /// * `screen_height` - Height of screen in pixels
    /// * `high_priority` - If true, only render high-priority tiles
    pub fn render_with_priority(
        &self,
        framebuffer: &mut [u8],
        vram: &[u8],
        cram: &[u16],
        screen_width: u32,
        screen_height: u32,
        high_priority: bool,
    ) {
        let start_tile_x = (self.h_scroll / 8) as u16;
        let start_tile_y = (self.v_scroll / 8) as u16;
        let pixel_offset_x = (self.h_scroll % 8) as i32;
        let pixel_offset_y = (self.v_scroll % 8) as i32;

        let tiles_x = (screen_width / 8) + 2;
        let tiles_y = (screen_height / 8) + 2;

        for ty in 0..tiles_y {
            for tx in 0..tiles_x {
                let tile_x = (start_tile_x + tx as u16) % self.width_tiles;
                let tile_y = (start_tile_y + ty as u16) % self.height_tiles;

                let entry = match self.get_nametable_entry(vram, tile_x, tile_y) {
                    Some(e) => e,
                    None => continue,
                };

                // Priority filter
                if entry.priority != high_priority {
                    continue;
                }

                let pattern_addr = (entry.pattern_index as usize) * 32;
                if pattern_addr + 32 > vram.len() {
                    continue;
                }

                let tile = Tile::decode(&vram[pattern_addr..pattern_addr + 32]);
                let palette = build_rgba_palette(cram, entry.palette);
                let screen_x = (tx as i32) * 8 - pixel_offset_x;
                let screen_y = (ty as i32) * 8 - pixel_offset_y;

                tile.render_to_framebuffer(
                    framebuffer,
                    screen_x,
                    screen_y,
                    screen_width,
                    &palette,
                    entry.flip_h,
                    entry.flip_v,
                );
            }
        }
    }
}

/// Window plane configuration.
///
/// Window plane is always fixed (no scrolling) and covers a portion of the screen.
#[derive(Debug, Clone)]
pub struct WindowConfig {
    /// Base address of nametable in VRAM.
    pub nametable_addr: u16,

    /// Window width in tiles.
    pub width_tiles: u16,

    /// Window height in tiles.
    pub height_tiles: u16,

    /// The column edge, in tiles: the window takes the tiles from it
    /// rightwards (`right_side`) or the tiles left of it.
    pub h_pos: u16,

    /// The line edge, in tiles: the window takes the rows from it down
    /// (`down_side`) or the rows above it.
    pub v_pos: u16,

    /// The window grows from the right edge (true) or the left (false).
    pub right_side: bool,

    /// The window grows from the bottom edge (true) or the top (false).
    pub down_side: bool,
}

impl WindowConfig {
    /// Get nametable entry at (tile_x, tile_y).
    pub fn get_nametable_entry(
        &self,
        vram: &[u8],
        tile_x: u16,
        tile_y: u16,
    ) -> Option<NametableEntry> {
        if tile_x >= self.width_tiles || tile_y >= self.height_tiles {
            return None;
        }

        let offset = (tile_y * self.width_tiles + tile_x) * 2;
        let addr = self.nametable_addr + offset;

        if addr as usize + 1 >= vram.len() {
            return None;
        }

        let word = ((vram[addr as usize] as u16) << 8) | (vram[addr as usize + 1] as u16);
        Some(NametableEntry::from_word(word))
    }

    /// Whether the window covers screen position (x, y), by the rule the
    /// renderer uses ([`WindowArea`]): the rows its line edge picks, plus
    /// the columns its column edge picks on the other rows. An edge at 0
    /// gives nothing from the top or left and everything from the bottom or
    /// right.
    pub fn is_visible_at(&self, screen_x: u32, screen_y: u32) -> bool {
        let area = WindowArea {
            column_edge: self.h_pos.saturating_mul(8),
            from_right: self.right_side,
            line_edge: self.v_pos.saturating_mul(8),
            from_bottom: self.down_side,
        };
        let x = u16::try_from(screen_x).unwrap_or(u16::MAX);
        let y = u16::try_from(screen_y).unwrap_or(u16::MAX);
        area.covers(x, y)
    }

    /// Render window plane.
    pub fn render(
        &self,
        framebuffer: &mut [u8],
        vram: &[u8],
        cram: &[u16],
        screen_width: u32,
        screen_height: u32,
    ) {
        let tiles_x = screen_width / 8;
        let tiles_y = screen_height / 8;

        for ty in 0..tiles_y {
            for tx in 0..tiles_x {
                if !self.is_visible_at(tx * 8, ty * 8) {
                    continue;
                }

                let entry = match self.get_nametable_entry(vram, tx as u16, ty as u16) {
                    Some(e) => e,
                    None => continue,
                };

                let pattern_addr = (entry.pattern_index as usize) * 32;
                if pattern_addr + 32 > vram.len() {
                    continue;
                }

                let tile = Tile::decode(&vram[pattern_addr..pattern_addr + 32]);
                let palette = build_rgba_palette(cram, entry.palette);

                tile.render_to_framebuffer(
                    framebuffer,
                    (tx * 8) as i32,
                    (ty * 8) as i32,
                    screen_width,
                    &palette,
                    entry.flip_h,
                    entry.flip_v,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nametable_entry_decode() {
        // Pattern 0x123, no flips, palette 0, no priority
        let word = 0x0123;
        let entry = NametableEntry::from_word(word);

        assert_eq!(entry.pattern_index, 0x123);
        assert!(!entry.flip_h);
        assert!(!entry.flip_v);
        assert_eq!(entry.palette, 0);
        assert!(!entry.priority);
    }

    #[test]
    fn test_nametable_entry_priority() {
        let word = 0x8000; // Priority bit set
        let entry = NametableEntry::from_word(word);
        assert!(entry.priority);
    }

    #[test]
    fn test_nametable_entry_palette() {
        // Palette 3 (bits 13-14)
        let word = 0x6000;
        let entry = NametableEntry::from_word(word);
        assert_eq!(entry.palette, 3);
    }

    #[test]
    fn test_nametable_entry_flip_v() {
        let word = 0x1000; // V-flip bit set
        let entry = NametableEntry::from_word(word);
        assert!(entry.flip_v);
        assert!(!entry.flip_h);
    }

    #[test]
    fn test_nametable_entry_flip_h() {
        let word = 0x0800; // H-flip bit set
        let entry = NametableEntry::from_word(word);
        assert!(entry.flip_h);
        assert!(!entry.flip_v);
    }

    #[test]
    fn test_nametable_entry_all_flags() {
        // Pattern 0x7FF (max 11-bit value), both flips, palette 2, priority
        let word = 0xDFFF; // 1101 1111 1111 1111
        let entry = NametableEntry::from_word(word);

        assert_eq!(entry.pattern_index, 0x7FF); // 2047 (max pattern index)
        assert!(entry.flip_h);
        assert!(entry.flip_v);
        assert_eq!(entry.palette, 2);
        assert!(entry.priority);
    }

    #[test]
    fn test_nametable_entry_encode() {
        let entry = NametableEntry {
            pattern_index: 0x123,
            flip_h: true,
            flip_v: false,
            palette: 1,
            priority: true,
        };

        let word = entry.to_word();
        assert_eq!(word, 0xA923); // 1010 1001 0010 0011
    }

    #[test]
    fn test_nametable_entry_roundtrip() {
        let original = NametableEntry {
            pattern_index: 0x456,
            flip_h: true,
            flip_v: true,
            palette: 3,
            priority: false,
        };

        let word = original.to_word();
        let decoded = NametableEntry::from_word(word);

        assert_eq!(decoded, original);
    }

    #[test]
    fn test_plane_get_nametable_entry() {
        // Create a small test VRAM
        let mut vram = vec![0u8; 0x10000];

        // Write a nametable entry at offset 0
        // Pattern 0, palette 1, flip H
        vram[0] = 0x28; // 0010 1000
        vram[1] = 0x00; // 0000 0000

        let config = PlaneConfig {
            nametable_addr: 0,
            width_tiles: 32,
            height_tiles: 32,
            h_scroll: 0,
            v_scroll: 0,
        };

        let entry = config.get_nametable_entry(&vram, 0, 0).unwrap();
        assert_eq!(entry.pattern_index, 0x000);
        assert_eq!(entry.palette, 1);
        assert!(entry.flip_h);
    }

    #[test]
    fn test_plane_get_nametable_entry_out_of_bounds() {
        let vram = vec![0u8; 0x10000];

        let config = PlaneConfig {
            nametable_addr: 0,
            width_tiles: 32,
            height_tiles: 32,
            h_scroll: 0,
            v_scroll: 0,
        };

        // Out of bounds
        assert!(config.get_nametable_entry(&vram, 32, 0).is_none());
        assert!(config.get_nametable_entry(&vram, 0, 32).is_none());
    }

    #[test]
    fn test_plane_render_single_tile() {
        // Create test VRAM with pattern and nametable
        let mut vram = vec![0u8; 0x10000];

        // Pattern at address 0: filled tile, all pixels = color 1
        // In packed nibble format: 0x11 means pixel=1, pixel=1
        for row in 0..8 {
            let base = row * 4;
            vram[base] = 0x11; // pixels 0,1 = color 1
            vram[base + 1] = 0x11; // pixels 2,3 = color 1
            vram[base + 2] = 0x11; // pixels 4,5 = color 1
            vram[base + 3] = 0x11; // pixels 6,7 = color 1
        }

        // Nametable at address 0x1000: one entry pointing to pattern 0
        vram[0x1000] = 0x00;
        vram[0x1001] = 0x00; // Pattern 0, palette 0

        // CRAM: palette 0, color 1 = red
        let mut cram = vec![0u16; 64];
        cram[1] = 0x000E; // Red

        let config = PlaneConfig {
            nametable_addr: 0x1000,
            width_tiles: 32,
            height_tiles: 32,
            h_scroll: 0,
            v_scroll: 0,
        };

        let mut framebuffer = vec![0u8; 320 * 224 * 4];
        config.render(&mut framebuffer, &vram, &cram, 320, 224);

        // Check pixel at (0, 0) should be red
        assert_eq!(framebuffer[0], 252); // R
        assert_eq!(framebuffer[1], 0); // G
        assert_eq!(framebuffer[2], 0); // B
    }

    #[test]
    fn test_window_is_visible_at_right() {
        let config = WindowConfig {
            nametable_addr: 0,
            width_tiles: 32,
            height_tiles: 32,
            h_pos: 20,
            v_pos: 0,
            right_side: true,
            down_side: false,
        };

        assert!(!config.is_visible_at(152, 0)); // Tile 19
        assert!(config.is_visible_at(160, 0)); // Tile 20
    }

    #[test]
    fn test_window_is_visible_at_left() {
        let config = WindowConfig {
            nametable_addr: 0,
            width_tiles: 32,
            height_tiles: 32,
            h_pos: 20,
            v_pos: 0,
            right_side: false,
            down_side: false,
        };

        assert!(config.is_visible_at(152, 0));
        assert!(!config.is_visible_at(160, 0));
    }

    #[test]
    fn test_window_is_visible_at_follows_the_renderer_rule() {
        let config = |h_pos, right_side, v_pos, down_side| WindowConfig {
            nametable_addr: 0,
            width_tiles: 32,
            height_tiles: 32,
            h_pos,
            v_pos,
            right_side,
            down_side,
        };

        // Edges at 0 from the top and left: nothing.
        assert!(!config(0, false, 0, false).is_visible_at(100, 100));
        // A column edge at 0 from the right: every line.
        assert!(config(0, true, 0, false).is_visible_at(0, 0));
        assert!(config(0, true, 0, false).is_visible_at(319, 223));
        // A line edge at 0 from the bottom: every line.
        assert!(config(0, false, 0, true).is_visible_at(319, 223));

        // Rows from tile 15 down, plus tiles from column 20 on the rows
        // above: the union, not the intersection.
        let both = config(20, true, 15, true);
        assert!(
            both.is_visible_at(0, 120),
            "a whole row below the line edge"
        );
        assert!(
            both.is_visible_at(160, 0),
            "a column right of the column edge"
        );
        assert!(!both.is_visible_at(0, 0), "neither");
    }

    #[test]
    fn test_window_render() {
        let mut vram = vec![0u8; 0x10000];

        // Fill pattern 0: all pixels = color 1 (nibble format: 0x11)
        for row in 0..8 {
            let base = row * 4;
            vram[base] = 0x11;
            vram[base + 1] = 0x11;
            vram[base + 2] = 0x11;
            vram[base + 3] = 0x11;
        }

        let nametable_addr = 0x1000;
        let entry = NametableEntry {
            pattern_index: 0,
            flip_h: false,
            flip_v: false,
            palette: 0,
            priority: false,
        };
        let word = entry.to_word();
        vram[nametable_addr] = (word >> 8) as u8;
        vram[nametable_addr + 1] = word as u8;

        let mut cram = vec![0u16; 64];
        cram[1] = 0x000E;

        let config = WindowConfig {
            nametable_addr: nametable_addr as u16,
            width_tiles: 32,
            height_tiles: 32,
            h_pos: 20,
            v_pos: 15,
            right_side: true,
            down_side: true,
        };

        let mut fb = vec![0u8; 320 * 224 * 4];
        config.render(&mut fb, &vram, &cram, 320, 224);

        let idx = ((120 * 320 + 160) * 4) as usize;
        assert_eq!(fb[idx], 252);
        assert_eq!(fb[0], 0);
    }

    #[test]
    fn test_plane_render_with_priority() {
        let mut vram = vec![0u8; 0x10000];

        // Pattern 0: all pixels = color 1 (nibble format: 0x11)
        for row in 0..8 {
            let base = row * 4;
            vram[base] = 0x11;
            vram[base + 1] = 0x11;
            vram[base + 2] = 0x11;
            vram[base + 3] = 0x11;
        }
        // Pattern 1: all pixels = color 2 (nibble format: 0x22)
        for row in 0..8 {
            let base = 32 + row * 4;
            vram[base] = 0x22;
            vram[base + 1] = 0x22;
            vram[base + 2] = 0x22;
            vram[base + 3] = 0x22;
        }

        let nametable_addr = 0x1000;

        let entry0 = NametableEntry {
            pattern_index: 0,
            flip_h: false,
            flip_v: false,
            palette: 0,
            priority: false,
        };
        let word0 = entry0.to_word();
        vram[nametable_addr] = (word0 >> 8) as u8;
        vram[nametable_addr + 1] = word0 as u8;

        let entry1 = NametableEntry {
            pattern_index: 1,
            flip_h: false,
            flip_v: false,
            palette: 0,
            priority: true,
        };
        let word1 = entry1.to_word();
        vram[nametable_addr + 2] = (word1 >> 8) as u8;
        vram[nametable_addr + 3] = word1 as u8;

        let mut cram = vec![0u16; 64];
        cram[1] = 0x000E;
        cram[2] = 0x00E0;

        let config = PlaneConfig {
            nametable_addr: nametable_addr as u16,
            width_tiles: 32,
            height_tiles: 32,
            h_scroll: 0,
            v_scroll: 0,
        };

        let mut fb_high = vec![0u8; 320 * 224 * 4];
        config.render_with_priority(&mut fb_high, &vram, &cram, 320, 224, true);
        assert_eq!(fb_high[0], 0);

        let idx = (8 * 4) as usize;
        assert_eq!(fb_high[idx + 1], 252);

        let mut fb_low = vec![0u8; 320 * 224 * 4];
        config.render_with_priority(&mut fb_low, &vram, &cram, 320, 224, false);
        assert_eq!(fb_low[0], 252);
    }
}
