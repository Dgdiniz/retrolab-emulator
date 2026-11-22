//! The sprites of the VDP's sprite table: their attributes, the link chain
//! the chip walks, the VDP's own copy of the table's first words, the list
//! the renderer draws from, and the drawing of each line's sprites.
//!
//! The table holds 80 sprites in the 40-cell mode, 64 in the 32-cell mode;
//! a line shows at most 20 or 16 of them. The VDP keeps a copy of the
//! first two words of each entry, its Y and its size and link: a write
//! into them at the table's current base writes the copy too, and moving
//! the base leaves the copy as it is. The chip walks the link chain and
//! finds a line's sprites in the copy, and reads an entry's other two
//! words, its pattern and attributes and its X, from video memory.
//! `Vdp::render_sprite_line` draws them line by line from the list here,
//! which is built again, from the line after next, once the table, its
//! base or the cell mode has changed. In interlace mode 2 a sprite's cells
//! are 8 pixels by 16 and its Y counts the picture's rows, so each field's
//! row of a line takes the sprites on that row.

use retro_core::capture::{PixelTrace, SpriteFate};

use super::picture::{Interlace, DOUBLE_CELL_ROWS, NORMAL_CELL_ROWS};
use super::sources::{self, LineTrace};
use super::Vdp;

/// The bytes of the VDP's copy of the sprite table: the first four bytes
/// of each of the 80 entries, in the order video memory holds them.
pub const SPRITE_HEAD_BYTES: usize = 80 * 4;

/// Where the first two words of a sprite table entry, its Y and its size
/// and link, are read from. Its other two words always come from video
/// memory.
#[derive(Debug, Clone, Copy)]
pub enum EntryHeads<'a> {
    /// The VDP's own copy ([`SPRITE_HEAD_BYTES`]), which the chip reads.
    Copy(&'a [u8; SPRITE_HEAD_BYTES]),
    /// The table itself, in the video memory the other words come from.
    Table,
}

impl EntryHeads<'_> {
    /// The first four bytes of entry `slot` of the table at `sat_base` in
    /// `vram`, or `None` past the copy's 80 entries or the end of `vram`.
    fn of(self, vram: &[u8], sat_base: u16, slot: u8) -> Option<[u8; 4]> {
        let bytes = match self {
            EntryHeads::Copy(copy) => {
                let at = usize::from(slot) * 4;
                copy.get(at..at + 4)?
            }
            EntryHeads::Table => {
                let at = usize::from(sat_base) + usize::from(slot) * 8;
                vram.get(at..at + 4)?
            }
        };
        Some([bytes[0], bytes[1], bytes[2], bytes[3]])
    }
}

/// Sprite data structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sprite {
    /// X position (screen coordinates)
    pub x: i16,

    /// Y position: the screen line its top row is on, the table's 9 bits
    /// less 128. In the renderer's list of a frame in interlace mode 2, the
    /// picture row its top row is on, the table's 10 bits less 256
    /// ([`SpriteCache::build`]).
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
    /// Parse sprite from SAT (Sprite Attribute Table), every word of the
    /// entry from `vram` ([`Self::read`] from the table).
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
        Self::read(EntryHeads::Table, vram, sat_base, index)
    }

    /// The sprite of slot `slot` of the table at `sat_base`: its first two
    /// words from `heads`, its other two from `vram` ([`Self::from_sat`]
    /// gives the layout). `None` when the entry runs past the end of video
    /// memory, or past the copy's 80 entries.
    pub fn read(heads: EntryHeads<'_>, vram: &[u8], sat_base: u16, slot: u8) -> Option<Self> {
        let addr = usize::from(sat_base) + usize::from(slot) * 8;

        if addr + 8 > vram.len() {
            return None;
        }

        // The 4 words (big-endian), the first two from `heads`.
        let head = heads.of(vram, sat_base, slot)?;
        let word0 = u16::from_be_bytes([head[0], head[1]]);
        let word1 = u16::from_be_bytes([head[2], head[3]]);
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

    /// The first row it covers and the row past its last, from its `y`,
    /// its cells `cell_rows` rows tall: 8, or 16 in interlace mode 2.
    pub fn rows(&self, cell_rows: u16) -> (i16, i16) {
        let height = self.height_tiles as i16 * cell_rows as i16;
        (self.y, self.y + height)
    }

    /// Its Y as interlace mode 2 reads it, where it is the sprite of slot
    /// `slot` of the table at `sat_base`, its first word from `heads`: the
    /// word's low 10 bits, which count the picture's rows from 256. Outside
    /// that mode the Y has 9 bits and counts lines from 128
    /// ([`Self::read`]).
    pub fn double_resolution_y(heads: EntryHeads<'_>, vram: &[u8], sat_base: u16, slot: u8) -> i16 {
        let head = heads.of(vram, sat_base, slot).unwrap_or_default();
        let word = u16::from_be_bytes([head[0], head[1]]);
        (word & 0x03FF) as i16 - 256
    }
}

