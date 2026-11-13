//! PPU debugger surface: the [`VideoDebugger`] trait implementation
//! plus the shared 15-bit-BGR -> RGBA helper used by both the trait and
//! the viewer exporters (`SnesEmulator::export_*`).
//!
//! Following the SG1 precedent, `DebuggerCore::video_debugger()` returns
//! `None` (the debugger struct doesn't own the PPU); the host reaches
//! the live PPU through `SnesEmulator::video_debugger()` instead, which
//! hands back the `&mut dyn VideoDebugger` implemented here on [`Ppu`].
//!
//! The tile viewer decodes VRAM as 4bpp 8x8 tiles, the most common
//! SNES format, across the whole 64 KiB word space (1024 tiles per
//! 32 KiB half, 2048 total). The palette viewer groups CGRAM into the
//! 16 sixteen-colour palettes a 4bpp screen uses. Both reuse the pure
//! decoders in [`crate::ppu::tile`] / [`crate::ppu::sprite`] so the
//! debug path can never diverge from the renderer.

use retro_core::video::{
    ExportError, IndexedImage, LayerData, PaletteData, PixelImage, SpriteData, SpriteOrderEntry,
    TileData, TileDecodeRequest, TileSize, TileSource, VideoDebugger, VideoRegister,
};

use crate::ppu::render::{bpp_for_bg, palette_offset_for_bg};
use crate::ppu::sprite::{
    parse_sprite, sprite_row_in_sprite, sprite_size, sprite_tile_number, SpriteAttrs,
};
use crate::ppu::tile::{decode_tile_2bpp_row, decode_tile_4bpp_row, decode_tile_8bpp_row};
use crate::ppu::{Ppu, LAYER_COUNT, LAYER_OBJ};

/// Number of 4bpp 8x8 tiles addressable in the 64 KiB VRAM word space
/// (16 words per tile x 2048 tiles = 32768 words = the full bank).
pub(crate) const VRAM_4BPP_TILE_COUNT: u32 = 2048;
/// 4bpp tiles addressable from a single 32 KiB-byte tile-data base
/// before the `tile_index & 0x3FF` mask wraps.
const TILES_PER_HALF: u32 = 1024;

/// Expand a 15-bit BGR555 colour (`0BBBBBGGGGGRRRRR`) to RGBA8888 at
/// full brightness. `[r, g, b, 0xFF]` with the canonical 5->8 expansion
/// `(c5 << 3) | (c5 >> 2)`. Brightness scaling is intentionally omitted
/// so palette / tile / OAM viewers show the stored colours, not the
/// current master-brightness-dimmed output.
#[inline]
#[must_use]
pub(crate) fn bgr15_to_rgba8(bgr15: u16) -> [u8; 4] {
    let expand = |c5: u16| -> u8 {
        let c = (c5 & 0x1F) as u8;
        (c << 3) | (c >> 2)
    };
    [expand(bgr15), expand(bgr15 >> 5), expand(bgr15 >> 10), 0xFF]
}

/// Pack an RGBA8888 byte quad into a `u32` (`0xRRGGBBAA`).
#[inline]
#[must_use]
pub(crate) fn rgba8_to_u32([r, g, b, a]: [u8; 4]) -> u32 {
    (u32::from(r) << 24) | (u32::from(g) << 16) | (u32::from(b) << 8) | u32::from(a)
}

/// Normalize a 9-bit OBJ X (`0..=511`) to signed screen-space.
///
/// The upper half of the 9-bit range is the wrapped negative region: a
/// sprite at `x = 500` (width 64) spans `500..564`, wrapping past 512 to
/// sit at screen `-12..52`. Mapping `x >= 256` to `x - 512` recovers that
/// signed position so the host can place every sprite without re-deriving
/// the wrap (the "partially off the left" case). Y has no such bit.
#[inline]
#[must_use]
fn normalize_obj_x(x: u16) -> i32 {
    if x >= 256 {
        i32::from(x) - 512
    } else {
        i32::from(x)
    }
}

/// Which VRAM character region [`Ppu::export_tile_image`] decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum TileSetSource {
    /// Start from VRAM word 0, the common background character base.
    Bg,
    /// Start from the OBJ character-data base (`OBSEL`, `$2101`).
    Obj,
}

/// A decoded RGBA8888 image produced by a viewer exporter. `rgba` holds
/// `width * height * 4` bytes, row-major, top-left origin.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ViewerImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One OAM sprite flattened for the sprite viewer (`export_oam_sprites`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct SpriteDescriptor {
    /// OAM slot `0..=127`.
    pub index: u8,
    /// 9-bit X position (`0..=511`).
    pub x: u16,
    /// Top scanline (raw OAM Y byte).
    pub y: u8,
    /// Resolved width in pixels (`8..=64`).
    pub width: u8,
    /// Resolved height in pixels (`8..=64`).
    pub height: u8,
    /// 8-bit character (tile) number.
    pub tile: u8,
    /// 3-bit palette select (`0..=7`).
    pub palette: u8,
    /// 2-bit priority (`0..=3`).
    pub priority: u8,
    pub hflip: bool,
    pub vflip: bool,
    /// `true` when the per-sprite size bit selects the large size.
    pub large: bool,
}

/// Per-background addressing + format snapshot for the host's
/// full-map raw-layer assembler.
///
/// Everything here is read straight off live PPU state for one BG; the
/// host pairs it with `get_vram()` + `get_cgram()` and `decode_tiles`
/// to rebuild the whole tilemap as an image, owning colour and the
/// transparency policy itself (the same OSS/host split Genesis uses).
/// `bpp` is `0` when the BG does not draw under the current mode
/// (e.g. BG3 used only as an offset table), the host treats that as
/// "no full-map for this layer" and falls back to the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct BgLayout {
    /// BG index `0..=3`.
    pub bg: u8,
    /// Current BG mode `0..=7` (`$2105` bits 0-2).
    pub bg_mode: u8,
    /// Tilemap base, byte address into VRAM (`$2107..=$210A`).
    pub tilemap_base_byte: u32,
    /// Character-data base, byte address into VRAM (`$210B/$210C`).
    pub char_base_byte: u32,
    /// Raw 2-bit screen-size selector (`$2107..` bits 0-1).
    pub screen_size: u8,
    /// Tilemap width in cells (`32` or `64`).
    pub width_cells: u32,
    /// Tilemap height in cells (`32` or `64`).
    pub height_cells: u32,
    /// Bits per pixel under the current mode (`2`/`4`/`8`, or `0`).
    pub bpp: u8,
    /// `true` when this BG uses 16x16 character tiles (`$2105`).
    pub tile_size16: bool,
    /// CGRAM colour-region base (Mode 0 splits BGs into 32-colour
    /// quadrants; every other mode starts at 0).
    pub palette_base: u8,
    /// Horizontal scroll (`$210D`).
    pub scroll_x: u16,
    /// Vertical scroll (`$210E`).
    pub scroll_y: u16,
}

