//! The capture contract's values as a host reads them: each function turns
//! one of `retro_core::capture`'s shapes into the JavaScript value every
//! binding crate gives for it, a plain object of copies, never a `Map` and
//! never a view into the module's memory. [`capture_bindings!`] writes the
//! methods that return them.
//!
//! [`capture_bindings!`]: crate::capture_bindings

use js_sys::{Array, Object, Reflect};
use retro_core::capture::{
    CaptureRecording, FrameSnapshot, FrameWrites, LinePalettes, LineSprites, PixelSources,
    RenderedLayer, VideoMemorySources, WORDS_PER_RECORD,
};
use retro_core::video::{LineStateTable, PixelImage};
use wasm_bindgen::JsValue;

use crate::js::{
    int16_array, map_as_object, plain_object, set_property, uint16_array, uint32_array, uint8_array,
};

/// The property names of [`CaptureRecording`]'s fields, as a host writes and
/// reads them, in the order of the fields.
pub const RECORDING_KEYS: [&str; 5] = [
    "lineStates",
    "pixelSources",
    "lineSprites",
    "videoMemorySources",
    "frameWrites",
];

/// The fields of a [`CaptureRecording`] in the order of [`RECORDING_KEYS`].
fn recording_flags(recording: &mut CaptureRecording) -> [&mut bool; 5] {
    [
        &mut recording.line_states,
        &mut recording.pixel_sources,
        &mut recording.line_sprites,
        &mut recording.video_memory_sources,
        &mut recording.frame_writes,
    ]
}

/// `current` with each key of [`RECORDING_KEYS`] that `options` gives as a
/// boolean set to it: a key left out, or not a boolean, keeps its value, and
/// `options` that is not an object keeps them all.
pub fn recording_from_js(options: &JsValue, current: CaptureRecording) -> CaptureRecording {
    let mut recording = current;
    for (key, flag) in RECORDING_KEYS.iter().zip(recording_flags(&mut recording)) {
        let given = Reflect::get(options, &JsValue::from_str(key))
            .ok()
            .and_then(|value| value.as_bool());
        if let Some(on) = given {
            *flag = on;
        }
    }
    recording
}

/// `{ lineStates, pixelSources, lineSprites, videoMemorySources,
/// frameWrites }`, a plain object of booleans.
pub fn recording_to_js(recording: CaptureRecording) -> JsValue {
    let mut recording = recording;
    let entries: Vec<(&str, JsValue)> = RECORDING_KEYS
        .iter()
        .zip(recording_flags(&mut recording))
        .map(|(key, flag)| (*key, JsValue::from_bool(*flag)))
        .collect();
    plain_object(&entries)
}

/// `{ frame, atBoundary, width, height, framebuffer: Uint8Array, vram:
/// Uint8Array, palette: Uint16Array, ..., registers }`: the snapshot's
/// shared fields, in this order, with `console` writing the console's own
/// keys into the object after `palette` and before `registers`. `registers`
/// is an array of plain objects (`{ index, name, value, description }`).
pub fn frame_snapshot_object<C>(
    snapshot: &FrameSnapshot<C>,
    console: impl FnOnce(&C, &Object),
) -> JsValue {
    let obj = Object::new();
    set_property(&obj, "frame", &JsValue::from_f64(snapshot.frame as f64));
    set_property(
        &obj,
        "atBoundary",
        &JsValue::from_bool(snapshot.at_boundary),
    );
    set_property(&obj, "width", &JsValue::from_f64(f64::from(snapshot.width)));
    set_property(
        &obj,
        "height",
        &JsValue::from_f64(f64::from(snapshot.height)),
    );
    set_property(&obj, "framebuffer", &uint8_array(&snapshot.framebuffer));
    set_property(&obj, "vram", &uint8_array(&snapshot.vram));
    set_property(&obj, "palette", &uint16_array(&snapshot.palette));
    console(&snapshot.console, &obj);
    set_property(&obj, "registers", &map_as_object(&snapshot.registers));
    obj.into()
}

/// `{ width, height, data: Uint8Array }` for an RGBA picture, its bytes
/// copied.
pub fn picture_object(picture: &PixelImage) -> JsValue {
    plain_object(&[
        ("width", JsValue::from_f64(f64::from(picture.width))),
        ("height", JsValue::from_f64(f64::from(picture.height))),
        ("data", uint8_array(&picture.rgba)),
    ])
}

/// `{ width, height, data: Uint8Array, fromLineStates }`: the layer's
/// picture ([`picture_object`]) and whether it was drawn from the line table.
pub fn rendered_layer_object(layer: &RenderedLayer) -> JsValue {
    let obj = picture_object(&layer.image);
    set_property(
        &obj,
        "fromLineStates",
        &JsValue::from_bool(layer.from_line_states),
    );
    obj
}

/// `{ frame, fromFirstLine, complete, memoryExact, rowsPerLine,
/// firstRowLine, columns: string[], values: Uint32Array }`.
pub fn line_states_object(table: &LineStateTable) -> JsValue {
    let columns = Array::new();
    for name in &table.columns {
        columns.push(&JsValue::from_str(name));
    }
    plain_object(&[
        ("frame", JsValue::from_f64(table.frame as f64)),
        ("fromFirstLine", JsValue::from_bool(table.from_first_line)),
        ("complete", JsValue::from_bool(table.complete)),
        ("memoryExact", JsValue::from_bool(table.memory_exact)),
        (
            "rowsPerLine",
            JsValue::from_f64(f64::from(table.rows_per_line)),
        ),
        (
            "firstRowLine",
            JsValue::from_f64(f64::from(table.first_row_line)),
        ),
        ("columns", columns.into()),
        ("values", uint32_array(&table.values)),
    ])
}

/// `{ coloursPerPalette, palettes: Uint16Array, paletteOfLine: Uint16Array,
/// changes?: Uint16Array }`, `changes` only where the core records them.
pub fn line_palettes_object(palettes: &LinePalettes) -> JsValue {
    let mut entries = vec![
        (
            "coloursPerPalette",
            JsValue::from_f64(f64::from(palettes.colours_per_palette)),
        ),
        ("palettes", uint16_array(&palettes.palettes)),
        ("paletteOfLine", uint16_array(&palettes.palette_of_line)),
    ];
    if let Some(changes) = &palettes.changes {
        entries.push(("changes", uint16_array(changes)));
    }
    plain_object(&entries)
}

/// `{ frame, width, height, fromLineStates, matchesPicture, layer?:
/// Uint16Array, colour?: Uint16Array, tile?: Uint32Array, cell?:
/// Uint16Array, subLayer?: Uint8Array, subColour?: Uint16Array }`, a plane
/// left out where the sources hold none.
pub fn pixel_sources_object(sources: &PixelSources) -> JsValue {
    let mut entries = vec![
        ("frame", JsValue::from_f64(sources.frame as f64)),
        ("width", JsValue::from_f64(f64::from(sources.width))),
        ("height", JsValue::from_f64(f64::from(sources.height))),
        (
            "fromLineStates",
            JsValue::from_bool(sources.from_line_states),
        ),
        (
            "matchesPicture",
            JsValue::from_bool(sources.matches_picture),
        ),
    ];
    if let Some(layer) = &sources.layer {
        entries.push(("layer", uint16_array(layer)));
    }
    if let Some(colour) = &sources.colour {
        entries.push(("colour", uint16_array(colour)));
    }
    if let Some(tile) = &sources.tile {
        entries.push(("tile", uint32_array(tile)));
    }
    if let Some(cell) = &sources.cell {
        entries.push(("cell", uint16_array(cell)));
    }
    if let Some(sub_layer) = &sources.sub_layer {
        entries.push(("subLayer", uint8_array(sub_layer)));
    }
    if let Some(sub_colour) = &sources.sub_colour {
        entries.push(("subColour", uint16_array(sub_colour)));
    }
    plain_object(&entries)
}