/// The sprites the VDP draws in a frame, in the order it takes them: the
/// slot at the head of the table (slot 0) first, then the slot each one's
/// link names. The walk stops after a link of 0, before a link that names a
/// slot past the table (64 slots in the 32-cell mode, 80 in the 40-cell
/// mode, given as `slots`), and after `slots` sprites, so a chain that loops
/// back repeats its sprites up to that count, as the chip's own walk does.
/// Each item is a slot and its sprite, its first two words read from
/// `heads` and its other two from `vram`.
///
/// The renderer's sprite list, the sprite table the debugger shows and the
/// paint order it reports all walk the chain through this, from the VDP's
/// copy of the table's first words, so the three never disagree on where
/// it stops.
pub fn link_chain<'a>(
    heads: EntryHeads<'a>,
    vram: &'a [u8],
    sat_base: u16,
    slots: u8,
) -> LinkChain<'a> {
    LinkChain {
        heads,
        vram,
        sat_base,
        slots,
        next: Some(0),
        taken: 0,
    }
}

/// The walk of the sprite table's link chain [`link_chain`] returns.
pub struct LinkChain<'a> {
    heads: EntryHeads<'a>,
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
        let Some(sprite) = Sprite::read(self.heads, self.vram, self.sat_base, slot) else {
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

    /// Build sprite cache from SAT, following link chain ([`link_chain`],
    /// each entry's first two words from `heads`), each sprite's `y` as a
    /// frame drawn in `interlace` places it: a picture row in interlace
    /// mode 2 ([`Sprite::double_resolution_y`]), a line otherwise.
    pub fn build(
        &mut self,
        heads: EntryHeads<'_>,
        vram: &[u8],
        sat_base: u16,
        max_sprites: u8,
        interlace: Interlace,
    ) {
        self.sprites.clear();
        let chain = link_chain(heads, vram, sat_base, max_sprites);
        if interlace == Interlace::Double {
            self.sprites.extend(chain.map(|(slot, sprite)| Sprite {
                y: Sprite::double_resolution_y(heads, vram, sat_base, slot),
                ..sprite
            }));
        } else {
            self.sprites.extend(chain.map(|(_, sprite)| sprite));
        }
    }

    /// The list made `sprites`, in their order, as a savestate holds it.
    pub fn replace(&mut self, sprites: impl IntoIterator<Item = Sprite>) {
        self.sprites.clear();
        self.sprites.extend(sprites);
    }

    /// The slot in the sprite table of the list's sprite at `index`. The
    /// list is the link chain in the order the walk took it, which starts
    /// at slot 0 and goes on to the slot each sprite's link names, so the
    /// slot is read back from the sprite before it, and the list keeps no
    /// slots of its own.
    pub fn slot_at(&self, index: usize) -> u8 {
        match index {
            0 => 0,
            _ => self.sprites[index - 1].link,
        }
    }
}

