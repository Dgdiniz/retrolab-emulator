//! Drawing a line of the picture: tile reads, the horizontal scroll, the
//! scanline render, the compositor (the planes, the window and the line's
//! sprites, their priorities and the shadow/highlight operators), the remap
//! from palette indices to colours, the blanked lines, and the planes'
//! geometry.

use retro_core::capture::{NoTrace, PixelTrace};

use super::sources::{self, LineTrace};
use super::window::WindowArea;
use super::{
    RenderLayerMode, Vdp, LAYER_PLANE_A, LAYER_PLANE_B, LAYER_SPRITES, LAYER_WINDOW,
    TRANSPARENT_RGB,
};

impl Vdp {
    /// Render backdrop color to entire framebuffer.
    pub(super) fn render_backdrop(&mut self) {
        // Register 7 contains backdrop color
        let palette_index = ((self.registers[7] >> 4) & 0x3) as usize;
        let color_index = (self.registers[7] & 0xF) as usize;
        let cram_index = palette_index * 16 + color_index;

        // Get color from CRAM (9-bit RGB: 0000 BBB0 GGG0 RRR0)
        let color_9bit = self.cram[cram_index];
        let (r, g, b) = Self::convert_genesis_color(color_9bit);

        // Fill framebuffer with backdrop color
        for pixel in self.framebuffer.as_chunks_mut::<4>().0 {
            pixel[0] = r;
            pixel[1] = g;
            pixel[2] = b;
            pixel[3] = 255; // Alpha
        }
    }

    /// Rebuild the renderer's tables of the palette (`cram_rgb` and its
    /// shadowed and highlighted twins) from CRAM as it is: a savestate's
    /// load calls it, a CRAM write sets its one entry.
    pub(super) fn rebuild_cram_rgb(&mut self) {
        for i in 0..64 {
            if i < self.cram.len() {
                let word = self.cram[i];
                self.set_palette_colour(i, word);
            }
        }
    }

    /// Read a single pixel (4-bit color index) directly from a VRAM tile pattern.
    /// This avoids decoding all 64 pixels of a tile when only 1 is needed.
    /// pattern_index: tile number in VRAM, px/py: pixel coords within tile (0-7).
    #[inline(always)]
    pub(super) fn read_tile_pixel_from_vram(vram: &[u8], pattern_index: u16, px: u8, py: u8) -> u8 {
        let addr = (pattern_index as usize) * 32 + (py as usize) * 4 + (px as usize) / 2;
        if addr >= vram.len() {
            return 0;
        }
        let byte = vram[addr];
        if px & 1 == 0 {
            byte >> 4
        } else {
            byte & 0x0F
        }
    }

    /// Read a single pixel from a VRAM tile pattern with flip support.
    #[inline(always)]
    pub(super) fn read_tile_pixel_flipped(
        vram: &[u8],
        pattern_index: u16,
        px: u8,
        py: u8,
        flip_h: bool,
        flip_v: bool,
    ) -> u8 {
        let px = if flip_h { 7 - px } else { px };
        let py = if flip_v { 7 - py } else { py };
        Self::read_tile_pixel_from_vram(vram, pattern_index, px, py)
    }

    /// Read horizontal scroll value from VRAM for a given plane and scanline.
    ///
    /// Modes (register 11, bits 0-1):
    ///   0 = Full screen scroll (single value)
    ///   1 = First 8 lines (line & 7, prohibited but used by Populous)
    ///   2 = Per cell (every 8 scanlines)
    ///   3 = Per line
    #[inline(always)]
    pub(super) fn read_hscroll(
        &self,
        hscroll_base: usize,
        plane_offset: usize,
        mode: u8,
        line: u16,
    ) -> u16 {
        let Some(addr) = Self::horizontal_scroll_address(hscroll_base, plane_offset, mode, line)
        else {
            return 0;
        };
        if addr + 1 < self.vram.len() {
            let high = self.vram[addr] as u16;
            let low = self.vram[addr + 1] as u16;
            ((high << 8) | low) & 0x03FF
        } else {
            0
        }
    }

