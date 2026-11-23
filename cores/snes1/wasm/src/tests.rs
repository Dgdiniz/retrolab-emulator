use super::*;
use retro_wasm::test_support::property;
use wasm_bindgen_test::*;

#[wasm_bindgen_test]
fn test_emulator_creation() {
    assert!(RetroEmulator::new("snes").is_ok());
    assert!(RetroEmulator::new("superfamicom").is_ok());
    assert!(RetroEmulator::new("sfc").is_ok());
}

#[wasm_bindgen_test]
fn test_unknown_system() {
    assert!(RetroEmulator::new("genesis").is_err());
    assert!(RetroEmulator::new("unknown").is_err());
}

#[wasm_bindgen_test]
fn test_get_system_info() {
    let emu = RetroEmulator::new("snes").unwrap();
    assert!(!emu.get_system_info().is_null());
}

/// A 128 KiB LoROM that loops at `$00:8000`, named by `title`, so two
/// titles make two ROMs with different hashes.
fn synth_lorom(title: &str) -> Vec<u8> {
    lorom_running(title, &[0x80, 0xFE], &[]) // BRA -2
}

/// A 128 KiB LoROM named by `title` that runs `program` from `$00:8000`,
/// with each `(address, bytes)` of `data` at that address of bank `$00`.
fn lorom_running(title: &str, program: &[u8], data: &[(u16, &[u8])]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x2_0000];
    rom[..program.len()].copy_from_slice(program);
    for (address, bytes) in data {
        let at = usize::from(address - 0x8000);
        rom[at..at + bytes.len()].copy_from_slice(bytes);
    }
    let header = 0x7FB0;
    let name = format!("{title:<21}");
    rom[header + 0x10..header + 0x25].copy_from_slice(&name.as_bytes()[..21]);
    rom[header + 0x25] = 0x20; // LoROM
    rom[header + 0x27] = 0x07; // 128 KiB
    rom[header + 0x29] = 0x01;
    rom[header + 0x2C..header + 0x2E].copy_from_slice(&(!0x1234u16).to_le_bytes());
    rom[header + 0x2E..header + 0x30].copy_from_slice(&0x1234u16.to_le_bytes());
    rom[header + 0x4C..header + 0x4E].copy_from_slice(&0x8000u16.to_le_bytes());
    rom
}

#[wasm_bindgen_test]
fn the_cartridge_hash_is_the_sha256_of_the_file_loaded_its_copier_header_included() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    assert_eq!(emu.get_cartridge_hash(), None, "no load yet");
    assert!(emu.get_rom_bytes().is_empty(), "no load yet");
    let mut file = vec![0u8; 512];
    file.extend(synth_lorom("HASHED GAME"));
    emu.load_rom(&file).unwrap();
    assert_eq!(
        emu.get_cartridge_hash(),
        Some(retro_core::cartridge::rom_sha256(&file))
    );
    // The ROM's bytes are the image: the copier header taken off, the
    // image's hash the cartridge's.
    let image = emu.get_rom_bytes();
    assert_eq!(image, synth_lorom("HASHED GAME"));
    let info = emu.get_cartridge_info().unwrap();
    assert_eq!(
        property(&info, "rom_hash").as_string(),
        Some(retro_core::cartridge::rom_sha256(&image))
    );
}

#[wasm_bindgen_test]
fn the_spc_is_the_apu_s_and_its_tag_the_host_s() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&synth_lorom("SPC GAME")).unwrap();
    emu.run_frame();
    let plain = emu.export_spc(JsValue::UNDEFINED).unwrap();
    assert_eq!(plain.len(), 0x10200);
    assert_eq!(&plain[..33], b"SNES-SPC700 Sound File Data v0.30");
    assert_eq!(plain[0x23], 27, "no tag");
    let tag = js_sys::Object::new();
    js_sys::Reflect::set(&tag, &"title".into(), &"Song".into()).unwrap();
    let tagged = emu.export_spc(tag.into()).unwrap();
    assert_eq!(plain[0x23], 27);
    assert_eq!((tagged[0x23], &tagged[0x2E..0x33]), (26, &b"Song\0"[..]));
    assert!(emu.export_spc(JsValue::from(3)).is_err(), "not a tag");
    assert_eq!(
        emu.take_key_on_spc(JsValue::UNDEFINED).unwrap(),
        None,
        "never armed"
    );
    emu.arm_spc_at_key_on();
    emu.run_frame();
    assert_eq!(
        emu.take_key_on_spc(JsValue::UNDEFINED).unwrap(),
        None,
        "the program keys no voice"
    );
}

#[wasm_bindgen_test]
fn the_sample_directory_and_a_decode_read_aram() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&synth_lorom("BRR GAME")).unwrap();
    let directory: js_sys::Array = emu.get_sample_directory().dyn_into().unwrap();
    assert_eq!(directory.length(), 256);
    assert_eq!(
        retro_wasm::test_support::shape(&directory.get(0)),
        "{loop_start: 0, source: 0, start: 0, voices: 255}",
        "ARAM empty, every voice on source 0"
    );
    let sample = emu.decode_brr(0x0000, 0x0000);
    let pcm: js_sys::Int16Array = property(&sample, "pcm").dyn_into().unwrap();
    let blocks = property(&sample, "blocks").as_f64().unwrap() as u32;
    assert_eq!(pcm.length(), blocks * 16);
    assert_eq!(property(&sample, "loop_sample").as_f64(), Some(0.0));
    assert_eq!(
        property(&sample, "ends").as_string().as_deref(),
        Some("Unended")
    );
}