/// Where the renderer finds the sprites' tiles, and the two sizes a
/// sprite can take, read off OBSEL (`$2101`) and the OBJ interlace bit
/// of SETINI (`$2133` bit 1).
///
/// A sprite's tiles sit in a table of 16 x 16 characters, 4bpp, 16 VRAM
/// words each. The character byte names the top-left tile (its low
/// nibble the column, its high nibble the row) and the sprite's other
/// tiles are its neighbours, wrapping inside the table on both axes
/// ([`sprite_tile_number`]). A sprite whose name-select bit is set (bit 0
/// of its fourth OAM byte) reads the second table. A horizontal flip
/// reads the columns right to left; a vertical flip mirrors the rows in
/// blocks as tall as the sprite is wide ([`sprite_row_in_sprite`]).
///
/// The host pairs this with `get_oam()` and a 4bpp decode of the whole
/// VRAM (`decode_tiles`) to read a sprite's tiles the way the renderer
/// reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct SpriteTileSettings {
    /// Width in pixels of a small sprite (size bit clear).
    pub small_width: u8,
    /// Height in pixels of a small sprite. Under OBJ interlace the
    /// small height of size selectors 6 and 7 is 16, as in the sprite
    /// list.
    pub small_height: u8,
    /// Width in pixels of a large sprite (size bit set).
    pub large_width: u8,
    /// Height in pixels of a large sprite.
    pub large_height: u8,
    /// VRAM word address of character 0 of the first table, wrapped
    /// inside VRAM's 32 K words as the renderer wraps it.
    pub first_table_base_word: u16,
    /// VRAM word address of character 0 of the second table: 256 tiles
    /// past the first, plus the gap OBSEL bits 3-4 select in steps of
    /// 256 tiles, wrapped the same way.
    pub second_table_base_word: u16,
}

/// Read one tilemap entry for background `tx,ty` honouring the SNES
/// quadrant layout (32x32 sub-screens: TL, TR, BL, BR). `word_base` is
/// the tilemap base in VRAM words.
fn tilemap_entry(
    vram: &[u16; 0x8000],
    word_base: usize,
    tx: u32,
    ty: u32,
    w_tiles: u32,
    h_tiles: u32,
) -> u16 {
    let screen_x = (tx / 32) as usize;
    let screen_y = (ty / 32) as usize;
    let within = ((ty % 32) * 32 + (tx % 32)) as usize;
    let screen = if h_tiles > 32 {
        let screens_wide = if w_tiles > 32 { 2 } else { 1 };
        screen_y * screens_wide + screen_x
    } else {
        screen_x
    };
    let offset = screen * 0x400 + within;
    vram[(word_base + offset) & 0x7fff]
}

impl Ppu {
    /// Decode a single 4bpp 8x8 tile from VRAM into 64 palette indices
    /// (row-major). `index` selects across the whole word space; the
    /// upper 1024 tiles read from the second 32 KiB half. Shared by the
    /// `VideoDebugger` tile getter and the tile-image exporter.
    pub(crate) fn decode_vram_tile_4bpp(&self, index: u32) -> [u8; 64] {
        let base_byte = (index / TILES_PER_HALF) * 0x8000;
        let tile_index = (index % TILES_PER_HALF) as u16;
        let mut pixels = [0u8; 64];
        for row in 0..8u8 {
            let cols = decode_tile_4bpp_row(self.vram(), tile_index, base_byte, row, false, false);
            pixels[(row as usize) * 8..(row as usize) * 8 + 8].copy_from_slice(&cols);
        }
        pixels
    }

    /// Bits per pixel for background `bg` (`0..=3`) under the current
    /// mode (`2`/`4`/`8`), or `0` when the BG does not draw pixels in
    /// this mode. Thin wrapper over the renderer's single source of
    /// truth so the host's full-map assembler can never pick a depth
    /// that disagrees with the live frame.
    #[must_use]
    pub fn bg_bpp(&self, bg: usize) -> u8 {
        bpp_for_bg(self.bgmode, bg.min(3))
    }

    /// Per-background addressing + format snapshot ([`BgLayout`]) for the
    /// host's raw-layer assembler. Reads the tilemap base / char base /
    /// screen size / tile size / scroll straight off live state, never
    /// the mode-derived `getPpuModeBreakdown` flags, which describe the
    /// mode default rather than the per-BG registers.
    #[must_use]
    pub fn bg_layout(&self, bg: usize) -> BgLayout {
        let bg = bg.min(3);
        let (w_tiles, h_tiles) = match self.bg_screen_size[bg] & 0x03 {
            0 => (32u32, 32u32),
            1 => (64, 32),
            2 => (32, 64),
            _ => (64, 64),
        };
        BgLayout {
            bg: bg as u8,
            bg_mode: self.bgmode,
            tilemap_base_byte: self.bg_screen_addr[bg],
            char_base_byte: self.bg_tiledata_addr[bg],
            screen_size: self.bg_screen_size[bg] & 0x03,
            width_cells: w_tiles,
            height_cells: h_tiles,
            bpp: self.bg_bpp(bg),
            tile_size16: self.bg_tile_size[bg],
            palette_base: palette_offset_for_bg(self.bgmode, bg),
            scroll_x: self.bg_hoffset(bg),
            scroll_y: self.bg_voffset(bg),
        }
    }

    /// Where the renderer finds the sprites' tiles, and the two sprite
    /// sizes ([`SpriteTileSettings`]), as they stand now. Each table's
    /// address is the tile the renderer reads for character 0 of that
    /// table, so the two cannot disagree.
    #[must_use]
    pub fn sprite_tile_settings(&self) -> SpriteTileSettings {
        let (small_width, small_height) =
            sprite_size(self.obj_base_size, false, self.obj_interlace);
        let (large_width, large_height) = sprite_size(self.obj_base_size, true, self.obj_interlace);
        let table_base_word = |second_table: bool| {
            let tile = sprite_tile_number(
                self.obj_tiledata_addr,
                self.obj_name_select,
                0,
                second_table,
                false,
                8,
                0,
                0,
            );
            // A 4bpp tile is 16 words; the tile number is below 2048, so
            // the address fits in 16 bits.
            tile << 4
        };
        SpriteTileSettings {
            small_width,
            small_height,
            large_width,
            large_height,
            first_table_base_word: table_base_word(false),
            second_table_base_word: table_base_word(true),
        }
    }
}

impl VideoDebugger for Ppu {
    fn get_tile_data(&self, index: u32) -> Option<TileData> {
        if index >= VRAM_4BPP_TILE_COUNT {
            return None;
        }
        Some(TileData {
            index,
            width: 8,
            height: 8,
            pixels: self.decode_vram_tile_4bpp(index).to_vec(),
        })
    }

