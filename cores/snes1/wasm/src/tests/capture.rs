use super::*;

#[wasm_bindgen_test]
fn the_frame_snapshot_is_a_plain_object() {
    let emu = RetroEmulator::new("snes").unwrap();
    let snapshot = emu.capture_frame_snapshot();
    assert_plain_object(&snapshot, "framebuffer");
    assert_plain_object(&property(&snapshot, "ppuSettings"), "bgmode");
    let registers = js_sys::Array::from(&property(&snapshot, "registers"));
    assert_plain_object(&registers.get(0), "name");
}

#[wasm_bindgen_test]
fn a_capture_boundary_snapshot_carries_copies_under_its_names() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&synth_lorom("CAPTURE")).unwrap();
    assert!(!emu.at_capture_boundary());
    emu.run_to_capture_boundary();
    assert!(emu.at_capture_boundary());
    assert_eq!(
        emu.get_frame_number(),
        0,
        "the held frame is not counted yet"
    );

    let snapshot = emu.capture_frame_snapshot();
    assert_eq!(property(&snapshot, "frame").as_f64(), Some(1.0));
    assert_eq!(property(&snapshot, "atBoundary").as_bool(), Some(true));
    let width = property(&snapshot, "width").as_f64().unwrap() as u32;
    let height = property(&snapshot, "height").as_f64().unwrap() as u32;
    assert_eq!((width, height), (256, 224));
    let bytes = |key: &str| -> js_sys::Uint8Array { property(&snapshot, key).dyn_into().unwrap() };
    assert_eq!(bytes("framebuffer").length(), width * height * 4);
    assert_eq!(bytes("vram").length(), 0x10000);
    assert_eq!(bytes("oam").length(), 0x220);
    let palette: js_sys::Uint16Array = property(&snapshot, "palette").dyn_into().unwrap();
    assert_eq!(palette.length(), 0x100);
    assert!(js_sys::Array::is_array(&property(&snapshot, "registers")));
    let settings = property(&snapshot, "ppuSettings");
    assert_eq!(property(&settings, "bgmode").as_f64(), Some(0.0));
    assert!(property(&settings, "sprite_tiles").is_object());

    // A save finishes the held frame first.
    emu.save_state();
    assert!(!emu.at_capture_boundary());
    assert_eq!(emu.get_frame_number(), 1);
}

#[wasm_bindgen_test]
fn a_layer_render_keys_what_the_layer_does_not_cover() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    // Display on over a red backdrop, and no layer drawn anywhere: the
    // composite is red, every layer image empty.
    emu.write_palette_memory(0, &[0x001F]);
    emu.write_memory_debug(0x2100, &[0x0F]);
    emu.repaint_frame();
    let image = emu.render_layer(0);
    assert_eq!(property(&image, "width").as_f64(), Some(256.0));
    let data: js_sys::Uint8Array = property(&image, "data").dyn_into().unwrap();
    assert!(data
        .to_vec()
        .iter()
        .skip(3)
        .step_by(4)
        .all(|&alpha| alpha == 0));
    assert_eq!(
        &emu.get_framebuffer_data().to_vec()[..4],
        &[0xFF, 0x00, 0x00, 0xFF],
        "the composite the host shows is left as it was"
    );
    assert!(emu.render_layer(5).is_null());
}

#[wasm_bindgen_test]
fn the_mode_breakdown_and_the_settings_read_the_flags_from_their_fields() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    // BGMODE: mode 1, BG3 lifted, 16x16 tiles on BG2 and BG4.
    emu.write_memory_debug(0x2105, &[0b1010_1001]);
    let breakdown = emu.get_ppu_mode_breakdown();
    assert_eq!(property(&breakdown, "mode").as_f64(), Some(1.0));
    assert_eq!(property(&breakdown, "bg3Priority").as_bool(), Some(true));
    let sizes: Vec<Option<bool>> = js_sys::Array::from(&property(&breakdown, "bgTileSize16"))
        .iter()
        .map(|size| size.as_bool())
        .collect();
    assert_eq!(
        sizes,
        vec![Some(false), Some(true), Some(false), Some(true)]
    );
    let settings = emu.get_ppu_settings();
    assert_eq!(property(&settings, "bg3_priority").as_bool(), Some(true));
}

#[wasm_bindgen_test]
fn decode_tiles_takes_the_mode7_source_and_16x16_blocks() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    let mode7 = emu.decode_tiles(1, 0, 0, 256, 8, 0);
    assert_eq!(property(&mode7, "width").as_f64(), Some(8.0));
    assert_eq!(property(&mode7, "height").as_f64(), Some(2048.0));
    assert!(
        emu.decode_tiles(1, 0, 0, 1, 4, 0).is_null(),
        "Mode 7 is 8bpp"
    );
    let blocks = emu.decode_tiles(0, 0, 0, 4, 4, 1);
    assert_eq!(property(&blocks, "width").as_f64(), Some(16.0));
    assert_eq!(property(&blocks, "height").as_f64(), Some(64.0));
}

/// The options object `setCaptureRecording` takes.
fn recording_options(entries: &[(&str, bool)]) -> JsValue {
    let options = js_sys::Object::new();
    for (key, on) in entries {
        js_sys::Reflect::set(&options, &JsValue::from_str(key), &JsValue::from_bool(*on)).unwrap();
    }
    options.into()
}

/// A machine running [`synth_lorom`] with the line table kept, the
/// display lit over a red backdrop, held at the capture boundary of its
/// second frame.
fn recording_a_red_screen() -> RetroEmulator {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&synth_lorom("LINE STATES")).unwrap();
    emu.set_capture_recording(recording_options(&[("lineStates", true)]));
    emu.write_palette_memory(0, &[0x001F]);
    emu.write_memory_debug(0x2100, &[0x0F]);
    emu.run_to_capture_boundary();
    emu.run_to_capture_boundary();
    emu
}

#[wasm_bindgen_test]
fn the_recording_switch_is_a_plain_object_and_a_key_left_out_keeps_its_value() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    let flags = |emu: &RetroEmulator| -> Vec<Option<bool>> {
        let switch = emu.get_capture_recording();
        assert_plain_object(&switch, "lineStates");
        retro_wasm::capture::RECORDING_KEYS
            .iter()
            .map(|key| property(&switch, key).as_bool())
            .collect()
    };
    assert_eq!(flags(&emu), vec![Some(false); 5], "all off at first");
    emu.set_capture_recording(recording_options(&[
        ("lineStates", true),
        ("frameWrites", true),
    ]));
    let on = vec![
        Some(true),
        Some(false),
        Some(false),
        Some(false),
        Some(true),
    ];
    assert_eq!(flags(&emu), on);
    emu.set_capture_recording(recording_options(&[]));
    assert_eq!(flags(&emu), on, "an empty object changes nothing");
    emu.set_capture_recording(JsValue::NULL);
    assert_eq!(flags(&emu), on, "nor does no object");
    emu.set_capture_recording(recording_options(&[("lineStates", false)]));
    let only_writes = vec![
        Some(false),
        Some(false),
        Some(false),
        Some(false),
        Some(true),
    ];
    assert_eq!(flags(&emu), only_writes);
    emu.load_rom(&synth_lorom("KEPT")).unwrap();
    emu.reset();
    assert_eq!(flags(&emu), only_writes, "a load and a reset keep it");
}