    /// The address in VRAM of the horizontal scroll a plane reads on `line`
    /// ([`Self::read_hscroll`]), or `None` for a mode with none.
    #[inline(always)]
    pub(super) fn horizontal_scroll_address(
        hscroll_base: usize,
        plane_offset: usize,
        mode: u8,
        line: u16,
    ) -> Option<usize> {
        match mode {
            0 => Some(hscroll_base + plane_offset),
            1 => {
                // "Prohibited" mode: only the line's low three bits pick the
                // entry, so it wraps within the first 8.
                let l = (line & 7) as usize;
                Some(hscroll_base + l * 4 + plane_offset)
            }
            2 => {
                // Per-cell: the line counts in eight bits and its low three
                // are dropped, so the table can't run past the 8-bit line
                // range.
                let cell = ((line & 0xFF) / 8) as usize;
                Some(hscroll_base + cell * 32 + plane_offset)
            }
            3 => {
                // Per-line: the line counts in eight bits.
                let l = (line & 0xFF) as usize;
                Some(hscroll_base + l * 4 + plane_offset)
            }
            _ => None,
        }
    }

    /// Render a single scanline. Two-stage pipeline: `composite_scanline`
    /// resolves BG + sprite priority into a per-line palette-index buffer, then
    /// `remap_scanline` turns indices into RGB via `cram_rgb`. Keeping them
    /// separate lets a mid-line CRAM write re-tint the line (remap only) without
    /// recompositing; an ordinary frame just runs both back to back.
    #[inline(always)]
    pub(super) fn render_scanline(&mut self, line: u16) {
        self.render_scanline_with(line, &mut NoTrace::new());
    }

    /// [`Self::render_scanline`] with a trace, which notes where each
    /// pixel of the line came from (`T::ON`); the frame loops pass
    /// [`NoTrace`].
    pub(super) fn render_scanline_with<T: PixelTrace<Notes = LineTrace>>(
        &mut self,
        line: u16,
        trace: &mut T,
    ) {
        let display_enabled = (self.registers[1] & 0x40) != 0;
        if !display_enabled {
            self.render_blanked_line(line);
            if T::ON {
                let width = self.display_width() as usize;
                trace.notes().blanked(width, self.backdrop_index());
            }
            return;
        }
        self.composite_scanline_with(line, trace);
        self.remap_scanline(line);
        if T::ON {
            let width = self.display_width() as usize;
            let shadow_highlight = (self.registers[12] & 0x08) != 0;
            trace.notes().remapped(
                (&self.line_index, &self.line_sh_op, &self.line_high_pri),
                (self.line_shaded, &self.line_shade),
                0,
                width,
                shadow_highlight,
            );
        }
    }

    /// Backdrop CRAM index (palette*16 + colour) from reg7.
    pub(super) fn backdrop_index(&self) -> usize {
        let bd_pal = ((self.registers[7] >> 4) & 0x3) as usize;
        let bd_col = (self.registers[7] & 0xF) as usize;
        bd_pal * 16 + bd_col
    }

    /// Backdrop RGB from reg7 via the CRAM LUT.
    pub(super) fn backdrop_rgb(&self) -> (u8, u8, u8) {
        let bd_idx = self.backdrop_index();
        if bd_idx < 64 {
            self.cram_rgb[bd_idx]
        } else {
            (0, 0, 0)
        }
    }

    /// True when a VDP write on the current (visible) line lands early enough to
    /// be a raster effect for THIS line rather than setup for the next one.
    /// The window is the first 860 of the line's 3420 master clocks (the
    /// window Genesis Plus GX uses), expressed in the 68K-cycle beam
    /// units SG1 tracks. Inert until a frame has run (`beam_cycles_per_line`
    /// still 0), so pure-unit-test register pokes never trigger a re-render.
    ///
    /// SG1 advances the beam *after* each instruction, so a write in the very
    /// first instruction of a line still sees the previous line's high beam
    /// value and is skipped. Under-triggering (a missed effect) is safe; the
    /// alternative (a stale-low beam) would over-trigger and corrupt lines.
    pub(super) fn midline_render_window(&self) -> bool {
        if self.beam_cycles_per_line == 0 {
            return false;
        }
        if self.timing.scanline >= self.timing.visible_scanlines() {
            return false;
        }
        let window = (self.beam_cycles_per_line as u64 * 860 / 3420) as u32;
        self.beam_cycles_into_line <= window
    }

    /// Active-display pixel column under the beam right now, from the
    /// per-instruction beam cycle position. SG1 places HBLANK in the last ~72
    /// 68K cycles of the line; the active scanout is the rest, mapped linearly
    /// onto the display width. A mid-line CRAM write re-tints `[beam_active_x,
    /// width)` so the already-scanned left of the line keeps its colour.
    pub(super) fn beam_active_x(&self) -> usize {
        let per_line = self.beam_cycles_per_line.max(1);
        let active = per_line.saturating_sub(72).max(1);
        let pos = self.beam_cycles_into_line.min(active);
        let width = self.display_width() as usize;
        ((pos as u64 * width as u64) / active as u64) as usize
    }

