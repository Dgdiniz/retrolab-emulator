//! Pure sprite (OBJ) evaluation helpers for the per-scanline
//! renderer.
//!
//! Like [`crate::ppu::tile`], every function is `&self`-free, they
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