#[wasm_bindgen_test]
fn the_line_states_are_a_plain_object_of_copies() {
    let emu = recording_a_red_screen();
    let table = emu.get_line_states();
    assert_plain_object(&table, "values");
    assert_eq!(property(&table, "frame").as_f64(), Some(2.0));
    for key in ["fromFirstLine", "complete", "memoryExact"] {
        assert_eq!(property(&table, key).as_bool(), Some(true), "{key}");
    }
    assert_eq!(property(&table, "rowsPerLine").as_f64(), Some(1.0));
    assert_eq!(property(&table, "firstRowLine").as_f64(), Some(1.0));
    let columns = js_sys::Array::from(&property(&table, "columns"));
    assert_eq!(columns.get(0).as_string().as_deref(), Some("line"));
    let values: js_sys::Uint32Array = property(&table, "values").dyn_into().unwrap();
    assert_eq!(values.length(), columns.length() * 224);
    assert_eq!(values.get_index(0), 1, "the first row is line 1");
    values.set_index(0, 999);
    let again: js_sys::Uint32Array = property(&emu.get_line_states(), "values")
        .dyn_into()
        .unwrap();
    assert_eq!(again.get_index(0), 1, "a copy, not a view");
}

#[wasm_bindgen_test]
fn the_palettes_and_the_picture_come_from_the_line_states() {
    let mut emu = recording_a_red_screen();
    let palettes = emu.get_line_palettes();
    assert_plain_object(&palettes, "paletteOfLine");
    assert_eq!(
        property(&palettes, "coloursPerPalette").as_f64(),
        Some(256.0)
    );
    let colours: js_sys::Uint16Array = property(&palettes, "palettes").dyn_into().unwrap();
    assert_eq!(colours.length(), 256, "one palette for the whole frame");
    assert_eq!(colours.get_index(0), 0x001F);
    let of_line: js_sys::Uint16Array = property(&palettes, "paletteOfLine").dyn_into().unwrap();
    assert_eq!(of_line.length(), 224);
    assert!(of_line.to_vec().iter().all(|&palette| palette == 0));

    let picture = emu.picture_from_line_states();
    assert_plain_object(&picture, "data");
    assert_eq!(property(&picture, "width").as_f64(), Some(256.0));
    assert_eq!(property(&picture, "height").as_f64(), Some(224.0));
    let data: js_sys::Uint8Array = property(&picture, "data").dyn_into().unwrap();
    assert_eq!(data.to_vec(), emu.get_framebuffer_data().to_vec());
    assert_eq!(&data.to_vec()[..4], &[0xFF, 0x00, 0x00, 0xFF]);

    emu.set_capture_recording(recording_options(&[("lineStates", false)]));
    assert!(emu.get_line_states().is_null());
    assert!(emu.get_line_palettes().is_null());
    assert!(emu.picture_from_line_states().is_null());
}

#[wasm_bindgen_test]
fn a_layer_render_says_whether_it_drew_from_the_line_states() {
    let mut emu = recording_a_red_screen();
    let from_line_states =
        |emu: &mut RetroEmulator| property(&emu.render_layer(0), "fromLineStates").as_bool();
    assert_eq!(from_line_states(&mut emu), Some(true));
    // A register written by hand: the registers as they stand show it.
    emu.write_memory_debug(0x2105, &[0x01]);
    assert_eq!(from_line_states(&mut emu), Some(false));
    emu.run_to_capture_boundary();
    assert_eq!(from_line_states(&mut emu), Some(true));
    emu.set_capture_recording(recording_options(&[("lineStates", false)]));
    assert_eq!(from_line_states(&mut emu), Some(false));
}

/// The typed array under `key`, which must be a `T`.
fn typed<T: JsCast>(object: &JsValue, key: &str) -> T {
    property(object, key)
        .dyn_into()
        .unwrap_or_else(|_| panic!("{key} is not the typed array it should be"))
}

/// Whether `object` has a property `key`.
fn has(object: &JsValue, key: &str) -> bool {
    js_sys::Reflect::has(object, &JsValue::from_str(key)).unwrap()
}

#[wasm_bindgen_test]
fn the_pixel_sources_are_a_plain_object_of_copies() {
    let mut idle = RetroEmulator::new("snes").unwrap();
    assert!(
        idle.get_pixel_sources(None).is_null(),
        "no game, no sources"
    );

    let mut emu = recording_a_red_screen();
    let sources = emu.get_pixel_sources(None);
    assert_plain_object(&sources, "layer");
    assert_eq!(property(&sources, "frame").as_f64(), Some(2.0));
    assert_eq!(property(&sources, "width").as_f64(), Some(256.0));
    assert_eq!(property(&sources, "height").as_f64(), Some(224.0));
    assert_eq!(property(&sources, "fromLineStates").as_bool(), Some(true));
    assert_eq!(property(&sources, "matchesPicture").as_bool(), Some(true));
    let pixels = 256 * 224;
    // A red backdrop and no layer: the backdrop drew every pixel, no
    // colour math, no character, no cell.
    let layer: js_sys::Uint16Array = typed(&sources, "layer");
    assert_eq!(layer.length(), pixels);
    assert!(layer.to_vec().iter().all(|&value| value == 0x0070));
    let colour: js_sys::Uint16Array = typed(&sources, "colour");
    assert_eq!(colour.length(), pixels);
    let tile: js_sys::Uint32Array = typed(&sources, "tile");
    assert_eq!(tile.length(), pixels);
    let cell: js_sys::Uint16Array = typed(&sources, "cell");
    assert!(cell.to_vec().iter().all(|&value| value == 0xFFFF));
    assert!(!has(&sources, "subLayer") && !has(&sources, "subColour"));

    layer.set_index(0, 7);
    let again: js_sys::Uint16Array = typed(&emu.get_pixel_sources(None), "layer");
    assert_eq!(again.get_index(0), 0x0070, "a copy, not a view");

    // The layer and the sub screen alone.
    let some = emu.get_pixel_sources(Some(1 | 16));
    assert_plain_object(&some, "subLayer");
    let sub_layer: js_sys::Uint8Array = typed(&some, "subLayer");
    assert_eq!(sub_layer.length(), pixels);
    assert!(
        sub_layer.to_vec().iter().all(|&value| value == 7),
        "no colour math: the line built no sub screen"
    );
    let sub_colour: js_sys::Uint16Array = typed(&some, "subColour");
    assert_eq!(sub_colour.length(), pixels);
    let _: js_sys::Uint16Array = typed(&some, "layer");
    for key in ["colour", "tile", "cell"] {
        assert!(!has(&some, key), "{key} was not asked for");
    }
}

/// A machine running [`synth_lorom`] with the line table kept and the
/// sprites shown over a red backdrop: OAM is all zero, so all 128
/// sprites sit at (0, 0) as 8x8 sprites of character 0, which is
/// filled with colour 1 (green in sprite palette 0). Held at the
/// capture boundary of its second frame.
fn recording_sprites() -> RetroEmulator {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&synth_lorom("LINE SPRITES")).unwrap();
    emu.set_capture_recording(recording_options(&[("lineStates", true)]));
    emu.write_palette_memory(0, &[0x001F]);
    emu.write_palette_memory(129, &[0x03E0]);
    // Character 0, 4bpp: plane 0 set on every row, the rest clear.
    emu.write_vram(0, &[0x00FF; 8]);
    emu.write_memory_debug(0x212C, &[0x10]);
    emu.write_memory_debug(0x2100, &[0x0F]);
    emu.run_to_capture_boundary();
    emu.run_to_capture_boundary();
    emu
}

