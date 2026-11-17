use super::*;
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

#[wasm_bindgen_test]
fn test_display_info() {
    let emu = RetroEmulator::new("snes").unwrap();
    let info = emu.get_display_info();
    assert!(!info.is_null());
    assert!(!info.is_undefined());
}

#[wasm_bindgen_test]
fn test_framebuffer_packed_after_reset() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.reset();
    let w = emu.get_framebuffer_width();
    let h = emu.get_framebuffer_height();
    let len = emu.get_framebuffer_len();
    // The packed buffer must satisfy len == w * h * 4 so the renderer
    // can upload it straight as a w x h RGBA texture.
    assert!(w > 0 && h > 0, "expected a non-empty active rectangle");
    assert_eq!(len, w * h * 4, "framebuffer must be tightly packed");
}

#[wasm_bindgen_test]
fn test_run_frame_keeps_framebuffer_packed() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.run_frame();
    let w = emu.get_framebuffer_width();
    let h = emu.get_framebuffer_height();
    assert_eq!(emu.get_framebuffer_len(), w * h * 4);
}

#[wasm_bindgen_test]
fn test_audio_drain_bounded() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.set_audio_enabled(true);
    emu.run_frame();
    let available = emu.get_audio_available();
    let pulled = emu.get_audio_samples_n(available + 1024);
    // Never returns more than was queued (no over-drain).
    assert!(pulled.len() <= available);
}

#[wasm_bindgen_test]
fn test_input_round_trip() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.set_input(0, 0x01);
    assert_eq!(emu.get_port_state(0), 0x01);
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
fn a_savestate_from_another_rom_is_refused_by_name_and_loads_when_asked() {
    let mut first = RetroEmulator::new("snes").unwrap();
    first.load_rom(&synth_lorom("FIRST BUILD")).unwrap();
    first.run_frame();
    let state = first.save_state();

    let mut second = RetroEmulator::new("snes").unwrap();
    second.load_rom(&synth_lorom("SECOND BUILD")).unwrap();
    let refusal: js_sys::Error = second
        .load_state(&state)
        .unwrap_err()
        .dyn_into()
        .expect("a refusal over the ROM is a JS Error");
    assert_eq!(String::from(refusal.name()), "SaveStateOtherRom");
    assert!(second.load_state_any_rom(&state).is_ok());

    // Its own ROM loads it, and bytes that are no savestate are refused
    // as text, as before.
    assert!(first.load_state(&state).is_ok());
    assert!(second
        .load_state(&[1, 2, 3])
        .unwrap_err()
        .as_string()
        .is_some());
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
fn test_match_spec_round_trips_through_js() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    let id = emu.add_breakpoint(0x7E0000, "write".to_string(), None);
    let spec = MatchSpec {
        value: Some(0x42),
        hit_skip: 3,
        one_shot: true,
        ..Default::default()
    };
    let spec_js = serde_wasm_bindgen::to_value(&spec).unwrap();
    emu.set_breakpoint_match_spec(id, spec_js).unwrap();
    let list: Vec<BreakpointInfo> = serde_wasm_bindgen::from_value(emu.list_breakpoints()).unwrap();
    let info = list.into_iter().find(|b| b.id == id).unwrap();
    assert_eq!(info.match_spec.value, Some(0x42));
    assert_eq!(info.match_spec.hit_skip, 3);
    assert!(info.match_spec.one_shot);
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
    assert!(!emu.get_dsp_voice_states().is_null());
    assert!(!emu.get_cartridge_manifest().is_null());
    assert!(!emu.export_oam_sprites().is_null());
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
fn test_load_low_confidence_rom_auto_falls_back() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    // A 32 KiB all-zero buffer scores below the detector's
    // confidence floor, so a plain detection load fails with
    // "no mapping...". loadRom must transparently retry with the
    // vector-implied LoROM mapping and succeed instead of bubbling
    // the error up to the console.
    let rom = vec![0u8; 0x8000];
    assert!(
        emu.load_rom(&rom).is_ok(),
        "low-confidence ROM should auto-load via the fallback"
    );

    // The fallback override is transient: a second load of the same
    // low-confidence ROM still works (it isn't relying on a leaked
    // staged override).
    assert!(emu.load_rom(&rom).is_ok(), "second auto-load still works");

    // A user-staged manifest stays authoritative: its failure is
    // surfaced verbatim, never replaced by the fallback. An invalid
    // preset passes JSON staging (parse_manifest only checks the
    // version) but is rejected at build time, and because a
    // manifest is staged, loadRom must NOT fall back, so the error
    // propagates.
    let bad = r#"{"core":"snes1","version":"1.0","rom":{"sha256":"","size":0},"mapping":{"preset":"bogus"}}"#;
    emu.set_cartridge_manifest(bad).unwrap();
    assert!(
        emu.load_rom(&rom).is_err(),
        "a staged manifest's failure must not be papered over by the fallback"
    );
    emu.clear_cartridge_manifest();

    // With the bad manifest cleared, the fallback is back in charge.
    assert!(emu.load_rom(&rom).is_ok(), "fallback resumes after clear");
}