impl Default for SpriteCache {
    fn default() -> Self {
        Self::new()
    }
}

impl Vdp {
    /// The VDP's copy of the sprite table's first words, as the readers of
    /// the table take it.
    pub(super) fn sprite_heads(&self) -> EntryHeads<'_> {
        EntryHeads::Copy(&self.sprite_heads)
    }

    /// A word was stored at VRAM bytes `at` and `at + 1`, `at` even: where
    /// it lands in the sprite table at its current base, the copy of the
    /// entries' first words takes its bytes there, and the sprite list is
    /// stale. Every store of a word into VRAM calls this after it.
    #[inline(always)]
    pub(super) fn note_sprite_table_word(&mut self, at: usize) {
        let offset = at.wrapping_sub(usize::from(self.sat_base()));
        if offset < usize::from(self.sprite_table_slots()) * 8 {
            self.store_sprite_table_bytes(at, offset, 2);
        }
    }

    /// [`Self::note_sprite_table_word`] for one byte, stored at VRAM byte
    /// `at`, as a fill, a copy or the debugger stores them.
    #[inline(always)]
    pub(super) fn note_sprite_table_byte(&mut self, at: usize) {
        let offset = at.wrapping_sub(usize::from(self.sat_base()));
        if offset < usize::from(self.sprite_table_slots()) * 8 {
            self.store_sprite_table_bytes(at, offset, 1);
        }
    }

    /// The `count` bytes stored from VRAM byte `at`, `offset` bytes into the
    /// sprite table: the copy takes those of them that are an entry's first
    /// four bytes, and the sprite list is stale.
    #[cold]
    #[inline(never)]
    fn store_sprite_table_bytes(&mut self, at: usize, offset: usize, count: usize) {
        for i in 0..count {
            let byte = offset + i;
            if byte & 4 == 0 {
                self.sprite_heads[(byte >> 3) * 4 + (byte & 3)] = self.vram[at + i];
            }
        }
        self.sprite_list_stale = true;
    }

    /// The copy taken from the table in video memory at its current base,
    /// where a load and a reset find them alike: whatever was written into
    /// the table was written into the copy, unless the base moved since.
    pub(super) fn take_sprite_heads_from_table(&mut self) {
        let base = usize::from(self.sat_base());
        for slot in 0..SPRITE_HEAD_BYTES / 4 {
            let at = base + slot * 8;
            if let Some(bytes) = self.vram.get(at..at + 4) {
                self.sprite_heads[slot * 4..slot * 4 + 4].copy_from_slice(bytes);
            }
        }
    }

    /// Line `line`, drawn with the display on, has its sprites: a stale
    /// list is built again now, from the copy and video memory as they are
    /// as the line starts, and the lines from the next one on draw from it.
    /// Genesis Plus GX parses a line's sprites at the start of the line
    /// before; so a change made in the table while a line runs shows from
    /// the line after next.
    pub(super) fn refresh_stale_sprite_list(&mut self, line: u16) {
        if self.sprite_list_stale && self.sprite_cache_valid {
            self.rebuild_sprite_cache();
            if self.recording.is_some() {
                self.record_sprite_list(line + 1);
            }
        }
    }
}

