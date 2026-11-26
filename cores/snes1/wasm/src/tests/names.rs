//! Each name both consoles share against what the SNES's core gives for
//! it, read through the core's own accessors: the two give the same value,
//! or have the same effect, on the same machine.

use super::capture::capture_program_rom;
use super::*;
use retro_core::debug::{trace_to_text, TraceEntry};
use retro_core::EmulatorCore;
use retro_wasm::js::to_js;
use retro_wasm::test_support::shape;
#[cfg(feature = "chip-superfx")]
use snes1_core::TraceEntryBinaryGsu;
use snes1_core::TraceEntryBinarySpc700;

/// The capture program after a few frames, sound on and the SPC700 traced.
fn running() -> RetroEmulator {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&capture_program_rom()).unwrap();
    emu.set_audio_enabled(true);
    emu.set_cpu_tracing_enabled(1, true);
    for _ in 0..4 {
        emu.run_frame();
    }
    emu
}

/// The words of `bytes`, a word's low byte first.
fn little_endian_words(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

fn number(value: &JsValue, key: &str) -> f64 {
    property(value, key).as_f64().unwrap()
}

#[wasm_bindgen_test]
fn the_cpus_are_the_65c816_stepped_and_the_spc700_not() {
    let emu = running();
    assert_eq!(
        shape(&emu.get_cpus()),
        "[{architecture: \"Mos65816\", id: 0, location: \"Motherboard\", name: \"Ricoh 5A22 \
         (WDC 65C816)\", records_coverage: true, steppable: true}, {architecture: \"Spc700\", \
         id: 1, location: \"Motherboard\", name: \"Sony SPC700\", records_coverage: false, \
         steppable: false}]"
    );
}

#[cfg(feature = "chip-superfx")]
#[wasm_bindgen_test]
fn the_cartridge_s_cpu_is_the_third_and_traces_by_its_id() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&super::debugger_shape_tests::super_fx_rom())
        .unwrap();
    let cpus = js_sys::Array::from(&emu.get_cpus());
    assert_eq!(cpus.length(), 3);
    assert_eq!(
        shape(&cpus.get(2)),
        "{architecture: \"SuperFx\", id: 2, location: \"Cartridge\", name: \"Super FX GSU\", \
         records_coverage: false, steppable: true}"
    );

    assert_eq!(
        emu.get_cpu_trace_entry_size(2),
        <TraceEntryBinaryGsu as TraceEntry>::SIZE
    );
    emu.set_cpu_tracing_enabled(2, true);
    assert!(emu.is_cpu_tracing_enabled(2));
    for _ in 0..2 {
        emu.run_frame();
    }
    let data = emu.get_cpu_trace_flush_data(2);
    assert!(!data.is_empty());
    assert_eq!(
        emu.convert_cpu_trace_chunk_to_text(2, &data),
        trace_to_text::<TraceEntryBinaryGsu, ()>(&data, &mut ())
    );
    emu.clear_cpu_trace_flushed(2);
    assert!(emu.get_cpu_trace_flush_data(2).is_empty());
    emu.set_cpu_tracing_enabled(2, false);
    assert!(!emu.is_cpu_tracing_enabled(2));
}

#[wasm_bindgen_test]
fn the_video_registers_are_the_ppu_s() {
    let mut emu = running();
    let registers = emu.snes.video_debugger().get_registers();
    assert_eq!(shape(&emu.get_video_registers()), shape(&to_js(&registers)));
}

#[wasm_bindgen_test]
fn the_colour_levels_are_the_ppu_s() {
    let mut emu = running();
    let levels = emu.snes.video_debugger().colour_levels();
    assert_eq!((levels.name, levels.levels.len()), ("snes-32", 32));
    assert_eq!(shape(&emu.get_colour_levels()), shape(&to_js(&levels)));
}

#[wasm_bindgen_test]
fn the_video_memories_are_vram_cgram_and_oam_low_byte_first() {
    let mut emu = running();
    assert_eq!(
        shape(&emu.get_video_memories()),
        "[{address_unit: 2, id: \"vram\", little_endian: true, name: \"VRAM\", size: 65536}, \
         {address_unit: 2, id: \"palette\", little_endian: true, name: \"CGRAM\", size: 512}, \
         {address_unit: 1, id: \"sprite_table\", little_endian: true, name: \"OAM\", size: \
         544}]"
    );
    assert_eq!(emu.read_video_memory("vram"), Some(emu.get_vram()));
    let cgram = emu.read_video_memory("palette").unwrap();
    assert_eq!(little_endian_words(&cgram), emu.get_palette_memory());
    assert_eq!(emu.read_video_memory("sprite_table"), Some(emu.get_oam()));
    assert_eq!(emu.read_video_memory("vertical_scroll"), None);
    assert!(!emu.write_video_memory("vertical_scroll", 0, &[1, 2]));
}