#[wasm_bindgen_test]
fn the_line_sprites_are_a_plain_object_of_copies() {
    let mut idle = RetroEmulator::new("snes").unwrap();
    assert!(idle.get_line_sprites().is_null(), "no game, no sprites");

    let mut emu = recording_sprites();
    let sprites = emu.get_line_sprites();
    assert_plain_object(&sprites, "drawnWidth");
    assert_eq!(property(&sprites, "frame").as_f64(), Some(2.0));
    assert_eq!(property(&sprites, "fromLineStates").as_bool(), Some(true));
    let lines: js_sys::Uint16Array = typed(&sprites, "lines");
    assert_eq!(lines.length(), 224 * 4);
    let slots: js_sys::Uint8Array = typed(&sprites, "slots");
    let fates: js_sys::Uint8Array = typed(&sprites, "fates");
    let x: js_sys::Int16Array = typed(&sprites, "x");
    let width: js_sys::Uint8Array = typed(&sprites, "width");
    let drawn: js_sys::Uint8Array = typed(&sprites, "drawnWidth");
    // Lines 1-8 meet all 128 sprites: 32 kept and drawn whole, 96 over
    // the range limit; the other lines meet none.
    let entries = 8 * 128;
    for column in [
        slots.length(),
        fates.length(),
        x.length(),
        width.length(),
        drawn.length(),
    ] {
        assert_eq!(column, entries);
    }
    let lines = lines.to_vec();
    assert_eq!(&lines[..4], &[1, 0, 128, 1], "line 1, over range");
    assert_eq!(&lines[8 * 4..9 * 4], &[9, entries as u16, 0, 0]);
    let fates = fates.to_vec();
    assert!(fates[..32].iter().all(|&fate| fate == 0));
    assert!(fates[32..128].iter().all(|&fate| fate == 3));
    assert_eq!(&slots.to_vec()[..3], &[0, 1, 2]);
    assert!(drawn.to_vec()[..32].iter().all(|&pixels| pixels == 8));
    assert!(width.to_vec().iter().all(|&pixels| pixels == 8));

    x.set_index(0, 99);
    let again: js_sys::Int16Array = typed(&emu.get_line_sprites(), "x");
    assert_eq!(again.get_index(0), 0, "a copy, not a view");
}

#[wasm_bindgen_test]
fn a_sprite_render_draws_the_chosen_sprites_after_priority() {
    let mut idle = RetroEmulator::new("snes").unwrap();
    assert!(idle.render_sprites(&[1]).is_null(), "no game, no render");

    let mut emu = recording_sprites();
    let alpha_at = |image: &JsValue, x: u32, y: u32| -> u8 {
        let data: js_sys::Uint8Array = typed(image, "data");
        data.get_index((y * 256 + x) * 4 + 3)
    };
    let every = emu.render_sprites(&[1u8; 128]);
    assert_plain_object(&every, "fromLineStates");
    assert_eq!(property(&every, "width").as_f64(), Some(256.0));
    assert_eq!(property(&every, "height").as_f64(), Some(224.0));
    assert_eq!(property(&every, "fromLineStates").as_bool(), Some(true));
    let data: js_sys::Uint8Array = typed(&every, "data");
    assert_eq!(data.length(), 256 * 224 * 4);
    assert_eq!(alpha_at(&every, 0, 0), 0xFF);
    assert_eq!(alpha_at(&every, 8, 0), 0);
    assert_eq!(alpha_at(&every, 0, 8), 0);
    // Slot 0 is in front of the others at (0, 0): chosen alone it
    // shows; slot 1 alone shows nothing, being behind slot 0.
    assert_eq!(alpha_at(&emu.render_sprites(&[1]), 0, 0), 0xFF);
    assert_eq!(alpha_at(&emu.render_sprites(&[0, 1]), 0, 0), 0);
    assert_eq!(alpha_at(&emu.render_sprites(&[]), 0, 0), 0);
}

#[wasm_bindgen_test]
fn the_window_masks_are_a_plain_object_of_copies() {
    let idle = RetroEmulator::new("snes").unwrap();
    assert!(idle.get_window_masks().is_null(), "no game, no masks");

    let emu = recording_a_red_screen();
    let masks = emu.get_window_masks();
    assert_plain_object(&masks, "masks");
    assert_eq!(property(&masks, "frame").as_f64(), Some(2.0));
    assert_eq!(property(&masks, "fromLineStates").as_bool(), Some(true));
    let lines: js_sys::Uint16Array = typed(&masks, "lines");
    assert_eq!(lines.length(), 224);
    assert_eq!(lines.get_index(0), 1);
    let bytes: js_sys::Uint8Array = typed(&masks, "masks");
    assert_eq!(bytes.length(), 224 * 8 * 32);
    // No window on: the six regions are empty; the main screen is kept
    // and colour math allowed everywhere.
    let line = bytes.to_vec()[..256].to_vec();
    assert!(line[..6 * 32].iter().all(|&byte| byte == 0));
    assert!(line[6 * 32..].iter().all(|&byte| byte == 0xFF));
}

#[wasm_bindgen_test]
fn the_offset_per_tile_columns_are_a_plain_object_of_copies() {
    let idle = RetroEmulator::new("snes").unwrap();
    assert!(idle.get_offset_per_tile().is_null(), "no game, no columns");

    let mut emu = recording_a_red_screen();
    let table = emu.get_offset_per_tile();
    assert_plain_object(&table, "replaced");
    assert_eq!(property(&table, "fromLineStates").as_bool(), Some(true));
    assert_eq!(property(&table, "columns").as_f64(), Some(33.0));
    let lines: js_sys::Uint16Array = typed(&table, "lines");
    assert_eq!(lines.length(), 0, "Mode 0 has no offset-per-tile");

    // Mode 2 from the next frame on.
    emu.write_memory_debug(0x2105, &[0x02]);
    emu.run_to_capture_boundary();
    let table = emu.get_offset_per_tile();
    assert_eq!(property(&table, "frame").as_f64(), Some(3.0));
    let lines: js_sys::Uint16Array = typed(&table, "lines");
    assert_eq!(lines.length(), 224);
    let scroll: js_sys::Uint16Array = typed(&table, "scroll");
    assert_eq!(scroll.length(), 224 * 2 * 33 * 2);
    let replaced: js_sys::Uint8Array = typed(&table, "replaced");
    assert_eq!(replaced.length(), 224 * 2 * 33);
    // No scroll: column 32 starts at dot 256, past the line.
    assert_eq!(replaced.get_index(32), 0x80);
    assert_eq!(replaced.get_index(0), 0);
}

#[wasm_bindgen_test]
fn the_video_memory_sources_are_a_plain_object_of_copies() {
    let idle = RetroEmulator::new("snes").unwrap();
    assert!(
        idle.get_video_memory_sources().is_null(),
        "not recorded, no sources"
    );

    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&synth_lorom("VRAM SOURCES")).unwrap();
    emu.set_capture_recording(recording_options(&[("videoMemorySources", true)]));
    assert_eq!(
        property(&emu.get_capture_recording(), "videoMemorySources").as_bool(),
        Some(true)
    );
    let sources = emu.get_video_memory_sources();
    assert_plain_object(&sources, "origins");
    let origins: js_sys::Uint32Array = typed(&sources, "origins");
    let frames: js_sys::Uint32Array = typed(&sources, "frames");
    assert_eq!(origins.length(), 0x1_0000);
    assert_eq!(frames.length(), 0x1_0000);
    assert!(
        origins.to_vec().iter().all(|&origin| origin == 0),
        "empty when turned on"
    );

    // A debugger write straight into VRAM, then one through the port
    // (the reset state is forced blank: the port is open).
    emu.write_vram(0x10, &[1, 2]);
    emu.write_memory_debug(0x2115, &[0x80]);
    emu.write_memory_debug(0x2116, &[0x00, 0x10]);
    emu.write_memory_debug(0x2118, &[0xAA]);
    let sources = emu.get_video_memory_sources();
    let origins: js_sys::Uint32Array = typed(&sources, "origins");
    let frames: js_sys::Uint32Array = typed(&sources, "frames");
    for byte in [0x20, 0x21, 0x2000] {
        assert_eq!(
            origins.get_index(byte),
            0x4000_0000,
            "byte {byte:#x}: the debugger"
        );
        assert_eq!(frames.get_index(byte), 1, "byte {byte:#x}: frame 1");
    }
    assert_eq!(origins.get_index(0x2001), 0, "not written");

    origins.set_index(0x20, 7);
    let again: js_sys::Uint32Array = typed(&emu.get_video_memory_sources(), "origins");
    assert_eq!(again.get_index(0x20), 0x4000_0000, "a copy, not a view");
}

