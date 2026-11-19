//! Each name both consoles share against the older names it stands beside
//! on the Mega Drive: the two give the same value, or have the same effect,
//! on the same machine.

use super::capture::capture_shapes_rom;
use super::*;
use retro_wasm::test_support::shape;

/// The capture program after a few frames, with the PSG's first channel
/// sounding a tone, the YM2612's first channel keyed on, and the Z80 running
/// a loop of its own, its trace on.
fn running() -> RetroEmulator {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.load_rom(&capture_shapes_rom()).unwrap();
    emu.set_audio_enabled(true);
    emu.run_frame();
    // The 68000 takes the Z80's bus, puts `JR $0000` at Z80 address 0, keys
    // the YM2612's first channel on, lets the Z80 out of reset and gives
    // the bus back; the PSG's first channel sounds a tone.
    emu.write_memory_debug(0xA1_1100, &[0x01]);
    emu.write_memory_debug(0xA0_0000, &[0x18, 0xFE]);
    for (register, value) in [(0xB4, 0xC0), (0xA4, 0x22), (0xA0, 0x69), (0x28, 0xF0)] {
        emu.write_memory_debug(0xA0_4000, &[register]);
        emu.write_memory_debug(0xA0_4001, &[value]);
    }
    emu.write_memory_debug(0xA1_1200, &[0x01]);
    emu.write_memory_debug(0xA1_1100, &[0x00]);
    for byte in [0x8E, 0x0F, 0x90] {
        emu.write_memory_debug(0xC0_0011, &[byte]);
    }
    emu.set_cpu_tracing_enabled(1, true);
    for _ in 0..3 {
        emu.run_frame();
    }
    emu
}

/// The words of `bytes`, a word's high byte first.
fn big_endian_words(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect()
}

#[wasm_bindgen_test]
fn the_cpus_are_the_68000_and_the_z80_both_stepped_and_recorded() {
    let emu = RetroEmulator::new("genesis").unwrap();
    assert_eq!(
        shape(&emu.get_cpus()),
        "[{architecture: \"M68000\", id: 0, location: \"Motherboard\", name: \"Motorola 68000\", \
         records_coverage: true, steppable: true}, {architecture: \"Z80\", id: 1, location: \
         \"Motherboard\", name: \"Zilog Z80\", records_coverage: true, steppable: true}]"
    );
}

#[wasm_bindgen_test]
fn the_video_registers_are_the_vdp_s() {
    let mut emu = running();
    assert_eq!(
        shape(&emu.get_video_registers()),
        shape(&emu.get_vdp_registers())
    );
}

#[wasm_bindgen_test]
fn the_video_memories_are_vram_cram_and_vsram_high_byte_first() {
    let mut emu = running();
    assert_eq!(
        shape(&emu.get_video_memories()),
        "[{address_unit: 1, id: \"vram\", little_endian: false, name: \"VRAM\", size: 65536}, \
         {address_unit: 2, id: \"palette\", little_endian: false, name: \"CRAM\", size: 128}, \
         {address_unit: 2, id: \"vertical_scroll\", little_endian: false, name: \"VSRAM\", \
         size: 128}]"
    );
    assert_eq!(emu.read_video_memory("vram"), Some(emu.get_vram()));
    let cram = emu.read_video_memory("palette").unwrap();
    assert_eq!(big_endian_words(&cram), emu.get_cram());
    let vsram = emu.read_video_memory("vertical_scroll").unwrap();
    assert_eq!(big_endian_words(&vsram), emu.get_vsram());
    assert_eq!(emu.read_video_memory("sprite_table"), None);
    assert!(!emu.write_video_memory("sprite_table", 0, &[1]));
}

#[wasm_bindgen_test]
fn the_palette_memory_is_cram() {
    let mut emu = running();
    assert_eq!(emu.get_palette_memory(), emu.get_cram());
    assert_eq!(emu.get_palette_colours(), emu.get_all_palettes_rgba());

    let mut old = running();
    let mut new = running();
    old.write_cram(70, &[0x0FFE, 0x0222]);
    new.write_palette_memory(70, &[0x0FFE, 0x0222]);
    assert_eq!(new.get_cram(), old.get_cram());
    assert_eq!(
        new.get_cram()[6],
        0x0EEE,
        "the index wraps, the port's bits kept"
    );
    assert_eq!(
        new.get_framebuffer_data().to_vec(),
        old.get_framebuffer_data().to_vec()
    );
}