    fn get_tile_count(&self) -> u32 {
        VRAM_4BPP_TILE_COUNT
    }

    fn get_palette_data(&self) -> Vec<PaletteData> {
        let cgram = self.cgram();
        // 16 palettes x 16 colours, the 4bpp grouping a CGRAM viewer
        // renders as a 16x16 swatch grid.
        (0..16u32)
            .map(|pal| {
                let base = (pal as usize) * 16;
                let colors = (0..16)
                    .map(|i| rgba8_to_u32(bgr15_to_rgba8(cgram[base + i])))
                    .collect();
                PaletteData { index: pal, colors }
            })
            .collect()
    }

    fn get_sprite_data(&self) -> Vec<SpriteData> {
        let oam = self.oam();
        (0..128usize)
            .map(|idx| {
                let s = parse_sprite(oam, idx, self.obj_base_size, self.obj_interlace);
                SpriteData {
                    index: idx as u32,
                    x: normalize_obj_x(s.x),
                    // Y is already the top scanline (8-bit, wraps at 256,
                    // no sign bit), so it is left as-is.
                    y: i32::from(s.y),
                    width: u32::from(s.width),
                    height: u32::from(s.height),
                    tile_index: u32::from(s.character),
                    palette: u32::from(s.palette),
                    priority: u32::from(s.priority),
                    flip_h: s.hflip,
                    flip_v: s.vflip,
                }
            })
            .collect()
    }

    fn resolved_sprite_render_order(&self) -> Vec<SpriteOrderEntry> {
        let oam = self.oam();
        let first = usize::from(self.oam_first_sprite);
        // Priority order (front-to-back) walks all 128 OAM entries starting
        // at the rotated first sprite, exactly like `render_obj_row`'s range
        // eval (`(oam_first_sprite + i) & 0x7F`). The renderer then paints
        // that gathered set in reverse so the highest-priority sprite wins,
        // so back-to-front paint order is the reverse of the priority walk.
        (0..128usize)
            .rev()
            .map(|i| {
                let idx = (first + i) & 0x7F;
                let s = parse_sprite(oam, idx, self.obj_base_size, self.obj_interlace);
                SpriteOrderEntry {
                    index: idx as u32,
                    priority: u32::from(s.priority),
                }
            })
            .collect()
    }

    fn get_layer_data(&self, layer_id: u32) -> Option<LayerData> {
        let id = layer_id as usize;
        if id == LAYER_OBJ {
            return Some(LayerData {
                id: layer_id,
                name: "OBJ".to_string(),
                // The OBJ plane spans the 9-bit (512) X space over the
                // 256-line screen.
                width: 512,
                height: 256,
                scroll_x: 0,
                scroll_y: 0,
                // The checkbox mirrors the debugger's per-layer hide toggle
                // only, not the game's TM/TS main/sub enable bits. Folding
                // those in would make the checkbox read unchecked for a layer
                // the game isn't currently displaying, so hiding it would take
                // a confusing check-then-uncheck. Matches the Genesis VDP.
                enabled: !self.layer_debug_disabled(LAYER_OBJ),
            });
        }
        if id >= 4 {
            return None;
        }
        // Map pixel size from the per-BG screen-size selector (32/64
        // tiles each axis x 8 px/tile).
        let (w_tiles, h_tiles) = match self.bg_screen_size[id] & 0x03 {
            0 => (32u32, 32u32),
            1 => (64, 32),
            2 => (32, 64),
            _ => (64, 64),
        };
        Some(LayerData {
            id: layer_id,
            name: format!("BG{}", id + 1),
            width: w_tiles * 8,
            height: h_tiles * 8,
            scroll_x: i32::from(self.bg_hoffset(id)),
            scroll_y: i32::from(self.bg_voffset(id)),
            // Debug hide-toggle state only (see the OBJ branch above).
            enabled: !self.layer_debug_disabled(id),
        })
    }

    fn get_layer_count(&self) -> u32 {
        LAYER_COUNT as u32
    }

    fn toggle_layer(&mut self, layer_id: u32, visible: bool) {
        self.set_layer_debug_disabled(layer_id as usize, !visible);
    }

    fn get_vram(&self) -> &[u8] {
        // SAFETY: `vram` is a densely packed `[u16; 0x8000]`, so
        // reinterpreting it as `0x10000` bytes is layout-compatible and
        // the returned borrow is tied to `&self`. Bytes come out in the
        // host's native little-endian order, which is what a raw VRAM
        // hex view expects.
        let words = self.vram();
        unsafe { std::slice::from_raw_parts(words.as_ptr() as *const u8, words.len() * 2) }
    }

    fn get_registers(&self) -> Vec<VideoRegister> {
        let reg = |index: u32, name: &str, value: u32, description: &str| VideoRegister {
            index,
            name: name.to_string(),
            value,
            description: description.to_string(),
        };
        // Reconstruct the TM/TS screen-designation bytes from the
        // per-layer enable bits.
        let mut tm = 0u32;
        let mut ts = 0u32;
        for bg in 0..4 {
            if self.bg_main_enable[bg] {
                tm |= 1 << bg;
            }
            if self.bg_sub_enable[bg] {
                ts |= 1 << bg;
            }
        }
        if self.obj_main_enable {
            tm |= 0x10;
        }
        if self.obj_sub_enable {
            ts |= 0x10;
        }
        vec![
            reg(
                0x2105,
                "BGMODE",
                u32::from(self.bgmode()),
                "BG mode + tile size",
            ),
            reg(0x212C, "TM", tm, "Main screen designation"),
            reg(0x212D, "TS", ts, "Sub screen designation"),
            reg(0x211A, "M7SEL", 0, "Mode 7 settings"),
            reg(0x211B, "M7A", u32::from(self.m7a()), "Mode 7 matrix A"),
            reg(0x211C, "M7B", u32::from(self.m7b()), "Mode 7 matrix B"),
            reg(0x211D, "M7C", u32::from(self.m7c()), "Mode 7 matrix C"),
            reg(0x211E, "M7D", u32::from(self.m7d()), "Mode 7 matrix D"),
            reg(0x211F, "M7X", u32::from(self.m7x()), "Mode 7 centre X"),
            reg(0x2120, "M7Y", u32::from(self.m7y()), "Mode 7 centre Y"),
            reg(
                0x210D,
                "BG1HOFS",
                u32::from(self.bg_hoffset(0)),
                "BG1 horizontal scroll",
            ),
            reg(
                0x210E,
                "BG1VOFS",
                u32::from(self.bg_voffset(0)),
                "BG1 vertical scroll",
            ),
        ]
    }