#[wasm_bindgen_test]
fn resolving_sources_answers_an_int32_array_of_rom_offsets_and_codes() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&synth_lorom("RESOLVE SOURCES")).unwrap();
    let addresses = [
        0x00_8000u32,
        0x81_8123,
        0x7E_0040,
        0x00_1FFF,
        0x00_2100,
        0x40_0000,
    ];
    let resolved = emu.resolve_sources(&addresses);
    let value: JsValue = resolved.clone().into();
    assert!(value.is_instance_of::<js_sys::Int32Array>());
    assert_eq!(
        resolved.to_vec(),
        vec![0, 0x8123, -2, -2, -1, -1],
        "ROM, its mirror, work RAM, its mirror, the B bus, open bus"
    );
    resolved.set_index(0, 99);
    assert_eq!(emu.resolve_sources(&addresses).get_index(0), 0, "a copy");
    assert_eq!(emu.resolve_sources(&[]).length(), 0);
}

#[wasm_bindgen_test]
fn the_frame_writes_are_a_plain_object_of_copies_emptied_by_a_take() {
    let mut idle = RetroEmulator::new("snes").unwrap();
    assert!(
        idle.take_frame_writes().is_null(),
        "not recorded, no writes"
    );

    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&synth_lorom("FRAME WRITES")).unwrap();
    emu.set_capture_recording(recording_options(&[("frameWrites", true)]));
    // Two colours from work RAM into CGRAM by a DMA on channel 0, run at
    // the end of the next instruction.
    emu.write_memory_debug(0x7E_0000, &[0x1F, 0x00, 0xE0, 0x03]);
    emu.write_memory_debug(0x2121, &[0x00]);
    emu.write_memory_debug(0x4300, &[0x00, 0x22, 0x00, 0x00, 0x7E, 0x04, 0x00]);
    emu.write_memory_debug(0x420B, &[0x01]);
    let _ = emu.step();

    let writes = emu.take_frame_writes();
    assert_plain_object(&writes, "records");
    assert_eq!(property(&writes, "wordsPerRecord").as_f64(), Some(8.0));
    assert_eq!(property(&writes, "dropped").as_f64(), Some(0.0));
    let records: js_sys::Uint32Array = typed(&writes, "records");
    assert_eq!(records.length(), 8, "one DMA block");
    let words = records.to_vec();
    assert_eq!(words[0], 1, "a DMA block on channel 0, A to B, unit 0");
    assert_eq!(words[1], 1, "frame 1");
    assert_eq!(words[3], 0x7E_0000);
    assert_eq!(words[4], 0x22 | (4 << 8));
    assert_eq!((words[5], words[6]), (0, 1), "colours 0 to 1");

    records.set_index(0, 0);
    let again = emu.take_frame_writes();
    let records: js_sys::Uint32Array = typed(&again, "records");
    assert_eq!(records.length(), 0, "a take empties the log");
}

#[wasm_bindgen_test]
fn the_event_catalogue_lists_the_video_memory_events() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    let events = js_sys::Array::from(&emu.get_available_events());
    let codes: Vec<f64> = events
        .iter()
        .map(|event| property(&event, "code").as_f64().unwrap())
        .collect();
    for code in [10.0, 11.0, 12.0] {
        assert!(codes.contains(&code), "event {code} listed");
    }
}

#[wasm_bindgen_test]
fn write_oam_reads_back_and_moves_a_sprite() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    // The last byte of the high table, then on round to sprite 0's X.
    emu.write_oam(543, &[0xAA, 100]);
    let oam = emu.get_oam();
    assert_eq!(oam[543], 0xAA);
    assert_eq!(oam[0], 100);

    let sprites: Vec<retro_core::video::SpriteData> =
        serde_wasm_bindgen::from_value(emu.get_sprite_data()).unwrap();
    assert_eq!(sprites[0].x, 100, "sprite 0 is at x 100");

    let long: Vec<u8> = (0..600).map(|i| if i < 544 { 1 } else { 2 }).collect();
    emu.write_oam(10, &long);
    assert!(
        emu.get_oam().iter().all(|&byte| byte == 1),
        "a buffer longer than OAM is cut to it"
    );
}

/// The few 65C816 instructions the capture program needs, assembled in
/// emulation mode, the code starting at `$00:8000`.
#[derive(Default)]
struct Code(Vec<u8>);

impl Code {
    /// The address of the next instruction.
    fn here(&self) -> u16 {
        0x8000 + self.0.len() as u16
    }

    /// Append `bytes` as they are.
    fn op(&mut self, bytes: &[u8]) -> &mut Self {
        self.0.extend_from_slice(bytes);
        self
    }

    /// `LDA #value`, `STA address`.
    fn store(&mut self, address: u16, value: u8) -> &mut Self {
        let [low, high] = address.to_le_bytes();
        self.op(&[0xA9, value, 0x8D, low, high])
    }

    /// `STZ address`.
    fn zero(&mut self, address: u16) -> &mut Self {
        let [low, high] = address.to_le_bytes();
        self.op(&[0x9C, low, high])
    }

    /// `LDA from`, `STA to`.
    fn copy(&mut self, from: u16, to: u16) -> &mut Self {
        let [from_low, from_high] = from.to_le_bytes();
        let [to_low, to_high] = to.to_le_bytes();
        self.op(&[0xAD, from_low, from_high, 0x8D, to_low, to_high])
    }

    /// A DMA channel's A-bus address, `$00:address`, into the three
    /// registers from `register` on.
    fn source(&mut self, register: u16, address: u16) -> &mut Self {
        let [low, high] = address.to_le_bytes();
        self.store(register, low)
            .store(register + 1, high)
            .zero(register + 2)
    }

    /// A branch, `opcode`, to `target`.
    fn branch(&mut self, opcode: u8, target: u16) -> &mut Self {
        let offset = i32::from(target) - i32::from(self.here()) - 2;
        self.op(&[opcode, offset as i8 as u8])
    }
}

/// Where the capture program's tables sit in bank `$00`.
const PROGRAM_PALETTE: u16 = 0x9000;
const PROGRAM_CHARACTERS: u16 = 0x9020;
const PROGRAM_MAP: u16 = 0x9060;
const PROGRAM_SCROLLS: u16 = 0x90A0;