#[wasm_bindgen_test]
fn a_write_counts_bytes_as_write_vram_does() {
    let mut old = running();
    let mut new = running();
    old.write_vram(0xFFFF, &[0xAA, 0xBB, 0xCC]);
    assert!(new.write_video_memory("vram", 0xFFFF, &[0xAA, 0xBB, 0xCC]));
    assert_eq!(new.get_vram(), old.get_vram());
    assert_eq!(&new.get_vram()[..2], &[0xBB, 0xCC], "the address wraps");

    old.write_vsram(63, &[0xFFFF, 0x0123]);
    assert!(new.write_video_memory("vertical_scroll", 126, &[0xFF, 0xFF, 0x01, 0x23]));
    assert_eq!(new.get_vsram(), old.get_vsram());
    assert_eq!(new.get_vsram()[63], 0x07FF, "11 bits kept");

    old.write_cram(5, &[0x0246]);
    assert!(new.write_video_memory("palette", 11, &[0x02, 0x46, 0x77]));
    assert_eq!(
        new.get_cram(),
        old.get_cram(),
        "an offset inside a word starts at it, an odd last byte is dropped"
    );
}

#[wasm_bindgen_test]
fn a_tile_in_colour_is_get_tile_rgba() {
    let mut emu = running();
    for (tile, palette) in [(0, 0), (1, 1), (0x20, 2), (0x7FF, 3)] {
        let picture = emu.get_tile_colours(0, 0, tile, 1, 4, 0, 16 * u32::from(palette));
        let data = field(&picture, "data")
            .dyn_into::<js_sys::Uint8Array>()
            .unwrap()
            .to_vec();
        assert_eq!(data, emu.get_tile_rgba(tile, palette), "tile {tile}");
    }
}

#[wasm_bindgen_test]
fn the_sprite_table_is_the_link_chain() {
    let mut emu = running();
    let table = js_sys::Array::from(&emu.get_sprite_table());
    let chain = js_sys::Array::from(&emu.get_sprite_attr_table());
    let sprites = js_sys::Array::from(&emu.get_sprite_data());
    assert_eq!(table.length() as usize, emu.get_sprite_count());
    assert_eq!(table.length(), chain.length());
    assert!(table.length() > 1);
    let number = |value: &JsValue, key: &str| field(value, key).as_f64().unwrap();
    for i in 0..table.length() {
        let entry = table.get(i);
        let attr = chain.get(i);
        let index = number(&entry, "index");
        assert_eq!(index, number(&attr, "index"));
        assert_eq!(number(&entry, "width"), number(&attr, "h_tiles") * 8.0);
        assert_eq!(number(&entry, "height"), number(&attr, "v_tiles") * 8.0);
        assert_eq!(number(&entry, "tile_index"), number(&attr, "tile"));
        assert_eq!(number(&entry, "tile_address"), number(&attr, "tile") * 32.0);
        assert_eq!(field(&entry, "large").as_bool(), Some(false));
        let next = field(&entry, "next");
        if i + 1 < table.length() {
            assert_eq!(next.as_f64(), Some(number(&attr, "link")));
            assert_eq!(next.as_f64(), Some(number(&chain.get(i + 1), "index")));
        } else {
            assert!(next.is_undefined(), "the last one links to none");
        }
        let sprite = sprites
            .iter()
            .find(|sprite| number(sprite, "index") == index)
            .unwrap();
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
        ] {
            assert_eq!(
                shape(&field(&entry, key)),
                shape(&field(&sprite, key)),
                "{key} of sprite {index}"
            );
        }
    }
}

#[wasm_bindgen_test]
fn the_layers_read_their_maps_where_the_plane_addresses_say() {
    let mut emu = running();
    let addresses = emu.get_plane_addresses();
    for (layer, key) in [(0, "plane_a"), (1, "plane_b"), (2, "window")] {
        let layout = emu.get_layer_layout(layer);
        assert_eq!(
            field(&layout, "map_base").as_f64(),
            field(&addresses, key).as_f64(),
            "{key}"
        );
    }
    assert_eq!(
        shape(&emu.get_layer_layout(0)),
        "{bits_per_pixel: 4, cell_height: 8, cell_width: 8, character_base: 0, first_colour: 0, \
         id: 0, kind: \"tile_map\", map_base: 49152, map_height: 32, map_width: 64, scroll_x: \
         0, scroll_y: 0}"
    );
    assert_eq!(
        field(&emu.get_layer_layout(3), "kind")
            .as_string()
            .as_deref(),
        Some("sprites")
    );
    assert!(emu.get_layer_layout(4).is_null());
}