    /// Decode OAM sprite `index` (`0..=127`) into a tight RGBA crop with
    /// hardware transparency (palette index 0 -> alpha 0).
    ///
    /// Tile selection, name resolution (OBSEL second page), the 16x16
    /// character-grid wrap, H/V flip and the OBJ palette region
    /// (`128 + pal*16`) all reuse the renderer's
    /// [`sprite_tile_number`] / [`sprite_row_in_sprite`] /
    /// [`decode_tile_4bpp_row`], so the cut-out is pixel-identical to
    /// what the compositor draws. The sprite is exported at its full
    /// (non-interlace) art size, OBJ interlace is a display-time
    /// vertical halving, not part of the stored art, so the crop is the
    /// whole sprite regardless of the live `$2133` interlace bit.
    fn export_sprite_image(&self, index: u32) -> Result<PixelImage, ExportError> {
        if index >= 128 {
            return Err(ExportError::OutOfRange);
        }
        let s = parse_sprite(self.oam(), index as usize, self.obj_base_size, false);
        let width = u32::from(s.width);
        let height = u32::from(s.height);
        if width == 0 || height == 0 {
            return Err(ExportError::Empty);
        }

        let cgram = self.cgram();
        let vram = self.vram();
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        let tiles_wide = u16::from(s.width >> 3);
        // Anchor the sprite at Y=0 so each output row maps to its own
        // row through the renderer's V-flip-aware row resolver.
        let s0 = SpriteAttrs { y: 0, ..s };

        for oy in 0..height {
            let y_row = sprite_row_in_sprite(&s0, oy as u16, false, false);
            let row_in_tile = (y_row & 7) as u8;
            for tx in 0..tiles_wide {
                let tile_number = sprite_tile_number(
                    self.obj_tiledata_addr,
                    self.obj_name_select,
                    s.character,
                    s.nameselect,
                    s.hflip,
                    s.width,
                    tx as u8,
                    y_row,
                );
                // Sprite tiles are 4bpp; `tile_number << 5` is the byte
                // base the renderer's tile cache uses.
                let base_byte = u32::from(tile_number) << 5;
                let decoded = decode_tile_4bpp_row(vram, 0, base_byte, row_in_tile, false, false);
                for px in 0..8u32 {
                    let col = if s.hflip {
                        7 - px as usize
                    } else {
                        px as usize
                    };
                    let pixel = decoded[col];
                    if pixel == 0 {
                        continue;
                    }
                    let cgram_idx = 128 + usize::from(s.palette) * 16 + usize::from(pixel);
                    let color = bgr15_to_rgba8(cgram[cgram_idx]);
                    let ox = u32::from(tx) * 8 + px;
                    let off = ((oy * width + ox) * 4) as usize;
                    rgba[off..off + 4].copy_from_slice(&color);
                }
            }
        }

        Ok(PixelImage {
            width,
            height,
            rgba,
        })
    }

    /// Decode a contiguous run of VRAM tiles to a vertically stacked
    /// *indexed* sheet (tile `i` occupies rows `[i*8, i*8+8)`), honouring
    /// 2/4/8 bpp.
    ///
    /// Unlike the Genesis 4bpp-only path, the SNES needs all three BG
    /// depths. The core owns only the per-bpp bitplane decode (shared
    /// with the renderer's `decode_tile_{2,4,8}bpp_row`); the host reads
    /// the tilemap and applies the per-cell palette + flip + transparency
    /// policy. 16x16-tile modes are resolved host-side over these 8x8
    /// tiles, so only `Vram` + `Px8` is decoded here. The per-tile byte
    /// base folds `first_tile + i` into the address (not the decoder's
    /// `& 0x3FF` tile index) so runs past the 1024-tile half still
    /// advance, while each row read wraps inside the 32 KiB-word bank.
    fn decode_tiles(&self, request: &TileDecodeRequest) -> Result<IndexedImage, ExportError> {
        if request.source != TileSource::Vram || request.tile_size != TileSize::Px8 {
            return Err(ExportError::Unsupported);
        }
        let words_per_tile: u32 = match request.bpp {
            2 => 8,
            4 => 16,
            8 => 32,
            _ => return Err(ExportError::Unsupported),
        };
        if request.count == 0 {
            return Err(ExportError::Empty);
        }
        let vram = self.vram();
        let bytes_per_tile = words_per_tile * 2;
        // A start word already past the 32 KiB-word bank is a bogus base
        // (the per-tile reads wrap, but the *origin* must be in range).
        let start_word = (request.base_addr >> 1) as usize
            + (request.first_tile as usize) * (words_per_tile as usize);
        if start_word >= vram.len() {
            return Err(ExportError::OutOfRange);
        }

        let width = 8u32;
        let count = request.count as usize;
        let height = request.count * 8;
        let mut pixels = vec![0u8; (width * height) as usize];
        for i in 0..count {
            let tile_byte = request
                .base_addr
                .wrapping_add((request.first_tile + i as u32).wrapping_mul(bytes_per_tile));
            let row0 = i * 8;
            for py in 0..8u8 {
                let cols = match request.bpp {
                    2 => decode_tile_2bpp_row(vram, 0, tile_byte, py, false, false),
                    4 => decode_tile_4bpp_row(vram, 0, tile_byte, py, false, false),
                    _ => decode_tile_8bpp_row(vram, 0, tile_byte, py, false, false),
                };
                let base = (row0 + py as usize) * 8;
                pixels[base..base + 8].copy_from_slice(&cols);
            }
        }

        Ok(IndexedImage {
            width,
            height,
            pixels,
            bpp: request.bpp,
            transparent_index: Some(0),
        })
    }
}

// -- Viewer exporters (inherent, SNES-specific) ----------------------
//
// These produce host-ready images / descriptors for the debugger's
// visualisers. They are inherent `Ppu` methods (not trait methods)
// because their shapes are SNES-specific; `SnesEmulator` re-exposes
// thin wrappers. All RGBA output reuses `bgr15_to_rgba8`, and all tile
// decoding reuses `decode_vram_tile_4bpp`, so a viewer can never
// diverge from the renderer's colour / tile decode.

impl Ppu {
    /// The live Mode 7 matrix as signed 16-bit `[A, B, C, D, X, Y]`.
    /// The registers are stored unsigned; Mode 7 treats them as signed
    /// fixed-point, so they are reinterpreted here.
    pub fn get_mode7_matrix(&self) -> [i16; 6] {
        [
            self.m7a() as i16,
            self.m7b() as i16,
            self.m7c() as i16,
            self.m7d() as i16,
            self.m7x() as i16,
            self.m7y() as i16,
        ]
    }

