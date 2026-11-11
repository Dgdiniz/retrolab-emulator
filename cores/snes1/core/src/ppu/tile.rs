//! Pure tile / tilemap decode helpers used by the per-scanline
//! renderer.
//!
//! The module is intentionally `&self`-free, every function takes
//! the VRAM word array plus the addressing parameters by value, so
//! tests can drive them with a synthetic buffer and the renderer in
//! [`crate::ppu::render`] can mix them with mutable PPU state without
//! re-borrow gymnastics. The byte-address conventions match what the
//! `$2107-$210C` write handlers store (byte addresses, kept in `u32`
//! because the max value overflows `u16`); each helper divides by two
//! to index our word-addressed VRAM.
//!
//! Written from fullsnes's tilemap, tile and offset-per-tile formats
//! and Anomie's Mode 7 notes; checked against bsnes-plus. Tile pixels
//! are returned in left-to-right screen order, so a `flip_h` request
//! reverses the tuple before the renderer slots it into the per-BG
//! row buffer.

/// Decoded tilemap entry. The SNES packs the 16-bit tilemap word as
/// `vhpppccccccccccc` (bit 15 = vflip, bit 14 = hflip, bit 13 =
/// priority, bits 10-12 = palette `0..7`, bits 0-9 = 10-bit tile
/// index). Real silicon has one priority bit per tile and the renderer
/// promotes it via the per-mode priority list.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TilemapEntry {
    /// 10-bit raw tile index (`0..1023`).
    pub tile: u16,
    /// 3-bit palette number (`0..7`).
    pub palette: u8,
    /// Priority bit, the per-mode priority list places a tile's
    /// pixels by it.
    pub priority: bool,
    /// Horizontal flip, reverses the 8-pixel row at decode time.
    pub flip_h: bool,
    /// Vertical flip, reflects the row selector inside the tile.
    pub flip_v: bool,
}

impl TilemapEntry {
    /// Decode a raw tilemap word into the struct view.
    #[inline]
    #[must_use]
    pub const fn decode(word: u16) -> Self {
        Self {
            tile: word & 0x03FF,
            palette: ((word >> 10) & 0x07) as u8,
            priority: word & 0x2000 != 0,
            flip_h: word & 0x4000 != 0,
            flip_v: word & 0x8000 != 0,
        }
    }
}

/// BG-pixel `(mask_x, mask_y)` for a given tilemap size + hires flag.
///
/// The masks wrap the BG-pixel coordinate into the tilemap's virtual
/// screen before the tile-column / tile-row shift. Size bit 0 doubles
/// the horizontal span (64-wide), size bit 1 doubles the vertical
/// span (64-tall). `hires` doubles the base horizontal mask because a
/// Mode 5/6 tilemap entry covers a 16-px slot; the vertical mask is
/// unaffected (hires is X-only). Shared by [`fetch_bg_tilemap_entry`]
/// and the renderer's tilemap-row cache so the wrap stays in one
/// place.
#[inline]
#[must_use]
pub fn tilemap_masks(screen_size: u8, hires: bool, tile16: bool) -> (u16, u16) {
    // A 16x16-tile BG covers twice the BG-pixels per tilemap entry in
    // *both* axes (one 32x32 page spans 512x512 px, not 256x256), so it
    // doubles the base mask on X and Y. Hires doubles X only (a Mode 5/6
    // entry spans a 16-px slot but the rows stay 8-px). The two *do*
    // combine: Mode 5/6 with the size bit set (the PPU bus activity
    // test's Layer 1) is a 16x16 hires BG, and both want the same widened
    // X, so `wide_x` folds them into one code path; only `tile16`
    // additionally widens Y.
    let wide_x = hires || tile16;
    let mut mask_x: u16 = if wide_x { 0x01FF } else { 0x00FF };
    let mut mask_y: u16 = if tile16 { 0x01FF } else { 0x00FF };
    if screen_size & 1 != 0 {
        mask_x = if wide_x { 0x03FF } else { 0x01FF };
    }
    if screen_size & 2 != 0 {
        mask_y = if tile16 { 0x03FF } else { 0x01FF };
    }
    (mask_x, mask_y)
}

/// Fetch a tilemap entry covering the pixel `(screen_x, screen_y)` of
/// a background.
///
/// `screen_addr_byte` is the raw `$2107-$210A` value (post-`<< 9`,
/// already in byte units), `screen_size` is the 2-bit code: 0 =
/// 32x32 (1 page), 1 = 64x32 (2 horizontal), 2 = 32x64 (2 vertical),
/// 3 = 64x64 (4 pages). The pages are the tilemap format's: the
/// first horizontal page sits at `screen_addr`, the next horizontal
/// page at `screen_addr + 0x800` (1024 tiles x 2 bytes), the vertical
/// page below at the fixed `0x800`/`0x1000` offset.
///
/// `hires` widens the BG-pixel address space (doubled `mask_x` base)
/// and switches the tile-column shift from `>> 3` (8-px stride) to
/// `>> 4` (16-px stride). In hires Modes 5/6 each tilemap entry
/// describes a *pair* of adjacent 8-px tile slots, the renderer
/// then calls [`hires_adjacent_tile_offset`] to pick which half of
/// the pair to actually decode. The Y mask is **not** doubled
/// because hires affects only the X dimension; vertical doubling
/// belongs to the interlace path and kicks in inside the renderer via
/// `py = (py << 1) | field`, not via the tilemap fetch.
#[must_use]
pub fn fetch_bg_tilemap_entry(
    vram: &[u16; 0x8000],
    screen_addr_byte: u32,
    screen_size: u8,
    screen_x: u16,
    screen_y: u16,
    hires: bool,
    tile16: bool,
) -> TilemapEntry {
    let (mask_x, mask_y) = tilemap_masks(screen_size, hires, tile16);

    // Hires *or* 16x16 tiles force a 16-px horizontal tile stride; the
    // adjacent-tile bump (`hires_adjacent_tile_offset` for hires, the
    // renderer's right/bottom sub-tile pick for 16x16) then selects the
    // 8-px quadrant. 16x16 additionally forces a 16-px *vertical* stride
    // (`>> 4`) so each entry spans two 8-px tile rows.
    let tile_width_shift: u8 = if hires || tile16 { 4 } else { 3 };
    let tile_height_shift: u8 = if tile16 { 4 } else { 3 };
    let tx = (screen_x & mask_x) >> tile_width_shift;
    let ty = (screen_y & mask_y) >> tile_height_shift;

    let mut offset_tiles: u32 = (((ty & 0x1F) as u32) << 5) + ((tx & 0x1F) as u32);
    if tx & 0x20 != 0 {
        // Horizontal sub-page jump (32 tiles wide x 32 rows = 0x400).
        offset_tiles += 0x400;
    }
    if ty & 0x20 != 0 {
        // Vertical sub-page jump. For size 3 (64x64) the vertical
        // stride is 2 sub-pages (0x800); for size 2 (32x64) it's
        // a single sub-page (0x400).
        offset_tiles += if screen_size == 3 { 0x800 } else { 0x400 };
    }

    let word_index = ((screen_addr_byte >> 1) + offset_tiles) as usize & 0x7FFF;
    TilemapEntry::decode(vram[word_index])
}

