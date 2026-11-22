//! What each line of the held frame did, worked out from its line table
//! without drawing: the sprites the evaluation met and what became of each,
//! the window masks, and the offset-per-tile columns.
//!
//! Each reader walks the frame's lines with the settings each was drawn
//! with and, where it needs it, the video memory each read, worked out on
//! copies (see [`record::ReplayedMemory`]), so none of them touches the
//! PPU. When the line table does not hold the frame (no table kept, a
//! table armed in the middle of the frame, or a register the debugger
//! wrote since), each reads the visible lines with the registers and the
//! memory as they stand, and says so.

use retro_core::capture::{line_flags, LineSprites, SpriteFate};
use retro_core::video::SpriteOrderEntry;

use super::picture::WovenRow;
use super::record::{self, LineState, ReplayedMemory};
use super::sprite::{self, line_budget};
use super::tile::offset_per_tile;
use super::window::{col_region, window_inside};
use super::{window_layer, Ppu};

/// Masks per line in [`WindowMasks::masks`]: BG1, BG2, BG3, BG4, the
/// sprites, the colour window, where the main screen is kept and where
/// colour math may run.
pub const WINDOW_MASKS_PER_LINE: usize = 8;

/// Bytes per mask: one bit a dot of the 256.
pub const WINDOW_MASK_BYTES: usize = 32;

/// Each line's window masks (see [`Ppu::window_masks`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowMasks {
    /// The number of the frame the picture is, as the caller numbers it
    /// (the emulator: as a frame snapshot numbers it).
    pub frame: u64,
    /// The lines were read from the frame's line table; otherwise from the
    /// registers as they stand.
    pub from_line_states: bool,
    /// The lines, in the order they were drawn (1 the first).
    pub lines: Vec<u16>,
    /// [`WINDOW_MASKS_PER_LINE`] masks a line of [`WINDOW_MASK_BYTES`]
    /// bytes, in the order of the lines: the window region of BG1, BG2,
    /// BG3, BG4 and the sprites (where the layer's combined windows cover,
    /// whether or not the screens clip the layer there), the colour
    /// window's region, where the main screen keeps its pixels (outside it
    /// they are black) and where colour math may run. Bit `x & 7` of byte
    /// `x >> 3` is dot `x`; in a hires or pseudo-hires line output
    /// column `k` is dot `k >> 1`.
    pub masks: Vec<u8>,
}

/// The bits of [`OffsetPerTileTable::replaced`].
pub mod offset_replaced {
    /// BG3's entry replaced the column's horizontal scroll.
    pub const HORIZONTAL: u8 = 1;
    /// BG3's entry replaced the column's vertical scroll.
    pub const VERTICAL: u8 = 2;
    /// The column lies past the line's end: the line did not fetch it.
    pub const PAST_THE_LINE: u8 = 0x80;
}

/// The offset-per-tile columns of the frame's lines in Modes 2, 4 and 6
/// (see [`Ppu::offset_per_tile_table`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OffsetPerTileTable {
    /// The number of the frame the picture is, as the caller numbers it
    /// (the emulator: as a frame snapshot numbers it).
    pub frame: u64,
    /// The lines were read from the frame's line table; otherwise from the
    /// registers and the memory as they stand.
    pub from_line_states: bool,
    /// The line of each row, in the order drawn. A line of an interlaced
    /// hires or pseudo-hires frame gives two rows, its fields', each with
    /// the background line it fetched.
    pub lines: Vec<u16>,
    /// Tile columns a row: 33 for a 256-dot line, 65 when a row is Mode 6,
    /// whose line is 512 dots.
    pub columns: u32,
    /// Per row, per background (BG1 then BG2), per column: the horizontal
    /// and then the vertical tilemap coordinate the column was fetched at,
    /// absolute, the scroll already in them.
    pub scroll: Vec<u16>,
    /// Per row, per background, per column: [`offset_replaced`] bits.
    pub replaced: Vec<u8>,
}

/// Tile columns of a 256-dot line: 32 and the one a fine scroll splits.
const COLUMNS_LORES: u32 = 33;
/// Tile columns of a 512-dot line.
const COLUMNS_HIRES: u32 = 65;