#[wasm_bindgen_test]
fn test_breakpoint_add_list() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    let id = emu.add_breakpoint(0x008000, "execute".to_string(), None);
    let list = emu.list_breakpoints();
    assert!(!list.is_null());
    emu.remove_breakpoint(id);
}

#[wasm_bindgen_test]
fn test_event_catalog_and_add_event_breakpoint() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    let events: Vec<EventClass> =
        serde_wasm_bindgen::from_value(emu.get_available_events()).unwrap();
    assert_eq!(events.len(), 12);
    // The first VBlank event can back an event breakpoint.
    let code = events[0].code;
    let id = emu.add_event_breakpoint(code, 0xFFFF_FFFF);
    let list = emu.list_breakpoints();
    assert!(!list.is_null());
    emu.remove_breakpoint(id);
}

#[wasm_bindgen_test]
fn test_execution_mode_round_trip() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.set_execution_mode("Paused".to_string());
    assert_eq!(emu.get_execution_mode(), "Paused");
    emu.set_execution_mode("Running".to_string());
    assert_eq!(emu.get_execution_mode(), "Running");
}

#[wasm_bindgen_test]
fn test_disassemble_window_does_not_panic() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    let rows = emu.disassemble_window(0, 0x008000, 4, 8);
    assert!(!rows.is_null());
}

#[wasm_bindgen_test]
fn test_snes_specific_surfaces() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    assert!(emu.get_ppu_mode() <= 7);
    assert!(!emu.get_ppu_mode_breakdown().is_null());
    assert!(!emu.get_mode7_matrix().is_null());
    // Mode 7 sampler reads live VRAM (zeroed at reset -> index 0).
    assert_eq!(emu.mode7_sample(0, 0), Some(0));
    assert!(!emu.get_apu_port_values().is_null());
    assert!(!emu.get_cartridge_manifest().is_null());
}

#[wasm_bindgen_test]
fn test_manifest_detect_set_clear() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    // No ROM loaded yet -> detected manifest for the running game is
    // the JSON literal `null`.
    assert_eq!(emu.get_detected_manifest().unwrap(), "null");

    // detectManifest on raw bytes always yields a manifest; an
    // unrecognised buffer falls back to a LoROM shell.
    let json = emu.detect_manifest(&[0u8; 0x8000]).unwrap();
    assert!(json.contains("\"preset\""));
    assert!(json.contains("lorom"));

    // The detected JSON round-trips back as a staged override.
    emu.set_cartridge_manifest(&json).unwrap();
    // Malformed JSON and unsupported versions are rejected loudly.
    assert!(emu.set_cartridge_manifest("{ not json }").is_err());
    assert!(emu
            .set_cartridge_manifest(
                r#"{"core":"snes1","version":"9.9","rom":{"sha256":"","size":0},"mapping":{"preset":"lorom"}}"#
            )
            .is_err());
    emu.clear_cartridge_manifest();
}

#[wasm_bindgen_test]
fn test_raw_memory_views_have_expected_sizes() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    // VRAM = 0x8000 words = 0x10000 bytes; CGRAM = 256 colours;
    // OAM = 0x220 bytes; ARAM = 64 KiB.
    assert_eq!(emu.get_vram().len(), 0x10000);
    assert_eq!(emu.get_palette_memory().len(), 0x100);
    assert_eq!(emu.get_oam().len(), 0x220);
    assert_eq!(emu.get_aram().len(), 0x10000);
}

#[wasm_bindgen_test]
fn test_sprite_tile_settings_carry_the_core_names() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    // OBSEL $FB: small 16x32, large 32x32, the first table at word
    // $6000, the second 256 tiles and three gaps of 256 tiles on,
    // which wraps to word $2000.
    emu.write_memory_debug(0x2101, &[0xFB]);
    let settings = emu.get_sprite_tile_settings();
    let field = |name: &str| {
        js_sys::Reflect::get(&settings, &JsValue::from_str(name))
            .unwrap()
            .as_f64()
            .unwrap_or_else(|| panic!("{name} is not a number"))
    };
    assert_eq!(field("small_width"), 16.0);
    assert_eq!(field("small_height"), 32.0);
    assert_eq!(field("large_width"), 32.0);
    assert_eq!(field("large_height"), 32.0);
    assert_eq!(field("first_table_base_word"), f64::from(0x6000));
    assert_eq!(field("second_table_base_word"), f64::from(0x2000));
}

#[wasm_bindgen_test]
fn test_video_registers_and_layers() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    assert!(!emu.get_video_registers().is_null());
    assert!(!emu.get_layer_info().is_null());
    // Toggling a layer must not panic and must be idempotent on read.
    emu.toggle_layer(0, false);
    emu.toggle_layer(0, true);
}

