//! Drawing a picture of the frame again on a debugger's request: the whole
//! picture into the framebuffer (a repaint), one layer alone, the picture
//! in one of the debugger's views, or the picture into a buffer of its own.
//!
//! Where the line table holds a line ([`super::recording`]), the line is
//! drawn from the state it was drawn with: its registers, scroll, palette,
//! sprite mask and sprite list, with video memory put back as the line read
//! it, and then the palette writes made while it was on screen, in order.
//! The lines nothing was recorded of are drawn from the state as it is now,
//! as every line was before the table existed. Either way nothing a
//! savestate holds moves: what drawing changes is kept and put back.

use retro_core::capture::{NoTrace, PixelTrace, RenderedLayer};
use retro_core::video::{crop_rgba, PixelImage};

use super::recording::{undo_journal, LineKind, LineRecord, LineTable, PaletteRedraw};
use super::sources::LineTrace;
use super::{
    DrawState, RenderLayerMode, Vdp, FRAMEBUFFER_WIDTH, LAYER_COUNT, LAYER_PLANE_A, LAYER_PLANE_B,
    LAYER_WINDOW,
};

/// The state a line drawn from its record replaces: kept before, put back
/// after. Video memory is copied only when the journal will change it.
struct LiveInputs {
    registers: [u8; 24],
    vertical_scroll: [u16; 40],
    palette: [(u8, u8, u8); 64],
    sprite_mask_overflow: bool,
    video_memory: Option<Vec<u8>>,
}

impl LiveInputs {
    fn keep(vdp: &Vdp, with_video_memory: bool) -> Self {
        let mut vertical_scroll = [0; 40];
        vertical_scroll.copy_from_slice(&vdp.vsram[..40]);
        Self {
            registers: vdp.registers,
            vertical_scroll,
            palette: vdp.cram_rgb,
            sprite_mask_overflow: vdp.sprite_mask_overflow,
            video_memory: with_video_memory.then(|| vdp.vram.clone()),
        }
    }

    fn put_back(&self, vdp: &mut Vdp) {
        vdp.registers = self.registers;
        vdp.vsram[..40].copy_from_slice(&self.vertical_scroll);
        vdp.cram_rgb = self.palette;
        vdp.sprite_mask_overflow = self.sprite_mask_overflow;
        if let Some(video_memory) = &self.video_memory {
            vdp.vram.copy_from_slice(video_memory);
        }
    }
}

impl Vdp {
    /// Re-render every visible scanline into the framebuffer, without
    /// advancing timing or touching interrupts, and return whether every
    /// line was drawn from the state it was recorded with.
    ///
    /// Used by the debugger to refresh a paused frame after a layer toggle
    /// or an edit: the live frame loop composites each line as the beam
    /// scans it, so a paused core never re-composites and the change would
    /// not show until the game resumes. It runs the same `render_scanline`
    /// the running emulator does, so the debug composite can never diverge
    /// from the real one, with the debugger's layer mask.
    ///
    /// While the line table is armed and holds the frame, each line is
    /// drawn from the state it was drawn with, so a scroll, palette or
    /// register change made part of the way down the screen, or a tile or
    /// a nametable cell rewritten, shows as it was drawn. A line the table
    /// does not hold is drawn from video memory, the palette and the
    /// registers as they are now: the end of the frame, which flattens a
    /// split made part of the way down.
    ///
    /// Only the framebuffer changes. What drawing a line leaves behind
    /// besides its pixels (`DrawState`: the sprite status flags a
    /// savestate holds, the sprite list, the line buffers) is put back as
    /// the frame left it, so a repaint moves neither the state hash nor
    /// the lines the game draws next.
    pub fn rerender_current_frame(&mut self) -> bool {
        let kept = DrawState::keep(self);
        let rows = self.timing.visible_scanlines();
        let from_line_states = self.redraw_rows(rows, |vdp, line, row| {
            vdp.draw_row(line, row, &mut NoTrace::new())
        });
        kept.restore(self);
        from_line_states
    }

