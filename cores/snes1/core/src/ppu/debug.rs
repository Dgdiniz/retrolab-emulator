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
//!
//! The debugger's writes live here too: `write_vram_debug`,
//! `write_cgram_debug` and `write_oam_debug` put new contents straight
//! into video memory, to edit the graphics of a running or paused game,
//! and keep the renderer's caches of VRAM in step.

use retro_core::video::{
    bytes_from_words, words_from_bytes, ColourLevels, ExportError, IndexedImage, LayerData,
    LayerKind, LayerLayout, PaletteData, PictureRect, PixelImage, SpriteData, SpriteOrderEntry,
    SpriteTableEntry, TileData, TileDecodeRequest, TileSize, TileSource, VideoDebugger,
    VideoMemoryId, VideoMemoryInfo, VideoRegister,
};

use crate::ppu::render::{bpp_for_bg, palette_offset_for_bg, COLOUR_LEVELS};
use crate::ppu::sprite::{
    parse_sprite, sprite_row_in_sprite, sprite_size, sprite_tile_number, SpriteAttrs,
};
use crate::ppu::tile::{
    decode_tile_2bpp_row, decode_tile_4bpp_row, decode_tile_8bpp_row, sign_extend_13,
};
use crate::ppu::{LineState, Ppu, WindowEnable, LAYER_COUNT, LAYER_OBJ};

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

/// A sprite's position in signed screen coordinates, as the sprite list
/// and the sprite table report it: where the renderer draws it. X comes
/// out of its 9-bit wrap; Y is the line of the sprite's first row,
/// counted from the first line the frame draws.
///
/// The renderer draws 224 lines, or 239 with the overscan bit set
/// ([`Ppu::in_active_scanline`]), and puts a sprite on each line whose
/// distance below the sprite's Y byte, counted with a wrap at 256, is
/// less than the lines the sprite covers: its height, halved under OBJ
/// interlace ([`sprite_on_scanline`](crate::ppu::sprite::sprite_on_scanline)).
/// A sprite whose first row falls on a line past the last one drawn, and
/// whose rows run on past line 255, therefore shows only the rows that
/// wrap onto the top, and its Y is its Y byte less 256: its first row
/// sits that far above the top, its other rows on the lines they are
/// drawn on. Every other sprite keeps its Y byte: one whose first row is
/// drawn, whether the bottom edge cuts it or it is tall enough to wrap
/// onto the top as well, and one that no line draws.
fn sprite_position(sprite: &SpriteAttrs, overscan: bool, obj_interlace: bool) -> (i32, i32) {
    let lines_drawn: u16 = if overscan { 239 } else { 224 };
    let lines_covered = u16::from(if obj_interlace {
        sprite.height >> 1
    } else {
        sprite.height
    });
    let first_line = u16::from(sprite.y);
    let y = if first_line >= lines_drawn && first_line + lines_covered > 256 {
        i32::from(sprite.y) - 256
    } else {
        i32::from(sprite.y)
    };
    (normalize_obj_x(sprite.x), y)
}