    /// Fill a scanline with the backdrop colour when the display is disabled
    /// (reg1 bit 6 clear): every layer mode shows the backdrop, as on hardware.
    pub(super) fn render_blanked_line(&mut self, line: u16) {
        let width = self.display_width() as usize;
        self.render_blanked_columns(line, width, self.backdrop_index());
    }

    /// Fill the first `width` pixels of scanline `line` with backdrop colour
    /// `backdrop` (a CRAM index), and its index buffers with that pen: the
    /// body of [`Self::render_blanked_line`], with the width and the
    /// backdrop given, as a line drawn again from its record gives them.
    pub(super) fn render_blanked_columns(&mut self, line: u16, width: usize, backdrop: usize) {
        let internal_width = 320usize;
        let fb_offset = (line as usize) * internal_width * 4;
        let backdrop_color = if backdrop < 64 {
            self.cram_rgb[backdrop]
        } else {
            (0, 0, 0)
        };
        let bd_idx = backdrop.min(63) as u8;
        // The backdrop is as it is: the line resolves no shade.
        self.line_shaded = false;
        for x in 0..width {
            // Keep the index buffers consistent with the backdrop fill: a
            // disabled line holds the backdrop pen, so a later mid-line CRAM
            // re-tint stays backdrop. Without this, `remap_scanline`
            // (a CRAM mid-line hook on a now-enabled line) would re-tint a
            // previous line's stale composite into this blanked line.
            self.line_index[x] = bd_idx;
            self.line_sh_op[x] = 0;
            self.line_high_pri[x] = false;
            self.line_plane_a[x] = -1;
            self.line_window[x] = -1;
            self.line_plane_b[x] = -1;
            let idx = fb_offset + x * 4;
            if idx + 3 < self.framebuffer.len() {
                self.framebuffer[idx] = backdrop_color.0;
                self.framebuffer[idx + 1] = backdrop_color.1;
                self.framebuffer[idx + 2] = backdrop_color.2;
                self.framebuffer[idx + 3] = 255;
            }
        }
    }

    /// Resolve plane B / plane A / window and sprites into this line's
    /// palette-index buffers. Produces no RGB (that is `remap_scanline`'s job),
    /// so the line can be re-tinted after a mid-line CRAM change.
    #[inline(always)]
    pub(super) fn composite_scanline(&mut self, line: u16) {
        self.composite_scanline_with(line, &mut NoTrace::new());
    }

    /// [`Self::composite_scanline`] with a trace, which notes where the
    /// pixel each column resolves to came from (`T::ON`): its layer, the
    /// character and the map entry or sprite it was read from. The frame
    /// loops pass [`NoTrace`].
    ///
    /// Shadow and highlight (register 12 bit 3) are on or off for the
    /// whole line, and the line is composited by a body built for each:
    /// with them on, each pixel's shade is resolved into `line_shade`;
    /// with them off, none is, and `line_shaded` says every pixel of the
    /// line is as it is.
    pub(super) fn composite_scanline_with<T: PixelTrace<Notes = LineTrace>>(
        &mut self,
        line: u16,
        trace: &mut T,
    ) {
        let shaded = (self.registers[12] & 0x08) != 0;
        if shaded {
            self.composite_line::<T, true>(line, trace);
        } else {
            self.composite_line::<T, false>(line, trace);
        }
        self.line_shaded = shaded;
    }