    /// One layer of the frame drawn again: `layer` is one of the debugger's
    /// layers (0 plane A, 1 plane B, 2 the window, 3 the sprites), `None`
    /// for any other. The picture is the screen, the mode's width by
    /// [`Self::drawn_height`] lines, in RGBA: a pixel the layer covers has
    /// its palette colour, before shadow and highlight, and alpha 255;
    /// every other pixel is 0 in all four bytes, never the backdrop nor a
    /// colour key. A line drawn with the display off covers nothing.
    ///
    /// A line the line table holds is drawn from the state it was drawn
    /// with, and its colours follow the palette writes made while it was
    /// on screen, column by column as they showed; any other line is drawn
    /// from the state as it is now. The debugger's layer toggles do not
    /// hide the layer asked for. The drawing goes to the line buffers alone,
    /// whose contents are put back after with the rest of `DrawState`: the
    /// framebuffer and everything a savestate holds stay as they were.
    pub fn render_layer_image(&mut self, layer: usize) -> Option<RenderedLayer> {
        if layer >= LAYER_COUNT {
            return None;
        }
        let width = self.display_width() as usize;
        let height = self.drawn_height() as usize;
        let mut rgba = vec![0u8; width * height * 4];
        let kept = DrawState::keep(self);
        let hidden = self.debug_layer_disable.take();
        let debug_pixel = self.debug_pixel.take();
        let from_line_states = self.redraw_rows(height as u16, |vdp, line, row| {
            let start = usize::from(line) * width * 4;
            vdp.draw_layer_row(layer, line, row, &mut rgba[start..start + width * 4]);
        });
        self.debug_pixel = debug_pixel;
        self.debug_layer_disable = hidden;
        kept.restore(self);
        Some(RenderedLayer {
            image: PixelImage {
                width: width as u32,
                height: height as u32,
                rgba,
            },
            from_line_states,
        })
    }

    /// The frame drawn again in `mode`, as an opaque RGBA picture of the
    /// screen (the mode's width by [`Self::drawn_height`] lines), from the
    /// line table where it holds a line. The framebuffer, the mode and the
    /// rest of `DrawState` are put back after, so nothing the game shows or
    /// a savestate holds changes.
    pub fn render_mode_image(&mut self, mode: RenderLayerMode) -> PixelImage {
        let width = self.display_width() as usize;
        let height = self.drawn_height() as usize;
        let kept = DrawState::keep(self);
        let framebuffer = self.framebuffer.clone();
        let previous_mode = std::mem::replace(&mut self.render_layer_mode, mode);
        self.redraw_rows(height as u16, |vdp, line, row| {
            vdp.draw_row(line, row, &mut NoTrace::new())
        });
        let rgba = self.framebuffer_picture(width, height);
        self.render_layer_mode = previous_mode;
        self.framebuffer = framebuffer;
        kept.restore(self);
        PixelImage {
            width: width as u32,
            height: height as u32,
            rgba,
        }
    }

    /// The picture drawn again from the line table into a buffer of its
    /// own, cut as the host's framebuffer is (the mode's width by
    /// [`Self::drawn_height`] lines), RGBA: only while the table holds the
    /// frame its journal follows from its first line to its finished
    /// picture, `None` otherwise (line states not armed, armed part of the
    /// way through the frame, or the frame stopped part of the way down).
    /// Where the journal had room and the debugger has written no video
    /// memory since the frame began, this is the picture the frame drew,
    /// byte for byte: what a host can compare the framebuffer with. The
    /// framebuffer and everything a savestate holds stay as they were.
    pub fn picture_from_line_states(&mut self) -> Option<PixelImage> {
        if !self.line_table_holds_frame() {
            return None;
        }
        let width = self.display_width() as usize;
        let height = self.drawn_height() as usize;
        let kept = DrawState::keep(self);
        let framebuffer = self.framebuffer.clone();
        self.redraw_rows(height as u16, |vdp, line, row| {
            vdp.draw_row(line, row, &mut NoTrace::new())
        });
        let rgba = self.framebuffer_picture(width, height);
        self.framebuffer = framebuffer;
        kept.restore(self);
        Some(PixelImage {
            width: width as u32,
            height: height as u32,
            rgba,
        })
    }

