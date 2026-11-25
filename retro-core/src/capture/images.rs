//! The images a host reads of a held frame, named as files: the picture,
//! each layer alone, each layer's whole map, each sprite the frame drew and
//! those sprites together, the video memory's characters in each palette
//! line in use, and the palette ([`frame_images`]). [`palette_swatches`]
//! and [`tile_grid`] lay out the palette's colours and a run of characters
//! as pictures.

use crate::capture::CaptureCore;
use crate::video::{PixelImage, TileDecodeRequest, TileSize, TileSource, VideoMemoryId};

/// Colours a palette line holds, as the tile grids count the lines.
pub const COLOURS_PER_LINE: u32 = 16;

/// Swatches a row of the palette's picture, and characters a row of a tile
/// grid, as [`frame_images`] lays them out.
pub const PER_ROW: u32 = 16;

/// The side of a swatch of the palette's picture, in pixels, as
/// [`frame_images`] draws it.
pub const SWATCH: u32 = 16;

/// Bytes of a character of 8 by 8 pixels of 4 bits, the characters the tile
/// grids read the video memory as.
const CHARACTER_BYTES: u32 = 32;

/// The file of an image set's folder that holds the program its images are
/// of. An image set is a folder a core's tests keep: the program, a folder
/// a frame of its images ([`frame_folder`]), each image a PNG named as
/// [`frame_images`] names it, and their hashes ([`IMAGE_SET_HASHES`]).
pub const IMAGE_SET_PROGRAM: &str = "program.bin";

/// The file of an image set's folder that holds its images' hashes.
pub const IMAGE_SET_HASHES: &str = "images.hashes";

/// The folder of frame `frame`'s images in an image set's folder, the frame
/// counted from 1: `frame-00008`.
pub fn frame_folder(frame: u64) -> String {
    format!("frame-{frame:05}")
}

/// The frame a folder of an image set named by [`frame_folder`] holds the
/// images of; `None` for a name it does not give.
pub fn folder_frame(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("frame-")?;
    (digits.len() >= 5 && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// The colours of `colours` (RGBA, four bytes a colour, as
/// [`crate::video::VideoDebugger::palette_colours`] gives them) as
/// swatches, `per_row` of them a row from the top left, each a square of
/// `cell` pixels in its colour; the part of the last row the colours do
/// not fill is transparent.
pub fn palette_swatches(colours: &[u8], per_row: u32, cell: u32) -> PixelImage {
    let per_row = per_row.max(1);
    let count = (colours.len() / 4) as u32;
    let (width, height) = (per_row * cell, count.div_ceil(per_row) * cell);
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for (index, colour) in (0u32..).zip(colours.as_chunks::<4>().0) {
        let (x, y) = ((index % per_row) * cell, (index / per_row) * cell);
        for row in y..y + cell {
            let start = ((row * width + x) * 4) as usize;
            for pixel in rgba[start..start + (cell * 4) as usize]
                .as_chunks_mut::<4>()
                .0
            {
                *pixel = *colour;
            }
        }
    }
    PixelImage {
        width,
        height,
        rgba,
    }
}

/// The characters of `tiles`, one under another as
/// [`crate::video::VideoDebugger::tile_colours`] gives them (each as tall
/// as the picture is wide), laid out `per_row` of them a row from the top
/// left; the part of the last row they do not fill is transparent.
pub fn tile_grid(tiles: &PixelImage, per_row: u32) -> PixelImage {
    let per_row = per_row.max(1);
    let side = tiles.width;
    let count = tiles.height.checked_div(side).unwrap_or(0);
    let (width, height) = (per_row * side, count.div_ceil(per_row) * side);
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    let row_bytes = (side * 4) as usize;
    for tile in 0..count {
        let (x, y) = ((tile % per_row) * side, (tile / per_row) * side);
        for row in 0..side {
            let from = ((tile * side + row) * side * 4) as usize;
            let to = (((y + row) * width + x) * 4) as usize;
            rgba[to..to + row_bytes].copy_from_slice(&tiles.rgba[from..from + row_bytes]);
        }
    }
    PixelImage {
        width,
        height,
        rgba,
    }
}

/// The palette lines in use in `palette` (its colour words, as
/// [`crate::video::VideoDebugger::palette_memory`] gives them): each line
/// of [`COLOURS_PER_LINE`] colours, counted from the first, in which one of
/// the colours a character's pixel can show through it (colours 1 to 15;
/// colour 0 is transparent) is not the word 0.
pub fn palette_lines_in_use(palette: &[u16]) -> Vec<u32> {
    (0u32..)
        .zip(palette.chunks(COLOURS_PER_LINE as usize))
        .filter(|(_, colours)| colours.iter().skip(1).any(|&word| word != 0))
        .map(|(line, _)| line)
        .collect()
}

/// The part of a file's name that names layer `name`: lower case, its
/// spaces as underscores (`Plane A` is `plane_a`, `BG1` is `bg1`).
pub fn layer_file_name(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace(' ', "_")
}

/// What an image of a held frame shows ([`FrameImage`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    /// The picture.
    Picture,
    /// A layer drawn alone.
    Layer,
    /// A layer's whole map.
    Map,
    /// One sprite of the sprite table.
    Sprite,
    /// The sprites the frame drew, together.
    Sprites,
    /// The video memory's characters in a palette line.
    Tiles,
    /// The palette.
    Palette,
}

impl ImageKind {
    /// The kind's name, as a file list writes it (`"map"`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Picture => "picture",
            Self::Layer => "layer",
            Self::Map => "map",
            Self::Sprite => "sprite",
            Self::Sprites => "sprites",
            Self::Tiles => "tiles",
            Self::Palette => "palette",
        }
    }
}