    /// The body of [`Self::composite_scanline_with`], with shadow and
    /// highlight on (`SHADED`) or off: built twice, so a line with them off
    /// does no work for them at all.
    fn composite_line<T: PixelTrace<Notes = LineTrace>, const SHADED: bool>(
        &mut self,
        line: u16,
        trace: &mut T,
    ) {
        let width = self.display_width() as usize;
        let shadow_highlight = SHADED;
        let bd_idx = self.backdrop_index().min(63) as u8;

        // Debugger per-layer force-disable mask (read once per line).
        let dis_plane_a = self.debug_layer_disable.is_hidden(LAYER_PLANE_A);
        let dis_plane_b = self.debug_layer_disable.is_hidden(LAYER_PLANE_B);
        let dis_window = self.debug_layer_disable.is_hidden(LAYER_WINDOW);
        let dis_sprites = self.debug_layer_disable.is_hidden(LAYER_SPRITES);

        // Render sprites for this scanline into the line buffer (ONCE)
        self.render_sprite_line(line, trace);

        // Pre-compute plane parameters to avoid repeated register reads
        let plane_a_nametable = ((self.registers[2] & 0x38) as u16) << 10;
        let plane_b_nametable = ((self.registers[4] & 0x07) as u16) << 13;
        // Plane geometry. `pf_shift` collapses to 0 for the invalid
        // horizontal size 0b10 (rows repeat); the masks wrap the tile
        // column / pixel row exactly like real hardware.
        let (pf_shift, pf_col_mask, pf_row_mask) = self.playfield_geometry();
        let reg11 = self.registers[11];
        let h_scroll_mode = reg11 & 0x03;
        let v_scroll_mode = (reg11 >> 2) & 0x01;
        let hscroll_base = ((self.registers[13] & 0x3F) as usize) << 10;

        // H40 select is RS0 (reg12 bit 0), as in display_width().
        let is_h40 = (self.registers[12] & 0x01) != 0;
        let window_nametable = self.get_window_nametable();
        let window_width_tiles: u16 = if is_h40 { 64 } else { 32 };
        let window_area = self.window_area();
        let line_is_window = window_area.takes_line(line);

        // Horizontal wrap span in pixels (tile-column count derived from the
        // column mask). The vertical wrap is an AND with `pf_row_mask` so the
        // invalid size 0b10 (mask 0x2FF) wraps non-power-of-two like hardware.
        let plane_w_px = ((pf_col_mask as i32) + 1) * 8;

        // Read horizontal scroll values (depends on mode but NOT on x)
        // Mode 0: full screen, Mode 2: per-cell (8 lines), Mode 3: per-line
        let h_scroll_a = self.read_hscroll(hscroll_base, 0, h_scroll_mode, line);
        let h_scroll_b = self.read_hscroll(hscroll_base, 2, h_scroll_mode, line);

        // Read vertical scroll for full-screen mode (mode 0)
        let v_scroll_a_full = if v_scroll_mode == 0 {
            if !self.vsram.is_empty() {
                self.vsram[0] & 0x03FF
            } else {
                0
            }
        } else {
            0
        };
        let v_scroll_b_full = if v_scroll_mode == 0 {
            if 1 < self.vsram.len() {
                self.vsram[1] & 0x03FF
            } else {
                0
            }
        } else {
            0
        };

        // Left-most 2-cell column vertical-scroll quirk:
        // with per-column vscroll active, a line whose horizontal scroll is not
        // 16px-aligned shows a partial left-edge column that does NOT read VSRAM
        // column 0. H40 shares column 19's Plane A AND Plane B value across both
        // planes; H32 disables it (value 0). The exact extent/value is
        // underdocumented (Genesis Plus GX and PicoDrive disagree); SG1 gives
        // the picture Genesis Plus GX gives. The fine shift is the hscroll's low 4 bits (the
        // offset within a 16px column); only the first `shift` pixels are special.
        let left_col_vs: u16 = if is_h40 && 39 < self.vsram.len() {
            (self.vsram[38] & self.vsram[39]) & 0x03FF
        } else {
            0
        };
        let shift_a = h_scroll_a & 0x0F;
        let shift_b = h_scroll_b & 0x0F;

        // Composite each pixel
        for x in 0..width {
            let xu16 = x as u16;
            // For a trace: where the plane B and the plane A or window
            // candidates came from.
            let mut origin_b = sources::PenOrigin::BACKDROP;
            let mut origin_a = sources::PenOrigin::BACKDROP;

            // ---- Plane B ----
            let v_scroll_b = if v_scroll_mode == 0 {
                v_scroll_b_full
            } else if shift_b != 0 && xu16 < shift_b {
                // Partial left-edge column: special left-most-column vscroll.
                left_col_vs
            } else {
                let col = (xu16 / 16) as usize;
                let vi = col * 2 + 1;
                if vi < self.vsram.len() {
                    self.vsram[vi] & 0x03FF
                } else {
                    0
                }
            };

            let scrolled_x_b = (xu16 as i32 - h_scroll_b as i32).rem_euclid(plane_w_px) as u16;
            let v_line_b = (line as u32 + v_scroll_b as u32) & pf_row_mask as u32;
            let tile_col_b = (scrolled_x_b / 8) & pf_col_mask;
            let tile_row_b = (v_line_b >> 3) as u16;
            let px_b = (scrolled_x_b % 8) as u8;
            let py_b = (v_line_b & 7) as u8;

            let row_off_b = (((tile_row_b as u32) << pf_shift) & 0x1FC0) as u16;
            let nt_off_b = row_off_b + tile_col_b * 2;
            let addr_b = (plane_b_nametable + nt_off_b) as usize;

            // `plane_b_bit`: the map entry's priority bit, which shadow and
            // highlight read where the pixel is transparent too;
            // `plane_b_pri` is the bit of an opaque pixel only.
            let (plane_b_idx, plane_b_pri, plane_b_bit) =
                if !dis_plane_b && addr_b + 1 < self.vram.len() {
                    let word_b = ((self.vram[addr_b] as u16) << 8) | (self.vram[addr_b + 1] as u16);
                    let pattern_b = word_b & 0x07FF;
                    let flip_h_b = (word_b & 0x0800) != 0;
                    let flip_v_b = (word_b & 0x1000) != 0;
                    let palette_b = ((word_b >> 13) & 0x03) as usize;
                    let priority_b = (word_b & 0x8000) != 0;

                    let ci_b = Self::read_tile_pixel_flipped(
                        &self.vram, pattern_b, px_b, py_b, flip_h_b, flip_v_b,
                    );
                    if ci_b != 0 {
                        if T::ON {
                            origin_b = sources::PenOrigin::character(
                                sources::DRAWN_BY_PLANE_B,
                                word_b,
                                addr_b,
                                px_b,
                                py_b,
                            );
                        }
                        let cram_i = palette_b * 16 + ci_b as usize;
                        (Some(cram_i.min(63) as u8), priority_b, priority_b)
                    } else {
                        (None, false, priority_b)
                    }
                } else {
                    (None, false, false)
                };

            // ---- Plane A or Window ----
            // `plane_a_bit`: as `plane_b_bit`, the window's bit where the
            // window covers.
            let in_window = line_is_window || window_area.takes_column(xu16);
            let (plane_a_idx, plane_a_pri, plane_a_bit) = if in_window {
                // Window plane (no scrolling)
                let wtx = xu16 / 8;
                let wty = line / 8;
                let wpx = (xu16 % 8) as u8;
                let wpy = (line % 8) as u8;
                let wnt_off = (wty * window_width_tiles + wtx) * 2;
                let waddr = (window_nametable + wnt_off) as usize;

                if !dis_window && waddr + 1 < self.vram.len() {
                    let wword = ((self.vram[waddr] as u16) << 8) | (self.vram[waddr + 1] as u16);
                    let wpat = wword & 0x07FF;
                    let wfh = (wword & 0x0800) != 0;
                    let wfv = (wword & 0x1000) != 0;
                    let wpal = ((wword >> 13) & 0x03) as usize;
                    let wpri = (wword & 0x8000) != 0;

                    let wci = Self::read_tile_pixel_flipped(&self.vram, wpat, wpx, wpy, wfh, wfv);
                    if wci != 0 {
                        if T::ON {
                            origin_a = sources::PenOrigin::character(
                                sources::DRAWN_BY_WINDOW,
                                wword,
                                waddr,
                                wpx,
                                wpy,
                            );
                        }
                        let cram_i = wpal * 16 + wci as usize;
                        (Some(cram_i.min(63) as u8), wpri, wpri)
                    } else {
                        (None, false, wpri)
                    }
                } else {
                    (None, false, false)
                }
            } else {
                // Plane A (with scrolling)
                let v_scroll_a = if v_scroll_mode == 0 {
                    v_scroll_a_full
                } else if shift_a != 0 && xu16 < shift_a {
                    // Partial left-edge column: special left-most-column vscroll.
                    left_col_vs
                } else {
                    let col = (xu16 / 16) as usize;
                    let vi = col * 2;
                    if vi < self.vsram.len() {
                        self.vsram[vi] & 0x03FF
                    } else {
                        0
                    }
                };

                let scrolled_x_a = (xu16 as i32 - h_scroll_a as i32).rem_euclid(plane_w_px) as u16;
                let v_line_a = (line as u32 + v_scroll_a as u32) & pf_row_mask as u32;
                let tile_col_a = (scrolled_x_a / 8) & pf_col_mask;
                let tile_row_a = (v_line_a >> 3) as u16;
                let px_a = (scrolled_x_a % 8) as u8;
                let py_a = (v_line_a & 7) as u8;

                let row_off_a = (((tile_row_a as u32) << pf_shift) & 0x1FC0) as u16;
                let nt_off_a = row_off_a + tile_col_a * 2;
                let addr_a = (plane_a_nametable + nt_off_a) as usize;

                if !dis_plane_a && addr_a + 1 < self.vram.len() {
                    let word_a = ((self.vram[addr_a] as u16) << 8) | (self.vram[addr_a + 1] as u16);
                    let pattern_a = word_a & 0x07FF;
                    let flip_h_a = (word_a & 0x0800) != 0;
                    let flip_v_a = (word_a & 0x1000) != 0;
                    let palette_a = ((word_a >> 13) & 0x03) as usize;
                    let priority_a = (word_a & 0x8000) != 0;

                    let ci_a = Self::read_tile_pixel_flipped(
                        &self.vram, pattern_a, px_a, py_a, flip_h_a, flip_v_a,
                    );
                    if ci_a != 0 {
                        if T::ON {
                            origin_a = sources::PenOrigin::character(
                                sources::DRAWN_BY_PLANE_A,
                                word_a,
                                addr_a,
                                px_a,
                                py_a,
                            );
                        }
                        let cram_i = palette_a * 16 + ci_a as usize;
                        (Some(cram_i.min(63) as u8), priority_a, priority_a)
                    } else {
                        (None, false, priority_a)
                    }
                } else {
                    (None, false, false)
                }
            };

            // ---- Merge planes (priority only; colour resolved in remap) ----
            // `merged_is_a`: the merged pixel is plane A's (or the window's),
            // not plane B's; read by a trace alone.
            let (merged_idx, merged_pri, merged_is_a) = match (plane_b_idx, plane_a_idx) {
                (None, None) => (None, false, false),
                (Some(b), None) => (Some(b), plane_b_pri, false),
                (None, Some(a)) => (Some(a), plane_a_pri, true),
                (Some(b), Some(a)) => {
                    if plane_a_pri {
                        (Some(a), true, true)
                    } else if plane_b_pri {
                        (Some(b), true, false)
                    } else {
                        (Some(a), false, true)
                    }
                }
            };

            // ---- Sprite from line buffer ----
            let s_ci = self.sprite_line_color[x];
            let has_sprite = s_ci != 0 && !dis_sprites;

            // ---- Composite to a final palette index ----
            // `sprite_shown`: the final pixel is the sprite's; read by a
            // trace alone. `sprite_pen`: the sprite pixel in front, its
            // palette line, colour (0 for none) and priority bit, which
            // shadow and highlight read.
            let (final_index, is_high_priority, sh_operator, sprite_shown, sprite_pen) =
                if has_sprite {
                    let s_pal = self.sprite_line_palette[x] as usize;
                    let s_pri = self.sprite_line_priority[x];
                    let s_idx = (s_pal * 16 + s_ci as usize).min(63) as u8;
                    let pen = (s_pal as u8, s_ci, s_pri);

                    // Shadow/Highlight operator sprites (palette 3, colour 14/15) are
                    // transparent brightness operators, not drawn pixels.
                    let is_sh_operator =
                        shadow_highlight && s_pal == 3 && (s_ci == 14 || s_ci == 15);
                    let sh_op = if is_sh_operator { s_ci } else { 0 };

                    if is_sh_operator {
                        // Operator sprite: transparent, keep the underlying pixel index.
                        match merged_idx {
                            None => (bd_idx, false, sh_op, false, pen),
                            Some(bi) => (bi, merged_pri, sh_op, false, pen),
                        }
                    } else {
                        let (idx, pri, shown) = match merged_idx {
                            None => (s_idx, false, true),
                            Some(bi) => {
                                if s_pri {
                                    (s_idx, true, true)
                                } else if merged_pri {
                                    (bi, true, false)
                                } else {
                                    (s_idx, false, true)
                                }
                            }
                        };
                        (idx, pri, 0, shown, pen)
                    }
                } else {
                    let none = (0, 0, false);
                    match merged_idx {
                        None => (bd_idx, false, 0u8, false, none),
                        Some(bi) => (bi, merged_pri, 0u8, false, none),
                    }
                };

            if T::ON {
                let origin = if sprite_shown {
                    trace.notes().sprite_origin(x)
                } else if merged_idx.is_none() {
                    sources::PenOrigin::BACKDROP
                } else if merged_is_a {
                    origin_a
                } else {
                    origin_b
                };
                trace.notes().pen(x, origin);
            }

            if let Some((dbg_x, dbg_y)) = self.debug_pixel {
                if xu16 == dbg_x && line == dbg_y {
                    let plane_b_color = plane_b_idx.map(|i| self.cram_rgb[i as usize]);
                    let plane_a_color = plane_a_idx.map(|i| self.cram_rgb[i as usize]);
                    let merged_color = merged_idx.map(|i| self.cram_rgb[i as usize]);
                    let final_color = self.cram_rgb[final_index as usize];
                    let backdrop_color = self.cram_rgb[bd_idx as usize];
                    self.log_debug_pixel(
                        x,
                        line,
                        in_window,
                        addr_b,
                        plane_a_nametable,
                        plane_b_nametable,
                        window_nametable,
                        pf_shift,
                        pf_col_mask,
                        pf_row_mask,
                        h_scroll_mode,
                        v_scroll_mode,
                        is_h40,
                        hscroll_base,
                        h_scroll_a,
                        h_scroll_b,
                        v_scroll_a_full,
                        v_scroll_b_full,
                        plane_w_px,
                        plane_b_color,
                        plane_b_pri,
                        plane_a_color,
                        plane_a_pri,
                        merged_color,
                        merged_pri,
                        has_sprite,
                        final_color,
                        is_high_priority,
                        sh_operator,
                        backdrop_color,
                    );
                }
            }

            // ---- Store this pixel's resolved indices for the remap pass ----
            self.line_index[x] = final_index;
            self.line_sh_op[x] = sh_operator;
            self.line_high_pri[x] = is_high_priority;
            // ---- What shadow and highlight do to the pixel ----
            if SHADED {
                self.line_shade[x] = sources::shade(
                    shadow_highlight,
                    sprite_pen,
                    (plane_a_idx.is_some(), plane_a_bit),
                    (plane_b_idx.is_some(), plane_b_bit),
                );
            }
            // `plane_a_idx` holds the window's pixel where the window covers.
            let plane_a_or_window = plane_a_idx.map_or(-1, |i| i as i16);
            if in_window {
                self.line_plane_a[x] = -1;
                self.line_window[x] = plane_a_or_window;
            } else {
                self.line_plane_a[x] = plane_a_or_window;
                self.line_window[x] = -1;
            }
            self.line_plane_b[x] = plane_b_idx.map_or(-1, |i| i as i16);
        }
    }

