//! VDP debugger surface: the [`VideoDebugger`] trait implementation on
//! [`Vdp`].
//!
//! Mirrors the SNES PPU precedent (`snes1::ppu::debug`): the generic
//! `retro_core::video::VideoDebugger` interface lets the host drive
//! per-layer toggles and inspect tiles/palette/sprites/registers without
//! knowing it is talking to a Genesis VDP. Layer visibility routes
//! through the compositor's `debug_layer_disable` mask
//! (`composite_scanline`), so hiding a layer suppresses it from the live
//! frame without touching the game's real VDP registers.
//!
//! The non-layer getters delegate to the inherent debug helpers already
//! used by the WASM bindings (`get_vram`, `get_cram`,
//! `convert_genesis_color`, `Sprite::from_sat`, `Tile::decode`) so the
//! debug path can never diverge from the renderer's decode.
//!
//! The debugger's writes live here too: `write_vram_debug`,
//! `write_cram_debug` and `write_vsram_debug` put new contents straight
//! into video memory, to edit the graphics of a running or paused game,
//! and keep the renderer's own copies of that memory in step.
//!
//! The per-pixel debug log is here too: `set_debug_pixel` names a pixel,
//! and the compositor logs every candidate for it to stderr.

use std::ops::Range;

use retro_core::video::{
    bytes_from_words, words_from_bytes, ColourLevels, ExportError, IndexedImage, LayerData,
    LayerKind, LayerLayout, PaletteData, PictureRect, PixelImage, SpriteData, SpriteOrderEntry,
    SpriteTableEntry, TileData, TileDecodeRequest, TileSize, TileSource, VideoDebugger,
    VideoMemoryId, VideoMemoryInfo, VideoRegister,
};

use super::colour::COLOUR_LEVELS;
use super::picture::DOUBLE_CELL_ROWS;
use super::sprites::{link_chain, Sprite};
use super::tiles::{build_rgba_palette, Tile};
use super::{
    Interlace, Vdp, LAYER_COUNT, LAYER_PLANE_A, LAYER_PLANE_B, LAYER_SPRITES, LAYER_WINDOW,
};

/// 4bpp 8x8 tiles addressable across the 64 KiB VRAM (32 bytes/tile).
const VRAM_TILE_COUNT: u32 = 0x1_0000 / 32; // 2048
/// Genesis has 4 sixteen-colour palette lines in CRAM.
const PALETTE_LINES: u32 = 4;
/// Bytes of VRAM, the span a debugger write into it wraps around.
const VRAM_BYTES: usize = 0x1_0000;
/// Colours in CRAM: the four palette lines of sixteen.
const CRAM_COLOURS: usize = 64;
/// The bits of a CRAM word that hold a colour, three each of blue, green
/// and red (`0000 BBB0 GGG0 RRR0`): what the data port stores.
const CRAM_COLOUR_BITS: u16 = 0x0EEE;
/// Words of vertical scroll RAM the data port can address.
const VSRAM_WORDS: usize = 64;
/// The bits of a vertical scroll word the data port stores: 11.
const VSRAM_SCROLL_BITS: u16 = 0x07FF;

/// Human-readable names for the 24 VDP control registers (`$00..=$17`).
const REGISTER_NAMES: [&str; 24] = [
    "Mode Register 1",
    "Mode Register 2",
    "Plane A Name Table",
    "Window Name Table",
    "Plane B Name Table",
    "Sprite Table",
    "Sprite Pattern Generator",
    "Background Color",
    "Unused ($08)",
    "Unused ($09)",
    "H Interrupt Counter",
    "Mode Register 3",
    "Mode Register 4",
    "H Scroll Table",
    "Plane Pattern Generator",
    "Auto Increment",
    "Plane Size",
    "Window H Position",
    "Window V Position",
    "DMA Length Low",
    "DMA Length High",
    "DMA Source Low",
    "DMA Source Mid",
    "DMA Source High",
];

impl Vdp {
    /// Sprite Attribute Table base address in VRAM, derived from reg 5
    /// (H40 masks bit 0). Mirrors `rebuild_sprite_cache`.
    pub(super) fn sat_base(&self) -> u16 {
        let reg5 = self.registers[5];
        let is_h40 = (self.registers[12] & 0x01) != 0;
        if is_h40 {
            ((reg5 & 0x7E) as u16) << 9
        } else {
            ((reg5 & 0x7F) as u16) << 9
        }
    }

    /// The bytes of VRAM the renderer parses its sprite list from: the
    /// table register 5 places, 80 entries of 8 bytes in the 40-cell mode
    /// and 64 in the 32-cell mode, as `rebuild_sprite_cache` reads it.
    pub(super) fn sprite_table_bytes(&self) -> Range<usize> {
        let base = usize::from(self.sat_base());
        base..base + usize::from(self.sprite_table_slots()) * 8
    }

    /// Entries in the sprite table: 80 in the 40-cell mode, 64 in the
    /// 32-cell mode.
    pub(super) fn sprite_table_slots(&self) -> u8 {
        let is_h40 = (self.registers[12] & 0x01) != 0;
        if is_h40 {
            80
        } else {
            64
        }
    }

    /// The sprites of the table in the order the renderer takes them, each
    /// with its slot: the link chain from slot 0, stopped where the
    /// renderer stops it ([`link_chain`]).
    pub fn sprite_table_chain(&self) -> Vec<(u8, Sprite)> {
        link_chain(&self.vram, self.sat_base(), self.sprite_table_slots()).collect()
    }

    /// The box `sprite`, of slot `slot`, covers on the picture: its cells
    /// from its screen position, a dot and a line to a column and a row
    /// ([`Self::picture_grid`]). In interlace mode 2 its Y counts the
    /// picture's rows from 256 ([`Sprite::double_resolution_y`]) and its
    /// cells are 16 rows tall; the box takes every line one of its rows is
    /// on, both rows of each, so it holds the rows of every line that draws
    /// it, in either field.
    fn sprite_picture(&self, slot: u8, sprite: &Sprite) -> PictureRect {
        let grid = self.picture_grid();
        let x = i32::from(sprite.x);
        let width = u32::from(sprite.width_tiles) * 8;
        if self.interlace != Interlace::Double {
            let height = u32::from(sprite.height_tiles) * 8;
            return PictureRect::on_grid(grid, x, i32::from(sprite.y), width, height);
        }
        let top = i32::from(Sprite::double_resolution_y(
            &self.vram,
            self.sat_base(),
            slot,
        ));
        let bottom = top + i32::from(sprite.height_tiles) * i32::from(DOUBLE_CELL_ROWS);
        // The lines of the first row and of the row past the last, rounded
        // out to whole lines.
        let first_line = top.div_euclid(2);
        let end_line = (bottom + 1).div_euclid(2);
        PictureRect::on_grid(grid, x, first_line, width, (end_line - first_line) as u32)
    }

    /// The screen line the top row of `sprite`, of slot `slot`, is on: its
    /// `y`; in interlace mode 2, the line of the picture row its 10-bit Y
    /// names ([`Sprite::double_resolution_y`]), so the debugger's `y`
    /// counts lines in either mode.
    fn sprite_line(&self, slot: u8, sprite: &Sprite) -> i32 {
        if self.interlace == Interlace::Double {
            let top = Sprite::double_resolution_y(&self.vram, self.sat_base(), slot);
            i32::from(top).div_euclid(2)
        } else {
            i32::from(sprite.y)
        }
    }

    /// The VRAM byte where cell `pattern` starts: 32 bytes a cell, or in
    /// interlace mode 2 64, the cell's number the index's low 10 bits.
    fn cell_address(&self, pattern: u16) -> u32 {
        if self.interlace == Interlace::Double {
            u32::from(pattern & 0x03FF) * 64
        } else {
            u32::from(pattern) * 32
        }
    }

    /// `sprite` as interlace mode 2 draws it, a tight RGBA crop its width
    /// by its cells' 16 rows each (its height in lines times two): the
    /// cells taken as the renderer takes them (`Vdp::render_sprite_line`),
    /// column-major, cell `x * height + y` from the pattern's low 10 bits,
    /// the whole sprite flipped first and each cell's 16 rows inside it,
    /// in the screen's colours, a pixel of colour 0 left alpha 0.
    fn double_resolution_sprite_image(&self, sprite: &Sprite) -> PixelImage {
        let cell_rows = u32::from(DOUBLE_CELL_ROWS);
        let width = u32::from(sprite.width_tiles) * 8;
        let height = u32::from(sprite.height_tiles) * cell_rows;
        let palette = build_rgba_palette(&self.cram, sprite.palette);
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for ty in 0..sprite.height_tiles {
            for tx in 0..sprite.width_tiles {
                let src_tx = if sprite.flip_h {
                    sprite.width_tiles - 1 - tx
                } else {
                    tx
                };
                let src_ty = if sprite.flip_v {
                    sprite.height_tiles - 1 - ty
                } else {
                    ty
                };
                let offset = u16::from(src_tx) * u16::from(sprite.height_tiles) + u16::from(src_ty);
                let cell = ((sprite.pattern_base & 0x03FF) + offset) & 0x03FF;
                for py in 0..DOUBLE_CELL_ROWS as u8 {
                    for px in 0..8u8 {
                        let pixel = Vdp::read_cell_pixel::<DOUBLE_CELL_ROWS>(
                            &self.vram,
                            cell,
                            px,
                            py,
                            sprite.flip_h,
                            sprite.flip_v,
                        );
                        if pixel == 0 {
                            continue;
                        }
                        let dst_x = u32::from(tx) * 8 + u32::from(px);
                        let dst_y = u32::from(ty) * cell_rows + u32::from(py);
                        let at = ((dst_y * width + dst_x) * 4) as usize;
                        let (r, g, b, a) = palette[pixel as usize];
                        rgba[at..at + 4].copy_from_slice(&[r, g, b, a]);
                    }
                }
            }
        }
        PixelImage {
            width,
            height,
            rgba,
        }
    }

