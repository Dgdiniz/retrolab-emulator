//! One background layer's row for the per-scanline renderer.
//!
//! [`Ppu::render_bg_row`] fetches a tile BG's pixels for a scanline
//! into a row buffer, in Modes 0-6: the 256-wide normal path and the
//! 512-wide hires path of Modes 5/6, 8x8 and 16x16 tiles,
//! offset-per-tile in Modes 2/4/6, and direct colour. In Modes 0, 1
//! and 3 with 8x8 tiles it reads each tile band's tilemap entries
//! through the cross-scanline row cache ([`Ppu::cached_tilemap_row`],
//! coherence in [`crate::ppu::BgTilemapRowCache`]).
//! [`Ppu::apply_mosaic_for_bg`] then applies the horizontal and
//! vertical mosaic to a BG's row, on either screen. The Mode 7 plane
//! fills the same row buffers from `mode7.rs`.

use retro_core::capture::{tile_value, PixelTrace};

use super::picture::WovenRow;
use super::render::{
    bpp_for_bg, cgram_index, direct_color_word, is_hires_mode, palette_offset_for_bg, BgRowBuffer,
    BgRowPixel, Screen, MAX_SCANLINE_WIDTH, NORMAL_SCANLINE_WIDTH,
};
use super::tile::{
    bg_entry_character, bg_tilemap_word_index, build_tilemap_row, compute_offset_per_tile,
    decode_tile_2bpp_row, decode_tile_4bpp_row, decode_tile_8bpp_row, fetch_bg_tilemap_entry,
    offset_per_tile, tilemap_masks, TilemapEntry,
};
use super::trace::{colour_value, pixel_layer, BgOrigin, FrameTrace};
use super::Ppu;

impl Ppu {
    /// Background `bg`'s pixels take direct colour: BG1's 8-bit characters
    /// in Modes 3 and 4 while CGWSEL's direct colour is on.
    pub(super) fn bg_direct_colour(&self, bg: usize) -> bool {
        bg == 0
            && (self.bgmode == 3 || self.bgmode == 4)
            && self.direct_color
            && bpp_for_bg(self.bgmode, bg) == 8
    }