/// The lines a reader walks: the line table's when a redraw draws from
/// it, else the visible lines with the settings as they stand.
struct FrameLines {
    from_table: bool,
    lines: Vec<(u16, LineState)>,
}

/// The screen x of a sprite at the nine-bit position `x`.
fn screen_x(x: u16) -> i16 {
    if x >= 256 {
        x as i16 - 512
    } else {
        x as i16
    }
}

/// Whether a line showed sprites at all.
fn shows_sprites(state: &LineState) -> bool {
    !state.display_disable
        && state.display_brightness != 0
        && (state.obj_main_enable || state.obj_sub_enable)
}

impl Ppu {
    /// The lines the readers walk, and whether they come from the table.
    fn frame_lines(&self) -> FrameLines {
        match self.recorders.lines.as_deref() {
            Some(table) if table.redraws_frame() => FrameLines {
                from_table: true,
                lines: table.lines().to_vec(),
            },
            _ => {
                let state = LineState::capture(self);
                FrameLines {
                    from_table: false,
                    lines: (1..240u16)
                        .filter(|&line| self.in_active_scanline(line))
                        .map(|line| (line, state))
                        .collect(),
                }
            }
        }
    }

    /// OAM as each of the walked lines read it.
    fn sprite_table_by_line(&self, from_table: bool) -> ReplayedMemory<[u8; 0x220]> {
        match self.recorders.lines.as_deref() {
            Some(table) if from_table => table.sprite_table_by_line(&self.oam),
            _ => record::sprite_table_as_it_stands(&self.oam),
        }
    }

    /// VRAM as each of the walked lines read it.
    fn vram_by_line(&self, from_table: bool) -> ReplayedMemory<[u16; 0x8000]> {
        match self.recorders.lines.as_deref() {
            Some(table) if from_table => table.vram_by_line(&self.vram),
            _ => record::vram_as_it_stands(&self.vram),
        }
    }

    /// The sprites each line of the held frame met and what became of each
    /// ([`LineSprites`], numbered `frame`), from the frame's line table when
    /// a redraw draws from it: each line evaluated with its own settings and
    /// the sprite table as it read it, by the rule the frame drew with
    /// ([`sprite::line_budget`]). The walk goes on past a line's sprite
    /// limit to list the sprites it dropped whole, which changes nothing.
    ///
    /// On this core a line is numbered from 1, the first drawn, and a
    /// sprite's x is its nine-bit position, 256 and up to the left of the
    /// screen (-256 to 255). The fates it gives: drawn whole, drawn in part
    /// (the line's 34-column time limit ran out inside it), over the line
    /// limit (met after the 32 sprites the line keeps), left out (the time
    /// limit dropped every column of it) and off screen (at x = 256, whose
    /// columns the line fetches but cannot show); no sprite mask hides one.
    /// The line flags: over the limit (STAT77 bit 6), out of time (STAT77
    /// bit 7) and not shown (forced blank, brightness 0, or sprites on
    /// neither screen; the evaluation runs all the same).
    pub fn line_sprites(&self, frame: u64) -> LineSprites {
        let walk = self.frame_lines();
        let mut oam = self.sprite_table_by_line(walk.from_table);
        let mut out = LineSprites {
            frame,
            from_line_states: walk.from_table,
            ..LineSprites::default()
        };
        let mut dropped = Vec::new();
        for (index, (line, state)) in walk.lines.iter().enumerate() {
            oam.advance_to(index);
            let table = oam.memory();
            dropped.clear();
            let budget = line_budget(
                table,
                state.oam_first_sprite,
                state.obj_base_size,
                state.obj_interlace,
                line.wrapping_sub(1),
                Some(&mut dropped),
            );
            let first = out.slots.len();
            let mut push = |slot: u8, fate: u8, drawn: u8| {
                let s = sprite::parse_sprite(
                    table,
                    usize::from(slot),
                    state.obj_base_size,
                    state.obj_interlace,
                );
                out.slots.push(slot);
                out.fates.push(fate);
                out.x.push(screen_x(s.x));
                out.width.push(s.width);
                out.drawn_width.push(drawn);
            };
            for (k, &slot) in budget.kept().iter().enumerate() {
                let s = sprite::parse_sprite(
                    table,
                    usize::from(slot),
                    state.obj_base_size,
                    state.obj_interlace,
                );
                let drawn = sprite::drawn_pixels(&s, budget.columns_drawn[k]);
                let fate = if sprite::drawn_pixels(&s, u8::MAX) == 0 {
                    SpriteFate::OffScreen
                } else if budget.columns_dropped[k] == 0 {
                    SpriteFate::DrawnWhole
                } else if budget.columns_drawn[k] > 0 {
                    SpriteFate::DrawnInPart
                } else {
                    SpriteFate::LeftOut
                };
                push(slot, fate as u8, drawn);
            }
            for &slot in &dropped {
                push(slot, SpriteFate::OverLineLimit as u8, 0);
            }
            let mut flags = 0;
            if budget.over_range {
                flags |= line_flags::OVER_LIMIT;
            }
            if budget.over_time {
                flags |= line_flags::OUT_OF_TIME;
            }
            if !shows_sprites(state) {
                flags |= line_flags::NOT_SHOWN;
            }
            out.lines
                .extend([*line, first as u16, (out.slots.len() - first) as u16, flags]);
        }
        out
    }