#[wasm_bindgen_test]
fn the_palette_memory_is_cgram() {
    let mut emu = running();
    assert_eq!(emu.get_palette_memory(), emu.snes.ppu().cgram());
    let colours = emu.get_palette_colours();
    assert_eq!(colours.len(), 256 * 4);
    // Each colour is the swatch the core's palette image draws for it, 16
    // pixels square, 16 to a row of the 256-pixel image.
    let image = emu.snes.export_palette_image();
    for colour in [0usize, 1, 2, 17, 255] {
        let (x, y) = ((colour % 16) * 16, (colour / 16) * 16);
        let at = (y * 256 + x) * 4;
        assert_eq!(
            &colours[colour * 4..colour * 4 + 4],
            &image[at..at + 4],
            "colour {colour}"
        );
    }

    emu.write_palette_memory(255, &[0xFFFF, 0x1234]);
    assert_eq!(emu.get_palette_memory(), emu.snes.ppu().cgram());
    assert_eq!(
        (emu.get_palette_memory()[255], emu.get_palette_memory()[0]),
        (0x7FFF, 0x1234),
        "the index wraps, bit 15 dropped"
    );
}

#[wasm_bindgen_test]
fn a_write_counts_bytes_where_write_vram_counts_words() {
    let mut old = running();
    let mut new = running();
    old.write_vram(0x7FFF, &[0xAA, 0xBB, 0xCC]);
    assert!(new.write_video_memory("vram", 0xFFFE, &[0xAA, 0xBB, 0xCC]));
    assert_eq!(new.get_vram(), old.get_vram());
    assert_eq!(new.get_vram()[0], 0xCC, "the address wraps");

    old.write_vram(0x0100, &[0x11, 0x22, 0x33]);
    assert!(new.write_video_memory("vram", 0x0200, &[0x11, 0x22, 0x33]));
    assert_eq!(new.get_vram(), old.get_vram(), "an odd count");
    assert!(new.write_video_memory("vram", 0x0301, &[0x44]));
    assert_eq!(new.get_vram()[0x0301], 0x44, "an odd byte alone");
    assert_eq!(new.get_vram()[0x0300], old.get_vram()[0x0300]);

    old.write_oam(543, &[0x77, 0x66]);
    assert!(new.write_video_memory("sprite_table", 543, &[0x77, 0x66]));
    assert_eq!(new.get_oam(), old.get_oam());

    old.write_palette_memory(3, &[0x0246]);
    assert!(new.write_video_memory("palette", 7, &[0x46, 0x02, 0x99]));
    assert_eq!(
        new.get_palette_memory(),
        old.get_palette_memory(),
        "an offset inside a word starts at it, an odd last byte is dropped"
    );
}

#[wasm_bindgen_test]
fn the_sprite_table_is_oam_from_the_first_sprite() {
    let mut emu = running();
    let table = js_sys::Array::from(&emu.get_sprite_table());
    let descriptors = emu.snes.ppu().export_oam_sprites();
    let sprites = js_sys::Array::from(&emu.get_sprite_data());
    let settings = emu.get_sprite_tile_settings();
    let oam = emu.get_oam();
    assert_eq!(table.length(), 128);
    for taken in 0..table.length() {
        let entry = table.get(taken);
        let index = number(&entry, "index") as u32;
        assert_eq!(index, taken, "sprite 0 first: no rotation");
        let descriptor = &descriptors[index as usize];
        let sprite = sprites.get(index);
        for key in [
            "x",
            "y",
            "width",
            "height",
            "tile_index",
            "palette",
            "priority",
            "flip_h",
            "flip_v",
            "picture",
        ] {
            assert_eq!(
                shape(&property(&entry, key)),
                shape(&property(&sprite, key)),
                "{key} of sprite {index}"
            );
        }
        assert_eq!(number(&entry, "tile_index"), f64::from(descriptor.tile));
        assert_eq!(property(&entry, "large").as_bool(), Some(descriptor.large));
        assert!(property(&entry, "next").is_undefined());
        let table_base = if oam[index as usize * 4 + 3] & 1 == 0 {
            number(&settings, "first_table_base_word")
        } else {
            number(&settings, "second_table_base_word")
        };
        let character = number(&entry, "tile_index");
        assert_eq!(
            number(&entry, "tile_address"),
            (table_base * 2.0 + character * 32.0) % 65536.0,
            "the character's byte, sprite {index}"
        );
    }
}

#[wasm_bindgen_test]
fn the_layers_are_the_backgrounds_field_for_field() {
    let mut emu = running();
    for bg in 0..4 {
        let layout = emu.get_layer_layout(bg);
        let registers = emu.snes.bg_layout(bg as usize);
        let cell = if registers.tile_size16 { 16.0 } else { 8.0 };
        for (key, value) in [
            ("map_base", registers.tilemap_base_byte),
            ("map_width", registers.width_cells),
            ("map_height", registers.height_cells),
            ("character_base", registers.char_base_byte),
            ("bits_per_pixel", u32::from(registers.bpp)),
            ("first_colour", u32::from(registers.palette_base)),
            ("scroll_x", u32::from(registers.scroll_x)),
            ("scroll_y", u32::from(registers.scroll_y)),
        ] {
            assert_eq!(
                number(&layout, key),
                f64::from(value),
                "{key} of BG{}",
                bg + 1
            );
        }
        assert_eq!(number(&layout, "cell_width"), cell);
        assert_eq!(number(&layout, "cell_height"), cell);
        let kind = if registers.bpp == 0 {
            "off"
        } else {
            "tile_map"
        };
        assert_eq!(property(&layout, "kind").as_string().as_deref(), Some(kind));
    }
    let sprites = emu.get_layer_layout(4);
    assert_eq!(
        property(&sprites, "kind").as_string().as_deref(),
        Some("sprites")
    );
    assert_eq!(
        number(&sprites, "character_base"),
        number(&emu.get_sprite_tile_settings(), "first_table_base_word") * 2.0
    );
    assert!(emu.get_layer_layout(5).is_null());
}