    /// Fill a row buffer with a single BG's tile output. `hires`
    /// selects between the 256-wide normal path (Modes 0-4) and
    /// the 512-wide hires path (Modes 5/6); in hires every tilemap
    /// entry covers 16 BG-pixels and is paired with its right-side
    /// neighbour via [`hires_adjacent_tile_offset`].
    pub(super) fn render_bg_row<T: PixelTrace<Notes = FrameTrace>>(
        &mut self,
        bg_id: usize,
        scanline: u16,
        row: &mut BgRowBuffer,
        hires: bool,
        trace: &mut T,
    ) {
        let bpp = bpp_for_bg(self.bgmode, bg_id);
        let palette_offset = palette_offset_for_bg(self.bgmode, bg_id);
        let base_hoffset = self.bg_hoffset[bg_id];
        let base_voffset = self.bg_voffset[bg_id];
        // Mode 5/6 fetch the BG on the 512-dot hires grid (`x` runs
        // 0..511), so the horizontal scroll is doubled to live in that
        // space (the scroll register counts the BG's own pixels). At
        // scroll 0 (e.g. RPM Racing's static logo) this is a no-op; once
        // the camera scrolls an undoubled value would run the BG at half
        // speed, breaking the track and sliding sprites onto the grass.
        // Vertical scroll is unaffected, the interlace weave already
        // supplies the doubled `vy`. The offset-per-tile path (Mode 6)
        // consumes the same doubled value (`opt_x`/`hoffset`).
        let h_scroll = if hires {
            base_hoffset << 1
        } else {
            base_hoffset
        };
        let screen_addr = self.bg_screen_addr[bg_id];
        let screen_size = self.bg_screen_size[bg_id];
        let tile_data_addr = self.bg_tiledata_addr[bg_id];
        // 16x16-tile BGs (`$2105` bits 4-7). Each tilemap entry then
        // covers a 2x2 block of 8-px characters; the renderer fetches the
        // entry on a 16-px grid and picks the right/bottom quadrant below.
        // Honoured in every mode, hires included: a Mode 5/6 BG with the
        // size bit set (the PPU bus activity test's Layer 1, Air Strike
        // Patrol's menu text) lays its tilemap out as 16x16 blocks across
        // the 512-dot grid. The 16-px horizontal pick coincides with
        // `hires_adjacent_tile_offset`
        // (`tile` for the left 8 px, `tile+1` for the right), so the two
        // are mutually exclusive per BG: an 8x8 hires BG keeps taking the
        // `else if hires` arm below, a 16x16 one takes the `tile16` arm
        // which additionally selects the bottom characters (`+16`) the
        // 8x8-only hires path can never reach. With interlace the doubled
        // `screen_y` makes that bottom-char pick follow the field
        // (`py = (y << 1) | field`).
        let tile16 = self.bg_tile_size[bg_id];
        let direct_color = self.bg_direct_colour(bg_id);
        let row_width: u16 = if hires {
            MAX_SCANLINE_WIDTH as u16
        } else {
            NORMAL_SCANLINE_WIDTH as u16
        };
        let (mask_x, mask_y) = tilemap_masks(screen_size, hires, tile16);

        // Tilemap-row cache (Modes 0/1/3). Offset-per-tile (2/4/6) has
        // a per-column voffset, hires (5/6) a 16-px stride, and 16x16
        // tiles span two 8-px tile rows per entry, so all three keep the
        // direct fetch. For the cached modes the voffset is constant
        // across the scanline, so the whole row sits in a single tile
        // band and one cached row serves all 8 lines.
        let use_cache = !hires && !tile16 && !matches!(self.bgmode, 2 | 4 | 6);
        let cached_row = if use_cache {
            let screen_y = scanline.wrapping_add(base_voffset);
            let ty = (screen_y & mask_y) >> 3;
            Some(self.cached_tilemap_row(bg_id, screen_addr, screen_size, ty))
        } else {
            None
        };

        // Per-scanline decoded-tile memo ("char row" reuse). Runs of
        // the same (tile, flips, row_in_tile), solid fills, repeated
        // patterns, decode once. Local to this call, so it reads live
        // VRAM and needs no invalidation.
        let mut last_decode: Option<(u16, bool, bool, u8, [u8; 8])> = None;

        // Resolve per-pixel by walking the output columns. We
        // group on tile boundaries inside the loop so the tilemap
        // entry + decoded row are reused for each 8-pixel block
        // (and to make the offset-per-tile lookup cheap).
        let mut x: u16 = 0;
        while x < row_width {
            // Per-tile-column offset override (Modes 2/4/6). Mode 6
            // is hires with offset-per-tile; it reuses the same
            // algorithm as Mode 2 (`compute_offset_per_tile` already
            // documents that the BG3 lookup is identical).
            // Tile coordinates within the BG's virtual screen, in
            // *absolute* space. For the offset-per-tile modes
            // `compute_offset_per_tile` already folds the base scroll into
            // the column / scanline; the other modes
            // add it here. Either branch yields absolute coordinates that
            // must not be offset by `x` / `scanline` again.
            let mut offset_replaced = false;
            let (screen_x, screen_y) = if matches!(self.bgmode, 2 | 4 | 6) {
                let bg3_screen_addr = self.bg_screen_addr[2];
                let bg3_screen_size = self.bg_screen_size[2];
                let bg3_hoffset = self.bg_hoffset[2];
                let bg3_voffset = self.bg_voffset[2];
                if T::ON {
                    let scroll = offset_per_tile(
                        self.vram(),
                        bg3_screen_addr,
                        bg3_screen_size,
                        bg3_hoffset,
                        bg3_voffset,
                        self.bgmode,
                        bg_id as u8,
                        x,
                        h_scroll,
                        base_voffset,
                        scanline,
                    );
                    offset_replaced = scroll.horizontal_replaced || scroll.vertical_replaced;
                    (scroll.hoffset, scroll.voffset)
                } else {
                    compute_offset_per_tile(
                        self.vram(),
                        bg3_screen_addr,
                        bg3_screen_size,
                        bg3_hoffset,
                        bg3_voffset,
                        self.bgmode,
                        bg_id as u8,
                        x,
                        h_scroll,
                        base_voffset,
                        scanline,
                    )
                }
            } else {
                (
                    x.wrapping_add(h_scroll),
                    scanline.wrapping_add(base_voffset),
                )
            };

            // Cached modes index the prebuilt row by absolute tile
            // column; the others fetch directly. Both produce the same
            // entry, `build_tilemap_row` is built from this very
            // function (see `BgTilemapRowCache`).
            let entry = match cached_row {
                Some(ref entries) => entries[((screen_x & mask_x) >> 3) as usize],
                None => fetch_bg_tilemap_entry(
                    self.vram(),
                    screen_addr,
                    screen_size,
                    screen_x,
                    screen_y,
                    hires,
                    tile16,
                ),
            };

            // The 8-px character the entry names here: a 16x16 entry's
            // quadrant, an 8x8 hires entry's half of its 16-px slot.
            let effective_tile = bg_entry_character(entry, screen_x, screen_y, hires, tile16);

            let row_in_tile = (screen_y & 7) as u8;
            // The map entry's VRAM word, for a traced redraw's notes.
            let cell = if T::ON {
                bg_tilemap_word_index(screen_addr, screen_size, screen_x, screen_y, hires, tile16)
                    as u16
            } else {
                0
            };
            let pixels = match last_decode {
                Some((t, fh, fv, r, px))
                    if t == effective_tile
                        && fh == entry.flip_h
                        && fv == entry.flip_v
                        && r == row_in_tile =>
                {
                    px
                }
                _ => {
                    let decoded = match bpp {
                        2 => decode_tile_2bpp_row(
                            self.vram(),
                            effective_tile,
                            tile_data_addr,
                            row_in_tile,
                            entry.flip_h,
                            entry.flip_v,
                        ),
                        4 => decode_tile_4bpp_row(
                            self.vram(),
                            effective_tile,
                            tile_data_addr,
                            row_in_tile,
                            entry.flip_h,
                            entry.flip_v,
                        ),
                        _ => decode_tile_8bpp_row(
                            self.vram(),
                            effective_tile,
                            tile_data_addr,
                            row_in_tile,
                            entry.flip_h,
                            entry.flip_v,
                        ),
                    };
                    last_decode = Some((
                        effective_tile,
                        entry.flip_h,
                        entry.flip_v,
                        row_in_tile,
                        decoded,
                    ));
                    decoded
                }
            };

            // Skip into the tile to consume the fractional scroll
            // (`screen_x & 7` pixels into the decoded row).
            let skip = (screen_x & 7) as usize;
            // Where this character's pixels start on the line: a traced
            // redraw works out each pixel's place in the decoded row from it.
            let first_x = x;
            for &pixel in &pixels[skip..8] {
                if x >= row_width {
                    break;
                }
                if pixel == 0 {
                    row[x as usize] = BgRowPixel {
                        color: 0,
                        opaque: false,
                        priority: entry.priority,
                    };
                } else {
                    let color = if direct_color {
                        direct_color_word(pixel, entry.palette)
                    } else {
                        let cgram_idx = cgram_index(palette_offset, entry.palette, pixel, bpp);
                        self.cgram()[cgram_idx as usize]
                    };
                    row[x as usize] = BgRowPixel {
                        color,
                        opaque: true,
                        priority: entry.priority,
                    };
                    if T::ON {
                        // The decoded row is in screen order: a flipped
                        // character is read from its far column.
                        let column = skip + usize::from(x - first_x);
                        let read_column = if entry.flip_h { 7 - column } else { column };
                        let read_row = if entry.flip_v {
                            7 - row_in_tile
                        } else {
                            row_in_tile
                        };
                        let character = u32::from(effective_tile & 0x03FF);
                        let start =
                            (tile_data_addr >> 1).wrapping_add(character * u32::from(bpp) * 4);
                        let index = if direct_color {
                            pixel
                        } else {
                            cgram_index(palette_offset, entry.palette, pixel, bpp)
                        };
                        let mut flags = 0;
                        if direct_color {
                            flags |= pixel_layer::DIRECT_COLOUR;
                        }
                        if entry.priority {
                            flags |= 1 << pixel_layer::PRIORITY_SHIFT;
                        }
                        if offset_replaced {
                            flags |= pixel_layer::OFFSET_REPLACED;
                        }
                        trace.notes().fetch[bg_id][x as usize] = BgOrigin {
                            tile: tile_value(
                                start,
                                character,
                                u32::from(read_row),
                                read_column as u32,
                            ),
                            cell,
                            colour: colour_value(index, entry.palette, bpp),
                            flags,
                        };
                    }
                }
                x = x.wrapping_add(1);
            }
        }
    }