    /// The sprites the held frame drew, back to front: the ones that drew
    /// a pixel on a line that showed sprites, ordered by the priority
    /// rotation of the first line that showed them, the sprite after the
    /// first sprite at the back and the first sprite at the front. Each
    /// line's own order, and what each line dropped, are in
    /// [`Self::line_sprites`].
    pub(crate) fn sprite_render_order(&self) -> Vec<SpriteOrderEntry> {
        let frame = self.frame_lines();
        let mut oam = self.sprite_table_by_line(frame.from_table);
        let mut first_sprite: Option<u8> = None;
        let mut priority: [Option<u8>; 128] = [None; 128];
        for (index, (line, state)) in frame.lines.iter().enumerate() {
            if !shows_sprites(state) {
                continue;
            }
            first_sprite.get_or_insert(state.oam_first_sprite);
            oam.advance_to(index);
            let table = oam.memory();
            let budget = line_budget(
                table,
                state.oam_first_sprite,
                state.obj_base_size,
                state.obj_interlace,
                line.wrapping_sub(1),
                None,
            );
            for (k, &slot) in budget.kept().iter().enumerate() {
                let s = sprite::parse_sprite(
                    table,
                    usize::from(slot),
                    state.obj_base_size,
                    state.obj_interlace,
                );
                if sprite::drawn_pixels(&s, budget.columns_drawn[k]) > 0 {
                    priority[usize::from(slot)].get_or_insert(s.priority);
                }
            }
        }
        let first = usize::from(first_sprite.unwrap_or(0));
        let mut order: Vec<SpriteOrderEntry> = priority
            .iter()
            .enumerate()
            .filter_map(|(slot, priority)| {
                priority.map(|priority| SpriteOrderEntry {
                    index: slot as u32,
                    priority: u32::from(priority),
                })
            })
            .collect();
        order.sort_by_key(|entry| {
            std::cmp::Reverse((entry.index as usize).wrapping_sub(first) & 0x7F)
        });
        order
    }