/// A program that draws, moves a sprite and writes video memory. It
/// loads sixteen colours (for the backgrounds, then for the sprites), two
/// characters and the first row of BG1's map by DMA, shows BG1 in Mode 1
/// and the sprites, and scrolls BG1 line by line by HDMA (32 lines at 4,
/// then 16 lines at 0, 2, ..., 30). In each vertical blank it counts a
/// frame in work RAM and, through the ports, moves sprite 0 to that x,
/// writes the count into the map entry at word `$0420` and into colour 2.
pub(super) fn capture_program_rom() -> Vec<u8> {
    let mut code = Code::default();
    code.op(&[0x78]); // SEI
    code.zero(0x2121).store(0x4300, 0x00).store(0x4301, 0x22);
    code.source(0x4302, PROGRAM_PALETTE)
        .store(0x4305, 32)
        .zero(0x4306)
        .store(0x420B, 0x01);
    code.store(0x2121, 0x80)
        .source(0x4302, PROGRAM_PALETTE)
        .store(0x4305, 32)
        .store(0x420B, 0x01);
    code.store(0x2115, 0x80).zero(0x2116).zero(0x2117);
    code.store(0x4300, 0x01).store(0x4301, 0x18);
    code.source(0x4302, PROGRAM_CHARACTERS)
        .store(0x4305, 64)
        .store(0x420B, 0x01);
    code.zero(0x2116).store(0x2117, 0x04);
    code.source(0x4302, PROGRAM_MAP)
        .store(0x4305, 64)
        .store(0x420B, 0x01);
    code.store(0x2105, 0x01)
        .store(0x2107, 0x04)
        .zero(0x210B)
        .store(0x212C, 0x11)
        .zero(0x2101);
    // Sprite 0 at (16, 32), character 1, priority 3.
    code.zero(0x2102).zero(0x2103);
    code.store(0x2104, 16)
        .store(0x2104, 32)
        .store(0x2104, 0x01)
        .store(0x2104, 0x30);
    code.store(0x4310, 0x02)
        .store(0x4311, 0x0D)
        .source(0x4312, PROGRAM_SCROLLS)
        .store(0x420C, 0x02);
    code.store(0x2100, 0x0F);
    let wait_for_blank = code.here();
    code.op(&[0xAD, 0x12, 0x42]).branch(0x10, wait_for_blank); // LDA $4212, BPL
    code.op(&[0xEE, 0x00, 0x00]); // INC $0000
    code.zero(0x2102)
        .zero(0x2103)
        .copy(0x0000, 0x2104)
        .store(0x2104, 32);
    code.store(0x2116, 0x20)
        .store(0x2117, 0x04)
        .copy(0x0000, 0x2118)
        .zero(0x2119);
    code.store(0x2121, 0x02).copy(0x0000, 0x2122).zero(0x2122);
    let wait_for_lines = code.here();
    code.op(&[0xAD, 0x12, 0x42]).branch(0x30, wait_for_lines); // LDA $4212, BMI
    code.branch(0x80, wait_for_blank); // BRA

    let palette: Vec<u8> = [
        0x2800u16, 0x03FF, 0x7C1F, 0x001F, 0x03E0, 0x7C00, 0x7FFF, 0x0421, 0x0842, 0x0C63, 0x1084,
        0x14A5, 0x18C6, 0x1CE7, 0x2108, 0x2529,
    ]
    .iter()
    .flat_map(|colour| colour.to_le_bytes())
    .collect();
    let mut characters = [0u8; 64];
    for row in 0..8 {
        characters[2 * row] = 0xFF >> row;
        characters[2 * row + 1] = 1 << row;
        characters[32 + 2 * row] = if row % 2 == 0 { 0xAA } else { 0x55 };
        characters[32 + 2 * row + 1] = 0xF0;
        characters[48 + 2 * row] = 0x0F;
    }
    let map: Vec<u8> = (0u16..32)
        .flat_map(|entry| {
            let word = (entry & 1)
                | (((entry >> 1) & 7) << 10)
                | (((entry >> 2) & 1) << 13)
                | (((entry >> 3) & 1) << 14)
                | (((entry >> 4) & 1) << 15);
            word.to_le_bytes()
        })
        .collect();
    let mut scrolls = vec![0x20, 0x04, 0x00, 0x90];
    for line in 0..16u8 {
        scrolls.extend([2 * line, 0x00]);
    }
    scrolls.push(0x00);
    lorom_running(
        "CAPTURE PROGRAM",
        &code.0,
        &[
            (PROGRAM_PALETTE, &palette),
            (PROGRAM_CHARACTERS, &characters),
            (PROGRAM_MAP, &map),
            (PROGRAM_SCROLLS, &scrolls),
        ],
    )
}

/// The capture program with every recorder armed before its first frame,
/// held at the capture boundary of its third frame.
fn running_the_capture_program() -> RetroEmulator {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&capture_program_rom()).unwrap();
    emu.set_capture_recording(recording_options(&[
        ("lineStates", true),
        ("pixelSources", true),
        ("lineSprites", true),
        ("videoMemorySources", true),
        ("frameWrites", true),
    ]));
    for _ in 0..3 {
        emu.run_to_capture_boundary();
    }
    emu
}

#[wasm_bindgen_test]
fn rendered_frames_leave_the_last_one_s_picture() {
    let mut played = RetroEmulator::new("snes").unwrap();
    played.load_rom(&capture_program_rom()).unwrap();
    played.set_audio_enabled(true);
    let mut rendered = RetroEmulator::new("snes").unwrap();
    rendered.load_rom(&capture_program_rom()).unwrap();
    let before = rendered.get_framebuffer_data().to_vec();
    for _ in 0..3 {
        played.run_frame();
    }
    rendered.render_frames(3);
    let picture = rendered.get_framebuffer_data().to_vec();
    assert_ne!(picture, before, "the frames drew a picture");
    assert_eq!(
        picture,
        played.get_framebuffer_data().to_vec(),
        "the picture is the third frame's, as three frame calls leave it"
    );
}

#[wasm_bindgen_test]
fn the_capture_boundary_reads_as_before() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&capture_program_rom()).unwrap();
    let mut held = vec![JsValue::from_bool(emu.at_capture_boundary())];
    emu.run_to_capture_boundary();
    held.push(JsValue::from_bool(emu.at_capture_boundary()));
    emu.run_frame();
    held.push(JsValue::from_bool(emu.at_capture_boundary()));
    let expected = ["false", "true", "false"];
    assert_eq!(shapes(&held), expected);
}

