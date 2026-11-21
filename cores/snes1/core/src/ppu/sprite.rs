//! Sprites (OBJ) for the per-scanline renderer: the pure evaluation
//! helpers, and the sprite row the renderer paints with them.
//!
//! Like [`crate::ppu::tile`], every free function is `&self`-free, they
//! take the raw OAM buffer plus the OBSEL-derived addressing
//! parameters by value, so the renderer can mix them with mutable PPU
//! state and the unit tests can drive them with synthetic OAM.
//!
//! Written from fullsnes's OAM and OBSEL descriptions (the size table,
//! the 16x16 character grid, the name-select page) and Anomie's notes
//! on sprite evaluation; the evaluation and tile arithmetic were
//! checked against bsnes-plus.
//!
//! ## Coordinate conventions
//!
//! - X is kept in the wrapped 9-bit space (`0..=511`); a sprite with
//!   `x > 256` sits off the right edge unless its body wraps past 512
//!   back onto the left. The renderer only emits the columns that
//!   land at `sx < 256`; nothing sign-extends in the per-scanline
//!   path, X wraps at 512 and the columns are gated at 256.
//! - Y is the raw OAM byte. The "on this scanline" test wraps at 256
//!   so a sprite near the bottom of OAM space spills onto the top
//!   rows of the screen.
//!
//! ## The sprite row
//!
//! `Ppu::render_obj_row` paints a line's sprites into the 256-wide OBJ
//! row the compositor reads, keeping what the line's budget keeps
//! ([`line_budget`]: 32 sprites, 34 tile columns), with the decoded
//! tiles kept by `Ppu::cached_sprite_tile` until VRAM changes.
//! `Ppu::eval_obj_overflow` sets the STAT77 range and time flags on
//! every active line, whether sprites are shown or not.

use retro_core::capture::{tile_value, PixelTrace};

use super::render::{ObjRowBuffer, ObjRowPixel, NORMAL_SCANLINE_WIDTH};
use super::tile::decode_tile_4bpp_row;
use super::trace::{colour_value, FrameTrace, ObjOrigin};
use super::Ppu;

// The sprite row below calls this module's helpers by their path,
// `sprite::parse_sprite`.
use super::sprite;

/// One parsed OAM sprite entry.
///
/// Built by [`parse_sprite`] from the 4 low-table bytes plus the two
/// high-table bits. `width`/`height` are already resolved through the
/// OBSEL size selector + the per-sprite `large` bit so the evaluator
/// never re-reads the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteAttrs {
    /// 9-bit X position in the wrapped `0..=511` space (high-table
    /// bit forms bit 8).
    pub x: u16,
    /// Raw OAM Y byte, the top scanline the sprite occupies.
    pub y: u8,
    /// 8-bit character (tile) number from OAM byte 2.
    pub character: u8,
    /// 3-bit palette select (`0..=7`).
    pub palette: u8,
    /// 2-bit sprite priority (`0..=3`).
    pub priority: u8,
    /// Horizontal flip, applied per pixel at composite time.
    pub hflip: bool,
    /// Vertical flip, baked into the row selector by
    /// [`sprite_row_in_sprite`].
    pub vflip: bool,
    /// Name-select bit, picks the second sprite tile page.
    pub nameselect: bool,
    /// `false` = small size, `true` = large size.
    pub large: bool,
    /// Resolved width in pixels (`8..=64`).
    pub width: u8,
    /// Resolved height in pixels (`8..=64`).
    pub height: u8,
}

/// Resolve `(width, height)` in pixels for a sprite.
///
/// `base_size` is the OBSEL bits 5-7 selector (`0..=7`), `large` is
/// the per-sprite high-table size bit, and `obj_interlace` is the
/// `$2133` bit 1 flag. The table is OBSEL's, as fullsnes prints it;
/// the only interlace wrinkle is that the *small* height for
/// selectors 6/7 collapses to 16 (the large 32x64 / 32x32 height is
/// untouched).
#[must_use]
pub fn sprite_size(base_size: u8, large: bool, obj_interlace: bool) -> (u8, u8) {
    let (w, h) = match (base_size & 7, large) {
        (0, false) => (8, 8),
        (0, true) => (16, 16),
        (1, false) => (8, 8),
        (1, true) => (32, 32),
        (2, false) => (8, 8),
        (2, true) => (64, 64),
        (3, false) => (16, 16),
        (3, true) => (32, 32),
        (4, false) => (16, 16),
        (4, true) => (64, 64),
        (5, false) => (32, 32),
        (5, true) => (64, 64),
        (6, false) => (16, 32),
        (6, true) => (32, 64),
        (7, false) => (16, 32),
        (7, true) => (32, 32),
        _ => unreachable!("base_size masked to 0..=7"),
    };
    let h = if obj_interlace && !large && (base_size & 7) >= 6 {
        16
    } else {
        h
    };
    (w, h)
}

/// Parse sprite `idx` (`0..=127`) out of the 544-byte OAM buffer.
///
/// The low table holds 4 bytes per sprite (X-low, Y, character,
/// attribute); the high table packs 2 bits per sprite, 4 sprites per
/// byte, holding X bit 8 + the `large` size bit.
#[must_use]
pub fn parse_sprite(
    oam: &[u8; 0x220],
    idx: usize,
    base_size: u8,
    obj_interlace: bool,
) -> SpriteAttrs {
    let lo = idx * 4;
    let x_lo = u16::from(oam[lo]);
    let y = oam[lo + 1];
    let character = oam[lo + 2];
    let attr = oam[lo + 3];
    let vflip = attr & 0x80 != 0;
    let hflip = attr & 0x40 != 0;
    let priority = (attr >> 4) & 0x03;
    let palette = (attr >> 1) & 0x07;
    let nameselect = attr & 0x01 != 0;

    let hi_byte = oam[0x200 + (idx >> 2)];
    let bits = (hi_byte >> ((idx & 3) * 2)) & 0x03;
    let x = ((u16::from(bits) & 0x01) << 8) | x_lo;
    let large = bits & 0x02 != 0;

    let (width, height) = sprite_size(base_size, large, obj_interlace);
    SpriteAttrs {
        x,
        y,
        character,
        palette,
        priority,
        hflip,
        vflip,
        nameselect,
        large,
        width,
        height,
    }
}

/// `true` when sprite `s` is visible on `line`.
///
/// A sprite whose body lies entirely past the right edge (it starts
/// past column 256 and ends before 512, so no part wraps round to the
/// left) is not on any line. Otherwise it is on the line when the
/// line's distance from the sprite's top, counted with a wrap at 256
/// (so a sprite parked near the bottom of OAM space spills onto the
/// top rows), is within its height, halved under interlace, where a
/// field shows every other row.
#[must_use]
pub fn sprite_on_scanline(s: &SpriteAttrs, line: u16, obj_interlace: bool) -> bool {
    let right_edge = s.x + u16::from(s.width) - 1;
    if s.x > 256 && right_edge < 512 {
        return false;
    }
    let rows = u16::from(if obj_interlace {
        s.height >> 1
    } else {
        s.height
    });
    let from_top = line.wrapping_sub(u16::from(s.y)) & 0xff;
    from_top < rows
}