#[wasm_bindgen_test]
fn test_raw_memory_views_have_expected_sizes() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    // VRAM = 0x8000 words = 0x10000 bytes; CGRAM = 256 colours;
    // OAM = 0x220 bytes; ARAM = 64 KiB.
    assert_eq!(emu.get_vram().len(), 0x10000);
    assert_eq!(emu.get_cgram().len(), 0x100);
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
fn test_ppu_registers_and_layers() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    assert!(!emu.get_ppu_registers().is_null());
    assert!(!emu.get_layer_info().is_null());
    // Toggling a layer must not panic and must be idempotent on read.
    emu.toggle_layer(0, false);
    emu.toggle_layer(0, true);
}

#[wasm_bindgen_test]
fn test_dsp_mute_and_waveforms() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.mute_dsp_channel(0, true);
    emu.mute_dsp_channel(0, false);
    // A muted/keyed-off voice reads as `samples` of silence; the mixed
    // waveform peek returns the requested length.
    assert_eq!(emu.get_dsp_waveform(0, 64).len(), 64);
    assert_eq!(emu.get_dsp_mixed_waveform(64).len(), 64);
}

// The host sends a pad's map as a plain object, whose keys JavaScript
// holds as text. Read as no map at all, it would silence the pad on
// every port when the controller settings are saved. SNES B is bit 5,
// A is bit 4.
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
fn gamepad_map_sent_as_a_plain_object_is_applied() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.gamepad_button("gamepad-0".to_string(), 0, true);
    assert_eq!(
        emu.get_port_state(0),
        1 << 5,
        "the default map presses B on player 1"
    );
    assert!(emu.set_gamepad_mapping("gamepad-0".to_string(), plain_gamepad_map(&[(0, 4)])));
    assert_eq!(emu.get_port_state(0), 1 << 4, "button 0 must now press A");
}

#[wasm_bindgen_test]
fn a_malformed_gamepad_map_keeps_the_current_one() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.gamepad_button("gamepad-0".to_string(), 0, true);
    assert!(!emu.set_gamepad_mapping("gamepad-0".to_string(), JsValue::from_str("nope")));
    assert_eq!(emu.get_port_state(0), 1 << 5);
}

// Reading a pad's map must not panic: a panic leaves the emulator
// refusing every later call, and netplay reads the map on every frame.
#[wasm_bindgen_test]
fn reading_a_gamepad_map_keeps_the_emulator_working() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.gamepad_button("gamepad-0".to_string(), 0, true);
    let map = emu.get_gamepad_mapping("gamepad-0".to_string());
    let b = js_sys::Reflect::get(&map, &JsValue::from_str("0")).unwrap();
    assert_eq!(b.as_f64(), Some(5.0), "button 0 reads back as SNES B");
    emu.set_input(0, 1);
    emu.key_down("ArrowUp".to_string());
    emu.run_frame();
}

#[wasm_bindgen_test]
fn a_gamepad_plays_player_one_beside_the_keyboard_and_can_move_to_player_two() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.gamepad_button("gamepad-0".to_string(), 0, true);
    assert_eq!(emu.get_provider_port("gamepad-0".to_string()), 0);
    emu.set_provider_port("gamepad-0".to_string(), 1);
    assert_eq!(emu.get_port_state(0), 0);
    assert_eq!(emu.get_port_state(1), 1 << 5);
    emu.swap_port_assignments();
    assert_eq!(
        emu.get_port_state(0),
        1 << 5,
        "the swap brings the pad back to player 1"
    );
    assert!(emu.add_gamepad("gamepad-1".to_string()));
    assert_eq!(js_sys::Array::from(&emu.get_port_providers(1)).length(), 1);
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
fn write_cgram_reads_back_and_shows_after_a_repaint() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    // Colour 255, then on round to colour 0; bit 15 is dropped.
    emu.write_cgram(255, &[0xFFFF, 0x001F]);
    let cgram = emu.get_cgram();
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

/// A property of a JS object.
fn property(object: &JsValue, key: &str) -> JsValue {
    js_sys::Reflect::get(object, &JsValue::from_str(key)).unwrap()
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
fn the_cartridge_manifest_is_plain_objects_all_the_way_down() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&synth_lorom("MANIFEST")).unwrap();
    let manifest = emu.get_cartridge_manifest();
    assert_plain_object(&manifest, "memoryMap");
    assert_plain_object(&property(&manifest, "memoryMap"), "regions");
    assert_plain_object(&property(&manifest, "mapperState"), "mapper_id");
    assert!(js_sys::Array::is_array(&property(&manifest, "romRegions")));
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