    /// Turn this line's palette-index buffers into RGB in the framebuffer,
    /// applying shadow/highlight and the active render-layer mode. Cheap and
    /// re-runnable after a mid-line CRAM change (no recompositing).
    fn remap_scanline(&mut self, line: u16) {
        let width = self.display_width() as usize;
        self.remap_scanline_segment(line, 0, width);
    }

    /// Remap a horizontal span `[x_start, x_end)` of this line's index buffers
    /// to RGB. A mid-line CRAM write re-tints only `[beam_x, width)`, the
    /// pixels at and to the right of the beam, not yet scanned out, so several
    /// writes across a line build up multiple colour segments (a per-write
    /// approximation of per-pixel CRAM, finer than a whole-line update).
    /// `remap_scanline` is the whole-line case `[0, width)`.
    pub(super) fn remap_scanline_segment(&mut self, line: u16, x_start: usize, x_end: usize) {
        let shadow_highlight = (self.registers[12] & 0x08) != 0;
        self.remap_columns(line, x_start, x_end, shadow_highlight);
    }

    /// The body of [`Self::remap_scanline_segment`], with shadow and
    /// highlight given, as a palette write recorded on a line gives it when
    /// the line is drawn again: with them on, each pixel takes the shade the
    /// composite resolved for it (`line_shade`, none for a line composited
    /// with them off), with them off its colour as it is
    /// ([`sources::coloured_shade`]).
    #[inline]
    pub(super) fn remap_columns(
        &mut self,
        line: u16,
        x_start: usize,
        x_end: usize,
        shadow_highlight: bool,
    ) {
        let internal_width = 320usize;
        let fb_offset = (line as usize) * internal_width * 4;

        for x in x_start..x_end {
            let final_index = self.line_index[x];
            let sh_operator = self.line_sh_op[x];
            let is_high_priority = self.line_high_pri[x];

            // ---- Shadow/Highlight: the shade the composite resolved ----
            let shade =
                sources::coloured_shade(shadow_highlight, (self.line_shaded, &self.line_shade), x);
            let final_color = self.palette_colour(final_index as usize, shade);

            // ---- Layer-mode rendering (debug visualization) ----
            let output_color = match self.render_layer_mode {
                RenderLayerMode::Normal => final_color,
                RenderLayerMode::PlaneB => {
                    if self.line_plane_b[x] >= 0 {
                        self.cram_rgb[self.line_plane_b[x] as usize]
                    } else {
                        TRANSPARENT_RGB
                    }
                }
                RenderLayerMode::PlaneA => {
                    if self.line_plane_a[x] >= 0 {
                        self.cram_rgb[self.line_plane_a[x] as usize]
                    } else {
                        TRANSPARENT_RGB
                    }
                }
                RenderLayerMode::Window => {
                    if self.line_window[x] >= 0 {
                        self.cram_rgb[self.line_window[x] as usize]
                    } else {
                        TRANSPARENT_RGB
                    }
                }
                RenderLayerMode::Sprites => {
                    let s_ci = self.sprite_line_color[x];
                    if s_ci != 0 && sh_operator == 0 {
                        let sp = self.sprite_line_palette[x] as usize;
                        let ci = (sp * 16 + s_ci as usize).min(63);
                        self.cram_rgb[ci]
                    } else {
                        TRANSPARENT_RGB
                    }
                }
                RenderLayerMode::PriorityOverlay => {
                    if is_high_priority {
                        // Tint green for high-priority pixels
                        let r = (final_color.0 as u16 * 6 / 10) as u8;
                        let g = ((final_color.1 as u16).saturating_add(80)).min(255) as u8;
                        let b = (final_color.2 as u16 * 6 / 10) as u8;
                        (r, g, b)
                    } else {
                        final_color
                    }
                }
                RenderLayerMode::SpritePriority => {
                    let s_ci = self.sprite_line_color[x];
                    if s_ci != 0 && sh_operator == 0 {
                        if self.sprite_line_priority[x] {
                            (220, 60, 60) // Red = high-priority sprite
                        } else {
                            (60, 60, 220) // Blue = low-priority sprite
                        }
                    } else {
                        (final_color.0 / 3, final_color.1 / 3, final_color.2 / 3)
                    }
                }
            };

            let idx = fb_offset + x * 4;
            if idx + 3 < self.framebuffer.len() {
                self.framebuffer[idx] = output_color.0;
                self.framebuffer[idx + 1] = output_color.1;
                self.framebuffer[idx + 2] = output_color.2;
                self.framebuffer[idx + 3] = 255;
            }
        }
    }