#[wasm_bindgen_test]
fn the_frame_snapshot_reads_as_before() {
    let idle = RetroEmulator::new("snes").unwrap();
    let emu = running_the_capture_program();
    let expected = [
        concat!(
            "{atBoundary: false, frame: 0, framebuffer: Uint8Array(len 229376, ",
            "fnv1a64 0xc9b1e505f9a82325), grid: {columns_per_dot: 1, rows_per_line: 1, ",
            "woven: false}, height: 224, oam: Uint8Array(len 544, ",
            "fnv1a64 0x9986ba9af2021da5), palette: Uint16Array(len 256, ",
            "fnv1a64 0x7da144b97d054b25), ppuSettings: {addsub_mode: false, ",
            "bg3_priority: false, bg_main_enable: [false, false, false, false], ",
            "bg_main_window_disable: [false, false, false, false], ",
            "bg_mosaic_enable: [false, false, false, false], bg_sub_enable: [false, ",
            "false, false, false], bg_sub_window_disable: [false, false, false, ",
            "false], bg_tile_size: [false, false, false, false], bgmode: 0, ",
            "col_main_mask: 0, col_sub_mask: 0, color_math_back_enable: false, ",
            "color_math_bg_enable: [false, false, false, false], ",
            "color_math_halve: false, color_math_oam_enable: false, ",
            "color_math_subtract: false, direct_color: false, display_brightness: 0, ",
            "display_disable: true, field: false, fixed_color: 0, interlace: false, ",
            "mode7_centre: [0, 0], mode7_extbg: false, mode7_hflip: false, ",
            "mode7_hoffset: 0, mode7_matrix: [0, 0, 0, 0], mode7_repeat: 0, ",
            "mode7_vflip: false, mode7_voffset: 0, mosaic_size: 1, obj_base_size: 0, ",
            "obj_interlace: false, obj_main_enable: false, ",
            "obj_main_window_disable: false, obj_name_select: 0, ",
            "obj_sub_enable: false, obj_sub_window_disable: false, ",
            "obj_tiledata_addr: 0, overscan: false, pseudo_hires: false, ",
            "sprite_tiles: {first_table_base_word: 0, large_height: 16, ",
            "large_width: 16, second_table_base_word: 4096, small_height: 8, ",
            "small_width: 8}, window_combine: [0, 0, 0, 0, 0, 0], ",
            "window_enable: [{w1_enable: false, w1_invert: false, w2_enable: false, ",
            "w2_invert: false}, {w1_enable: false, w1_invert: false, ",
            "w2_enable: false, w2_invert: false}, {w1_enable: false, ",
            "w1_invert: false, w2_enable: false, w2_invert: false}, ",
            "{w1_enable: false, w1_invert: false, w2_enable: false, ",
            "w2_invert: false}, {w1_enable: false, w1_invert: false, ",
            "w2_enable: false, w2_invert: false}, {w1_enable: false, ",
            "w1_invert: false, w2_enable: false, w2_invert: false}], ",
            "window_one_left: 0, window_one_right: 0, window_two_left: 0, ",
            "window_two_right: 0}, registers: [{description: \"BG mode + tile size\", ",
            "index: 8453, name: \"BGMODE\", value: 0}, ",
            "{description: \"Main screen designation\", index: 8492, name: \"TM\", ",
            "value: 0}, {description: \"Sub screen designation\", index: 8493, ",
            "name: \"TS\", value: 0}, {description: \"Mode 7 settings\", index: 8474, ",
            "name: \"M7SEL\", value: 0}, {description: \"Mode 7 matrix A\", index: 8475, ",
            "name: \"M7A\", value: 0}, {description: \"Mode 7 matrix B\", index: 8476, ",
            "name: \"M7B\", value: 0}, {description: \"Mode 7 matrix C\", index: 8477, ",
            "name: \"M7C\", value: 0}, {description: \"Mode 7 matrix D\", index: 8478, ",
            "name: \"M7D\", value: 0}, {description: \"Mode 7 centre X\", index: 8479, ",
            "name: \"M7X\", value: 0}, {description: \"Mode 7 centre Y\", index: 8480, ",
            "name: \"M7Y\", value: 0}, {description: \"BG1 horizontal scroll\", ",
            "index: 8461, name: \"BG1HOFS\", value: 0}, ",
            "{description: \"BG1 vertical scroll\", index: 8462, name: \"BG1VOFS\", ",
            "value: 0}], vram: Uint8Array(len 65536, fnv1a64 0xeb05052ea5b62325), ",
            "width: 256}",
        ),
        concat!(
            "{atBoundary: true, frame: 3, framebuffer: Uint8Array(len 229376, ",
            "fnv1a64 0x75f379eed654dafa), grid: {columns_per_dot: 1, rows_per_line: 1, ",
            "woven: false}, height: 224, oam: Uint8Array(len 544, ",
            "fnv1a64 0xb3c3b956cfe107de), palette: Uint16Array(len 256, ",
            "fnv1a64 0x82bce97466c89cfc), ppuSettings: {addsub_mode: false, ",
            "bg3_priority: false, bg_main_enable: [true, false, false, false], ",
            "bg_main_window_disable: [false, false, false, false], ",
            "bg_mosaic_enable: [false, false, false, false], bg_sub_enable: [false, ",
            "false, false, false], bg_sub_window_disable: [false, false, false, ",
            "false], bg_tile_size: [false, false, false, false], bgmode: 1, ",
            "col_main_mask: 0, col_sub_mask: 0, color_math_back_enable: false, ",
            "color_math_bg_enable: [false, false, false, false], ",
            "color_math_halve: false, color_math_oam_enable: false, ",
            "color_math_subtract: false, direct_color: false, ",
            "display_brightness: 15, display_disable: false, field: false, ",
            "fixed_color: 0, interlace: false, mode7_centre: [0, 0], ",
            "mode7_extbg: false, mode7_hflip: false, mode7_hoffset: 30, ",
            "mode7_matrix: [0, 0, 0, 0], mode7_repeat: 0, mode7_vflip: false, ",
            "mode7_voffset: 0, mosaic_size: 1, obj_base_size: 0, ",
            "obj_interlace: false, obj_main_enable: true, ",
            "obj_main_window_disable: false, obj_name_select: 0, ",
            "obj_sub_enable: false, obj_sub_window_disable: false, ",
            "obj_tiledata_addr: 0, overscan: false, pseudo_hires: false, ",
            "sprite_tiles: {first_table_base_word: 0, large_height: 16, ",
            "large_width: 16, second_table_base_word: 4096, small_height: 8, ",
            "small_width: 8}, window_combine: [0, 0, 0, 0, 0, 0], ",
            "window_enable: [{w1_enable: false, w1_invert: false, w2_enable: false, ",
            "w2_invert: false}, {w1_enable: false, w1_invert: false, ",
            "w2_enable: false, w2_invert: false}, {w1_enable: false, ",
            "w1_invert: false, w2_enable: false, w2_invert: false}, ",
            "{w1_enable: false, w1_invert: false, w2_enable: false, ",
            "w2_invert: false}, {w1_enable: false, w1_invert: false, ",
            "w2_enable: false, w2_invert: false}, {w1_enable: false, ",
            "w1_invert: false, w2_enable: false, w2_invert: false}], ",
            "window_one_left: 0, window_one_right: 0, window_two_left: 0, ",
            "window_two_right: 0}, registers: [{description: \"BG mode + tile size\", ",
            "index: 8453, name: \"BGMODE\", value: 1}, ",
            "{description: \"Main screen designation\", index: 8492, name: \"TM\", ",
            "value: 17}, {description: \"Sub screen designation\", index: 8493, ",
            "name: \"TS\", value: 0}, {description: \"Mode 7 settings\", index: 8474, ",
            "name: \"M7SEL\", value: 0}, {description: \"Mode 7 matrix A\", index: 8475, ",
            "name: \"M7A\", value: 0}, {description: \"Mode 7 matrix B\", index: 8476, ",
            "name: \"M7B\", value: 0}, {description: \"Mode 7 matrix C\", index: 8477, ",
            "name: \"M7C\", value: 0}, {description: \"Mode 7 matrix D\", index: 8478, ",
            "name: \"M7D\", value: 0}, {description: \"Mode 7 centre X\", index: 8479, ",
            "name: \"M7X\", value: 0}, {description: \"Mode 7 centre Y\", index: 8480, ",
            "name: \"M7Y\", value: 0}, {description: \"BG1 horizontal scroll\", ",
            "index: 8461, name: \"BG1HOFS\", value: 30}, ",
            "{description: \"BG1 vertical scroll\", index: 8462, name: \"BG1VOFS\", ",
            "value: 0}], vram: Uint8Array(len 65536, fnv1a64 0x15727000ffe79428), ",
            "width: 256}",
        ),
    ];
    assert_eq!(
        shapes(&[idle.capture_frame_snapshot(), emu.capture_frame_snapshot()]),
        expected
    );
}

#[wasm_bindgen_test]
fn the_layer_renders_read_as_before() {
    let mut emu = running_the_capture_program();
    let layers: Vec<JsValue> = (0..=5).map(|layer| emu.render_layer(layer)).collect();
    let expected = [
        concat!(
            "{data: Uint8Array(len 229376, fnv1a64 0xc995b76295b606ea), ",
            "fromLineStates: true, height: 224, width: 256}",
        ),
        concat!(
            "{data: Uint8Array(len 229376, fnv1a64 0xb293664dc5b90c09), ",
            "fromLineStates: true, height: 224, width: 256}",
        ),
        concat!(
            "{data: Uint8Array(len 229376, fnv1a64 0x85068eaa1516ba85), ",
            "fromLineStates: true, height: 224, width: 256}",
        ),
        concat!(
            "{data: Uint8Array(len 229376, fnv1a64 0xc9b1e505f9a82325), ",
            "fromLineStates: true, height: 224, width: 256}",
        ),
        concat!(
            "{data: Uint8Array(len 229376, fnv1a64 0x59b321ddc659a405), ",
            "fromLineStates: true, height: 224, width: 256}",
        ),
        "null",
    ];
    assert_eq!(shapes(&layers), expected);
}

#[wasm_bindgen_test]
fn the_recording_reads_as_before() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    let mut read = vec![emu.get_capture_recording()];
    emu.set_capture_recording(recording_options(&[
        ("lineStates", true),
        ("videoMemorySources", true),
    ]));
    read.push(emu.get_capture_recording());
    emu.set_capture_recording(recording_options(&[
        ("lineStates", false),
        ("frameWrites", true),
    ]));
    read.push(emu.get_capture_recording());
    let expected = [
        concat!(
            "{frameWrites: false, lineSprites: false, lineStates: false, ",
            "pixelSources: false, videoMemorySources: false}",
        ),
        concat!(
            "{frameWrites: false, lineSprites: false, lineStates: true, ",
            "pixelSources: false, videoMemorySources: true}",
        ),
        concat!(
            "{frameWrites: true, lineSprites: false, lineStates: false, ",
            "pixelSources: false, videoMemorySources: true}",
        ),
    ];
    assert_eq!(shapes(&read), expected);
}

