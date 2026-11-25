//! [`video_bindings!`], which writes the video chip's methods every binding
//! crate exports the same way over `retro_core`'s `VideoDebugger`, and the
//! request of a tile decode as a host sends it.
//!
//! [`video_bindings!`]: crate::video_bindings

use retro_core::video::{TileDecodeRequest, TileSize, TileSource};

/// A tile decode as a host asks for it: `source` 0 the video memory's
/// characters, 1 the Mode 7 characters; `tile_size` 0 for 8 by 8, 1 for the
/// 16 by 16 block a background tile names. `None` for a source or size no
/// console has.
pub fn tile_request(
    source: u32,
    base_addr: u32,
    first_tile: u32,
    count: u32,
    bpp: u8,
    tile_size: u32,
) -> Option<TileDecodeRequest> {
    let source = match source {
        0 => TileSource::Vram,
        1 => TileSource::Mode7,
        _ => return None,
    };
    let tile_size = match tile_size {
        0 => TileSize::Px8,
        1 => TileSize::Px16,
        _ => return None,
    };
    Some(TileDecodeRequest {
        source,
        base_addr,
        first_tile,
        count,
        bpp,
        tile_size,
    })
}

/// Writes the video chip's methods on `$emulator`, over the
/// `EmulatorCore` in its field `$core` and its `video_debugger()`:
/// `getVram()`, `getLayerInfo()`, `toggleLayer(layer_id, visible)`,
/// `exportSpriteImage(index)`, `getSpriteData()`,
/// `resolvedSpriteRenderOrder()` and `decodeTiles(...)`; the registers,
/// `getVideoRegisters()`; the video memories, `getVideoMemories()`,
/// `readVideoMemory(id)` and `writeVideoMemory(id, offset, bytes)`; the
/// palette, `getPaletteMemory()`, `writePaletteMemory(colour_index,
/// colours)` and `getPaletteColours()`; `getTileColours(...)`, tiles in
/// colour; `getSpriteTable()`; `getLayerLayout(layer_id)`;
/// `layerMap(layer_id)`, a layer's whole map; and `getColourLevels()`, the
/// levels the picture's colour channels take.
///
/// Each method's Rust name is its JavaScript name in snake case, which no
/// other method of the struct may have. The crate expanding the macro
/// depends on `wasm-bindgen` itself, as every crate declaring a
/// `#[wasm_bindgen]` struct does.
#[macro_export]
macro_rules! video_bindings {
    ($emulator:ident, $core:ident $(,)?) => {
        #[$crate::__private::wasm_bindgen::prelude::wasm_bindgen]
        impl $emulator {
            /// A copy of the video memory (VRAM, 64 KiB), in the byte order
            /// the chip's debugger reads it, for a hex view.
            #[wasm_bindgen(js_name = getVram)]
            pub fn get_vram(&mut self) -> ::std::vec::Vec<u8> {
                $crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                    .get_vram()
                    .to_vec()
            }

            /// Each layer the debugger can show or hide (`LayerData[]`: its
            /// id, name, size, scroll and whether it is shown, and
            /// `line_scroll`, `{ line, x, y }[]`, the scroll each line the
            /// line table holds was drawn with), for a layer list.
            #[wasm_bindgen(js_name = getLayerInfo)]
            pub fn get_layer_info(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                let video =
                    $crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core);
                let count = video.get_layer_count();
                let mut layers = ::std::vec::Vec::with_capacity(count as usize);
                for id in 0..count {
                    if let ::core::option::Option::Some(layer) = video.get_layer_data(id) {
                        layers.push(layer);
                    }
                }
                $crate::js::to_js(&layers)
            }

            /// Show or hide a layer in the picture (`getLayerInfo`'s ids). A
            /// hidden layer is left out of the composite without touching
            /// the game's registers.
            #[wasm_bindgen(js_name = toggleLayer)]
            pub fn toggle_layer(&mut self, layer_id: u32, visible: bool) {
                $crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                    .toggle_layer(layer_id, visible);
            }

            /// One sprite (by its index in the sprite table) as a tight,
            /// transparent RGBA picture, `{ width, height, data: Uint8Array }`,
            /// or `null` for an index out of range or an empty sprite. The
            /// core decodes it as the hardware does (its tiles, flips and
            /// palette, colour 0 transparent); the host encodes it.
            #[wasm_bindgen(js_name = exportSpriteImage)]
            pub fn export_sprite_image(
                &mut self,
                index: u32,
            ) -> $crate::__private::wasm_bindgen::JsValue {
                match $crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                    .export_sprite_image(index)
                {
                    ::core::result::Result::Ok(picture) => $crate::js::picture_object(&picture),
                    ::core::result::Result::Err(_) => {
                        $crate::__private::wasm_bindgen::JsValue::NULL
                    }
                }
            }

            /// Every sprite of the sprite table (`SpriteData[]`), in signed
            /// screen coordinates (the console's offsets undone), for an
            /// object picker that works the same on every console: `x`, `y`,
            /// `width` and `height` count the console's dots and lines, and
            /// `picture` (`{ x, y, width, height }`) is the box the sprite
            /// covers on the picture, in its pixels.
            #[wasm_bindgen(js_name = getSpriteData)]
            pub fn get_sprite_data(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js(
                    &$crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                        .get_sprite_data(),
                )
            }

            /// The sprites the held frame drew, as `SpriteOrderEntry[]` from
            /// back to front (the last draws on top), so a composite is
            /// drawn as the chip drew it: only sprites of which some line
            /// drew a visible column, each once, with the priority the first
            /// line that drew it read; an empty array when the frame drew
            /// none.
            #[wasm_bindgen(js_name = resolvedSpriteRenderOrder)]
            pub fn resolved_sprite_render_order(
                &mut self,
            ) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js(
                    &$crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                        .resolved_sprite_render_order(),
                )
            }

            /// Decode a run of tiles to an indexed sheet, for a host that
            /// assembles a whole map itself: `{ width, height, pixels:
            /// Uint8Array, bpp, transparentIndex }`, one palette index a
            /// pixel, `count` tiles stacked down, or `null` for a request
            /// the chip does not take or out of range. The core decodes only
            /// the characters; the host reads the map and applies palettes,
            /// flips and transparency.
            ///
            /// `source`: 0 the video memory's characters from `baseAddr` (a
            /// byte address), 1 the Mode 7 characters (the SNES only;
            /// `baseAddr` 0 for the plane's own). `tileSize`: 0 for 8 by 8, 1
            /// for the 16 by 16 block a background tile names (`firstTile`
            /// its tile number). `bpp` 2, 4 or 8 where the console has them.
            #[wasm_bindgen(js_name = decodeTiles)]
            pub fn decode_tiles(
                &mut self,
                source: u32,
                base_addr: u32,
                first_tile: u32,
                count: u32,
                bpp: u8,
                tile_size: u32,
            ) -> $crate::__private::wasm_bindgen::JsValue {
                let ::core::option::Option::Some(request) = $crate::video::tile_request(
                    source, base_addr, first_tile, count, bpp, tile_size,
                ) else {
                    return $crate::__private::wasm_bindgen::JsValue::NULL;
                };
                match $crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                    .decode_tiles(&request)
                {
                    ::core::result::Result::Ok(image) => $crate::js::indexed_image_object(&image),
                    ::core::result::Result::Err(_) => {
                        $crate::__private::wasm_bindgen::JsValue::NULL
                    }
                }
            }

            /// The video chip's registers as `VideoRegister[]` (`{ index,
            /// name, value, description }`), for a state view.
            #[wasm_bindgen(js_name = getVideoRegisters)]
            pub fn get_video_registers(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js(
                    &$crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                        .get_registers(),
                )
            }

            /// The video memories the chip has, `VideoMemoryInfo[]`: each
            /// one's `id` (the name `readVideoMemory` and `writeVideoMemory`
            /// take: `"vram"`, `"palette"`, `"vertical_scroll"`,
            /// `"sprite_table"`), its console's `name`, its `size` in bytes,
            /// its `address_unit` (the bytes one address of the console's
            /// own writer counts: `writeVram`'s is 1 on the Mega Drive and 2
            /// on the SNES, a palette's and the vertical scroll's 2, the
            /// SNES's sprite table's 1) and `little_endian` (a 16-bit word's
            /// low byte first).
            #[wasm_bindgen(js_name = getVideoMemories)]
            pub fn get_video_memories(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js(
                    &$crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                        .video_memories(),
                )
            }

            /// A copy of video memory `id`'s bytes, in its byte order
            /// (`getVideoMemories`); `"vram"` is `getVram()`. `undefined` for
            /// a memory the chip lacks or a name no console has.
            #[wasm_bindgen(js_name = readVideoMemory)]
            pub fn read_video_memory(
                &mut self,
                id: &str,
            ) -> ::core::option::Option<::std::vec::Vec<u8>> {
                let id = $crate::__private::retro_core::video::VideoMemoryId::parse(id)?;
                $crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                    .read_video_memory(id)
            }

            /// Write `bytes` into video memory `id` from byte `offset` on,
            /// for debugging, in the layout `readVideoMemory` returns, so
            /// that read gives them back: straight into memory, through no
            /// port or transfer; the timing, the CPUs and the chip's
            /// registers are untouched. The offset counts bytes on every
            /// memory of every console. It wraps at the memory's end, a
            /// buffer longer than the memory is cut to it, and an empty one
            /// writes nothing. A memory of 16-bit words (a palette, the
            /// vertical scroll) takes whole words: an offset inside a word
            /// starts at that word, an odd last byte is dropped, and each
            /// word keeps only the bits its port keeps. A running game shows
            /// the change on its next frame, a paused one after
            /// `repaintFrame`. `false` for a memory the chip lacks or a name
            /// no console has.
            #[wasm_bindgen(js_name = writeVideoMemory)]
            pub fn write_video_memory(&mut self, id: &str, offset: u32, bytes: &[u8]) -> bool {
                let ::core::option::Option::Some(id) =
                    $crate::__private::retro_core::video::VideoMemoryId::parse(id)
                else {
                    return false;
                };
                $crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                    .write_video_memory(id, offset, bytes)
            }

            /// The palette's colour words, a `Uint16Array` in colour order:
            /// the Mega Drive's 64 CRAM words (`0000 BBB0 GGG0 RRR0`), the
            /// SNES's 256 CGRAM words (BGR555).
            #[wasm_bindgen(js_name = getPaletteMemory)]
            pub fn get_palette_memory(&mut self) -> ::std::vec::Vec<u16> {
                $crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                    .palette_memory()
            }

            /// Write palette colours for debugging, to edit a palette:
            /// `colours` from colour `colour_index` on, in the format
            /// `getPaletteMemory` returns (the bits the console's port drops
            /// are dropped). Straight into memory, through no port or
            /// transfer. The index wraps at the palette's end, a buffer
            /// longer than the palette is cut to it, and an empty one writes
            /// nothing. A running game shows the change on its next frame, a
            /// paused one after `repaintFrame`.
            #[wasm_bindgen(js_name = writePaletteMemory)]
            pub fn write_palette_memory(&mut self, colour_index: u32, colours: &[u16]) {
                $crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                    .write_palette_memory(colour_index, colours);
            }

            /// Every palette colour as RGBA, a `Uint8Array` of 4 bytes a
            /// colour in colour order: the renderer's conversion at full
            /// brightness, alpha 255.
            #[wasm_bindgen(js_name = getPaletteColours)]
            pub fn get_palette_colours(&mut self) -> ::std::vec::Vec<u8> {
                $crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                    .palette_colours()
            }

            /// `decodeTiles` in colour: `{ width, height, data: Uint8Array }`
            /// of RGBA, pixel index `i` coloured with palette colour
            /// `firstColour + i` (`getPaletteColours`, wrapping at the
            /// palette's end), the transparent index at alpha 0; `null`
            /// where `decodeTiles` refuses.
            #[wasm_bindgen(js_name = getTileColours)]
            #[allow(clippy::too_many_arguments)]
            pub fn get_tile_colours(
                &mut self,
                source: u32,
                base_addr: u32,
                first_tile: u32,
                count: u32,
                bpp: u8,
                tile_size: u32,
                first_colour: u32,
            ) -> $crate::__private::wasm_bindgen::JsValue {
                let ::core::option::Option::Some(request) = $crate::video::tile_request(
                    source, base_addr, first_tile, count, bpp, tile_size,
                ) else {
                    return $crate::__private::wasm_bindgen::JsValue::NULL;
                };
                match $crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                    .tile_colours(&request, first_colour)
                {
                    ::core::result::Result::Ok(picture) => $crate::js::picture_object(&picture),
                    ::core::result::Result::Err(_) => {
                        $crate::__private::wasm_bindgen::JsValue::NULL
                    }
                }
            }

            /// The sprite table's entries the hardware takes, in the order
            /// it takes them, `SpriteTableEntry[]`: the position, size,
            /// tile, palette, priority and flips `getSpriteData` gives each
            /// sprite, and `next` (the slot the hardware takes after it on a
            /// table that links its entries, the Mega Drive's; `undefined`
            /// for the last one and on the SNES), `tile_address` (the VRAM
            /// byte of its first character), `large` (the larger of two
            /// sprite sizes) and `picture` (its box on the picture, as
            /// `getSpriteData` gives it).
            #[wasm_bindgen(js_name = getSpriteTable)]
            pub fn get_sprite_table(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js(
                    &$crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                        .sprite_table(),
                )
            }

            /// Where layer `layer_id` (`getLayerInfo`'s ids) reads its map
            /// and its characters, `LayerLayout`: `{ id, kind ("tile_map",
            /// "window", "affine_map", "sprites" or "off"), map_base (a VRAM
            /// byte), map_width, map_height (cells), character_base (a VRAM
            /// byte), bits_per_pixel, cell_width, cell_height, first_colour,
            /// scroll_x, scroll_y, line_scroll }`; `null` for a layer the
            /// chip lacks.
            #[wasm_bindgen(js_name = getLayerLayout)]
            pub fn get_layer_layout(
                &mut self,
                layer_id: u32,
            ) -> $crate::__private::wasm_bindgen::JsValue {
                match $crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                    .layer_layout(layer_id)
                {
                    ::core::option::Option::Some(layout) => $crate::js::to_js(&layout),
                    ::core::option::Option::None => $crate::__private::wasm_bindgen::JsValue::NULL,
                }
            }

            /// Layer `layer_id`'s whole map (`getLayerInfo`'s ids) as one
            /// picture, `{ width, height, data: Uint8Array }`, RGBA, drawn
            /// from video memory as it stands: every cell of the map in its
            /// place, its character in its palette with its flips, with no
            /// scroll and no window, a pixel of colour 0 transparent (0 in
            /// all four bytes); `null` for a layer that reads no map (the
            /// sprites) and for one the chip lacks. What a console's map is
            /// (its size, its cells, a map turned by a matrix) is documented
            /// by the core. A copy.
            #[wasm_bindgen(js_name = layerMap)]
            pub fn layer_map(&mut self, layer_id: u32) -> $crate::__private::wasm_bindgen::JsValue {
                match $crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                    .layer_map(layer_id)
                {
                    ::core::option::Option::Some(map) => $crate::js::picture_object(&map),
                    ::core::option::Option::None => $crate::__private::wasm_bindgen::JsValue::NULL,
                }
            }

            /// The levels the picture's colour channels take, `{ name,
            /// levels }`: `levels` the 8-bit value of each level, darkest
            /// first, and `name` the conversion they come from, the same
            /// while the levels are; an empty name and no levels on a chip
            /// that names none.
            #[wasm_bindgen(js_name = getColourLevels)]
            pub fn get_colour_levels(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js(
                    &$crate::__private::retro_core::EmulatorCore::video_debugger(&mut self.$core)
                        .colour_levels(),
                )
            }
        }
    };
}