    /// Planes A and B's size in cells, from the plane-size register (16).
    fn plane_size_cells(&self) -> (u32, u32) {
        match self.registers[16] & 0x33 {
            0x00 => (32, 32),
            0x01 => (64, 32),
            0x03 => (128, 32),
            0x10 => (32, 64),
            0x11 => (64, 64),
            0x20 => (32, 32), // invalid horizontal size -> rows repeat
            0x30 => (32, 128),
            _ => (64, 64),
        }
    }

    /// Write `bytes` into VRAM from `byte_address` on, the way a debugger
    /// edits graphics: straight into memory, through no port, FIFO or DMA,
    /// so the access address and code, the status bits and the timing
    /// stay as they were. Byte `n` lands at `byte_address + n` in the
    /// layout [`Self::get_vram`] returns (the high byte of each word at
    /// its even address), so that read gives back what was written.
    ///
    /// An address past the end wraps to the start of the 64 KiB, and so
    /// does a run that crosses the end. A buffer longer than VRAM is cut
    /// to its size, so no byte is written twice; an empty one writes
    /// nothing.
    ///
    /// The renderer draws sprites from a list it parses out of the sprite
    /// table once a frame. A write that lands in the table marks that
    /// list stale, so the next line drawn, or a repaint, reads the sprites
    /// as they now are. A write elsewhere leaves the list alone.
    ///
    /// The write changes VRAM as it is now. While the line table is armed,
    /// a line drawn again from it reads VRAM as the line read it: the
    /// frame's own writes are undone, so a line drawn before the frame last
    /// wrote one of these bytes shows the frame's earlier value there, and
    /// every other line shows the debugger's. The sprite lists the table
    /// recorded are parsed again on the same terms. No line read the edit,
    /// so the table's `memory_exact` is false until the next picture begins.
    pub fn write_vram_debug(&mut self, byte_address: u32, bytes: &[u8]) {
        let sprite_table = self.sprite_table_bytes();
        let start = byte_address as usize % VRAM_BYTES;
        let mut sprite_table_written = false;
        for (offset, &value) in bytes.iter().take(VRAM_BYTES).enumerate() {
            let at = (start + offset) % VRAM_BYTES;
            self.vram[at] = value;
            sprite_table_written |= sprite_table.contains(&at);
        }
        if self.recording.is_some() {
            self.note_debugger_vram_store(start, bytes.len());
            if !bytes.is_empty() {
                self.note_debugger_video_edit();
            }
        }
        if sprite_table_written {
            self.invalidate_sprite_cache();
            self.reparse_recorded_sprite_lists();
        }
    }

    /// Write `colours` into CRAM from colour `colour_index` on (palette
    /// line `colour_index / 16`, entry `colour_index % 16`), the way a
    /// debugger edits a palette: straight into memory, through no port or
    /// DMA. Each word is stored as the data port stores it, keeping only
    /// the three bits it has for each of blue, green and red
    /// (`0000 BBB0 GGG0 RRR0`), so [`Self::get_cram`] gives back the
    /// written words with their other bits clear. The renderer's table of
    /// the colours as RGB follows at once, so the next line drawn, or a
    /// repaint, shows them.
    ///
    /// An index past the 64 colours wraps to the start, and so does a run
    /// that crosses the end. A buffer longer than CRAM is cut to its 64
    /// colours; an empty one writes nothing.
    ///
    /// While the line table is armed, every line it holds takes the new
    /// colour too, and so does every palette write it recorded of that
    /// colour, so a repaint shows the edit wherever the colour is used: a
    /// colour the frame changed part of the way down the screen takes the
    /// edit above and below the change alike. No line read the edit, so the
    /// table's `memory_exact` is false until the next picture begins.
    pub fn write_cram_debug(&mut self, colour_index: u32, colours: &[u16]) {
        let start = colour_index as usize % CRAM_COLOURS;
        for (offset, &colour) in colours.iter().take(CRAM_COLOURS).enumerate() {
            let index = (start + offset) % CRAM_COLOURS;
            let stored = colour & CRAM_COLOUR_BITS;
            self.cram[index] = stored;
            self.set_palette_colour(index, stored);
            self.patch_recorded_colour(index, stored);
        }
        if !colours.is_empty() {
            self.note_debugger_video_edit();
        }
    }

    /// Write `values` into the vertical scroll RAM from word `index` on,
    /// the way a debugger edits it: straight into memory, through no port
    /// or DMA. Each word keeps its low 11 bits, as the data port stores
    /// it, so [`Self::get_vsram`] gives back the written words with their
    /// other bits clear. The renderer reads the scroll as it draws each
    /// line, so the next line drawn, or a repaint, uses it.
    ///
    /// An index past the 64 words wraps to the start, and so does a run
    /// that crosses the end. A buffer longer than the 64 words is cut to
    /// them; an empty one writes nothing.
    ///
    /// While the line table is armed, every line it holds takes the new
    /// words too, so a repaint shows the edit on every line. No line read
    /// the edit, so the table's `memory_exact` is false until the next
    /// picture begins.
    pub fn write_vsram_debug(&mut self, index: u32, values: &[u16]) {
        let start = index as usize % VSRAM_WORDS;
        for (offset, &value) in values.iter().take(VSRAM_WORDS).enumerate() {
            let word = (start + offset) % VSRAM_WORDS;
            self.vsram[word] = value & VSRAM_SCROLL_BITS;
            self.patch_recorded_vertical_scroll(word, value & VSRAM_SCROLL_BITS);
        }
        if !values.is_empty() {
            self.note_debugger_video_edit();
        }
    }
}

impl VideoDebugger for Vdp {
    fn get_tile_data(&self, index: u32) -> Option<TileData> {
        if index >= VRAM_TILE_COUNT {
            return None;
        }
        let base = (index as usize) * 32;
        if base + 32 > self.vram.len() {
            return None;
        }
        let tile = Tile::decode(&self.vram[base..base + 32]);
        Some(TileData {
            index,
            width: 8,
            height: 8,
            pixels: tile.pixels.to_vec(),
        })
    }

    fn get_tile_count(&self) -> u32 {
        VRAM_TILE_COUNT
    }

    fn get_palette_data(&self) -> Vec<PaletteData> {
        (0..PALETTE_LINES)
            .map(|line| {
                let base = (line as usize) * 16;
                let colors = (0..16)
                    .map(|i| {
                        let raw = self.cram.get(base + i).copied().unwrap_or(0);
                        let (r, g, b) = Vdp::convert_genesis_color(raw);
                        (u32::from(r) << 24) | (u32::from(g) << 16) | (u32::from(b) << 8) | 0xFF
                    })
                    .collect();
                PaletteData {
                    index: line,
                    colors,
                }
            })
            .collect()
    }

    fn get_sprite_data(&self) -> Vec<SpriteData> {
        let is_h40 = (self.registers[12] & 0x01) != 0;
        let count = if is_h40 { 80 } else { 64 };
        let sat_base = self.sat_base();
        (0..count)
            .filter_map(|i| {
                let s = Sprite::from_sat(&self.vram, sat_base, i as u8)?;
                Some(SpriteData {
                    index: i,
                    x: i32::from(s.x),
                    y: self.sprite_line(i as u8, &s),
                    width: u32::from(s.width_tiles) * 8,
                    height: u32::from(s.height_tiles) * 8,
                    tile_index: u32::from(s.pattern_base),
                    palette: u32::from(s.palette),
                    priority: u32::from(s.priority),
                    flip_h: s.flip_h,
                    flip_v: s.flip_v,
                    picture: self.sprite_picture(i as u8, &s),
                })
            })
            .collect()
    }

    /// The sprites the held frame drew, back to front: each line's sprite
    /// evaluation run again from the line table where it holds the line, as
    /// [`Vdp::line_sprites`] runs it. In the link chain the renderer walks,
    /// a sprite taken earlier is drawn in front of the ones after it.
    fn resolved_sprite_render_order(&mut self) -> Vec<SpriteOrderEntry> {
        self.sprite_render_order()
    }

    fn get_layer_data(&self, layer_id: u32) -> Option<LayerData> {
        let id = layer_id as usize;
        if id >= LAYER_COUNT {
            return None;
        }
        let (cols, rows) = self.plane_size_cells();
        // The checkbox mirrors the debugger's per-layer hide toggle only,
        // NOT the game's display-enable bit (reg1 bit6). A blanked screen is
        // the game's own choice and is already visible on the canvas; folding
        // it in here would make every checkbox read *unchecked* whenever the
        // beam is paused with the display off, so hiding a layer would take a
        // confusing check-then-uncheck. Layers report enabled (checked) until
        // the user explicitly hides them.
        let visible = !self.layer_debug_disabled(id);
        let (name, width, height) = match id {
            LAYER_PLANE_A => ("Plane A", cols * 8, rows * 8),
            LAYER_PLANE_B => ("Plane B", cols * 8, rows * 8),
            LAYER_WINDOW => ("Window", self.display_width(), self.display_height()),
            LAYER_SPRITES => ("Sprites", self.display_width(), self.display_height()),
            _ => return None,
        };
        // Scroll is reported as 0: the Genesis playfield scroll is
        // per-line (H-scroll table) / per-column (VSRAM), so a single
        // layer-wide value would be misleading. The dedicated scroll
        // viewers expose the real values.
        Some(LayerData {
            id: layer_id,
            name: name.to_string(),
            width,
            height,
            scroll_x: 0,
            scroll_y: 0,
            enabled: visible,
        })
    }