/// One image of a held frame ([`frame_images`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameImage {
    /// The name of its file, without the extension (`map_plane_a`).
    pub name: String,
    /// What it shows.
    pub kind: ImageKind,
    /// What it is, in words (`Plane A's whole map`).
    pub about: String,
    /// The image, RGBA.
    pub image: PixelImage,
}

impl FrameImage {
    fn new(name: String, kind: ImageKind, about: String, image: PixelImage) -> Self {
        Self {
            name,
            kind,
            about,
            image,
        }
    }
}

/// Which images of a held frame [`frame_images`] takes besides the picture,
/// which it always takes; none by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImageSubjects {
    /// Each layer drawn alone.
    pub layers: bool,
    /// Each layer's whole map.
    pub maps: bool,
    /// Each sprite the frame drew, and those sprites together.
    pub sprites: bool,
    /// The video memory's characters in each palette line in use.
    pub tiles: bool,
    /// The palette.
    pub palette: bool,
}

impl ImageSubjects {
    /// Every image.
    pub const ALL: Self = Self {
        layers: true,
        maps: true,
        sprites: true,
        tiles: true,
        palette: true,
    };
}

/// The images of the frame `emu` holds, in this order:
///
/// - `picture`: the picture drawn again from the line table
///   ([`CaptureCore::picture_from_line_states`]), left out when the table
///   does not hold the whole frame;
/// - `layer_<name>`: each layer drawn alone ([`CaptureCore::render_layer`]),
///   `<name>` the layer's ([`layer_file_name`]);
/// - `map_<name>`: each layer's whole map
///   ([`crate::video::VideoDebugger::layer_map`]), for each layer that has
///   one;
/// - `sprite_<slot>`: each sprite the frame drew, by its slot in the sprite
///   table written with three digits, in the order of the slots
///   ([`crate::video::VideoDebugger::resolved_sprite_render_order`],
///   [`crate::video::VideoDebugger::export_sprite_image`]), then `sprites`,
///   those sprites together as the frame drew them
///   ([`CaptureCore::render_sprites`]), transparent when it drew none;
/// - `tiles_palette_<line>`: the video memory read as characters of 8 by 8
///   pixels of 4 bits, [`PER_ROW`] a row ([`tile_grid`]), in each palette
///   line in use ([`palette_lines_in_use`]), the line written with two
///   digits ([`crate::video::VideoDebugger::tile_colours`]);
/// - `palette`: the palette's colours, [`PER_ROW`] a row, each a square of
///   [`SWATCH`] pixels ([`palette_swatches`]).
///
/// `subjects` chooses which of the last five kinds are taken. Nothing runs,
/// and nothing a savestate or the state hash reads moves.
pub fn frame_images<E: CaptureCore>(emu: &mut E, subjects: ImageSubjects) -> Vec<FrameImage> {
    let mut images = Vec::new();
    if let Some(picture) = emu.picture_from_line_states() {
        images.push(FrameImage::new(
            "picture".to_string(),
            ImageKind::Picture,
            "The picture, drawn again from the frame's line table".to_string(),
            picture,
        ));
    }
    let layers: Vec<(u32, String)> = {
        let video = emu.video_debugger();
        (0..video.get_layer_count())
            .map(|id| {
                let name = video
                    .get_layer_data(id)
                    .map_or_else(|| format!("Layer {id}"), |data| data.name);
                (id, name)
            })
            .collect()
    };
    if subjects.layers {
        for (id, name) in &layers {
            if let Some(layer) = emu.render_layer(*id) {
                images.push(FrameImage::new(
                    format!("layer_{}", layer_file_name(name)),
                    ImageKind::Layer,
                    format!("{name} drawn alone, transparent where it draws nothing"),
                    layer.image,
                ));
            }
        }
    }
    if subjects.maps {
        for (id, name) in &layers {
            if let Some(map) = emu.video_debugger().layer_map(*id) {
                images.push(FrameImage::new(
                    format!("map_{}", layer_file_name(name)),
                    ImageKind::Map,
                    format!("{name}'s whole map, with no scroll, transparent where colour 0"),
                    map,
                ));
            }
        }
    }
    if subjects.sprites {
        sprite_images(emu, &mut images);
    }
    if subjects.tiles {
        tile_images(emu, &mut images);
    }
    if subjects.palette {
        let colours = emu.video_debugger().palette_colours();
        images.push(FrameImage::new(
            "palette".to_string(),
            ImageKind::Palette,
            format!("The palette, {PER_ROW} colours a row"),
            palette_swatches(&colours, PER_ROW, SWATCH),
        ));
    }
    images
}