    /// Each line's window masks ([`WindowMasks`], numbered `frame`), from
    /// the settings each line of the held frame was drawn with when a
    /// redraw draws from the line table.
    pub fn window_masks(&self, frame: u64) -> WindowMasks {
        let walk = self.frame_lines();
        let mut out = WindowMasks {
            frame,
            from_line_states: walk.from_table,
            lines: Vec::with_capacity(walk.lines.len()),
            masks: Vec::with_capacity(walk.lines.len() * WINDOW_MASKS_PER_LINE * WINDOW_MASK_BYTES),
        };
        for (line, state) in &walk.lines {
            let mut masks = [[0u8; WINDOW_MASK_BYTES]; WINDOW_MASKS_PER_LINE];
            for dot in 0..256u16 {
                let (byte, bit) = (usize::from(dot >> 3), 1u8 << (dot & 7));
                for (layer, mask) in masks.iter_mut().enumerate().take(6) {
                    if window_inside(
                        dot,
                        state.window_enable[layer],
                        state.window_combine[layer],
                        state.window_one_left,
                        state.window_one_right,
                        state.window_two_left,
                        state.window_two_right,
                    ) {
                        mask[byte] |= bit;
                    }
                }
                let colour = masks[window_layer::COL][byte] & bit != 0;
                if col_region(state.col_main_mask, colour) {
                    masks[6][byte] |= bit;
                }
                if col_region(state.col_sub_mask, colour) {
                    masks[7][byte] |= bit;
                }
            }
            out.lines.push(*line);
            for mask in &masks {
                out.masks.extend_from_slice(mask);
            }
        }
        out
    }

    /// The offset-per-tile columns of the held frame ([`OffsetPerTileTable`],
    /// numbered `frame`): every line drawn in Mode 2, 4 or 6 outside
    /// forced blank and brightness 0, each background's tile columns as the
    /// fetch reads them, from the settings and the VRAM each line read when
    /// a redraw draws from the line table. The values are those the
    /// renderer fetched with (one rule, [`offset_per_tile`]); a column the
    /// line did not reach is marked.
    pub fn offset_per_tile_table(&self, frame: u64) -> OffsetPerTileTable {
        let walk = self.frame_lines();
        let mut vram = self.vram_by_line(walk.from_table);
        // The interlace weave is latched for the frame at its first line.
        let interlace = walk
            .lines
            .first()
            .map_or(self.interlace, |(_, state)| state.interlace);
        let wide = walk.lines.iter().any(|(_, state)| {
            state.bgmode == 6 && !state.display_disable && state.display_brightness != 0
        });
        let columns = if wide { COLUMNS_HIRES } else { COLUMNS_LORES };
        let mut out = OffsetPerTileTable {
            frame,
            from_line_states: walk.from_table,
            columns,
            ..OffsetPerTileTable::default()
        };
        for (index, (line, state)) in walk.lines.iter().enumerate() {
            if !matches!(state.bgmode, 2 | 4 | 6)
                || state.display_disable
                || state.display_brightness == 0
            {
                continue;
            }
            vram.advance_to(index);
            let hires = state.bgmode == 6;
            let line_width: u16 = if hires { 512 } else { 256 };
            // The background lines the line fetched: two in an interlaced
            // hires or pseudo-hires frame, which weaves its two fields into
            // two rows.
            let (rows, count) = if (hires || state.pseudo_hires) && interlace {
                let row = |grid_field| WovenRow::of(*line, grid_field).background_line(hires);
                ([row(0), row(1)], 2)
            } else {
                ([*line, 0], 1)
            };
            for &vy in &rows[..count] {
                out.lines.push(*line);
                for bg in 0..2usize {
                    let h_scroll = if hires {
                        state.bg_hoffset[bg] << 1
                    } else {
                        state.bg_hoffset[bg]
                    };
                    let fine = h_scroll & 7;
                    for k in 0..columns as u16 {
                        let x = if k == 0 { 0 } else { (k << 3) - fine };
                        let scroll = offset_per_tile(
                            vram.memory(),
                            state.bg_screen_addr[2],
                            state.bg_screen_size[2],
                            state.bg_hoffset[2],
                            state.bg_voffset[2],
                            state.bgmode,
                            bg as u8,
                            x,
                            h_scroll,
                            state.bg_voffset[bg],
                            vy,
                        );
                        out.scroll.extend([scroll.hoffset, scroll.voffset]);
                        let mut bits = 0;
                        if scroll.horizontal_replaced {
                            bits |= offset_replaced::HORIZONTAL;
                        }
                        if scroll.vertical_replaced {
                            bits |= offset_replaced::VERTICAL;
                        }
                        if x >= line_width {
                            bits |= offset_replaced::PAST_THE_LINE;
                        }
                        out.replaced.push(bits);
                    }
                }
            }
        }
        out
    }
}