    /// Human-readable dump of the per-scanline compositor configuration:
    /// window edges + per-layer enable/invert/combine, the TM/TS/TMW/TSW
    /// screen designation, and the CGWSEL/CGADSUB/COLDATA colour-math
    /// state. Surfaced via `ppu-dump --compositing` to diagnose a render
    /// divergence when VRAM/CGRAM/OAM already match a reference (the
    /// difference must then live in compositing, not in the data).
    pub fn debug_compositing(&self) -> String {
        use core::fmt::Write as _;
        let names = ["BG1", "BG2", "BG3", "BG4", "OBJ", "COL"];
        let mut s = String::new();
        let _ = writeln!(s, "  Compositing:");
        let _ = writeln!(
            s,
            "    W1 [{:>3}..={:<3}]  W2 [{:>3}..={:<3}]",
            self.window_one_left,
            self.window_one_right,
            self.window_two_left,
            self.window_two_right
        );
        for (i, name) in names.iter().enumerate() {
            let we = self.window_enable[i];
            let _ = writeln!(
                s,
                "    {} win: w1en={} w1inv={} w2en={} w2inv={} combine={}",
                name,
                we.w1_enable as u8,
                we.w1_invert as u8,
                we.w2_enable as u8,
                we.w2_invert as u8,
                self.window_combine[i],
            );
        }
        let _ = writeln!(s, "    bg3_priority={}", self.bg3_priority as u8);
        for b in 0..4 {
            let _ = writeln!(
                s,
                "    BG{} main={} sub={} mainWinDis={} subWinDis={} scroll=({},{})",
                b + 1,
                self.bg_main_enable[b] as u8,
                self.bg_sub_enable[b] as u8,
                self.bg_main_window_disable[b] as u8,
                self.bg_sub_window_disable[b] as u8,
                self.bg_hoffset(b),
                self.bg_voffset(b),
            );
            let _ = writeln!(
                s,
                "        mapBaseByte=${:04X} chrBaseByte=${:04X} screenSize={} tile16={}",
                self.bg_screen_addr[b],
                self.bg_tiledata_addr[b],
                self.bg_screen_size[b],
                self.bg_tile_size[b] as u8,
            );
        }
        let _ = writeln!(
            s,
            "    OBJ main={} sub={} mainWinDis={} subWinDis={}",
            self.obj_main_enable as u8,
            self.obj_sub_enable as u8,
            self.obj_main_window_disable as u8,
            self.obj_sub_window_disable as u8,
        );
        let _ = writeln!(
            s,
            "    CGWSEL: col_main_mask={} col_sub_mask={} addsub_subscreen={} direct_color={}",
            self.col_main_mask, self.col_sub_mask, self.addsub_mode as u8, self.direct_color as u8,
        );
        let _ = writeln!(
            s,
            "    CGADSUB: subtract={} halve={} back_en={} oam_en={} bg_en={:?}",
            self.color_math_subtract as u8,
            self.color_math_halve as u8,
            self.color_math_back_enable as u8,
            self.color_math_oam_enable as u8,
            self.color_math_bg_enable.map(|b| b as u8),
        );
        let _ = writeln!(s, "    COLDATA: fixed_color=${:04X}", self.fixed_color);
        s
    }

    /// Render the 256-entry CGRAM as a 16x16 grid of 16x16-pixel
    /// swatches -> a 256x256 RGBA8888 image (`256 * 256 * 4` bytes).
    pub fn export_palette_image(&self) -> Vec<u8> {
        const SWATCH: usize = 16;
        const GRID: usize = 16;
        const DIM: usize = SWATCH * GRID;
        let cgram = self.cgram();
        let mut img = vec![0u8; DIM * DIM * 4];
        for (color_idx, &entry) in cgram.iter().enumerate().take(256) {
            let rgba = bgr15_to_rgba8(entry);
            let gx = (color_idx % GRID) * SWATCH;
            let gy = (color_idx / GRID) * SWATCH;
            for py in 0..SWATCH {
                for px in 0..SWATCH {
                    let off = ((gy + py) * DIM + (gx + px)) * 4;
                    img[off..off + 4].copy_from_slice(&rgba);
                }
            }
        }
        img
    }

    /// Render a 16x16 grid (256 tiles) of 4bpp 8x8 tiles -> a 128x128
    /// RGBA image. `source` chooses the starting VRAM character base
    /// (BG = word 0, OBJ = the `OBSEL` tile-data base); `palette`
    /// (`0..=15`) selects the CGRAM 16-colour sub-palette.
    pub fn export_tile_image(&self, source: TileSetSource, palette: u8) -> ViewerImage {
        const TILES_W: u32 = 16;
        const TILES_H: u32 = 16;
        let start = match source {
            TileSetSource::Bg => 0,
            // `obj_tiledata_addr` is a byte address; 32 bytes per 4bpp
            // tile gives the starting tile index.
            TileSetSource::Obj => self.obj_tiledata_addr / 32,
        };
        let pal_base = (u32::from(palette) & 0x0f) * 16;
        let cgram = self.cgram();
        let width = TILES_W * 8;
        let height = TILES_H * 8;
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for t in 0..(TILES_W * TILES_H) {
            let abs = (start + t) % VRAM_4BPP_TILE_COUNT;
            let pixels = self.decode_vram_tile_4bpp(abs);
            let tx = (t % TILES_W) * 8;
            let ty = (t / TILES_W) * 8;
            blit_tile(
                &mut rgba, width, tx, ty, &pixels, cgram, pal_base, false, false,
            );
        }
        ViewerImage {
            width,
            height,
            rgba,
        }
    }

    /// Render background `bg` (`0..=3`) tilemap as an RGBA image, sized
    /// to the tilemap dimensions (`32/64` tiles per axis x 8 px). Tiles
    /// are decoded as 4bpp with each entry's own palette and H/V flip.
    /// Assumes 8x8 character tiles (the common case); 16x16-tile modes
    /// are rendered as their top-left 8x8 quadrant.
    pub fn export_tilemap_image(&self, bg: usize) -> ViewerImage {
        let bg = bg.min(3);
        let (w_tiles, h_tiles) = match self.bg_screen_size[bg] & 0x03 {
            0 => (32u32, 32u32),
            1 => (64, 32),
            2 => (32, 64),
            _ => (64, 64),
        };
        let width = w_tiles * 8;
        let height = h_tiles * 8;
        let map_word_base = (self.bg_screen_addr[bg] / 2) as usize;
        let char_tile_base = self.bg_tiledata_addr[bg] / 32;
        let cgram = self.cgram();
        let vram = self.vram();
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for ty in 0..h_tiles {
            for tx in 0..w_tiles {
                let entry = tilemap_entry(vram, map_word_base, tx, ty, w_tiles, h_tiles);
                let tile_num = u32::from(entry & 0x03ff);
                let pal_base = u32::from((entry >> 10) & 0x07) * 16;
                let hflip = entry & 0x4000 != 0;
                let vflip = entry & 0x8000 != 0;
                let abs = (char_tile_base + tile_num) % VRAM_4BPP_TILE_COUNT;
                let pixels = self.decode_vram_tile_4bpp(abs);
                blit_tile(
                    &mut rgba,
                    width,
                    tx * 8,
                    ty * 8,
                    &pixels,
                    cgram,
                    pal_base,
                    hflip,
                    vflip,
                );
            }
        }
        ViewerImage {
            width,
            height,
            rgba,
        }
    }