/// Row within the sprite (`0..height-1`) for `line`, with vertical
/// flip and OBJ-interlace already applied.
///
/// The row is the line's distance from the sprite's top, wrapping at
/// 256. Under interlace a field holds every other row, so the distance
/// is doubled and the field bit added. A vertical flip mirrors the
/// sprite block by block: each block of `width` rows is reflected on
/// itself, which for a square sprite is the whole sprite and for a
/// 16x32 one is each of its two 16-row halves. The caller takes
/// `result >> 3` for the tile-row advance and `result & 7` for the row
/// inside the 8x8 tile.
#[must_use]
pub fn sprite_row_in_sprite(s: &SpriteAttrs, line: u16, field: bool, obj_interlace: bool) -> u16 {
    let mut row = line.wrapping_sub(u16::from(s.y)) & 0xff;
    if obj_interlace {
        row <<= 1;
    }
    if s.vflip {
        let block = u16::from(s.width);
        row = (row / block) * block + (block - 1 - row % block);
    }
    if obj_interlace {
        row = if s.vflip {
            row - u16::from(field)
        } else {
            row + u16::from(field)
        };
    }
    row & 0xff
}

/// 11-bit VRAM tile number for tile-column `tx` of a sprite.
///
/// Sprite tiles live in a grid of 16 x 16 characters: the character
/// byte names the top-left one (its low nibble the column, its high
/// nibble the row), and the sprite's other tiles are its neighbours,
/// wrapping within the grid on both axes. The grid starts at OBSEL's
/// tile-data base; a sprite with the name-select bit uses a second
/// grid 256 tiles on, plus OBSEL's name-select gap. A horizontal flip
/// reads the tile columns right to left. `y_row` is the
/// [`sprite_row_in_sprite`] result; its `>> 3` is the tile row. The
/// returned number maps 1:1 onto VRAM 4bpp tile slots (`tile << 5`
/// bytes), which is exactly the [`crate::ppu::SpriteTileCache`] key.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn sprite_tile_number(
    obj_tiledata_addr: u32,
    obj_name_select: u8,
    character: u8,
    nameselect: bool,
    hflip: bool,
    tile_width: u8,
    tx: u8,
    y_row: u16,
) -> u16 {
    let mut grid = obj_tiledata_addr >> 5;
    if nameselect {
        grid += 256 + (u32::from(obj_name_select) << 8);
    }
    let tiles_wide = tile_width >> 3;
    let column = if hflip { tiles_wide - 1 - tx } else { tx };
    let grid_column = (u32::from(character & 15) + u32::from(column)) & 15;
    let grid_row = (u32::from(character >> 4) + u32::from(y_row >> 3)) & 15;
    ((grid + grid_row * 16 + grid_column) & 0x07ff) as u16
}

/// The most sprites one line keeps: the evaluation's range limit. The
/// sprite after them sets the range-over flag (STAT77 bit 6).
pub const SPRITES_PER_LINE: usize = 32;

/// The most 8-pixel sprite tile columns one line draws: its time limit.
/// The column after them sets the time-over flag (STAT77 bit 7).
pub const TILE_COLUMNS_PER_LINE: usize = 34;

/// Whether the tile column of a sprite at `x` that starts at `column_x`
/// (both in the wrapped 9-bit space) is skipped as off screen: it lies
/// whole in the band past the right edge, `256..512`. The evaluation
/// fetches every column of a sprite placed exactly at 256, although none
/// of them shows.
#[inline]
#[must_use]
pub fn column_off_screen(x: u16, column_x: u16) -> bool {
    x != 256 && column_x >= 256 && column_x + 7 < 512
}

/// How many of the eight pixels of a tile column starting at `column_x`
/// fall on the screen's 256 columns, the 9-bit position wrapping at 512.
#[must_use]
pub fn column_visible_pixels(column_x: u16) -> u8 {
    (0..8u16)
        .filter(|&px| (column_x + px) & 0x1FF < 256)
        .count() as u8
}

/// What the sprite evaluation of one line keeps and drops: the one rule
/// the STAT77 flags, the line's sprite list and the painter share.
///
/// The evaluation walks the 128 sprites from the first sprite (the
/// priority rotation) and keeps the first [`SPRITES_PER_LINE`] that fall
/// on the line, in the order it met them; the next one sets
/// `over_range`. It then fetches the tile columns of the kept sprites
/// backwards, from the last kept to the first, each sprite's columns
/// left to right in tile order, skipping the off-screen ones
/// ([`column_off_screen`]); past [`TILE_COLUMNS_PER_LINE`] columns it
/// drops the rest and sets `over_time`. So the columns dropped belong to
/// the sprites met first, the ones with the highest priority, which is
/// the hardware's "too many sprites vanish" order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineBudget {
    /// The sprites kept, by their slot in the table, in the order the
    /// evaluation met them; the first `kept_count` entries are used.
    pub kept: [u8; SPRITES_PER_LINE],
    /// How many sprites the line keeps.
    pub kept_count: u8,
    /// For each kept sprite: its tile columns the line draws.
    pub columns_drawn: [u8; SPRITES_PER_LINE],
    /// For each kept sprite: its tile columns the time limit dropped.
    pub columns_dropped: [u8; SPRITES_PER_LINE],
    /// For each kept sprite: its tile columns skipped as off screen.
    pub columns_off_screen: [u8; SPRITES_PER_LINE],
    /// More sprites than the line keeps fell on it (STAT77 bit 6).
    pub over_range: bool,
    /// More tile columns than the line draws were wanted (STAT77 bit 7).
    pub over_time: bool,
}

impl LineBudget {
    /// The sprites kept, by slot, in the order the evaluation met them.
    #[must_use]
    pub fn kept(&self) -> &[u8] {
        &self.kept[..usize::from(self.kept_count)]
    }
}

/// Evaluate the sprites of one line: see [`LineBudget`]. `line` is the
/// line counted from 0, as [`sprite_on_scanline`] takes it.
///
/// With `over_range` set, the walk goes on past the kept sprites and
/// pushes, in the order it meets them, every further sprite that falls on
/// the line, the ones the range limit dropped whole; it changes nothing
/// else. Without it, the walk stops at the first of them, as the flags
/// need no more.
#[must_use]
pub fn line_budget(
    oam: &[u8; 0x220],
    first_sprite: u8,
    base_size: u8,
    obj_interlace: bool,
    line: u16,
    over_range: Option<&mut Vec<u8>>,
) -> LineBudget {
    evaluate_line::<true>(
        oam,
        first_sprite,
        base_size,
        obj_interlace,
        line,
        over_range,
    )
}