    /// The framebuffer's first `height` rows, each cut to `width` pixels.
    fn framebuffer_picture(&self, width: usize, height: usize) -> Vec<u8> {
        crop_rgba(
            &self.framebuffer,
            FRAMEBUFFER_WIDTH,
            width as u32,
            height as u32,
        )
    }

    /// Draw the lines `0..rows` again with `draw`, each from its record in
    /// the line table while one is armed (`Some`), otherwise from the state
    /// as it is now (`None`), and return whether every line had a record.
    /// Nothing is recorded meanwhile: the recording is set aside.
    pub(super) fn redraw_rows(
        &mut self,
        rows: u16,
        mut draw: impl FnMut(&mut Vdp, u16, Option<&LineRecord>),
    ) -> bool {
        let recording = self.recording.take();
        let from_line_states = match recording.as_deref().and_then(|r| r.lines.as_ref()) {
            Some(table) => self.redraw_rows_from(table, rows, &mut draw),
            None => {
                // The sprite list is parsed again from video memory as it
                // is now: an edit while paused may have changed the table.
                self.sprite_cache_valid = false;
                for line in 0..rows {
                    draw(self, line, None);
                }
                false
            }
        };
        self.recording = recording;
        from_line_states
    }

    /// [`Self::redraw_rows`] with the line table `table`.
    ///
    /// The lines of the frame the journal follows are drawn from the bottom
    /// up: before each, the journal's writes made from its line on are
    /// undone, newest first, so video memory is as the line read it. The
    /// lines of an earlier frame (below the beam after a stop part of the
    /// way down) come next, with video memory as it was when the journal
    /// began, which is as close as the journal reaches, and the horizontal
    /// scroll each read written into the scroll table for it. A journal
    /// that ran out of room undoes nothing: each line is drawn from video
    /// memory as it is at the end, with its own horizontal scroll written
    /// in the same way. The lines nothing was recorded of come last, from
    /// the state as it is now. Everything a line's record replaces is put
    /// back, video memory included.
    fn redraw_rows_from(
        &mut self,
        table: &LineTable,
        rows: u16,
        draw: &mut impl FnMut(&mut Vdp, u16, Option<&LineRecord>),
    ) -> bool {
        let rows = usize::from(rows).min(table.rows.len());
        let of_frame =
            |row: &LineRecord| row.kind != LineKind::NotRecorded && row.frame == table.frame;
        let of_earlier_frame =
            |row: &LineRecord| row.kind != LineKind::NotRecorded && row.frame != table.frame;
        let earlier = table.rows[..rows].iter().any(of_earlier_frame);
        let live = LiveInputs::keep(self, !table.journal.is_empty() || earlier);
        let mut installed_list = None;

        let mut cursor = table.journal.len();
        for line in (0..rows).rev() {
            let row = &table.rows[line];
            if !of_frame(row) {
                continue;
            }
            if !table.journal_full {
                cursor = undo_journal(&table.journal, cursor, line as u16, &mut self.vram);
            }
            let write_scroll = table.journal_full;
            self.install_row(table, line as u16, row, write_scroll, &mut installed_list);
            draw(self, line as u16, Some(row));
        }

        if earlier {
            if !table.journal_full {
                undo_journal(&table.journal, cursor, 0, &mut self.vram);
            }
            for line in (0..rows).rev() {
                let row = &table.rows[line];
                if of_earlier_frame(row) {
                    self.install_row(table, line as u16, row, true, &mut installed_list);
                    draw(self, line as u16, Some(row));
                }
            }
        }

        live.put_back(self);
        let unrecorded = |row: &LineRecord| row.kind == LineKind::NotRecorded;
        if !table.rows[..rows].iter().any(unrecorded) {
            return true;
        }
        self.sprite_cache_valid = false;
        for line in 0..rows {
            if unrecorded(&table.rows[line]) {
                draw(self, line as u16, None);
            }
        }
        false
    }