/// Build the whole tilemap-entry row for tile-row `ty` (non-hires).
///
/// Returns 64 entries indexed by the **absolute** tile column
/// `tx = (screen_x & mask_x) >> 3`. The renderer caches this row per
/// BG and indexes it directly, so the 8 scanlines of one tile band
/// do not each re-run [`fetch_bg_tilemap_entry`] per tile. Every
/// slot is produced by that same function (with `hires = false`), so
/// the cached values are identical to the direct path by
/// construction, and the only behaviour the cache adds is its
/// invalidation.
///
/// `ty` is the already-masked tile-row index (`(screen_y & mask_y) >>
/// 3`, `0..=63`); any `screen_y` in the band yields the same row, so
/// the builder feeds `screen_y = ty << 3`. 64 entries cover the
/// widest page (`mask_x` up to `0x1FF` non-hires -> `tx` up to 63);
/// narrower sizes simply leave the high half as harmless duplicates
/// that the renderer never indexes (its `screen_x & mask_x` wraps
/// first). Offset-per-tile (Modes 2/4/6), hires (Modes 5/6) and 16x16
/// tiles do **not** use this path, they keep calling
/// [`fetch_bg_tilemap_entry`] because their per-column `voffset` /
/// 16-px stride / two-row entries break the "one tile row per
/// scanline" assumption the cache relies on.
#[must_use]
pub fn build_tilemap_row(
    vram: &[u16; 0x8000],
    screen_addr_byte: u32,
    screen_size: u8,
    ty: u16,
) -> [TilemapEntry; 64] {
    let screen_y = ty << 3;
    let mut row = [TilemapEntry::default(); 64];
    for (tx, slot) in row.iter_mut().enumerate() {
        let screen_x = (tx as u16) << 3;
        *slot = fetch_bg_tilemap_entry(
            vram,
            screen_addr_byte,
            screen_size,
            screen_x,
            screen_y,
            false,
            false,
        );
    }
    row
}

/// Hires-mode adjacent-tile selector.
///
/// In Modes 5/6 each tilemap entry covers 16 BG-pixels because
/// [`fetch_bg_tilemap_entry`] uses a `>> 4` tile-column shift. The
/// underlying tile data is still 8-px wide, so the renderer pairs
/// each entry with its right-side neighbour: bit 3 of `screen_x`
/// (= "second 8-px half of the 16-px slot") picks the neighbour
/// instead of the entry's own tile. `flip_h` swaps which half is
/// "first" so a horizontally-flipped 16-px slot still renders
/// left-to-right consistently.
///
/// Returns the additive offset to apply to the tilemap entry's
/// `tile` field before the per-row decode. Always 0 in non-hires
/// callers (they don't call this helper) and 0 or 1 in hires.
///
/// The second half of the 16-pixel slot takes the next tile, unless
/// the slot is flipped, in which case the first half does.
#[inline]
#[must_use]
pub fn hires_adjacent_tile_offset(screen_x: u16, flip_h: bool) -> u16 {
    let in_second_half = screen_x & 0x08 != 0;
    u16::from(in_second_half != flip_h)
}

/// Decode one row (`0..=7`) of a 2bpp tile.
///
/// `tile_index` is the 10-bit value from the tilemap (mapped via
/// `& 0x3FF`), `tile_data_base_byte` is the raw `$210B/$210C` field
/// for the BG. Returns 8 pixel values in screen order; `flip_h`
/// reverses the row after planar decode, `flip_v` flips the row
/// selector inside the tile.
#[must_use]
pub fn decode_tile_2bpp_row(
    vram: &[u16; 0x8000],
    tile_index: u16,
    tile_data_base_byte: u32,
    row_in_tile: u8,
    flip_h: bool,
    flip_v: bool,
) -> [u8; 8] {
    let effective_row = if flip_v { 7 - row_in_tile } else { row_in_tile };
    // 2bpp tile = 8 words. tile_data_base_byte is byte addressed.
    let tile_word_base =
        ((tile_data_base_byte >> 1) as usize).wrapping_add((tile_index as usize & 0x3FF) * 8);
    let word_index = (tile_word_base + effective_row as usize) & 0x7FFF;
    let plane01 = vram[word_index];
    let plane0 = (plane01 & 0xFF) as u8;
    let plane1 = (plane01 >> 8) as u8;
    extract_row_2plane(plane0, plane1, flip_h)
}

/// Decode one row of a 4bpp tile. In the tile format planes 0-1 sit
/// at the tile base and planes 2-3 sixteen bytes later (eight words
/// later in our addressing).
#[must_use]
pub fn decode_tile_4bpp_row(
    vram: &[u16; 0x8000],
    tile_index: u16,
    tile_data_base_byte: u32,
    row_in_tile: u8,
    flip_h: bool,
    flip_v: bool,
) -> [u8; 8] {
    let effective_row = if flip_v { 7 - row_in_tile } else { row_in_tile };
    // 4bpp tile = 16 words.
    let tile_word_base =
        ((tile_data_base_byte >> 1) as usize).wrapping_add((tile_index as usize & 0x3FF) * 16);
    let lo = vram[(tile_word_base + effective_row as usize) & 0x7FFF];
    let hi = vram[(tile_word_base + 8 + effective_row as usize) & 0x7FFF];
    let plane0 = (lo & 0xFF) as u8;
    let plane1 = (lo >> 8) as u8;
    let plane2 = (hi & 0xFF) as u8;
    let plane3 = (hi >> 8) as u8;
    extract_row_4plane(plane0, plane1, plane2, plane3, flip_h)
}