/// `{ frame, fromLineStates, lines: Uint16Array, slots: Uint8Array, fates:
/// Uint8Array, x: Int16Array, width: Uint8Array, drawnWidth: Uint8Array }`.
pub fn line_sprites_object(sprites: &LineSprites) -> JsValue {
    plain_object(&[
        ("frame", JsValue::from_f64(sprites.frame as f64)),
        (
            "fromLineStates",
            JsValue::from_bool(sprites.from_line_states),
        ),
        ("lines", uint16_array(&sprites.lines)),
        ("slots", uint8_array(&sprites.slots)),
        ("fates", uint8_array(&sprites.fates)),
        ("x", int16_array(&sprites.x)),
        ("width", uint8_array(&sprites.width)),
        ("drawnWidth", uint8_array(&sprites.drawn_width)),
    ])
}

/// `{ origins: Uint32Array, frames: Uint32Array }`, one entry a byte of video
/// memory.
pub fn video_memory_sources_object(sources: &VideoMemorySources) -> JsValue {
    plain_object(&[
        ("origins", uint32_array(sources.origins())),
        ("frames", uint32_array(sources.frames())),
    ])
}

/// `{ records: Uint32Array, wordsPerRecord: 8, dropped }`.
pub fn frame_writes_object(writes: &FrameWrites) -> JsValue {
    plain_object(&[
        ("records", uint32_array(&writes.records)),
        ("wordsPerRecord", JsValue::from_f64(WORDS_PER_RECORD as f64)),
        ("dropped", JsValue::from_f64(f64::from(writes.dropped))),
    ])
}

