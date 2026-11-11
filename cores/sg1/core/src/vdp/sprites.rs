//! Sprite rendering for VDP.
//!
//! Genesis supports 80 sprites with up to 20 per scanline in H40 (64 and 16 in H32).

use super::tiles::{build_rgba_palette, Tile};

/// Sprite data structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sprite {
    /// X position (screen coordinates)
    pub x: i16,

    /// Y position (screen coordinates, offset by 128)
    pub y: i16,

    /// Width in tiles (1-4)
    pub width_tiles: u8,

    /// Height in tiles (1-4)
    pub height_tiles: u8,

    /// Pattern base index in VRAM
    pub pattern_base: u16,

    /// Palette (0-3)
    pub palette: u8,

    /// Priority flag
    pub priority: bool,

    /// Horizontal flip
    pub flip_h: bool,

    /// Vertical flip
    pub flip_v: bool,

    /// Link to next sprite (0-79, or 0 = end)
    pub link: u8,
}

impl Sprite {
    /// Parse sprite from SAT (Sprite Attribute Table).
    ///
    /// SAT format (8 bytes / 4 words per sprite):
    /// - Word 0 (bytes 0-1): Y position (bits 0-8)
    /// - Word 1 (bytes 2-3): V size (bits 8-9), H size (bits 10-11), Link (bits 0-6)
    /// - Word 2 (bytes 4-5): Pattern (bits 0-10), H flip (bit 11), V flip (bit 12),
    ///   Palette (bits 13-14), Priority (bit 15)
    /// - Word 3 (bytes 6-7): X position (bits 0-8)
    ///
    /// (The documented attribute layout; checked against MAME and Genesis
    /// Plus GX.)
    pub fn from_sat(vram: &[u8], sat_base: u16, index: u8) -> Option<Self> {
        let addr = (sat_base + (index as u16 * 8)) as usize;

        if addr + 8 > vram.len() {
            return None;
        }

        // Read all 4 words (big-endian)
        let word0 = ((vram[addr] as u16) << 8) | (vram[addr + 1] as u16);
        let word1 = ((vram[addr + 2] as u16) << 8) | (vram[addr + 3] as u16);
        let word2 = ((vram[addr + 4] as u16) << 8) | (vram[addr + 5] as u16);
        let word3 = ((vram[addr + 6] as u16) << 8) | (vram[addr + 7] as u16);

        // Word 0: Y position (bits 0-8)
        let y_pos = (word0 & 0x01FF) as i16;

        // Word 1: V size (bits 8-9), H size (bits 10-11), Link (bits 0-6)
        let v_size = ((word1 >> 8) & 0x03) as u8;
        let h_size = ((word1 >> 10) & 0x03) as u8;
        let link = (word1 & 0x7F) as u8;

        // Word 2: Pattern (bits 0-10), H flip (bit 11), V flip (bit 12),
        //         Palette (bits 13-14), Priority (bit 15)
        let pattern_base = word2 & 0x07FF;
        let flip_h = (word2 & 0x0800) != 0;
        let flip_v = (word2 & 0x1000) != 0;
        let palette = ((word2 >> 13) & 0x03) as u8;
        let priority = (word2 & 0x8000) != 0;

        // Word 3: X position (bits 0-8)
        let x_pos = (word3 & 0x01FF) as i16;

        Some(Sprite {
            x: x_pos - 128, // Offset by 128 for screen coordinates
            y: y_pos - 128,
            width_tiles: h_size + 1,
            height_tiles: v_size + 1,
            pattern_base,
            palette,
            priority,
            flip_h,
            flip_v,
            link,
        })
    }

    /// Get bounding box in screen coordinates.
    pub fn bounds(&self) -> (i16, i16, i16, i16) {
        let width = (self.width_tiles as i16) * 8;
        let height = (self.height_tiles as i16) * 8;
        (self.x, self.y, self.x + width, self.y + height)
    }

    /// Check if sprite is visible on screen.
    pub fn is_visible(&self, screen_width: u32, screen_height: u32) -> bool {
        let (x1, y1, x2, y2) = self.bounds();

        // Check if sprite overlaps screen area
        !(x2 <= 0 || x1 >= screen_width as i16 || y2 <= 0 || y1 >= screen_height as i16)
    }