/// Decode one row of an 8bpp tile. Planes 0-1 at base, 2-3 at +8
/// words, 4-5 at +16 words, 6-7 at +24 words.
#[must_use]
pub fn decode_tile_8bpp_row(
    vram: &[u16; 0x8000],
    tile_index: u16,
    tile_data_base_byte: u32,
    row_in_tile: u8,
    flip_h: bool,
    flip_v: bool,
) -> [u8; 8] {
    let effective_row = if flip_v { 7 - row_in_tile } else { row_in_tile };
    // 8bpp tile = 32 words.
    let tile_word_base =
        ((tile_data_base_byte >> 1) as usize).wrapping_add((tile_index as usize & 0x3FF) * 32);
    let w01 = vram[(tile_word_base + effective_row as usize) & 0x7FFF];
    let w23 = vram[(tile_word_base + 8 + effective_row as usize) & 0x7FFF];
    let w45 = vram[(tile_word_base + 16 + effective_row as usize) & 0x7FFF];
    let w67 = vram[(tile_word_base + 24 + effective_row as usize) & 0x7FFF];
    let p0 = (w01 & 0xFF) as u8;
    let p1 = (w01 >> 8) as u8;
    let p2 = (w23 & 0xFF) as u8;
    let p3 = (w23 >> 8) as u8;
    let p4 = (w45 & 0xFF) as u8;
    let p5 = (w45 >> 8) as u8;
    let p6 = (w67 & 0xFF) as u8;
    let p7 = (w67 >> 8) as u8;
    let mut row = [0u8; 8];
    for (col, slot) in row.iter_mut().enumerate() {
        let bit = 7 - col as u8;
        *slot = ((p0 >> bit) & 1)
            | (((p1 >> bit) & 1) << 1)
            | (((p2 >> bit) & 1) << 2)
            | (((p3 >> bit) & 1) << 3)
            | (((p4 >> bit) & 1) << 4)
            | (((p5 >> bit) & 1) << 5)
            | (((p6 >> bit) & 1) << 6)
            | (((p7 >> bit) & 1) << 7);
    }
    if flip_h {
        row.reverse();
    }
    row
}

#[inline]
fn extract_row_2plane(plane0: u8, plane1: u8, flip_h: bool) -> [u8; 8] {
    let mut row = [0u8; 8];
    for (col, slot) in row.iter_mut().enumerate() {
        let bit = 7 - col as u8;
        *slot = ((plane0 >> bit) & 1) | (((plane1 >> bit) & 1) << 1);
    }
    if flip_h {
        row.reverse();
    }
    row
}

#[inline]
fn extract_row_4plane(p0: u8, p1: u8, p2: u8, p3: u8, flip_h: bool) -> [u8; 8] {
    let mut row = [0u8; 8];
    for (col, slot) in row.iter_mut().enumerate() {
        let bit = 7 - col as u8;
        *slot = ((p0 >> bit) & 1)
            | (((p1 >> bit) & 1) << 1)
            | (((p2 >> bit) & 1) << 2)
            | (((p3 >> bit) & 1) << 3);
    }
    if flip_h {
        row.reverse();
    }
    row
}

/// One BG3 tilemap word read as an offset-per-tile entry: a 10-bit
/// offset, a flag for each of the two backgrounds it may apply to,
/// and, in Mode 4, where one entry serves both axes, the axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OffsetEntry {
    offset: u16,
    applies_to_bg1: bool,
    applies_to_bg2: bool,
    vertical: bool,
}

impl OffsetEntry {
    fn decode(word: u16) -> Self {
        Self {
            offset: word & 0x03ff,
            applies_to_bg1: word & 0x2000 != 0,
            applies_to_bg2: word & 0x4000 != 0,
            vertical: word & 0x8000 != 0,
        }
    }

    fn applies_to(self, bg_index: u8) -> bool {
        if bg_index == 0 {
            self.applies_to_bg1
        } else {
            self.applies_to_bg2
        }
    }
}

/// Offset-per-tile (Modes 2, 4 and 6): a background's scroll replaced,
/// tile column by tile column, by values read from BG3's tilemap.
///
/// For the tile that starts `tile_left_x` into the line, BG3's tilemap
/// is read one tile to the left of it (so the first tile column keeps
/// the plain scroll), at BG3's own scroll. In Modes 2 and 6 that entry
/// may replace the horizontal scroll and the entry eight rows below
/// it the vertical one; in Mode 4 one entry serves both, bit 15
/// saying which. An entry applies only when its flag for the
/// background is set.
///
/// The returned `(hoffset, voffset)` are absolute tilemap coordinates:
/// the base scroll is already folded into `tile_left_x` / `scanline_y`,
/// and a replacement is likewise absolute, the tile's own position
/// plus the entry's offset, whose low three bits a horizontal
/// replacement drops. The caller must not add `x` / `scanline` again.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn compute_offset_per_tile(
    vram: &[u16; 0x8000],
    bg3_screen_addr_byte: u32,
    bg3_screen_size: u8,
    bg3_hoffset: u16,
    bg3_voffset: u16,
    bgmode: u8,
    bg_index: u8,
    tile_left_x: u16,
    base_hoffset: u16,
    base_voffset: u16,
    scanline_y: u16,
) -> (u16, u16) {
    let plain = (
        tile_left_x.wrapping_add(base_hoffset),
        scanline_y.wrapping_add(base_voffset),
    );
    // The tile's position within the scrolled line; the first tile
    // column has no column to its left to read from.
    let column_x = tile_left_x.wrapping_add(base_hoffset & 7);
    if column_x < 8 {
        return plain;
    }

    // Mode 6 is hires, and BG3 is read on the 16-pixel stride every
    // background has in Modes 5/6, over the doubled line.
    let hires = bgmode == 6;
    let source_x = (column_x - 8).wrapping_add(bg3_hoffset & !7);
    let entry_at = |rows_below: u16| {
        OffsetEntry::decode(fetch_bg_tilemap_word_raw(
            vram,
            bg3_screen_addr_byte,
            bg3_screen_size,
            source_x,
            bg3_voffset.wrapping_add(rows_below),
            hires,
        ))
    };
    let horizontal = |entry: OffsetEntry| column_x.wrapping_add(entry.offset & !7);
    let vertical = |entry: OffsetEntry| scanline_y.wrapping_add(entry.offset);

    let (mut hoffset, mut voffset) = plain;
    let first = entry_at(0);
    if bgmode == 4 {
        if first.applies_to(bg_index) {
            if first.vertical {
                voffset = vertical(first);
            } else {
                hoffset = horizontal(first);
            }
        }
    } else {
        if first.applies_to(bg_index) {
            hoffset = horizontal(first);
        }
        let second = entry_at(8);
        if second.applies_to(bg_index) {
            voffset = vertical(second);
        }
    }
    (hoffset, voffset)
}