#[wasm_bindgen_test]
fn test_dsp_mute_and_waveforms() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.mute_audio_chip_channel(0, 0, true);
    emu.mute_audio_chip_channel(0, 0, false);
    // A muted/keyed-off voice reads as `samples` of silence; the mixed
    // waveform peek returns the requested length.
    assert_eq!(emu.get_audio_chip_waveform(0, 0, 64).len(), 64);
    assert_eq!(emu.get_audio_output_waveform(64).len(), 64);
}

/// A pad's map as the host sends it: a plain object, whose keys
/// JavaScript holds as text. SNES B is bit 5, A is bit 4.
fn plain_gamepad_map(pairs: &[(u32, u32)]) -> JsValue {
    let obj = js_sys::Object::new();
    for (button, bit) in pairs {
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str(&button.to_string()),
            &JsValue::from(*bit),
        )
        .unwrap();
    }
    obj.into()
}

#[wasm_bindgen_test]
fn write_vram_reads_back_through_get_vram_wrapping_at_the_end() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    // Word 0x7FFF is the last; the run goes on at word 0.
    emu.write_vram(0x7FFF, &[1, 2, 3, 4]);
    let vram = emu.get_vram();
    assert_eq!(&vram[0xFFFE..], &[1, 2]);
    assert_eq!(&vram[..2], &[3, 4]);

    emu.write_vram(0x8003, &[9]);
    assert_eq!(emu.get_vram()[6], 9, "a word address past the end wraps");

    let before = emu.get_vram();
    emu.write_vram(0x10, &[]);
    assert_eq!(emu.get_vram(), before, "an empty write changes nothing");
}

#[wasm_bindgen_test]
fn the_palette_written_reads_back_and_shows_after_a_repaint() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    // Colour 255, then on round to colour 0; bit 15 is dropped.
    emu.write_palette_memory(255, &[0xFFFF, 0x001F]);
    let cgram = emu.get_palette_memory();
    assert_eq!(cgram[255], 0x7FFF);
    assert_eq!(cgram[0], 0x001F);

    // INIDISP $0F: the display on at full brightness. With no layer
    // on, the screen is the backdrop, colour 0, red now.
    emu.write_memory_debug(0x2100, &[0x0F]);
    emu.repaint_frame();
    assert_eq!(
        &emu.get_framebuffer_data().to_vec()[..4],
        &[0xFF, 0x00, 0x00, 0xFF]
    );
}

/// A reader's result reaches the app as a plain object: not a `Map`,
/// whose entries a property read does not see, and `key` one of its
/// properties.
fn assert_plain_object(value: &JsValue, key: &str) {
    assert!(value.is_object(), "an object, holding {key}");
    assert!(
        !value.is_instance_of::<js_sys::Map>(),
        "not a Map, holding {key}"
    );
    assert!(
        js_sys::Reflect::has(value, &JsValue::from_str(key)).unwrap(),
        "{key} is a property"
    );
}

#[wasm_bindgen_test]
fn the_coverage_stats_are_a_plain_object() {
    let emu = RetroEmulator::new("snes").unwrap();
    let stats = emu.get_coverage_stats(0);
    assert_plain_object(&stats, "totalBytes");
    assert_eq!(
        property(&stats, "totalBytes").as_f64(),
        Some(f64::from(0x0100_0000))
    );
}

#[wasm_bindgen_test]
fn the_mode_breakdown_is_a_plain_object() {
    let emu = RetroEmulator::new("snes").unwrap();
    assert_plain_object(&emu.get_ppu_mode_breakdown(), "mode");
}

#[wasm_bindgen_test]
fn the_ppu_settings_are_a_plain_object() {
    let emu = RetroEmulator::new("snes").unwrap();
    let settings = emu.get_ppu_settings();
    assert_plain_object(&settings, "obj_base_size");
    assert_plain_object(&property(&settings, "sprite_tiles"), "small_width");
}

/// What `shape` says of each value.
fn shapes(values: &[JsValue]) -> Vec<String> {
    values.iter().map(retro_wasm::test_support::shape).collect()
}

mod capture;

/// The debugger's readers, each held to the shape it answers today, and
/// what each step returns. Most run on one small program with an execute
/// breakpoint, a write watch, an event breakpoint, the main CPU's trace
/// and two instructions of history (`debugging_the_program`); the readers
/// of the cartridge CPU's trace run on two cartridges whose chip runs a
/// short program. A literal that stops matching means a value a host
/// reads changed: record a new one only for a change a host is meant to
/// see.
mod debugger_shape_tests;

/// What the bindings answer about the machine, recorded.
mod machine_baseline;

/// What the bindings answer, method by method, recorded: the machine and
/// what each call leaves, the display after each call that makes a frame,
/// the audio, an input session, the debugger's memory and disassembly, the
/// PPU's and the S-DSP's readers, and the cartridge's.
mod binding_baseline;

/// Each name both consoles share against what the core gives for it: the
/// same value, or the same effect.
mod names;