    /// Put line `line`'s record in place of the state it replaces: its
    /// registers, vertical scroll, palette, sprite mask and sprite list,
    /// and, with `write_scroll`, its horizontal scroll, written into the
    /// scroll table for a line whose video memory cannot be put back.
    /// `installed_list` is the sprite list in place, so a list is copied
    /// once for the lines that share it.
    fn install_row(
        &mut self,
        table: &LineTable,
        line: u16,
        row: &LineRecord,
        write_scroll: bool,
        installed_list: &mut Option<usize>,
    ) {
        self.registers = row.registers;
        self.vsram[..40].copy_from_slice(&row.vertical_scroll);
        for (colour, &word) in self.cram_rgb.iter_mut().zip(row.palette.iter()) {
            *colour = Self::convert_genesis_color(word);
        }
        self.sprite_mask_overflow = row.sprite_mask_carried_in;
        match table.sprite_list_for(row.frame, line) {
            Some(index) => {
                if *installed_list != Some(index) {
                    self.sprite_cache = table.sprite_lists[index].sprites.clone();
                    *installed_list = Some(index);
                }
                self.sprite_cache_valid = true;
            }
            None => {
                // No list was recorded for it: the renderer parses one from
                // video memory as the line now has it.
                self.sprite_cache_valid = false;
                *installed_list = None;
            }
        }
        if write_scroll {
            self.write_recorded_horizontal_scroll(line, row);
        }
    }

    /// Write the horizontal scroll line `line` read into the scroll table,
    /// where its registers make the renderer read it.
    fn write_recorded_horizontal_scroll(&mut self, line: u16, row: &LineRecord) {
        let mode = row.registers[11] & 0x03;
        let base = usize::from(row.registers[13] & 0x3F) << 10;
        for (plane, offset) in [(0, 0), (1, 2)] {
            if let Some(at) = Self::horizontal_scroll_address(base, offset, mode, line) {
                if at + 1 < self.vram.len() {
                    self.vram[at..at + 2]
                        .copy_from_slice(&row.horizontal_scroll[plane].to_be_bytes());
                }
            }
        }
    }

    /// Draw line `line` into the framebuffer: from its record, then the
    /// palette writes made while it was on screen, or from the state as it
    /// is now. With a trace (`T::ON`) it notes where each pixel the line
    /// shows came from ([`super::sources`]); the redraws that only draw pass
    /// [`NoTrace`].
    pub(super) fn draw_row<T: PixelTrace<Notes = LineTrace>>(
        &mut self,
        line: u16,
        row: Option<&LineRecord>,
        trace: &mut T,
    ) {
        let Some(row) = row else {
            self.render_scanline_with(line, trace);
            return;
        };
        if row.kind == LineKind::DirectColour {
            // The backdrop, then the colours the transfer painted.
            self.render_blanked_line(line);
            let offset = usize::from(line) * FRAMEBUFFER_WIDTH as usize * 4;
            let width = self.display_width() as usize;
            if T::ON {
                trace.notes().blanked(width, self.backdrop_index());
            }
            for (pair, &word) in row.direct_colours.iter().enumerate() {
                let (red, green, blue) = Self::convert_genesis_color(word);
                for x in [pair * 2, pair * 2 + 1] {
                    let at = offset + x * 4;
                    if x < width && at + 3 < self.framebuffer.len() {
                        self.framebuffer[at..at + 4].copy_from_slice(&[red, green, blue, 255]);
                        if T::ON {
                            trace.notes().painted(x, word);
                        }
                    }
                }
            }
        } else {
            self.render_scanline_with(line, trace);
        }
        for change in &row.palette_changes {
            self.cram_rgb[usize::from(change.index)] = Self::convert_genesis_color(change.colour);
            match change.redraw {
                PaletteRedraw::Nothing => {}
                PaletteRedraw::Columns {
                    from,
                    to,
                    shadow_highlight,
                } => {
                    let (from, to) = (usize::from(from), usize::from(to));
                    self.remap_columns(line, from, to, shadow_highlight);
                    if T::ON {
                        trace.notes().remapped(
                            (&self.line_index, &self.line_sh_op, &self.line_high_pri),
                            from,
                            to,
                            shadow_highlight,
                        );
                    }
                }
                PaletteRedraw::WholeLine { width, backdrop } => {
                    let (width, backdrop) = (usize::from(width), usize::from(backdrop));
                    self.render_blanked_columns(line, width, backdrop);
                    if T::ON {
                        trace.notes().blanked(width, backdrop);
                    }
                }
            }
        }
    }