    /// Render sprite to framebuffer.
    pub fn render(
        &self,
        framebuffer: &mut [u8],
        vram: &[u8],
        cram: &[u16],
        screen_width: u32,
        _screen_height: u32,
    ) {
        let palette = build_rgba_palette(cram, self.palette);

        // Render each tile in sprite
        for ty in 0..self.height_tiles {
            for tx in 0..self.width_tiles {
                // Calculate tile index in pattern (column-major: top-to-bottom, left-to-right)
                // Apply flips to source tile coordinates
                let src_tx = if self.flip_h {
                    self.width_tiles - 1 - tx
                } else {
                    tx
                };
                let src_ty = if self.flip_v {
                    self.height_tiles - 1 - ty
                } else {
                    ty
                };
                let tile_offset = src_tx * self.height_tiles + src_ty;

                let pattern_index = self.pattern_base + tile_offset as u16;
                let pattern_addr = (pattern_index as usize) * 32;

                if pattern_addr + 32 > vram.len() {
                    continue;
                }

                let tile = Tile::decode(&vram[pattern_addr..pattern_addr + 32]);

                // Calculate screen position
                let screen_x = self.x + (tx as i16) * 8;
                let screen_y = self.y + (ty as i16) * 8;

                // Render tile
                tile.render_to_framebuffer(
                    framebuffer,
                    screen_x as i32,
                    screen_y as i32,
                    screen_width,
                    &palette,
                    self.flip_h,
                    self.flip_v,
                );
            }
        }
    }
}

/// Sprite cache for efficient rendering.
pub struct SpriteCache {
    sprites: Vec<Sprite>,
}

impl SpriteCache {
    pub fn new() -> Self {
        Self {
            sprites: Vec::with_capacity(80),
        }
    }

    /// Get direct access to the sprite list.
    pub fn sprites(&self) -> &[Sprite] {
        &self.sprites
    }

    /// Get number of sprites in cache.
    pub fn len(&self) -> usize {
        self.sprites.len()
    }

    /// Check if cache is empty.
    pub fn is_empty(&self) -> bool {
        self.sprites.is_empty()
    }

    /// Build sprite cache from SAT, following link chain.
    pub fn build(&mut self, vram: &[u8], sat_base: u16, max_sprites: u8) {
        self.sprites.clear();

        let mut current = 0u8;
        let mut count = 0;

        loop {
            if count >= max_sprites || count >= 80 {
                break;
            }

            if let Some(sprite) = Sprite::from_sat(vram, sat_base, current) {
                let link = sprite.link;
                self.sprites.push(sprite);
                count += 1;

                // Stop on a null link or one that points past the valid SAT
                // range (>= 64 entries in H32, >= 80 in H40). Without it a
                // chain into the out-of-range indices renders phantom sprites
                // past the per-frame limit (Genesis Plus GX stops its walk
                // there too).
                if link == 0 || link >= max_sprites {
                    break;
                }
                current = link;
            } else {
                break;
            }
        }
    }

    /// Get sprites overlapping a specific scanline (max 20).
    pub fn get_sprites_on_line(&self, scanline: i16) -> Vec<&Sprite> {
        self.sprites
            .iter()
            .filter(|s| {
                let (_, y1, _, y2) = s.bounds();
                scanline >= y1 && scanline < y2
            })
            .take(20) // H40's limit of 20 sprites per line (H32's is 16)
            .collect()
    }

    /// Render all sprites to framebuffer.
    pub fn render_all(
        &self,
        framebuffer: &mut [u8],
        vram: &[u8],
        cram: &[u16],
        screen_width: u32,
        screen_height: u32,
    ) {
        for sprite in &self.sprites {
            if sprite.is_visible(screen_width, screen_height) {
                sprite.render(framebuffer, vram, cram, screen_width, screen_height);
            }
        }
    }
}

impl Default for SpriteCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sprite_parse_sat_simple() {
        let mut vram = vec![0u8; 0x10000];

        // Create SAT entry at address 0
        // Word 0: y_pos=150
        let word0: u16 = 0x0096;
        vram[0] = (word0 >> 8) as u8;
        vram[1] = word0 as u8;

        // Word 1: v_size=0, h_size=0, link=0
        let word1: u16 = 0x0000;
        vram[2] = (word1 >> 8) as u8;
        vram[3] = word1 as u8;

        // Word 2: pattern_base=0, no flags
        let word2: u16 = 0x0000;
        vram[4] = (word2 >> 8) as u8;
        vram[5] = word2 as u8;

        // Word 3: x_pos=160
        let word3: u16 = 0x00A0;
        vram[6] = (word3 >> 8) as u8;
        vram[7] = word3 as u8;

        let sprite = Sprite::from_sat(&vram, 0, 0).unwrap();