#[wasm_bindgen_test]
fn the_sound_chips_are_the_ym2612_then_the_psg() {
    let mut emu = running();
    let chips = js_sys::Array::from(&emu.get_audio_chips());
    assert_eq!(chips.length(), 2);
    let (fm, psg) = (chips.get(0), chips.get(1));
    assert_eq!(field(&fm, "name").as_string().as_deref(), Some("YM2612"));
    assert_eq!(
        field(&fm, "channels").as_f64(),
        Some(f64::from(emu.get_ym2612_channel_count()))
    );
    assert_eq!(
        field(&psg, "channels").as_f64(),
        Some(f64::from(emu.get_audio_channel_count()))
    );
}

/// The older chip reader is the PSG as the chip describes itself, the same
/// value as `getAudioChips()[1]`: the chip's name is `"SN76489"`.
#[wasm_bindgen_test]
fn the_older_chip_reader_is_the_psg_as_it_describes_itself() {
    let mut emu = running();
    let psg = js_sys::Array::from(&emu.get_audio_chips()).get(1);
    let info = emu.get_audio_chip_info();
    assert_eq!(shape(&info), shape(&psg));
    assert_eq!(field(&info, "name").as_string().as_deref(), Some("SN76489"));
}

#[wasm_bindgen_test]
fn the_channels_are_each_chip_s_and_mute_as_the_older_names_do() {
    let mut emu = running();
    let psg = js_sys::Array::from(&emu.get_audio_chip_channels(1));
    for channel in 0..4 {
        assert_eq!(
            shape(&psg.get(channel)),
            shape(&emu.get_audio_channel_state(channel))
        );
    }
    let fm = js_sys::Array::from(&emu.get_audio_chip_channels(0));
    for channel in 0..6 {
        assert_eq!(
            shape(&fm.get(channel)),
            shape(&emu.get_ym2612_channel_state(channel))
        );
    }
    assert!(emu.get_audio_chip_channels(2).is_null());

    let mut old = running();
    let mut new = running();
    old.mute_audio_channel(0, true);
    old.mute_ym2612_channel(0, true);
    new.mute_audio_chip_channel(1, 0, true);
    new.mute_audio_chip_channel(0, 0, true);
    old.run_frame();
    new.run_frame();
    assert_eq!(
        shape(&new.get_audio_channel_state(0)),
        shape(&old.get_audio_channel_state(0))
    );
    assert_eq!(
        shape(&new.get_ym2612_channel_state(0)),
        shape(&old.get_ym2612_channel_state(0))
    );
    assert_eq!(new.get_audio_samples(), old.get_audio_samples());
}

#[wasm_bindgen_test]
fn the_waveforms_are_the_chip_s_and_the_mix() {
    let mut emu = running();
    assert_eq!(
        emu.get_audio_chip_waveform(1, 0, 64),
        emu.get_audio_waveform(0, 64)
    );
    assert_eq!(
        emu.get_audio_output_waveform(64),
        emu.get_mixed_waveform(64)
    );
    assert!(emu.get_audio_output_waveform(64).iter().any(|&s| s != 0.0));
    assert!(emu.get_audio_chip_waveform(2, 0, 4).is_empty());
}

#[wasm_bindgen_test]
fn the_z80_s_trace_is_cpu_1_s() {
    let mut emu = running();
    assert!(emu.is_cpu_tracing_enabled(1) && emu.is_z80_tracing_enabled());
    assert_eq!(
        emu.get_cpu_trace_entry_size(1),
        emu.get_z80_trace_entry_size()
    );
    assert_eq!(emu.needs_cpu_trace_flush(1), emu.needs_z80_trace_flush());
    let data = emu.get_cpu_trace_flush_data(1);
    assert!(!data.is_empty());
    assert_eq!(data, emu.get_z80_trace_flush_data());
    let head = &data[..emu.get_z80_trace_entry_size() * 4];
    assert_eq!(
        emu.convert_cpu_trace_chunk_to_text(1, head),
        emu.convert_z80_trace_chunk_to_text(head)
    );
    emu.clear_z80_trace_flushed();
    assert!(emu.get_cpu_trace_flush_data(1).is_empty());
    emu.set_z80_tracing_enabled(false);
    assert!(!emu.is_cpu_tracing_enabled(1));
}