/// Same addressing as [`fetch_bg_tilemap_entry`] but returns the
/// raw tilemap word instead of the decoded struct, used by the
/// offset-per-tile lookup (which cares about specific bits rather
/// than the standard fields).
///
/// `hires` selects the addressing space of the offset *source* plane
/// (BG3). In Modes 5/6 every background, BG3 included, is read on a
/// 16-px horizontal tile stride over the doubled 512-px space, Mode
/// 6's offset column runs 0..511. Reading it at the non-hires 8-px /
/// 256-px granularity instead folds the right half of the line back
/// onto the left and corrupts every Mode 6 offset, dropping the BG1
/// text (checked against bsnes-plus, which found the same). Modes 2/4
/// are non-hires and keep the 8-px / 256-px addressing. The vertical
/// stride stays 8 px in both; only the base mask widens to 0x1FF so a
/// multi-page BG3 (`$210x` size bits) wraps in the doubled space.
#[must_use]
fn fetch_bg_tilemap_word_raw(
    vram: &[u16; 0x8000],
    screen_addr_byte: u32,
    screen_size: u8,
    screen_x: u16,
    screen_y: u16,
    hires: bool,
) -> u16 {
    let base: u16 = if hires { 0x01FF } else { 0x00FF };
    let mask_x = if screen_size & 1 != 0 {
        (base << 1) | 1
    } else {
        base
    };
    let mask_y = if screen_size & 2 != 0 {
        (base << 1) | 1
    } else {
        base
    };
    let x_shift: u8 = if hires { 4 } else { 3 };

    let tx = (screen_x & mask_x) >> x_shift;
    let ty = (screen_y & mask_y) >> 3;

    let mut offset_tiles: u32 = (((ty & 0x1F) as u32) << 5) + ((tx & 0x1F) as u32);
    if tx & 0x20 != 0 {
        offset_tiles += 0x400;
    }
    if ty & 0x20 != 0 {
        offset_tiles += if screen_size == 3 { 0x800 } else { 0x400 };
    }

    let word_index = ((screen_addr_byte >> 1) + offset_tiles) as usize & 0x7FFF;
    vram[word_index]
}

// -------------------------------------------------------------
// Mode 7 (affine) sampling
// -------------------------------------------------------------

/// Sign-extend the low 13 bits of `v`.
///
/// Reads the Mode 7 centre (`M7X`/`M7Y`) and scroll
/// (`mode7_hoffset`/`voffset`) registers, which hold 13-bit signed
/// quantities inside 16-bit ports (bit 12 is the sign).
#[inline]
#[must_use]
pub fn sign_extend_13(v: u16) -> i32 {
    ((i32::from(v) & 0x1FFF) ^ 0x1000) - 0x1000
}

/// Mode 7 coordinate clip.
///
/// Applied to the `(hoffset - cx)` / `(voffset - cy)` differences
/// before the matrix multiply. Unlike a plain sign-extend it keeps
/// only the low 10 bits of magnitude and pulls the sign from bit
/// 13 (`0x2000`), discarding bits 10-12, the Mode 7 scroll latch
/// behaviour Anomie documented: `--s---nnnnnnnnnn ->
/// ssssssnnnnnnnnnn`.
#[inline]
#[must_use]
pub fn mode7_clip(n: i32) -> i32 {
    if n & 0x2000 != 0 {
        n | !0x3FF
    } else {
        n & 0x3FF
    }
}

/// Sample one Mode 7 character byte at world coordinates
/// `(px, py)`: the caller has already shifted past the 8-bit
/// pseudo-fraction.
///
/// `repeat` is M7SEL's screen-over field (`$211A` bits 6-7): `0`/`1`
/// wrap the 1024x1024 plane, `2` returns `None` (palette-0 /
/// transparent) outside it, `3` forces tile 0 outside. The return
/// is the raw 8-bit character byte, the renderer splits it into
/// colour / priority per the BG1 vs EXTBG-BG2 rules.
///
/// Mode 7 VRAM interleaves the two tables byte by byte, so with VRAM
/// word-addressed here the tilemap index is the **low** byte of
/// `vram[ty*128 + tx]` and the character pixel is the **high** byte
/// of `vram[tile*64 + (py & 7) * 8 + (px & 7)]`.
#[must_use]
pub fn mode7_sample(vram: &[u16; 0x8000], px: i32, py: i32, repeat: u8) -> Option<u8> {
    let outside = ((px | py) & !1023) != 0;
    let tile: u32 = match repeat {
        // Palette colour 0 outside the plane -> fully transparent.
        2 if outside => return None,
        // Character 0 repetition outside the plane.
        3 if outside => 0,
        // Inside (any mode) or wrap modes 0/1. The `& 1023` is a
        // no-op when already inside, so one branch serves both.
        _ => mode7_tile_at(vram, px & 1023, py & 1023),
    };
    // The character pixel always uses the low 3 bits of the raw
    // coordinate; `& 7` is invariant under the `& 1023` wrap, so the
    // forced-tile-0 branch reads the same pixel of tile 0.
    let cx = (px & 7) as u32;
    let cy = (py & 7) as u32;
    let pixel_word = vram[(tile * 64 + cy * 8 + cx) as usize];
    Some((pixel_word >> 8) as u8)
}