/// Each sprite the held frame drew, by slot, then those sprites together.
fn sprite_images<E: CaptureCore>(emu: &mut E, images: &mut Vec<FrameImage>) {
    let mut slots: Vec<u32> = emu
        .video_debugger()
        .resolved_sprite_render_order()
        .iter()
        .map(|entry| entry.index)
        .collect();
    slots.sort_unstable();
    slots.dedup();
    let mut selection = vec![0u8; slots.last().map_or(0, |&last| last as usize + 1)];
    for &slot in &slots {
        selection[slot as usize] = 1;
        if let Ok(sprite) = emu.video_debugger().export_sprite_image(slot) {
            images.push(FrameImage::new(
                format!("sprite_{slot:03}"),
                ImageKind::Sprite,
                format!("Sprite {slot} of the sprite table, which the frame drew"),
                sprite,
            ));
        }
    }
    if let Some(sprites) = emu.render_sprites(&selection) {
        images.push(FrameImage::new(
            "sprites".to_string(),
            ImageKind::Sprites,
            "The sprites the frame drew, together, as it drew them".to_string(),
            sprites.image,
        ));
    }
}

/// The video memory's characters in each palette line in use.
fn tile_images<E: CaptureCore>(emu: &mut E, images: &mut Vec<FrameImage>) {
    let video = emu.video_debugger();
    let Some(vram) = video
        .video_memories()
        .into_iter()
        .find(|memory| memory.id == VideoMemoryId::Vram)
    else {
        return;
    };
    let request = TileDecodeRequest {
        source: TileSource::Vram,
        base_addr: 0,
        first_tile: 0,
        count: vram.size / CHARACTER_BYTES,
        bpp: 4,
        tile_size: TileSize::Px8,
    };
    for line in palette_lines_in_use(&video.palette_memory()) {
        let first = line * COLOURS_PER_LINE;
        if let Ok(tiles) = video.tile_colours(&request, first) {
            images.push(FrameImage::new(
                format!("tiles_palette_{line:02}"),
                ImageKind::Tiles,
                format!(
                    "The video memory read as characters of 8 by 8 pixels of 4 bits, {PER_ROW} a \
                     row, in palette line {line} (colours {first} to {})",
                    first + COLOURS_PER_LINE - 1
                ),
                tile_grid(&tiles, PER_ROW),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pixel at `(x, y)` of `image`.
    fn pixel(image: &PixelImage, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * image.width + x) * 4) as usize;
        image.rgba[at..at + 4].try_into().expect("four bytes")
    }

    #[test]
    fn swatches_lay_the_colours_out_a_row_at_a_time() {
        let colours: Vec<u8> = (1..=5u8).flat_map(|c| [c, c * 10, c * 20, 255]).collect();
        let swatches = palette_swatches(&colours, 2, 3);
        assert_eq!((swatches.width, swatches.height), (6, 9));
        assert_eq!(swatches.rgba.len(), 6 * 9 * 4);
        assert_eq!(pixel(&swatches, 0, 0), [1, 10, 20, 255]);
        assert_eq!(pixel(&swatches, 2, 2), [1, 10, 20, 255], "a square of 3");
        assert_eq!(pixel(&swatches, 3, 0), [2, 20, 40, 255]);
        assert_eq!(pixel(&swatches, 5, 5), [4, 40, 80, 255]);
        assert_eq!(pixel(&swatches, 0, 6), [5, 50, 100, 255]);
        assert_eq!(pixel(&swatches, 3, 8), [0; 4], "past the last colour");
        let none = palette_swatches(&[], 16, 16);
        assert_eq!((none.width, none.height), (256, 0));
        assert!(none.rgba.is_empty());
    }

    #[test]
    fn a_tile_grid_lays_the_characters_out_a_row_at_a_time() {
        // Three characters of 2 by 2, one under another; each pixel's red
        // is its character, its green its row, its blue its column.
        let mut rgba = Vec::new();
        for tile in 0..3u8 {
            for row in 0..2u8 {
                for column in 0..2u8 {
                    rgba.extend_from_slice(&[tile, row, column, 255]);
                }
            }
        }
        let tiles = PixelImage {
            width: 2,
            height: 6,
            rgba,
        };
        let grid = tile_grid(&tiles, 2);
        assert_eq!((grid.width, grid.height), (4, 4));
        assert_eq!(pixel(&grid, 0, 0), [0, 0, 0, 255]);
        assert_eq!(pixel(&grid, 1, 1), [0, 1, 1, 255]);
        assert_eq!(pixel(&grid, 2, 0), [1, 0, 0, 255], "the second beside it");
        assert_eq!(pixel(&grid, 3, 1), [1, 1, 1, 255]);
        assert_eq!(pixel(&grid, 1, 2), [2, 0, 1, 255], "the third under them");
        assert_eq!(pixel(&grid, 2, 2), [0; 4], "past the last character");
        let wide = tile_grid(&tiles, 16);
        assert_eq!((wide.width, wide.height), (32, 2));
        let empty = tile_grid(
            &PixelImage {
                width: 0,
                height: 0,
                rgba: Vec::new(),
            },
            16,
        );
        assert_eq!((empty.width, empty.height), (0, 0));
    }

    #[test]
    fn a_palette_line_is_in_use_when_a_colour_a_pixel_shows_is_set() {
        let mut palette = [0u16; 64];
        palette[0] = 0x0EEE; // colour 0 of line 0: a pixel never shows it
        palette[16 + 15] = 0x0002;
        palette[48 + 1] = 0x0E00;
        assert_eq!(palette_lines_in_use(&palette), [1, 3]);
        assert!(palette_lines_in_use(&[0; 32]).is_empty());
        assert_eq!(palette_lines_in_use(&[0, 0, 7]), [0], "a short last line");
    }

    #[test]
    fn a_frame_s_folder_is_named_by_its_number_and_read_back() {
        assert_eq!(frame_folder(8), "frame-00008");
        assert_eq!(frame_folder(12345), "frame-12345");
        assert_eq!(folder_frame("frame-00008"), Some(8));
        assert_eq!(folder_frame(&frame_folder(600)), Some(600));
        assert_eq!(folder_frame(&frame_folder(123_456)), Some(123_456));
        for other in [
            "frame-8",
            "frame-0000a",
            "frames-00008",
            "images.hashes",
            "",
        ] {
            assert_eq!(folder_frame(other), None, "{other}");
        }
    }

    #[test]
    fn a_layer_file_is_named_after_its_layer() {
        assert_eq!(layer_file_name("Plane A"), "plane_a");
        assert_eq!(layer_file_name("BG1"), "bg1");
        assert_eq!(layer_file_name("OBJ"), "obj");
        assert_eq!(layer_file_name(" Sprites "), "sprites");
    }

    #[test]
    fn every_kind_has_its_own_name() {
        let kinds = [
            ImageKind::Picture,
            ImageKind::Layer,
            ImageKind::Map,
            ImageKind::Sprite,
            ImageKind::Sprites,
            ImageKind::Tiles,
            ImageKind::Palette,
        ];
        let mut names: Vec<&str> = kinds.iter().map(|kind| kind.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), kinds.len());
        assert_eq!(ImageKind::Map.name(), "map");
    }
}