/// Writes the capture methods every binding crate exports, as a second
/// `#[wasm_bindgen] impl` block on its emulator struct: each method calls the
/// [`CaptureCore`](retro_core::capture::CaptureCore) method of the same job
/// on one field of the struct and turns what it returns into the JavaScript
/// value the marshallers of [`capture`](crate::capture) make.
///
/// ```text
/// retro_wasm::capture_bindings!(
///     Emulator,                 // a #[wasm_bindgen] struct
///     core,                     // its field implementing CaptureCore
///     console = snapshot_console,  // optional: writes a snapshot's console keys
///     after_frame = refresh,       // optional: a method of Emulator
/// );
/// ```
///
/// - `console` is a function or closure taking
///   `(&<core as CaptureCore>::SnapshotConsole, &js_sys::Object)`, which
///   writes the keys of the console's part of a frame snapshot into the
///   object ([`frame_snapshot_object`](crate::capture::frame_snapshot_object)).
///   Left out, the macro does not write `captureFrameSnapshot`, for a crate
///   that writes its own.
/// - `after_frame` is a method of the struct taking `&mut self`, run after
///   each call that ran frames or drew the framebuffer again
///   (`runToCaptureBoundary`, `repaintFrame`), for a crate that keeps a copy
///   of the picture.
///
/// The methods, by their JavaScript names: `runToCaptureBoundary`,
/// `atCaptureBoundary`, `captureFrameSnapshot`, `renderLayer(layer_id)`,
/// `setCaptureRecording(options)`, `getCaptureRecording`, `getLineStates`,
/// `getLinePalettes`, `pictureFromLineStates`, `getPixelSources(planes)`,
/// `getLineSprites`, `renderSprites(selection)`, `getVideoMemorySources`,
/// `resolveSources(addresses)`, `takeFrameWrites` and `repaintFrame`. Their
/// Rust names are the JavaScript names in snake case
/// (`get_capture_recording`), which no other method of the struct may have.
/// The crate expanding the macro depends on `wasm-bindgen` itself, as every
/// crate declaring a `#[wasm_bindgen]` struct does.
#[macro_export]
macro_rules! capture_bindings {
    (
        $emulator:ident, $core:ident
        $(, console = $console:expr)?
        $(, after_frame = $after_frame:ident)?
        $(,)?
    ) => {
        #[$crate::__private::wasm_bindgen::prelude::wasm_bindgen]
        impl $emulator {
            /// Run to the next capture boundary and stop there: the frame's
            /// picture is drawn and its vertical blank has not begun, so the
            /// video memory, the palette and the registers still hold what
            /// the picture was drawn from (`captureFrameSnapshot`,
            /// `renderLayer`). Each call moves the game on one frame: a frame
            /// already held at its boundary is finished first. The frame loop
            /// is the one `runFrame` picks, and a breakpoint still stops where
            /// it stops (`atCaptureBoundary` is then false).
            ///
            /// The frame stays held until the next frame call (`runFrame`,
            /// `runFrameHeadless`, `runUntilVBlank`) runs the rest of it;
            /// `saveState` finishes it before it saves; a reset, `loadState`
            /// and `loadRom` drop it.
            #[wasm_bindgen(js_name = runToCaptureBoundary)]
            pub fn run_to_capture_boundary(&mut self) {
                $crate::__private::retro_core::capture::CaptureCore::run_to_capture_boundary(
                    &mut self.$core,
                );
                $( self.$after_frame(); )?
            }

            /// Whether the core is held at a capture boundary.
            #[wasm_bindgen(js_name = atCaptureBoundary)]
            pub fn at_capture_boundary(&self) -> bool {
                $crate::__private::retro_core::capture::CaptureCore::at_capture_boundary(
                    &self.$core,
                )
            }

            $(
                /// The frame's picture and the video memory, the palette and
                /// the registers it was drawn from, all copies, never views
                /// into this module's memory: `{ frame, atBoundary, width,
                /// height, framebuffer: Uint8Array, vram: Uint8Array, palette:
                /// Uint16Array, ..., registers }`, a plain object, with the
                /// console's own keys (documented by the core) before
                /// `registers`. `framebuffer` is the picture, RGBA, `width *
                /// height * 4` bytes, as `getDisplayInfo` sizes it; `vram` is
                /// video memory as `getVram` returns it; `palette` the palette
                /// memory's colour words; `registers` the video registers, an
                /// array of `{ index, name, value, description }`.
                ///
                /// At a capture boundary (`atBoundary`) the picture and the
                /// memory belong to the same frame; anywhere else the memory
                /// may already hold the game's uploads for the next frame.
                /// `frame` is the frames finished, plus one while the frame is
                /// held at its boundary, so a snapshot taken there and one
                /// taken after the same frame ran whole carry the same number.
                #[wasm_bindgen(js_name = captureFrameSnapshot)]
                pub fn capture_frame_snapshot(&self) -> $crate::__private::wasm_bindgen::JsValue {
                    let snapshot =
                        $crate::__private::retro_core::capture::CaptureCore::capture_frame_snapshot(
                            &self.$core,
                        );
                    $crate::capture::frame_snapshot_object(&snapshot, $console)
                }
            )?

            /// The held frame drawn again with one layer alone, by the ids
            /// `getLayerInfo` and `toggleLayer` use: `{ width, height, data:
            /// Uint8Array, fromLineStates }`, RGBA the size of the picture,
            /// each pixel the layer covers opaque in the layer's colour and
            /// every other pixel 0 in all four bytes (never the backdrop,
            /// never a colour key); `null` for an id that is no layer's.
            /// `fromLineStates` says the frame was drawn from its line table
            /// (`setCaptureRecording`), each line from the settings and the
            /// video memory it was drawn with, so a raster effect comes out as
            /// the frame drew it; otherwise from the state as it stands. How
            /// a layer is drawn on each console is documented by the core.
            /// Nothing runs: the framebuffer `getFramebuffer` gives, and
            /// everything a savestate holds, stay as they were.
            #[wasm_bindgen(js_name = renderLayer)]
            pub fn render_layer(&mut self, layer_id: u32) -> $crate::__private::wasm_bindgen::JsValue {
                match $crate::__private::retro_core::capture::CaptureCore::render_layer(
                    &mut self.$core,
                    layer_id,
                ) {
                    ::core::option::Option::Some(layer) => {
                        $crate::capture::rendered_layer_object(&layer)
                    }
                    ::core::option::Option::None => $crate::__private::wasm_bindgen::JsValue::NULL,
                }
            }

            /// Choose what the core records while frames run, for a capture
            /// to read: `options` is `{ lineStates?, pixelSources?,
            /// lineSprites?, videoMemorySources?, frameWrites? }`, booleans; a
            /// key left out, or not a boolean, keeps its value. A host
            /// setting: all off at first, never part of a savestate, kept
            /// across a reset, `loadRom` and `loadState`, and nothing the
            /// game does depends on it.
            ///
            /// `lineStates` keeps the line table (`getLineStates`,
            /// `getLinePalettes`, `pictureFromLineStates`), from which
            /// `repaintFrame`, `renderLayer`, `renderSprites`,
            /// `getPixelSources` and `getLineSprites` draw the frame again; a
            /// core that works the pixel sources and the line sprites out of
            /// that table keeps it for `pixelSources` and `lineSprites` too.
            /// `videoMemorySources` (`getVideoMemorySources`) and
            /// `frameWrites` (`takeFrameWrites`) are recorded as the writes
            /// land, each starting empty when turned on.
            #[wasm_bindgen(js_name = setCaptureRecording)]
            pub fn set_capture_recording(&mut self, options: $crate::__private::wasm_bindgen::JsValue) {
                let current =
                    $crate::__private::retro_core::capture::CaptureCore::capture_recording(
                        &self.$core,
                    );
                let wanted = $crate::capture::recording_from_js(&options, current);
                $crate::__private::retro_core::capture::CaptureCore::set_capture_recording(
                    &mut self.$core,
                    wanted,
                );
            }

            /// What the core records while frames run: `{ lineStates,
            /// pixelSources, lineSprites, videoMemorySources, frameWrites }`,
            /// a plain object of booleans.
            #[wasm_bindgen(js_name = getCaptureRecording)]
            pub fn get_capture_recording(&self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::capture::recording_to_js(
                    $crate::__private::retro_core::capture::CaptureCore::capture_recording(
                        &self.$core,
                    ),
                )
            }

            /// The settings each line of the frame was drawn with, while the
            /// line table is kept, else `null`: `{ frame, fromFirstLine,
            /// complete, memoryExact, rowsPerLine, firstRowLine, columns:
            /// string[], values: Uint32Array }`. `columns` names each value of
            /// a row, `line` first, then the settings in the core's own names
            /// (documented by the core); `values` holds the rows one after the
            /// other, `columns.length` to a row. `frame` is the number
            /// `captureFrameSnapshot` gives the same frame. `fromFirstLine`
            /// and `complete` say the table holds the frame from its first
            /// line to its finished picture; `memoryExact` that the video
            /// memory can be put back as each line read it; `rowsPerLine` (1,
            /// or 2 when lines are woven or doubled) and `firstRowLine` map
            /// the lines to the picture's rows. Copies.
            #[wasm_bindgen(js_name = getLineStates)]
            pub fn get_line_states(&self) -> $crate::__private::wasm_bindgen::JsValue {
                match $crate::__private::retro_core::capture::CaptureCore::line_states(&self.$core) {
                    ::core::option::Option::Some(table) => {
                        $crate::capture::line_states_object(&table)
                    }
                    ::core::option::Option::None => $crate::__private::wasm_bindgen::JsValue::NULL,
                }
            }

            /// The palettes the lines of the line table were drawn with, while
            /// the table is kept, else `null`: `{ coloursPerPalette, palettes:
            /// Uint16Array, paletteOfLine: Uint16Array, changes?: Uint16Array
            /// }`. `palettes` holds each palette a line was drawn with once,
            /// `coloursPerPalette` colours each, in the console's own colour
            /// words; `paletteOfLine` has one entry per row of
            /// `getLineStates`, an index into `palettes` (65535 for a line not
            /// recorded). `changes`, on a core that records the palette writes
            /// made while each line was on screen, lists them in the layout
            /// the core documents; it is absent on a core that does not.
            /// Copies.
            #[wasm_bindgen(js_name = getLinePalettes)]
            pub fn get_line_palettes(&self) -> $crate::__private::wasm_bindgen::JsValue {
                match $crate::__private::retro_core::capture::CaptureCore::line_palettes(&self.$core) {
                    ::core::option::Option::Some(palettes) => {
                        $crate::capture::line_palettes_object(&palettes)
                    }
                    ::core::option::Option::None => $crate::__private::wasm_bindgen::JsValue::NULL,
                }
            }

            /// The picture drawn again from the line table, `{ width, height,
            /// data: Uint8Array }`, RGBA, cut as `getFramebuffer`'s picture
            /// is, or `null`. When it answers, and how it draws a line the
            /// table does not hold, is the core's rule, documented by the
            /// core. Where the table holds the whole frame and its memory is
            /// exact (`getLineStates().complete` and `memoryExact`), it is the
            /// frame's own picture byte for byte. Nothing runs, and the
            /// framebuffer and everything a savestate holds stay as they were.
            #[wasm_bindgen(js_name = pictureFromLineStates)]
            pub fn picture_from_line_states(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                match $crate::__private::retro_core::capture::CaptureCore::picture_from_line_states(
                    &mut self.$core,
                ) {
                    ::core::option::Option::Some(picture) => {
                        $crate::capture::picture_object(&picture)
                    }
                    ::core::option::Option::None => $crate::__private::wasm_bindgen::JsValue::NULL,
                }
            }

            /// Where each pixel of the held frame came from: `null` when no
            /// game is loaded, else `{ frame, width, height, fromLineStates,
            /// matchesPicture, layer?: Uint16Array, colour?: Uint16Array,
            /// tile?: Uint32Array, cell?: Uint16Array, subLayer?: Uint8Array,
            /// subColour?: Uint16Array }`, one value a pixel in each plane,
            /// row after row, `width` x `height`, cut as `getFramebuffer`'s
            /// picture is. `planes` asks for planes, 15 when left out: 1
            /// `layer`, 2 `colour`, 4 `tile`, 8 `cell`, 16 `subLayer` and
            /// `subColour` (a second screen the core composites with); a plane
            /// not asked for, or one the core has not, is absent, and other
            /// bits are ignored. The frame is drawn again with a trace, from
            /// its line table when the table holds it (`fromLineStates`);
            /// `matchesPicture` says that picture is the one the host holds,
            /// byte for byte, so the planes describe it. `frame` is the number
            /// `captureFrameSnapshot` gives the picture.
            ///
            /// `layer`: bits 0-2 what drew the pixel: 0 the backdrop, a
            /// layer's id (as `getLayerInfo` numbers it) plus one, 6 a colour
            /// no layer drew, 7 nothing; bits 3-15 are the core's own.
            /// `tile`: bits 0-14 the video memory word where the character
            /// starts, bits 15-25 the character, bits 26-28 the row and bits
            /// 29-31 the column read in it, after flips; 0 where no character
            /// drew the pixel. `cell`: a map pixel's entry, as the index of
            /// its video memory word; a sprite pixel's slot in the sprite
            /// table; 65535 otherwise. `colour`, the second screen's planes
            /// and the core's own bits are documented by the core. Nothing
            /// runs, and the framebuffer and everything a savestate or the
            /// state hash reads stay as they were. Copies.
            #[wasm_bindgen(js_name = getPixelSources)]
            pub fn get_pixel_sources(
                &mut self,
                planes: ::core::option::Option<u32>,
            ) -> $crate::__private::wasm_bindgen::JsValue {
                let planes =
                    planes.unwrap_or($crate::__private::retro_core::capture::planes::MAIN);
                match $crate::__private::retro_core::capture::CaptureCore::pixel_sources(
                    &mut self.$core,
                    planes,
                ) {
                    ::core::option::Option::Some(sources) => {
                        $crate::capture::pixel_sources_object(&sources)
                    }
                    ::core::option::Option::None => $crate::__private::wasm_bindgen::JsValue::NULL,
                }
            }

            /// The sprites each line of the held frame met, in the order its
            /// evaluation met them, and what became of each: `null` when no
            /// game is loaded, else, in columns, `{ frame, fromLineStates,
            /// lines: Uint16Array, slots: Uint8Array, fates: Uint8Array, x:
            /// Int16Array, width: Uint8Array, drawnWidth: Uint8Array }`.
            ///
            /// `lines` holds four values a line, every line the frame drew in
            /// order: the line (as the core numbers its lines), the index of
            /// its first entry, its number of entries, and its flags (1 the
            /// line met more sprites than it keeps, 2 its time or dots ran
            /// out, 4 the sprite mask carried in from the line above, 8 the
            /// line showed no sprite). Per entry: `slots` the sprite's slot in
            /// the sprite table; `fates` 0 drawn whole, 1 drawn in part (the
            /// time or dots ran out inside it), 2 hidden by the sprite mask,
            /// 3 over the line's sprite limit, 4 left out after the time or
            /// dots ran out, 5 off screen; `x` its x on screen; `width` its
            /// width and `drawnWidth` the pixels of it the line drew. A core
            /// uses the fates and flags its hardware has, documented by the
            /// core. Each line is read from the line table when it holds the
            /// frame (`fromLineStates`). `frame` is the number
            /// `captureFrameSnapshot` gives the picture. Nothing runs, and
            /// nothing a savestate or the state hash reads moves. Copies.
            #[wasm_bindgen(js_name = getLineSprites)]
            pub fn get_line_sprites(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                match $crate::__private::retro_core::capture::CaptureCore::line_sprites(
                    &mut self.$core,
                ) {
                    ::core::option::Option::Some(sprites) => {
                        $crate::capture::line_sprites_object(&sprites)
                    }
                    ::core::option::Option::None => $crate::__private::wasm_bindgen::JsValue::NULL,
                }
            }

            /// The chosen sprites of the held frame drawn alone: `null` when
            /// no game is loaded, else `{ width, height, data: Uint8Array,
            /// fromLineStates }`, the shape of `renderLayer`. `selection` has
            /// one flag a slot of the sprite table, not 0 for a chosen
            /// sprite; a shorter array chooses nothing past its end. The
            /// sprites are drawn as the frame drew them, after each line's
            /// limits and after priority among sprites, so a chosen sprite
            /// shows only where it won its column; every other pixel is 0 in
            /// all four bytes. Nothing runs, and the framebuffer and
            /// everything a savestate holds stay as they were.
            #[wasm_bindgen(js_name = renderSprites)]
            pub fn render_sprites(&mut self, selection: &[u8]) -> $crate::__private::wasm_bindgen::JsValue {
                match $crate::__private::retro_core::capture::CaptureCore::render_sprites(
                    &mut self.$core,
                    selection,
                ) {
                    ::core::option::Option::Some(layer) => {
                        $crate::capture::rendered_layer_object(&layer)
                    }
                    ::core::option::Option::None => $crate::__private::wasm_bindgen::JsValue::NULL,
                }
            }

            /// Who wrote each byte of video memory, while `videoMemorySources`
            /// is recorded, else `null`: `{ origins: Uint32Array, frames:
            /// Uint32Array }`, one entry a byte, in the byte order of
            /// `getVram`. An origin holds in bits 28-30 what wrote the byte (0
            /// not written since the recorder was turned on or emptied, 1 a
            /// transfer from memory, 2 a transfer made line by line, 3 a store
            /// by the processor, 4 the debugger, 5 a fill, 6 a copy within
            /// video memory), in bits 24-26 the transfer channel where the
            /// writer has channels, and in bits 0-23 where the byte came from,
            /// which the core documents for each writer (`resolveSources`
            /// names the ROM file offset behind an address). `frames` holds
            /// the frame each byte was last written in, its low 32 bits,
            /// numbered as `captureFrameSnapshot` numbers its picture; 0 for a
            /// byte not written. Turned on, the recorder starts empty; a
            /// reset, `loadState` and `loadRom` empty it. Copies.
            #[wasm_bindgen(js_name = getVideoMemorySources)]
            pub fn get_video_memory_sources(&self) -> $crate::__private::wasm_bindgen::JsValue {
                match $crate::__private::retro_core::capture::CaptureCore::video_memory_sources(
                    &self.$core,
                ) {
                    ::core::option::Option::Some(sources) => {
                        $crate::capture::video_memory_sources_object(sources)
                    }
                    ::core::option::Option::None => $crate::__private::wasm_bindgen::JsValue::NULL,
                }
            }

            /// For each address in `addresses` (bits 0-23 of an origin; bits
            /// 24-31 are ignored), the offset into the ROM file a read of it
            /// reaches, through the cartridge's mapping as it stands now, or a
            /// negative code: -1 unknown, -2 work RAM, -3 cartridge RAM, -4 a
            /// coprocessor. What each code covers on a console is documented
            /// by the core. Reads the mapping and changes nothing. An
            /// `Int32Array` as long as `addresses`, a copy.
            #[wasm_bindgen(js_name = resolveSources)]
            pub fn resolve_sources(&self, addresses: &[u32]) -> $crate::__private::js_sys::Int32Array {
                let resolved = $crate::__private::retro_core::capture::CaptureCore::resolve_sources(
                    &self.$core,
                    addresses,
                );
                $crate::__private::js_sys::Int32Array::from(&resolved[..])
            }

            /// The writes recorded since the last take, the log then emptied,
            /// while `frameWrites` is recorded, else `null`: `{ records:
            /// Uint32Array, wordsPerRecord: 8, dropped }`. Taken at each
            /// capture boundary they are what built the frame held there: the
            /// uploads of the vertical blank before it and the writes made
            /// while its picture was drawn.
            ///
            /// A record is eight words: 0 bits 0-7 the kind (1 a transfer from
            /// memory, 2 one line's unit of a transfer made line by line, 3
            /// such a transfer's channel set up for the frame, 4 a palette
            /// colour stored, 5 a store into the sprite table, 6 a fill, 7 a
            /// copy), bits 8-10 the channel, bit 11 a transfer from the ports
            /// to memory, bits 12-14 the transfer unit, bit 15 an indirect
            /// channel, bits 16-18 the writer of a store (as in
            /// `getVideoMemorySources`), bit 20 a transfer none of whose bytes
            /// landed; 1 the frame, its low 32 bits; 2 bits 0-15 the line and
            /// bits 16-31 the position on it; 3 where the first byte came
            /// from; 4 bits 0-7 the memory or port written and bits 8-31 the
            /// byte count; 5 where the writes started; 6 where they ended, or
            /// the value written; 7 a word of the core's own. The kinds and
            /// fields a console uses, and their units, are documented by the
            /// core. `dropped` counts the records past the core's cap since
            /// the last take. Turned on, the log starts empty; a reset,
            /// `loadState` and `loadRom` empty it. `records` is a copy.
            #[wasm_bindgen(js_name = takeFrameWrites)]
            pub fn take_frame_writes(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                match $crate::__private::retro_core::capture::CaptureCore::take_frame_writes(
                    &mut self.$core,
                ) {
                    ::core::option::Option::Some(writes) => {
                        $crate::capture::frame_writes_object(&writes)
                    }
                    ::core::option::Option::None => $crate::__private::wasm_bindgen::JsValue::NULL,
                }
            }

            /// Draw the held frame again into the framebuffer, with the
            /// debugger's layer mask, without advancing the emulator: from
            /// the frame's line table where it holds the frame
            /// (`setCaptureRecording`), else from the state as it stands. The
            /// host calls this after a `toggleLayer` or an edit while paused,
            /// so the canvas shows the change at once (a running core repaints
            /// on its own next frame; a paused one never would). Nothing a
            /// savestate or the state hash reads moves.
            #[wasm_bindgen(js_name = repaintFrame)]
            pub fn repaint_frame(&mut self) {
                $crate::__private::retro_core::capture::CaptureCore::repaint_held_frame(
                    &mut self.$core,
                );
                $( self.$after_frame(); )?
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::shape;
    use retro_core::capture::{
        CaptureCore, FrameWrite, WriteKind, WriteLog, WriteOrigin, NO_PALETTE,
    };
    use retro_core::cartridge::SourcePlace;
    use retro_core::debugger::DebuggerCore;
    use retro_core::emulator::{EmulatorCore, StepResult};
    use retro_core::hash::fnv1a64;
    use retro_core::video::VideoRegister;
    use retro_core::{
        AudioInfo, DisplayInfo, Framebuffer, InputInfo, InputState, Instruction, RomInfo,
        SystemInfo,
    };
    use wasm_bindgen::prelude::wasm_bindgen;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_test::*;

    /// The names of `value`'s own properties, in the order they were set.
    fn keys(value: &JsValue) -> Vec<String> {
        Object::keys(value.unchecked_ref::<Object>())
            .iter()
            .filter_map(|key| key.as_string())
            .collect()
    }

    /// The description `shape` gives a typed array holding `bytes`.
    fn typed(constructor: &str, length: usize, bytes: &[u8]) -> String {
        format!(
            "{constructor}(len {length}, fnv1a64 {:#018x})",
            fnv1a64(bytes)
        )
    }

    fn le_bytes_16(values: &[u16]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    fn le_bytes_32(values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    #[wasm_bindgen_test]
    fn a_recording_reads_back_as_five_booleans_in_order() {
        let recording = CaptureRecording {
            line_states: true,
            frame_writes: true,
            ..CaptureRecording::default()
        };
        let value = recording_to_js(recording);
        assert_eq!(keys(&value), RECORDING_KEYS);
        assert_eq!(
            shape(&value),
            "{frameWrites: true, lineSprites: false, lineStates: true, pixelSources: false, \
             videoMemorySources: false}"
        );
    }

    #[wasm_bindgen_test]
    fn a_key_left_out_or_not_a_boolean_keeps_its_value() {
        let current = CaptureRecording {
            line_states: true,
            pixel_sources: true,
            ..CaptureRecording::default()
        };
        let options = plain_object(&[
            ("pixelSources", JsValue::FALSE),
            ("frameWrites", JsValue::TRUE),
            ("lineSprites", JsValue::from_f64(1.0)),
        ]);
        assert_eq!(
            recording_from_js(&options, current),
            CaptureRecording {
                line_states: true,
                frame_writes: true,
                ..CaptureRecording::default()
            }
        );
        for nothing in [JsValue::UNDEFINED, JsValue::NULL, JsValue::from_f64(3.0)] {
            assert_eq!(recording_from_js(&nothing, current), current);
        }
        let every = plain_object(
            &RECORDING_KEYS
                .iter()
                .map(|key| (*key, JsValue::TRUE))
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            recording_from_js(&every, CaptureRecording::default()),
            CaptureRecording {
                line_states: true,
                pixel_sources: true,
                line_sprites: true,
                video_memory_sources: true,
                frame_writes: true,
            }
        );
    }

    fn picture() -> PixelImage {
        PixelImage {
            width: 2,
            height: 1,
            rgba: vec![1, 2, 3, 255, 4, 5, 6, 0],
        }
    }

    #[wasm_bindgen_test]
    fn a_picture_is_its_size_and_a_copy_of_its_bytes() {
        let value = picture_object(&picture());
        assert_eq!(keys(&value), ["width", "height", "data"]);
        assert_eq!(
            shape(&value),
            format!(
                "{{data: {}, height: 1, width: 2}}",
                typed("Uint8Array", 8, &picture().rgba)
            )
        );
    }

    #[wasm_bindgen_test]
    fn a_rendered_layer_is_its_picture_and_where_it_was_drawn_from() {
        let value = rendered_layer_object(&RenderedLayer {
            image: picture(),
            from_line_states: true,
        });
        assert_eq!(keys(&value), ["width", "height", "data", "fromLineStates"]);
        assert_eq!(
            shape(&value),
            format!(
                "{{data: {}, fromLineStates: true, height: 1, width: 2}}",
                typed("Uint8Array", 8, &picture().rgba)
            )
        );
    }

    /// The console part of a test snapshot.
    struct Console {
        vsram: Vec<u16>,
    }

    fn snapshot() -> FrameSnapshot<Console> {
        FrameSnapshot {
            frame: 12,
            at_boundary: true,
            width: 2,
            height: 1,
            framebuffer: picture().rgba,
            vram: vec![0xAA, 0xBB],
            palette: vec![0x0EEE, 0x0002],
            registers: vec![VideoRegister {
                index: 1,
                name: "Mode".to_string(),
                value: 0x44,
                description: "display on".to_string(),
            }],
            console: Console { vsram: vec![7, 8] },
        }
    }

    /// Writes a test snapshot's console keys: one key of its own.
    fn console_keys(console: &Console, obj: &Object) {
        let _ = Reflect::set(
            obj,
            &JsValue::from_str("vsram"),
            &uint16_array(&console.vsram),
        );
    }

    #[wasm_bindgen_test]
    fn a_snapshot_writes_the_console_keys_before_the_registers() {
        let value = frame_snapshot_object(&snapshot(), console_keys);
        assert_eq!(
            keys(&value),
            [
                "frame",
                "atBoundary",
                "width",
                "height",
                "framebuffer",
                "vram",
                "palette",
                "vsram",
                "registers"
            ]
        );
        assert_eq!(
            shape(&value),
            format!(
                "{{atBoundary: true, frame: 12, framebuffer: {}, height: 1, palette: {}, \
                 registers: [{{description: \"display on\", index: 1, name: \"Mode\", \
                 value: 68}}], vram: {}, vsram: {}, width: 2}}",
                typed("Uint8Array", 8, &picture().rgba),
                typed("Uint16Array", 2, &le_bytes_16(&[0x0EEE, 0x0002])),
                typed("Uint8Array", 2, &[0xAA, 0xBB]),
                typed("Uint16Array", 2, &le_bytes_16(&[7, 8])),
            )
        );
    }

    fn table() -> LineStateTable {
        LineStateTable {
            frame: 3,
            from_first_line: true,
            complete: false,
            memory_exact: true,
            rows_per_line: 2,
            first_row_line: 1,
            columns: vec!["line".to_string(), "bg_hoffset[0]".to_string()],
            values: vec![1, 5, 2, 6],
        }
    }

    #[wasm_bindgen_test]
    fn a_line_table_is_its_columns_and_values() {
        let value = line_states_object(&table());
        assert_eq!(
            keys(&value),
            [
                "frame",
                "fromFirstLine",
                "complete",
                "memoryExact",
                "rowsPerLine",
                "firstRowLine",
                "columns",
                "values"
            ]
        );
        assert_eq!(
            shape(&value),
            format!(
                "{{columns: [\"line\", \"bg_hoffset[0]\"], complete: false, firstRowLine: 1, \
                 frame: 3, fromFirstLine: true, memoryExact: true, rowsPerLine: 2, values: {}}}",
                typed("Uint32Array", 4, &le_bytes_32(&[1, 5, 2, 6]))
            )
        );
    }

    #[wasm_bindgen_test]
    fn the_palette_writes_are_there_only_where_the_core_records_them() {
        let mut palettes = LinePalettes {
            colours_per_palette: 2,
            palettes: vec![1, 2],
            palette_of_line: vec![0, NO_PALETTE],
            changes: None,
        };
        let without = line_palettes_object(&palettes);
        assert_eq!(
            keys(&without),
            ["coloursPerPalette", "palettes", "paletteOfLine"]
        );
        assert_eq!(
            shape(&without),
            format!(
                "{{coloursPerPalette: 2, paletteOfLine: {}, palettes: {}}}",
                typed("Uint16Array", 2, &le_bytes_16(&[0, NO_PALETTE])),
                typed("Uint16Array", 2, &le_bytes_16(&[1, 2])),
            )
        );
        palettes.changes = Some(vec![0, 3, 5, 0x0EEE, 0]);
        let with = line_palettes_object(&palettes);
        assert_eq!(
            keys(&with),
            ["coloursPerPalette", "palettes", "paletteOfLine", "changes"]
        );
        assert!(shape(&with).starts_with(&format!(
            "{{changes: {}, ",
            typed("Uint16Array", 5, &le_bytes_16(&[0, 3, 5, 0x0EEE, 0]))
        )));
    }

    fn sources(with_planes: bool, with_sub_screen: bool) -> PixelSources {
        PixelSources {
            frame: 9,
            width: 2,
            height: 1,
            from_line_states: false,
            matches_picture: true,
            layer: with_planes.then(|| vec![1, 7]),
            colour: with_planes.then(|| vec![3, 4]),
            tile: with_planes.then(|| vec![0x10, 0]),
            cell: with_planes.then(|| vec![0x20, 0xFFFF]),
            sub_layer: with_sub_screen.then(|| vec![7, 0]),
            sub_colour: with_sub_screen.then(|| vec![0, 0x7FFF]),
        }
    }

    #[wasm_bindgen_test]
    fn the_pixel_sources_leave_out_the_planes_they_hold_not() {
        let none = pixel_sources_object(&sources(false, false));
        assert_eq!(
            keys(&none),
            [
                "frame",
                "width",
                "height",
                "fromLineStates",
                "matchesPicture"
            ]
        );
        assert_eq!(
            shape(&none),
            "{frame: 9, fromLineStates: false, height: 1, matchesPicture: true, width: 2}"
        );

        let main = pixel_sources_object(&sources(true, false));
        assert_eq!(
            keys(&main),
            [
                "frame",
                "width",
                "height",
                "fromLineStates",
                "matchesPicture",
                "layer",
                "colour",
                "tile",
                "cell"
            ]
        );
        assert_eq!(
            shape(&main),
            format!(
                "{{cell: {}, colour: {}, frame: 9, fromLineStates: false, height: 1, layer: {}, \
                 matchesPicture: true, tile: {}, width: 2}}",
                typed("Uint16Array", 2, &le_bytes_16(&[0x20, 0xFFFF])),
                typed("Uint16Array", 2, &le_bytes_16(&[3, 4])),
                typed("Uint16Array", 2, &le_bytes_16(&[1, 7])),
                typed("Uint32Array", 2, &le_bytes_32(&[0x10, 0])),
            )
        );

        let every = pixel_sources_object(&sources(true, true));
        assert_eq!(&keys(&every)[9..], ["subLayer", "subColour"]);
        let described = shape(&every);
        let sub_colour = typed("Uint16Array", 2, &le_bytes_16(&[0, 0x7FFF]));
        let sub_layer = typed("Uint8Array", 2, &[7, 0]);
        assert!(described.contains(&format!("subColour: {sub_colour}")));
        assert!(described.contains(&format!("subLayer: {sub_layer}")));
    }

    #[wasm_bindgen_test]
    fn the_line_sprites_are_columns_of_their_types() {
        let sprites = LineSprites {
            frame: 4,
            from_line_states: true,
            lines: vec![1, 0, 2, 3],
            slots: vec![5, 6],
            fates: vec![0, 5],
            x: vec![-8, 300],
            width: vec![16, 8],
            drawn_width: vec![8, 0],
        };
        let value = line_sprites_object(&sprites);
        assert_eq!(
            keys(&value),
            [
                "frame",
                "fromLineStates",
                "lines",
                "slots",
                "fates",
                "x",
                "width",
                "drawnWidth"
            ]
        );
        let x_bytes: Vec<u8> = [-8i16, 300].iter().flat_map(|x| x.to_le_bytes()).collect();
        assert_eq!(
            shape(&value),
            format!(
                "{{drawnWidth: {}, fates: {}, frame: 4, fromLineStates: true, lines: {}, \
                 slots: {}, width: {}, x: {}}}",
                typed("Uint8Array", 2, &[8, 0]),
                typed("Uint8Array", 2, &[0, 5]),
                typed("Uint16Array", 4, &le_bytes_16(&[1, 0, 2, 3])),
                typed("Uint8Array", 2, &[5, 6]),
                typed("Uint8Array", 2, &[16, 8]),
                typed("Int16Array", 2, &x_bytes),
            )
        );
    }

    #[wasm_bindgen_test]
    fn the_video_memory_sources_are_one_entry_a_byte() {
        let mut sources = VideoMemorySources::new(4);
        sources.note(1, WriteOrigin::transfer(2, 0x12_3456), 7);
        let value = video_memory_sources_object(&sources);
        assert_eq!(keys(&value), ["origins", "frames"]);
        assert_eq!(
            shape(&value),
            format!(
                "{{frames: {}, origins: {}}}",
                typed("Uint32Array", 4, &le_bytes_32(&[0, 7, 0, 0])),
                typed("Uint32Array", 4, &le_bytes_32(&[0, 0x1212_3456, 0, 0])),
            )
        );
    }

    #[wasm_bindgen_test]
    fn the_frame_writes_are_their_records_and_the_dropped_count() {
        let mut log = WriteLog::new(1);
        let record = FrameWrite {
            kind: WriteKind::Fill as u8,
            count: 2,
            ..FrameWrite::default()
        }
        .pack();
        log.push(&record);
        log.push(&record);
        let value = frame_writes_object(&log.take());
        assert_eq!(keys(&value), ["records", "wordsPerRecord", "dropped"]);
        assert_eq!(
            shape(&value),
            format!(
                "{{dropped: 1, records: {}, wordsPerRecord: 8}}",
                typed("Uint32Array", 8, &le_bytes_32(&record))
            )
        );
    }

    #[wasm_bindgen_test]
    fn the_typed_arrays_are_copies_of_their_type() {
        assert_eq!(
            shape(&uint8_array(&[1, 2])),
            typed("Uint8Array", 2, &[1, 2])
        );
        assert_eq!(
            shape(&uint16_array(&[1, 2])),
            typed("Uint16Array", 2, &le_bytes_16(&[1, 2]))
        );
        assert_eq!(
            shape(&uint32_array(&[1, 2])),
            typed("Uint32Array", 2, &le_bytes_32(&[1, 2]))
        );
        let signed: Vec<u8> = [-1i16, 2].iter().flat_map(|x| x.to_le_bytes()).collect();
        assert_eq!(
            shape(&int16_array(&[-1, 2])),
            typed("Int16Array", 2, &signed)
        );
        assert_eq!(shape(&uint8_array(&[])), typed("Uint8Array", 0, &[]));
    }

    #[wasm_bindgen_test]
    fn an_object_is_plain_and_keeps_its_order() {
        let value = plain_object(&[("b", JsValue::TRUE), ("a", JsValue::NULL)]);
        assert_eq!(keys(&value), ["b", "a"]);
        assert_eq!(shape(&value), "{a: null, b: true}");

        let mut map = std::collections::HashMap::new();
        map.insert("only".to_string(), 1u32);
        assert_eq!(shape(&map_as_object(&map)), "{only: 1}");
        let refused = std::collections::HashMap::from([((1u32, 2u32), 3u32)]);
        assert_eq!(shape(&map_as_object(&refused)), "undefined");
    }

    // ===== The bindings the macro writes, on a core that answers each call
    // with a value of its own =====

    /// A core whose capture methods answer with fixed values and count the
    /// calls that run or draw frames.
    struct FakeCore {
        recording: CaptureRecording,
        loaded: bool,
        runs: u32,
        repaints: u32,
        sources: VideoMemorySources,
        writes: WriteLog,
    }

    impl FakeCore {
        fn new() -> Self {
            let mut writes = WriteLog::new(4);
            writes.push(
                &FrameWrite {
                    kind: WriteKind::Transfer as u8,
                    count: 32,
                    ..FrameWrite::default()
                }
                .pack(),
            );
            let mut sources = VideoMemorySources::new(2);
            sources.note(0, WriteOrigin::DEBUGGER, 3);
            Self {
                recording: CaptureRecording::default(),
                loaded: true,
                runs: 0,
                repaints: 0,
                sources,
                writes,
            }
        }
    }

    impl EmulatorCore for FakeCore {
        fn system_info(&self) -> &SystemInfo {
            unreachable!("the bindings call only the capture methods")
        }
        fn reset(&mut self) {
            unreachable!()
        }
        fn step_cpu(&mut self, _cpu_id: usize) -> StepResult {
            unreachable!()
        }
        fn focused_cpu(&self) -> usize {
            unreachable!()
        }
        fn set_focused_cpu(&mut self, _cpu_id: usize) {
            unreachable!()
        }
        fn run_frame(&mut self) {
            unreachable!()
        }
        fn run_until_vblank(&mut self) {
            unreachable!()
        }
        fn load_rom(&mut self, _data: &[u8]) -> Result<(), String> {
            unreachable!()
        }
        fn get_rom_info(&self) -> Option<RomInfo> {
            unreachable!()
        }
        fn get_framebuffer(&self) -> &Framebuffer {
            unreachable!()
        }
        fn get_display_info(&self) -> &DisplayInfo {
            unreachable!()
        }
        fn get_audio_samples(&mut self) -> &[f32] {
            unreachable!()
        }
        fn get_audio_info(&self) -> &AudioInfo {
            unreachable!()
        }
        fn set_audio_enabled(&mut self, _enabled: bool) {
            unreachable!()
        }
        fn is_audio_enabled(&self) -> bool {
            unreachable!()
        }
        fn set_input(&mut self, _player: u8, _input: InputState) {
            unreachable!()
        }
        fn get_input_info(&self) -> &InputInfo {
            unreachable!()
        }
        fn save_state(&self) -> Vec<u8> {
            unreachable!()
        }
        fn load_state(&mut self, _data: &[u8]) -> Result<(), String> {
            unreachable!()
        }
        fn get_save_ram(&self) -> Option<&[u8]> {
            unreachable!()
        }
        fn set_save_ram(&mut self, _data: &[u8]) {
            unreachable!()
        }
        fn debugger(&mut self) -> &mut dyn DebuggerCore {
            unreachable!()
        }
        fn debugger_ref(&self) -> &dyn DebuggerCore {
            unreachable!()
        }
        fn refresh_debugger_state(&mut self) {
            unreachable!()
        }
        fn read_memory_debug(&mut self, _addr: u32, _size: u32) -> Vec<u8> {
            unreachable!()
        }
        fn disassemble_at(&mut self, _addr: u32, _count: u32) -> Vec<Instruction> {
            unreachable!()
        }
        fn disassemble_cpu(&mut self, _cpu_id: usize, _addr: u32, _count: u32) -> Vec<Instruction> {
            unreachable!()
        }
    }

    impl CaptureCore for FakeCore {
        type SnapshotConsole = Console;

        fn set_capture_recording(&mut self, wanted: CaptureRecording) {
            self.recording = wanted;
        }
        fn capture_recording(&self) -> CaptureRecording {
            self.recording
        }
        fn run_to_capture_boundary(&mut self) {
            self.runs += 1;
        }
        fn at_capture_boundary(&self) -> bool {
            self.runs > 0
        }
        fn save_state_at_frame_end(&mut self) -> Vec<u8> {
            Vec::new()
        }
        fn capture_frame_snapshot(&self) -> FrameSnapshot<Console> {
            snapshot()
        }
        fn repaint_held_frame(&mut self) -> bool {
            self.repaints += 1;
            true
        }
        fn render_layer(&mut self, layer: u32) -> Option<RenderedLayer> {
            (layer < 2).then(|| RenderedLayer {
                image: picture(),
                from_line_states: layer == 1,
            })
        }
        fn picture_from_line_states(&mut self) -> Option<PixelImage> {
            self.recording.line_states.then(picture)
        }
        fn line_states(&self) -> Option<LineStateTable> {
            self.recording.line_states.then(table)
        }
        fn line_palettes(&self) -> Option<LinePalettes> {
            self.recording.line_states.then(|| LinePalettes {
                colours_per_palette: 1,
                palettes: vec![5],
                palette_of_line: vec![0],
                changes: None,
            })
        }
        fn pixel_sources(&mut self, planes: u32) -> Option<PixelSources> {
            self.loaded.then(|| {
                let mut found = sources(true, true);
                if planes & retro_core::capture::planes::LAYER == 0 {
                    found.layer = None;
                }
                if planes & retro_core::capture::planes::SUB_SCREEN == 0 {
                    found.sub_layer = None;
                    found.sub_colour = None;
                }
                found
            })
        }
        fn line_sprites(&mut self) -> Option<LineSprites> {
            self.loaded.then(LineSprites::default)
        }
        fn render_sprites(&mut self, selection: &[u8]) -> Option<RenderedLayer> {
            self.loaded.then(|| RenderedLayer {
                image: PixelImage {
                    width: selection.len() as u32,
                    height: 1,
                    rgba: selection.iter().flat_map(|&flag| [flag; 4]).collect(),
                },
                from_line_states: false,
            })
        }
        fn video_memory_sources(&self) -> Option<&VideoMemorySources> {
            self.recording.video_memory_sources.then_some(&self.sources)
        }
        fn take_frame_writes(&mut self) -> Option<FrameWrites> {
            self.recording.frame_writes.then(|| self.writes.take())
        }
        fn resolve_sources(&self, addresses: &[u32]) -> Vec<i32> {
            addresses
                .iter()
                .map(|&address| match address {
                    0..=0xFF => SourcePlace::Rom(address).code(),
                    _ => SourcePlace::WorkRam.code(),
                })
                .collect()
        }
    }

    /// An emulator struct as a binding crate declares one, with a copy of
    /// the picture it refreshes after a frame.
    #[wasm_bindgen]
    pub struct TestEmulator {
        core: FakeCore,
        refreshes: u32,
    }

    impl TestEmulator {
        fn refresh(&mut self) {
            self.refreshes += 1;
        }
    }

    crate::capture_bindings!(
        TestEmulator,
        core,
        console = console_keys,
        after_frame = refresh
    );

    /// An emulator struct that writes its own snapshot and keeps no copy of
    /// the picture.
    #[wasm_bindgen]
    pub struct BareEmulator {
        core: FakeCore,
    }

    crate::capture_bindings!(BareEmulator, core);

    fn emulator() -> TestEmulator {
        TestEmulator {
            core: FakeCore::new(),
            refreshes: 0,
        }
    }

    #[wasm_bindgen_test]
    fn the_frame_calls_run_the_core_and_then_the_after_frame_method() {
        let mut emulator = emulator();
        assert!(!emulator.at_capture_boundary());
        emulator.run_to_capture_boundary();
        assert!(emulator.at_capture_boundary());
        assert_eq!((emulator.core.runs, emulator.refreshes), (1, 1));
        emulator.repaint_frame();
        assert_eq!((emulator.core.repaints, emulator.refreshes), (1, 2));
        // The readers draw nothing into the host's picture.
        let _ = emulator.render_layer(0);
        let _ = emulator.render_sprites(&[1]);
        let _ = emulator.picture_from_line_states();
        let _ = emulator.get_pixel_sources(None);
        assert_eq!(emulator.refreshes, 2);

        let mut bare = BareEmulator {
            core: FakeCore::new(),
        };
        bare.run_to_capture_boundary();
        bare.repaint_frame();
        assert_eq!((bare.core.runs, bare.core.repaints), (1, 1));
    }

    #[wasm_bindgen_test]
    fn the_recording_goes_through_the_core() {
        let mut emulator = emulator();
        emulator.set_capture_recording(plain_object(&[
            ("lineStates", JsValue::TRUE),
            ("videoMemorySources", JsValue::TRUE),
        ]));
        emulator.set_capture_recording(plain_object(&[("frameWrites", JsValue::TRUE)]));
        assert_eq!(
            emulator.core.recording,
            CaptureRecording {
                line_states: true,
                video_memory_sources: true,
                frame_writes: true,
                ..CaptureRecording::default()
            },
            "a key left out keeps its value"
        );
        assert_eq!(
            shape(&emulator.get_capture_recording()),
            shape(&recording_to_js(emulator.core.recording))
        );
    }

    #[wasm_bindgen_test]
    fn each_reader_gives_its_marshalled_value_or_null() {
        let mut emulator = emulator();
        assert!(emulator.get_line_states().is_null());
        assert!(emulator.get_line_palettes().is_null());
        assert!(emulator.picture_from_line_states().is_null());
        assert!(emulator.get_video_memory_sources().is_null());
        assert!(emulator.take_frame_writes().is_null());
        assert!(emulator.render_layer(2).is_null(), "no such layer");

        emulator.core.recording = CaptureRecording {
            line_states: true,
            video_memory_sources: true,
            frame_writes: true,
            ..CaptureRecording::default()
        };
        assert_eq!(
            shape(&emulator.capture_frame_snapshot()),
            shape(&frame_snapshot_object(&snapshot(), console_keys))
        );
        assert_eq!(
            shape(&emulator.get_line_states()),
            shape(&line_states_object(&table()))
        );
        assert_eq!(
            shape(&emulator.get_line_palettes()),
            format!(
                "{{coloursPerPalette: 1, paletteOfLine: {}, palettes: {}}}",
                typed("Uint16Array", 1, &[0, 0]),
                typed("Uint16Array", 1, &[5, 0]),
            )
        );
        assert_eq!(
            shape(&emulator.picture_from_line_states()),
            shape(&picture_object(&picture()))
        );
        assert_eq!(
            shape(&emulator.render_layer(1)),
            shape(&rendered_layer_object(&RenderedLayer {
                image: picture(),
                from_line_states: true,
            }))
        );
        assert_eq!(
            shape(&emulator.get_video_memory_sources()),
            shape(&video_memory_sources_object(&emulator.core.sources))
        );
        let writes = emulator.take_frame_writes();
        assert_eq!(keys(&writes), ["records", "wordsPerRecord", "dropped"]);
        assert!(shape(&writes).contains("Uint32Array(len 8, "));
        assert!(
            shape(&emulator.take_frame_writes()).contains("Uint32Array(len 0, "),
            "a take empties the log"
        );
        assert_eq!(
            shape(&emulator.get_line_sprites()),
            shape(&line_sprites_object(&LineSprites::default()))
        );

        emulator.core.loaded = false;
        assert!(emulator.get_pixel_sources(None).is_null());
        assert!(emulator.get_line_sprites().is_null());
        assert!(emulator.render_sprites(&[1, 0]).is_null());
    }

    #[wasm_bindgen_test]
    fn the_planes_are_the_main_four_when_left_out() {
        let mut emulator = emulator();
        assert_eq!(
            keys(&emulator.get_pixel_sources(None))[5..],
            ["layer", "colour", "tile", "cell"]
        );
        assert_eq!(
            keys(&emulator.get_pixel_sources(Some(2)))[5..],
            ["colour", "tile", "cell"]
        );
        assert_eq!(
            keys(&emulator.get_pixel_sources(Some(31)))[5..],
            ["layer", "colour", "tile", "cell", "subLayer", "subColour"]
        );
    }

    #[wasm_bindgen_test]
    fn the_selection_and_the_addresses_reach_the_core() {
        let mut emulator = emulator();
        let chosen = emulator.render_sprites(&[1, 0, 2]);
        assert_eq!(
            shape(&chosen),
            format!(
                "{{data: {}, fromLineStates: false, height: 1, width: 3}}",
                typed("Uint8Array", 12, &[1, 1, 1, 1, 0, 0, 0, 0, 2, 2, 2, 2])
            )
        );
        let resolved = emulator.resolve_sources(&[0x10, 0x7E_0000]);
        assert_eq!(resolved.to_vec(), [0x10, -2]);
        assert_eq!(
            shape(&resolved.into()),
            typed("Int32Array", 2, &[0x10, 0, 0, 0, 0xFE, 0xFF, 0xFF, 0xFF])
        );
    }
}
