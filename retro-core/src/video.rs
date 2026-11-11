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
/// SNES 9-bit X wrap) so a sprite straddling the left/top edge reports a
/// negative coordinate and the host can place every sprite in one frame
/// without re-deriving hardware quirks. This is the single canonical
/// sprite list both cores expose for composite/object capture.
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
/// `priority` is the console's per-sprite priority attribute (Genesis
/// priority bit, SNES 2-bit OBJ priority), carried for UI/debug display;
/// it is *not* the sole ordering key, `index`'s position in the vector
/// already encodes the fully-resolved order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpriteOrderEntry {
    pub index: u32,
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
/// request shape stays stable for the consoles that need them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileDecodeRequest {
    pub source: TileSource,
    pub base_addr: u32,
    pub first_tile: u32,
    pub count: u32,
    pub bpp: u8,
    pub tile_size: TileSize,
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

    /// The hardware sprite draw order exactly as the *renderer itself*
    /// resolves it (Genesis SAT link chain, SNES OAM order from the
    /// rotated first-sprite), returned back-to-front (see
    /// [`SpriteOrderEntry`]). Consumers blit selected members in this
    /// order so a composite object reproduces on-screen overlaps without
    /// re-deriving priority rules and drifting from the emulator.
    ///
    /// Default: empty, cores without a sprite renderer expose no order.
    fn resolved_sprite_render_order(&self) -> Vec<SpriteOrderEntry> {
        Vec::new()
    }

    /// Get layer data by ID.
    fn get_layer_data(&self, layer_id: u32) -> Option<LayerData>;

    /// Get total number of layers.
    fn get_layer_count(&self) -> u32;

    /// Toggle layer visibility.
    fn toggle_layer(&mut self, layer_id: u32, visible: bool);

    /// Re-composite the current frame from the present video state
    /// (VRAM/palette/registers plus any debugger layer mask) into the
    /// core's framebuffer, without advancing emulation.
    ///
    /// The live frame loop repaints every frame, so this only matters
    /// while the core is paused: after a host toggles a layer it can call
    /// `repaint()` to make the change visible immediately instead of
    /// waiting for the game to resume. Default: no-op, cores that have no
    /// re-render path (or always composite on framebuffer read) need
    /// nothing here.
    fn repaint(&mut self) {}

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
}
