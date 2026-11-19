//! Video debugging interface.

use serde::{Deserialize, Serialize};

/// Tile data for tile-based graphics systems.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TileData {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // Palette indices or RGBA
}

/// Palette data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaletteData {
    pub index: u32,
    pub colors: Vec<u32>, // RGBA colors
}

/// Sprite data.
///
/// `x`/`y` are **normalized signed screen-space** coordinates: the core
/// has already undone the per-console hardware bias (Genesis `-128`,
/// SNES 9-bit X wrap and Y wrap past line 255) so a sprite straddling the
/// left/top edge reports a negative coordinate and the host can place
/// every sprite in one frame without re-deriving hardware quirks. This is
/// the single canonical sprite list both cores expose for composite/object
/// capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpriteData {
    pub index: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub tile_index: u32,
    pub palette: u32,
    pub priority: u32,
    pub flip_h: bool,
    pub flip_v: bool,
}

/// One entry in the renderer's resolved sprite paint order.
///
/// Returned by [`VideoDebugger::resolved_sprite_render_order`] in
/// **back-to-front paint order**: index `0` of the returned vector is
/// drawn first (furthest back) and the last entry is drawn on top. A
/// composite exporter that blits the selected members in this exact order
/// (later overwriting earlier where alpha allows) reproduces overlaps
/// exactly as the emulator drew them, so "what the debugger exports" can
/// never drift from "what the renderer drew".
///
/// `priority` is the console's per-sprite priority attribute (the Mega
/// Drive's priority bit, the SNES's 2-bit sprite priority) as the line that
/// drew the sprite first read it, carried for display; it is *not* the
/// ordering key, `index`'s position in the vector already encodes the
/// fully-resolved order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpriteOrderEntry {
    /// The sprite's slot in the console's sprite table.
    pub index: u32,
    /// Its priority attribute.
    pub priority: u32,
}

/// Layer/plane data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerData {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub scroll_x: i32,
    pub scroll_y: i32,
    pub enabled: bool,
}

/// Video register.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoRegister {
    pub index: u32,
    pub name: String,
    pub value: u32,
    pub description: String,
}

/// A decoded raw-pixel image handed from the core to the host.
///
/// This is the "hardware truth" contract: the core decodes hardware
/// memory (VRAM tiles + CRAM/CGRAM colours) into straight RGBA8888 and
/// hands it over; the host owns everything downstream (PNG encoding,
/// compositing, preview chrome). Pixels the hardware treats as
/// transparent (palette index 0) are emitted with alpha 0 so the host
/// never has to know the per-console transparency key.
///
/// `rgba` is row-major, `width * height * 4` bytes, no padding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Why a graphics export could not be produced.
///
/// Kept deliberately small: the host turns these into user-facing
/// messages, so they describe the *class* of failure, not a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportError {
    /// The core does not implement this export (default trait behaviour).
    Unsupported,
    /// The requested index/address is outside the valid range.
    OutOfRange,
    /// The request resolved to a zero-area image (nothing to encode).
    Empty,
}

impl core::fmt::Display for ExportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let msg = match self {
            ExportError::Unsupported => "export not supported by this core",
            ExportError::OutOfRange => "export index out of range",
            ExportError::Empty => "export produced an empty image",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for ExportError {}

/// A decoded run of tiles as *indexed* pixels (one palette index per
/// pixel), handed from the core to the host for bulk assembly.
///
/// Unlike [`PixelImage`], this is pre-colour: the host applies the
/// palette and the per-context transparency policy when it colours the
/// indices, so the OSS core never bakes a colour or transparency choice
/// into a bulk tile decode. Decoding the fiddly per-console tile
/// bit-layout once for a whole sheet also amortises the FFI cost versus
/// one call per tile.
///
/// `pixels` is row-major, `width * height` bytes, each a palette index in
/// `0..(1 << bpp)`. `transparent_index` is the hardware transparency key
/// (index 0 on the tile-based consoles) the host should treat as alpha 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub bpp: u8,
    pub transparent_index: Option<u8>,
}