        assert_eq!(sprite.x, 160 - 128); // 32
        assert_eq!(sprite.y, 150 - 128); // 22
        assert_eq!(sprite.width_tiles, 1);
        assert_eq!(sprite.height_tiles, 1);
        assert_eq!(sprite.pattern_base, 0);
        assert_eq!(sprite.palette, 0);
        assert!(!sprite.priority);
        assert!(!sprite.flip_h);
        assert!(!sprite.flip_v);
        assert_eq!(sprite.link, 0);
    }

    #[test]
    fn test_sprite_parse_sat_with_size() {
        let mut vram = vec![0u8; 0x10000];

        // Word 0: y=150
        let word0: u16 = 0x0096;
        vram[0] = (word0 >> 8) as u8;
        vram[1] = word0 as u8;

        // Word 1: v_size=3 (bits 8-9), h_size=3 (bits 10-11), link=0
        // 4x4 sprite: (3 << 8) | (3 << 10) = 0x0300 | 0x0C00 = 0x0F00
        let word1: u16 = 0x0F00;
        vram[2] = (word1 >> 8) as u8;
        vram[3] = word1 as u8;

        // Word 2: pattern=0
        let word2: u16 = 0x0000;
        vram[4] = (word2 >> 8) as u8;
        vram[5] = word2 as u8;

        // Word 3: x=160
        let word3: u16 = 0x00A0;
        vram[6] = (word3 >> 8) as u8;
        vram[7] = word3 as u8;

        let sprite = Sprite::from_sat(&vram, 0, 0).unwrap();

        assert_eq!(sprite.width_tiles, 4);
        assert_eq!(sprite.height_tiles, 4);
    }

    #[test]
    fn test_sprite_parse_sat_with_flags() {
        let mut vram = vec![0u8; 0x10000];

        // Word 0: y=150
        let word0: u16 = 0x0096;
        vram[0] = (word0 >> 8) as u8;
        vram[1] = word0 as u8;

        // Word 1: size=0, link=0
        let word1: u16 = 0x0000;
        vram[2] = (word1 >> 8) as u8;
        vram[3] = word1 as u8;

        // Word 2: priority=1 (bit 15), palette=3 (bits 13-14), flip_v=1 (bit 12), flip_h=1 (bit 11)
        // 0x8000 | 0x6000 | 0x1000 | 0x0800 = 0xF800
        let word2: u16 = 0xF800;
        vram[4] = (word2 >> 8) as u8;
        vram[5] = word2 as u8;

        // Word 3: x=160
        let word3: u16 = 0x00A0;
        vram[6] = (word3 >> 8) as u8;
        vram[7] = word3 as u8;

        let sprite = Sprite::from_sat(&vram, 0, 0).unwrap();

        assert!(sprite.priority);
        assert_eq!(sprite.palette, 3);
        assert!(sprite.flip_h);
        assert!(sprite.flip_v);
    }

    #[test]
    fn test_sprite_bounds() {
        let sprite = Sprite {
            x: 10,
            y: 20,
            width_tiles: 2,
            height_tiles: 3,
            pattern_base: 0,
            palette: 0,
            priority: false,
            flip_h: false,
            flip_v: false,
            link: 0,
        };

        let (x1, y1, x2, y2) = sprite.bounds();
        assert_eq!(x1, 10);
        assert_eq!(y1, 20);
        assert_eq!(x2, 26); // 10 + 2*8
        assert_eq!(y2, 44); // 20 + 3*8
    }

    #[test]
    fn test_sprite_is_visible_on_screen() {
        let sprite = Sprite {
            x: 10,
            y: 20,
            width_tiles: 1,
            height_tiles: 1,
            pattern_base: 0,
            palette: 0,
            priority: false,
            flip_h: false,
            flip_v: false,
            link: 0,
        };

        assert!(sprite.is_visible(320, 224));
    }

    #[test]
    fn test_sprite_is_visible_offscreen_left() {
        let sprite = Sprite {
            x: -16,
            y: 100,
            width_tiles: 1,
            height_tiles: 1,
            pattern_base: 0,
            palette: 0,
            priority: false,
            flip_h: false,
            flip_v: false,
            link: 0,
        };

        assert!(!sprite.is_visible(320, 224));
    }

    #[test]
    fn test_sprite_is_visible_partially_offscreen() {
        let sprite = Sprite {
            x: -4,
            y: 100,
            width_tiles: 1,
            height_tiles: 1,
            pattern_base: 0,
            palette: 0,
            priority: false,
            flip_h: false,
            flip_v: false,
            link: 0,
        };

        assert!(sprite.is_visible(320, 224));
    }

    #[test]
    fn test_sprite_cache_build_single() {
        let mut vram = vec![0u8; 0x10000];

        // Sprite at index 0, link = 0 (end)
        // Word 0: y=150
        let word0: u16 = 0x0096;
        vram[0] = (word0 >> 8) as u8;
        vram[1] = word0 as u8;

        // Word 1: link=0
        let word1: u16 = 0x0000;
        vram[2] = (word1 >> 8) as u8;
        vram[3] = word1 as u8;

        // Word 2: pattern=0
        let word2: u16 = 0x0000;
        vram[4] = (word2 >> 8) as u8;
        vram[5] = word2 as u8;

        // Word 3: x=160
        let word3: u16 = 0x00A0;
        vram[6] = (word3 >> 8) as u8;
        vram[7] = word3 as u8;

        let mut cache = SpriteCache::new();
        cache.build(&vram, 0, 80);

        assert_eq!(cache.sprites.len(), 1);
    }

    #[test]
    fn test_sprite_cache_build_chain() {
        let mut vram = vec![0u8; 0x10000];

        // Sprite 0: link to sprite 1
        // Word 0: y=150
        let word0: u16 = 0x0096;
        vram[0] = (word0 >> 8) as u8;
        vram[1] = word0 as u8;

        // Word 1: link=1 (bits 0-6)
        let word1: u16 = 0x0001;
        vram[2] = (word1 >> 8) as u8;
        vram[3] = word1 as u8;

        // Word 2: pattern=0
        vram[4] = 0x00;
        vram[5] = 0x00;

        // Word 3: x=160
        let word3: u16 = 0x00A0;
        vram[6] = (word3 >> 8) as u8;
        vram[7] = word3 as u8;

        // Sprite 1: link = 0 (end)
        // Word 0: y=200
        let word0_2: u16 = 0x00C8;
        vram[8] = (word0_2 >> 8) as u8;
        vram[9] = word0_2 as u8;

        // Word 1: link=0
        let word1_2: u16 = 0x0000;
        vram[10] = (word1_2 >> 8) as u8;
        vram[11] = word1_2 as u8;

        // Word 2: pattern=0
        vram[12] = 0x00;
        vram[13] = 0x00;

        // Word 3: x=176
        let word3_2: u16 = 0x00B0;
        vram[14] = (word3_2 >> 8) as u8;
        vram[15] = word3_2 as u8;

        let mut cache = SpriteCache::new();
        cache.build(&vram, 0, 80);

        assert_eq!(cache.sprites.len(), 2);
    }

    #[test]
    fn test_sprite_cache_max_sprites() {
        let mut vram = vec![0u8; 0x10000];

        // Create chain of 85 sprites (should stop at 80)
        for i in 0..85 {
            let addr = i * 8;

            // Word 0: y=150
            let word0: u16 = 0x0096;
            vram[addr] = (word0 >> 8) as u8;
            vram[addr + 1] = word0 as u8;

            // Word 1: link to next sprite (bits 0-6)
            let link = if i < 84 { i + 1 } else { 0 };
            let word1: u16 = link as u16;
            vram[addr + 2] = (word1 >> 8) as u8;
            vram[addr + 3] = word1 as u8;

            // Word 2: pattern=0
            vram[addr + 4] = 0x00;
            vram[addr + 5] = 0x00;

            // Word 3: x=160
            let word3: u16 = 0x00A0;
            vram[addr + 6] = (word3 >> 8) as u8;
            vram[addr + 7] = word3 as u8;
        }

        let mut cache = SpriteCache::new();
        cache.build(&vram, 0, 80);

        assert_eq!(cache.sprites.len(), 80);
    }

    #[test]
    fn test_sprite_cache_get_sprites_on_line() {
        let sprite1 = Sprite {
            x: 0,
            y: 10,
            width_tiles: 1,
            height_tiles: 2, // covers y=10..26
            pattern_base: 0,
            palette: 0,
            priority: false,
            flip_h: false,
            flip_v: false,
            link: 0,
        };

        let sprite2 = Sprite {
            x: 0,
            y: 100,
            width_tiles: 1,
            height_tiles: 1,
            pattern_base: 0,
            palette: 0,
            priority: false,
            flip_h: false,
            flip_v: false,
            link: 0,
        };

        let mut cache = SpriteCache::new();
        cache.sprites.push(sprite1);
        cache.sprites.push(sprite2);

        let line_15 = cache.get_sprites_on_line(15);
        assert_eq!(line_15.len(), 1);
        assert_eq!(line_15[0].y, 10);

        let line_100 = cache.get_sprites_on_line(100);
        assert_eq!(line_100.len(), 1);
        assert_eq!(line_100[0].y, 100);
    }

    #[test]
    fn test_sprite_cache_20_per_line_limit() {
        let mut cache = SpriteCache::new();

        // Add 30 sprites all on same line
        for i in 0..30 {
            cache.sprites.push(Sprite {
                x: i * 8,
                y: 100,
                width_tiles: 1,
                height_tiles: 1,
                pattern_base: 0,
                palette: 0,
                priority: false,
                flip_h: false,
                flip_v: false,
                link: 0,
            });
        }

        let sprites = cache.get_sprites_on_line(100);
        assert_eq!(sprites.len(), 20); // Hardware limit
    }
}
