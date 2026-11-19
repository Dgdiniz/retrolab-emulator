use super::*;

/// A ROM whose 68000 loops at $400 with no interrupt on.
fn looping_rom() -> Vec<u8> {
    let mut rom = synth_genesis_rom("LOOPING");
    rom[0x400..0x402].copy_from_slice(&[0x60, 0xFE]); // BRA *
    rom
}

#[wasm_bindgen_test]
fn a_frame_held_at_its_capture_boundary_is_read_drawn_and_saved() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.load_rom(&looping_rom()).unwrap();
    emu.set_execution_mode("Running".to_string());
    emu.run_frame();
    emu.run_to_capture_boundary();
    assert!(emu.at_capture_boundary());
    let frame = emu.get_frame_number();

    let snapshot = emu.capture_frame_snapshot();
    assert_plain_object(
        &snapshot,
        &[
            "frame",
            "atBoundary",
            "width",
            "height",
            "framebuffer",
            "vram",
            "palette",
            "vsram",
            "registers",
        ],
        "captureFrameSnapshot",
    );
    let get = |key: &str| js_sys::Reflect::get(&snapshot, &JsValue::from_str(key)).unwrap();
    // The held frame is counted once it ends: the picture is the next one.
    assert_eq!(get("frame").as_f64(), Some((frame + 1) as f64));
    assert_eq!(get("atBoundary").as_bool(), Some(true));
    assert_eq!(get("width").as_f64(), Some(320.0));
    assert_eq!(get("height").as_f64(), Some(224.0));
    for (key, length) in [("framebuffer", 320 * 224 * 4), ("vram", 0x10000)] {
        let bytes: js_sys::Uint8Array = get(key).dyn_into().expect(key);
        assert_eq!(bytes.length(), length, "{key}");
    }
    for key in ["palette", "vsram"] {
        let words: js_sys::Uint16Array = get(key).dyn_into().expect(key);
        assert_eq!(words.length(), 64, "{key}");
    }
    assert_eq!(js_sys::Array::from(&get("registers")).length(), 24);
    // Copies, not views into this module's memory.
    let vram: js_sys::Uint8Array = get("vram").dyn_into().unwrap();
    let memory: js_sys::WebAssembly::Memory = wasm_bindgen::memory().dyn_into().unwrap();
    assert!(!js_sys::Object::is(&vram.buffer(), &memory.buffer()));

    let layer = emu.render_layer(0);
    let data: js_sys::Uint8Array = js_sys::Reflect::get(&layer, &JsValue::from_str("data"))
        .unwrap()
        .dyn_into()
        .unwrap();
    assert_eq!(data.length(), 320 * 224 * 4);
    assert!(emu.render_layer(7).is_null());
    assert!(emu.at_capture_boundary(), "drawing a layer runs nothing");

    // A save finishes the held frame first.
    assert!(!emu.save_state().is_empty());
    assert!(!emu.at_capture_boundary());
    assert_eq!(emu.get_frame_number(), frame + 1);
}

/// `{ key: value, ... }` as the options a host passes.
fn options(pairs: &[(&str, bool)]) -> JsValue {
    let obj = js_sys::Object::new();
    for &(key, on) in pairs {
        js_sys::Reflect::set(&obj, &JsValue::from_str(key), &JsValue::from_bool(on)).unwrap();
    }
    obj.into()
}

/// Whether `array` is a view into this module's memory.
fn views_module_memory(buffer: &js_sys::ArrayBuffer) -> bool {
    let memory: js_sys::WebAssembly::Memory = wasm_bindgen::memory().dyn_into().unwrap();
    js_sys::Object::is(buffer, &memory.buffer())
}

#[wasm_bindgen_test]
fn capture_recording_is_a_plain_object_and_a_key_left_out_keeps_its_value() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    let keys = [
        "lineStates",
        "pixelSources",
        "lineSprites",
        "videoMemorySources",
        "frameWrites",
    ];
    assert_plain_object(&emu.get_capture_recording(), &keys, "getCaptureRecording");
    let on = |emu: &RetroEmulator, key: &str| field(&emu.get_capture_recording(), key).as_bool();
    for key in keys {
        assert_eq!(on(&emu, key), Some(false), "{key} starts off");
    }

    emu.set_capture_recording(options(&[("lineStates", true)]));
    assert_eq!(on(&emu, "lineStates"), Some(true));
    emu.set_capture_recording(options(&[]));
    assert_eq!(
        on(&emu, "lineStates"),
        Some(true),
        "a key left out keeps its value"
    );
    emu.set_capture_recording(options(&[("pixelSources", true), ("lineSprites", true)]));
    assert_eq!(on(&emu, "lineStates"), Some(true));
    assert_eq!(on(&emu, "pixelSources"), Some(true), "read back as asked");
    assert_eq!(on(&emu, "lineSprites"), Some(true), "read back as asked");
    emu.set_capture_recording(options(&[
        ("videoMemorySources", true),
        ("frameWrites", true),
    ]));
    assert_eq!(
        on(&emu, "videoMemorySources"),
        Some(true),
        "read back as asked"
    );
    assert_eq!(on(&emu, "frameWrites"), Some(true), "read back as asked");
    emu.set_capture_recording(options(&[("lineStates", false)]));
    assert_eq!(on(&emu, "lineStates"), Some(false));
    assert_eq!(on(&emu, "pixelSources"), Some(true));
    // The pixel sources keep the line table on their own.
    emu.load_rom(&looping_rom()).unwrap();
    emu.set_execution_mode("Running".to_string());
    emu.run_frame();
    assert!(!emu.get_line_states().is_null());
    emu.set_capture_recording(options(&[("pixelSources", false), ("lineSprites", false)]));
    assert!(emu.get_line_states().is_null());
}

#[wasm_bindgen_test]
fn line_states_their_palettes_and_the_picture_drawn_from_them_are_plain_objects_of_copies() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.load_rom(&looping_rom()).unwrap();
    emu.set_execution_mode("Running".to_string());
    assert!(emu.get_line_states().is_null(), "nothing recorded yet");
    assert!(emu.get_line_palettes().is_null());
    assert!(emu.picture_from_line_states().is_null());
    let layer = emu.render_layer(0);
    assert_plain_object(
        &layer,
        &["width", "height", "data", "fromLineStates"],
        "renderLayer",
    );
    assert_eq!(field(&layer, "fromLineStates").as_bool(), Some(false));

    emu.set_capture_recording(options(&[("lineStates", true)]));
    emu.run_frame();
    emu.run_to_capture_boundary();

    let states = emu.get_line_states();
    assert_plain_object(
        &states,
        &[
            "frame",
            "fromFirstLine",
            "complete",
            "memoryExact",
            "rowsPerLine",
            "firstRowLine",
            "columns",
            "values",
        ],
        "getLineStates",
    );
    assert_eq!(
        field(&states, "frame").as_f64(),
        Some((emu.get_frame_number() + 1) as f64)
    );
    assert_eq!(field(&states, "complete").as_bool(), Some(true));
    assert_eq!(field(&states, "fromFirstLine").as_bool(), Some(true));
    assert_eq!(field(&states, "rowsPerLine").as_f64(), Some(1.0));
    let columns = js_sys::Array::from(&field(&states, "columns"));
    assert_eq!(columns.length(), 72);
    assert_eq!(columns.get(0).as_string().as_deref(), Some("line"));
    assert_eq!(columns.get(71).as_string().as_deref(), Some("palette"));
    let values: js_sys::Uint32Array = field(&states, "values").dyn_into().unwrap();
    assert_eq!(values.length(), 72 * 224);
    assert!(!views_module_memory(&values.buffer()));

    let palettes = emu.get_line_palettes();
    assert_plain_object(
        &palettes,
        &["coloursPerPalette", "palettes", "paletteOfLine", "changes"],
        "getLinePalettes",
    );
    assert_eq!(field(&palettes, "coloursPerPalette").as_f64(), Some(64.0));
    let of_line: js_sys::Uint16Array = field(&palettes, "paletteOfLine").dyn_into().unwrap();
    assert_eq!(of_line.length(), 224);
    let all: js_sys::Uint16Array = field(&palettes, "palettes").dyn_into().unwrap();
    assert!(all.length() > 0 && all.length().is_multiple_of(64));
    let changes: js_sys::Uint16Array = field(&palettes, "changes").dyn_into().unwrap();
    assert!(changes.length().is_multiple_of(5));
    assert!(!views_module_memory(&all.buffer()));

    let picture = emu.picture_from_line_states();
    assert_plain_object(
        &picture,
        &["width", "height", "data"],
        "pictureFromLineStates",
    );
    assert_eq!(field(&picture, "width").as_f64(), Some(320.0));
    assert_eq!(field(&picture, "height").as_f64(), Some(224.0));
    let data: js_sys::Uint8Array = field(&picture, "data").dyn_into().unwrap();
    assert!(!views_module_memory(&data.buffer()));
    assert_eq!(data.to_vec(), emu.get_framebuffer_data().to_vec());

    let layer = emu.render_layer(0);
    assert_eq!(field(&layer, "fromLineStates").as_bool(), Some(true));
    assert!(emu.at_capture_boundary(), "nothing ran");
}