#[wasm_bindgen_test]
fn a_layer_s_map_is_the_ppu_s() {
    let mut emu = running();
    for bg in 0..4 {
        let map = emu
            .snes
            .video_debugger()
            .layer_map(bg)
            .expect("each background has a map");
        assert_eq!(
            shape(&emu.layer_map(bg)),
            shape(&retro_wasm::js::picture_object(&map)),
            "BG{}",
            bg + 1
        );
    }
    assert!(emu.layer_map(4).is_null(), "the sprites have no map");
    assert!(emu.layer_map(5).is_null());
}

#[wasm_bindgen_test]
fn a_layer_s_priorities_and_the_draw_order_are_the_ppu_s() {
    let mut emu = running();
    let orders = retro_core::capture::CaptureCore::draw_order(&emu.snes);
    assert_eq!(
        shape(&emu.get_draw_order()),
        shape(&retro_wasm::capture::draw_order_array(&orders))
    );
    let slots: Vec<_> = orders.iter().flat_map(|run| run.slots.clone()).collect();
    assert!(!slots.is_empty());
    for slot in slots {
        let layer = retro_core::capture::CaptureCore::render_layer_priority(
            &mut emu.snes,
            slot.layer,
            slot.priority,
        )
        .expect("a layer the draw order names");
        assert_eq!(
            shape(&emu.render_layer_priority(slot.layer, slot.priority)),
            shape(&retro_wasm::capture::rendered_layer_object(&layer)),
            "{slot:?}"
        );
    }
    assert!(emu.render_layer_priority(5, 0).is_null());
}

#[wasm_bindgen_test]
fn the_sound_chip_is_the_s_dsp_and_its_voices() {
    let mut emu = running();
    assert_eq!(
        shape(&emu.get_audio_chips()),
        "[{channel_types: [\"Sample\", \"Sample\", \"Sample\", \"Sample\", \"Sample\", \
         \"Sample\", \"Sample\", \"Sample\"], channels: 8, manufacturer: \"Sony\", name: \
         \"S-DSP\", sample_rate: 32000, stereo: true}]"
    );
    let voices: Vec<_> = (0..8)
        .map(|voice| {
            let dsp = emu.snes.audio_debugger(0).unwrap();
            dsp.get_channel_state(voice)
        })
        .collect();
    assert_eq!(
        shape(&emu.get_audio_chip_channels(0)),
        shape(&to_js(&voices))
    );
    assert!(emu.get_audio_chip_channels(1).is_null());
    let dsp = emu.snes.audio_debugger(0).unwrap();
    let (voice, mixed) = (dsp.get_waveform(0, 64), dsp.get_output_waveform(64));
    assert_eq!(emu.get_audio_chip_waveform(0, 0, 64), voice);
    assert_eq!(emu.get_audio_output_waveform(64), mixed);

    emu.mute_audio_chip_channel(0, 2, true);
    emu.run_frame();
    let voices = js_sys::Array::from(&emu.get_audio_chip_channels(0));
    assert_eq!(property(&voices.get(2), "muted").as_bool(), Some(true));
    assert_eq!(property(&voices.get(1), "muted").as_bool(), Some(false));
    let dsp = emu.snes.audio_debugger(0).unwrap();
    assert_eq!(
        dsp.get_channel_state(2).map(|state| state.muted),
        Some(true)
    );
}

#[wasm_bindgen_test]
fn the_spc700_s_trace_is_cpu_1_s() {
    let mut emu = running();
    assert!(emu.is_cpu_tracing_enabled(1));
    let size = <TraceEntryBinarySpc700 as TraceEntry>::SIZE;
    assert_eq!(emu.get_cpu_trace_entry_size(1), size);
    let data = emu.get_cpu_trace_flush_data(1);
    assert!(!data.is_empty());
    assert_eq!(data.len() % size, 0);
    let head = &data[..size * 4];
    assert_eq!(
        emu.convert_cpu_trace_chunk_to_text(1, head),
        trace_to_text::<TraceEntryBinarySpc700, ()>(head, &mut ())
    );
    emu.clear_cpu_trace_flushed(1);
    assert!(emu.get_cpu_trace_flush_data(1).is_empty());
    emu.set_cpu_tracing_enabled(1, false);
    assert!(!emu.is_cpu_tracing_enabled(1));
}