/// The two STAT77 flags of one line, `(over_range, over_time)`, by the
/// rule of [`line_budget`]: the same evaluation, stopping as soon as both
/// flags are known and keeping no account of the sprites, as the frame
/// loops need it on every line.
#[inline(always)]
#[must_use]
pub fn line_flags(
    oam: &[u8; 0x220],
    first_sprite: u8,
    base_size: u8,
    obj_interlace: bool,
    line: u16,
) -> (bool, bool) {
    let budget = evaluate_line::<false>(oam, first_sprite, base_size, obj_interlace, line, None);
    (budget.over_range, budget.over_time)
}

/// The evaluation of [`line_budget`]; with `ACCOUNT` false it keeps no
/// per-sprite counts and stops the fetch at the first dropped column,
/// which is all [`line_flags`] needs.
#[inline(always)]
fn evaluate_line<const ACCOUNT: bool>(
    oam: &[u8; 0x220],
    first_sprite: u8,
    base_size: u8,
    obj_interlace: bool,
    line: u16,
    mut over_range: Option<&mut Vec<u8>>,
) -> LineBudget {
    let mut budget = LineBudget {
        kept: [0xFF; SPRITES_PER_LINE],
        kept_count: 0,
        columns_drawn: [0; SPRITES_PER_LINE],
        columns_dropped: [0; SPRITES_PER_LINE],
        columns_off_screen: [0; SPRITES_PER_LINE],
        over_range: false,
        over_time: false,
    };

    // The range evaluation.
    for i in 0..128usize {
        let slot = (usize::from(first_sprite) + i) & 0x7F;
        let s = parse_sprite(oam, slot, base_size, obj_interlace);
        if !sprite_on_scanline(&s, line, obj_interlace) {
            continue;
        }
        let count = usize::from(budget.kept_count);
        if count >= SPRITES_PER_LINE {
            budget.over_range = true;
            match over_range.as_deref_mut() {
                Some(dropped) => dropped.push(slot as u8),
                None => break,
            }
            continue;
        }
        budget.kept[count] = slot as u8;
        budget.kept_count += 1;
    }

    // The tile fetch, backwards over the kept sprites.
    let mut columns = 0usize;
    'sprites: for k in (0..usize::from(budget.kept_count)).rev() {
        let s = parse_sprite(oam, usize::from(budget.kept[k]), base_size, obj_interlace);
        for tx in 0..u16::from(s.width >> 3) {
            let column_x = (s.x + (tx << 3)) & 0x1FF;
            if column_off_screen(s.x, column_x) {
                if ACCOUNT {
                    budget.columns_off_screen[k] += 1;
                }
            } else if columns >= TILE_COLUMNS_PER_LINE {
                budget.over_time = true;
                if !ACCOUNT {
                    break 'sprites;
                }
                budget.columns_dropped[k] += 1;
            } else {
                columns += 1;
                if ACCOUNT {
                    budget.columns_drawn[k] += 1;
                }
            }
        }
    }
    budget
}

/// How many pixels of sprite `s` the line shows on the screen's 256
/// columns when it draws the first `columns_drawn` of its fetched tile
/// columns (the off-screen ones are never fetched), in tile order.
#[must_use]
pub fn drawn_pixels(s: &SpriteAttrs, columns_drawn: u8) -> u8 {
    let mut left = columns_drawn;
    let mut pixels = 0u8;
    for tx in 0..u16::from(s.width >> 3) {
        if left == 0 {
            break;
        }
        let column_x = (s.x + (tx << 3)) & 0x1FF;
        if column_off_screen(s.x, column_x) {
            continue;
        }
        left -= 1;
        pixels += column_visible_pixels(column_x);
    }
    pixels
}

impl Ppu {
    /// Evaluate OAM for one visible `scanline` and update the STAT77
    /// ($213E) sprite range-over (bit 6) / time-over (bit 7) flags
    /// **without painting**.
    ///
    /// On hardware the sprite range/tile evaluation walks OAM on every
    /// active scanline regardless of whether OBJ is enabled on either
    /// screen ($212C/$212D bit 4) or whether the screen is in forced
    /// blank, only the *compositing* is gated (checked against
    /// snes9x, which evaluates them whatever the display state).
    /// Keeping this as a flag-only twin
    /// of [`Self::render_obj_row`]'s two passes lets the painted path
    /// stay display-gated while STAT77 reads back correctly when OBJ is
    /// off-screen or the line is blanked.
    ///
    /// The evaluation is [`sprite::line_flags`], the rule of
    /// [`sprite::line_budget`] that the painter's caps and the line's
    /// sprite list share (33rd sprite -> range-over, 35th tile column ->
    /// time-over, the same off-screen tile-column skip), so the flags agree
    /// exactly with what the painted path draws.
    pub(super) fn eval_obj_overflow(&mut self, scanline: u16) {
        let (over_range, over_time) = sprite::line_flags(
            self.oam(),
            self.oam_first_sprite,
            self.obj_base_size,
            self.obj_interlace,
            scanline.wrapping_sub(1),
        );
        self.sprite_range_over |= over_range;
        self.sprite_time_over |= over_time;
    }

    /// Evaluate OAM for `scanline` and rasterise the visible sprites
    /// into `obj_row`.
    ///
    /// Two passes, the hardware's evaluation and its painting (checked
    /// against bsnes-plus):
    ///
    /// 1. **Range eval**: walk all 128 sprites starting at
    ///    [`Ppu::oam_first_sprite`]; gather up to 32 that fall on the
    ///    line. The 33rd onward are dropped (capped at 32).
    /// 2. **Tile build + raster**: replay the gathered sprites in
    ///    reverse so the highest-priority sprite paints last (and thus
    ///    wins on overlap). Each 8-px tile column consumes one of the
    ///    34 per-scanline tile slots; the 35th caps the loop and drops
    ///    the remaining (higher-priority) tiles, exactly the hardware
    ///    "too many sprites vanish" quirk.
    ///
    /// This path only *paints* and so runs solely when OBJ is displayed
    /// (not forced-blank, OBJ enabled on a screen). The STAT77 ($213E)
    /// range/time-overflow flags are owned by [`Self::eval_obj_overflow`],
    /// which runs every active scanline regardless of display state, so
    /// the caps here intentionally leave the flags alone.
    ///
    /// `scanline` is the 1-indexed vcounter; the OBJ Y comparison runs
    /// against `scanline - 1` so a sprite with OAM Y = N lands with its
    /// top row at output row N (the one-line OBJ pipeline latency is
    /// already absorbed by the vcounter being 1-indexed, see the
    /// Mode 7 note in [`Self::render_mode7_rows`] for the same offset
    /// reasoning).
    pub(super) fn render_obj_row<T: PixelTrace<Notes = FrameTrace>>(
        &mut self,
        scanline: u16,
        obj_row: &mut ObjRowBuffer,
        trace: &mut T,
    ) {
        *obj_row = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];