/// The typed array `value[key]` holds, its length checked and that it
/// is a copy, not a view into this module's memory.
fn typed_array<A>(value: &JsValue, key: &str, length: u32) -> A
where
    A: JsCast + AsRef<js_sys::Object>,
{
    let array: A = field(value, key)
        .dyn_into()
        .unwrap_or_else(|_| panic!("`{key}` is the typed array named"));
    let view: &js_sys::Object = array.as_ref();
    let buffer: js_sys::ArrayBuffer = js_sys::Reflect::get(view, &JsValue::from_str("buffer"))
        .unwrap()
        .dyn_into()
        .unwrap();
    assert!(!views_module_memory(&buffer), "`{key}` is a copy");
    let found = js_sys::Reflect::get(view, &JsValue::from_str("length"))
        .unwrap()
        .as_f64();
    assert_eq!(found, Some(f64::from(length)), "`{key}`'s length");
    array
}

#[wasm_bindgen_test]
fn pixel_sources_are_a_plain_object_of_copies_with_the_planes_asked_for() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    assert!(emu.get_pixel_sources(None).is_null(), "no game loaded");
    emu.load_rom(&looping_rom()).unwrap();
    emu.set_execution_mode("Running".to_string());
    emu.set_capture_recording(options(&[("pixelSources", true)]));
    emu.run_frame();
    emu.run_to_capture_boundary();

    let sources = emu.get_pixel_sources(None);
    assert_plain_object(
        &sources,
        &[
            "frame",
            "width",
            "height",
            "fromLineStates",
            "matchesPicture",
            "layer",
            "colour",
            "tile",
            "cell",
        ],
        "getPixelSources",
    );
    assert_eq!(
        field(&sources, "frame").as_f64(),
        Some((emu.get_frame_number() + 1) as f64)
    );
    assert_eq!(field(&sources, "width").as_f64(), Some(320.0));
    assert_eq!(field(&sources, "height").as_f64(), Some(224.0));
    assert_eq!(field(&sources, "fromLineStates").as_bool(), Some(true));
    assert_eq!(field(&sources, "matchesPicture").as_bool(), Some(true));
    let pixels = 320 * 224;
    typed_array::<js_sys::Uint16Array>(&sources, "layer", pixels);
    typed_array::<js_sys::Uint16Array>(&sources, "colour", pixels);
    typed_array::<js_sys::Uint32Array>(&sources, "tile", pixels);
    typed_array::<js_sys::Uint16Array>(&sources, "cell", pixels);

    // A plane not asked for is not there; bit 16 is ignored.
    let layer_only = emu.get_pixel_sources(Some(1 | 16));
    typed_array::<js_sys::Uint16Array>(&layer_only, "layer", pixels);
    for key in ["colour", "tile", "cell", "subLayer", "subColour"] {
        assert!(
            field(&layer_only, key).is_undefined(),
            "{key} not asked for"
        );
    }
    assert!(emu.at_capture_boundary(), "nothing ran");
}

#[wasm_bindgen_test]
fn line_sprites_are_a_plain_object_of_copies_in_columns() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    assert!(emu.get_line_sprites().is_null(), "no game loaded");
    emu.load_rom(&looping_rom()).unwrap();
    emu.set_execution_mode("Running".to_string());
    emu.set_capture_recording(options(&[("lineSprites", true)]));
    emu.run_frame();
    emu.run_to_capture_boundary();

    let sprites = emu.get_line_sprites();
    assert_plain_object(
        &sprites,
        &[
            "frame",
            "fromLineStates",
            "lines",
            "slots",
            "fates",
            "x",
            "width",
            "drawnWidth",
        ],
        "getLineSprites",
    );
    assert_eq!(field(&sprites, "fromLineStates").as_bool(), Some(true));
    let lines: js_sys::Uint16Array = typed_array(&sprites, "lines", 224 * 4);
    let lines = lines.to_vec();
    let lines = lines.as_chunks::<4>().0;
    let entries: u32 = lines.iter().map(|line| u32::from(line[2])).sum();
    for (row, line) in lines.iter().enumerate() {
        assert_eq!(usize::from(line[0]), row, "every line in order");
    }
    typed_array::<js_sys::Uint8Array>(&sprites, "slots", entries);
    typed_array::<js_sys::Uint8Array>(&sprites, "fates", entries);
    typed_array::<js_sys::Int16Array>(&sprites, "x", entries);
    typed_array::<js_sys::Uint8Array>(&sprites, "width", entries);
    typed_array::<js_sys::Uint8Array>(&sprites, "drawnWidth", entries);
}

#[wasm_bindgen_test]
fn chosen_sprites_drawn_alone_have_the_shape_of_a_layer() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    assert!(emu.render_sprites(&[1; 80]).is_null(), "no game loaded");
    emu.load_rom(&looping_rom()).unwrap();
    emu.set_execution_mode("Running".to_string());
    emu.set_capture_recording(options(&[("lineStates", true)]));
    emu.run_frame();
    emu.run_to_capture_boundary();

    let all = emu.render_sprites(&[1; 80]);
    assert_plain_object(
        &all,
        &["width", "height", "data", "fromLineStates"],
        "renderSprites",
    );
    assert_eq!(field(&all, "width").as_f64(), Some(320.0));
    assert_eq!(field(&all, "height").as_f64(), Some(224.0));
    assert_eq!(field(&all, "fromLineStates").as_bool(), Some(true));
    let data: js_sys::Uint8Array = typed_array(&all, "data", 320 * 224 * 4);
    let layer = emu.render_layer(3);
    let layer_data: js_sys::Uint8Array = field(&layer, "data").dyn_into().unwrap();
    assert_eq!(data.to_vec(), layer_data.to_vec(), "every slot: the layer");
    let none = emu.render_sprites(&[]);
    let none: js_sys::Uint8Array = field(&none, "data").dyn_into().unwrap();
    assert!(none.to_vec().iter().all(|&byte| byte == 0));
}

#[wasm_bindgen_test]
fn the_sprite_table_stops_where_the_renderer_stops() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    // 40 cells and the table at 0, the VDP's state after a reset: slot 0
    // links to slot 100, past the table's 80 slots.
    emu.write_vram(3, &[100]);
    assert_eq!(
        js_sys::Array::from(&emu.get_sprite_attr_table()).length(),
        1
    );
}

#[wasm_bindgen_test]
fn write_vram_reads_back_through_get_vram_wrapping_at_the_end() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.write_vram(0xFFFE, &[1, 2, 3, 4]);
    let vram = emu.get_vram();
    assert_eq!(&vram[0xFFFE..], &[1, 2]);
    assert_eq!(&vram[..2], &[3, 4], "the run goes on from the start");

    emu.write_vram(0x1_0005, &[9]);
    assert_eq!(emu.get_vram()[5], 9, "an address past the end wraps");

    let before = emu.get_vram();
    emu.write_vram(0x10, &[]);
    assert_eq!(emu.get_vram(), before, "an empty write changes nothing");
}