    /// Draw line `line` of `layer` into `pixels` (one row of the layer's
    /// picture): composited from its record, or from the state as it is
    /// now, and coloured column by column as the palette writes made while
    /// it was on screen coloured it.
    fn draw_layer_row(
        &mut self,
        layer: usize,
        line: u16,
        row: Option<&LineRecord>,
        pixels: &mut [u8],
    ) {
        if !self.row_is_drawn(row) {
            return;
        }
        self.composite_scanline(line);
        self.colour_row(row, pixels, |vdp, x| vdp.layer_pixel(layer, x));
    }

    /// Colour `pixels` (one row of a layer's picture) from the line last
    /// composited: `pixel` gives the colour of column `x`, or `None` where
    /// the row stays transparent. The columns are coloured with the palette
    /// the line began with, then again as each palette write made while it
    /// was on screen coloured them (`row`'s, none for a line drawn from the
    /// state as it is now).
    pub(super) fn colour_row(
        &mut self,
        row: Option<&LineRecord>,
        pixels: &mut [u8],
        pixel: impl Fn(&Vdp, usize) -> Option<(u8, u8, u8)>,
    ) {
        let columns = (pixels.len() / 4).min(self.display_width() as usize);
        self.colour_columns(pixels, 0, columns, &pixel);
        for change in row.map_or(&[][..], |row| row.palette_changes.as_slice()) {
            self.cram_rgb[usize::from(change.index)] = Self::convert_genesis_color(change.colour);
            match change.redraw {
                PaletteRedraw::Nothing => {}
                PaletteRedraw::Columns { from, to, .. } => {
                    let to = usize::from(to).min(columns);
                    self.colour_columns(pixels, usize::from(from), to, &pixel);
                }
                PaletteRedraw::WholeLine { .. } => {
                    // The line became the backdrop: no layer covers it, and
                    // a later write can only colour that backdrop again.
                    pixels.fill(0);
                    return;
                }
            }
        }
    }

    /// Colour the columns `from..to` of `pixels` as `pixel` gives them, with
    /// the palette as it is now.
    fn colour_columns(
        &self,
        pixels: &mut [u8],
        from: usize,
        to: usize,
        pixel: &impl Fn(&Vdp, usize) -> Option<(u8, u8, u8)>,
    ) {
        for x in from..to {
            let colour = match pixel(self, x) {
                Some((red, green, blue)) => [red, green, blue, 255],
                None => [0; 4],
            };
            pixels[x * 4..x * 4 + 4].copy_from_slice(&colour);
        }
    }