        let line = scanline.wrapping_sub(1);
        let obj_interlace = self.obj_interlace;
        let field = self.field;
        let base_size = self.obj_base_size;

        // Pass 1: range eval.
        let mut items: [u8; 32] = [0xFF; 32];
        let mut item_count = 0usize;
        for i in 0..128usize {
            let idx = (usize::from(self.oam_first_sprite) + i) & 0x7F;
            let s = sprite::parse_sprite(self.oam(), idx, base_size, obj_interlace);
            if !sprite::sprite_on_scanline(&s, line, obj_interlace) {
                continue;
            }
            if item_count >= 32 {
                // Cap the painted set at 32; the range-over flag is set
                // by `eval_obj_overflow` (runs even when OBJ is off).
                break;
            }
            items[item_count] = idx as u8;
            item_count += 1;
        }

        // The active-scan OAM read pointer tracks the last in-range
        // sprite's high-table word.
        if item_count > 0 {
            self.oam_iaddr = 0x0200 + (u16::from(items[item_count - 1]) >> 2);
        }

        // Pass 2: reverse tile build + raster.
        let mut tile_count = 0usize;
        'items: for i in (0..item_count).rev() {
            let idx = usize::from(items[i]);
            let s = sprite::parse_sprite(self.oam(), idx, base_size, obj_interlace);
            let y_row = sprite::sprite_row_in_sprite(&s, line, field, obj_interlace);
            let row_in_tile = (y_row & 7) as usize;
            let tiles_wide = u16::from(s.width >> 3);