impl Vdp {
    /// Render all sprite pixels for a given scanline into the sprite line buffer.
    /// Called ONCE per scanline. The buffer is then read during compositing.
    ///
    /// Implements sprite masking per the Mega Drive VDP:
    /// - When a sprite with raw X position = 0 is encountered AND at least one
    ///   sprite with X > 0 was already seen, all remaining sprites on this
    ///   scanline are masked (not drawn).
    /// - Per-scanline pixel count limit (320 in H40, 256 in H32).
    /// - The masking flag carries over to the next scanline when the pixel
    ///   limit is reached.
    ///
    /// With a trace (`T::ON`) it also notes each sprite the evaluation met
    /// and, where the line's sprite limit or its dots stop the evaluation,
    /// the sprites on the line after, read without changing anything; and
    /// the sprite each pixel of the line buffer came from.
    ///
    /// In interlace mode 2 the line is the picture row of the field being
    /// drawn ([`Vdp::picture_row`]), the limits and the mask count rows,
    /// and the cells are 16 rows tall.
    pub(super) fn render_sprite_line<T: PixelTrace<Notes = LineTrace>>(
        &mut self,
        line: u16,
        trace: &mut T,
    ) {
        if self.interlace == Interlace::Double {
            self.render_sprite_row::<T, DOUBLE_CELL_ROWS>(line, trace);
        } else {
            self.render_sprite_row::<T, NORMAL_CELL_ROWS>(line, trace);
        }
    }