    /// Get scroll plane size from register 16.
    pub(super) fn get_scroll_size(&self) -> (u16, u16) {
        let reg16 = self.registers[16];
        let h_size = reg16 & 0x03;
        let v_size = (reg16 >> 4) & 0x03;

        let width = match h_size {
            0 => 32,
            1 => 64,
            3 => 128,
            _ => 32,
        };

        let height = match v_size {
            0 => 32,
            1 => 64,
            3 => 128,
            _ => 32,
        };

        (width, height)
    }

    /// Scroll-plane geometry derived from register 16.
    ///
    /// Returns `(row_shift, col_mask_tiles, row_mask_px)`:
    /// - `row_shift`: how far the tile-row index is shifted to form the
    ///   nametable row offset (also the log2 of the byte stride per row).
    /// - `col_mask_tiles`: AND-mask that wraps the horizontal tile column.
    /// - `row_mask_px`: AND-mask that wraps the vertical pixel position.
    ///
    /// A plane is 32, 64 or 128 tiles wide (sizes 0, 1 and 3), and its two
    /// vertical size bits are bits 8 and 9 of the row mask, above eight
    /// bits that are always set. The horizontal size `0b10` is *invalid* on
    /// real hardware: the row offset stays 0, so every scanline re-reads
    /// nametable row 0 of a plane 32 tiles wide. Likewise the vertical size
    /// `0b10` yields the non-power-of-two mask `0x2FF`. Scroll-size / window
    /// test ROMs depend on these quirks, so the render path uses this model
    /// instead of a plain width multiply (checked against Genesis Plus GX).
    pub(super) fn playfield_geometry(&self) -> (u32, u16, u16) {
        let reg16 = self.registers[16];
        let (row_shift, col_mask_tiles) = match reg16 & 0x03 {
            0b00 => (6, 0x1F),
            0b01 => (7, 0x3F),
            0b11 => (8, 0x7F),
            _ => (0, 0x1F),
        };
        let v_size = u16::from((reg16 >> 4) & 0x03);
        (row_shift, col_mask_tiles, (v_size << 8) | 0x0FF)
    }

    /// The part of the screen the window plane takes from plane A, from
    /// registers 17 and 18 (see [`WindowArea`] for the rule).
    pub fn window_area(&self) -> WindowArea {
        WindowArea::from_registers(self.registers[17], self.registers[18])
    }
}
