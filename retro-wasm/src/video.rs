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
/// `resolvedSpriteRenderOrder()` and `decodeTiles(...)`.
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
            /// id, name, size, scroll and whether it is shown), for a layer
            /// list.
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
            /// object picker that works the same on every console.
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
        }
    };
}