/// Where a [`TileDecodeRequest`] reads its tiles from.
///
/// Genesis decodes a flat run of 4bpp tiles straight from VRAM
/// (`Vram`); other consoles resolve the tile address from PPU state
/// (BG/OBJ/Mode 7 source select), so this enum is the seam those cores
/// extend without changing the request shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TileSource {
    /// A contiguous run of tiles addressed directly in VRAM.
    Vram,
    /// The SNES Mode 7 characters: 8bpp 8x8 tiles that share VRAM with
    /// the Mode 7 tilemap, one pixel in the high byte of each 16-bit word,
    /// 64 words a character, character 0 at word 0. `base_addr` is the
    /// byte address of character 0 and `first_tile` + `count` pick the
    /// characters; the request is 8bpp and 8x8.
    Mode7,
}

/// Tile dimensions for a decode request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TileSize {
    /// 8x8 (the Genesis tile; the SNES small/default object size).
    Px8,
    /// 16x16 (SNES large objects / some BG modes).
    Px16,
}

/// Describes exactly which tiles to decode.
///
/// `base_addr` + `first_tile` + `count` pick the run (byte address is
/// `base_addr + tile_index * tile_byte_size`); `bpp` and `tile_size`
/// describe the pixel layout. The fields the Genesis path does not vary
/// (`source` beyond `Vram`, non-`Px8` sizes, non-4 `bpp`) exist so the
/// request shape stays stable for the consoles that need them. A `Px16`
/// tile is the block a 16x16 background tile names, which the core lays
/// out the way its renderer reads such a tile; `width` and the height of
/// each tile in the result are then 16.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileDecodeRequest {
    pub source: TileSource,
    pub base_addr: u32,
    pub first_tile: u32,
    pub count: u32,
    pub bpp: u8,
    pub tile_size: TileSize,
}

/// The settings each line of a frame was drawn with, as a core recorded them
/// while the lines were drawn: one column per setting, in the core's own
/// names, one row per line. A core records it only while a host asks for it
/// ([`crate::capture::CaptureCore::line_states`]), and never keeps it in a
/// savestate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineStateTable {
    /// The number of the frame the lines belong to: the frames finished, plus
    /// one while that frame is in progress, wherever it is held; the number
    /// a frame snapshot gives the frame's finished picture.
    pub frame: u64,
    /// The table begins with the frame's first drawn line.
    pub from_first_line: bool,
    /// The frame's picture is finished: every line drawn is in the table.
    pub complete: bool,
    /// Video memory can be put back as each line read it. A debugger write
    /// into video memory since the table's frame began makes it false until
    /// the next frame's first line: a frame drawn again shows the edit,
    /// which no line read.
    pub memory_exact: bool,
    /// Rows of the picture each line gives: 1, or 2 when lines are woven or
    /// doubled.
    pub rows_per_line: u32,
    /// The line drawn into the picture's first row.
    pub first_row_line: u32,
    /// The column names, `line` first.
    pub columns: Vec<String>,
    /// The values, row after row, `columns.len()` to a row.
    pub values: Vec<u32>,
}

/// The picture held in the top-left corner of an RGBA framebuffer whose rows
/// are `source_width` pixels wide: its first `height` rows, each cut to its
/// first `width` pixels, packed row after row (`width * height * 4` bytes).
///
/// `width` is at most `source_width`.
///
/// # Panics
///
/// When `source` holds fewer than `height` rows.
pub fn crop_rgba(source: &[u8], source_width: u32, width: u32, height: u32) -> Vec<u8> {
    let stride = source_width as usize * 4;
    let row_bytes = width as usize * 4;
    let mut rgba = Vec::with_capacity(row_bytes * height as usize);
    for y in 0..height as usize {
        rgba.extend_from_slice(&source[y * stride..y * stride + row_bytes]);
    }
    rgba
}