    /// Look up, or rebuild, the cached tilemap row for `bg_id` at
    /// tile row `ty`, returning a copy of the 64 entries. Invoked only
    /// for the cache-eligible modes (0/1/3). The returned array is
    /// `Copy`, so the `&mut self` borrow ends here and the caller can
    /// freely re-borrow `self.vram()` while indexing the row. See
    /// [`crate::ppu::BgTilemapRowCache`] for the coherence contract.
    fn cached_tilemap_row(
        &mut self,
        bg_id: usize,
        screen_addr: u32,
        screen_size: u8,
        ty: u16,
    ) -> [TilemapEntry; 64] {
        let generation = self.vram_generation;
        if !self.bg_tilemap_row_cache[bg_id].matches(generation, screen_addr, screen_size, ty) {
            let entries = build_tilemap_row(self.vram(), screen_addr, screen_size, ty);
            self.bg_tilemap_row_cache[bg_id].store(
                generation,
                screen_addr,
                screen_size,
                ty,
                entries,
            );
        }
        *self.bg_tilemap_row_cache[bg_id].entries()
    }

    /// Apply both horizontal + vertical mosaic for one BG.
    ///
    /// The hardware's V-mosaic counter starts at 1 on
    /// scanline 1 and is decremented every line; when it hits 0 it
    /// resets to `mosaic_size` and the current
    /// row becomes the new V-mosaic source. The equivalent
    /// stateless formulation is `(scanline - 1) % N == 0`, fresh
    /// at lines 1, 1+N, 1+2N, ...; replayed everywhere else. The
    /// `scanline % N == 0` formulation would mis-classify line 1
    /// (the first visible line) as "replay" and pull the empty
    /// previous-row buffer through, which manifests as black BGs
    /// for the first mosaic block of the frame. The lines count from the
    /// first background line the frame draws: line 1, or in a woven hires
    /// frame, whose rows fetch the doubled frame's lines, line 2, so the
    /// blocks start at the frame's first row either way.
    ///
    /// The size N is `(($2106 >> 4) & 0xF)
    /// + 1`, stored already-adjusted in [`Ppu::mosaic_size`] (range
    /// `1..=16`). Size 1 is a no-op even when the enable bit is set.
    ///
    /// `width` lets the same path serve both 256-wide (non-hires)
    /// and 512-wide (hires) rows. The vertical replay copies the
    /// full 512-wide previous-row buffer regardless so the cold
    /// tail stays consistent if the mode flips mid-frame.
    ///
    /// `screen` names the row's screen: the mosaic is the background's,
    /// the same on both, and each screen replays its own rows of the
    /// line before (`previous_bg_rows`, `previous_sub_rows`). A sub screen
    /// row whose line before kept none (`sub_rows_kept`) starts a block.
    pub(super) fn apply_mosaic_for_bg<T: PixelTrace<Notes = FrameTrace>>(
        &mut self,
        screen: Screen,
        bg_id: usize,
        scanline: u16,
        row: &mut BgRowBuffer,
        width: usize,
        trace: &mut T,
    ) {
        if !self.bg_mosaic_enable[bg_id] || self.mosaic_size <= 1 {
            return;
        }
        let n = self.mosaic_size as u16;
        let first_line = if self.render_interlace {
            WovenRow::first_background_line(is_hires_mode(self.bgmode))
        } else {
            1
        };
        // The sub screen starts a block too where its line before kept
        // no rows to replay.
        let is_fresh_row = scanline.wrapping_sub(first_line).is_multiple_of(n)
            || (screen == Screen::Sub && !self.sub_rows_kept);
        if !is_fresh_row {
            *row = match screen {
                Screen::Main => self.previous_bg_rows[bg_id],
                Screen::Sub => self.previous_sub_rows[bg_id],
            };
            if T::ON {
                trace.notes().repeat_previous(screen, bg_id);
            }
            return;
        }
        // H-mosaic on the fresh row: replicate the leftmost pixel
        // of each N-wide block over its siblings.
        let n_us = n as usize;
        let mut x = 0usize;
        while x < width {
            let src = row[x];
            let end = (x + n_us).min(width);
            for slot in &mut row[x..end] {
                *slot = src;
            }
            if T::ON {
                trace.notes().repeat_block(screen, bg_id, x, end);
            }
            x += n_us;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::capture::NoTrace;

    // -- Tilemap-row cache -------------------------------------

    /// Mode 1 BG1 (4bpp) with two distinct tiles in tile-row 0
    /// (columns 0/1) and a third tile in tile-row 1 (column 0), plus a
    /// palette. `bg_voffset = 0xFFFF` so `scanline` maps to
    /// `screen_y = scanline - 1` (tile row 0 covers scanlines 1..=8).
    fn mode1_cache_ppu() -> Ppu {
        let mut ppu = Ppu::new();
        ppu.bgmode = 1;
        ppu.bg_screen_addr[0] = 0x0000; // tilemap word base 0
        ppu.bg_tiledata_addr[0] = 0x2000; // char word base 0x1000
        ppu.bg_screen_size[0] = 0;
        ppu.bg_voffset[0] = 0xFFFF;

        // Tilemap: (col0,row0)=tile1, (col1,row0)=tile2, (col0,row1)=tile2.
        ppu.vram[0] = 0x0001;
        ppu.vram[1] = 0x0002;
        ppu.vram[32] = 0x0002;

        // Tile 1 row 0: plane0 = 0xFF -> every pixel = colour index 1.
        ppu.vram[0x1000 + 16] = 0x00FF;
        // Tile 2 row 0: plane1 = 0xFF -> every pixel = colour index 2.
        ppu.vram[0x1000 + 32] = 0xFF00;

        ppu.cgram[1] = 0x1111;
        ppu.cgram[2] = 0x2222;
        ppu
    }

    /// Fill all 8 rows of a 4bpp character (char word base 0x1000) with
    /// the same plane0/1 and plane2/3 words, so its colour index is
    /// constant across every row and column, handy when a flip would
    /// otherwise pick an unpopulated row/column.
    fn fill_solid_4bpp_tile(ppu: &mut Ppu, tile: usize, plane01: u16, plane23: u16) {
        let base = 0x1000 + tile * 16;
        for r in 0..8 {
            ppu.vram[base + r] = plane01;
            ppu.vram[base + 8 + r] = plane23;
        }
    }

    #[test]
    fn render_bg_row_cache_indexes_columns_correctly() {
        let mut ppu = mode1_cache_ppu();
        let mut row = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        // Scanline 1 -> screen_y 0 -> tile row 0.
        ppu.render_bg_row(0, 1, &mut row, false, &mut NoTrace::<FrameTrace>::new());
        // Column 0 (x 0..7) = tile 1 = colour index 1 = cgram[1].
        assert!(row[0].opaque && row[7].opaque);
        assert_eq!(row[0].color, 0x1111);
        assert_eq!(row[7].color, 0x1111);
        // Column 1 (x 8..15) = tile 2 = colour index 2 = cgram[2].
        assert_eq!(row[8].color, 0x2222);
        assert_eq!(row[15].color, 0x2222);
    }

    #[test]
    fn render_bg_row_cache_hit_matches_miss_and_forced_rebuild() {
        let mut ppu = mode1_cache_ppu();
        let mut cold = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        let mut warm = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        let mut rebuilt = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];

        // First render builds the cache (cold miss).
        ppu.render_bg_row(0, 1, &mut cold, false, &mut NoTrace::<FrameTrace>::new());
        // Second render of the same scanline hits the warm cache.
        ppu.render_bg_row(0, 1, &mut warm, false, &mut NoTrace::<FrameTrace>::new());
        // Forcing a miss must reproduce the identical output.
        ppu.invalidate_tilemap_caches();
        ppu.render_bg_row(0, 1, &mut rebuilt, false, &mut NoTrace::<FrameTrace>::new());

        assert_eq!(&cold[..256], &warm[..256], "cache hit diverged from build");
        assert_eq!(
            &cold[..256],
            &rebuilt[..256],
            "forced rebuild diverged from build"
        );
    }

    #[test]
    fn render_bg_row_cache_rebuilds_on_tile_row_change() {
        let mut ppu = mode1_cache_ppu();
        let mut band0 = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        let mut band1 = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        // Scanline 1 -> tile row 0 -> column 0 = tile 1 = cgram[1].
        ppu.render_bg_row(0, 1, &mut band0, false, &mut NoTrace::<FrameTrace>::new());
        // Scanline 9 -> screen_y 8 -> tile row 1 -> column 0 = tile 2 =
        // cgram[2]. A stale ty-0 row would still report cgram[1].
        ppu.render_bg_row(0, 9, &mut band1, false, &mut NoTrace::<FrameTrace>::new());
        assert_eq!(band0[0].color, 0x1111);
        assert_eq!(band1[0].color, 0x2222, "ty rollover must rebuild the row");
    }

    #[test]
    fn render_bg_row_offset_mode_bypasses_cache() {
        let mut ppu = mode1_cache_ppu();
        ppu.bgmode = 2; // offset-per-tile -> direct fetch, no caching
        let mut row = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        ppu.render_bg_row(0, 1, &mut row, false, &mut NoTrace::<FrameTrace>::new());
        assert!(
            !ppu.bg_tilemap_row_cache[0].valid,
            "offset-per-tile must not populate the tilemap-row cache"
        );
    }

    /// Mode 1 BG1 with 16x16 tiles: a single tilemap entry must expand to
    /// the 2x2 block of 8-px characters TL=N, TR=N+1, BL=N+16, BR=N+17.
    /// Contra III's Konami logo needs it: BG1 there is a 16x16 layer
    /// that, (mis)read as 8x8, paints opaque garbage (a pink band + a
    /// white wash) over the BG2 logo instead of staying transparent.
    #[test]
    fn render_bg_row_tile16_selects_four_quadrants() {
        let mut ppu = Ppu::new();
        ppu.bgmode = 1;
        ppu.bg_screen_addr[0] = 0x0000;
        ppu.bg_tiledata_addr[0] = 0x2000; // char word base 0x1000
        ppu.bg_screen_size[0] = 0;
        ppu.bg_voffset[0] = 0xFFFF; // screen_y = scanline - 1
        ppu.bg_tile_size[0] = true; // 16x16 tiles

        // One tilemap entry at word 0 names base tile N = 4 (palette 0).
        ppu.vram[0] = 0x0004;

        // Fill all 8 rows of each quadrant's character with a solid
        // colour index so the assertion holds regardless of which row a
        // flip would select. 4bpp: plane0/1 at (0x1000 + T*16 + row),
        // plane2/3 at +8.
        fill_solid_4bpp_tile(&mut ppu, 4, 0x00FF, 0x0000); // TL -> index 1
        fill_solid_4bpp_tile(&mut ppu, 5, 0xFF00, 0x0000); // TR -> index 2
        fill_solid_4bpp_tile(&mut ppu, 20, 0xFFFF, 0x0000); // BL -> index 3
        fill_solid_4bpp_tile(&mut ppu, 21, 0x0000, 0x00FF); // BR -> index 4

        ppu.cgram[1] = 0x1111;
        ppu.cgram[2] = 0x2222;
        ppu.cgram[3] = 0x3333;
        ppu.cgram[4] = 0x4444;

        let mut top = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        let mut bot = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        ppu.render_bg_row(0, 1, &mut top, false, &mut NoTrace::<FrameTrace>::new()); // screen_y 0 -> top half
        ppu.render_bg_row(0, 9, &mut bot, false, &mut NoTrace::<FrameTrace>::new()); // screen_y 8 -> bottom half

        assert_eq!(top[0].color, 0x1111, "x0..7,y0..7 -> TL tile N");
        assert_eq!(top[8].color, 0x2222, "x8..15,y0..7 -> TR tile N+1");
        assert_eq!(bot[0].color, 0x3333, "x0..7,y8..15 -> BL tile N+16");
        assert_eq!(bot[8].color, 0x4444, "x8..15,y8..15 -> BR tile N+17");

        assert!(
            !ppu.bg_tilemap_row_cache[0].valid,
            "16x16 tiles must bypass the 8x8 tilemap-row cache"
        );
    }

    /// H/V flips on a 16x16 entry mirror *which* quadrant each screen
    /// position reads: with both flips set, the top-left screen corner
    /// must resolve to the bottom-right character (N+17), etc.
    #[test]
    fn render_bg_row_tile16_flips_swap_quadrants() {
        let mut ppu = Ppu::new();
        ppu.bgmode = 1;
        ppu.bg_screen_addr[0] = 0x0000;
        ppu.bg_tiledata_addr[0] = 0x2000;
        ppu.bg_screen_size[0] = 0;
        ppu.bg_voffset[0] = 0xFFFF;
        ppu.bg_tile_size[0] = true;

        // Entry: tile N = 4 with flip_h (0x4000) + flip_v (0x8000).
        ppu.vram[0] = 0x0004 | 0x4000 | 0x8000;

        // Same four characters as the non-flipped test, every row solid
        // so the per-quadrant colour index is unaffected by the decoder's
        // pixel mirroring, only the quadrant *selection* flips.
        fill_solid_4bpp_tile(&mut ppu, 4, 0x00FF, 0x0000); // tile 4  -> index 1
        fill_solid_4bpp_tile(&mut ppu, 5, 0xFF00, 0x0000); // tile 5  -> index 2
        fill_solid_4bpp_tile(&mut ppu, 20, 0xFFFF, 0x0000); // tile 20 -> index 3
        fill_solid_4bpp_tile(&mut ppu, 21, 0x0000, 0x00FF); // tile 21 -> index 4

        ppu.cgram[1] = 0x1111;
        ppu.cgram[2] = 0x2222;
        ppu.cgram[3] = 0x3333;
        ppu.cgram[4] = 0x4444;

        let mut top = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        let mut bot = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        ppu.render_bg_row(0, 1, &mut top, false, &mut NoTrace::<FrameTrace>::new());
        ppu.render_bg_row(0, 9, &mut bot, false, &mut NoTrace::<FrameTrace>::new());

        // Both flips set -> screen TL reads BR char (N+17, idx 4), screen
        // TR reads BL (N+16, idx 3), screen BL reads TR (N+1, idx 2),
        // screen BR reads TL (N, idx 1).
        assert_eq!(top[0].color, 0x4444, "flip: screen-TL -> char N+17");
        assert_eq!(top[8].color, 0x3333, "flip: screen-TR -> char N+16");
        assert_eq!(bot[0].color, 0x2222, "flip: screen-BL -> char N+1");
        assert_eq!(bot[8].color, 0x1111, "flip: screen-BR -> char N");
    }

    /// Mode 5 (hires) BG1 with 16x16 tiles must still expand each entry
    /// to the 2x2 block of 8-px characters across the 512-dot grid. The
    /// PPU bus activity test's Mode 6 text and Air Strike Patrol's menu
    /// need it: read as 8x8, the bottom characters (`+16`) are never
    /// selected and tall text shows only its top half.
    #[test]
    fn render_bg_row_hires_tile16_selects_four_quadrants() {
        let mut ppu = Ppu::new();
        ppu.bgmode = 5; // hires
        ppu.bg_screen_addr[0] = 0x0000;
        ppu.bg_tiledata_addr[0] = 0x2000; // char word base 0x1000
        ppu.bg_screen_size[0] = 0;
        ppu.bg_voffset[0] = 0xFFFF; // screen_y = scanline - 1
        ppu.bg_tile_size[0] = true; // 16x16 tiles

        ppu.vram[0] = 0x0004; // entry names base tile N = 4

        fill_solid_4bpp_tile(&mut ppu, 4, 0x00FF, 0x0000); // TL -> index 1
        fill_solid_4bpp_tile(&mut ppu, 5, 0xFF00, 0x0000); // TR -> index 2
        fill_solid_4bpp_tile(&mut ppu, 20, 0xFFFF, 0x0000); // BL -> index 3
        fill_solid_4bpp_tile(&mut ppu, 21, 0x0000, 0x00FF); // BR -> index 4

        ppu.cgram[1] = 0x1111;
        ppu.cgram[2] = 0x2222;
        ppu.cgram[3] = 0x3333;
        ppu.cgram[4] = 0x4444;

        let mut top = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        let mut bot = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        ppu.render_bg_row(0, 1, &mut top, true, &mut NoTrace::<FrameTrace>::new()); // screen_y 0 -> top half
        ppu.render_bg_row(0, 9, &mut bot, true, &mut NoTrace::<FrameTrace>::new()); // screen_y 8 -> bottom half

        // 512-dot grid: cols 0..7 = left char, cols 8..15 = right char.
        assert_eq!(top[0].color, 0x1111, "x0..7,y0..7 -> TL tile N");
        assert_eq!(top[8].color, 0x2222, "x8..15,y0..7 -> TR tile N+1");
        assert_eq!(bot[0].color, 0x3333, "x0..7,y8..15 -> BL tile N+16");
        assert_eq!(bot[8].color, 0x4444, "x8..15,y8..15 -> BR tile N+17");
    }

    /// In hires a 16x16 entry spans 16 BG rows: `screen_y >> 4` advances
    /// the tilemap row and `screen_y & 8` picks top vs bottom char within
    /// the block. This also guards the interlace weave, which feeds a
    /// *doubled* `screen_y` into the same path.
    #[test]
    fn render_bg_row_hires_tile16_vertical_stride_is_16() {
        let mut ppu = Ppu::new();
        ppu.bgmode = 5;
        ppu.bg_screen_addr[0] = 0x0000;
        ppu.bg_tiledata_addr[0] = 0x2000;
        ppu.bg_screen_size[0] = 0;
        ppu.bg_voffset[0] = 0; // screen_y = scanline
        ppu.bg_tile_size[0] = true;

        // Block row 0 = tilemap word 0 -> tile 4; block row 1 = word 32
        // (one 16-px-tall row down) -> tile 8.
        ppu.vram[0] = 0x0004;
        ppu.vram[32] = 0x0008;
        fill_solid_4bpp_tile(&mut ppu, 4, 0x00FF, 0x0000); // block0 TL -> idx 1
        fill_solid_4bpp_tile(&mut ppu, 20, 0xFFFF, 0x0000); // block0 BL (4+16) -> idx 3
        fill_solid_4bpp_tile(&mut ppu, 8, 0xFF00, 0x0000); // block1 TL -> idx 2
        ppu.cgram[1] = 0x1111;
        ppu.cgram[2] = 0x2222;
        ppu.cgram[3] = 0x3333;

        let top_color = |ppu: &mut Ppu, sy: u16| {
            let mut row = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
            ppu.render_bg_row(0, sy, &mut row, true, &mut NoTrace::<FrameTrace>::new());
            row[0].color
        };
        assert_eq!(top_color(&mut ppu, 0), 0x1111, "y0 -> block0 top char");
        assert_eq!(
            top_color(&mut ppu, 8),
            0x3333,
            "y8 -> block0 bottom char (+16), same entry"
        );
        assert_eq!(
            top_color(&mut ppu, 16),
            0x2222,
            "y16 -> block1 top char (next 16-px entry row)"
        );
    }

    /// Honouring 16x16 in hires must not disturb 8x8 hires BGs (size bit
    /// clear): those keep pairing `tile` (left 8 px) with the adjacent
    /// `tile + 1` (right 8 px) via `hires_adjacent_tile_offset`, never the
    /// 16x16 `+16`.
    #[test]
    fn render_bg_row_hires_8x8_still_pairs_adjacent_tiles() {
        let mut ppu = Ppu::new();
        ppu.bgmode = 5;
        ppu.bg_screen_addr[0] = 0x0000;
        ppu.bg_tiledata_addr[0] = 0x2000;
        ppu.bg_screen_size[0] = 0;
        ppu.bg_voffset[0] = 0xFFFF; // screen_y = scanline - 1
        ppu.bg_tile_size[0] = false; // 8x8

        ppu.vram[0] = 0x0004; // entry tile N = 4
        fill_solid_4bpp_tile(&mut ppu, 4, 0x00FF, 0x0000); // tile N -> index 1
        fill_solid_4bpp_tile(&mut ppu, 5, 0xFF00, 0x0000); // tile N+1 -> index 2
        ppu.cgram[1] = 0x1111;
        ppu.cgram[2] = 0x2222;

        let mut row = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        ppu.render_bg_row(0, 1, &mut row, true, &mut NoTrace::<FrameTrace>::new()); // screen_y 0

        assert_eq!(row[0].color, 0x1111, "left 8 px -> tile N");
        assert_eq!(row[8].color, 0x2222, "right 8 px -> adjacent tile N+1");
    }
}