impl Ppu {
    /// The box sprite `s` covers on the picture, its position `(x, y)` as
    /// [`sprite_position`] gives it: the lines it covers (its height,
    /// halved under OBJ interlace, where a field shows every other row),
    /// counted from the line drawn into the picture's first row, on the
    /// picture's grid ([`Ppu::picture_grid`]).
    fn sprite_picture(&self, s: &SpriteAttrs, x: i32, y: i32) -> PictureRect {
        let lines = if self.obj_interlace {
            s.height >> 1
        } else {
            s.height
        };
        // The lines of the frame above the picture's first row: the
        // overscan lines a cropped frame leaves out.
        let above = self.first_row_line() as i32 - 1;
        PictureRect::on_grid(
            self.picture_grid(),
            x,
            y - above,
            u32::from(s.width),
            u32::from(lines),
        )
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

/// The PPU's settings, read off the live registers in the names the
/// renderer uses, for a host that has to know how the picture is drawn:
/// the display state, the sprites' tiles and sizes, the background mode,
/// mosaic, Mode 7, the windows, the screens and colour math.
///
/// The positions of the backgrounds (tilemap and character bases, screen
/// size, scroll) are per background, in [`BgLayout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PpuSettings {
    /// Forced blank (INIDISP bit 7): the frame draws nothing, black.
    pub display_disable: bool,
    /// Master brightness `0..=15` (INIDISP bits 0-3).
    pub display_brightness: u8,

    /// The sprite size pair selector `0..=7` (OBSEL bits 5-7).
    pub obj_base_size: u8,
    /// The gap between the two sprite character tables, in steps of 256
    /// tiles (OBSEL bits 3-4).
    pub obj_name_select: u8,
    /// Byte address in VRAM of the first sprite character table (OBSEL
    /// bits 0-2).
    pub obj_tiledata_addr: u32,
    /// The two sprite sizes and the VRAM word address of character 0 of
    /// each table, as the renderer resolves them.
    pub sprite_tiles: SpriteTileSettings,

    /// Background mode `0..=7` (BGMODE bits 0-2).
    pub bgmode: u8,
    /// Mode 1's BG3 priority lift (BGMODE bit 3).
    pub bg3_priority: bool,
    /// 16x16 tiles, per background BG1-4 (BGMODE bits 4-7).
    pub bg_tile_size: [bool; 4],

    /// Mosaic block size in pixels, `1..=16` (MOSAIC bits 4-7, plus 1).
    pub mosaic_size: u8,
    /// Mosaic on, per background BG1-4 (MOSAIC bits 0-3).
    pub bg_mosaic_enable: [bool; 4],

    /// Mode 7 screen over (M7SEL bits 6-7): 0 or 1 wrap the plane, 2
    /// leaves the outside transparent, 3 fills it with character 0.
    pub mode7_repeat: u8,
    /// Mode 7 vertical flip (M7SEL bit 1).
    pub mode7_vflip: bool,
    /// Mode 7 horizontal flip (M7SEL bit 0).
    pub mode7_hflip: bool,
    /// Mode 7 horizontal offset, sign-extended from the 13 bits the
    /// renderer reads.
    pub mode7_hoffset: i16,
    /// Mode 7 vertical offset, sign-extended from 13 bits.
    pub mode7_voffset: i16,
    /// The Mode 7 matrix `[A, B, C, D]`, signed 8.8 fixed point.
    pub mode7_matrix: [i16; 4],
    /// The Mode 7 centre `[X, Y]`, sign-extended from 13 bits.
    pub mode7_centre: [i16; 2],

    /// Window enables and inversions per layer, in the order BG1, BG2,
    /// BG3, BG4, OBJ, colour (W12SEL, W34SEL, WOBJSEL).
    pub window_enable: [WindowEnable; 6],
    /// Left edge of window 1 (WH0).
    pub window_one_left: u8,
    /// Right edge of window 1 (WH1).
    pub window_one_right: u8,
    /// Left edge of window 2 (WH2).
    pub window_two_left: u8,
    /// Right edge of window 2 (WH3).
    pub window_two_right: u8,
    /// How the two windows combine, per layer in the same order: 0 OR,
    /// 1 AND, 2 XOR, 3 XNOR (WBGLOG, WOBJLOG).
    pub window_combine: [u8; 6],
    /// The windows clip BG1-4 on the main screen (TMW bits 0-3).
    pub bg_main_window_disable: [bool; 4],
    /// The windows clip sprites on the main screen (TMW bit 4).
    pub obj_main_window_disable: bool,
    /// The windows clip BG1-4 on the sub screen (TSW bits 0-3).
    pub bg_sub_window_disable: [bool; 4],
    /// The windows clip sprites on the sub screen (TSW bit 4).
    pub obj_sub_window_disable: bool,

    /// BG1-4 on the main screen (TM bits 0-3).
    pub bg_main_enable: [bool; 4],
    /// Sprites on the main screen (TM bit 4).
    pub obj_main_enable: bool,
    /// BG1-4 on the sub screen (TS bits 0-3).
    pub bg_sub_enable: [bool; 4],
    /// Sprites on the sub screen (TS bit 4).
    pub obj_sub_enable: bool,

    /// Where the main screen shows its pixels, black elsewhere: 0
    /// everywhere, 1 inside the colour window, 2 outside it, 3 nowhere
    /// (CGWSEL bits 6-7).
    pub col_main_mask: u8,
    /// Where colour math may run, in the same encoding (CGWSEL bits 4-5).
    pub col_sub_mask: u8,
    /// Colour math adds or subtracts the sub screen instead of the fixed
    /// colour (CGWSEL bit 1).
    pub addsub_mode: bool,
    /// Direct colour for the 256-colour backgrounds (CGWSEL bit 0).
    pub direct_color: bool,
    /// Colour math subtracts instead of adding (CGADSUB bit 7).
    pub color_math_subtract: bool,
    /// Colour math halves its result (CGADSUB bit 6).
    pub color_math_halve: bool,
    /// Colour math on the backdrop (CGADSUB bit 5).
    pub color_math_back_enable: bool,
    /// Colour math on sprites, the ones in palettes 4-7 (CGADSUB bit 4).
    pub color_math_oam_enable: bool,
    /// Colour math on BG1-4 (CGADSUB bits 0-3).
    pub color_math_bg_enable: [bool; 4],
    /// The fixed colour, BGR555 (COLDATA).
    pub fixed_color: u16,

    /// Mode 7's second background (SETINI bit 6).
    pub mode7_extbg: bool,
    /// Pseudo-hires (SETINI bit 3).
    pub pseudo_hires: bool,
    /// Overscan, 239 visible lines instead of 224 (SETINI bit 2).
    pub overscan: bool,
    /// Sprite interlace (SETINI bit 1).
    pub obj_interlace: bool,
    /// Screen interlace (SETINI bit 0).
    pub interlace: bool,
    /// The interlace field the frame is drawn in, which alternates every
    /// frame whether interlace is on or not.
    pub field: bool,
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

    /// The PPU's settings as they stand now ([`PpuSettings`]).
    #[must_use]
    pub fn settings(&self) -> PpuSettings {
        let signed_13 = |value: u16| sign_extend_13(value) as i16;
        PpuSettings {
            display_disable: self.display_disable,
            display_brightness: self.display_brightness,
            obj_base_size: self.obj_base_size,
            obj_name_select: self.obj_name_select,
            obj_tiledata_addr: self.obj_tiledata_addr,
            sprite_tiles: self.sprite_tile_settings(),
            bgmode: self.bgmode,
            bg3_priority: self.bg3_priority,
            bg_tile_size: self.bg_tile_size,
            mosaic_size: self.mosaic_size,
            bg_mosaic_enable: self.bg_mosaic_enable,
            mode7_repeat: self.mode7_repeat,
            mode7_vflip: self.mode7_vflip,
            mode7_hflip: self.mode7_hflip,
            mode7_hoffset: signed_13(self.mode7_hoffset),
            mode7_voffset: signed_13(self.mode7_voffset),
            mode7_matrix: [
                self.m7a as i16,
                self.m7b as i16,
                self.m7c as i16,
                self.m7d as i16,
            ],
            mode7_centre: [signed_13(self.m7x), signed_13(self.m7y)],
            window_enable: self.window_enable,
            window_one_left: self.window_one_left,
            window_one_right: self.window_one_right,
            window_two_left: self.window_two_left,
            window_two_right: self.window_two_right,
            window_combine: self.window_combine,
            bg_main_window_disable: self.bg_main_window_disable,
            obj_main_window_disable: self.obj_main_window_disable,
            bg_sub_window_disable: self.bg_sub_window_disable,
            obj_sub_window_disable: self.obj_sub_window_disable,
            bg_main_enable: self.bg_main_enable,
            obj_main_enable: self.obj_main_enable,
            bg_sub_enable: self.bg_sub_enable,
            obj_sub_enable: self.obj_sub_enable,
            col_main_mask: self.col_main_mask,
            col_sub_mask: self.col_sub_mask,
            addsub_mode: self.addsub_mode,
            direct_color: self.direct_color,
            color_math_subtract: self.color_math_subtract,
            color_math_halve: self.color_math_halve,
            color_math_back_enable: self.color_math_back_enable,
            color_math_oam_enable: self.color_math_oam_enable,
            color_math_bg_enable: self.color_math_bg_enable,
            fixed_color: self.fixed_color,
            mode7_extbg: self.mode7_extbg,
            pseudo_hires: self.pseudo_hires,
            overscan: self.overscan,
            obj_interlace: self.obj_interlace,
            interlace: self.interlace,
            field: self.field,
        }
    }

    /// The M7SEL byte as the game wrote it: the screen over in bits 6-7,
    /// the vertical flip in bit 1, the horizontal flip in bit 0.
    fn mode7_settings_byte(&self) -> u8 {
        (self.mode7_repeat << 6) | (u8::from(self.mode7_vflip) << 1) | u8::from(self.mode7_hflip)
    }

    /// The 16x16 blocks a run of 16x16 background tiles name, stacked 16
    /// rows apiece. Block `N` (`first_tile + i`) is what the renderer
    /// reads for a 16x16 tile numbered `N`: the character `N` at its top
    /// left, `N + 1` to its right, `N + 16` and `N + 17` below (the
    /// character table is 16 tiles wide), each wrapped to the ten bits of
    /// a character number, from the character base `base_addr`. The
    /// renderer's flips mirror the block as a whole; the host applies
    /// them as it does for an 8x8 tile. `first_tile` is a character
    /// number, below 1024.
    fn decode_vram_tiles_16(
        &self,
        request: &TileDecodeRequest,
    ) -> Result<IndexedImage, ExportError> {
        let decode_row = match request.bpp {
            2 => decode_tile_2bpp_row,
            4 => decode_tile_4bpp_row,
            8 => decode_tile_8bpp_row,
            _ => return Err(ExportError::Unsupported),
        };
        if request.count == 0 {
            return Err(ExportError::Empty);
        }
        // The blocks are numbered as characters are, in ten bits: no more
        // are read than there are numbers.
        if request.first_tile >= CHARACTER_NUMBERS
            || request.count > CHARACTER_NUMBERS
            || (request.base_addr >> 1) as usize >= 0x8000
        {
            return Err(ExportError::OutOfRange);
        }
        const SIDE: usize = 16;
        // The four characters of a block, from its top-left one, and where
        // each lands in the block.
        const QUADRANTS: [(u32, usize, usize); 4] = [(0, 0, 0), (1, 8, 0), (16, 0, 8), (17, 8, 8)];
        let vram = self.vram();
        let count = request.count as usize;
        let mut pixels = vec![0u8; SIDE * SIDE * count];
        for i in 0..count {
            let block = request.first_tile.wrapping_add(i as u32);
            for &(step, dx, dy) in &QUADRANTS {
                let character = (block.wrapping_add(step) % CHARACTER_NUMBERS) as u16;
                for py in 0..8u8 {
                    let cols = decode_row(vram, character, request.base_addr, py, false, false);
                    let start = (i * SIDE + dy + py as usize) * SIDE + dx;
                    pixels[start..start + 8].copy_from_slice(&cols);
                }
            }
        }
        Ok(IndexedImage {
            width: SIDE as u32,
            height: (SIDE * count) as u32,
            pixels,
            bpp: request.bpp,
            transparent_index: Some(0),
        })
    }

    /// The Mode 7 characters `first_tile..first_tile + count`, 8bpp, 8x8,
    /// stacked. Mode 7 VRAM interleaves its two tables byte by byte: the
    /// tilemap in the low bytes of the first 16 K words and the characters
    /// in their high bytes, 64 words a character, row by row. Character `c`
    /// is therefore the high bytes of the words from `base_addr / 2 +
    /// c * 64` on (wrapping inside VRAM), the bytes the Mode 7 plane
    /// samples; character 0 sits at word 0 on the hardware, so a host
    /// passes a `base_addr` of 0 for the plane's own characters.
    fn decode_mode7_tiles(&self, request: &TileDecodeRequest) -> Result<IndexedImage, ExportError> {
        if request.bpp != 8 {
            return Err(ExportError::Unsupported);
        }
        const WORDS_PER_CHARACTER: usize = 64;
        // The reads wrap at the end of VRAM; the first character starts
        // inside it, and no more are read than VRAM holds.
        let start_byte =
            request.first_character_byte(VRAM_BYTES as u32, 2 * WORDS_PER_CHARACTER as u32)?;
        let start_word = start_byte as usize >> 1;
        let vram = self.vram();
        let count = request.count as usize;
        // Character after character, so pixel `n` of the sheet is the high
        // byte of word `start_word + n`.
        let pixels = (0..WORDS_PER_CHARACTER * count)
            .map(|n| (vram[(start_word + n) & 0x7FFF] >> 8) as u8)
            .collect();
        Ok(IndexedImage {
            width: 8,
            height: (8 * count) as u32,
            pixels,
            bpp: 8,
            transparent_index: Some(0),
        })
    }
}

/// How many character numbers a tilemap entry can name: ten bits.
const CHARACTER_NUMBERS: u32 = 1024;

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
                let (x, y) = sprite_position(&s, self.overscan, self.obj_interlace);
                SpriteData {
                    index: idx as u32,
                    x,
                    y,
                    width: u32::from(s.width),
                    height: u32::from(s.height),
                    tile_index: u32::from(s.character),
                    palette: u32::from(s.palette),
                    priority: u32::from(s.priority),
                    flip_h: s.hflip,
                    flip_v: s.vflip,
                    picture: self.sprite_picture(&s, x, y),
                }
            })
            .collect()
    }

    /// The sprites the held frame drew, back to front: only the sprites
    /// that drew a pixel on a line that showed sprites, ordered by the
    /// priority rotation (`(slot - first) & 0x7F`, largest first) of the
    /// first line that showed them, so the first sprite paints on top.
    /// Each line is read with its own settings and sprite table when the
    /// line table holds the frame (see [`Ppu::line_sprites`], which also
    /// gives each line's own order and what it dropped); otherwise the
    /// visible lines are read with the registers and OAM as they stand.
    fn resolved_sprite_render_order(&mut self) -> Vec<SpriteOrderEntry> {
        self.sprite_render_order()
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
                line_scroll: Vec::new(),
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
        let (scroll_x, scroll_y) = LineState::capture(self).bg_scroll(id);
        Some(LayerData {
            id: layer_id,
            name: format!("BG{}", id + 1),
            width: w_tiles * 8,
            height: h_tiles * 8,
            scroll_x,
            scroll_y,
            // Debug hide-toggle state only (see the OBJ branch above).
            enabled: !self.layer_debug_disabled(id),
            line_scroll: self.bg_line_scroll(id),
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
            reg(
                0x211A,
                "M7SEL",
                u32::from(self.mode7_settings_byte()),
                "Mode 7 settings",
            ),
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

    /// Decode a run of tiles to a vertically stacked *indexed* sheet
    /// (tile `i` occupies rows `[i*h, i*h+h)`, `h` the tile's height),
    /// honouring 2/4/8 bpp.
    ///
    /// Unlike the Genesis 4bpp-only path, the SNES needs all three BG
    /// depths. The core owns only the per-bpp bitplane decode (shared
    /// with the renderer's `decode_tile_{2,4,8}bpp_row`); the host reads
    /// the tilemap and applies the per-cell palette + flip + transparency
    /// policy. Three kinds of tile:
    ///
    /// - `Vram` + `Px8`: 8x8 characters from `base_addr` on. The per-tile
    ///   byte base folds `first_tile + i` into the address (not the
    ///   decoder's `& 0x3FF` tile index) so runs past the 1024-tile half
    ///   still advance, while each row read wraps inside the 32 KiB-word
    ///   bank.
    /// - `Vram` + `Px16`: the 16x16 block a 16x16 background tile names,
    ///   read as the renderer reads it (`decode_vram_tiles_16`).
    /// - `Mode7` + `Px8`: the 8bpp Mode 7 characters
    ///   (`decode_mode7_tiles`).
    fn decode_tiles(&self, request: &TileDecodeRequest) -> Result<IndexedImage, ExportError> {
        match (request.source, request.tile_size) {
            (TileSource::Vram, TileSize::Px8) => {}
            (TileSource::Vram, TileSize::Px16) => return self.decode_vram_tiles_16(request),
            (TileSource::Mode7, TileSize::Px8) => return self.decode_mode7_tiles(request),
            (TileSource::Mode7, TileSize::Px16) => return Err(ExportError::Unsupported),
        }
        let words_per_tile: u32 = match request.bpp {
            2 => 8,
            4 => 16,
            8 => 32,
            _ => return Err(ExportError::Unsupported),
        };
        let vram = self.vram();
        let bytes_per_tile = words_per_tile * 2;
        // The per-tile reads wrap at the end of VRAM, as the PPU's do, but
        // the first character must start inside it, and no more characters
        // are read than VRAM holds at this depth.
        request.first_character_byte(VRAM_BYTES as u32, bytes_per_tile)?;

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

    /// VRAM (word addresses), CGRAM (colours) and OAM (bytes), each
    /// keeping a word's low byte first, as the 65C816 reads them.
    fn video_memories(&self) -> Vec<VideoMemoryInfo> {
        let memory = |id, name, size: usize, address_unit| VideoMemoryInfo {
            id,
            name,
            size: size as u32,
            address_unit,
            little_endian: true,
        };
        vec![
            memory(VideoMemoryId::Vram, "VRAM", VRAM_BYTES, 2),
            memory(VideoMemoryId::Palette, "CGRAM", CGRAM_COLOURS * 2, 2),
            memory(VideoMemoryId::SpriteTable, "OAM", OAM_BYTES, 1),
        ]
    }

    fn read_video_memory(&self, id: VideoMemoryId) -> Option<Vec<u8>> {
        match id {
            VideoMemoryId::Vram => Some(self.get_vram().to_vec()),
            VideoMemoryId::Palette => Some(bytes_from_words(self.cgram(), true)),
            VideoMemoryId::SpriteTable => Some(self.oam().to_vec()),
            VideoMemoryId::VerticalScroll => None,
        }
    }

    /// Through [`Ppu::write_vram_debug`]'s byte writer,
    /// [`Ppu::write_cgram_debug`] and [`Ppu::write_oam_debug`]; the SNES
    /// has no vertical scroll memory.
    fn write_video_memory(&mut self, id: VideoMemoryId, offset: u32, bytes: &[u8]) -> bool {
        match id {
            VideoMemoryId::Vram => self.write_vram_bytes_debug(offset, bytes),
            VideoMemoryId::Palette => {
                self.write_cgram_debug(offset / 2, &words_from_bytes(bytes, true));
            }
            VideoMemoryId::SpriteTable => self.write_oam_debug(offset, bytes),
            VideoMemoryId::VerticalScroll => return false,
        }
        true
    }

    /// The 128 entries from the one the sprite evaluation starts at (sprite
    /// 0, or the rotation's first sprite), wrapping; the table is read in
    /// order, so no entry links to another.
    fn sprite_table(&self) -> Vec<SpriteTableEntry> {
        let oam = self.oam();
        (0..128usize)
            .map(|taken| {
                let idx = (usize::from(self.oam_first_sprite) + taken) & 0x7F;
                let s = parse_sprite(oam, idx, self.obj_base_size, self.obj_interlace);
                let (x, y) = sprite_position(&s, self.overscan, self.obj_interlace);
                let first_tile = sprite_tile_number(
                    self.obj_tiledata_addr,
                    self.obj_name_select,
                    s.character,
                    s.nameselect,
                    false,
                    8,
                    0,
                    0,
                );
                SpriteTableEntry {
                    index: idx as u32,
                    x,
                    y,
                    width: u32::from(s.width),
                    height: u32::from(s.height),
                    tile_index: u32::from(s.character),
                    palette: u32::from(s.palette),
                    priority: u32::from(s.priority),
                    flip_h: s.hflip,
                    flip_v: s.vflip,
                    next: None,
                    tile_address: u32::from(first_tile) << 5,
                    large: s.large,
                    picture: self.sprite_picture(&s, x, y),
                }
            })
            .collect()
    }

    /// BG1 to BG4 as [`Ppu::bg_layout`] reads their registers, and the
    /// sprites (the first character table, 4 bits a pixel, colours from
    /// 128). In Mode 7, BG1 (and BG2 under EXTBG, 7 bits a pixel) is an
    /// affine map in Mode 7's own layout: a 128 by 128 map in the low byte
    /// of each of the first 16 K words of VRAM, its 8 by 8 characters of 8
    /// bits a pixel in the high bytes; the other backgrounds are off.
    fn layer_layout(&self, layer_id: u32) -> Option<LayerLayout> {
        let id = layer_id as usize;
        if id == LAYER_OBJ {
            let settings = self.sprite_tile_settings();
            return Some(LayerLayout {
                id: layer_id,
                kind: LayerKind::Sprites,
                map_base: 0,
                map_width: 0,
                map_height: 0,
                character_base: u32::from(settings.first_table_base_word) * 2,
                bits_per_pixel: 4,
                cell_width: 8,
                cell_height: 8,
                first_colour: 128,
                scroll_x: 0,
                scroll_y: 0,
                line_scroll: Vec::new(),
            });
        }
        if id >= 4 {
            return None;
        }
        let bg = self.bg_layout(id);
        let affine_bits = match (self.bgmode & 0x07, id) {
            (7, 0) => Some(8),
            (7, 1) if self.mode7_extbg => Some(7),
            _ => None,
        };
        let cell = if bg.tile_size16 { 16 } else { 8 };
        let (scroll_x, scroll_y) = LineState::capture(self).bg_scroll(id);
        let layout = LayerLayout {
            id: layer_id,
            kind: if bg.bpp == 0 {
                LayerKind::Off
            } else {
                LayerKind::TileMap
            },
            map_base: bg.tilemap_base_byte,
            map_width: bg.width_cells,
            map_height: bg.height_cells,
            character_base: bg.char_base_byte,
            bits_per_pixel: bg.bpp,
            cell_width: cell,
            cell_height: cell,
            first_colour: u32::from(bg.palette_base),
            scroll_x,
            scroll_y,
            line_scroll: self.bg_line_scroll(id),
        };
        Some(match affine_bits {
            Some(bits_per_pixel) => LayerLayout {
                kind: LayerKind::AffineMap,
                map_base: 0,
                map_width: 128,
                map_height: 128,
                character_base: 0,
                bits_per_pixel,
                cell_width: 8,
                cell_height: 8,
                first_colour: 0,
                ..layout
            },
            None => layout,
        })
    }

    /// The 32 levels of a CGRAM colour's channel at full brightness
    /// ([`COLOUR_LEVELS`]).
    fn colour_levels(&self) -> ColourLevels {
        COLOUR_LEVELS
    }

    /// A background's whole map, Mode 7's plane in Mode 7, as
    /// [`Ppu::map_image`] draws it.
    fn layer_map(&self, layer_id: u32) -> Option<PixelImage> {
        self.map_image(layer_id)
    }
}

// -- Viewer exporters (inherent, SNES-specific) ----------------------
//
// These produce host-ready images / descriptors for the debugger's
// visualisers. They are inherent `Ppu` methods (not trait methods)
// because their shapes are SNES-specific; `SnesEmulator` re-exposes
// thin wrappers. All RGBA output reuses `bgr15_to_rgba8`, and the tile
// sheet decodes through `decode_vram_tile_4bpp`, so a viewer cannot
// diverge from the renderer's colour or tile decode. A background's whole
// map is the trait's `layer_map` (`map_image.rs`), which draws each entry
// through the renderer's own fetch, character pick, decoders and colour
// rules.

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

// -- Debugger writes (inherent) --------------------------------------
//
// A debugger edits a game's graphics by writing video memory straight in,
// through none of the ports: the address registers, their latches, the
// VRAM read buffer and the gates that close the ports while the picture
// is drawn play no part, and the timing and every flag stay as they were.
// `SnesEmulator` re-exposes thin wrappers.

/// Bytes of VRAM, the span a debugger write into it wraps around.
const VRAM_BYTES: usize = 0x1_0000;
/// Colours in CGRAM.
const CGRAM_COLOURS: usize = 0x100;
/// The bits of a CGRAM word that hold a colour, five each of blue, green
/// and red (`0BBBBBGGGGGRRRRR`): what the port stores.
const CGRAM_COLOUR_BITS: u16 = 0x7FFF;
/// Bytes of OAM: the 512-byte low table, then the 32-byte high table.
const OAM_BYTES: usize = 0x220;

impl Ppu {
    /// Write `bytes` into VRAM from word `word_address` on, the way a
    /// debugger edits graphics: straight into memory, through no port, so
    /// the VRAM address, its read buffer and the gate that closes the port
    /// while the picture is drawn play no part. Byte `n` lands at byte
    /// `2 * word_address + n` of the 64 KiB as [`VideoDebugger::get_vram`]
    /// lays them out, the low half of each word first, so that read gives
    /// back what was written; an odd count leaves the high half of the
    /// last word as it was.
    ///
    /// A word address past the 32 K words wraps to the start, and so does
    /// a run that crosses the end. A buffer longer than VRAM is cut to its
    /// size, so no byte is written twice; an empty one writes nothing.
    ///
    /// The renderer keeps the tilemap rows and the decoded sprite tiles it
    /// read from VRAM until the VRAM generation moves. A write moves it,
    /// as a write through the port does, so the next line drawn, or a
    /// repaint, reads the new contents. A write is no write of the frame's:
    /// a frame drawn again from its line table shows it on every line, and
    /// the table then reports its memory as not exact. While the VRAM
    /// sources are recorded, each byte written is put down to the debugger.
    pub fn write_vram_debug(&mut self, word_address: u32, bytes: &[u8]) {
        self.write_vram_bytes_debug((word_address % (VRAM_BYTES as u32 / 2)) * 2, bytes);
    }

    /// [`Self::write_vram_debug`] from byte `byte_address` on, which wraps
    /// at the 64 KiB: an odd address writes the high half of its word
    /// first.
    fn write_vram_bytes_debug(&mut self, byte_address: u32, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.before_video_memory_edit();
        let start = byte_address as usize % VRAM_BYTES;
        for (offset, &value) in bytes.iter().take(VRAM_BYTES).enumerate() {
            let byte_address = (start + offset) % VRAM_BYTES;
            let word = &mut self.vram[byte_address >> 1];
            let [mut low, mut high] = word.to_le_bytes();
            if byte_address & 1 == 0 {
                low = value;
            } else {
                high = value;
            }
            *word = u16::from_le_bytes([low, high]);
            self.note_vram_debug_write(byte_address);
        }
        self.vram_generation = self.vram_generation.wrapping_add(1);
        self.after_video_memory_edit();
    }

    /// Write `colours` into CGRAM from colour `colour_index` on, the way a
    /// debugger edits a palette: straight into memory, through no port, so
    /// the CGRAM address and its latch stay as they were. Each colour is a
    /// BGR555 word (`0BBBBBGGGGGRRRRR`) and keeps its low 15 bits, as the
    /// port stores it, so [`Self::cgram`] gives back the written words with
    /// bit 15 clear. The renderer reads CGRAM as it draws, so the next line
    /// drawn, or a repaint, shows the new colours; a line table then
    /// reports its memory as not exact, as for [`Self::write_vram_debug`].
    ///
    /// An index past the 256 colours wraps to the start, and so does a run
    /// that crosses the end. A buffer longer than CGRAM is cut to its 256
    /// colours; an empty one writes nothing.
    pub fn write_cgram_debug(&mut self, colour_index: u32, colours: &[u16]) {
        if colours.is_empty() {
            return;
        }
        self.before_video_memory_edit();
        let start = colour_index as usize % CGRAM_COLOURS;
        for (offset, &colour) in colours.iter().take(CGRAM_COLOURS).enumerate() {
            self.cgram[(start + offset) % CGRAM_COLOURS] = colour & CGRAM_COLOUR_BITS;
        }
        self.after_video_memory_edit();
    }

    /// Write `bytes` into OAM from `byte_address` on, the way a debugger
    /// edits sprites: straight into memory, through no port, so the OAM
    /// address, its latch and the first-sprite rotation stay as they were.
    /// The 544 bytes are laid out as [`Self::oam`] returns them: four for
    /// each of the 128 sprites in the low table (X, Y, character,
    /// attributes), then the 32-byte high table, two bits a sprite (bit 8
    /// of X, then the size). The renderer reads OAM as it draws each line,
    /// so the next line drawn, or a repaint, shows the new sprites; a line
    /// table then reports its memory as not exact, as for
    /// [`Self::write_vram_debug`].
    ///
    /// An address past the 544 bytes wraps to the start, and so does a run
    /// that crosses the end. A buffer longer than OAM is cut to its 544
    /// bytes; an empty one writes nothing.
    pub fn write_oam_debug(&mut self, byte_address: u32, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.before_video_memory_edit();
        let start = byte_address as usize % OAM_BYTES;
        for (offset, &value) in bytes.iter().take(OAM_BYTES).enumerate() {
            self.oam[(start + offset) % OAM_BYTES] = value;
        }
        self.after_video_memory_edit();
    }
}

#[cfg(test)]
mod tests;