    fn get_layer_count(&self) -> u32 {
        LAYER_COUNT as u32
    }

    fn toggle_layer(&mut self, layer_id: u32, visible: bool) {
        self.set_layer_debug_disabled(layer_id as usize, !visible);
    }

    fn get_vram(&self) -> &[u8] {
        &self.vram
    }

    fn get_registers(&self) -> Vec<VideoRegister> {
        REGISTER_NAMES
            .iter()
            .enumerate()
            .map(|(i, name)| VideoRegister {
                index: i as u32,
                name: (*name).to_string(),
                value: u32::from(self.registers[i]),
                description: String::new(),
            })
            .collect()
    }

    /// Decode a Genesis sprite (1-4 tiles per side) into a tight RGBA crop
    /// with hardware transparency (palette index 0 -> alpha 0).
    ///
    /// Tiles are picked as the renderer picks them (`Vdp::render_sprite_line`):
    /// column-major, tile `x * height + y` from the sprite's pattern, the
    /// whole sprite flipped first, then each tile's pixels flipped inside
    /// it ([`Tile::get_pixel`]). So the crop holds the pixels the renderer
    /// draws. Its colours are the ones the screen shows
    /// ([`build_rgba_palette`], through [`Vdp::convert_genesis_color`]).
    /// The buffer starts fully transparent, so any pixel left untouched
    /// (index 0, or a pattern address past VRAM) stays alpha 0.
    ///
    /// In interlace mode 2 the sprite is drawn as the picture shows it
    /// ([`Self::double_resolution_sprite_image`]): its cells 8 pixels by
    /// 16, the crop its width by its height in lines times two rows.
    fn export_sprite_image(&self, index: u32) -> Result<PixelImage, ExportError> {
        // 80 is the hardware per-frame sprite cap (and the max SAT slot the
        // host's sprite list walks). Guard before the u8 cast so a bogus
        // large index can't silently wrap onto a valid slot.
        if index >= 80 {
            return Err(ExportError::OutOfRange);
        }
        let sprite = Sprite::from_sat(&self.vram, self.sat_base(), index as u8)
            .ok_or(ExportError::OutOfRange)?;
        if self.interlace == Interlace::Double {
            return Ok(self.double_resolution_sprite_image(&sprite));
        }

        let width = u32::from(sprite.width_tiles) * 8;
        let height = u32::from(sprite.height_tiles) * 8;
        if width == 0 || height == 0 {
            return Err(ExportError::Empty);
        }

        let palette = build_rgba_palette(&self.cram, sprite.palette);
        let mut rgba = vec![0u8; (width * height * 4) as usize];

        for ty in 0..sprite.height_tiles {
            for tx in 0..sprite.width_tiles {
                let src_tx = if sprite.flip_h {
                    sprite.width_tiles - 1 - tx
                } else {
                    tx
                };
                let src_ty = if sprite.flip_v {
                    sprite.height_tiles - 1 - ty
                } else {
                    ty
                };
                let tile_offset = src_tx * sprite.height_tiles + src_ty;
                let pattern_addr = (sprite.pattern_base as usize + tile_offset as usize) * 32;
                if pattern_addr + 32 > self.vram.len() {
                    continue;
                }
                let tile = Tile::decode(&self.vram[pattern_addr..pattern_addr + 32]);

                for py in 0..8u32 {
                    for px in 0..8u32 {
                        let pixel =
                            tile.get_pixel(px as u8, py as u8, sprite.flip_h, sprite.flip_v);
                        // Index 0 is the hardware transparency key, leave it
                        // as the pre-zeroed (0,0,0,0) so the crop stays cut out.
                        if pixel == 0 {
                            continue;
                        }
                        let dst_x = u32::from(tx) * 8 + px;
                        let dst_y = u32::from(ty) * 8 + py;
                        let idx = ((dst_y * width + dst_x) * 4) as usize;
                        let (r, g, b, a) = palette[pixel as usize];
                        rgba[idx] = r;
                        rgba[idx + 1] = g;
                        rgba[idx + 2] = b;
                        rgba[idx + 3] = a;
                    }
                }
            }
        }

        Ok(PixelImage {
            width,
            height,
            rgba,
        })
    }

    /// Decode a contiguous run of 4bpp VRAM tiles into a vertically
    /// stacked indexed sheet (tile `i` occupies rows `[i*8, i*8+8)`).
    ///
    /// This is the bulk primitive the host uses to assemble a full-map
    /// plane: it reads the nametable itself and looks up each cell's tile
    /// block here, so the core only owns the 4bpp nibble decode (shared
    /// with [`Tile::decode`]) and never the per-cell palette/flip or the
    /// transparency policy. Genesis tiles are a flat 32-byte/tile run, so
    /// only `Vram` + `Px8` + 4bpp is meaningful; anything else is
    /// `Unsupported`.
    fn decode_tiles(&self, request: &TileDecodeRequest) -> Result<IndexedImage, ExportError> {
        if request.source != TileSource::Vram
            || request.bpp != 4
            || request.tile_size != TileSize::Px8
        {
            return Err(ExportError::Unsupported);
        }
        if request.count == 0 {
            return Err(ExportError::Empty);
        }
        let base_addr = request.base_addr as usize;
        let first = request.first_tile as usize;
        let count = request.count as usize;
        // 32 bytes per 8x8 4bpp tile. The whole run must fit in VRAM.
        let end = base_addr + (first + count) * 32;
        if end > self.vram.len() {
            return Err(ExportError::OutOfRange);
        }

        let width = 8u32;
        let height = request.count * 8;
        let mut pixels = vec![0u8; (width * height) as usize];
        for i in 0..count {
            let tbase = base_addr + (first + i) * 32;
            let tile = Tile::decode(&self.vram[tbase..tbase + 32]);
            let row0 = i * 8;
            for py in 0..8usize {
                for px in 0..8usize {
                    pixels[(row0 + py) * 8 + px] = tile.pixels[py * 8 + px];
                }
            }
        }

        Ok(IndexedImage {
            width,
            height,
            pixels,
            bpp: 4,
            transparent_index: Some(0),
        })
    }

    /// VRAM (byte addresses), CRAM (colours) and VSRAM (words), each
    /// keeping a word's high byte first, as the 68000 reads them.
    fn video_memories(&self) -> Vec<VideoMemoryInfo> {
        let memory = |id, name, size: usize, address_unit| VideoMemoryInfo {
            id,
            name,
            size: size as u32,
            address_unit,
            little_endian: false,
        };
        vec![
            memory(VideoMemoryId::Vram, "VRAM", VRAM_BYTES, 1),
            memory(VideoMemoryId::Palette, "CRAM", CRAM_COLOURS * 2, 2),
            memory(VideoMemoryId::VerticalScroll, "VSRAM", VSRAM_WORDS * 2, 2),
        ]
    }

    fn read_video_memory(&self, id: VideoMemoryId) -> Option<Vec<u8>> {
        match id {
            VideoMemoryId::Vram => Some(self.vram.clone()),
            VideoMemoryId::Palette => Some(bytes_from_words(&self.cram, false)),
            VideoMemoryId::VerticalScroll => Some(bytes_from_words(&self.vsram, false)),
            VideoMemoryId::SpriteTable => None,
        }
    }

    /// Through [`Vdp::write_vram_debug`], [`Vdp::write_cram_debug`] and
    /// [`Vdp::write_vsram_debug`]; the sprite table lives in VRAM.
    fn write_video_memory(&mut self, id: VideoMemoryId, offset: u32, bytes: &[u8]) -> bool {
        match id {
            VideoMemoryId::Vram => self.write_vram_debug(offset, bytes),
            VideoMemoryId::Palette => {
                self.write_cram_debug(offset / 2, &words_from_bytes(bytes, false));
            }
            VideoMemoryId::VerticalScroll => {
                self.write_vsram_debug(offset / 2, &words_from_bytes(bytes, false));
            }
            VideoMemoryId::SpriteTable => return false,
        }
        true
    }

    /// The link chain the renderer walks ([`Vdp::sprite_table_chain`]),
    /// each entry linking to the one after it; one size of sprite.
    fn sprite_table(&self) -> Vec<SpriteTableEntry> {
        let chain = self.sprite_table_chain();
        chain
            .iter()
            .enumerate()
            .map(|(position, (slot, sprite))| SpriteTableEntry {
                index: u32::from(*slot),
                x: i32::from(sprite.x),
                y: self.sprite_line(*slot, sprite),
                width: u32::from(sprite.width_tiles) * 8,
                height: u32::from(sprite.height_tiles) * 8,
                tile_index: u32::from(sprite.pattern_base),
                palette: u32::from(sprite.palette),
                priority: u32::from(sprite.priority),
                flip_h: sprite.flip_h,
                flip_v: sprite.flip_v,
                next: chain.get(position + 1).map(|(next, _)| u32::from(*next)),
                tile_address: self.cell_address(sprite.pattern_base),
                large: false,
                picture: self.sprite_picture(*slot, sprite),
            })
            .collect()
    }