/// Whether `rgba` is the picture [`crop_rgba`] would cut out of `source`
/// (`width` by `height`, rows of `source_width` pixels), byte for byte,
/// without making the copy: it holds exactly `width * height` pixels and
/// each of its rows equals the start of the same row of `source`.
///
/// `width` is at most `source_width`.
///
/// # Panics
///
/// When `rgba` has the picture's size and `source` holds fewer than `height`
/// rows.
pub fn equals_cropped_rgba(
    source: &[u8],
    source_width: u32,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> bool {
    let stride = source_width as usize * 4;
    let row_bytes = width as usize * 4;
    rgba.len() == row_bytes * height as usize
        && rgba
            .chunks(row_bytes.max(1))
            .enumerate()
            .all(|(y, row)| row == &source[y * stride..y * stride + row_bytes])
}

/// Which of a chip's `N` layers the debugger hides from the picture: a
/// hidden layer is left out of the composite without touching the game's
/// registers, so the debugger can show what each one contributes. Every
/// layer is shown by default. A host setting, never part of a savestate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerMask<const N: usize> {
    hidden: [bool; N],
}

impl<const N: usize> Default for LayerMask<N> {
    fn default() -> Self {
        Self { hidden: [false; N] }
    }
}

impl<const N: usize> LayerMask<N> {
    /// Hide (`true`) or show (`false`) layer `layer`; a layer past the
    /// mask's `N` is ignored.
    pub fn set_hidden(&mut self, layer: usize, hidden: bool) {
        if let Some(slot) = self.hidden.get_mut(layer) {
            *slot = hidden;
        }
    }

    /// Whether layer `layer` is hidden; `false` past the mask's `N`.
    #[inline]
    pub fn is_hidden(&self, layer: usize) -> bool {
        self.hidden.get(layer).copied().unwrap_or(false)
    }

    /// The mask as it stands, every layer shown in its place: what a
    /// redraw that must draw every layer sets aside and puts back.
    pub fn take(&mut self) -> Self {
        std::mem::take(self)
    }
}

/// A video memory, named by what it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoMemoryId {
    /// The video RAM: the characters, the maps and, where it lives there,
    /// the sprite table.
    Vram,
    /// The palette's colour words (the Mega Drive's CRAM, the SNES's CGRAM).
    Palette,
    /// The vertical scroll words (the Mega Drive's VSRAM).
    VerticalScroll,
    /// The sprite table, where it has a memory of its own (the SNES's OAM).
    SpriteTable,
}

impl VideoMemoryId {
    /// Every video memory a chip can have, in this order.
    pub const ALL: [Self; 4] = [
        Self::Vram,
        Self::Palette,
        Self::VerticalScroll,
        Self::SpriteTable,
    ];

    /// The memory a host names: `"vram"`, `"palette"`, `"vertical_scroll"`
    /// or `"sprite_table"`; `None` for any other name.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.name() == name)
    }

    /// The memory's name, as [`Self::parse`] reads it and a host sends it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Vram => "vram",
            Self::Palette => "palette",
            Self::VerticalScroll => "vertical_scroll",
            Self::SpriteTable => "sprite_table",
        }
    }
}

/// What [`VideoDebugger::video_memories`] says of one memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VideoMemoryInfo {
    /// Which memory it is.
    pub id: VideoMemoryId,
    /// The console's name for it (`"VRAM"`, `"CRAM"`, `"CGRAM"`, `"VSRAM"`,
    /// `"OAM"`).
    pub name: &'static str,
    /// Its size in bytes.
    pub size: u32,
    /// The bytes one address of the console's own writer of this memory
    /// counts: 1 where it takes a byte address, 2 where it takes the
    /// address of a 16-bit word or a colour.
    pub address_unit: u32,
    /// A 16-bit word's low byte comes first in the memory's bytes.
    pub little_endian: bool,
}

