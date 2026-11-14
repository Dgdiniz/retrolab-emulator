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

use std::ops::Range;

use retro_core::video::{
    ExportError, IndexedImage, LayerData, PaletteData, PixelImage, SpriteData, SpriteOrderEntry,
    TileData, TileDecodeRequest, TileSize, TileSource, VideoDebugger, VideoRegister,
};

use super::sprites::Sprite;
use super::tiles::{build_rgba_palette, Tile};
use super::{Vdp, LAYER_COUNT, LAYER_PLANE_A, LAYER_PLANE_B, LAYER_SPRITES, LAYER_WINDOW};

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
    fn sat_base(&self) -> u16 {
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
    fn sprite_table_bytes(&self) -> Range<usize> {
        let is_h40 = (self.registers[12] & 0x01) != 0;
        let entries = if is_h40 { 80 } else { 64 };
        let base = usize::from(self.sat_base());
        base..base + entries * 8
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
    pub fn write_vram_debug(&mut self, byte_address: u32, bytes: &[u8]) {
        let sprite_table = self.sprite_table_bytes();
        let start = byte_address as usize % VRAM_BYTES;
        let mut sprite_table_written = false;
        for (offset, &value) in bytes.iter().take(VRAM_BYTES).enumerate() {
            let at = (start + offset) % VRAM_BYTES;
            self.vram[at] = value;
            sprite_table_written |= sprite_table.contains(&at);
        }
        if sprite_table_written {
            self.invalidate_sprite_cache();
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
    pub fn write_cram_debug(&mut self, colour_index: u32, colours: &[u16]) {
        let start = colour_index as usize % CRAM_COLOURS;
        for (offset, &colour) in colours.iter().take(CRAM_COLOURS).enumerate() {
            let index = (start + offset) % CRAM_COLOURS;
            let stored = colour & CRAM_COLOUR_BITS;
            self.cram[index] = stored;
            self.cram_rgb[index] = Self::convert_genesis_color(stored);
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
    pub fn write_vsram_debug(&mut self, index: u32, values: &[u16]) {
        let start = index as usize % VSRAM_WORDS;
        for (offset, &value) in values.iter().take(VSRAM_WORDS).enumerate() {
            self.vsram[(start + offset) % VSRAM_WORDS] = value & VSRAM_SCROLL_BITS;
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
                    y: i32::from(s.y),
                    width: u32::from(s.width_tiles) * 8,
                    height: u32::from(s.height_tiles) * 8,
                    tile_index: u32::from(s.pattern_base),
                    palette: u32::from(s.palette),
                    priority: u32::from(s.priority),
                    flip_h: s.flip_h,
                    flip_v: s.flip_v,
                })
            })
            .collect()
    }

    fn resolved_sprite_render_order(&self) -> Vec<SpriteOrderEntry> {
        let is_h40 = (self.registers[12] & 0x01) != 0;
        let max_sprites: u8 = if is_h40 { 80 } else { 64 };
        let sat_base = self.sat_base();

        // Walk the SAT link chain exactly like `SpriteCache::build` so the
        // order can never diverge from the renderer: the chain head is the
        // highest-priority sprite (drawn on top of the ones it links to).
        let mut chain: Vec<SpriteOrderEntry> = Vec::with_capacity(max_sprites as usize);
        let mut current = 0u8;
        let mut count = 0u8;
        loop {
            if count >= max_sprites {
                break;
            }
            if let Some(sprite) = Sprite::from_sat(&self.vram, sat_base, current) {
                chain.push(SpriteOrderEntry {
                    index: u32::from(current),
                    priority: u32::from(sprite.priority),
                });
                count += 1;
                let link = sprite.link;
                // Stop on a null link or one past the valid SAT range, the
                // same guard the renderer's chain walk uses.
                if link == 0 || link >= max_sprites {
                    break;
                }
                current = link;
            } else {
                break;
            }
        }

        // The renderer paints the per-line list in reverse (lowest priority
        // first) so the chain head wins overlaps (`vdp/mod.rs`: "Render
        // sprites in REVERSE order"). Back-to-front paint order is therefore
        // the reverse of link-chain order.
        chain.reverse();
        chain
    }

    fn get_layer_data(&self, layer_id: u32) -> Option<LayerData> {
        let id = layer_id as usize;
        if id >= LAYER_COUNT {
            return None;
        }
        // Playfield pixel dimensions from the plane-size register (reg 16).
        let (cols, rows) = match self.registers[16] & 0x33 {
            0x00 => (32u32, 32u32),
            0x01 => (64, 32),
            0x03 => (128, 32),
            0x10 => (32, 64),
            0x11 => (64, 64),
            0x20 => (32, 32), // invalid horizontal size -> rows repeat
            0x30 => (32, 128),
            _ => (64, 64),
        };
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

    fn repaint(&mut self) {
        self.rerender_current_frame();
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
    /// Tile selection and flips mirror [`Sprite::render`] exactly so the
    /// cut-out is pixel-identical to what the compositor draws: the
    /// destination tile `(tx, ty)` reads the source tile chosen by the
    /// flip-aware column-major offset, and intra-tile pixels are flipped
    /// via [`Tile::get_pixel`]. The buffer starts fully transparent, so any
    /// pixel left untouched (index 0, or a pattern address past VRAM)
    /// stays alpha 0.
    fn export_sprite_image(&self, index: u32) -> Result<PixelImage, ExportError> {
        // 80 is the hardware per-frame sprite cap (and the max SAT slot the
        // host's sprite list walks). Guard before the u8 cast so a bogus
        // large index can't silently wrap onto a valid slot.
        if index >= 80 {
            return Err(ExportError::OutOfRange);
        }
        let sprite = Sprite::from_sat(&self.vram, self.sat_base(), index as u8)
            .ok_or(ExportError::OutOfRange)?;

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

        // Palette 0, colour 1 = red (3-bit 7 -> 252).
        vdp.cram[1] = 0x000E;

        let img = vdp.export_sprite_image(0).expect("sprite 0 exports");
        assert_eq!(img.width, 8);
        assert_eq!(img.height, 8);
        assert_eq!(img.rgba.len(), 8 * 8 * 4);

        // Pixel (0,0): index 0 -> fully transparent.
        assert_eq!(&img.rgba[0..4], &[0, 0, 0, 0]);
        // Pixel (1,0): index 1 -> opaque red.
        assert_eq!(&img.rgba[4..8], &[252, 0, 0, 255]);
    }

    #[test]
    fn export_sprite_image_out_of_range_slot_errors() {
        let vdp = Vdp::new();
        // Slot 200 is past the 80-entry SAT window.
        assert_eq!(vdp.export_sprite_image(200), Err(ExportError::OutOfRange));
    }

    #[test]
    fn resolved_render_order_reverses_the_link_chain() {
        let mut vdp = Vdp::new();
        vdp.registers[5] = 0; // SAT base 0
        vdp.registers[12] = 0; // H32 (64-slot window)

        // Link chain 0 -> 3 -> 1 -> end. word1 (bytes 2-3) holds the link
        // in bits 0-6; the other words can stay zero (from_sat still parses).
        let set_link = |vdp: &mut Vdp, slot: usize, link: u8| {
            vdp.vram[slot * 8 + 2] = 0x00;
            vdp.vram[slot * 8 + 3] = link;
        };
        set_link(&mut vdp, 0, 3);
        set_link(&mut vdp, 3, 1);
        set_link(&mut vdp, 1, 0);
        // Give the chain head the priority bit (word2 bit 15) so we can
        // confirm it is carried through.
        vdp.vram[4] = 0x80;

        let order = vdp.resolved_sprite_render_order();
        let indices: Vec<u32> = order.iter().map(|e| e.index).collect();
        // Chain front-to-back priority is [0, 3, 1]; paint order is the
        // reverse (the renderer paints the on-line list back-to-front).
        assert_eq!(indices, [1u32, 3, 0]);
        let top = order.last().expect("non-empty order");
        assert_eq!(top.index, 0, "chain head paints last (on top)");
        assert_eq!(top.priority, 1, "priority bit carried");
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