    /// Flatten all 128 OAM sprites into [`SpriteDescriptor`]s, reusing
    /// the renderer's [`parse_sprite`] so size/flip/palette resolution
    /// matches the live frame exactly.
    pub fn export_oam_sprites(&self) -> Vec<SpriteDescriptor> {
        let oam = self.oam();
        (0..128usize)
            .map(|idx| {
                let s = parse_sprite(oam, idx, self.obj_base_size, self.obj_interlace);
                SpriteDescriptor {
                    index: idx as u8,
                    x: s.x,
                    y: s.y,
                    width: s.width,
                    height: s.height,
                    tile: s.character,
                    palette: s.palette,
                    priority: s.priority,
                    hflip: s.hflip,
                    vflip: s.vflip,
                    large: s.large,
                }
            })
            .collect()
    }
}

/// Blit one decoded 8x8 4bpp tile (palette indices) into `rgba` at
/// `(dx, dy)`, colouring through `pal_base..pal_base+16` of CGRAM and
/// applying optional H/V flip.
#[allow(clippy::too_many_arguments)]
fn blit_tile(
    rgba: &mut [u8],
    width: u32,
    dx: u32,
    dy: u32,
    pixels: &[u8; 64],
    cgram: &[u16],
    pal_base: u32,
    hflip: bool,
    vflip: bool,
) {
    for row in 0..8u32 {
        for col in 0..8u32 {
            let sx = if hflip { 7 - col } else { col };
            let sy = if vflip { 7 - row } else { row };
            let idx = pixels[(sy * 8 + sx) as usize];
            let color = cgram[(pal_base + u32::from(idx)) as usize];
            let px = bgr15_to_rgba8(color);
            let off = (((dy + row) * width + (dx + col)) * 4) as usize;
            rgba[off..off + 4].copy_from_slice(&px);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 15-bit BGR red (R = 31) expands to opaque RGBA red.
    #[test]
    fn bgr15_red_expands_to_full_red() {
        assert_eq!(bgr15_to_rgba8(0x001F), [0xFF, 0x00, 0x00, 0xFF]);
        assert_eq!(rgba8_to_u32([0xFF, 0x00, 0x00, 0xFF]), 0xFF00_00FF);
    }

    #[test]
    fn tile_data_count_and_bounds() {
        let ppu = Ppu::new();
        assert_eq!(ppu.get_tile_count(), VRAM_4BPP_TILE_COUNT);
        let tile = ppu.get_tile_data(0).expect("tile 0 in range");
        assert_eq!(tile.width, 8);
        assert_eq!(tile.height, 8);
        assert_eq!(tile.pixels.len(), 64);
        // One past the last addressable tile is out of range.
        assert!(ppu.get_tile_data(VRAM_4BPP_TILE_COUNT).is_none());
    }

    #[test]
    fn palette_data_decodes_cgram() {
        let mut ppu = Ppu::new();
        // CGRAM colour 1 = 15-bit BGR red.
        ppu.cgram[1] = 0x001F;
        let palette = ppu.get_palette_data();
        assert_eq!(palette.len(), 16, "16 sixteen-colour palettes");
        assert_eq!(palette[0].colors.len(), 16);
        assert_eq!(palette[0].colors[1], 0xFF00_00FF, "decoded red swatch");
    }

    #[test]
    fn sprite_data_lists_all_128() {
        let ppu = Ppu::new();
        assert_eq!(ppu.get_sprite_data().len(), 128);
    }

    #[test]
    fn vram_view_spans_the_full_bank() {
        let ppu = Ppu::new();
        assert_eq!(ppu.get_vram().len(), 0x1_0000, "64 KiB of VRAM as bytes");
    }

    #[test]
    fn registers_snapshot_includes_bgmode_and_mode7() {
        let ppu = Ppu::new();
        let regs = ppu.get_registers();
        assert!(regs.iter().any(|r| r.name == "BGMODE"));
        assert!(regs.iter().any(|r| r.name == "M7A"));
    }

    #[test]
    fn layer_count_is_five() {
        let ppu = Ppu::new();
        assert_eq!(ppu.get_layer_count(), LAYER_COUNT as u32);
    }

    /// `toggle_layer` drives the debug-disable mask: a BG reads back
    /// disabled after a hide and enabled again after a show. The reported
    /// `enabled` flag tracks the debug mask ONLY, never the game's TM
    /// main-screen enable, so the checkbox starts checked even for a BG the
    /// game isn't currently displaying, and hiding it takes a single uncheck.
    #[test]
    fn toggle_layer_blanks_a_bg() {
        let mut ppu = Ppu::new();
        // bg_main_enable left false on purpose: the checkbox must still start
        // checked because nothing is debug-hidden.
        assert!(
            !ppu.bg_main_enable[0],
            "game has BG1 off on the main screen"
        );
        assert!(
            ppu.get_layer_data(0).expect("BG1 layer").enabled,
            "a BG starts visible regardless of the game's TM enable bit"
        );
        ppu.toggle_layer(0, false);
        assert!(
            !ppu.get_layer_data(0).unwrap().enabled,
            "hiding the layer clears its enabled flag"
        );
        ppu.toggle_layer(0, true);
        assert!(ppu.get_layer_data(0).unwrap().enabled, "showing it again");
    }

    /// The Mode 7 matrix export reinterprets the stored unsigned
    /// registers as signed fixed-point.
    #[test]
    fn mode7_matrix_export() {
        let mut ppu = Ppu::new();
        ppu.m7a = 0x0100; // 256
        ppu.m7b = 0x0001; // 1
        ppu.m7c = 0xFFFF; // -1
        ppu.m7d = 0x0100; // 256
        ppu.m7x = 0x0010; // 16
        ppu.m7y = 0xFFF0; // -16
        assert_eq!(ppu.get_mode7_matrix(), [256, 1, -1, 256, 16, -16]);
    }

    #[test]
    fn palette_image_dims() {
        let ppu = Ppu::new();
        // 16x16 grid of 16x16-pixel swatches = 256x256 RGBA.
        assert_eq!(ppu.export_palette_image().len(), 256 * 256 * 4);
    }

    #[test]
    fn tile_image_dims() {
        let ppu = Ppu::new();
        let img = ppu.export_tile_image(TileSetSource::Bg, 0);
        assert_eq!((img.width, img.height), (128, 128));
        assert_eq!(img.rgba.len(), 128 * 128 * 4);
    }

    #[test]
    fn tilemap_image_dims_default_32x32() {
        let ppu = Ppu::new();
        // Default screen size selector (0) -> 32x32 tiles -> 256x256 px.
        let img = ppu.export_tilemap_image(0);
        assert_eq!((img.width, img.height), (256, 256));
        assert_eq!(img.rgba.len(), 256 * 256 * 4);
    }

    #[test]
    fn oam_sprites_export_all_128() {
        let ppu = Ppu::new();
        assert_eq!(ppu.export_oam_sprites().len(), 128);
    }

    // -- decode_tiles (2 / 4 / 8 bpp) --------------------------------

    fn vram_decode_req(bpp: u8, count: u32) -> TileDecodeRequest {
        TileDecodeRequest {
            source: TileSource::Vram,
            base_addr: 0,
            first_tile: 0,
            count,
            bpp,
            tile_size: TileSize::Px8,
        }
    }

    #[test]
    fn decode_tiles_decodes_each_depth() {
        let mut ppu = Ppu::new();
        // Row 0 plane 0 = 0xFF (low byte of VRAM word 0) -> every column
        // resolves to palette index 1 at 2/4/8 bpp alike.
        ppu.vram[0] = 0x00FF;
        for bpp in [2u8, 4, 8] {
            let img = ppu.decode_tiles(&vram_decode_req(bpp, 1)).expect("decode");
            assert_eq!((img.width, img.height), (8, 8), "bpp {bpp} dims");
            assert_eq!(img.bpp, bpp);
            assert_eq!(img.transparent_index, Some(0));
            assert_eq!(&img.pixels[0..8], &[1u8; 8], "bpp {bpp} row 0 indices");
            assert_eq!(&img.pixels[8..16], &[0u8; 8], "bpp {bpp} row 1 untouched");
        }
    }

    #[test]
    fn decode_tiles_stacks_count_vertically() {
        let mut ppu = Ppu::new();
        // Two 4bpp tiles (16 words each): tile 0 + tile 1 both lit on row 0.
        ppu.vram[0] = 0x00FF;
        ppu.vram[16] = 0x00FF;
        let img = ppu.decode_tiles(&vram_decode_req(4, 2)).expect("two tiles");
        assert_eq!((img.width, img.height), (8, 16));
        assert_eq!(&img.pixels[0..8], &[1u8; 8], "tile 0 row 0");
        assert_eq!(
            &img.pixels[64..72],
            &[1u8; 8],
            "tile 1 row 0 (output row 8)"
        );
    }

    #[test]
    fn decode_tiles_rejects_bad_requests() {
        let ppu = Ppu::new();
        // count 0 -> Empty.
        assert_eq!(
            ppu.decode_tiles(&vram_decode_req(4, 0)),
            Err(ExportError::Empty)
        );
        // Unsupported bpp (only 2/4/8).
        assert_eq!(
            ppu.decode_tiles(&vram_decode_req(3, 1)),
            Err(ExportError::Unsupported)
        );
        // 16x16 tile size is resolved host-side, not in the core.
        let px16 = TileDecodeRequest {
            tile_size: TileSize::Px16,
            ..vram_decode_req(4, 1)
        };
        assert_eq!(ppu.decode_tiles(&px16), Err(ExportError::Unsupported));
        // A start word already past the 32 KiB-word bank -> OutOfRange.
        let oob = TileDecodeRequest {
            base_addr: 0x1_0000,
            ..vram_decode_req(4, 1)
        };
        assert_eq!(ppu.decode_tiles(&oob), Err(ExportError::OutOfRange));
    }

    // -- sprite list + render order ----------------------------------

    #[test]
    fn get_sprite_data_normalizes_obj_x_to_signed_screen_space() {
        let mut ppu = Ppu::new();
        // Sprite 0: X = 500 (x_lo = 244, high-table X bit 8 set) -> the
        // 9-bit value wraps to screen -12 (partially off the left).
        ppu.oam[0] = 244;
        ppu.oam[1] = 30; // Y top scanline, kept verbatim
        ppu.oam[0x200] |= 0x01; // sprite 0 high bits: X bit 8 = 1
                                // Sprite 1: X = 100, no high bit -> unchanged.
        ppu.oam[4] = 100;
        ppu.oam[5] = 40;

        let sprites = ppu.get_sprite_data();
        assert_eq!(sprites[0].x, -12, "9-bit X 500 normalizes to -12");
        assert_eq!(sprites[0].y, 30, "Y stays the raw top scanline");
        assert_eq!(sprites[1].x, 100, "in-range X is unchanged");
    }

    #[test]
    fn resolved_render_order_reverses_oam_first_sprite_walk() {
        let mut ppu = Ppu::new();
        ppu.oam_first_sprite = 5;

        let order = ppu.resolved_sprite_render_order();
        assert_eq!(order.len(), 128);

        // Front-to-back priority walks 5,6,...,127,0,...,4 from the rotated
        // first sprite; back-to-front paint order is the reverse.
        let forward: Vec<u32> = (0..128usize).map(|i| ((5 + i) & 0x7F) as u32).collect();
        let mut expected = forward;
        expected.reverse();
        let got: Vec<u32> = order.iter().map(|e| e.index).collect();
        assert_eq!(got, expected);
        assert_eq!(order.last().unwrap().index, 5, "first-sprite paints on top");
        assert_eq!(order[0].index, 4, "sprite before first-sprite is rearmost");
    }

    // -- export_sprite_image -----------------------------------------

    #[test]
    fn export_sprite_index_out_of_range() {
        let ppu = Ppu::new();
        assert_eq!(ppu.export_sprite_image(128), Err(ExportError::OutOfRange));
    }

    #[test]
    fn export_sprite_applies_palette_and_index0_transparency() {
        let mut ppu = Ppu::new();
        // Sprite 0: small 8x8 (OBSEL selector 0), palette 1.
        ppu.oam[3] = 0x02; // attr: palette 1 (bits 1-3), no flip/priority
                           // Tile 0 row 0: plane0 bit 7 set -> only column 0 is index 1.
        ppu.vram[0] = 0x0080;
        // OBJ palette 1 colour 1 lives at CGRAM 128 + 1*16 + 1 = 145.
        ppu.cgram[145] = 0x001F; // red
        let img = ppu.export_sprite_image(0).expect("sprite 0");
        assert_eq!((img.width, img.height), (8, 8));
        assert_eq!(&img.rgba[0..4], &[0xFF, 0, 0, 0xFF], "col 0 = opaque red");
        assert_eq!(
            &img.rgba[4..8],
            &[0, 0, 0, 0],
            "col 1 index 0 = transparent"
        );
    }

    #[test]
    fn export_sprite_honours_hflip() {
        let mut ppu = Ppu::new();
        ppu.oam[3] = 0x02 | 0x40; // palette 1 + hflip
        ppu.vram[0] = 0x0080; // col 0 index 1 before flip
        ppu.cgram[145] = 0x001F;
        let img = ppu.export_sprite_image(0).expect("sprite 0");
        assert_eq!(&img.rgba[0..4], &[0, 0, 0, 0], "flipped: col 0 transparent");
        let last = 7 * 4;
        assert_eq!(
            &img.rgba[last..last + 4],
            &[0xFF, 0, 0, 0xFF],
            "lit pixel moved to col 7"
        );
    }

    // -- bg_bpp / bg_layout ------------------------------------------

    #[test]
    fn bg_bpp_tracks_mode() {
        let mut ppu = Ppu::new();
        ppu.bgmode = 1;
        assert_eq!(ppu.bg_bpp(0), 4, "BG1 mode 1 = 4bpp");
        assert_eq!(ppu.bg_bpp(2), 2, "BG3 mode 1 = 2bpp");
        ppu.bgmode = 3;
        assert_eq!(ppu.bg_bpp(0), 8, "BG1 mode 3 = 8bpp");
    }

    #[test]
    fn bg_layout_reads_live_registers() {
        let mut ppu = Ppu::new();
        ppu.bgmode = 0;
        ppu.bg_screen_addr[1] = 0x2000;
        ppu.bg_tiledata_addr[1] = 0x4000;
        ppu.bg_screen_size[1] = 0x03; // 64x64
        ppu.bg_tile_size[1] = true;
        ppu.bg_hoffset[1] = 0x1FF;
        ppu.bg_voffset[1] = 0x0AA;
        let layout = ppu.bg_layout(1);
        assert_eq!(layout.bg, 1);
        assert_eq!(layout.bg_mode, 0);
        assert_eq!(layout.tilemap_base_byte, 0x2000);
        assert_eq!(layout.char_base_byte, 0x4000);
        assert_eq!((layout.width_cells, layout.height_cells), (64, 64));
        assert_eq!(layout.screen_size, 0x03);
        assert!(layout.tile_size16);
        assert_eq!(layout.bpp, 2, "BG2 mode 0 = 2bpp");
        assert_eq!(layout.palette_base, 32, "mode 0 -> bg*32");
        assert_eq!((layout.scroll_x, layout.scroll_y), (0x1FF, 0x0AA));
    }

    // -- sprite_tile_settings ----------------------------------------

    #[test]
    fn sprite_tile_settings_read_obsel_and_obj_interlace() {
        let mut ppu = Ppu::new();
        // Power-on OBSEL is 0: 8x8 and 16x16, the first table at word 0,
        // the second 256 tiles past it.
        assert_eq!(
            ppu.sprite_tile_settings(),
            SpriteTileSettings {
                small_width: 8,
                small_height: 8,
                large_width: 16,
                large_height: 16,
                first_table_base_word: 0,
                second_table_base_word: 0x1000,
            }
        );
        // Base 2, gap 1: the tables at words $4000 and $4000 + $1000 +
        // $1000.
        ppu.write_2101(0x0A);
        let settings = ppu.sprite_tile_settings();
        assert_eq!(settings.first_table_base_word, 0x4000);
        assert_eq!(settings.second_table_base_word, 0x6000);
        // Base 4 is past VRAM's 32 K words and wraps to word 0, as the
        // renderer reads it.
        ppu.write_2101(0x04);
        assert_eq!(ppu.sprite_tile_settings().first_table_base_word, 0);
        // Selector 6 under OBJ interlace: the small height is 16, as in
        // the sprite list; the large size is unchanged.
        ppu.write_2101(0xC0);
        ppu.obj_interlace = true;
        let settings = ppu.sprite_tile_settings();
        assert_eq!((settings.small_width, settings.small_height), (16, 16));
        assert_eq!((settings.large_width, settings.large_height), (32, 64));
    }

    /// A host that adds a cell's place in the 16 x 16 table to the
    /// table's address, both axes wrapping inside the table, reads the
    /// tile the renderer reads: for every OBSEL value, both tables, and
    /// every cell of a 64x64 sprite at characters on the table's edges
    /// and inside it.
    #[test]
    fn sprite_tile_settings_locate_every_tile_the_renderer_reads() {
        let mut ppu = Ppu::new();
        for obsel in 0..=0xFFu8 {
            ppu.write_2101(obsel);
            let settings = ppu.sprite_tile_settings();
            for second_table in [false, true] {
                let table = if second_table {
                    settings.second_table_base_word
                } else {
                    settings.first_table_base_word
                } >> 4;
                for character in [0x00u8, 0x0F, 0x5A, 0xF0, 0xFE, 0xFF] {
                    for column in 0..8u8 {
                        for tile_row in 0..8u16 {
                            let renderer = sprite_tile_number(
                                ppu.obj_tiledata_addr,
                                ppu.obj_name_select,
                                character,
                                second_table,
                                false,
                                64,
                                column,
                                tile_row * 8,
                            );
                            let grid_column = (u16::from(character & 15) + u16::from(column)) & 15;
                            let grid_row = (u16::from(character >> 4) + tile_row) & 15;
                            assert_eq!(
                                renderer,
                                (table + grid_row * 16 + grid_column) & 0x07FF,
                                "OBSEL {obsel:#04x}, second table {second_table}, \
                                 character {character:#04x}, column {column}, row {tile_row}"
                            );
                        }
                    }
                }
            }
        }
    }

    // -- repaint sequence (the core of SnesEmulator::repaint_held_frame) --

    #[test]
    fn repaint_sequence_reflects_layer_toggle() {
        use retro_core::Framebuffer;
        let mut ppu = Ppu::new();
        ppu.display_disable = false;
        ppu.display_brightness = 15;
        ppu.bgmode = 0; // BG1 = 2bpp
        ppu.bg_main_enable[0] = true;
        ppu.bg_screen_addr[0] = 0; // tilemap word base 0
        ppu.bg_tiledata_addr[0] = 0x1000; // char word base 0x800
                                          // Fill the whole 32x32 tilemap with "tile 1, palette 0" so the
                                          // BG covers the screen regardless of scroll.
        for cell in &mut ppu.vram[0..1024] {
            *cell = 0x0001;
        }
        // Char tile 1 (2bpp, 8 words at word base 0x800 + 1*8 = 0x808):
        // every row plane0 = 0xFF -> a solid index-1 tile.
        for r in 0..8 {
            ppu.vram[0x808 + r] = 0x00FF;
        }
        ppu.cgram[0] = 0x0000; // backdrop black
        ppu.cgram[1] = 0x7FFF; // BG colour white

        let repaint = |ppu: &mut Ppu| -> Vec<u8> {
            let mut fb = Framebuffer::new(256, 224);
            for scanline in 1..240u16 {
                ppu.render_scanline(scanline, &mut fb);
            }
            ppu.finalize_frame(&mut fb);
            fb.data
        };

        let with_bg = repaint(&mut ppu);
        ppu.set_layer_debug_disabled(0, true);
        let without_bg = repaint(&mut ppu);
        assert_ne!(
            with_bg, without_bg,
            "hiding BG1 must change the repainted frame"
        );
        // Re-enabling restores the original frame exactly.
        ppu.set_layer_debug_disabled(0, false);
        assert_eq!(repaint(&mut ppu), with_bg, "showing BG1 again is identical");
    }
}