    /// Planes A and B, the window and the sprites: 4 bits a pixel in 8 by
    /// 8 cells from VRAM byte 0, each cell naming its own palette; the
    /// scroll as the layer list reports it (0: it is set per line and per
    /// column).
    fn layer_layout(&self, layer_id: u32) -> Option<LayerLayout> {
        let (cols, rows) = self.plane_size_cells();
        let window_cols = if (self.registers[12] & 0x01) != 0 {
            64
        } else {
            32
        };
        let (kind, map_base, map_width, map_height) = match layer_id as usize {
            LAYER_PLANE_A => (LayerKind::TileMap, self.get_plane_a_nametable(), cols, rows),
            LAYER_PLANE_B => (LayerKind::TileMap, self.get_plane_b_nametable(), cols, rows),
            LAYER_WINDOW => (
                LayerKind::Window,
                self.get_window_nametable(),
                window_cols,
                32,
            ),
            LAYER_SPRITES => (LayerKind::Sprites, self.sat_base(), 0, 0),
            _ => return None,
        };
        Some(LayerLayout {
            id: layer_id,
            kind,
            map_base: u32::from(map_base),
            map_width,
            map_height,
            character_base: 0,
            bits_per_pixel: 4,
            cell_width: 8,
            cell_height: 8,
            first_colour: 0,
            scroll_x: 0,
            scroll_y: 0,
        })
    }

    /// The fifteen levels a channel of a colour the VDP draws takes, shadow
    /// and highlight included ([`COLOUR_LEVELS`]).
    fn colour_levels(&self) -> ColourLevels {
        COLOUR_LEVELS
    }
}

impl Vdp {
    /// Set a pixel coordinate to debug. During render_scanline, all composition
    /// candidates for this pixel will be logged to stderr.
    pub fn set_debug_pixel(&mut self, x: u16, y: u16) {
        self.debug_pixel = Some((x, y));
    }

    /// Clear the debug pixel.
    pub fn clear_debug_pixel(&mut self) {
        self.debug_pixel = None;
    }