/// One entry of a chip's sprite table, as
/// [`VideoDebugger::sprite_table`] lists it. The position, size, tile,
/// palette, priority and flips are those [`SpriteData`] gives the sprite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SpriteTableEntry {
    /// The sprite's slot in the table.
    pub index: u32,
    /// Its left column, in signed screen coordinates.
    pub x: i32,
    /// Its top row, in signed screen coordinates.
    pub y: i32,
    /// Its width in pixels.
    pub width: u32,
    /// Its height in pixels.
    pub height: u32,
    /// Its character (tile) number, as the table holds it.
    pub tile_index: u32,
    /// Its palette.
    pub palette: u32,
    /// Its priority attribute.
    pub priority: u32,
    /// It is drawn mirrored left to right.
    pub flip_h: bool,
    /// It is drawn mirrored top to bottom.
    pub flip_v: bool,
    /// The slot the hardware takes after this one, where the table links
    /// its entries (the Mega Drive's link chain); `None` for the last one
    /// taken and on a table the hardware reads in order.
    pub next: Option<u32>,
    /// The VRAM byte where the sprite's first (top-left) character
    /// starts.
    pub tile_address: u32,
    /// The sprite takes the larger of the console's two sprite sizes;
    /// `false` on a console with one.
    pub large: bool,
}

/// What a layer draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
    /// A map of characters, scrolled.
    TileMap,
    /// A map of characters drawn in a window of the screen, not scrolled
    /// (the Mega Drive's window plane).
    Window,
    /// A map of characters turned and scaled by a matrix (the SNES's Mode
    /// 7).
    AffineMap,
    /// The sprites.
    Sprites,
    /// Nothing, in the mode the chip is in.
    Off,
}

/// Where a layer reads its map and its characters, as the chip's registers
/// stand: what a host needs to draw the whole map itself from the video
/// memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LayerLayout {
    /// The layer's id, as [`VideoDebugger::get_layer_data`] numbers it.
    pub id: u32,
    /// What it draws.
    pub kind: LayerKind,
    /// The VRAM byte where its map starts; for the sprites, the sprite
    /// table's first byte in the memory that holds it (VRAM on the Mega
    /// Drive, the table's own memory on the SNES, where it is 0).
    pub map_base: u32,
    /// The map's width in cells; 0 for the sprites, which have no map.
    pub map_width: u32,
    /// The map's height in cells; 0 for the sprites.
    pub map_height: u32,
    /// The VRAM byte where its character 0 starts.
    pub character_base: u32,
    /// Bits a pixel of its characters; 0 when it draws nothing.
    pub bits_per_pixel: u8,
    /// A cell's width in pixels.
    pub cell_width: u8,
    /// A cell's height in pixels.
    pub cell_height: u8,
    /// The palette colour its pixel colours count from.
    pub first_colour: u32,
    /// Its horizontal scroll, as [`LayerData`] reports it.
    pub scroll_x: i32,
    /// Its vertical scroll, as [`LayerData`] reports it.
    pub scroll_y: i32,
}

/// 16-bit words read from `bytes`, two bytes a word in the order given; an
/// odd last byte is left out.
pub fn words_from_bytes(bytes: &[u8], little_endian: bool) -> Vec<u16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| {
            if little_endian {
                u16::from_le_bytes(pair)
            } else {
                u16::from_be_bytes(pair)
            }
        })
        .collect()
}

/// The bytes of `words`, two a word in the order given.
pub fn bytes_from_words(words: &[u16], little_endian: bool) -> Vec<u8> {
    words
        .iter()
        .flat_map(|word| {
            if little_endian {
                word.to_le_bytes()
            } else {
                word.to_be_bytes()
            }
        })
        .collect()
}

/// Video debugging interface for tile-based graphics systems.
pub trait VideoDebugger {
    /// Get tile data by index.
    fn get_tile_data(&self, index: u32) -> Option<TileData>;

    /// Get total number of tiles.
    fn get_tile_count(&self) -> u32;

    /// Get all palette data.
    fn get_palette_data(&self) -> Vec<PaletteData>;

    /// Get all sprite data.
    fn get_sprite_data(&self) -> Vec<SpriteData>;