    /// The colour `layer` gives pixel `x` of the line last composited, or
    /// `None` where the layer is transparent: what the single-layer modes
    /// of `remap_scanline_segment` draw, without their colour key.
    pub(super) fn layer_pixel(&self, layer: usize, x: usize) -> Option<(u8, u8, u8)> {
        let index = match layer {
            LAYER_PLANE_A => self.line_plane_a[x],
            LAYER_PLANE_B => self.line_plane_b[x],
            LAYER_WINDOW => self.line_window[x],
            _ => {
                // An operator sprite of shadow and highlight is not a pixel.
                let colour = self.sprite_line_color[x];
                if colour == 0 || self.line_sh_op[x] != 0 {
                    -1
                } else {
                    (self.sprite_line_palette[x] as i16 * 16 + colour as i16).min(63)
                }
            }
        };
        (index >= 0).then(|| self.cram_rgb[index as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::super::recording::JOURNAL_CAPACITY;
    use super::*;
    use retro_core::capture::CaptureRecording;

    /// A VDP with the display on in the 40-cell mode, plane A at `$C000`
    /// with tile 1 (colour 1, red) in column 0 of every row, the scroll
    /// table at `$FC00`, and line states recorded for frame 1.
    fn recording_vdp() -> Vdp {
        let mut vdp = Vdp::new();
        for (register, value) in [
            (1, 0x44),
            (2, 0x30),
            (4, 0x07),
            (12, 0x81),
            (13, 0x3F),
            (15, 0x02),
        ] {
            vdp.set_test_register(register, value);
        }
        vdp.write_vram_debug(32, &[0x11; 32]);
        for row in 0..32u32 {
            vdp.write_vram_debug(0xC000 + row * 64, &[0x00, 0x01]);
        }
        vdp.write_cram_debug(1, &[0x000E]);
        vdp.set_capture_recording(CaptureRecording {
            line_states: true,
            ..CaptureRecording::default()
        });
        vdp.set_frame_being_drawn(1);
        vdp
    }

    /// Write `value` into VRAM at `address` through the data port.
    fn port_write(vdp: &mut Vdp, address: u16, value: u16) {
        vdp.write_word_port(4, 0x4000 | (address & 0x3FFF));
        vdp.write_word_port(4, address >> 14);
        vdp.write_word_port(0, value);
    }

    fn red_at(vdp: &Vdp, picture: &[u8], x: usize, y: usize) -> bool {
        let at = (y * vdp.display_width() as usize + x) * 4;
        (picture[at], picture[at + 1], picture[at + 2]) == Vdp::convert_genesis_color(0x000E)
    }

    #[test]
    fn a_full_journal_draws_each_line_with_its_own_horizontal_scroll() {
        let mut vdp = recording_vdp();
        for line in 0..224u16 {
            vdp.scanline(line);
            if line == 100 {
                // Plane A eight pixels right from line 101 on.
                port_write(&mut vdp, 0xFC00, 8);
            }
            if line == 110 {
                // Two words written by turns, more times than the journal
                // holds: it fills, and nothing they hold changes.
                for i in 0..=JOURNAL_CAPACITY {
                    vdp.journal_vram_write(0x0400 + (i % 2) * 2);
                }
            }
        }
        let table = vdp.line_table().expect("armed");
        assert!(table.journal_full);
        let states = vdp.line_state_table().expect("armed");
        assert!(!states.memory_exact);

        let framebuffer = vdp.framebuffer_picture(320, 224);
        let picture = vdp.picture_from_line_states().expect("armed").rgba;
        assert!(red_at(&vdp, &picture, 0, 50), "line 50 had no scroll");
        assert!(!red_at(&vdp, &picture, 0, 150));
        assert!(red_at(&vdp, &picture, 8, 150), "line 150 had eight pixels");
        assert_eq!(picture, framebuffer);
    }

    #[test]
    fn a_debugger_write_into_the_sprite_table_moves_the_sprite_of_the_lines_drawn_again() {
        let mut vdp = recording_vdp();
        vdp.set_test_register(5, 0x78); // sprite table at $F000
                                        // Sprite 0: one cell of tile 1 at (40, 20).
        vdp.write_vram_debug(0xF000, &[0x00, 0x94, 0x00, 0x00, 0x00, 0x01, 0x00, 0xA8]);
        for line in 0..224u16 {
            vdp.scanline(line);
        }
        let picture = vdp.picture_from_line_states().expect("armed").rgba;
        assert!(red_at(&vdp, &picture, 40, 20));

        // X 80 instead.
        vdp.write_vram_debug(0xF006, &[0x00, 0xD0]);
        assert!(vdp.rerender_current_frame(), "every line from its record");
        let picture = vdp.framebuffer_picture(320, 224);
        assert!(!red_at(&vdp, &picture, 40, 20));
        assert!(red_at(&vdp, &picture, 80, 20));
    }
}