    /// Detailed per-pixel composition diagnostic (stderr).
    /// Called from render_scanline only when debug_pixel is set and the
    /// current pixel matches. Kept separate to avoid bloating the hot loop.
    #[cold]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn log_debug_pixel(
        &self,
        x: usize,
        line: u16,
        in_window: bool,
        addr_b: usize,
        plane_a_nametable: u16,
        plane_b_nametable: u16,
        window_nametable: u16,
        pf_shift: u32,
        pf_col_mask: u16,
        pf_row_mask: u16,
        h_scroll_mode: u8,
        v_scroll_mode: u8,
        is_h40: bool,
        hscroll_base: usize,
        h_scroll_a: u16,
        h_scroll_b: u16,
        v_scroll_a_full: u16,
        v_scroll_b_full: u16,
        plane_w_px: i32,
        plane_b_color: Option<(u8, u8, u8)>,
        plane_b_pri: bool,
        plane_a_color: Option<(u8, u8, u8)>,
        plane_a_pri: bool,
        merged_color: Option<(u8, u8, u8)>,
        merged_pri: bool,
        has_sprite: bool,
        final_color: (u8, u8, u8),
        is_high_priority: bool,
        sh_operator: u8,
        backdrop_color: (u8, u8, u8),
    ) {
        let xu16 = x as u16;
        let s_ci = self.sprite_line_color[x];
        let s_pal = self.sprite_line_palette[x];
        let s_pri = self.sprite_line_priority[x];

        let winner = if !has_sprite {
            if merged_color.is_some() {
                "plane"
            } else {
                "backdrop"
            }
        } else if sh_operator != 0 {
            "sh_operator"
        } else if s_pri {
            "sprite(hi)"
        } else if merged_pri {
            "plane(hi)"
        } else {
            "sprite(lo)"
        };

        eprintln!(
            "=== DEBUG PIXEL ({}, {}) frame={} ===",
            xu16, line, self.timing.frame_count
        );

        eprintln!("  --- VDP Registers ---");
        eprintln!(
            "  r2(PlaneA)=0x{:02X} -> base=0x{:04X}  r4(PlaneB)=0x{:02X} -> base=0x{:04X}",
            self.registers[2], plane_a_nametable, self.registers[4], plane_b_nametable
        );
        let (scroll_width, scroll_height) = self.get_scroll_size();
        eprintln!(
            "  r3(Window)=0x{:02X} -> base=0x{:04X}  r16(scroll_size)=0x{:02X} -> {}x{} tiles (shift={} col_mask=0x{:02X} row_mask=0x{:03X})",
            self.registers[3], window_nametable, self.registers[16], scroll_width, scroll_height,
            pf_shift, pf_col_mask, pf_row_mask
        );
        eprintln!(
            "  r11(mode3)=0x{:02X} -> hscroll_mode={} vscroll_mode={}",
            self.registers[11], h_scroll_mode, v_scroll_mode
        );
        eprintln!(
            "  r12(mode4)=0x{:02X} -> H40={}  r13(hscroll_base)=0x{:02X} -> 0x{:04X}",
            self.registers[12], is_h40, self.registers[13], hscroll_base
        );
        eprintln!(
            "  r15(auto_inc)=0x{:02X}  r17(WinH)=0x{:02X}  r18(WinV)=0x{:02X}",
            self.registers[15], self.registers[17], self.registers[18]
        );

        eprintln!("  --- Window Selection ---");
        let reg17 = self.registers[17];
        let reg18 = self.registers[18];
        let v_pos_w = (reg18 & 0x1F) as u16 * 8;
        let v_down_w = (reg18 & 0x80) != 0;
        let h_pos_w = (reg17 & 0x1F) as u16;
        let h_right_w = (reg17 & 0x80) != 0;
        eprintln!(
            "  window: v_pos={} v_down={} h_pos={} h_right={} -> in_window={}",
            v_pos_w, v_down_w, h_pos_w, h_right_w, in_window
        );

        // Plane B geometry
        eprintln!("  --- Plane B Geometry ---");
        let dbg_vscroll_b = if v_scroll_mode == 0 {
            v_scroll_b_full
        } else {
            let col = (xu16 / 16) as usize;
            let vi = col * 2 + 1;
            if vi < self.vsram.len() {
                self.vsram[vi] & 0x03FF
            } else {
                0
            }
        };
        let dbg_sx_b = (xu16 as i32 - h_scroll_b as i32).rem_euclid(plane_w_px) as u16;
        let dbg_vline_b = (line as u32 + dbg_vscroll_b as u32) & pf_row_mask as u32;
        let dbg_tr_b = (dbg_vline_b >> 3) as u16;
        let dbg_tc_b = (dbg_sx_b / 8) & pf_col_mask;
        eprintln!(
            "  hscroll_b={} vscroll_b={} scrolled=({},{}) tile=({},{}) px=({},{})",
            h_scroll_b,
            dbg_vscroll_b,
            dbg_sx_b,
            dbg_vline_b,
            dbg_tc_b,
            dbg_tr_b,
            dbg_sx_b % 8,
            dbg_vline_b & 7
        );
        eprintln!(
            "  nt_base=0x{:04X} nt_off=0x{:04X} -> addr=0x{:04X}",
            plane_b_nametable,
            (((dbg_tr_b as u32) << pf_shift) & 0x1FC0) as u16 + dbg_tc_b * 2,
            addr_b
        );
        let word_b = if addr_b + 1 < self.vram.len() {
            let w = ((self.vram[addr_b] as u16) << 8) | (self.vram[addr_b + 1] as u16);
            eprintln!(
                "  raw_bytes=[0x{:02X}, 0x{:02X}] -> word=0x{:04X}",
                self.vram[addr_b],
                self.vram[addr_b + 1],
                w
            );
            w
        } else {
            0
        };
        let ci_b = Self::read_tile_pixel_flipped(
            &self.vram,
            word_b & 0x07FF,
            (dbg_sx_b % 8) as u8,
            (dbg_vline_b & 7) as u8,
            (word_b & 0x0800) != 0,
            (word_b & 0x1000) != 0,
        );
        eprintln!(
            "  decoded: pat={} pal={} flipH={} flipV={} pri={} ci={} color={:?}",
            word_b & 0x07FF,
            (word_b >> 13) & 0x03,
            (word_b & 0x0800) != 0,
            (word_b & 0x1000) != 0,
            plane_b_pri,
            ci_b,
            plane_b_color
        );

        // Plane A / Window geometry
        eprintln!("  --- Plane A Geometry ---");
        if in_window {
            let wtx = xu16 / 8;
            let wty = line / 8;
            let win_width_tiles: u16 = if is_h40 { 64 } else { 32 };
            let wnt_off = (wty * win_width_tiles + wtx) * 2;
            let waddr = (window_nametable + wnt_off) as usize;
            eprintln!(
                "  SOURCE=WINDOW  tile=({},{}) window_base=0x{:04X} addr=0x{:04X}",
                wtx, wty, window_nametable, waddr
            );
            if waddr + 1 < self.vram.len() {
                let w = ((self.vram[waddr] as u16) << 8) | (self.vram[waddr + 1] as u16);
                eprintln!(
                    "  raw_bytes=[0x{:02X}, 0x{:02X}] -> word=0x{:04X}",
                    self.vram[waddr],
                    self.vram[waddr + 1],
                    w
                );
                eprintln!(
                    "  decoded: pat={} pal={} flipH={} flipV={} pri={} color={:?}",
                    w & 0x07FF,
                    (w >> 13) & 0x03,
                    (w & 0x0800) != 0,
                    (w & 0x1000) != 0,
                    plane_a_pri,
                    plane_a_color
                );
            }
        } else {
            let dbg_vscroll_a = if v_scroll_mode == 0 {
                v_scroll_a_full
            } else {
                let col = (xu16 / 16) as usize;
                let vi = col * 2;
                if vi < self.vsram.len() {
                    self.vsram[vi] & 0x03FF
                } else {
                    0
                }
            };
            let dbg_sx_a = (xu16 as i32 - h_scroll_a as i32).rem_euclid(plane_w_px) as u16;
            let dbg_vline_a = (line as u32 + dbg_vscroll_a as u32) & pf_row_mask as u32;
            let dbg_tr_a = (dbg_vline_a >> 3) as u16;
            let dbg_tc_a = (dbg_sx_a / 8) & pf_col_mask;
            eprintln!("  SOURCE=PLANE_A  hscroll_a={} vscroll_a={} scrolled=({},{}) tile=({},{}) px=({},{})",
                h_scroll_a, dbg_vscroll_a, dbg_sx_a, dbg_vline_a,
                dbg_tc_a, dbg_tr_a, dbg_sx_a % 8, dbg_vline_a & 7);
            let nt_off_a = (((dbg_tr_a as u32) << pf_shift) & 0x1FC0) as u16 + dbg_tc_a * 2;
            let addr_a = (plane_a_nametable + nt_off_a) as usize;
            eprintln!(
                "  nt_base=0x{:04X} nt_off=0x{:04X} -> addr=0x{:04X}",
                plane_a_nametable, nt_off_a, addr_a
            );
            if addr_a + 1 < self.vram.len() {
                let w = ((self.vram[addr_a] as u16) << 8) | (self.vram[addr_a + 1] as u16);
                eprintln!(
                    "  raw_bytes=[0x{:02X}, 0x{:02X}] -> word=0x{:04X}",
                    self.vram[addr_a],
                    self.vram[addr_a + 1],
                    w
                );
                eprintln!(
                    "  decoded: pat={} pal={} flipH={} flipV={} pri={} color={:?}",
                    w & 0x07FF,
                    (w >> 13) & 0x03,
                    (w & 0x0800) != 0,
                    (w & 0x1000) != 0,
                    plane_a_pri,
                    plane_a_color
                );
            }
        }

        eprintln!("  --- Merge & Composite ---");
        eprintln!("  merged: color={:?} pri={}", merged_color, merged_pri);
        eprintln!(
            "  sprite: ci={} pal={} pri={} has_sprite={}",
            s_ci, s_pal, s_pri, has_sprite
        );
        eprintln!(
            "  winner={} final_color={:?} is_high_pri={} sh_op={}",
            winner, final_color, is_high_priority, sh_operator
        );
        eprintln!("  backdrop: {:?}", backdrop_color);
        eprintln!("=== END DEBUG PIXEL ===");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_count_is_four() {
        let vdp = Vdp::new();
        assert_eq!(vdp.get_layer_count(), LAYER_COUNT as u32);
    }

    #[test]
    fn layer_data_names_in_order() {
        let vdp = Vdp::new();
        assert_eq!(vdp.get_layer_data(0).unwrap().name, "Plane A");
        assert_eq!(vdp.get_layer_data(1).unwrap().name, "Plane B");
        assert_eq!(vdp.get_layer_data(2).unwrap().name, "Window");
        assert_eq!(vdp.get_layer_data(3).unwrap().name, "Sprites");
        assert!(vdp.get_layer_data(4).is_none());
    }

    #[test]
    fn layer_enabled_tracks_debug_mask_not_display_bit() {
        // The layer checkbox must mirror the debugger's per-layer hide toggle,
        // never the game's display-enable bit. A fresh VDP has the display OFF
        // (reg1 bit6 clear) yet nothing is hidden, so every layer must report
        // enabled and the checkboxes start checked.
        let mut vdp = Vdp::new();
        assert_eq!(vdp.registers[1] & 0x40, 0, "display starts off");
        for id in 0..LAYER_COUNT as u32 {
            assert!(
                vdp.get_layer_data(id).unwrap().enabled,
                "layer {id} should report enabled with the display off",
            );
        }
        // Hiding a layer via the debugger flips only that layer off, still
        // independent of the (off) display bit.
        vdp.set_layer_debug_disabled(LAYER_PLANE_B, true);
        assert!(!vdp.get_layer_data(LAYER_PLANE_B as u32).unwrap().enabled);
        assert!(vdp.get_layer_data(LAYER_PLANE_A as u32).unwrap().enabled);
    }

    #[test]
    fn toggle_layer_drives_the_disable_mask() {
        let mut vdp = Vdp::new();
        assert!(vdp.get_layer_data(0).unwrap().enabled, "starts visible");
        vdp.toggle_layer(0, false);
        assert!(vdp.layer_debug_disabled(LAYER_PLANE_A));
        assert!(
            !vdp.get_layer_data(0).unwrap().enabled,
            "hidden after toggle"
        );
        vdp.toggle_layer(0, true);
        assert!(!vdp.layer_debug_disabled(LAYER_PLANE_A));
        assert!(vdp.get_layer_data(0).unwrap().enabled, "visible again");
    }

    #[test]
    fn out_of_range_toggle_is_ignored() {
        let mut vdp = Vdp::new();
        vdp.toggle_layer(99, false);
        assert!(!vdp.layer_debug_disabled(99));
    }

    #[test]
    fn tile_count_and_bounds() {
        let vdp = Vdp::new();
        assert_eq!(vdp.get_tile_count(), VRAM_TILE_COUNT);
        assert!(vdp.get_tile_data(0).is_some());
        assert!(vdp.get_tile_data(VRAM_TILE_COUNT).is_none());
    }

    #[test]
    fn palette_data_has_four_lines() {
        let vdp = Vdp::new();
        let pal = vdp.get_palette_data();
        assert_eq!(pal.len(), PALETTE_LINES as usize);
        assert_eq!(pal[0].colors.len(), 16);
    }

    #[test]
    fn registers_snapshot_has_24_named_entries() {
        let vdp = Vdp::new();
        let regs = vdp.get_registers();
        assert_eq!(regs.len(), 24);
        assert_eq!(regs[0].name, "Mode Register 1");
        assert_eq!(regs[2].name, "Plane A Name Table");
    }

    #[test]
    fn vram_view_is_64k() {
        let vdp = Vdp::new();
        assert_eq!(VideoDebugger::get_vram(&vdp).len(), 0x1_0000);
    }

    #[test]
    fn export_sprite_image_crops_tight_and_keys_index0_transparent() {
        let mut vdp = Vdp::new();
        // SAT base 0 (reg5 = 0), H32 path (reg12 bit0 clear).
        vdp.registers[5] = 0;
        vdp.registers[12] = 0;

        // SAT slot 0: a 1x1 sprite using pattern 2, palette 0.
        // word0 = Y (0x0080), word1 = size/link 0, word2 = pattern 2,
        // word3 = X (0x0080).
        vdp.vram[0] = 0x00;
        vdp.vram[1] = 0x80;
        vdp.vram[2] = 0x00;
        vdp.vram[3] = 0x00;
        vdp.vram[4] = 0x00;
        vdp.vram[5] = 0x02;
        vdp.vram[6] = 0x00;
        vdp.vram[7] = 0x80;

        // Pattern 2 lives at byte 64. Row 0, byte 0 = 0x01 -> left pixel
        // index 0 (transparent), right pixel index 1 (coloured).
        vdp.vram[64] = 0x01;

        // Palette 0, colour 1 = red (3-bit 7 -> 255).
        vdp.cram[1] = 0x000E;

        let img = vdp.export_sprite_image(0).expect("sprite 0 exports");
        assert_eq!(img.width, 8);
        assert_eq!(img.height, 8);
        assert_eq!(img.rgba.len(), 8 * 8 * 4);

        // Pixel (0,0): index 0 -> fully transparent.
        assert_eq!(&img.rgba[0..4], &[0, 0, 0, 0]);
        // Pixel (1,0): index 1 -> opaque red.
        assert_eq!(&img.rgba[4..8], &[255, 0, 0, 255]);
    }

    #[test]
    fn export_sprite_image_out_of_range_slot_errors() {
        let vdp = Vdp::new();
        // Slot 200 is past the 80-entry SAT window.
        assert_eq!(vdp.export_sprite_image(200), Err(ExportError::OutOfRange));
    }

    #[test]
    fn resolved_render_order_lists_the_drawn_sprites_against_the_link_chain() {
        let mut vdp = Vdp::new();
        vdp.registers[1] = 0x44; // display on (bit 6) + mode 5 (bit 2)
        vdp.registers[5] = 0; // SAT base 0
        vdp.registers[12] = 0; // H32 (64-slot window)

        // One cell each on lines 0 to 7 (Y 0x80), at the table's X `x`,
        // linking to `link`; the pattern word carries the priority bit.
        let place = |vdp: &mut Vdp, slot: usize, x: u16, link: u8, pattern: u16| {
            let at = slot * 8;
            vdp.vram[at..at + 2].copy_from_slice(&0x0080u16.to_be_bytes());
            vdp.vram[at + 2] = 0x00; // one cell by one
            vdp.vram[at + 3] = link;
            vdp.vram[at + 4..at + 6].copy_from_slice(&pattern.to_be_bytes());
            vdp.vram[at + 6..at + 8].copy_from_slice(&x.to_be_bytes());
        };
        // Link chain 0 -> 3 -> 1 -> 4 -> end. Slot 4 lies left of the
        // screen (screen X -112), so no column of it is drawn, and slot 2
        // is not in the chain at all.
        place(&mut vdp, 0, 0x80, 3, 0x8000);
        place(&mut vdp, 3, 0x90, 1, 0x0000);
        place(&mut vdp, 1, 0xA0, 4, 0x0000);
        place(&mut vdp, 4, 0x10, 0, 0x0000);
        place(&mut vdp, 2, 0xB0, 0, 0x0000);

        let order = vdp.resolved_sprite_render_order();
        let indices: Vec<u32> = order.iter().map(|e| e.index).collect();
        // Chain front-to-back is [0, 3, 1] among the sprites drawn; paint
        // order is the reverse (the renderer paints the on-line list
        // back-to-front).
        assert_eq!(indices, [1u32, 3, 0]);
        let top = order.last().expect("non-empty order");
        assert_eq!(top.index, 0, "chain head paints last (on top)");
        assert_eq!(top.priority, 1, "priority bit carried");
        assert!(order[..2].iter().all(|e| e.priority == 0));

        // With the display off no line draws a sprite.
        vdp.registers[1] = 0x04;
        assert!(vdp.resolved_sprite_render_order().is_empty());
    }

    #[test]
    fn the_sprite_table_chain_stops_where_the_renderer_stops() {
        let mut vdp = Vdp::new();
        vdp.registers[5] = 0; // SAT base 0
        vdp.registers[12] = 0; // 32 cells: 64 slots
                               // Slot 0 links to 70, past a table of 64.
        vdp.vram[3] = 70;
        vdp.vram[70 * 8 + 3] = 0;

        let slots = |vdp: &Vdp| -> Vec<u8> {
            vdp.sprite_table_chain()
                .iter()
                .map(|(slot, _)| *slot)
                .collect()
        };
        assert_eq!(slots(&vdp), [0], "the walk ends before slot 70");
        vdp.rebuild_sprite_cache();
        assert_eq!(vdp.sprite_cache.len(), 1, "as the renderer's list does");

        // 40 cells: 80 slots, so slot 70 is in the table.
        vdp.registers[12] = 0x81;
        assert_eq!(slots(&vdp), [0, 70]);

        // A chain that loops is cut at the table's size in the 32-cell mode.
        vdp.registers[12] = 0;
        vdp.vram[3] = 1;
        vdp.vram[8 + 3] = 1; // slot 1 links to itself
        assert_eq!(vdp.sprite_table_chain().len(), 64);
    }

    #[test]
    fn the_sprite_table_is_the_chain_each_entry_linking_to_the_next() {
        let mut vdp = Vdp::new();
        vdp.registers[5] = 0; // SAT base 0
        vdp.registers[12] = 0x81; // 40 cells
        vdp.vram[3] = 70; // slot 0 links to 70
        vdp.vram[70 * 8 + 3] = 5; // slot 70 links to 5
        vdp.vram[70 * 8 + 4] = 0x80 | 0x20 | 0x01; // priority, palette 1, pattern 0x100
        vdp.vram[5 * 8 + 3] = 0; // slot 5 ends the chain

        let table = vdp.sprite_table();
        let chain = vdp.sprite_table_chain();
        assert_eq!(
            table.iter().map(|entry| entry.index).collect::<Vec<_>>(),
            [0, 70, 5]
        );
        assert_eq!(
            table.iter().map(|entry| entry.next).collect::<Vec<_>>(),
            [Some(70), Some(5), None]
        );
        let sprites = vdp.get_sprite_data();
        for (entry, (slot, sprite)) in table.iter().zip(&chain) {
            let data = sprites
                .iter()
                .find(|s| s.index == u32::from(*slot))
                .unwrap();
            assert_eq!(
                (
                    entry.x,
                    entry.y,
                    entry.width,
                    entry.height,
                    entry.tile_index
                ),
                (data.x, data.y, data.width, data.height, data.tile_index)
            );
            assert_eq!(entry.tile_address, u32::from(sprite.pattern_base) * 32);
            assert!(!entry.large);
        }
        assert_eq!((table[1].palette, table[1].priority), (1, 1));
        assert_eq!(table[1].tile_address, 0x100 * 32);
    }

    #[test]
    fn the_layers_read_their_maps_where_the_registers_say() {
        let mut vdp = Vdp::new();
        vdp.registers[2] = 0x30; // plane A at $C000
        vdp.registers[4] = 0x07; // plane B at $E000
        vdp.registers[3] = 0x2C; // window at $B000
        vdp.registers[5] = 0x7E; // the sprite table at $FC00
        vdp.registers[12] = 0x81; // 40 cells
        vdp.registers[16] = 0x01; // planes of 64 by 32 cells
        let layout = |id| vdp.layer_layout(id).unwrap();
        assert_eq!(
            (layout(0).kind, layout(0).map_base, layout(0).map_width),
            (LayerKind::TileMap, 0xC000, 64)
        );
        assert_eq!(layout(1).map_base, u32::from(vdp.get_plane_b_nametable()));
        assert_eq!(
            (layout(2).kind, layout(2).map_base, layout(2).map_width),
            (LayerKind::Window, 0xB000, 64)
        );
        assert_eq!(
            (layout(3).kind, layout(3).map_base, layout(3).map_width),
            (LayerKind::Sprites, u32::from(vdp.sat_base()), 0)
        );
        assert!(vdp.layer_layout(4).is_none());
    }

    #[test]
    fn the_word_memories_are_read_and_written_high_byte_first() {
        let mut vdp = Vdp::new();
        assert!(vdp.write_video_memory(VideoMemoryId::Palette, 3, &[0x0E, 0x24, 0xFF]));
        assert_eq!(vdp.get_cram()[1], 0x0E24, "from the word the offset is in");
        assert_eq!(
            vdp.read_video_memory(VideoMemoryId::Palette).unwrap()[2..4],
            [0x0E, 0x24]
        );
        assert!(vdp.write_video_memory(
            VideoMemoryId::VerticalScroll,
            126,
            &[0xFF, 0xFF, 0x01, 0x23]
        ));
        assert_eq!((vdp.get_vsram()[63], vdp.get_vsram()[0]), (0x07FF, 0x0123));
        assert_eq!(vdp.read_video_memory(VideoMemoryId::SpriteTable), None);
        assert!(!vdp.write_video_memory(VideoMemoryId::SpriteTable, 0, &[1]));
        assert_eq!(
            vdp.video_memories()
                .iter()
                .map(|memory| (memory.id, memory.size, memory.address_unit))
                .collect::<Vec<_>>(),
            [
                (VideoMemoryId::Vram, 0x1_0000, 1),
                (VideoMemoryId::Palette, 128, 2),
                (VideoMemoryId::VerticalScroll, 128, 2)
            ]
        );
    }

    #[test]
    fn resolved_render_order_matches_framebuffer_overlap() {
        // Parity guard: the sprite the order reports on top (last)
        // must be the sprite that actually wins the overlap pixel in the
        // renderer's framebuffer.
        let mut vdp = Vdp::new();
        vdp.registers[1] = 0x44; // display enable (bit6) + mode5 (bit2)
        vdp.registers[12] = 0x00; // H32
        vdp.registers[5] = 0x00; // SAT base 0

        // Isolate sprites: hide the planes/window so only OBJ composites at
        // (0,0) over the backdrop.
        vdp.set_layer_debug_disabled(LAYER_PLANE_A, true);
        vdp.set_layer_debug_disabled(LAYER_PLANE_B, true);
        vdp.set_layer_debug_disabled(LAYER_WINDOW, true);

        // Two 1x1 sprites stacked at screen (0,0) (raw 0x80,0x80), linked
        // 0 -> 1 -> end so slot 0 is the chain head (drawn on top).
        // slot 0: pattern 2, palette 0, link 1.
        vdp.vram[0] = 0x00;
        vdp.vram[1] = 0x80; // Y raw 0x80 -> screen 0
        vdp.vram[2] = 0x00;
        vdp.vram[3] = 0x01; // size 0, link 1
        vdp.vram[4] = 0x00;
        vdp.vram[5] = 0x02; // pattern 2, palette 0
        vdp.vram[6] = 0x00;
        vdp.vram[7] = 0x80; // X raw 0x80 -> screen 0
                            // slot 1: pattern 3, palette 0, link 0 (end).
        vdp.vram[8] = 0x00;
        vdp.vram[9] = 0x80;
        vdp.vram[10] = 0x00;
        vdp.vram[11] = 0x00;
        vdp.vram[12] = 0x00;
        vdp.vram[13] = 0x03;
        vdp.vram[14] = 0x00;
        vdp.vram[15] = 0x80;

        // Pattern 2 (byte 64): pixel (0,0) = index 1. Pattern 3 (byte 96):
        // pixel (0,0) = index 2. (4bpp: high nibble = even x.)
        vdp.vram[64] = 0x10;
        vdp.vram[96] = 0x20;

        // Palette 0: index 1 = red, index 2 = blue (distinct).
        vdp.cram[1] = 0x000E; // red
        vdp.cram[2] = 0x0E00; // blue
        vdp.rebuild_cram_rgb();
        let top_rgb = vdp.cram_rgb[1];
        let bottom_rgb = vdp.cram_rgb[2];
        assert_ne!(top_rgb, bottom_rgb, "test colours must differ");

        vdp.rerender_current_frame();
        let fb = (vdp.framebuffer[0], vdp.framebuffer[1], vdp.framebuffer[2]);

        let order = vdp.resolved_sprite_render_order();
        let top = order.last().expect("non-empty order");
        assert_eq!(top.index, 0, "order reports slot 0 on top");
        assert_eq!(fb, top_rgb, "framebuffer overlap shows the top sprite");
        assert_ne!(fb, bottom_rgb, "lower sprite did not win the overlap");
    }

    /// Build a deterministic, fully-coloured 1x1 sprite (pattern 2, palette
    /// 0) whose 8x8 pattern walks every palette index, so the exported crop
    /// exercises the palette map and the index-0 transparency key across the
    /// whole tile. Used by the golden-image test; the byte-level test builds
    /// its own sprite.
    fn vdp_with_rainbow_sprite() -> Vdp {
        let mut vdp = Vdp::new();
        vdp.registers[5] = 0; // SAT base 0
        vdp.registers[12] = 0; // H32

        // SAT slot 0: 1x1 sprite, pattern 2, palette 0, at (0x80, 0x80).
        vdp.vram[0] = 0x00;
        vdp.vram[1] = 0x80;
        vdp.vram[2] = 0x00;
        vdp.vram[3] = 0x00;
        vdp.vram[4] = 0x00;
        vdp.vram[5] = 0x02;
        vdp.vram[6] = 0x00;
        vdp.vram[7] = 0x80;

        // Pattern 2 at byte 64: pixel (x,y) = (x + y) % 16. Two 4-bit pixels
        // per byte (high nibble = even x, low nibble = odd x).
        let base = 2 * 32;
        for y in 0..8usize {
            for bx in 0..4usize {
                let v0 = ((bx * 2 + y) % 16) as u8;
                let v1 = ((bx * 2 + 1 + y) % 16) as u8;
                vdp.vram[base + y * 4 + bx] = (v0 << 4) | v1;
            }
        }

        // Palette 0: 16 distinct 9-bit BGR colours (index 0 stays the
        // transparency key regardless of its colour word).
        for i in 0..16u16 {
            let r = (i & 0x7) << 1;
            let g = ((i + 5) & 0x7) << 1;
            let b = ((i + 10) & 0x7) << 1;
            vdp.cram[i as usize] = (b << 8) | (g << 4) | r;
        }
        vdp
    }

    /// Golden-image lock for the sprite-export primitive.
    ///
    /// Encodes the exported RGBA to PNG via the `image` crate and compares
    /// the decoded pixels against a committed reference under
    /// `tests/golden/`. PNG is lossless, so decoded bytes equal the source
    /// RGBA exactly.
    ///
    /// Regen path: delete the file, or run with `SG1_UPDATE_GOLDEN=1`:
    ///   `SG1_UPDATE_GOLDEN=1 cargo test -p sg1-core export_sprite_image_matches_golden`
    #[test]
    fn export_sprite_image_matches_golden() {
        use std::path::Path;

        let vdp = vdp_with_rainbow_sprite();
        let img = vdp.export_sprite_image(0).expect("sprite 0 exports");
        assert_eq!((img.width, img.height), (8, 8));

        let golden_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/golden/sprite_export_basic.png"
        );
        let encoded = image::RgbaImage::from_raw(img.width, img.height, img.rgba.clone())
            .expect("rgba buffer matches dimensions");

        if std::env::var("SG1_UPDATE_GOLDEN").is_ok() || !Path::new(golden_path).exists() {
            if let Some(dir) = Path::new(golden_path).parent() {
                std::fs::create_dir_all(dir).expect("create golden dir");
            }
            encoded.save(golden_path).expect("write golden png");
        }

        let golden = image::open(golden_path)
            .expect("open golden png")
            .to_rgba8();
        assert_eq!(golden.dimensions(), (img.width, img.height));
        assert_eq!(
            golden.as_raw(),
            &img.rgba,
            "exported sprite drifted from golden"
        );
    }

    fn vram_tile_request(first_tile: u32, count: u32) -> TileDecodeRequest {
        TileDecodeRequest {
            source: TileSource::Vram,
            base_addr: 0,
            first_tile,
            count,
            bpp: 4,
            tile_size: TileSize::Px8,
        }
    }

    #[test]
    fn decode_tiles_stacks_indexed_tiles() {
        let mut vdp = Vdp::new();
        // Tile 0 row 0: byte 0x12 -> indices 1,2. Tile 1 row 0: byte 0x34 ->
        // indices 3,4. (32 bytes per 8x8 4bpp tile, so tile 1 starts at 32.)
        vdp.vram[0] = 0x12;
        vdp.vram[32] = 0x34;

        let img = vdp
            .decode_tiles(&vram_tile_request(0, 2))
            .expect("two tiles decode");

        assert_eq!(img.width, 8);
        assert_eq!(img.height, 16); // 2 tiles stacked vertically
        assert_eq!(img.bpp, 4);
        assert_eq!(img.transparent_index, Some(0));
        assert_eq!(img.pixels.len(), 8 * 16);

        // Tile 0 occupies rows 0..8.
        assert_eq!(img.pixels[0], 1);
        assert_eq!(img.pixels[1], 2);
        // Tile 1 occupies rows 8..16 -> row 8, columns 0/1.
        assert_eq!(img.pixels[8 * 8], 3);
        assert_eq!(img.pixels[8 * 8 + 1], 4);
    }

    #[test]
    fn decode_tiles_honors_first_tile_offset() {
        let mut vdp = Vdp::new();
        // Put a marker in tile 5 (byte 5*32 = 160) and decode just that tile.
        vdp.vram[160] = 0xF0; // index 15, then 0
        let img = vdp
            .decode_tiles(&vram_tile_request(5, 1))
            .expect("single offset tile decodes");
        assert_eq!((img.width, img.height), (8, 8));
        assert_eq!(img.pixels[0], 15);
        assert_eq!(img.pixels[1], 0);
    }

    #[test]
    fn decode_tiles_empty_count_is_empty() {
        let vdp = Vdp::new();
        assert_eq!(
            vdp.decode_tiles(&vram_tile_request(0, 0)),
            Err(ExportError::Empty)
        );
    }

    #[test]
    fn decode_tiles_past_vram_is_out_of_range() {
        let vdp = Vdp::new();
        // VRAM holds 2048 tiles; asking for tile 2048 runs past the end.
        assert_eq!(
            vdp.decode_tiles(&vram_tile_request(2048, 1)),
            Err(ExportError::OutOfRange)
        );
    }

    #[test]
    fn decode_tiles_rejects_non_genesis_params() {
        let vdp = Vdp::new();
        let mut req = vram_tile_request(0, 1);
        req.bpp = 8;
        assert_eq!(vdp.decode_tiles(&req), Err(ExportError::Unsupported));
        let mut req = vram_tile_request(0, 1);
        req.tile_size = TileSize::Px16;
        assert_eq!(vdp.decode_tiles(&req), Err(ExportError::Unsupported));
    }

    // -- debugger writes into video memory ------------------------------

    #[test]
    fn a_vram_write_reads_back_and_leaves_the_ports_alone() {
        let mut vdp = Vdp::new();
        // A VRAM write armed on the data port.
        vdp.code = 0x01;
        vdp.address = 0x1234;
        let status = vdp.status_word();
        let registers = *vdp.registers();

        vdp.write_vram_debug(0x2000, &[0xDE, 0xAD, 0xBE, 0xEF]);

        let vram = vdp.get_vram();
        assert_eq!(&vram[0x2000..0x2004], &[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(vram[0x1FFF], 0, "the byte before is untouched");
        assert_eq!(vram[0x2004], 0, "the byte after is untouched");
        assert_eq!(
            (vdp.address, vdp.code),
            (0x1234, 0x01),
            "the port's address and code stay"
        );
        assert_eq!(vdp.fifo_entries, 0, "nothing went through the FIFO");
        assert!(!vdp.dma_pending && !vdp.dma_busy, "no DMA started");
        assert_eq!(vdp.status_word(), status);
        assert_eq!(*vdp.registers(), registers);
    }

    #[test]
    fn a_vram_write_wraps_past_the_end_and_is_cut_to_the_memory() {
        let mut vdp = Vdp::new();
        // A run that crosses the end goes on from the start.
        vdp.write_vram_debug(0xFFFE, &[1, 2, 3, 4]);
        assert_eq!(&vdp.get_vram()[0xFFFE..], &[1, 2]);
        assert_eq!(&vdp.get_vram()[..2], &[3, 4]);

        // An address past the end wraps.
        vdp.write_vram_debug(0x1_0005, &[9]);
        assert_eq!(vdp.get_vram()[5], 9);

        // A buffer two bytes longer than VRAM: its first 64 KiB land once
        // each, from byte 2 round to byte 1, and the two left over are
        // dropped instead of written over the first two.
        let pattern = |i: usize| (i % 251) as u8;
        let long: Vec<u8> = (0..VRAM_BYTES + 2).map(pattern).collect();
        vdp.write_vram_debug(2, &long);
        let vram = vdp.get_vram();
        assert_eq!(vram[2], pattern(0));
        assert_eq!(vram[3], pattern(1));
        assert!((0..VRAM_BYTES).all(|i| vram[(2 + i) % VRAM_BYTES] == pattern(i)));
    }

    #[test]
    fn an_empty_vram_write_changes_nothing() {
        let mut vdp = Vdp::new();
        vdp.vram[0x100] = 0x5A;
        vdp.rebuild_sprite_cache();
        let before = vdp.get_vram().to_vec();

        // 0x100 is inside the sprite table (base 0 after a reset).
        vdp.write_vram_debug(0x100, &[]);

        assert_eq!(vdp.get_vram(), &before[..]);
        assert!(
            vdp.sprite_cache_valid,
            "the sprite list is not marked stale"
        );
    }

    /// A VDP in the 32-cell mode with the display on and the planes
    /// hidden, its sprite table at 0xF000, and sprite 0 an 8x8 sprite at
    /// the top left of the screen drawn with pattern 2, all colour 1.
    fn vdp_with_one_sprite() -> Vdp {
        let mut vdp = Vdp::new();
        vdp.registers[1] = 0x44; // display on (bit 6) + mode 5 (bit 2)
        vdp.registers[12] = 0x00; // 32 cells
        vdp.registers[5] = 0x78; // sprite table at 0x78 << 9 = 0xF000
        vdp.set_layer_debug_disabled(LAYER_PLANE_A, true);
        vdp.set_layer_debug_disabled(LAYER_PLANE_B, true);
        vdp.set_layer_debug_disabled(LAYER_WINDOW, true);
        // Sprite 0: Y and X 0x80 (screen 0), one cell, link 0, pattern 2.
        vdp.vram[0xF000..0xF008].copy_from_slice(&[0x00, 0x80, 0x00, 0x00, 0x00, 0x02, 0x00, 0x80]);
        // Pattern 2, bytes 64..96: every pixel colour 1.
        vdp.vram[64..96].fill(0x11);
        vdp
    }

    #[test]
    fn a_write_into_the_sprite_table_reaches_the_next_line_drawn() {
        let mut vdp = vdp_with_one_sprite();
        vdp.composite_scanline(0);
        assert_eq!(vdp.line_index[0], 1, "the sprite is drawn at x 0");
        assert_eq!(vdp.line_index[16], 0, "the backdrop at x 16");

        // The renderer draws from the list it parsed: X moved to 0x90
        // (screen 16) behind its back leaves the sprite where it was.
        vdp.vram[0xF006..0xF008].copy_from_slice(&[0x00, 0x90]);
        vdp.composite_scanline(0);
        assert_eq!(vdp.line_index[0], 1, "the parsed list still has x 0");

        // The same bytes through the debugger write reach the list.
        vdp.write_vram_debug(0xF006, &[0x00, 0x90]);
        vdp.composite_scanline(0);
        assert_eq!(vdp.line_index[0], 0, "x 0 shows the backdrop now");
        assert_eq!(vdp.line_index[16], 1, "the sprite is drawn at x 16");
    }

    #[test]
    fn only_a_write_into_the_sprite_table_marks_the_list_stale() {
        let mut vdp = vdp_with_one_sprite();
        let marks_stale = |vdp: &mut Vdp, at: u32, len: usize| {
            vdp.rebuild_sprite_cache();
            vdp.write_vram_debug(at, &vec![0; len]);
            !vdp.sprite_cache_valid
        };

        // 32 cells: 64 entries of 8 bytes, 0xF000..0xF200.
        assert!(
            !marks_stale(&mut vdp, 0xEFFF, 1),
            "the byte before the table"
        );
        assert!(
            marks_stale(&mut vdp, 0xEFFF, 2),
            "a run into its first byte"
        );
        assert!(marks_stale(&mut vdp, 0xF1FF, 1), "its last byte");
        assert!(!marks_stale(&mut vdp, 0xF200, 1), "the byte after it");

        // 40 cells: 80 entries, 0xF000..0xF280.
        vdp.registers[12] = 0x81;
        assert!(marks_stale(&mut vdp, 0xF27F, 1), "the last of 80 entries");
        assert!(!marks_stale(&mut vdp, 0xF280, 1), "the byte after them");

        // A run that wraps past the end reaches a table at the start.
        vdp.registers[5] = 0x00;
        assert!(!marks_stale(&mut vdp, 0xFFFF, 1), "the last byte of VRAM");
        assert!(marks_stale(&mut vdp, 0xFFFF, 2), "and on into byte 0");
    }

    #[test]
    fn a_cram_write_reads_back_and_recolours_the_next_repaint() {
        let mut vdp = Vdp::new();
        vdp.registers[1] = 0x44; // display on: an empty screen is all backdrop
        vdp.registers[7] = 0x05; // the backdrop is colour 5 of line 0
        let pixel = |vdp: &Vdp| (vdp.framebuffer[0], vdp.framebuffer[1], vdp.framebuffer[2]);
        vdp.rerender_current_frame();
        assert_eq!(pixel(&vdp), (0, 0, 0), "colour 5 starts black");

        vdp.write_cram_debug(5, &[0x000E]);

        assert_eq!(vdp.get_cram()[5], 0x000E);
        vdp.rerender_current_frame();
        assert_eq!(
            pixel(&vdp),
            Vdp::convert_genesis_color(0x000E),
            "the repaint shows the new colour"
        );
    }

    #[test]
    fn a_cram_write_keeps_the_colour_bits_wraps_and_is_cut_to_the_memory() {
        let mut vdp = Vdp::new();
        // Colour 63, then on round to colour 0. The bits outside
        // 0000 BBB0 GGG0 RRR0 are dropped, as the data port drops them.
        vdp.write_cram_debug(63, &[0xFFFF, 0x0123]);
        assert_eq!(vdp.get_cram()[63], 0x0EEE);
        assert_eq!(vdp.get_cram()[0], 0x0022);
        assert_eq!(vdp.cram_rgb[63], Vdp::convert_genesis_color(0x0EEE));
        assert_eq!(vdp.cram_rgb[0], Vdp::convert_genesis_color(0x0022));

        // An index past the 64 colours wraps.
        vdp.write_cram_debug(64 + 7, &[0x0042]);
        assert_eq!(vdp.get_cram()[7], 0x0042);

        // Seventy colours from colour 10: the first 64 land once each, the
        // six left over are dropped instead of written over 10..16.
        let long: Vec<u16> = (0..70)
            .map(|i| if i < 64 { 0x0002 } else { 0x0E00 })
            .collect();
        vdp.write_cram_debug(10, &long);
        assert!(vdp.get_cram().iter().all(|&colour| colour == 0x0002));

        let before = vdp.get_cram().to_vec();
        vdp.write_cram_debug(3, &[]);
        assert_eq!(
            vdp.get_cram(),
            &before[..],
            "an empty write changes nothing"
        );
    }

    #[test]
    fn a_vsram_write_keeps_11_bits_wraps_and_is_cut_to_the_memory() {
        let mut vdp = Vdp::new();
        vdp.write_vsram_debug(63, &[0xFFFF, 0x0123]);
        assert_eq!(vdp.get_vsram()[63], 0x07FF);
        assert_eq!(vdp.get_vsram()[0], 0x0123);

        vdp.write_vsram_debug(64 + 5, &[0x0042]);
        assert_eq!(vdp.get_vsram()[5], 0x0042);

        let long: Vec<u16> = (0..70)
            .map(|i| if i < 64 { 0x0001 } else { 0x0002 })
            .collect();
        vdp.write_vsram_debug(10, &long);
        assert!(vdp.get_vsram().iter().all(|&value| value == 0x0001));

        let before = vdp.get_vsram().to_vec();
        vdp.write_vsram_debug(3, &[]);
        assert_eq!(
            vdp.get_vsram(),
            &before[..],
            "an empty write changes nothing"
        );
    }

    #[test]
    fn a_vsram_write_scrolls_the_next_line_drawn() {
        let mut vdp = Vdp::new();
        vdp.registers[1] = 0x44; // display on + mode 5
        vdp.registers[2] = 0x30; // plane A at 0xC000: all tile 0, transparent
        vdp.registers[4] = 0x07; // plane B at 0xE000

        // Tile 1 all colour 1, tile 2 all colour 2.
        vdp.vram[0x20..0x40].fill(0x11);
        vdp.vram[0x40..0x60].fill(0x22);
        // Plane B, 32 cells wide after a reset: cell row 0 is tile 1, row
        // 1, 64 bytes on, tile 2.
        for column in 0..32 {
            vdp.vram[0xE000 + column * 2 + 1] = 0x01;
            vdp.vram[0xE040 + column * 2 + 1] = 0x02;
        }
        vdp.composite_scanline(0);
        assert_eq!(vdp.line_plane_b[0], 1, "line 0 shows cell row 0");

        // Plane B's whole-screen vertical scroll is word 1.
        vdp.write_vsram_debug(1, &[8]);
        vdp.composite_scanline(0);
        assert_eq!(vdp.line_plane_b[0], 2, "eight lines down, cell row 1");
    }
}