#[wasm_bindgen_test]
fn the_line_states_read_as_before() {
    let idle = RetroEmulator::new("snes").unwrap();
    let emu = running_the_capture_program();
    let expected = [
        "null",
        concat!(
            "{columns: [\"line\", \"display_disable\", \"display_brightness\", ",
            "\"obj_base_size\", \"obj_name_select\", \"obj_tiledata_addr\", ",
            "\"oam_first_sprite\", \"bgmode\", \"bg3_priority\", \"bg_tile_size[0]\", ",
            "\"bg_tile_size[1]\", \"bg_tile_size[2]\", \"bg_tile_size[3]\", \"mosaic_size\", ",
            "\"bg_mosaic_enable[0]\", \"bg_mosaic_enable[1]\", \"bg_mosaic_enable[2]\", ",
            "\"bg_mosaic_enable[3]\", \"bg_screen_addr[0]\", \"bg_screen_addr[1]\", ",
            "\"bg_screen_addr[2]\", \"bg_screen_addr[3]\", \"bg_screen_size[0]\", ",
            "\"bg_screen_size[1]\", \"bg_screen_size[2]\", \"bg_screen_size[3]\", ",
            "\"bg_tiledata_addr[0]\", \"bg_tiledata_addr[1]\", \"bg_tiledata_addr[2]\", ",
            "\"bg_tiledata_addr[3]\", \"bg_hoffset[0]\", \"bg_hoffset[1]\", ",
            "\"bg_hoffset[2]\", \"bg_hoffset[3]\", \"bg_voffset[0]\", \"bg_voffset[1]\", ",
            "\"bg_voffset[2]\", \"bg_voffset[3]\", \"mode7_repeat\", \"mode7_vflip\", ",
            "\"mode7_hflip\", \"mode7_matrix_a\", \"mode7_matrix_b\", \"mode7_matrix_c\", ",
            "\"mode7_matrix_d\", \"mode7_centre_x\", \"mode7_centre_y\", \"mode7_hoffset\", ",
            "\"mode7_voffset\", \"mode7_extbg\", \"window_enable[0].w1_enable\", ",
            "\"window_enable[0].w1_invert\", \"window_enable[0].w2_enable\", ",
            "\"window_enable[0].w2_invert\", \"window_enable[1].w1_enable\", ",
            "\"window_enable[1].w1_invert\", \"window_enable[1].w2_enable\", ",
            "\"window_enable[1].w2_invert\", \"window_enable[2].w1_enable\", ",
            "\"window_enable[2].w1_invert\", \"window_enable[2].w2_enable\", ",
            "\"window_enable[2].w2_invert\", \"window_enable[3].w1_enable\", ",
            "\"window_enable[3].w1_invert\", \"window_enable[3].w2_enable\", ",
            "\"window_enable[3].w2_invert\", \"window_enable[4].w1_enable\", ",
            "\"window_enable[4].w1_invert\", \"window_enable[4].w2_enable\", ",
            "\"window_enable[4].w2_invert\", \"window_enable[5].w1_enable\", ",
            "\"window_enable[5].w1_invert\", \"window_enable[5].w2_enable\", ",
            "\"window_enable[5].w2_invert\", \"window_one_left\", \"window_one_right\", ",
            "\"window_two_left\", \"window_two_right\", \"window_combine[0]\", ",
            "\"window_combine[1]\", \"window_combine[2]\", \"window_combine[3]\", ",
            "\"window_combine[4]\", \"window_combine[5]\", \"bg_main_enable[0]\", ",
            "\"bg_main_enable[1]\", \"bg_main_enable[2]\", \"bg_main_enable[3]\", ",
            "\"obj_main_enable\", \"bg_sub_enable[0]\", \"bg_sub_enable[1]\", ",
            "\"bg_sub_enable[2]\", \"bg_sub_enable[3]\", \"obj_sub_enable\", ",
            "\"bg_main_window_disable[0]\", \"bg_main_window_disable[1]\", ",
            "\"bg_main_window_disable[2]\", \"bg_main_window_disable[3]\", ",
            "\"obj_main_window_disable\", \"bg_sub_window_disable[0]\", ",
            "\"bg_sub_window_disable[1]\", \"bg_sub_window_disable[2]\", ",
            "\"bg_sub_window_disable[3]\", \"obj_sub_window_disable\", \"col_main_mask\", ",
            "\"col_sub_mask\", \"addsub_mode\", \"direct_color\", \"color_math_subtract\", ",
            "\"color_math_halve\", \"color_math_back_enable\", \"color_math_oam_enable\", ",
            "\"color_math_bg_enable[0]\", \"color_math_bg_enable[1]\", ",
            "\"color_math_bg_enable[2]\", \"color_math_bg_enable[3]\", \"fixed_color\", ",
            "\"pseudo_hires\", \"overscan\", \"obj_interlace\", \"interlace\", \"field\"], ",
            "complete: true, firstRowLine: 1, frame: 3, fromFirstLine: true, ",
            "memoryExact: true, rowsPerLine: 1, values: Uint32Array(len 27328, ",
            "fnv1a64 0x512f8064a14a9c85)}",
        ),
    ];
    assert_eq!(
        shapes(&[idle.get_line_states(), emu.get_line_states()]),
        expected
    );
}

#[wasm_bindgen_test]
fn the_line_palettes_read_as_before() {
    let idle = RetroEmulator::new("snes").unwrap();
    let emu = running_the_capture_program();
    let expected = [
        "null",
        concat!(
            "{coloursPerPalette: 256, paletteOfLine: Uint16Array(len 224, ",
            "fnv1a64 0xddedd579bea76625), palettes: Uint16Array(len 256, ",
            "fnv1a64 0x82bce97466c89cfc)}",
        ),
    ];
    assert_eq!(
        shapes(&[idle.get_line_palettes(), emu.get_line_palettes()]),
        expected
    );
}

#[wasm_bindgen_test]
fn the_picture_from_the_line_states_reads_as_before() {
    let mut idle = RetroEmulator::new("snes").unwrap();
    let mut emu = running_the_capture_program();
    let expected = [
        "null",
        concat!(
            "{data: Uint8Array(len 229376, fnv1a64 0x75f379eed654dafa), height: 224, ",
            "width: 256}",
        ),
    ];
    assert_eq!(
        shapes(&[
            idle.picture_from_line_states(),
            emu.picture_from_line_states()
        ]),
        expected
    );
}