    /// The sprites the held frame drew, back to front, as the renderer
    /// orders them among themselves (see [`SpriteOrderEntry`]): only the
    /// sprites the frame drew, that is a sprite of which some line drew a
    /// visible column while the line showed sprites (a column of transparent
    /// pixels counts), each listed once, with its priority attribute as
    /// the line that drew it first read it. A frame that drew no sprite
    /// gives an empty list. The order is the one the console's hardware
    /// takes its sprites in (the Mega Drive's link chain, the SNES's sprite
    /// table from its rotated first sprite), the sprite taken first in
    /// front. Consumers blit selected members in this order so a composite
    /// object reproduces on-screen overlaps without re-deriving priority
    /// rules and drifting from the emulator.
    ///
    /// Each line is read with the settings and the sprite table it was
    /// drawn with where the core's line table holds it, else with the
    /// state as it stands. Nothing runs, and nothing a savestate or the
    /// state hash reads moves.
    ///
    /// Default: empty, cores without a sprite renderer expose no order.
    fn resolved_sprite_render_order(&mut self) -> Vec<SpriteOrderEntry> {
        Vec::new()
    }

    /// Get layer data by ID.
    fn get_layer_data(&self, layer_id: u32) -> Option<LayerData>;

    /// Get total number of layers.
    fn get_layer_count(&self) -> u32;

    /// Toggle layer visibility.
    fn toggle_layer(&mut self, layer_id: u32, visible: bool);

    /// Get raw VRAM.
    fn get_vram(&self) -> &[u8];

    /// Get video registers.
    fn get_registers(&self) -> Vec<VideoRegister>;

    /// Render a single hardware sprite to a tight, transparent RGBA image.
    ///
    /// `index` is the sprite's Sprite-Attribute-Table slot (the same index
    /// reported by the sprite list the host inspects). The result is
    /// cropped to the sprite's own `width x height` (no surrounding screen
    /// area) and uses palette index 0 -> alpha 0, so the host gets the
    /// composite-sprite cut-out with hardware transparency already applied.
    ///
    /// Default: `Err(ExportError::Unsupported)` so cores without a sprite
    /// decoder keep compiling unchanged.
    fn export_sprite_image(&self, index: u32) -> Result<PixelImage, ExportError> {
        let _ = index;
        Err(ExportError::Unsupported)
    }

    /// Decode a described run of tiles to *indexed* pixels.
    ///
    /// The result stacks `count` tiles top-to-bottom (`width` = the tile
    /// width, `height` = `count * tile_height`), one palette index per
    /// pixel, with `transparent_index` set to the hardware key. The host
    /// walks a nametable, looks up each cell's tile block in the returned
    /// sheet, then applies the per-cell flip + palette and colours it,
    /// keeping colour and the transparency policy out of the core (see
    /// [`IndexedImage`]).
    ///
    /// Default: `Err(ExportError::Unsupported)` so cores without a bulk
    /// tile decoder keep compiling unchanged.
    fn decode_tiles(&self, request: &TileDecodeRequest) -> Result<IndexedImage, ExportError> {
        let _ = request;
        Err(ExportError::Unsupported)
    }

    /// The video memories the chip has, in [`VideoMemoryId::ALL`]'s order:
    /// each one's id, name, size in bytes, the bytes one address of the
    /// console's own writer counts, and its byte order.
    fn video_memories(&self) -> Vec<VideoMemoryInfo>;

    /// A copy of memory `id`'s bytes, in its byte order; `None` for a
    /// memory the chip lacks.
    fn read_video_memory(&self, id: VideoMemoryId) -> Option<Vec<u8>>;

    /// Write `bytes` into memory `id` from byte `offset` on, the way a
    /// debugger edits it: straight in, through no port or transfer, with
    /// the side effects the console's debug writers have (the renderer's
    /// caches follow, a line table stops being exact). The offset wraps at
    /// the memory's end, and so does a run that crosses it; a buffer longer
    /// than the memory is cut to it, and an empty one writes nothing. A
    /// memory the chip keeps as 16-bit words (a palette, the vertical
    /// scroll) takes whole words: an offset inside a word starts at that
    /// word, an odd last byte is dropped, and each word keeps only the bits
    /// its port keeps. `false` for a memory the chip lacks.
    fn write_video_memory(&mut self, id: VideoMemoryId, offset: u32, bytes: &[u8]) -> bool;

    /// The sprite table's entries the hardware takes, in the order it takes
    /// them: the Mega Drive's link chain from slot 0, as far as the
    /// renderer follows it; the SNES's 128 entries from the first sprite
    /// its evaluation starts at.
    fn sprite_table(&self) -> Vec<SpriteTableEntry>;