#[wasm_bindgen_test]
fn write_cram_reads_back_and_shows_after_a_repaint() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    // Colour 63, then on round to colour 0, keeping only the colour bits.
    emu.write_cram(63, &[0xFFFF, 0x000E]);
    let cram = emu.get_cram();
    assert_eq!(cram[63], 0x0EEE);
    assert_eq!(cram[0], 0x000E);

    // A fresh VDP has its display off, so the screen is the backdrop:
    // colour 0, red now.
    emu.repaint_frame();
    let (r, g, b) = sg1_core::vdp::Vdp::convert_genesis_color(0x000E);
    assert_eq!(&emu.get_framebuffer_data().to_vec()[..4], &[r, g, b, 255]);
}

#[wasm_bindgen_test]
fn write_vsram_reads_back_eleven_bits_wrapping_at_the_end() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.write_vsram(63, &[0xFFFF, 0x0123]);
    let vsram = emu.get_vsram();
    assert_eq!(vsram[63], 0x07FF);
    assert_eq!(vsram[0], 0x0123);

    let long: Vec<u16> = (0..70).map(|i| if i < 64 { 1 } else { 2 }).collect();
    emu.write_vsram(10, &long);
    assert!(
        emu.get_vsram().iter().all(|&value| value == 1),
        "a buffer longer than VSRAM is cut to it"
    );
}

/// A ROM whose 68000 stores colour 0 through the data port, then loops.
fn colour_store_rom() -> Vec<u8> {
    let mut rom = synth_genesis_rom("COLOUR STORE");
    #[rustfmt::skip]
        let code: [u16; 10] = [
            0x23FC, 0xC000, 0x0000, 0x00C0, 0x0004, // MOVE.L #$C0000000,($C00004).L
            0x33FC, 0x0EEE, 0x00C0, 0x0000,         // MOVE.W #$0EEE,($C00000).L
            0x60FE,                                 // BRA *
        ];
    for (i, word) in code.iter().enumerate() {
        rom[0x400 + i * 2..0x402 + i * 2].copy_from_slice(&word.to_be_bytes());
    }
    rom
}

#[wasm_bindgen_test]
fn the_writes_to_video_memory_are_plain_objects_of_copies() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.load_rom(&colour_store_rom()).unwrap();
    assert!(emu.get_video_memory_sources().is_null(), "not recorded");
    assert!(emu.take_frame_writes().is_null(), "not recorded");
    emu.set_capture_recording(options(&[
        ("videoMemorySources", true),
        ("frameWrites", true),
    ]));
    emu.set_execution_mode("Running".to_string());
    emu.run_frame();
    emu.write_vram(0x20, &[1, 2]);

    let sources = emu.get_video_memory_sources();
    assert_plain_object(&sources, &["origins", "frames"], "getVideoMemorySources");
    let origins: js_sys::Uint32Array = field(&sources, "origins").dyn_into().unwrap();
    let frames: js_sys::Uint32Array = field(&sources, "frames").dyn_into().unwrap();
    assert_eq!((origins.length(), frames.length()), (0x1_0000, 0x1_0000));
    assert!(!views_module_memory(&origins.buffer()));
    assert!(!views_module_memory(&frames.buffer()));
    assert_eq!(origins.get_index(0x20) >> 28, 4, "the debugger wrote it");
    assert_eq!(origins.get_index(0x22), 0, "nothing wrote it");

    let writes = emu.take_frame_writes();
    assert_plain_object(
        &writes,
        &["records", "wordsPerRecord", "dropped"],
        "takeFrameWrites",
    );
    assert_eq!(field(&writes, "wordsPerRecord").as_f64(), Some(8.0));
    assert_eq!(field(&writes, "dropped").as_f64(), Some(0.0));
    let records: js_sys::Uint32Array = field(&writes, "records").dyn_into().unwrap();
    assert!(!views_module_memory(&records.buffer()));
    assert_eq!(records.length(), 8, "the colour store");
    assert_eq!(records.get_index(0), 4 | (3 << 16));
    assert_eq!(records.get_index(4), 1 | (2 << 8));
    assert_eq!((records.get_index(5), records.get_index(6)), (0, 0x0EEE));
    let again = emu.take_frame_writes();
    let again: js_sys::Uint32Array = field(&again, "records").dyn_into().unwrap();
    assert_eq!(again.length(), 0, "a take empties the record");

    let resolved = emu.resolve_sources(&[0x40C, 0xFF_0000, 0xC0_0000]);
    assert!(resolved.is_instance_of::<js_sys::Int32Array>());
    assert!(!views_module_memory(&resolved.buffer()));
    assert_eq!(resolved.to_vec(), [0x40C, -2, -1]);
}

// ===== What each capture reader returns, whole =====
//
// Each reader's value on a program that draws, moves its sprites and
// writes video memory every way the 68000 can, described in full by
// `retro_wasm::test_support::shape` (every key, every value, each typed
// array's constructor, length and a hash of its bytes) and held to the
// description recorded from these bindings. A change that moves a key,
// a value, an array's type or one byte of what a host reads fails here.