#[wasm_bindgen_test]
fn the_pixel_sources_read_as_before() {
    let mut idle = RetroEmulator::new("snes").unwrap();
    let mut emu = running_the_capture_program();
    let mut sources = vec![idle.get_pixel_sources(None)];
    for planes in [
        None,
        Some(0),
        Some(1),
        Some(2),
        Some(4),
        Some(8),
        Some(16),
        Some(15),
        Some(31),
        Some(0x8000_0011),
    ] {
        sources.push(emu.get_pixel_sources(planes));
    }
    let expected = [
        "null",
        concat!(
            "{cell: Uint16Array(len 57344, fnv1a64 0x2abaaefb122acfa1), ",
            "colour: Uint16Array(len 57344, fnv1a64 0x80879d5b3e4821fd), frame: 3, ",
            "fromLineStates: true, height: 224, layer: Uint16Array(len 57344, ",
            "fnv1a64 0x6e92b53c2738661d), matchesPicture: true, ",
            "tile: Uint32Array(len 57344, fnv1a64 0x5e95f5527dac8199), width: 256}",
        ),
        "{frame: 3, fromLineStates: true, height: 224, matchesPicture: true, width: 256}",
        concat!(
            "{frame: 3, fromLineStates: true, height: 224, ",
            "layer: Uint16Array(len 57344, fnv1a64 0x6e92b53c2738661d), ",
            "matchesPicture: true, width: 256}",
        ),
        concat!(
            "{colour: Uint16Array(len 57344, fnv1a64 0x80879d5b3e4821fd), frame: 3, ",
            "fromLineStates: true, height: 224, matchesPicture: true, width: 256}",
        ),
        concat!(
            "{frame: 3, fromLineStates: true, height: 224, matchesPicture: true, ",
            "tile: Uint32Array(len 57344, fnv1a64 0x5e95f5527dac8199), width: 256}",
        ),
        concat!(
            "{cell: Uint16Array(len 57344, fnv1a64 0x2abaaefb122acfa1), frame: 3, ",
            "fromLineStates: true, height: 224, matchesPicture: true, width: 256}",
        ),
        concat!(
            "{frame: 3, fromLineStates: true, height: 224, matchesPicture: true, ",
            "subColour: Uint16Array(len 57344, fnv1a64 0x045b5aaabee52325), ",
            "subLayer: Uint8Array(len 57344, fnv1a64 0xa7ceef1ebf7f8325), width: 256}",
        ),
        concat!(
            "{cell: Uint16Array(len 57344, fnv1a64 0x2abaaefb122acfa1), ",
            "colour: Uint16Array(len 57344, fnv1a64 0x80879d5b3e4821fd), frame: 3, ",
            "fromLineStates: true, height: 224, layer: Uint16Array(len 57344, ",
            "fnv1a64 0x6e92b53c2738661d), matchesPicture: true, ",
            "tile: Uint32Array(len 57344, fnv1a64 0x5e95f5527dac8199), width: 256}",
        ),
        concat!(
            "{cell: Uint16Array(len 57344, fnv1a64 0x2abaaefb122acfa1), ",
            "colour: Uint16Array(len 57344, fnv1a64 0x80879d5b3e4821fd), frame: 3, ",
            "fromLineStates: true, height: 224, layer: Uint16Array(len 57344, ",
            "fnv1a64 0x6e92b53c2738661d), matchesPicture: true, ",
            "subColour: Uint16Array(len 57344, fnv1a64 0x045b5aaabee52325), ",
            "subLayer: Uint8Array(len 57344, fnv1a64 0xa7ceef1ebf7f8325), ",
            "tile: Uint32Array(len 57344, fnv1a64 0x5e95f5527dac8199), width: 256}",
        ),
        concat!(
            "{frame: 3, fromLineStates: true, height: 224, ",
            "layer: Uint16Array(len 57344, fnv1a64 0x6e92b53c2738661d), ",
            "matchesPicture: true, subColour: Uint16Array(len 57344, ",
            "fnv1a64 0x045b5aaabee52325), subLayer: Uint8Array(len 57344, ",
            "fnv1a64 0xa7ceef1ebf7f8325), width: 256}",
        ),
    ];
    assert_eq!(shapes(&sources), expected);
}

#[wasm_bindgen_test]
fn the_line_sprites_read_as_before() {
    let mut idle = RetroEmulator::new("snes").unwrap();
    let mut emu = running_the_capture_program();
    let expected = [
        "null",
        concat!(
            "{drawnWidth: Uint8Array(len 1024, fnv1a64 0x967865f30558c925), ",
            "fates: Uint8Array(len 1024, fnv1a64 0xb9c6dee5fd4ffedd), frame: 3, ",
            "fromLineStates: true, lines: Uint16Array(len 896, ",
            "fnv1a64 0xc2b4dc5df408de2c), slots: Uint8Array(len 1024, ",
            "fnv1a64 0x13c81b8067849c25), width: Uint8Array(len 1024, ",
            "fnv1a64 0x80e6ac3166837325), x: Int16Array(len 1024, ",
            "fnv1a64 0x41d05ddfe959cc65)}",
        ),
    ];
    assert_eq!(
        shapes(&[idle.get_line_sprites(), emu.get_line_sprites()]),
        expected
    );
}

#[wasm_bindgen_test]
fn the_sprite_renders_read_as_before() {
    let mut idle = RetroEmulator::new("snes").unwrap();
    let mut emu = running_the_capture_program();
    let renders = [
        idle.render_sprites(&[1]),
        emu.render_sprites(&[]),
        emu.render_sprites(&[1, 0, 1]),
        emu.render_sprites(&[1; 128]),
    ];
    let expected = [
        "null",
        concat!(
            "{data: Uint8Array(len 229376, fnv1a64 0xc9b1e505f9a82325), ",
            "fromLineStates: true, height: 224, width: 256}",
        ),
        concat!(
            "{data: Uint8Array(len 229376, fnv1a64 0x472177febf22ec05), ",
            "fromLineStates: true, height: 224, width: 256}",
        ),
        concat!(
            "{data: Uint8Array(len 229376, fnv1a64 0x59b321ddc659a405), ",
            "fromLineStates: true, height: 224, width: 256}",
        ),
    ];
    assert_eq!(shapes(&renders), expected);
}

#[wasm_bindgen_test]
fn the_video_memory_sources_read_as_before() {
    let idle = RetroEmulator::new("snes").unwrap();
    let emu = running_the_capture_program();
    let expected = [
        "null",
        concat!(
            "{frames: Uint32Array(len 65536, fnv1a64 0x7e0954272f497645), ",
            "origins: Uint32Array(len 65536, fnv1a64 0x1d5aaafb91b9d8e8)}",
        ),
    ];
    assert_eq!(
        shapes(&[
            idle.get_video_memory_sources(),
            emu.get_video_memory_sources()
        ]),
        expected
    );
}

#[wasm_bindgen_test]
fn the_resolved_sources_read_as_before() {
    let emu = running_the_capture_program();
    let addresses = [
        0x00_8000u32,
        u32::from(PROGRAM_PALETTE),
        0x81_9020,
        0x7E_0000,
        0x00_0000,
        0x00_2100,
        0x40_0000,
        0xFF_FFFF,
        0xFF00_8000,
    ];
    let resolved = [
        JsValue::from(emu.resolve_sources(&addresses)),
        JsValue::from(emu.resolve_sources(&[])),
    ];
    let expected = [
        "Int32Array(len 9, fnv1a64 0x906e57eb2f638fa2)",
        "Int32Array(len 0, fnv1a64 0xcbf29ce484222325)",
    ];
    assert_eq!(shapes(&resolved), expected);
}

#[wasm_bindgen_test]
fn the_frame_writes_read_as_before() {
    let mut idle = RetroEmulator::new("snes").unwrap();
    let mut emu = running_the_capture_program();
    let writes = [
        idle.take_frame_writes(),
        emu.take_frame_writes(),
        emu.take_frame_writes(),
    ];
    let expected = [
        "null",
        concat!(
            "{dropped: 0, records: Uint32Array(len 1384, ",
            "fnv1a64 0x0bd0b238c49f8713), wordsPerRecord: 8}",
        ),
        concat!(
            "{dropped: 0, records: Uint32Array(len 0, fnv1a64 0xcbf29ce484222325), ",
            "wordsPerRecord: 8}",
        ),
    ];
    assert_eq!(shapes(&writes), expected);
}

#[wasm_bindgen_test]
fn a_repaint_draws_as_before() {
    let mut emu = running_the_capture_program();
    emu.toggle_layer(0, false);
    emu.repaint_frame();
    let mut pictures: Vec<JsValue> = vec![emu.get_framebuffer_data().into()];
    emu.toggle_layer(0, true);
    emu.toggle_layer(4, false);
    emu.repaint_frame();
    pictures.push(emu.get_framebuffer_data().into());
    let expected = [
        "Uint8ClampedArray(len 229376, fnv1a64 0x0f7bd3c3a27b6475)",
        "Uint8ClampedArray(len 229376, fnv1a64 0x5aa9123884e96d52)",
    ];
    assert_eq!(shapes(&pictures), expected);
}