    /// Where layer `layer_id` (as [`Self::get_layer_data`] numbers it)
    /// reads its map and its characters; `None` for a layer the chip lacks.
    fn layer_layout(&self, layer_id: u32) -> Option<LayerLayout>;

    /// The palette memory's colour words, in colour order: its bytes read
    /// in its byte order. Empty on a chip with no palette memory.
    fn palette_memory(&self) -> Vec<u16> {
        match self.read_video_memory(VideoMemoryId::Palette) {
            Some(bytes) => words_from_bytes(&bytes, palette_is_little_endian(self)),
            None => Vec::new(),
        }
    }

    /// Write colour words from colour `first_colour` on, as
    /// [`Self::write_video_memory`] writes the palette memory: the colour
    /// wraps at the palette's end, a longer buffer is cut, and each word
    /// keeps the bits the palette's port keeps.
    fn write_palette_memory(&mut self, first_colour: u32, colours: &[u16]) {
        let bytes = bytes_from_words(colours, palette_is_little_endian(self));
        self.write_video_memory(VideoMemoryId::Palette, first_colour.wrapping_mul(2), &bytes);
    }

    /// Every palette colour as RGBA, 4 bytes a colour in colour order, as
    /// [`Self::get_palette_data`] gives them: the renderer's conversion at
    /// full brightness, alpha 255.
    fn palette_colours(&self) -> Vec<u8> {
        self.get_palette_data()
            .iter()
            .flat_map(|palette| palette.colors.iter())
            .flat_map(|colour| colour.to_be_bytes())
            .collect()
    }

    /// [`Self::decode_tiles`] in colour, `width * height * 4` bytes of
    /// RGBA: pixel index `i` takes palette colour `first_colour + i`
    /// ([`Self::palette_colours`]), wrapping at the palette's end as the
    /// palette writer's colour wraps, and the transparent index keeps its
    /// colour's RGB at alpha 0. Refused as `decode_tiles` refuses, and as
    /// unsupported on a chip with no palette.
    fn tile_colours(
        &self,
        request: &TileDecodeRequest,
        first_colour: u32,
    ) -> Result<PixelImage, ExportError> {
        let image = self.decode_tiles(request)?;
        let colours = self.palette_colours();
        let count = (colours.len() / 4) as u64;
        if count == 0 {
            return Err(ExportError::Unsupported);
        }
        let mut rgba = Vec::with_capacity(image.pixels.len() * 4);
        for &index in &image.pixels {
            let colour = ((u64::from(first_colour) + u64::from(index)) % count) as usize;
            rgba.extend_from_slice(&colours[colour * 4..colour * 4 + 3]);
            rgba.push(if image.transparent_index == Some(index) {
                0
            } else {
                255
            });
        }
        Ok(PixelImage {
            width: image.width,
            height: image.height,
            rgba,
        })
    }
}