/// A program that draws both planes and 48 sprites, moves two of the
/// sprites every frame, and writes video memory every way the 68000
/// can. As it boots it sends the tiles, plane A's map, the palette and
/// the sprite table by transfer, fills four tiles, copies two of them,
/// and stores plane B's cells, a colour, the vertical scroll and a
/// sprite's Y through the data port. Its vertical interrupt moves
/// sprites 0 and 1, puts colour 1 and plane A's horizontal scroll back
/// and sends a tile again by transfer; its horizontal interrupt, every
/// 100 lines, changes colour 1 and plane A's horizontal scroll part of
/// the way down the picture. The sprites meet the line's sprite limit
/// on lines 120 to 127, run out of dots on lines 160 to 167, meet the
/// sprite mask on lines 200 to 207 and lie off screen on lines 60 to
/// 67.
pub(super) fn capture_shapes_rom() -> Vec<u8> {
    let mut rom = synth_genesis_rom("CAPTURE SHAPES");
    place(&mut rom, 0x00, &[0x00FF, 0xFE00]); // the stack
    place(&mut rom, 0x70, &[0x0000, 0x0700]); // level 4: the horizontal interrupt
    place(&mut rom, 0x78, &[0x0000, 0x0600]); // level 6: the vertical interrupt
    #[rustfmt::skip]
        place(&mut rom, 0x400, &[
            0x46FC, 0x2700,                 // MOVE.W #$2700,SR
            0x41F9, 0x00C0, 0x0004,         // LEA $C00004,A0: control port
            0x43F9, 0x00C0, 0x0000,         // LEA $C00000,A1: data port
            0x30BC, 0x8014,                 // register 0: horizontal interrupt on
            0x30BC, 0x8174,                 // register 1: display, vertical interrupt, transfers
            0x30BC, 0x8230,                 // register 2: plane A at $C000
            0x30BC, 0x8407,                 // register 4: plane B at $E000
            0x30BC, 0x8578,                 // register 5: sprites at $F000
            0x30BC, 0x8700,                 // register 7: backdrop colour 0
            0x30BC, 0x8A63,                 // register 10: an interrupt every 100 lines
            0x30BC, 0x8B00,                 // register 11: whole-screen scrolls
            0x30BC, 0x8C81,                 // register 12: 40 cells
            0x30BC, 0x8D3F,                 // register 13: horizontal scroll at $FC00
            0x30BC, 0x8F02,                 // register 15: auto-increment 2
            0x30BC, 0x9001,                 // register 16: planes of 64 by 32 cells
            // Tiles 1 to 16 from ROM $1000: 256 words.
            0x30BC, 0x9300, 0x30BC, 0x9401,
            0x30BC, 0x9500, 0x30BC, 0x9608, 0x30BC, 0x9700,
            0x20BC, 0x4020, 0x0080,         // VRAM $0020 by transfer
            // Plane A's map from ROM $2000: 2048 words.
            0x30BC, 0x9300, 0x30BC, 0x9408,
            0x30BC, 0x9500, 0x30BC, 0x9610, 0x30BC, 0x9700,
            0x20BC, 0x4000, 0x0083,         // VRAM $C000 by transfer
            // Tiles 20 to 23 filled with colour 3, increment 1.
            0x30BC, 0x8F01,
            0x30BC, 0x9380, 0x30BC, 0x9400,
            0x30BC, 0x9780,                 // fill
            0x20BC, 0x4280, 0x0080,         // VRAM $0280 by fill
            0x32BC, 0x3333,                 // MOVE.W #$3333,(A1): starts the fill
            0x3010,                         // MOVE.W (A0),D0: the status
            0x0800, 0x0001,                 // BTST #1,D0: a transfer running
            0x66F8,                         // BNE.S back to the status
            // Tiles 24 and 25 copied from tiles 20 and 21.
            0x30BC, 0x9340, 0x30BC, 0x9400,
            0x30BC, 0x9580, 0x30BC, 0x9602, // from $0280
            0x30BC, 0x97C0,                 // copy
            0x20BC, 0x0300, 0x00C0,         // VRAM $0300 by copy
            0x3010,
            0x0800, 0x0001,
            0x66F8,
            0x30BC, 0x8F02,                 // register 15: auto-increment 2
            // Eight cells of plane B on row 5, tiles 20 and 24 by turns.
            0x20BC, 0x6280, 0x0003,         // VRAM $E280
            0x32BC, 0x0014, 0x32BC, 0x0018, 0x32BC, 0x0014, 0x32BC, 0x0018,
            0x32BC, 0x0014, 0x32BC, 0x0018, 0x32BC, 0x0014, 0x32BC, 0x0018,
            // The palette from ROM $1200: 64 words.
            0x30BC, 0x9340, 0x30BC, 0x9400,
            0x30BC, 0x9500, 0x30BC, 0x9609, 0x30BC, 0x9700,
            0x20BC, 0xC000, 0x0080,         // CRAM colour 0 by transfer
            0x20BC, 0xC002, 0x0000,         // CRAM colour 1
            0x32BC, 0x000E,                 // MOVE.W #$000E,(A1)
            0x20BC, 0x4000, 0x0010,         // VSRAM word 0
            0x32BC, 0x0004,                 // plane A four lines up
            0x32BC, 0x0000,                 // plane B
            // The 48 sprites from ROM $1300: 192 words.
            0x30BC, 0x93C0, 0x30BC, 0x9400,
            0x30BC, 0x9580, 0x30BC, 0x9609, 0x30BC, 0x9700,
            0x20BC, 0x7000, 0x0083,         // VRAM $F000 by transfer
            0x20BC, 0x7000, 0x0003,         // VRAM $F000
            0x32BC, 0x0098,                 // sprite 0's Y: 24
            0x46FC, 0x2000,                 // MOVE.W #$2000,SR: interrupts on
            0x60FE,                         // BRA *
        ]);
    #[rustfmt::skip]
        place(&mut rom, 0x600, &[
            0x5279, 0x00FF, 0x0000,         // ADDQ.W #1,($FF0000).L: the frames
            0x3039, 0x00FF, 0x0000,         // MOVE.W ($FF0000).L,D0
            0x0640, 0x00A0,                 // ADDI.W #$A0,D0
            0x20BC, 0x7006, 0x0003,         // VRAM $F006: sprite 0's X
            0x3280,                         // MOVE.W D0,(A1): 32 + the frames
            0x0640, 0x0028,                 // ADDI.W #40,D0
            0x20BC, 0x700E, 0x0003,         // VRAM $F00E: sprite 1's X
            0x3280,                         // MOVE.W D0,(A1)
            0x20BC, 0xC002, 0x0000,         // CRAM colour 1
            0x32BC, 0x000E,                 // back to red
            0x20BC, 0x7C00, 0x0003,         // VRAM $FC00: plane A's horizontal scroll
            0x32BC, 0x0000,                 // back to 0
            // Tile 1 sent again, as tile 40: 16 words.
            0x30BC, 0x9310, 0x30BC, 0x9400,
            0x30BC, 0x9500, 0x30BC, 0x9608, 0x30BC, 0x9700,
            0x20BC, 0x4500, 0x0080,         // VRAM $0500 by transfer
            0x4E73,                         // RTE
        ]);
    #[rustfmt::skip]
        place(&mut rom, 0x700, &[
            0x20BC, 0xC002, 0x0000,         // CRAM colour 1
            0x32BC, 0x00E0,                 // green from here on
            0x20BC, 0x7C00, 0x0003,         // VRAM $FC00
            0x32BC, 0x0010,                 // plane A sixteen pixels right
            0x4E73,                         // RTE
        ]);
    // Tiles 1 to 16: diagonal stripes of every colour, with gaps.
    for tile in 0..16usize {
        for row in 0..8usize {
            let colour = |x: usize| -> u8 {
                if (x + row + tile).is_multiple_of(5) {
                    0
                } else {
                    ((tile + x * 2 + row) % 15 + 1) as u8
                }
            };
            for pair in 0..4usize {
                rom[0x1000 + tile * 32 + row * 4 + pair] =
                    (colour(pair * 2) << 4) | colour(pair * 2 + 1);
            }
        }
    }
    // The palette: 64 colours.
    for colour in 0..64u16 {
        let word = ((colour % 8) << 1) | (((colour / 8) % 8) << 5) | (((colour * 3) % 8) << 9);
        place(&mut rom, 0x1200 + usize::from(colour) * 2, &[word]);
    }
    // The sprites, linked in slot order, each (y, size, the table's x).
    for slot in 0..48u16 {
        let (y, size, x) = match slot {
            0..=7 => (
                16 + slot * 24,
                ((slot % 4) << 2) | (slot % 4),
                152 + slot * 36,
            ),
            8..=29 => (120, 0, 128 + (slot - 8) * 14),
            30..=41 => (160, 0b1100, 128 + (slot - 30) * 30),
            42 => (200, 0, 228),
            43 => (200, 0, 0),
            44 => (200, 0, 278),
            45 => (60, 0, 88),
            46 => (60, 0b0101, 458),
            _ => (60, 0, 300),
        };
        let link = if slot == 47 { 0 } else { slot + 1 };
        let pattern = ((slot % 2) << 15)
            | ((slot % 4) << 13)
            | ((slot % 3 / 2) << 12)
            | ((slot % 5 / 4) << 11)
            | (1 + slot % 16);
        let entry = [128 + y, (size << 8) | link, pattern, x];
        place(&mut rom, 0x1300 + usize::from(slot) * 8, &entry);
    }
    // Plane A's map, 64 by 32 cells.
    for row in 0..32u16 {
        for column in 0..64u16 {
            let cell = (u16::from(column % 7 == 0) << 15)
                | (((row / 8) % 4) << 13)
                | (u16::from(column % 3 == 0) << 12)
                | (u16::from(row % 5 == 0) << 11)
                | (1 + (row + column) % 16);
            place(
                &mut rom,
                0x2000 + usize::from(row * 64 + column) * 2,
                &[cell],
            );
        }
    }
    rom
}

/// Every recorder on.
const EVERY_RECORDER: [(&str, bool); 5] = [
    ("lineStates", true),
    ("pixelSources", true),
    ("lineSprites", true),
    ("videoMemorySources", true),
    ("frameWrites", true),
];

/// [`capture_shapes_rom`] with `recorded` on from its first frame, run
/// five frames and the sixth to its capture boundary.
fn held_capture_shapes(recorded: &[(&str, bool)]) -> RetroEmulator {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.load_rom(&capture_shapes_rom()).unwrap();
    emu.set_execution_mode("Running".to_string());
    emu.set_capture_recording(options(recorded));
    for _ in 0..5 {
        emu.run_frame();
    }
    emu.run_to_capture_boundary();
    emu
}