/// Read the Mode 7 tilemap byte (tile number) covering world
/// coordinate `(px, py)`, both pre-masked into `0..=1023`.
#[inline]
fn mode7_tile_at(vram: &[u16; 0x8000], px: i32, py: i32) -> u32 {
    let tx = ((px >> 3) & 127) as u32;
    let ty = ((py >> 3) & 127) as u32;
    u32::from(vram[(ty * 128 + tx) as usize] & 0x00FF)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vram() -> Box<[u16; 0x8000]> {
        vec![0u16; 0x8000].into_boxed_slice().try_into().unwrap()
    }

    #[test]
    fn tilemap_entry_decodes_full_layout() {
        // vhpppccccccccccc: vflip=1, hflip=1, prio=1, palette=5, tile=0x123
        // bits 15..0: 1 1 1 101 0100100011
        // nibbles   : 1111 0101 0010 0011 = 0xF523
        let word = 0b1111_0101_0010_0011u16;
        let entry = TilemapEntry::decode(word);
        assert_eq!(entry.tile, 0x123);
        assert_eq!(entry.palette, 5);
        assert!(entry.priority);
        assert!(entry.flip_h);
        assert!(entry.flip_v);
    }

    #[test]
    fn tilemap_entry_low_bits_only() {
        let entry = TilemapEntry::decode(0x0000);
        assert_eq!(entry.tile, 0);
        assert_eq!(entry.palette, 0);
        assert!(!entry.priority);
        assert!(!entry.flip_h);
        assert!(!entry.flip_v);
    }

    #[test]
    fn decode_tile_2bpp_row_round_trip() {
        let mut vram = make_vram();
        // Tile 0 row 0: plane0 = 0b10101010, plane1 = 0b11001100
        // Pixels left->right: bit 7 of each plane downto bit 0.
        // col 0: (1, 1) = 3
        // col 1: (0, 1) = 2
        // col 2: (1, 0) = 1
        // col 3: (0, 0) = 0
        // col 4: (1, 1) = 3
        // col 5: (0, 1) = 2
        // col 6: (1, 0) = 1
        // col 7: (0, 0) = 0
        vram[0] = 0b1100_1100_1010_1010;
        let row = decode_tile_2bpp_row(&vram, 0, 0, 0, false, false);
        assert_eq!(row, [3, 2, 1, 0, 3, 2, 1, 0]);
    }

    #[test]
    fn decode_tile_2bpp_row_flip_h_reverses() {
        let mut vram = make_vram();
        vram[0] = 0b1100_1100_1010_1010;
        let row = decode_tile_2bpp_row(&vram, 0, 0, 0, true, false);
        assert_eq!(row, [0, 1, 2, 3, 0, 1, 2, 3]);
    }

    #[test]
    fn decode_tile_2bpp_row_flip_v_picks_mirror_row() {
        let mut vram = make_vram();
        // Set distinguishable patterns in rows 0 and 7.
        vram[0] = 0x00FF; // row 0: plane0 = 0xFF, plane1 = 0x00 -> all 1s
        vram[7] = 0xFF00; // row 7: plane0 = 0x00, plane1 = 0xFF -> all 2s
        let row0 = decode_tile_2bpp_row(&vram, 0, 0, 0, false, false);
        assert_eq!(row0, [1; 8]);
        let row0_flipped = decode_tile_2bpp_row(&vram, 0, 0, 0, false, true);
        // With flip_v=true, row 0 fetches what's stored in row 7.
        assert_eq!(row0_flipped, [2; 8]);
    }

    #[test]
    fn decode_tile_4bpp_row_combines_planes() {
        let mut vram = make_vram();
        // Tile 0 row 0:
        // word 0 (planes 0+1): plane0=0xF0, plane1=0x0F -> pixels 0..3 = 1, 4..7 = 2
        // word 8 (planes 2+3): plane2=0xCC, plane3=0x33 -> cols 0/1=4, 2/3=8, 4/5=4+1+? wait
        // Let me reconsider. Each plane bit at column c contributes bit_value << plane_index.
        // word_lo = 0x0FF0 -> plane0 = 0xF0 (low byte), plane1 = 0x0F (high byte)
        // plane0 0xF0 = bits 7-4 set -> cols 0-3 get +1
        // plane1 0x0F = bits 3-0 set -> cols 4-7 get +2
        // word_hi = 0x33CC -> plane2 = 0xCC (low byte), plane3 = 0x33 (high byte)
        // plane2 0xCC = bits 7,6,3,2 -> cols 0,1,4,5 get +4
        // plane3 0x33 = bits 5,4,1,0 -> cols 2,3,6,7 get +8
        // Final: col0=1+4=5, col1=1+4=5, col2=1+8=9, col3=1+8=9,
        //        col4=2+4=6, col5=2+4=6, col6=2+8=10, col7=2+8=10
        vram[0] = 0x0FF0;
        vram[8] = 0x33CC;
        let row = decode_tile_4bpp_row(&vram, 0, 0, 0, false, false);
        assert_eq!(row, [5, 5, 9, 9, 6, 6, 10, 10]);
    }

    #[test]
    fn decode_tile_8bpp_row_combines_all_eight_planes() {
        let mut vram = make_vram();
        // Set every plane bit 7 (col 0 only) for tile 0 row 0.
        // word 0: planes 0+1 -> plane0 bit7=1, plane1 bit7=1 -> 0x8080
        // word 8: planes 2+3 -> 0x8080
        // word 16: planes 4+5 -> 0x8080
        // word 24: planes 6+7 -> 0x8080
        vram[0] = 0x8080;
        vram[8] = 0x8080;
        vram[16] = 0x8080;
        vram[24] = 0x8080;
        let row = decode_tile_8bpp_row(&vram, 0, 0, 0, false, false);
        assert_eq!(row[0], 0xFF);
        assert_eq!(&row[1..], &[0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn fetch_bg_tilemap_entry_size0_single_page() {
        let mut vram = make_vram();
        // screen_addr = 0x1000 (byte) -> word index 0x800.
        // pixel (16, 24) -> tile (2, 3) -> offset 3*32 + 2 = 98 -> word 0x862.
        vram[0x862] = 0x1234;
        let entry = fetch_bg_tilemap_entry(&vram, 0x1000, 0, 16, 24, false, false);
        assert_eq!(entry, TilemapEntry::decode(0x1234));
    }

    #[test]
    fn fetch_bg_tilemap_entry_size1_horizontal_wrap() {
        let mut vram = make_vram();
        // 64x32 layout. screen_addr = 0x0000.
        // Pixel x=256 = tile_x=32 -> tx & 0x20 -> +0x400 tile offset.
        // ty = 0 -> offset = 0 + 0 + 0x400 = 0x400 word index.
        vram[0x400] = 0xABCD;
        let entry = fetch_bg_tilemap_entry(&vram, 0x0000, 1, 256, 0, false, false);
        assert_eq!(entry, TilemapEntry::decode(0xABCD));
    }

    #[test]
    fn fetch_bg_tilemap_entry_size3_four_page() {
        let mut vram = make_vram();
        // 64x64 layout. screen_addr = 0x0000.
        // Pixel x=256 y=256 = tile_x=32 ty=32.
        // tx & 0x20 -> +0x400. ty & 0x20 + size==3 -> +0x800. Total +0xC00.
        // (ty & 0x1F)=0, (tx & 0x1F)=0 -> offset 0xC00.
        vram[0xC00] = 0xDEAD;
        let entry = fetch_bg_tilemap_entry(&vram, 0x0000, 3, 256, 256, false, false);
        assert_eq!(entry, TilemapEntry::decode(0xDEAD));
    }

    #[test]
    fn fetch_bg_tilemap_entry_hires_uses_16px_tile_stride() {
        // In hires the same tilemap entry covers 16 BG-pixels
        // because the column shift becomes `>> 4`. screen_x = 15
        // still resolves to tile column 0; screen_x = 16 moves to
        // tile column 1.
        let mut vram = make_vram();
        // screen_addr = 0x0000 (byte) -> word index 0.
        // Tile 0 = vram[0], tile 1 = vram[1] (size 0, ty=0 -> row
        // offset 0).
        vram[0] = 0x0001;
        vram[1] = 0x0002;
        let entry0 = fetch_bg_tilemap_entry(&vram, 0x0000, 0, 0, 0, true, false);
        let entry15 = fetch_bg_tilemap_entry(&vram, 0x0000, 0, 15, 0, true, false);
        let entry16 = fetch_bg_tilemap_entry(&vram, 0x0000, 0, 16, 0, true, false);
        assert_eq!(entry0.tile, 1, "screen_x=0 -> first 16-px slot");
        assert_eq!(
            entry15.tile, 1,
            "screen_x=15 still in first 16-px slot in hires"
        );
        assert_eq!(entry16.tile, 2, "screen_x=16 -> second 16-px slot");
    }

    #[test]
    fn fetch_bg_tilemap_entry_hires_size1_no_subpage_jump_in_visible_range() {
        // Hires size 1: mask_x = 0x3FF (1024-1) with `>> 4` stride
        // means tile column wraps at 64 (= screen_x 1024). In the
        // visible 0..511 range the high bit of tx never sets, so
        // the second 32-tile sub-page is unused (checked against
        // bsnes-plus: hires + size 1 still maps the first sub-page
        // across the full 512-px screen).
        let mut vram = make_vram();
        // Pin tile 31 (last entry of the first sub-page).
        vram[31] = 0xCAFE;
        // Plant a poison value in the second sub-page slot, should
        // NOT be touched in the visible range.
        vram[0x400] = 0xBADD;
        // screen_x = 511 -> tx = (511 & 0x3FF) >> 4 = 31, no bit 5.
        let entry = fetch_bg_tilemap_entry(&vram, 0x0000, 1, 511, 0, true, false);
        assert_eq!(entry, TilemapEntry::decode(0xCAFE));
    }

    #[test]
    fn fetch_bg_tilemap_entry_tile16_uses_16px_stride_both_axes() {
        // 16x16 tiles: each map entry spans 16 BG-px in X *and* Y, so the
        // column shift is `>> 4` and the row shift is `>> 4`. Size 0,
        // screen_addr 0 -> word index = ty*32 + tx.
        let mut vram = make_vram();
        vram[0] = 0x0010; // tile column 0, row 0
        vram[1] = 0x0011; // tile column 1, row 0
        vram[32] = 0x0012; // tile column 0, row 1
                           // X stride: 0..15 -> column 0, 16..31 -> column 1.
        let e0 = fetch_bg_tilemap_entry(&vram, 0x0000, 0, 0, 0, false, true);
        let e15 = fetch_bg_tilemap_entry(&vram, 0x0000, 0, 15, 0, false, true);
        let e16 = fetch_bg_tilemap_entry(&vram, 0x0000, 0, 16, 0, false, true);
        assert_eq!(e0, TilemapEntry::decode(0x0010));
        assert_eq!(e15, TilemapEntry::decode(0x0010), "x=15 still first 16-px");
        assert_eq!(e16, TilemapEntry::decode(0x0011), "x=16 -> second 16-px");
        // Y stride: 0..15 -> row 0, 16 -> row 1.
        let e_y15 = fetch_bg_tilemap_entry(&vram, 0x0000, 0, 0, 15, false, true);
        let e_y16 = fetch_bg_tilemap_entry(&vram, 0x0000, 0, 0, 16, false, true);
        assert_eq!(
            e_y15,
            TilemapEntry::decode(0x0010),
            "y=15 still first 16-px"
        );
        assert_eq!(
            e_y16,
            TilemapEntry::decode(0x0012),
            "y=16 -> second 16-px row"
        );
    }

    #[test]
    fn hires_adjacent_tile_offset_picks_second_half_above_bit3() {
        assert_eq!(hires_adjacent_tile_offset(0, false), 0);
        assert_eq!(hires_adjacent_tile_offset(7, false), 0);
        assert_eq!(hires_adjacent_tile_offset(8, false), 1);
        assert_eq!(hires_adjacent_tile_offset(15, false), 1);
        assert_eq!(hires_adjacent_tile_offset(16, false), 0);
    }

    #[test]
    fn hires_adjacent_tile_offset_flips_halves_when_flip_h() {
        // flip_h reverses which 8-px half is "first" so a flipped
        // 16-px slot still renders left-to-right consistently.
        assert_eq!(hires_adjacent_tile_offset(0, true), 1);
        assert_eq!(hires_adjacent_tile_offset(7, true), 1);
        assert_eq!(hires_adjacent_tile_offset(8, true), 0);
    }

    #[test]
    fn compute_offset_per_tile_mode4_bg1_no_override_when_invalid_mask() {
        let mut vram = make_vram();
        // BG3 tile at tx=0, ty=0 has hval=0x0100 (no valid bit set).
        vram[0] = 0x0100;
        let (h, v) = compute_offset_per_tile(
            &vram, 0x0000, 0, 0, 0, 4, 0, 8, // tile_left_x = 8 -> offset_x = 10
            10, 20, 100,
        );
        // No override -> absolute fallback: (tile_left_x + base_hoffset,
        // scanline_y + base_voffset) = (8 + 10, 100 + 20).
        assert_eq!((h, v), (18, 120));
    }

    #[test]
    fn compute_offset_per_tile_mode4_bg1_h_override() {
        let mut vram = make_vram();
        // BG3 tile at tx=0, ty=0 holds 0x2008: the flag for BG1 (bit
        // 13), bit 15 clear (a horizontal replacement), offset 8.
        vram[0] = 0x2008;
        let (h, v) = compute_offset_per_tile(
            &vram, 0x0000, 0, 0, 0, 4, 0, 8, // tile_left_x = 8
            0, // base_hoffset = 0 -> offset_x = 8 (clean math)
            20, 100,
        );
        // hoffset = the tile's position (8) + the entry's offset (8).
        assert_eq!(h, 16);
        // V has no override here -> absolute fallback scanline_y(100) +
        // base_voffset(20) = 120.
        assert_eq!(v, 120);
    }

    #[test]
    fn compute_offset_per_tile_mode4_bg1_v_override_when_bit15_set() {
        let mut vram = make_vram();
        // bit 15 set -> a vertical replacement; 0xA005 = the flag for
        // BG1 + bit 15 + offset 5.
        vram[0] = 0xA005;
        let (h, v) = compute_offset_per_tile(
            &vram, 0x0000, 0, 0, 0, 4, 0, 8, // tile_left_x = 8
            0, // base_hoffset = 0 -> offset_x = 8
            20, 100,
        );
        // V branch fires; H has no override -> absolute fallback
        // tile_left_x(8) + base_hoffset(0) = 8.
        assert_eq!(h, 8);
        assert_eq!(v, 100 + 5);
    }

    #[test]
    fn compute_offset_per_tile_mode2_uses_separate_vval() {
        let mut vram = make_vram();
        // BG3 voffset = 0 -> the H entry at row 0, the V entry at row 8.
        // H entry 0x2008: the flag for BG1, offset 8. V entry 0x2100:
        // the flag for BG1, offset 0x100.
        vram[0] = 0x2008;
        vram[0x20] = 0x2100; // tile (0,1) at word offset 32
        let (h, v) = compute_offset_per_tile(
            &vram, 0x0000, 0, 0, 0, 2, 0, 8, // tile_left_x = 8
            0, // base_hoffset = 0 -> offset_x = 8
            20, 100,
        );
        // hoffset = the tile's position (8) + the entry's offset (8).
        assert_eq!(h, 16);
        // voffset = scanline_y (100) + the V entry's offset (0x100).
        assert_eq!(v, 100 + 0x100);
    }

    #[test]
    fn compute_offset_per_tile_first_column_unchanged() {
        let vram = make_vram();
        let (h, v) = compute_offset_per_tile(
            &vram, 0x0000, 0, 0, 0, 4, 0, 0, // tile_left_x = 0
            10, 20, 100,
        );
        // offset_x = 0 + (10 & 7) = 2 < 8 -> no override -> absolute
        // fallback (tile_left_x + base_hoffset, scanline_y + base_voffset)
        // = (0 + 10, 100 + 20).
        assert_eq!((h, v), (10, 120));
    }

    #[test]
    fn compute_offset_per_tile_mode6_reads_bg3_source_at_hires_stride() {
        // Mode 6 is hires: every BG in Modes 5/6 is read on a 16-px
        // tile stride over the doubled 512-px space, BG3 as the offset
        // source included, unlike the non-hires Modes 2/4 (8-px
        // stride). For a source x of 64 that selects BG3 tile column 4
        // (64 >> 4) in Mode 6 but column 8 (64 >> 3) in Mode 2. Plant a
        // distinct H replacement in each column and confirm each mode
        // reads its own, proving the stride actually differs (at the
        // 8-px stride, Mode 6 would lose its offset-per-tile).
        let mut vram = make_vram();
        vram[4] = 0x2080; // BG3 col 4 -> Mode 6 (hires) reads this
        vram[8] = 0x2010; // BG3 col 8 -> Mode 2 (non-hires) reads this
                          // tile_left_x = 72, base_hoffset = 0 -> offset_x = 72,
                          // bg3_lookup_x = 72 - 8 = 64.
        let run =
            |mode: u8| compute_offset_per_tile(&vram, 0x0000, 0, 0, 0, mode, 0, 72, 0, 20, 100);
        let (h6, _) = run(6);
        let (h2, _) = run(2);
        assert_eq!(h6, 72 + 0x80, "Mode 6 reads BG3 col 4 (16-px hires stride)");
        assert_eq!(h2, 72 + 0x10, "Mode 2 reads BG3 col 8 (8-px stride)");
        assert_ne!(h6, h2, "hires vs non-hires BG3 stride must diverge");
    }

    // -- Mode 7 helpers ----------------------------------------

    #[test]
    fn sign_extend_13_handles_sign_bit() {
        assert_eq!(sign_extend_13(0x0000), 0);
        assert_eq!(sign_extend_13(0x0FFF), 4095); // max positive
        assert_eq!(sign_extend_13(0x1000), -4096); // sign bit only
        assert_eq!(sign_extend_13(0x1FFF), -1);
        // Bits above the low 13 are ignored.
        assert_eq!(sign_extend_13(0xFFFF), -1);
    }

    #[test]
    fn mode7_clip_keeps_low_10_bits_with_bit13_sign() {
        assert_eq!(mode7_clip(0), 0);
        assert_eq!(mode7_clip(0x3FF), 0x3FF); // max positive magnitude
        assert_eq!(mode7_clip(0x400), 0); // bit 10 discarded, no sign
        assert_eq!(mode7_clip(0x2000), -1024); // sign set -> 0x2000 | ~0x3FF
        assert_eq!(mode7_clip(0x23FF), -1); // sign + full magnitude
        assert_eq!(mode7_clip(-1), -1); // negative input keeps its sign
    }

    #[test]
    fn mode7_sample_inside_reads_tile_then_pixel() {
        let mut vram = make_vram();
        // Tilemap (0,0) = tile 5 (low byte of word 0).
        vram[0] = 0x0005;
        // Tile 5 pixel (cx=0, cy=0) = high byte of word 5*64 = 320.
        vram[320] = 0xAB00;
        assert_eq!(mode7_sample(&vram, 0, 0, 0), Some(0xAB));
    }

    #[test]
    fn mode7_sample_wrap_mode_wraps_outside_coords() {
        let mut vram = make_vram();
        vram[0] = 0x0005;
        vram[320] = 0xAB00;
        // px = 1024 wraps back to 0 under repeat mode 0.
        assert_eq!(mode7_sample(&vram, 1024, 0, 0), Some(0xAB));
    }

    #[test]
    fn mode7_sample_repeat2_is_transparent_outside() {
        let mut vram = make_vram();
        vram[320] = 0xAB00;
        vram[0] = 0x0005;
        assert_eq!(mode7_sample(&vram, 1024, 0, 2), None);
        // Inside the plane the same mode samples normally.
        assert_eq!(mode7_sample(&vram, 0, 0, 2), Some(0xAB));
    }

    #[test]
    fn mode7_sample_repeat3_forces_tile0_outside() {
        let mut vram = make_vram();
        // Poison the tilemap so a wrap (repeat 0) would pick tile 2,
        // proving repeat 3 ignores it and uses tile 0.
        vram[0] = 0x0002;
        // Tile 0 pixel (cx=0, cy=1) = high byte of word 0*64+1*8 = 8.
        vram[8] = 0xEF00;
        // px=1024 (cx=0), py=1025 (cy=1) -> outside -> tile 0.
        assert_eq!(mode7_sample(&vram, 1024, 1025, 3), Some(0xEF));
    }

    /// Drift guard for the platform's TypeScript port of this sampler:
    /// identical hand-computed VRAM layout, identical expected byte, so
    /// the two implementations cannot silently diverge.
    /// Cell (tx=1, ty=2) -> tile 7; tile 7 pixel (cx=3, cy=4) -> 0xAB; the
    /// world coord is therefore (px=11, py=20).
    #[test]
    fn mode7_sample_parity_golden() {
        let mut vram = make_vram();
        // Tilemap word for cell (1, 2): low byte = tile number.
        vram[2 * 128 + 1] = 0x0007;
        // Tile 7 pixel (cx=3, cy=4): high byte = colour index.
        vram[7 * 64 + 4 * 8 + 3] = 0xAB00;
        assert_eq!(mode7_sample(&vram, 11, 20, 0), Some(0xAB));
    }

    // -- Tilemap-row cache helpers -----------------------------

    #[test]
    fn tilemap_masks_per_size_and_hires() {
        // Non-hires, 8x8: bit 0 doubles X (0xFF -> 0x1FF), bit 1 doubles Y.
        assert_eq!(tilemap_masks(0, false, false), (0x00FF, 0x00FF));
        assert_eq!(tilemap_masks(1, false, false), (0x01FF, 0x00FF));
        assert_eq!(tilemap_masks(2, false, false), (0x00FF, 0x01FF));
        assert_eq!(tilemap_masks(3, false, false), (0x01FF, 0x01FF));
        // Hires doubles the X base only; Y is unchanged.
        assert_eq!(tilemap_masks(0, true, false), (0x01FF, 0x00FF));
        assert_eq!(tilemap_masks(1, true, false), (0x03FF, 0x00FF));
        assert_eq!(tilemap_masks(3, true, false), (0x03FF, 0x01FF));
    }

    #[test]
    fn tilemap_masks_tile16_doubles_both_axes() {
        // 16x16 tiles double the base mask on BOTH axes (one 32x32 page
        // spans 512x512 BG-px), and the size bits then double again.
        assert_eq!(tilemap_masks(0, false, true), (0x01FF, 0x01FF));
        assert_eq!(tilemap_masks(1, false, true), (0x03FF, 0x01FF));
        assert_eq!(tilemap_masks(2, false, true), (0x01FF, 0x03FF));
        assert_eq!(tilemap_masks(3, false, true), (0x03FF, 0x03FF));
    }

    #[test]
    fn build_tilemap_row_matches_fetch_per_column() {
        let mut vram = make_vram();
        // Seed a recognisable pattern across the first tile row of a
        // size-1 (64-wide) page so every column is distinct.
        for tx in 0u16..64 {
            // screen_addr 0 -> word index = tx (+ 0x400 for tx >= 32).
            let word_index = if tx & 0x20 != 0 {
                0x400 + (tx & 0x1F)
            } else {
                tx
            };
            vram[word_index as usize] = 0x1000 + tx;
        }
        let row = build_tilemap_row(&vram, 0x0000, 1, 0);
        // Every cached entry must equal the direct fetch for a
        // screen_x landing in that tile column.
        for tx in 0u16..64 {
            let screen_x = tx << 3;
            let expected = fetch_bg_tilemap_entry(&vram, 0x0000, 1, screen_x, 0, false, false);
            assert_eq!(row[tx as usize], expected, "column {tx}");
        }
    }

    #[test]
    fn build_tilemap_row_includes_horizontal_subpage_jump() {
        let mut vram = make_vram();
        // Size 1: tile column 32 (tx bit 5) jumps to the second
        // horizontal sub-page at word offset 0x400.
        vram[0x400] = 0xBEEF;
        let row = build_tilemap_row(&vram, 0x0000, 1, 0);
        assert_eq!(row[32], TilemapEntry::decode(0xBEEF));
    }

    #[test]
    fn build_tilemap_row_size3_vertical_subpage() {
        let mut vram = make_vram();
        // Size 3 (64x64): tile row 32 (ty bit 5) jumps +0x800.
        // (ty & 0x1F) = 0, tx = 0 -> word index 0x800.
        vram[0x800] = 0xF00D;
        let row = build_tilemap_row(&vram, 0x0000, 3, 32);
        assert_eq!(row[0], TilemapEntry::decode(0xF00D));
    }
}
