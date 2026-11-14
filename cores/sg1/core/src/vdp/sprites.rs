//! The sprites of the VDP's sprite table: their attributes, the link chain
//! the chip walks, and the list the renderer parses once a frame.
//!
//! The table holds 80 sprites in the 40-cell mode, 64 in the 32-cell mode;
//! a line shows at most 20 or 16 of them. The renderer
//! (`Vdp::render_sprite_line`) draws them line by line from the list here.

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
}

/// The sprites the VDP draws in a frame, in the order it takes them: the
/// slot at the head of the table (slot 0) first, then the slot each one's
/// link names. The walk stops after a link of 0, before a link that names a
/// slot past the table (64 slots in the 32-cell mode, 80 in the 40-cell
/// mode, given as `slots`), and after `slots` sprites, so a chain that loops
/// back repeats its sprites up to that count, as the chip's own walk does.
/// Each item is a slot and its sprite.
///
/// The renderer's sprite list, the sprite table the debugger shows and the
/// paint order it reports all walk the chain through this, so the three
/// never disagree on where it stops.
pub fn link_chain(vram: &[u8], sat_base: u16, slots: u8) -> LinkChain<'_> {
    LinkChain {
        vram,
        sat_base,
        slots,
        next: Some(0),
        taken: 0,
    }
}

/// The walk of the sprite table's link chain [`link_chain`] returns.
pub struct LinkChain<'a> {
    vram: &'a [u8],
    sat_base: u16,
    slots: u8,
    /// The slot read next, or `None` once the walk has stopped.
    next: Option<u8>,
    /// Sprites the walk has given so far.
    taken: u8,
}

impl Iterator for LinkChain<'_> {
    type Item = (u8, Sprite);

    fn next(&mut self) -> Option<Self::Item> {
        let slot = self.next?;
        // 80 is the most sprites a frame can hold, whatever `slots` says.
        if self.taken >= self.slots || self.taken >= 80 {
            self.next = None;
            return None;
        }
        let Some(sprite) = Sprite::from_sat(self.vram, self.sat_base, slot) else {
            self.next = None;
            return None;
        };
        self.taken += 1;
        // Stop on a null link or one that points past the valid SAT range.
        // Without it a chain into the out-of-range indices renders phantom
        // sprites past the per-frame limit (Genesis Plus GX stops its walk
        // there too).
        self.next = if sprite.link == 0 || sprite.link >= self.slots {
            None
        } else {
            Some(sprite.link)
        };
        Some((slot, sprite))
    }
}

/// Sprite cache for efficient rendering.
#[derive(Clone)]
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

    /// Build sprite cache from SAT, following link chain ([`link_chain`]).
    pub fn build(&mut self, vram: &[u8], sat_base: u16, max_sprites: u8) {
        self.sprites.clear();
        self.sprites
            .extend(link_chain(vram, sat_base, max_sprites).map(|(_, sprite)| sprite));
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

    /// Point the sprite table entry at `slot` (table at VRAM 0) to `link`.
    fn link_slot(vram: &mut [u8], slot: usize, link: u8) {
        vram[slot * 8 + 3] = link;
    }

    #[test]
    fn the_link_chain_stops_before_a_slot_past_the_table() {
        let mut vram = vec![0u8; 0x10000];
        link_slot(&mut vram, 0, 5);
        link_slot(&mut vram, 5, 70);
        link_slot(&mut vram, 70, 0);

        // 64 slots in the 32-cell mode: 70 is past the table, so the walk
        // ends at slot 5, as the renderer's does.
        let slots: Vec<u8> = link_chain(&vram, 0, 64).map(|(slot, _)| slot).collect();
        assert_eq!(slots, [0, 5]);
        let mut cache = SpriteCache::new();
        cache.build(&vram, 0, 64);
        assert_eq!(cache.len(), 2);

        // 80 slots in the 40-cell mode reach it.
        let slots: Vec<u8> = link_chain(&vram, 0, 80).map(|(slot, _)| slot).collect();
        assert_eq!(slots, [0, 5, 70]);
    }

    #[test]
    fn a_chain_that_loops_back_stops_at_the_table_size() {
        let mut vram = vec![0u8; 0x10000];
        link_slot(&mut vram, 0, 1);
        link_slot(&mut vram, 1, 1); // slot 1 links to itself
        assert_eq!(link_chain(&vram, 0, 64).count(), 64);
        assert_eq!(link_chain(&vram, 0, 80).count(), 80);
    }
}