/// Whether `video`'s palette memory keeps a word's low byte first; `true`
/// when the chip names no palette memory.
fn palette_is_little_endian<V: VideoDebugger + ?Sized>(video: &V) -> bool {
    video
        .video_memories()
        .iter()
        .find(|memory| memory.id == VideoMemoryId::Palette)
        .is_none_or(|memory| memory.little_endian)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layer_mask_shows_every_layer_until_one_is_hidden() {
        let mut mask = LayerMask::<4>::default();
        assert!((0..4).all(|layer| !mask.is_hidden(layer)));
        mask.set_hidden(1, true);
        assert!(mask.is_hidden(1));
        assert!(!mask.is_hidden(0) && !mask.is_hidden(2) && !mask.is_hidden(3));
        mask.set_hidden(1, false);
        assert_eq!(mask, LayerMask::default());
    }

    #[test]
    fn a_layer_past_the_mask_is_ignored_and_reads_shown() {
        let mut mask = LayerMask::<2>::default();
        mask.set_hidden(5, true);
        assert_eq!(mask, LayerMask::default());
        assert!(!mask.is_hidden(5));
    }

    #[test]
    fn taking_the_mask_leaves_every_layer_shown() {
        let mut mask = LayerMask::<3>::default();
        mask.set_hidden(2, true);
        let taken = mask.take();
        assert!(taken.is_hidden(2));
        assert_eq!(mask, LayerMask::default());
    }

    #[test]
    fn a_video_memory_is_read_by_the_name_it_gives() {
        for id in VideoMemoryId::ALL {
            assert_eq!(VideoMemoryId::parse(id.name()), Some(id));
        }
        assert_eq!(
            VideoMemoryId::parse("vertical_scroll"),
            Some(VideoMemoryId::VerticalScroll)
        );
        assert_eq!(VideoMemoryId::parse("VRAM"), None);
        assert_eq!(VideoMemoryId::parse(""), None);
    }

    #[test]
    fn words_and_bytes_follow_the_order_given() {
        assert_eq!(words_from_bytes(&[1, 2, 3, 4, 5], true), [0x0201, 0x0403]);
        assert_eq!(words_from_bytes(&[1, 2, 3, 4], false), [0x0102, 0x0304]);
        assert_eq!(bytes_from_words(&[0x0102], true), [2, 1]);
        assert_eq!(bytes_from_words(&[0x0102], false), [1, 2]);
    }

    /// A chip with a palette of four colours, words kept high byte first
    /// (or low first), each keeping its low 12 bits, and one 2x1 tile whose
    /// pixels are indices 0 and 2.
    struct Chip {
        palette: [u16; 4],
        little_endian: bool,
    }

    impl VideoDebugger for Chip {
        fn get_tile_data(&self, _index: u32) -> Option<TileData> {
            None
        }
        fn get_tile_count(&self) -> u32 {
            0
        }
        fn get_palette_data(&self) -> Vec<PaletteData> {
            vec![PaletteData {
                index: 0,
                colors: self
                    .palette
                    .iter()
                    .map(|&colour| (u32::from(colour) << 8) | 0xFF)
                    .collect(),
            }]
        }
        fn get_sprite_data(&self) -> Vec<SpriteData> {
            Vec::new()
        }
        fn get_layer_data(&self, _layer_id: u32) -> Option<LayerData> {
            None
        }
        fn get_layer_count(&self) -> u32 {
            0
        }
        fn toggle_layer(&mut self, _layer_id: u32, _visible: bool) {}
        fn get_vram(&self) -> &[u8] {
            &[]
        }
        fn get_registers(&self) -> Vec<VideoRegister> {
            Vec::new()
        }
        fn decode_tiles(&self, request: &TileDecodeRequest) -> Result<IndexedImage, ExportError> {
            if request.first_tile != 0 {
                return Err(ExportError::OutOfRange);
            }
            Ok(IndexedImage {
                width: 2,
                height: 1,
                pixels: vec![0, 2],
                bpp: 2,
                transparent_index: Some(0),
            })
        }
        fn video_memories(&self) -> Vec<VideoMemoryInfo> {
            vec![VideoMemoryInfo {
                id: VideoMemoryId::Palette,
                name: "PAL",
                size: 8,
                address_unit: 2,
                little_endian: self.little_endian,
            }]
        }
        fn read_video_memory(&self, id: VideoMemoryId) -> Option<Vec<u8>> {
            (id == VideoMemoryId::Palette)
                .then(|| bytes_from_words(&self.palette, self.little_endian))
        }
        fn write_video_memory(&mut self, id: VideoMemoryId, offset: u32, bytes: &[u8]) -> bool {
            if id != VideoMemoryId::Palette {
                return false;
            }
            let words = words_from_bytes(bytes, self.little_endian);
            for (i, word) in words.iter().take(4).enumerate() {
                self.palette[(offset as usize / 2 + i) % 4] = word & 0x0FFF;
            }
            true
        }
        fn sprite_table(&self) -> Vec<SpriteTableEntry> {
            Vec::new()
        }
        fn layer_layout(&self, _layer_id: u32) -> Option<LayerLayout> {
            None
        }
    }

    fn request(first_tile: u32) -> TileDecodeRequest {
        TileDecodeRequest {
            source: TileSource::Vram,
            base_addr: 0,
            first_tile,
            count: 1,
            bpp: 2,
            tile_size: TileSize::Px8,
        }
    }

    #[test]
    fn the_palette_is_read_and_written_as_words_in_the_memory_s_order() {
        for little_endian in [false, true] {
            let mut chip = Chip {
                palette: [0x0111, 0x0222, 0x0333, 0x0444],
                little_endian,
            };
            assert_eq!(chip.palette_memory(), [0x0111, 0x0222, 0x0333, 0x0444]);
            chip.write_palette_memory(3, &[0xF0AB, 0x0CDE]);
            assert_eq!(
                chip.palette_memory(),
                [0x0CDE, 0x0222, 0x0333, 0x00AB],
                "the colour wraps, the port's bits kept"
            );
        }
    }

    #[test]
    fn the_palette_s_colours_are_its_rgba() {
        let chip = Chip {
            palette: [0x0102, 0x0304, 0x0506, 0x0708],
            little_endian: false,
        };
        assert_eq!(
            chip.palette_colours(),
            [0, 1, 2, 255, 0, 3, 4, 255, 0, 5, 6, 255, 0, 7, 8, 255]
        );
    }

    #[test]
    fn tiles_in_colour_take_the_palette_from_the_first_colour_given() {
        let chip = Chip {
            palette: [0x0102, 0x0304, 0x0506, 0x0708],
            little_endian: true,
        };
        let picture = chip.tile_colours(&request(0), 1).expect("the tile");
        assert_eq!((picture.width, picture.height), (2, 1));
        // Index 0 is colour 1, transparent; index 2 is colour 3.
        assert_eq!(picture.rgba, [0, 3, 4, 0, 0, 7, 8, 255]);
        // Colour 3 + 2 wraps to colour 1.
        let wrapped = chip.tile_colours(&request(0), 3).expect("the tile");
        assert_eq!(wrapped.rgba, [0, 7, 8, 0, 0, 3, 4, 255]);
        assert_eq!(
            chip.tile_colours(&request(1), 0),
            Err(ExportError::OutOfRange)
        );
    }

    /// A framebuffer 4 pixels wide and 3 rows high, each byte its own index.
    fn framebuffer() -> Vec<u8> {
        (0..4 * 3 * 4).map(|byte| byte as u8).collect()
    }

    #[test]
    fn a_crop_keeps_the_first_rows_each_cut_to_the_width() {
        let source = framebuffer();
        let picture = crop_rgba(&source, 4, 2, 2);
        assert_eq!(
            picture,
            [0, 1, 2, 3, 4, 5, 6, 7, 16, 17, 18, 19, 20, 21, 22, 23]
        );
        assert_eq!(crop_rgba(&source, 4, 4, 3), source, "the whole picture");
        assert!(crop_rgba(&source, 4, 0, 3).is_empty());
        assert!(crop_rgba(&source, 4, 2, 0).is_empty());
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn a_crop_past_the_last_row_is_a_mistake() {
        crop_rgba(&framebuffer(), 4, 2, 4);
    }

    #[test]
    fn a_picture_equals_its_crop_and_nothing_else() {
        let source = framebuffer();
        let picture = crop_rgba(&source, 4, 3, 2);
        assert!(equals_cropped_rgba(&source, 4, 3, 2, &picture));

        let mut changed = picture.clone();
        changed[13] ^= 1;
        assert!(!equals_cropped_rgba(&source, 4, 3, 2, &changed));

        // Another size is another picture, whatever its bytes.
        assert!(!equals_cropped_rgba(&source, 4, 3, 2, &picture[..12]));
        assert!(!equals_cropped_rgba(&source, 4, 2, 3, &picture));
        let mut longer = picture.clone();
        longer.extend([0; 4]);
        assert!(!equals_cropped_rgba(&source, 4, 3, 2, &longer));

        // An empty picture is the crop of no pixel.
        assert!(equals_cropped_rgba(&source, 4, 0, 2, &[]));
        assert!(equals_cropped_rgba(&source, 4, 3, 0, &[]));
        assert!(!equals_cropped_rgba(&source, 4, 0, 2, &[1, 2, 3, 4]));
    }
}