    /// The body of [`Self::render_sprite_line`] for cells of `CELL_ROWS`
    /// rows: 8, or 16 in interlace mode 2, where a cell's number is the
    /// index's low 10 bits and a row is the picture's. Built for each, so a
    /// line of either draws with no test of the mode.
    pub(super) fn render_sprite_row<T: PixelTrace<Notes = LineTrace>, const CELL_ROWS: u16>(
        &mut self,
        line: u16,
        trace: &mut T,
    ) {
        let double = CELL_ROWS == DOUBLE_CELL_ROWS;
        let width = self.display_width() as usize;
        let is_h40 = (self.registers[12] & 0x01) != 0;
        let max_sprite_pixels: i32 = if is_h40 { 320 } else { 256 };
        // Per-line sprite count limit: 20 in H40, 16 in H32 (viewport.w >> 4).
        let max_sprites_per_line: u16 = if is_h40 { 20 } else { 16 };

        // Ensure sprite line buffers are large enough
        if self.sprite_line_color.len() < width {
            self.sprite_line_color.resize(width, 0);
            self.sprite_line_palette.resize(width, 0);
            self.sprite_line_priority.resize(width, false);
        }

        // Clear sprite line buffers
        for i in 0..width {
            self.sprite_line_color[i] = 0;
            self.sprite_line_palette[i] = 0;
            self.sprite_line_priority[i] = false;
        }

        // Clear collision tracking
        for i in 0..width {
            self.sprite_pixel_buffer[i] = false;
        }

        if !self.sprite_cache_valid {
            self.rebuild_sprite_cache();
            if self.recording.is_some() {
                self.record_sprite_list(line);
            }
        }

        // Build list of sprites on this line with masking (reuse scratch buffer)
        //
        // Sprites are taken in link order (front to back). A sprite at raw
        // X = 0 (screen X = -128) hides every sprite after it on the line,
        // but only once the mask is armed: by an earlier sprite of this line
        // at any other X, or by the line above having run out of sprite dots.
        // (Both cases are documented from hardware tests; checked against
        // Genesis Plus GX.)
        self.sprites_on_line_buf.clear();
        // The row the sprites are taken for: the line, or in interlace mode
        // 2 the picture row of the field being drawn.
        let scanline = if double {
            self.picture_row(line) as i16
        } else {
            line as i16
        };
        let mut count = 0u16;
        let mut masked = false;
        let mut mask_armed = self.sprite_mask_overflow;
        let mut pixel_count: i32 = 0;
        // When the per-line dot budget is crossed mid-sprite, that one sprite
        // is drawn clipped to the remaining dots. -1 = no sprite clipped.
        let mut clip_sprite_idx: i32 = -1;
        let mut clip_x_end: i16 = 0;
        // For a trace: the first sprite of the list not taken when the
        // evaluation stopped, and why it stopped.
        let mut stopped_at: Option<(usize, SpriteFate)> = None;
        if T::ON {
            trace.notes().begin_sprites(self.sprite_mask_overflow);
        }

        for (idx, sprite) in self.sprite_cache.sprites().iter().enumerate() {
            let (y1, y2) = sprite.rows(CELL_ROWS);
            if scanline >= y1 && scanline < y2 {
                count += 1;
                if count > max_sprites_per_line {
                    self.sprite_overflow = true;
                    if T::ON {
                        trace.notes().over_sprite_limit();
                        stopped_at = Some((idx, SpriteFate::OverLineLimit));
                    }
                    break;
                }

                // Raw SAT X position: sprite.x is screen-adjusted (raw - 128)
                let raw_x = sprite.x + 128;
                if raw_x != 0 {
                    mask_armed = true;
                } else if mask_armed {
                    masked = true;
                }

                let sprite_width_px = sprite.width_tiles as i32 * 8;
                let pixel_count_before = pixel_count;
                pixel_count += sprite_width_px;

                if !masked {
                    self.sprites_on_line_buf.push(idx as u16);
                }
                if T::ON {
                    let slot = self.sprite_cache.slot_at(idx);
                    trace.notes().met_sprite(slot, idx as u8, sprite, masked);
                }

                if pixel_count >= max_sprite_pixels {
                    // Dot limit crossed mid-sprite: that sprite is drawn clipped
                    // to the remaining budget. The chip draws whole 8px tiles
                    // measured from the sprite's left edge (off-screen dots still
                    // count toward the budget, so clip in raw sprite space;
                    // checked against Genesis Plus GX).
                    if !masked {
                        let allowed_tiles = (max_sprite_pixels - pixel_count_before).max(0) >> 3;
                        clip_sprite_idx = idx as i32;
                        clip_x_end = sprite.x + allowed_tiles as i16 * 8;
                    }
                    self.sprite_mask_overflow = pixel_count >= width as i32;
                    if T::ON {
                        trace.notes().dots_ran_out((!masked).then_some(clip_x_end));
                        stopped_at = Some((idx + 1, SpriteFate::LeftOut));
                    }
                    // The limit was hit, so the flag is not cleared below.
                    break;
                }
            }
        }

        // Clear masking for next line if pixel limit was NOT reached
        if pixel_count < max_sprite_pixels {
            self.sprite_mask_overflow = false;
        }

        // For a trace, the sprites on the line after the evaluation
        // stopped: read, never taken.
        if T::ON {
            if let Some((first, fate)) = stopped_at {
                for (idx, sprite) in self.sprite_cache.sprites().iter().enumerate().skip(first) {
                    let (y1, y2) = sprite.rows(CELL_ROWS);
                    if scanline >= y1 && scanline < y2 {
                        let slot = self.sprite_cache.slot_at(idx);
                        trace.notes().left_out(slot, idx as u8, sprite, fate);
                    }
                }
            }
        }

        // Render sprites in REVERSE order (lowest priority first) so that
        // higher-priority sprites (earlier in list) overwrite lower ones.
        let num_sprites = self.sprites_on_line_buf.len();
        for si in (0..num_sprites).rev() {
            let sprite_idx = self.sprites_on_line_buf[si] as usize;
            let sprite = self.sprite_cache.sprites()[sprite_idx];
            let slot = if T::ON {
                self.sprite_cache.slot_at(sprite_idx)
            } else {
                0
            };
            let (sx1, _, sx2, _) = sprite.bounds();

            let local_y = (scanline - sprite.y) as u16;
            let flipped_y = if sprite.flip_v {
                sprite.height_tiles as u16 * CELL_ROWS - 1 - local_y
            } else {
                local_y
            };

            let sprite_width_px = sprite.width_tiles as i16 * 8;

            // Clip to screen (ensure bounds are valid)
            let x_start = sx1.max(0) as usize;
            let mut x_end_raw = sx2;
            if sprite_idx as i32 == clip_sprite_idx {
                x_end_raw = x_end_raw.min(clip_x_end);
            }
            let x_end = x_end_raw.clamp(0, width as i16) as usize;

            for screen_x in x_start..x_end {
                let local_x = (screen_x as i16 - sx1) as u16;
                let flipped_x = if sprite.flip_h {
                    sprite_width_px as u16 - 1 - local_x
                } else {
                    local_x
                };

                let tile_x = flipped_x / 8;
                let tile_y = flipped_y / CELL_ROWS;
                let pixel_x = (flipped_x % 8) as u8;
                let pixel_y = (flipped_y % CELL_ROWS) as u8;

                // Column-major tile layout; a cell of 16 rows is numbered by
                // the index's low 10 bits.
                let tile_offset = tile_x * sprite.height_tiles as u16 + tile_y;
                let pattern_index = if double {
                    ((sprite.pattern_base & 0x03FF) + tile_offset) & 0x03FF
                } else {
                    sprite.pattern_base + tile_offset
                };

                let color_index = Self::read_cell_pixel::<CELL_ROWS>(
                    &self.vram,
                    pattern_index,
                    pixel_x,
                    pixel_y,
                    false,
                    false,
                );

                if color_index == 0 {
                    continue; // Transparent
                }

                // Track collision: if a sprite pixel already exists at this x
                if self.sprite_line_color[screen_x] != 0 {
                    self.sprite_collision = true;
                }

                // Write to line buffer (earlier sprites overwrite later ones)
                self.sprite_line_color[screen_x] = color_index;
                self.sprite_line_palette[screen_x] = sprite.palette;
                self.sprite_line_priority[screen_x] = sprite.priority;
                if T::ON {
                    trace.notes().sprite_pixel(
                        screen_x,
                        slot,
                        sources::cell_tile::<CELL_ROWS>(pattern_index, pixel_y, pixel_x),
                        sprite.flip_h,
                        sprite.flip_v,
                    );
                }
            }
        }
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
        cache.build(EntryHeads::Table, &vram, 0, 80, Interlace::Off);

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
        cache.build(EntryHeads::Table, &vram, 0, 80, Interlace::Off);

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
        cache.build(EntryHeads::Table, &vram, 0, 80, Interlace::Off);

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
        let slots: Vec<u8> = link_chain(EntryHeads::Table, &vram, 0, 64)
            .map(|(slot, _)| slot)
            .collect();
        assert_eq!(slots, [0, 5]);
        let mut cache = SpriteCache::new();
        cache.build(EntryHeads::Table, &vram, 0, 64, Interlace::Off);
        assert_eq!(cache.len(), 2);

        // 80 slots in the 40-cell mode reach it.
        let slots: Vec<u8> = link_chain(EntryHeads::Table, &vram, 0, 80)
            .map(|(slot, _)| slot)
            .collect();
        assert_eq!(slots, [0, 5, 70]);
    }