#[wasm_bindgen_test]
fn what_a_frame_snapshot_holds() {
    let mut emu = held_capture_shapes(&EVERY_RECORDER);
    let held = emu.capture_frame_snapshot();
    emu.run_frame();
    let after_the_frame = emu.capture_frame_snapshot();
    assert_shapes(
        &[
            ("held at the boundary", held),
            ("after the frame", after_the_frame),
        ],
        &[
            (
                "held at the boundary",
                "{atBoundary: true, frame: 6, framebuffer: Uint8Array(len 286720, \
                     fnv1a64 0x90450a1e4b4824fc), height: 224, palette: Uint16Array(len 64, \
                     fnv1a64 0xd9034ab48a655301), registers: [{description: \"\", index: 0, \
                     name: \"Mode Register 1\", value: 20}, {description: \"\", index: 1, \
                     name: \"Mode Register 2\", value: 116}, {description: \"\", index: 2, \
                     name: \"Plane A Name Table\", value: 48}, {description: \"\", index: 3, \
                     name: \"Window Name Table\", value: 0}, {description: \"\", index: 4, \
                     name: \"Plane B Name Table\", value: 7}, {description: \"\", index: 5, \
                     name: \"Sprite Table\", value: 120}, {description: \"\", index: 6, \
                     name: \"Sprite Pattern Generator\", value: 0}, {description: \"\", index: 7, \
                     name: \"Background Color\", value: 0}, {description: \"\", index: 8, \
                     name: \"Unused ($08)\", value: 0}, {description: \"\", index: 9, \
                     name: \"Unused ($09)\", value: 0}, {description: \"\", index: 10, \
                     name: \"H Interrupt Counter\", value: 99}, {description: \"\", index: 11, \
                     name: \"Mode Register 3\", value: 0}, {description: \"\", index: 12, \
                     name: \"Mode Register 4\", value: 129}, {description: \"\", index: 13, \
                     name: \"H Scroll Table\", value: 63}, {description: \"\", index: 14, \
                     name: \"Plane Pattern Generator\", value: 0}, {description: \"\", index: 15, \
                     name: \"Auto Increment\", value: 2}, {description: \"\", index: 16, \
                     name: \"Plane Size\", value: 1}, {description: \"\", index: 17, \
                     name: \"Window H Position\", value: 0}, {description: \"\", index: 18, \
                     name: \"Window V Position\", value: 0}, {description: \"\", index: 19, \
                     name: \"DMA Length Low\", value: 0}, {description: \"\", index: 20, \
                     name: \"DMA Length High\", value: 0}, {description: \"\", index: 21, \
                     name: \"DMA Source Low\", value: 16}, {description: \"\", index: 22, \
                     name: \"DMA Source Mid\", value: 8}, {description: \"\", index: 23, \
                     name: \"DMA Source High\", value: 0}], vram: Uint8Array(len 65536, \
                     fnv1a64 0x8a29d79466aa96ef), vsram: Uint16Array(len 64, \
                     fnv1a64 0xbb0bb9ec6512e521), width: 320}",
            ),
            (
                "after the frame",
                "{atBoundary: false, frame: 6, framebuffer: Uint8Array(len 286720, \
                     fnv1a64 0x90450a1e4b4824fc), height: 224, palette: Uint16Array(len 64, \
                     fnv1a64 0x80a6cbe3ccc7196f), registers: [{description: \"\", index: 0, \
                     name: \"Mode Register 1\", value: 20}, {description: \"\", index: 1, \
                     name: \"Mode Register 2\", value: 116}, {description: \"\", index: 2, \
                     name: \"Plane A Name Table\", value: 48}, {description: \"\", index: 3, \
                     name: \"Window Name Table\", value: 0}, {description: \"\", index: 4, \
                     name: \"Plane B Name Table\", value: 7}, {description: \"\", index: 5, \
                     name: \"Sprite Table\", value: 120}, {description: \"\", index: 6, \
                     name: \"Sprite Pattern Generator\", value: 0}, {description: \"\", index: 7, \
                     name: \"Background Color\", value: 0}, {description: \"\", index: 8, \
                     name: \"Unused ($08)\", value: 0}, {description: \"\", index: 9, \
                     name: \"Unused ($09)\", value: 0}, {description: \"\", index: 10, \
                     name: \"H Interrupt Counter\", value: 99}, {description: \"\", index: 11, \
                     name: \"Mode Register 3\", value: 0}, {description: \"\", index: 12, \
                     name: \"Mode Register 4\", value: 129}, {description: \"\", index: 13, \
                     name: \"H Scroll Table\", value: 63}, {description: \"\", index: 14, \
                     name: \"Plane Pattern Generator\", value: 0}, {description: \"\", index: 15, \
                     name: \"Auto Increment\", value: 2}, {description: \"\", index: 16, \
                     name: \"Plane Size\", value: 1}, {description: \"\", index: 17, \
                     name: \"Window H Position\", value: 0}, {description: \"\", index: 18, \
                     name: \"Window V Position\", value: 0}, {description: \"\", index: 19, \
                     name: \"DMA Length Low\", value: 0}, {description: \"\", index: 20, \
                     name: \"DMA Length High\", value: 0}, {description: \"\", index: 21, \
                     name: \"DMA Source Low\", value: 16}, {description: \"\", index: 22, \
                     name: \"DMA Source Mid\", value: 8}, {description: \"\", index: 23, \
                     name: \"DMA Source High\", value: 0}], vram: Uint8Array(len 65536, \
                     fnv1a64 0x1c28c67504e3498b), vsram: Uint16Array(len 64, \
                     fnv1a64 0xbb0bb9ec6512e521), width: 320}",
            ),
        ],
    );
}