            for tx in 0..tiles_wide {
                let sx0 = (s.x + (tx << 3)) & 0x1FF;
                // Skip tile columns that sit entirely in the off-screen
                // 256..512 band (unless x == 256, which loads them all).
                if s.x != 256 && sx0 >= 256 && (sx0 + 7) < 512 {
                    continue;
                }
                if tile_count >= 34 {
                    // Cap painting at 34 tile columns (the "too many
                    // sprites vanish" quirk); the time-over flag is set
                    // by `eval_obj_overflow`.
                    break 'items;
                }
                tile_count += 1;

                let tile_number = sprite::sprite_tile_number(
                    self.obj_tiledata_addr,
                    self.obj_name_select,
                    s.character,
                    s.nameselect,
                    s.hflip,
                    s.width,
                    tx as u8,
                    y_row,
                );
                let pixels = self.cached_sprite_tile(tile_number);
                for px in 0..8u16 {
                    let sx = (sx0 + px) & 0x1FF;
                    if sx >= 256 {
                        continue;
                    }
                    let col = if s.hflip {
                        7 - px as usize
                    } else {
                        px as usize
                    };
                    let pixel = pixels[row_in_tile * 8 + col];
                    if pixel != 0 {
                        let cgram_idx = 128 + usize::from(s.palette) * 16 + usize::from(pixel);
                        obj_row[sx as usize] = ObjRowPixel {
                            color: self.cgram()[cgram_idx],
                            opaque: true,
                            prio: s.priority,
                            palette: s.palette,
                        };
                        if T::ON {
                            trace.notes().obj[sx as usize] = ObjOrigin {
                                tile: tile_value(
                                    u32::from(tile_number) << 4,
                                    u32::from(tile_number),
                                    row_in_tile as u32,
                                    col as u32,
                                ),
                                colour: colour_value(cgram_idx as u8, s.palette, 4),
                                slot: idx as u8,
                                priority: s.priority,
                            };
                        }
                    }
                }
            }
        }
    }

    /// Look up, or decode, a 4bpp sprite tile, returning all eight
    /// rows (`[u8; 64]`, row-major). The returned array is `Copy`, so
    /// the `&mut self` borrow ends here and the caller can re-borrow
    /// `self.cgram()` while reading pixels. Coherence rides
    /// [`Ppu::vram_generation`], the same contract as
    /// [`Self::cached_tilemap_row`].
    fn cached_sprite_tile(&mut self, tile_number: u16) -> [u8; 64] {
        let generation = self.vram_generation;
        if !self.sprite_tile_cache.matches(tile_number, generation) {
            let base_byte = u32::from(tile_number) << 5;
            let mut pixels = [0u8; 64];
            for row in 0..8usize {
                let decoded =
                    decode_tile_4bpp_row(self.vram(), 0, base_byte, row as u8, false, false);
                pixels[row * 8..row * 8 + 8].copy_from_slice(&decoded);
            }
            self.sprite_tile_cache
                .store(tile_number, generation, pixels);
        }
        *self.sprite_tile_cache.pixels(tile_number)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use retro_core::capture::NoTrace;
    use retro_core::Framebuffer;

    fn empty_oam() -> [u8; 0x220] {
        [0u8; 0x220]
    }

    #[test]
    fn size_table_small_and_large() {
        // (selector, small w/h, large w/h) from list.cpp.
        let expect = [
            (0u8, (8u8, 8u8), (16u8, 16u8)),
            (1, (8, 8), (32, 32)),
            (2, (8, 8), (64, 64)),
            (3, (16, 16), (32, 32)),
            (4, (16, 16), (64, 64)),
            (5, (32, 32), (64, 64)),
            (6, (16, 32), (32, 64)),
            (7, (16, 32), (32, 32)),
        ];
        for (sel, small, large) in expect {
            assert_eq!(sprite_size(sel, false, false), small, "sel {sel} small");
            assert_eq!(sprite_size(sel, true, false), large, "sel {sel} large");
        }
    }

    #[test]
    fn size_table_interlace_collapses_small_height_for_6_and_7() {
        // Selectors 6/7 small: 16x32 -> 16x16 under interlace.
        assert_eq!(sprite_size(6, false, true), (16, 16));
        assert_eq!(sprite_size(7, false, true), (16, 16));
        // Large is untouched.
        assert_eq!(sprite_size(6, true, true), (32, 64));
        // Selectors < 6 unaffected.
        assert_eq!(sprite_size(3, false, true), (16, 16));
        assert_eq!(sprite_size(0, false, true), (8, 8));
    }

    #[test]
    fn parse_extracts_low_and_high_table_fields() {
        let mut oam = empty_oam();
        // Sprite 1: X-low=0x20, Y=0x30, char=0x42, attr=
        //   vflip(0x80) | priority 2 (0x20) | palette 3 (0x06) |
        //   nameselect(0x01) = 0xA7.
        oam[4] = 0x20;
        oam[5] = 0x30;
        oam[6] = 0x42;
        oam[7] = 0x80 | 0x20 | 0x06 | 0x01;
        // High table byte 0 holds sprites 0-3 (2 bits each). Sprite 1
        // occupies bits 2-3: set X bit 8 (0b01) + large (0b10) = 0b11
        // -> byte = 0b11 << 2 = 0x0C.
        oam[0x200] = 0x0C;

        let s = parse_sprite(&oam, 1, 0, false);
        assert_eq!(s.x, 0x120, "X bit 8 from high table");
        assert_eq!(s.y, 0x30);
        assert_eq!(s.character, 0x42);
        assert!(s.vflip);
        assert!(!s.hflip);
        assert_eq!(s.priority, 2);
        assert_eq!(s.palette, 3);
        assert!(s.nameselect);
        assert!(s.large, "large bit from high table");
        assert_eq!((s.width, s.height), (16, 16), "selector 0 large");
    }

    #[test]
    fn on_scanline_basic_range() {
        let s = SpriteAttrs {
            x: 0,
            y: 10,
            character: 0,
            palette: 0,
            priority: 0,
            hflip: false,
            vflip: false,
            nameselect: false,
            large: false,
            width: 8,
            height: 8,
        };
        assert!(!sprite_on_scanline(&s, 9, false));
        assert!(sprite_on_scanline(&s, 10, false));
        assert!(sprite_on_scanline(&s, 17, false));
        assert!(!sprite_on_scanline(&s, 18, false));
    }

    #[test]
    fn on_scanline_y_wrap() {
        // Y=252, height 8 -> covers 252..=255 and wraps to 0..=3.
        let s = SpriteAttrs {
            x: 0,
            y: 252,
            character: 0,
            palette: 0,
            priority: 0,
            hflip: false,
            vflip: false,
            nameselect: false,
            large: false,
            width: 8,
            height: 8,
        };
        assert!(sprite_on_scanline(&s, 255, false));
        assert!(sprite_on_scanline(&s, 0, false));
        assert!(sprite_on_scanline(&s, 3, false));
        assert!(!sprite_on_scanline(&s, 4, false));
    }

    #[test]
    fn on_scanline_offscreen_right_rejected() {
        // x=300, width 16 -> 300+15=315 < 512, x>256 -> rejected.
        let s = SpriteAttrs {
            x: 300,
            y: 0,
            character: 0,
            palette: 0,
            priority: 0,
            hflip: false,
            vflip: false,
            nameselect: false,
            large: false,
            width: 16,
            height: 8,
        };
        assert!(!sprite_on_scanline(&s, 0, false));
        // x=500, width 64 -> 500+63=563 >= 512 -> wraps to the left, kept.
        let wrap = SpriteAttrs {
            x: 500,
            width: 64,
            ..s
        };
        assert!(sprite_on_scanline(&wrap, 0, false));
    }

    #[test]
    fn on_scanline_interlace_halves_height() {
        // height 16 -> effective 8 under interlace.
        let s = SpriteAttrs {
            x: 0,
            y: 0,
            character: 0,
            palette: 0,
            priority: 0,
            hflip: false,
            vflip: false,
            nameselect: false,
            large: false,
            width: 8,
            height: 16,
        };
        assert!(sprite_on_scanline(&s, 7, true));
        assert!(!sprite_on_scanline(&s, 8, true));
        // Without interlace the full 16 rows are visible.
        assert!(sprite_on_scanline(&s, 15, false));
    }

    #[test]
    fn row_in_sprite_no_flip() {
        let s = SpriteAttrs {
            x: 0,
            y: 10,
            character: 0,
            palette: 0,
            priority: 0,
            hflip: false,
            vflip: false,
            nameselect: false,
            large: false,
            width: 8,
            height: 8,
        };
        assert_eq!(sprite_row_in_sprite(&s, 10, false, false), 0);
        assert_eq!(sprite_row_in_sprite(&s, 13, false, false), 3);
    }

    #[test]
    fn row_in_sprite_vflip_square() {
        let s = SpriteAttrs {
            x: 0,
            y: 0,
            character: 0,
            palette: 0,
            priority: 0,
            hflip: false,
            vflip: true,
            nameselect: false,
            large: false,
            width: 16,
            height: 16,
        };
        // Square: y -> (height-1) - y.
        assert_eq!(sprite_row_in_sprite(&s, 0, false, false), 15);
        assert_eq!(sprite_row_in_sprite(&s, 15, false, false), 0);
    }

    #[test]
    fn row_in_sprite_vflip_nonsquare() {
        // 16x32 (selector 6 large is 32x64; use 16x32 small).
        let s = SpriteAttrs {
            x: 0,
            y: 0,
            character: 0,
            palette: 0,
            priority: 0,
            hflip: false,
            vflip: true,
            nameselect: false,
            large: false,
            width: 16,
            height: 32,
        };
        // y < width(16): (width-1) - y. y=0 -> 15.
        assert_eq!(sprite_row_in_sprite(&s, 0, false, false), 15);
        // y >= width: width + ((width-1) - (y-width)). y=16 -> 16 + (15 - 0) = 31.
        assert_eq!(sprite_row_in_sprite(&s, 16, false, false), 31);
        // y=31 -> 16 + (15 - 15) = 16.
        assert_eq!(sprite_row_in_sprite(&s, 31, false, false), 16);
    }

    #[test]
    fn row_in_sprite_interlace_doubles_and_adds_field() {
        let s = SpriteAttrs {
            x: 0,
            y: 0,
            character: 0,
            palette: 0,
            priority: 0,
            hflip: false,
            vflip: false,
            nameselect: false,
            large: false,
            width: 8,
            height: 16,
        };
        // line 3, interlace, field 0 -> y = 3<<1 = 6.
        assert_eq!(sprite_row_in_sprite(&s, 3, false, true), 6);
        // field 1 -> +1 = 7.
        assert_eq!(sprite_row_in_sprite(&s, 3, true, true), 7);
    }

    #[test]
    fn tile_number_basic_and_hflip_column_order() {
        // base 0, char 0, no nameselect, 16px wide (2 tiles).
        let t0 = sprite_tile_number(0, 0, 0, false, false, 16, 0, 0);
        let t1 = sprite_tile_number(0, 0, 0, false, false, 16, 1, 0);
        assert_eq!(t0, 0);
        assert_eq!(t1, 1);
        // hflip reverses the tile-column order (mx = tiles_wide-1-tx).
        let h0 = sprite_tile_number(0, 0, 0, false, true, 16, 0, 0);
        let h1 = sprite_tile_number(0, 0, 0, false, true, 16, 1, 0);
        assert_eq!(h0, 1);
        assert_eq!(h1, 0);
    }

    #[test]
    fn tile_number_chrx_wraps_in_16_grid() {
        // char low nibble 15, second tile column wraps 15->0 (same row).
        let t = sprite_tile_number(0, 0, 0x0F, false, false, 16, 1, 0);
        // chrx=15, mx=1 -> (15+1)&15 = 0, chry=0 -> tile 0.
        assert_eq!(t, 0);
    }

    #[test]
    fn tile_number_advances_chry_per_8_rows() {
        // y_row 8 -> y_row>>3 = 1 -> chry += 1 -> chry<<4 = 16 -> tile 16.
        let t = sprite_tile_number(0, 0, 0, false, false, 8, 0, 8);
        assert_eq!(t, 16);
    }

    #[test]
    fn tile_number_nameselect_offset() {
        // nameselect adds 256*32 bytes = page -> tile += 256.
        let t = sprite_tile_number(0, 0, 0, true, false, 8, 0, 0);
        assert_eq!(t, 256);
        // name_select index adds another (idx<<13) bytes = idx*256 tiles.
        let t2 = sprite_tile_number(0, 1, 0, true, false, 8, 0, 0);
        assert_eq!(t2, 256 + 256);
    }

    /// Put sprite `slot` at `(x, y)`, its X bit 8 and size bit from
    /// `large`, the other fields zero.
    fn place(oam: &mut [u8; 0x220], slot: usize, x: u16, y: u8, large: bool) {
        oam[slot * 4] = x as u8;
        oam[slot * 4 + 1] = y;
        let shift = (slot & 3) * 2;
        let bits = u8::from(x >= 256) | (u8::from(large) << 1);
        let high = &mut oam[0x200 + (slot >> 2)];
        *high = (*high & !(0x03 << shift)) | (bits << shift);
    }

    /// Every sprite parked off the screen's lines.
    fn parked_oam() -> [u8; 0x220] {
        let mut oam = empty_oam();
        for slot in 0..128 {
            place(&mut oam, slot, 0, 0xF0, false);
        }
        oam
    }

    #[test]
    fn forty_sprites_on_a_line_keep_thirty_two_and_drop_eight_whole() {
        let mut oam = parked_oam();
        for slot in 0..40 {
            place(&mut oam, slot, (slot * 6) as u16, 50, false);
        }
        let mut dropped = Vec::new();
        let budget = line_budget(&oam, 0, 0, false, 50, Some(&mut dropped));
        assert_eq!(budget.kept(), (0..32u8).collect::<Vec<_>>().as_slice());
        assert_eq!(dropped, (32..40u8).collect::<Vec<_>>());
        assert!(budget.over_range);
        assert!(!budget.over_time, "32 eight-pixel sprites fetch 32 columns");
        assert!(budget.columns_drawn[..32].iter().all(|&c| c == 1));

        // Without the list the flags are the same.
        let flags_only = line_budget(&oam, 0, 0, false, 50, None);
        assert_eq!(flags_only, budget);

        // The rotation starts the walk at another sprite: from sprite 20,
        // sprites 20-39 and 0-11 are kept, 12-19 dropped.
        dropped.clear();
        let rotated = line_budget(&oam, 20, 0, false, 50, Some(&mut dropped));
        let kept: Vec<u8> = (20..40u8).chain(0..12).collect();
        assert_eq!(rotated.kept(), kept.as_slice());
        assert_eq!(dropped, (12..20u8).collect::<Vec<_>>());
    }

    #[test]
    fn ten_wide_sprites_draw_thirty_four_columns_and_lose_six_of_the_first() {
        // Selector 1: small 8x8, large 32x32; ten large sprites on line 10
        // want 40 columns.
        let mut oam = parked_oam();
        for slot in 0..10 {
            place(&mut oam, slot, (slot * 20) as u16, 10, true);
        }
        let budget = line_budget(&oam, 0, 1, false, 10, None);
        assert_eq!(budget.kept_count, 10);
        assert!(budget.over_time);
        assert!(!budget.over_range);
        let drawn: u32 = budget.columns_drawn.iter().map(|&c| u32::from(c)).sum();
        assert_eq!(drawn, 34);
        // The fetch runs from the last kept sprite back to the first, so
        // the sprites met first lose their columns: sprite 0 all four,
        // sprite 1 the last two of its four.
        assert_eq!(
            &budget.columns_dropped[..10],
            &[4, 2, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(&budget.columns_drawn[..10], &[0, 2, 4, 4, 4, 4, 4, 4, 4, 4]);
    }

    #[test]
    fn off_screen_columns_are_not_counted() {
        // Selector 1, large 32x32 sprites at x = 240: the first two of
        // their four columns show (240..255), the other two lie in
        // 256..271 and are skipped. Twenty of them want 40 columns on
        // screen, past the limit.
        let mut oam = parked_oam();
        for slot in 0..20 {
            place(&mut oam, slot, 240, 0, true);
        }
        let budget = line_budget(&oam, 0, 1, false, 0, None);
        assert!(budget.columns_off_screen[..20].iter().all(|&c| c == 2));
        let drawn: u32 = budget.columns_drawn.iter().map(|&c| u32::from(c)).sum();
        assert_eq!(drawn, 34, "the skipped columns cost nothing");
        assert!(budget.over_time);

        // Seventeen of them fit: 34 columns on screen, none dropped.
        let mut oam = parked_oam();
        for slot in 0..17 {
            place(&mut oam, slot, 240, 0, true);
        }
        let budget = line_budget(&oam, 0, 1, false, 0, None);
        assert!(!budget.over_time);
        assert!(budget.columns_dropped.iter().all(|&c| c == 0));

        // A sprite at x = 256 has its columns fetched although none shows.
        let mut oam = parked_oam();
        place(&mut oam, 0, 256, 0, true);
        let budget = line_budget(&oam, 0, 1, false, 0, None);
        assert_eq!(budget.columns_drawn[0], 4);
        let s = parse_sprite(&oam, 0, 1, false);
        assert_eq!(drawn_pixels(&s, 4), 0);
    }

    #[test]
    fn the_flags_alone_are_the_budgets_flags() {
        let mut state = 0xF1A6_5EEDu64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..500 {
            let mut oam = parked_oam();
            let line = (next() % 224) as u16;
            for slot in 0..128 {
                let y = line.wrapping_sub((next() % 40) as u16) as u8;
                let y = if next() % 3 == 0 {
                    (next() % 256) as u8
                } else {
                    y
                };
                place(&mut oam, slot, (next() % 512) as u16, y, next() % 2 == 0);
            }
            let first = (next() % 128) as u8;
            let size = (next() % 8) as u8;
            let interlace = next() % 4 == 0;
            let budget = line_budget(&oam, first, size, interlace, line, None);
            assert_eq!(
                line_flags(&oam, first, size, interlace, line),
                (budget.over_range, budget.over_time)
            );
        }
    }

    #[test]
    fn drawn_pixels_count_what_falls_on_the_screen() {
        let mut oam = parked_oam();
        // 16 wide at x = 250: the first column shows 250..255, the
        // second (258..265) lies past the right edge and is skipped.
        place(&mut oam, 0, 250, 0, false);
        let s = parse_sprite(&oam, 0, 3, false);
        assert_eq!(s.width, 16);
        assert_eq!(drawn_pixels(&s, 2), 6);
        // 16 wide at x = 505 (-7): 505..511 off, 512 -> 0 on; then 1..8.
        place(&mut oam, 1, 505, 0, false);
        let s = parse_sprite(&oam, 1, 3, false);
        assert_eq!(drawn_pixels(&s, 2), 9);
        assert_eq!(drawn_pixels(&s, 1), 1, "only the first column drawn");
        assert_eq!(column_visible_pixels(0), 8);
        assert_eq!(column_visible_pixels(252), 4);
        assert_eq!(column_visible_pixels(256), 0);
    }

    // -- Sprite (OBJ) rasteriser -------------------------------

    /// Bare 8x8-small sprite PPU with the OBJ tile data at VRAM 0 and
    /// every sprite parked at OAM Y = 240 (below the visible test
    /// lines), so individual tests can place exactly the sprites they
    /// care about.
    fn obj_test_ppu() -> Ppu {
        let mut ppu = Ppu::new();
        ppu.obj_base_size = 0;
        ppu.obj_tiledata_addr = 0;
        ppu.obj_name_select = 0;
        for i in 0..128 {
            ppu.oam[i * 4 + 1] = 0xF0;
        }
        ppu
    }

    /// Program sprite `idx`'s four low-table bytes.
    pub(crate) fn set_sprite(ppu: &mut Ppu, idx: usize, x: u8, y: u8, character: u8, attr: u8) {
        let base = idx * 4;
        ppu.oam[base] = x;
        ppu.oam[base + 1] = y;
        ppu.oam[base + 2] = character;
        ppu.oam[base + 3] = attr;
    }

    #[test]
    fn the_painter_draws_the_columns_the_line_budget_keeps() {
        // Over random sprite tables, each column of the sprite row belongs to
        // the sprite a painter working from `sprite::line_budget` gives it:
        // the kept sprites back to front, each its first `columns_drawn`
        // fetched columns. Every character pixel is opaque, so a painted
        // column is never left to the sprite behind.
        use crate::ppu::trace::FrameTrace;
        let mut state = 0x5EED_0B1Eu64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..300 {
            let mut ppu = obj_test_ppu();
            ppu.vram.fill(0xFFFF);
            ppu.obj_base_size = (next() % 8) as u8;
            ppu.oam_first_sprite = (next() % 128) as u8;
            let scanline = 1 + (next() % 224) as u16;
            let line = scanline - 1;
            for slot in 0..128 {
                // Most sprites near the line, so the limits are reached.
                let near = next() % 4 != 0;
                let y = if near {
                    line.wrapping_sub((next() % 48) as u16) as u8
                } else {
                    (next() % 256) as u8
                };
                let x = (next() % 512) as u16;
                set_sprite(
                    &mut ppu,
                    slot,
                    x as u8,
                    y,
                    (next() % 256) as u8,
                    (next() % 256) as u8,
                );
                let shift = (slot & 3) * 2;
                let bits = u8::from(x >= 256) | (((next() % 2) as u8) << 1);
                let high = &mut ppu.oam[0x200 + (slot >> 2)];
                *high = (*high & !(3 << shift)) | (bits << shift);
            }

            let mut obj = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];
            let mut trace = FrameTrace::new(0, None);
            ppu.render_obj_row(scanline, &mut obj, &mut trace);

            let budget = sprite::line_budget(
                &ppu.oam,
                ppu.oam_first_sprite,
                ppu.obj_base_size,
                false,
                line,
                None,
            );
            let mut expected = [None; NORMAL_SCANLINE_WIDTH];
            for k in (0..usize::from(budget.kept_count)).rev() {
                let slot = budget.kept[k];
                let s = sprite::parse_sprite(&ppu.oam, usize::from(slot), ppu.obj_base_size, false);
                let mut left = budget.columns_drawn[k];
                for tx in 0..u16::from(s.width >> 3) {
                    let column_x = (s.x + (tx << 3)) & 0x1FF;
                    if sprite::column_off_screen(s.x, column_x) {
                        continue;
                    }
                    if left == 0 {
                        break;
                    }
                    left -= 1;
                    for px in 0..8 {
                        let sx = usize::from((column_x + px) & 0x1FF);
                        if sx < NORMAL_SCANLINE_WIDTH {
                            expected[sx] = Some(slot);
                        }
                    }
                }
            }
            let painted: Vec<Option<u8>> = obj
                .iter()
                .enumerate()
                .map(|(x, pixel)| pixel.opaque.then(|| trace.obj[x].slot))
                .collect();
            assert_eq!(painted, expected.to_vec(), "case {case}");
        }
    }

    #[test]
    fn render_obj_row_single_8x8_sprite() {
        let mut ppu = obj_test_ppu();
        set_sprite(&mut ppu, 0, 0, 0, 0, 0);
        // Tile 0 row 0: plane0 = 0xFF -> colour index 1 across the row.
        ppu.vram[0] = 0x00FF;
        ppu.cgram[129] = 0x1234; // 128 + palette 0 * 16 + index 1

        let mut obj = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];
        // Scanline 1 -> line 0 -> sprite row 0.
        ppu.render_obj_row(1, &mut obj, &mut NoTrace::<FrameTrace>::new());

        for (x, px) in obj.iter().enumerate().take(8) {
            assert!(px.opaque, "col {x} opaque");
            assert_eq!(px.color, 0x1234, "col {x} colour");
            assert_eq!(px.prio, 0);
        }
        assert!(!obj[8].opaque, "column past the sprite is transparent");
    }

    #[test]
    fn render_obj_row_palette_and_priority_from_attr() {
        let mut ppu = obj_test_ppu();
        // attr: priority 3 (0x30) | palette 5 (0x0A) = 0x3A.
        set_sprite(&mut ppu, 0, 0, 0, 0, 0x3A);
        ppu.vram[0] = 0x00FF; // index 1
        ppu.cgram[128 + 5 * 16 + 1] = 0x7ABC;

        let mut obj = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];
        ppu.render_obj_row(1, &mut obj, &mut NoTrace::<FrameTrace>::new());
        assert!(obj[0].opaque);
        assert_eq!(obj[0].color, 0x7ABC);
        assert_eq!(obj[0].prio, 3);
    }

    #[test]
    fn render_obj_row_hflip_reverses_row() {
        let mut ppu = obj_test_ppu();
        set_sprite(&mut ppu, 0, 0, 0, 0, 0x40); // hflip
                                                // plane0 bit 7 set -> only column 0 opaque before flipping.
        ppu.vram[0] = 0x0080;
        ppu.cgram[129] = 0x1234;

        let mut obj = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];
        ppu.render_obj_row(1, &mut obj, &mut NoTrace::<FrameTrace>::new());
        assert!(obj[7].opaque, "hflip moves the leftmost pixel to column 7");
        assert_eq!(obj[7].color, 0x1234);
        assert!(!obj[0].opaque);
    }

    #[test]
    fn resolved_render_order_top_matches_obj_row_winner() {
        // Parity guard: the sprite the resolved order reports on top
        // (last) must win the overlap pixel the renderer actually paints.
        use retro_core::video::VideoDebugger;
        let mut ppu = obj_test_ppu();
        // The order lists what a frame showing sprites drew.
        ppu.display_disable = false;
        ppu.display_brightness = 15;
        ppu.obj_main_enable = true;
        // Two 8x8 sprites stacked at (0,0): slot 0 palette 0, slot 1
        // palette 1, both lit on row 0 -> they overlap at column 0.
        set_sprite(&mut ppu, 0, 0, 0, 0, 0x00); // palette 0
        set_sprite(&mut ppu, 1, 0, 0, 0, 0x02); // palette 1 (attr bit 1)
        ppu.vram[0] = 0x00FF; // tile 0 row 0 -> index 1 across
        ppu.cgram[128 + 1] = 0xAAAA; // palette 0 index 1 (slot 0)
        ppu.cgram[128 + 16 + 1] = 0x5555; // palette 1 index 1 (slot 1)

        let mut obj = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];
        ppu.render_obj_row(1, &mut obj, &mut NoTrace::<FrameTrace>::new());

        let order = ppu.resolved_sprite_render_order();
        let top = order.last().expect("non-empty order");
        assert_eq!(top.index, 0, "OAM slot 0 (no rotation) is highest priority");
        assert!(obj[0].opaque);
        assert_eq!(
            obj[0].color, 0xAAAA,
            "overlap shows the top sprite's colour"
        );
        assert_ne!(obj[0].color, 0x5555, "lower sprite did not win the overlap");
    }

    #[test]
    fn eval_obj_overflow_range_over_past_32_on_line() {
        let mut ppu = obj_test_ppu();
        // 33 sprites all on line 0 at x = 0 -> range overflow.
        ppu.vram[0] = 0x00FF;
        ppu.cgram[129] = 0x1234;
        for i in 0..33 {
            set_sprite(&mut ppu, i, 0, 0, 0, 0);
        }
        ppu.eval_obj_overflow(1);
        assert!(
            ppu.sprite_range_over,
            "33 sprites on one line sets range-over"
        );
        assert!(
            !ppu.sprite_time_over,
            "33 single-tile sprites stay under 34 tiles"
        );
    }

    #[test]
    fn eval_obj_overflow_time_over_past_34_tiles() {
        let mut ppu = obj_test_ppu();
        ppu.obj_base_size = 2; // small 8x8, large 64x64
        ppu.vram[0] = 0x00FF;
        ppu.cgram[129] = 0x1234;
        // Five 64-wide large sprites = 5 x 8 = 40 tile columns > 34.
        // Spread their X so the tile columns are on-screen.
        for i in 0..5 {
            // high-table large bit for sprite i.
            ppu.oam[0x200 + (i >> 2)] |= 0x02 << ((i & 3) * 2);
            set_sprite(&mut ppu, i, 0, 0, 0, 0);
        }
        ppu.eval_obj_overflow(1);
        assert!(ppu.sprite_time_over, ">34 tiles on one line sets time-over");
    }

    #[test]
    fn render_scanline_sets_overflow_flags_with_obj_off_and_forced_blank() {
        // The Electronics Test of Nintendo's SNES Test Program needs this:
        // STAT77 ($213E) range/time-over must be set by sprite evaluation even
        // when OBJ is disabled on both screens AND the screen is in
        // forced blank, only compositing is display-gated on hardware
        // (checked against snes9x). render_obj_row (the
        // painted path) would be skipped in both states, so the flags
        // have to come from the unconditional eval in render_scanline.
        let mut ppu = obj_test_ppu();
        ppu.vram[0] = 0x00FF;
        ppu.cgram[129] = 0x1234;
        for i in 0..33 {
            set_sprite(&mut ppu, i, 0, 0, 0, 0);
        }
        // OBJ off on both screens + forced blank: the painter never runs.
        ppu.obj_main_enable = false;
        ppu.obj_sub_enable = false;
        ppu.display_disable = true;

        let mut fb = Framebuffer::new(NORMAL_SCANLINE_WIDTH as u32, 224);
        ppu.render_scanline(1, &mut fb);
        assert!(
            ppu.sprite_range_over,
            "range-over must be set even with OBJ off + forced blank"
        );
    }

    #[test]
    fn cached_sprite_tile_reuses_until_generation_bumps() {
        let mut ppu = obj_test_ppu();
        ppu.vram[0] = 0x00FF; // tile 0 row 0 -> index 1 across

        let first = ppu.cached_sprite_tile(0);
        assert_eq!(first[0], 1);
        let second = ppu.cached_sprite_tile(0);
        assert_eq!(first, second, "same generation -> identical cache hit");

        // Poke VRAM without bumping the generation: the cache must keep
        // serving the prior decode (coherence is generation-keyed, and
        // real writes only land while the generation can move).
        ppu.vram[0] = 0x0000;
        assert_eq!(
            ppu.cached_sprite_tile(0)[0],
            1,
            "stale VRAM poke under a frozen generation stays cached"
        );

        // A landed VRAM write bumps the generation -> re-decode.
        ppu.vram_generation += 1;
        assert_eq!(
            ppu.cached_sprite_tile(0)[0],
            0,
            "generation bump forces a fresh decode"
        );
    }

    #[test]
    fn render_scanline_clears_sprite_overflow_at_frame_top() {
        let mut ppu = Ppu::new();
        ppu.display_disable = false;
        ppu.display_brightness = 15;
        // Push all 128 sprites off the tested scanlines (Y=224, the Y
        // byte is OAM offset +1): an all-zero OAM otherwise parks 128 8x8
        // sprites at Y=0, which the unconditional `eval_obj_overflow`
        // would (correctly) count as a line-0 range overflow and re-set
        // the flag right after the frame-top clear we're trying to
        // observe.
        for i in 0..128 {
            ppu.oam[i * 4 + 1] = 0xE0;
        }
        ppu.sprite_range_over = true;
        ppu.sprite_time_over = true;
        let mut fb = Framebuffer::new(256, 224);

        // Scanline 1 is the frame top -> both flags reset.
        ppu.render_scanline(1, &mut fb);
        assert!(!ppu.sprite_range_over);
        assert!(!ppu.sprite_time_over);

        // A mid-frame scanline must NOT touch them (they accumulate).
        ppu.sprite_range_over = true;
        ppu.render_scanline(50, &mut fb);
        assert!(ppu.sprite_range_over, "only the frame top clears the flags");
    }

    #[test]
    fn render_scanline_clears_overflow_even_in_forced_blank() {
        let mut ppu = Ppu::new();
        ppu.display_disable = true; // forced blank short-circuits below
                                    // Off-screen OAM (Y byte at offset +1; see frame-top-clear test)
                                    // so the unconditional overflow eval doesn't re-flag a line-0
                                    // all-zero OAM.
        for i in 0..128 {
            ppu.oam[i * 4 + 1] = 0xE0;
        }
        ppu.sprite_range_over = true;
        ppu.sprite_time_over = true;
        let mut fb = Framebuffer::new(256, 224);
        ppu.render_scanline(1, &mut fb);
        assert!(
            !ppu.sprite_range_over,
            "frame-top clear precedes forced blank"
        );
        assert!(!ppu.sprite_time_over);
    }
}