    #[test]
    fn the_list_gives_back_the_slot_of_each_of_its_sprites() {
        let mut vram = vec![0u8; 0x10000];
        link_slot(&mut vram, 0, 7);
        link_slot(&mut vram, 7, 3);
        link_slot(&mut vram, 3, 79);
        link_slot(&mut vram, 79, 0);
        let mut cache = SpriteCache::new();
        cache.build(EntryHeads::Table, &vram, 0, 80, Interlace::Off);
        let slots: Vec<u8> = link_chain(EntryHeads::Table, &vram, 0, 80)
            .map(|(slot, _)| slot)
            .collect();
        assert_eq!(slots, [0, 7, 3, 79]);
        let from_list: Vec<u8> = (0..cache.len()).map(|i| cache.slot_at(i)).collect();
        assert_eq!(from_list, slots);
    }

    #[test]
    fn a_chain_that_loops_back_stops_at_the_table_size() {
        let mut vram = vec![0u8; 0x10000];
        link_slot(&mut vram, 0, 1);
        link_slot(&mut vram, 1, 1); // slot 1 links to itself
        assert_eq!(link_chain(EntryHeads::Table, &vram, 0, 64).count(), 64);
        assert_eq!(link_chain(EntryHeads::Table, &vram, 0, 80).count(), 80);
    }
}