#[wasm_bindgen_test]
fn what_each_layer_drawn_alone_holds() {
    let mut emu = held_capture_shapes(&EVERY_RECORDER);
    let mut found = Vec::new();
    for (what, layer) in [
        ("plane A", 0),
        ("plane B", 1),
        ("window", 2),
        ("sprites", 3),
        ("layer 4", 4),
        ("layer 99", 99),
    ] {
        found.push((what, emu.render_layer(layer)));
    }
    let mut unrecorded = held_capture_shapes(&[]);
    found.push(("plane A, nothing recorded", unrecorded.render_layer(0)));
    assert_shapes(
        &found,
        &[
            (
                "plane A",
                "{data: Uint8Array(len 286720, fnv1a64 0xbea5713f2915922e), \
                     fromLineStates: true, height: 224, width: 320}",
            ),
            (
                "plane B",
                "{data: Uint8Array(len 286720, fnv1a64 0x4bbb1fba9a66c325), \
                     fromLineStates: true, height: 224, width: 320}",
            ),
            (
                "window",
                "{data: Uint8Array(len 286720, fnv1a64 0xe97846eb7709a325), \
                     fromLineStates: true, height: 224, width: 320}",
            ),
            (
                "sprites",
                "{data: Uint8Array(len 286720, fnv1a64 0xa2af1da2713f146f), \
                     fromLineStates: true, height: 224, width: 320}",
            ),
            ("layer 4", "null"),
            ("layer 99", "null"),
            (
                "plane A, nothing recorded",
                "{data: Uint8Array(len 286720, fnv1a64 0xf1ef581015fff896), \
                     fromLineStates: false, height: 224, width: 320}",
            ),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_capture_recording_reads_back() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    let mut found = vec![("at first", emu.get_capture_recording())];
    emu.set_capture_recording(options(&[("lineStates", true), ("frameWrites", true)]));
    found.push(("two turned on", emu.get_capture_recording()));
    emu.set_capture_recording(options(&[("pixelSources", true), ("frameWrites", false)]));
    found.push(("a partial object", emu.get_capture_recording()));
    emu.set_capture_recording(options(&[]));
    found.push(("an empty object", emu.get_capture_recording()));
    emu.set_capture_recording(JsValue::UNDEFINED);
    found.push(("no object", emu.get_capture_recording()));
    let not_booleans = js_sys::Object::new();
    for (key, value) in [
        ("lineSprites", JsValue::from_f64(1.0)),
        ("lineStates", JsValue::from_str("false")),
        ("videoMemorySources", JsValue::NULL),
    ] {
        js_sys::Reflect::set(&not_booleans, &JsValue::from_str(key), &value).unwrap();
    }
    emu.set_capture_recording(not_booleans.into());
    found.push(("values that are not booleans", emu.get_capture_recording()));
    emu.set_capture_recording(options(&EVERY_RECORDER));
    found.push(("every recorder", emu.get_capture_recording()));
    assert_shapes(
        &found,
        &[
            (
                "at first",
                "{frameWrites: false, lineSprites: false, lineStates: false, \
                     pixelSources: false, videoMemorySources: false}",
            ),
            (
                "two turned on",
                "{frameWrites: true, lineSprites: false, lineStates: true, \
                     pixelSources: false, videoMemorySources: false}",
            ),
            (
                "a partial object",
                "{frameWrites: false, lineSprites: false, lineStates: true, \
                     pixelSources: true, videoMemorySources: false}",
            ),
            (
                "an empty object",
                "{frameWrites: false, lineSprites: false, lineStates: true, \
                     pixelSources: true, videoMemorySources: false}",
            ),
            (
                "no object",
                "{frameWrites: false, lineSprites: false, lineStates: true, \
                     pixelSources: true, videoMemorySources: false}",
            ),
            (
                "values that are not booleans",
                "{frameWrites: false, lineSprites: false, lineStates: true, \
                     pixelSources: true, videoMemorySources: false}",
            ),
            (
                "every recorder",
                "{frameWrites: true, lineSprites: true, lineStates: true, pixelSources: true, \
                     videoMemorySources: true}",
            ),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_line_states_hold() {
    let recorded = held_capture_shapes(&EVERY_RECORDER);
    let for_the_sprites = held_capture_shapes(&[("lineSprites", true)]);
    let unrecorded = held_capture_shapes(&[]);
    assert_shapes(
        &[
            ("recorded", recorded.get_line_states()),
            (
                "kept for the line sprites",
                for_the_sprites.get_line_states(),
            ),
            ("not recorded", unrecorded.get_line_states()),
        ],
        &[
            (
                "recorded",
                "{columns: [\"line\", \"frame\", \"kind\", \"exact\", \"register[0]\", \
                     \"register[1]\", \"register[2]\", \"register[3]\", \"register[4]\", \
                     \"register[5]\", \"register[6]\", \"register[7]\", \"register[8]\", \
                     \"register[9]\", \"register[10]\", \"register[11]\", \"register[12]\", \
                     \"register[13]\", \"register[14]\", \"register[15]\", \"register[16]\", \
                     \"register[17]\", \"register[18]\", \"register[19]\", \"register[20]\", \
                     \"register[21]\", \"register[22]\", \"register[23]\", \
                     \"horizontal_scroll[0]\", \"horizontal_scroll[1]\", \"vertical_scroll[0]\", \
                     \"vertical_scroll[1]\", \"vertical_scroll[2]\", \"vertical_scroll[3]\", \
                     \"vertical_scroll[4]\", \"vertical_scroll[5]\", \"vertical_scroll[6]\", \
                     \"vertical_scroll[7]\", \"vertical_scroll[8]\", \"vertical_scroll[9]\", \
                     \"vertical_scroll[10]\", \"vertical_scroll[11]\", \"vertical_scroll[12]\", \
                     \"vertical_scroll[13]\", \"vertical_scroll[14]\", \"vertical_scroll[15]\", \
                     \"vertical_scroll[16]\", \"vertical_scroll[17]\", \"vertical_scroll[18]\", \
                     \"vertical_scroll[19]\", \"vertical_scroll[20]\", \"vertical_scroll[21]\", \
                     \"vertical_scroll[22]\", \"vertical_scroll[23]\", \"vertical_scroll[24]\", \
                     \"vertical_scroll[25]\", \"vertical_scroll[26]\", \"vertical_scroll[27]\", \
                     \"vertical_scroll[28]\", \"vertical_scroll[29]\", \"vertical_scroll[30]\", \
                     \"vertical_scroll[31]\", \"vertical_scroll[32]\", \"vertical_scroll[33]\", \
                     \"vertical_scroll[34]\", \"vertical_scroll[35]\", \"vertical_scroll[36]\", \
                     \"vertical_scroll[37]\", \"vertical_scroll[38]\", \"vertical_scroll[39]\", \
                     \"sprite_mask_carried_in\", \"palette\"], \
                     complete: true, firstRowLine: 0, frame: 6, fromFirstLine: true, \
                     memoryExact: true, rowsPerLine: 1, values: Uint32Array(len 16128, \
                     fnv1a64 0x6ff1a0129c1a30e5)}",
            ),
            (
                "kept for the line sprites",
                "{columns: [\"line\", \"frame\", \"kind\", \"exact\", \"register[0]\", \
                     \"register[1]\", \"register[2]\", \"register[3]\", \"register[4]\", \
                     \"register[5]\", \"register[6]\", \"register[7]\", \"register[8]\", \
                     \"register[9]\", \"register[10]\", \"register[11]\", \"register[12]\", \
                     \"register[13]\", \"register[14]\", \"register[15]\", \"register[16]\", \
                     \"register[17]\", \"register[18]\", \"register[19]\", \"register[20]\", \
                     \"register[21]\", \"register[22]\", \"register[23]\", \
                     \"horizontal_scroll[0]\", \"horizontal_scroll[1]\", \"vertical_scroll[0]\", \
                     \"vertical_scroll[1]\", \"vertical_scroll[2]\", \"vertical_scroll[3]\", \
                     \"vertical_scroll[4]\", \"vertical_scroll[5]\", \"vertical_scroll[6]\", \
                     \"vertical_scroll[7]\", \"vertical_scroll[8]\", \"vertical_scroll[9]\", \
                     \"vertical_scroll[10]\", \"vertical_scroll[11]\", \"vertical_scroll[12]\", \
                     \"vertical_scroll[13]\", \"vertical_scroll[14]\", \"vertical_scroll[15]\", \
                     \"vertical_scroll[16]\", \"vertical_scroll[17]\", \"vertical_scroll[18]\", \
                     \"vertical_scroll[19]\", \"vertical_scroll[20]\", \"vertical_scroll[21]\", \
                     \"vertical_scroll[22]\", \"vertical_scroll[23]\", \"vertical_scroll[24]\", \
                     \"vertical_scroll[25]\", \"vertical_scroll[26]\", \"vertical_scroll[27]\", \
                     \"vertical_scroll[28]\", \"vertical_scroll[29]\", \"vertical_scroll[30]\", \
                     \"vertical_scroll[31]\", \"vertical_scroll[32]\", \"vertical_scroll[33]\", \
                     \"vertical_scroll[34]\", \"vertical_scroll[35]\", \"vertical_scroll[36]\", \
                     \"vertical_scroll[37]\", \"vertical_scroll[38]\", \"vertical_scroll[39]\", \
                     \"sprite_mask_carried_in\", \"palette\"], \
                     complete: true, firstRowLine: 0, frame: 6, fromFirstLine: true, \
                     memoryExact: true, rowsPerLine: 1, values: Uint32Array(len 16128, \
                     fnv1a64 0x6ff1a0129c1a30e5)}",
            ),
            ("not recorded", "null"),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_line_palettes_hold() {
    let recorded = held_capture_shapes(&EVERY_RECORDER);
    let unrecorded = held_capture_shapes(&[]);
    assert_shapes(
        &[
            ("recorded", recorded.get_line_palettes()),
            ("not recorded", unrecorded.get_line_palettes()),
        ],
        &[
            (
                "recorded",
                "{changes: Uint16Array(len 10, fnv1a64 0x978cad4acc7ab705), \
                     coloursPerPalette: 64, paletteOfLine: Uint16Array(len 224, \
                     fnv1a64 0x5f8fc5aa294a6f75), palettes: Uint16Array(len 128, \
                     fnv1a64 0x0b27d5654f63eefb)}",
            ),
            ("not recorded", "null"),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_picture_drawn_from_the_line_states_holds() {
    let mut recorded = held_capture_shapes(&EVERY_RECORDER);
    let mut unrecorded = held_capture_shapes(&[]);
    assert_shapes(
        &[
            ("recorded", recorded.picture_from_line_states()),
            ("not recorded", unrecorded.picture_from_line_states()),
        ],
        &[
            (
                "recorded",
                "{data: Uint8Array(len 286720, fnv1a64 0x90450a1e4b4824fc), height: 224, \
                     width: 320}",
            ),
            ("not recorded", "null"),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_pixel_sources_hold_for_every_plane_mask() {
    let mut emu = held_capture_shapes(&EVERY_RECORDER);
    let mut found = vec![("planes left out".to_string(), emu.get_pixel_sources(None))];
    for planes in (0..16).chain([16, 31]) {
        found.push((
            format!("planes {planes}"),
            emu.get_pixel_sources(Some(planes)),
        ));
    }
    let mut unrecorded = held_capture_shapes(&[]);
    found.push((
        "nothing recorded".to_string(),
        unrecorded.get_pixel_sources(None),
    ));
    let mut no_game = RetroEmulator::new("genesis").unwrap();
    found.push(("no game".to_string(), no_game.get_pixel_sources(None)));
    assert_shapes(
        &found,
        &[
            (
                "planes left out",
                "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), \
                     colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
            ),
            (
                "planes 0",
                "{frame: 6, fromLineStates: true, height: 224, matchesPicture: true, \
                     width: 320}",
            ),
            (
                "planes 1",
                "{frame: 6, fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, width: 320}",
            ),
            (
                "planes 2",
                "{colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, matchesPicture: true, width: 320}",
            ),
            (
                "planes 3",
                "{colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, width: 320}",
            ),
            (
                "planes 4",
                "{frame: 6, fromLineStates: true, height: 224, matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
            ),
            (
                "planes 5",
                "{frame: 6, fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
            ),
            (
                "planes 6",
                "{colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
            ),
            (
                "planes 7",
                "{colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
            ),
            (
                "planes 8",
                "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), frame: 6, \
                     fromLineStates: true, height: 224, matchesPicture: true, width: 320}",
            ),
            (
                "planes 9",
                "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), frame: 6, \
                     fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, width: 320}",
            ),
            (
                "planes 10",
                "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), \
                     colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, matchesPicture: true, width: 320}",
            ),
            (
                "planes 11",
                "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), \
                     colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, width: 320}",
            ),
            (
                "planes 12",
                "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), frame: 6, \
                     fromLineStates: true, height: 224, matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
            ),
            (
                "planes 13",
                "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), frame: 6, \
                     fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
            ),
            (
                "planes 14",
                "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), \
                     colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
            ),
            (
                "planes 15",
                "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), \
                     colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
            ),
            (
                "planes 16",
                "{frame: 6, fromLineStates: true, height: 224, matchesPicture: true, \
                     width: 320}",
            ),
            (
                "planes 31",
                "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), \
                     colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
            ),
            (
                "nothing recorded",
                "{cell: Uint16Array(len 71680, fnv1a64 0x1534eb7b2a7cb9e3), \
                     colour: Uint16Array(len 71680, fnv1a64 0xbe0328133f1361c0), frame: 6, \
                     fromLineStates: false, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x6093cce1db1c529b), matchesPicture: false, \
                     tile: Uint32Array(len 71680, fnv1a64 0xbc22a43e29ecf66a), width: 320}",
            ),
            ("no game", "null"),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_line_sprites_hold() {
    let mut recorded = held_capture_shapes(&EVERY_RECORDER);
    let mut unrecorded = held_capture_shapes(&[]);
    let mut no_game = RetroEmulator::new("genesis").unwrap();
    assert_shapes(
        &[
            ("recorded", recorded.get_line_sprites()),
            ("nothing recorded", unrecorded.get_line_sprites()),
            ("no game", no_game.get_line_sprites()),
        ],
        &[
            (
                "recorded",
                "{drawnWidth: Uint8Array(len 488, fnv1a64 0x8a6c997556324945), \
                     fates: Uint8Array(len 488, fnv1a64 0x170fe62d918f96c5), frame: 6, \
                     fromLineStates: true, lines: Uint16Array(len 896, \
                     fnv1a64 0x4bd753302fe7e706), slots: Uint8Array(len 488, \
                     fnv1a64 0xe9ff911bdf30e495), width: Uint8Array(len 488, \
                     fnv1a64 0x2129b8dd2dc27785), x: Int16Array(len 488, \
                     fnv1a64 0x6f77b8bbd663dea5)}",
            ),
            (
                "nothing recorded",
                "{drawnWidth: Uint8Array(len 488, fnv1a64 0x8a6c997556324945), \
                     fates: Uint8Array(len 488, fnv1a64 0x170fe62d918f96c5), frame: 6, \
                     fromLineStates: false, lines: Uint16Array(len 896, \
                     fnv1a64 0x4bd753302fe7e706), slots: Uint8Array(len 488, \
                     fnv1a64 0xe9ff911bdf30e495), width: Uint8Array(len 488, \
                     fnv1a64 0x2129b8dd2dc27785), x: Int16Array(len 488, \
                     fnv1a64 0x6f77b8bbd663dea5)}",
            ),
            ("no game", "null"),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_sprite_render_order_holds() {
    let mut recorded = held_capture_shapes(&EVERY_RECORDER);
    let mut unrecorded = held_capture_shapes(&[]);
    let mut no_game = RetroEmulator::new("genesis").unwrap();
    assert_shapes(
        &[
            ("recorded", recorded.resolved_sprite_render_order()),
            (
                "nothing recorded",
                unrecorded.resolved_sprite_render_order(),
            ),
            ("no game", no_game.resolved_sprite_render_order()),
        ],
        &[
            (
                "recorded",
                "[{index: 47, priority: 1}, {index: 42, priority: 0}, \
                     {index: 39, priority: 1}, {index: 38, priority: 0}, {index: 37, priority: 1}, \
                     {index: 36, priority: 0}, {index: 35, priority: 1}, {index: 34, priority: 0}, \
                     {index: 33, priority: 1}, {index: 32, priority: 0}, {index: 31, priority: 1}, \
                     {index: 30, priority: 0}, {index: 27, priority: 1}, {index: 26, priority: 0}, \
                     {index: 25, priority: 1}, {index: 24, priority: 0}, {index: 23, priority: 1}, \
                     {index: 22, priority: 0}, {index: 21, priority: 1}, {index: 20, priority: 0}, \
                     {index: 19, priority: 1}, {index: 18, priority: 0}, {index: 17, priority: 1}, \
                     {index: 16, priority: 0}, {index: 15, priority: 1}, {index: 14, priority: 0}, \
                     {index: 13, priority: 1}, {index: 12, priority: 0}, {index: 11, priority: 1}, \
                     {index: 10, priority: 0}, {index: 9, priority: 1}, {index: 8, priority: 0}, \
                     {index: 7, priority: 1}, {index: 6, priority: 0}, {index: 5, priority: 1}, \
                     {index: 4, priority: 0}, {index: 3, priority: 1}, {index: 2, priority: 0}, \
                     {index: 1, priority: 1}, {index: 0, priority: 0}]",
            ),
            (
                "nothing recorded",
                "[{index: 47, priority: 1}, {index: 42, priority: 0}, \
                     {index: 39, priority: 1}, {index: 38, priority: 0}, {index: 37, priority: 1}, \
                     {index: 36, priority: 0}, {index: 35, priority: 1}, {index: 34, priority: 0}, \
                     {index: 33, priority: 1}, {index: 32, priority: 0}, {index: 31, priority: 1}, \
                     {index: 30, priority: 0}, {index: 27, priority: 1}, {index: 26, priority: 0}, \
                     {index: 25, priority: 1}, {index: 24, priority: 0}, {index: 23, priority: 1}, \
                     {index: 22, priority: 0}, {index: 21, priority: 1}, {index: 20, priority: 0}, \
                     {index: 19, priority: 1}, {index: 18, priority: 0}, {index: 17, priority: 1}, \
                     {index: 16, priority: 0}, {index: 15, priority: 1}, {index: 14, priority: 0}, \
                     {index: 13, priority: 1}, {index: 12, priority: 0}, {index: 11, priority: 1}, \
                     {index: 10, priority: 0}, {index: 9, priority: 1}, {index: 8, priority: 0}, \
                     {index: 7, priority: 1}, {index: 6, priority: 0}, {index: 5, priority: 1}, \
                     {index: 4, priority: 0}, {index: 3, priority: 1}, {index: 2, priority: 0}, \
                     {index: 1, priority: 1}, {index: 0, priority: 0}]",
            ),
            ("no game", "[]"),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_chosen_sprites_drawn_alone_hold() {
    let mut emu = held_capture_shapes(&EVERY_RECORDER);
    let mut some = [0u8; 30];
    for (slot, flag) in [(0, 1), (3, 7), (9, 1), (25, 255), (29, 1)] {
        some[slot] = flag;
    }
    let mut unrecorded = held_capture_shapes(&[]);
    let mut no_game = RetroEmulator::new("genesis").unwrap();
    assert_shapes(
        &[
            ("none chosen", emu.render_sprites(&[])),
            ("some chosen", emu.render_sprites(&some)),
            ("every slot chosen", emu.render_sprites(&[1; 80])),
            ("longer than the table", emu.render_sprites(&[1; 100])),
            ("nothing recorded", unrecorded.render_sprites(&[1; 80])),
            ("no game", no_game.render_sprites(&[1; 80])),
        ],
        &[
            (
                "none chosen",
                "{data: Uint8Array(len 286720, fnv1a64 0xe97846eb7709a325), \
                     fromLineStates: true, height: 224, width: 320}",
            ),
            (
                "some chosen",
                "{data: Uint8Array(len 286720, fnv1a64 0x7fd870991a18c139), \
                     fromLineStates: true, height: 224, width: 320}",
            ),
            (
                "every slot chosen",
                "{data: Uint8Array(len 286720, fnv1a64 0xa2af1da2713f146f), \
                     fromLineStates: true, height: 224, width: 320}",
            ),
            (
                "longer than the table",
                "{data: Uint8Array(len 286720, fnv1a64 0xa2af1da2713f146f), \
                     fromLineStates: true, height: 224, width: 320}",
            ),
            (
                "nothing recorded",
                "{data: Uint8Array(len 286720, fnv1a64 0x295ae25c4132c021), \
                     fromLineStates: false, height: 224, width: 320}",
            ),
            ("no game", "null"),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_video_memory_sources_hold() {
    let recorded = held_capture_shapes(&EVERY_RECORDER);
    let unrecorded = held_capture_shapes(&[]);
    assert_shapes(
        &[
            ("recorded", recorded.get_video_memory_sources()),
            ("not recorded", unrecorded.get_video_memory_sources()),
        ],
        &[
            (
                "recorded",
                "{frames: Uint32Array(len 65536, fnv1a64 0x79b47c3c923896a5), \
                     origins: Uint32Array(len 65536, fnv1a64 0xf16512049df41557)}",
            ),
            ("not recorded", "null"),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_resolved_sources_hold() {
    let emu = held_capture_shapes(&EVERY_RECORDER);
    let no_game = RetroEmulator::new("genesis").unwrap();
    let addresses = [
        0x0000_1000,
        0x0000_1235,
        0x0007_FFFF,
        0x0008_0000,
        0x0020_0001,
        0x003F_FFFF,
        0x0040_0000,
        0x00A0_0000,
        0x00A1_3000,
        0x00C0_0000,
        0x00E0_0000,
        0x00FF_0000,
        0x00FF_FFFF,
        0x1200_1234,
        0xFFFF_0000,
    ];
    assert_shapes(
        &[
            ("a game", emu.resolve_sources(&addresses).into()),
            ("no address", emu.resolve_sources(&[]).into()),
            ("no game", no_game.resolve_sources(&addresses).into()),
        ],
        &[
            ("a game", "Int32Array(len 15, fnv1a64 0xb4c0be8f60bbbc95)"),
            (
                "no address",
                "Int32Array(len 0, fnv1a64 0xcbf29ce484222325)",
            ),
            ("no game", "Int32Array(len 15, fnv1a64 0xada63fd6c9a763c9)"),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_frame_writes_hold() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.load_rom(&capture_shapes_rom()).unwrap();
    emu.set_execution_mode("Running".to_string());
    emu.set_capture_recording(options(&EVERY_RECORDER));
    emu.run_frame();
    let first_frame = emu.take_frame_writes();
    for _ in 0..4 {
        emu.run_frame();
    }
    let _ = emu.take_frame_writes();
    emu.run_to_capture_boundary();
    let to_the_boundary = emu.take_frame_writes();
    let taken_again = emu.take_frame_writes();
    emu.run_to_capture_boundary();
    let boundary_to_boundary = emu.take_frame_writes();
    let mut unrecorded = held_capture_shapes(&[]);
    assert_shapes(
        &[
            ("the first frame", first_frame),
            ("to the boundary", to_the_boundary),
            ("taken again", taken_again),
            ("from one boundary to the next", boundary_to_boundary),
            ("not recorded", unrecorded.take_frame_writes()),
        ],
        &[
            (
                "the first frame",
                "{dropped: 0, records: Uint32Array(len 48, fnv1a64 0x3e2be344a8da7c51), \
                     wordsPerRecord: 8}",
            ),
            (
                "to the boundary",
                "{dropped: 0, records: Uint32Array(len 16, fnv1a64 0x3a4a1295ad40bb60), \
                     wordsPerRecord: 8}",
            ),
            (
                "taken again",
                "{dropped: 0, records: Uint32Array(len 0, fnv1a64 0xcbf29ce484222325), \
                     wordsPerRecord: 8}",
            ),
            (
                "from one boundary to the next",
                "{dropped: 0, records: Uint32Array(len 48, fnv1a64 0xb5d5d9292f29c7e0), \
                     wordsPerRecord: 8}",
            ),
            ("not recorded", "null"),
        ],
    );
}

#[wasm_bindgen_test]
fn when_the_core_is_at_a_capture_boundary() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    let mut found = vec![("no game", emu.at_capture_boundary().into())];
    emu.load_rom(&capture_shapes_rom()).unwrap();
    emu.set_execution_mode("Running".to_string());
    emu.run_frame();
    found.push(("after a frame", emu.at_capture_boundary().into()));
    emu.run_to_capture_boundary();
    found.push(("held there", emu.at_capture_boundary().into()));
    let _ = emu.render_layer(0);
    let _ = emu.get_pixel_sources(None);
    found.push(("after reading", emu.at_capture_boundary().into()));
    emu.run_to_capture_boundary();
    found.push(("at the next boundary", emu.at_capture_boundary().into()));
    emu.run_frame();
    found.push(("after the frame call", emu.at_capture_boundary().into()));
    emu.run_to_capture_boundary();
    let _ = emu.save_state();
    found.push(("after a save", emu.at_capture_boundary().into()));
    assert_shapes(
        &found,
        &[
            ("no game", "false"),
            ("after a frame", "false"),
            ("held there", "true"),
            ("after reading", "true"),
            ("at the next boundary", "true"),
            ("after the frame call", "false"),
            ("after a save", "false"),
        ],
    );
}

#[wasm_bindgen_test]
fn what_a_repaint_draws() {
    // `repaintFrame` returns nothing to JavaScript.
    let repaint: fn(&mut RetroEmulator) = RetroEmulator::repaint_frame;
    let mut recorded = held_capture_shapes(&EVERY_RECORDER);
    recorded.toggle_layer(0, false);
    repaint(&mut recorded);
    let mut unrecorded = held_capture_shapes(&[]);
    unrecorded.toggle_layer(1, false);
    repaint(&mut unrecorded);
    assert_shapes(
        &[
            ("plane A hidden", recorded.get_framebuffer_data().into()),
            (
                "plane B hidden, nothing recorded",
                unrecorded.get_framebuffer_data().into(),
            ),
        ],
        &[
            (
                "plane A hidden",
                "Uint8ClampedArray(len 286720, fnv1a64 0x478376a41bb9c42b)",
            ),
            (
                "plane B hidden, nothing recorded",
                "Uint8ClampedArray(len 286720, fnv1a64 0xf1074741c02b84f3)",
            ),
        ],
    );
}
