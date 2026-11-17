use super::*;
use wasm_bindgen_test::*;

#[wasm_bindgen_test]
fn test_emulator_creation() {
    let emu = RetroEmulator::new("genesis");
    assert!(emu.is_ok());
}

#[wasm_bindgen_test]
fn test_unknown_system() {
    let emu = RetroEmulator::new("unknown");
    assert!(emu.is_err());
}

#[wasm_bindgen_test]
fn test_get_system_info() {
    let emu = RetroEmulator::new("genesis").unwrap();
    let info = emu.get_system_info();
    assert!(!info.is_null());
}

/// Build a minimal valid Genesis ROM (header + reset vectors).
fn synth_genesis_rom(title: &str) -> Vec<u8> {
    let mut rom = vec![0u8; 0x80000];
    // Reset vectors: SSP then PC=0x400.
    rom[4..8].copy_from_slice(&0x0000_0400u32.to_be_bytes());
    rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
    let t = format!("{title:<48}");
    rom[0x120..0x150].copy_from_slice(&t.as_bytes()[..48]);
    rom[0x150..0x180].copy_from_slice(&t.as_bytes()[..48]);
    rom[0x1F0..0x1F3].copy_from_slice(b"JUE");
    rom
}

#[wasm_bindgen_test]
fn test_cartridge_manifest_round_trip() {
    let mut emu = RetroEmulator::new("genesis").unwrap();

    // No ROM loaded -> getDetectedManifest is JSON null; the installed
    // manifest view still serializes (empty map plan).
    assert_eq!(emu.get_detected_manifest().unwrap(), "null");
    assert!(!emu.get_cartridge_manifest().is_null());
    assert!(emu.get_memory_map().is_ok());

    // detectManifest on raw bytes always yields a manifest.
    let rom = synth_genesis_rom("PLAIN GAME");
    let json = emu.detect_manifest(&rom).unwrap();
    assert!(json.contains("\"preset\":\"plain\""));

    // A valid override parses; a malformed one is a JS Error.
    emu.set_cartridge_manifest(&json).unwrap();
    assert!(emu.set_cartridge_manifest("{ not json }").is_err());
    // An unsupported schema version is rejected too.
    assert!(emu
            .set_cartridge_manifest(
                r#"{"core":"sg1","version":"9.9","rom":{"sha256":"x","size":1},"mapping":{"preset":"plain"}}"#
            )
            .is_err());
    emu.clear_cartridge_manifest();
}

#[wasm_bindgen_test]
fn test_detected_manifest_reflects_loaded_rom() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    let rom = synth_genesis_rom("LOADED GAME");
    emu.load_rom(&rom).unwrap();
    let json = emu.get_detected_manifest().unwrap();
    assert_ne!(json, "null");
    assert!(json.contains("LOADED GAME"));
}

#[wasm_bindgen_test]
fn a_savestate_from_another_rom_is_refused_by_name_and_loads_when_asked() {
    let mut first = RetroEmulator::new("genesis").unwrap();
    first.load_rom(&synth_genesis_rom("FIRST BUILD")).unwrap();
    first.run_frame();
    let state = first.save_state();

    let mut second = RetroEmulator::new("genesis").unwrap();
    second.load_rom(&synth_genesis_rom("SECOND BUILD")).unwrap();
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
fn test_vdp_registers_and_layers() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    assert!(!emu.get_vdp_registers().is_null());
    assert!(!emu.get_layer_info().is_null());
    // Toggling a layer must not panic and must be idempotent on read.
    emu.toggle_layer(0, false);
    emu.toggle_layer(0, true);
}

#[wasm_bindgen_test]
fn test_match_spec_round_trips_and_event_catalog() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    let id = emu.add_breakpoint(0xFF0000, "write".to_string(), None);
    let spec = MatchSpec {
        address_end: Some(0xFF000F),
        value_size: 2,
        expr: Some("d0 == 5".to_string()),
        ..Default::default()
    };
    let spec_js = serde_wasm_bindgen::to_value(&spec).unwrap();
    emu.set_breakpoint_match_spec(id, spec_js).unwrap();
    let list: Vec<BreakpointInfo> = serde_wasm_bindgen::from_value(emu.list_breakpoints()).unwrap();
    let info = list.into_iter().find(|b| b.id == id).unwrap();
    assert_eq!(info.match_spec.address_end, Some(0xFF000F));
    assert_eq!(info.match_spec.value_size, 2);
    assert_eq!(info.match_spec.expr.as_deref(), Some("d0 == 5"));
    // The Genesis event taxonomy reaches JavaScript.
    let events: Vec<EventClass> =
        serde_wasm_bindgen::from_value(emu.get_available_events()).unwrap();
    let labels: Vec<&str> = events.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "VBlank",
            "VInt",
            "HInt",
            "Scanline",
            "Transfer complete",
            "Palette write",
            "Sprite table transfer",
            "Tile rewritten",
        ]
    );
    let codes: Vec<u32> = events.iter().map(|e| e.code).collect();
    assert_eq!(codes, [1, 2, 3, 4, 5, 6, 7, 8]);
}

#[wasm_bindgen_test]
fn test_framebuffer() {
    let emu = RetroEmulator::new("genesis").unwrap();
    let fb_js = emu.get_framebuffer();
    // Just check that we got a JsValue (can't easily deserialize in test)
    assert!(!fb_js.is_null());
    assert!(!fb_js.is_undefined());
}

#[wasm_bindgen_test]
fn test_keyboard_input() {
    let mut emu = RetroEmulator::new("genesis").unwrap();

    // Send key down event
    emu.key_down("ArrowUp".to_string());

    // Get port state
    let state = emu.get_port_state(0);

    // Should have some input (keyboard is assigned to port 0 by default)
    assert!(state != 0, "Port 0 should have input after key down");
}

#[wasm_bindgen_test]
fn test_keyboard_key_up() {
    let mut emu = RetroEmulator::new("genesis").unwrap();

    // Press and release
    emu.key_down("ArrowUp".to_string());
    emu.key_up("ArrowUp".to_string());

    // State should be 0 after release
    let state = emu.get_port_state(0);
    assert_eq!(state, 0, "Port 0 should have no input after key up");
}

#[wasm_bindgen_test]
fn test_gamepad_button() {
    let mut emu = RetroEmulator::new("genesis").unwrap();

    // Add gamepad and assign to port 1
    // Note: In real use, JS would call this through the API
    emu.gamepad_button("gamepad-0".to_string(), 0, true);

    // Get input providers
    let providers = emu.get_input_providers();
    assert!(!providers.is_null());
}

#[wasm_bindgen_test]
fn test_gamepad_axis() {
    let mut emu = RetroEmulator::new("genesis").unwrap();

    // Update axis
    emu.gamepad_axis("gamepad-0".to_string(), 0, -0.8);

    // Should not panic
    let providers = emu.get_input_providers();
    assert!(!providers.is_null());
}

#[wasm_bindgen_test]
fn test_input_history() {
    let emu = RetroEmulator::new("genesis").unwrap();

    // Get history
    let history = emu.get_input_history(10);
    assert!(!history.is_null());
}

#[wasm_bindgen_test]
fn test_port_assignment() {
    let emu = RetroEmulator::new("genesis").unwrap();

    // Get port 0 assignment (should be keyboard by default)
    let assignment = emu.get_port_assignment(0);
    assert!(!assignment.is_null());
}

#[wasm_bindgen_test]
fn test_get_input_providers() {
    let emu = RetroEmulator::new("genesis").unwrap();

    let providers = emu.get_input_providers();
    assert!(!providers.is_null());
    assert!(!providers.is_undefined());
}

// The host sends a pad's map as a plain object, whose keys JavaScript
// holds as text. Read as no map at all, it would silence the pad on
// every port when the controller settings are saved.
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
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.gamepad_button("gamepad-0".to_string(), 0, true);
    // Genesis A is bit 6, C is bit 5.
    assert!(emu.set_gamepad_mapping("gamepad-0".to_string(), plain_gamepad_map(&[(0, 5)])));
    assert_eq!(
        emu.get_port_state(0),
        1 << 5,
        "button 0 must now press C on player 1"
    );
}

#[wasm_bindgen_test]
fn gamepad_map_sent_as_a_map_with_number_keys_is_applied() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.gamepad_button("gamepad-0".to_string(), 0, true);
    let map = js_sys::Map::new();
    map.set(&JsValue::from(0), &JsValue::from(5));
    assert!(emu.set_gamepad_mapping("gamepad-0".to_string(), map.into()));
    assert_eq!(emu.get_port_state(0), 1 << 5);
}

#[wasm_bindgen_test]
fn a_malformed_gamepad_map_keeps_the_current_one() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.gamepad_button("gamepad-0".to_string(), 0, true);
    assert!(!emu.set_gamepad_mapping("gamepad-0".to_string(), JsValue::from_str("nope")));
    assert_eq!(
        emu.get_port_state(0),
        1 << 6,
        "the default map still presses A"
    );
}

// Reading a pad's map must not panic: a panic leaves the emulator
// refusing every later call, and netplay reads the map on every frame.
#[wasm_bindgen_test]
fn reading_a_gamepad_map_keeps_the_emulator_working() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.gamepad_button("gamepad-0".to_string(), 0, true);
    let map = emu.get_gamepad_mapping("gamepad-0".to_string());
    let a = js_sys::Reflect::get(&map, &JsValue::from_str("0")).unwrap();
    assert_eq!(a.as_f64(), Some(6.0), "button 0 reads back as Genesis A");
    emu.set_input(0, 1);
    emu.key_down("ArrowUp".to_string());
    emu.run_frame();
}

#[wasm_bindgen_test]
fn a_gamepad_plays_player_one_beside_the_keyboard_and_can_move_to_player_two() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.gamepad_button("gamepad-0".to_string(), 0, true);
    assert_eq!(emu.get_provider_port("gamepad-0".to_string()), 0);
    assert_eq!(emu.get_port_state(0), 1 << 6);

    emu.set_provider_port("gamepad-0".to_string(), 1);
    assert_eq!(emu.get_port_state(0), 0);
    assert_eq!(emu.get_port_state(1), 1 << 6);

    emu.set_provider_port("gamepad-0".to_string(), -1);
    assert_eq!(emu.get_provider_port("gamepad-0".to_string()), -1);
    assert_eq!(
        emu.get_port_state(1),
        0,
        "a pad on no port reaches no player"
    );
}

#[wasm_bindgen_test]
fn a_pad_added_before_its_first_input_stays_where_the_host_put_it() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    assert!(emu.add_gamepad("gamepad-0".to_string()));
    emu.set_provider_port("gamepad-0".to_string(), 1);
    emu.gamepad_button("gamepad-0".to_string(), 0, true);
    assert_eq!(emu.get_port_state(1), 1 << 6);
    let providers = emu.get_port_providers(1);
    assert_eq!(js_sys::Array::from(&providers).length(), 1);
}

/// Assert `value` is a plain object, not a `Map`, holding every key of
/// `keys`: what the app's readers find with `value[key]`.
fn assert_plain_object(value: &JsValue, keys: &[&str], reader: &str) {
    assert!(value.is_object(), "{reader}: an object");
    assert!(
        !value.is_instance_of::<js_sys::Map>(),
        "{reader}: a plain object, not a Map"
    );
    for key in keys {
        let field = js_sys::Reflect::get(value, &JsValue::from_str(key)).unwrap();
        assert!(!field.is_undefined(), "{reader}: `{key}` is there");
    }
}

/// An emulator with a plain cartridge loaded.
fn with_plain_cartridge() -> RetroEmulator {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.load_rom(&synth_genesis_rom("PLAIN GAME")).unwrap();
    emu
}

/// An emulator with an SVP cartridge loaded (the `SV` header tag).
fn with_svp_cartridge() -> RetroEmulator {
    let mut rom = synth_genesis_rom("SVP GAME");
    rom[0x1C8..0x1CA].copy_from_slice(b"SV");
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.load_rom(&rom).unwrap();
    emu
}

#[wasm_bindgen_test]
fn get_mapper_state_is_a_plain_object() {
    let mut emu = with_plain_cartridge();
    let state = emu.get_mapper_state().unwrap();
    assert_plain_object(
        &state,
        &["mapper_type", "registers", "banks"],
        "getMapperState",
    );
}

#[wasm_bindgen_test]
fn get_chip_state_is_a_plain_object() {
    let emu = with_svp_cartridge();
    let state = emu.get_chip_state().unwrap();
    assert_plain_object(
        &state,
        &["chip_type", "registers", "cycle_count"],
        "getChipState",
    );
    let registers = js_sys::Array::from(
        &js_sys::Reflect::get(&state, &JsValue::from_str("registers")).unwrap(),
    );
    assert!(registers.length() > 0);
    assert_plain_object(
        &registers.get(0),
        &["address", "name", "value"],
        "getChipState register",
    );
}

#[wasm_bindgen_test]
fn get_chip_states_holds_plain_objects() {
    let emu = with_svp_cartridge();
    let rows = js_sys::Array::from(&emu.get_chip_states().unwrap());
    assert_eq!(rows.length(), 1);
    let row = rows.get(0);
    assert_plain_object(&row, &["chipId", "label", "state"], "getChipStates");
    let state = js_sys::Reflect::get(&row, &JsValue::from_str("state")).unwrap();
    assert_plain_object(&state, &["chip_type"], "getChipStates state");
}

#[wasm_bindgen_test]
fn get_memory_map_is_a_plain_object() {
    let emu = with_plain_cartridge();
    let map = emu.get_memory_map().unwrap();
    assert_plain_object(&map, &["regions"], "getMemoryMap");
    let regions =
        js_sys::Array::from(&js_sys::Reflect::get(&map, &JsValue::from_str("regions")).unwrap());
    assert!(regions.length() > 0);
    assert_plain_object(
        &regions.get(0),
        &["label", "start", "end", "detail"],
        "getMemoryMap region",
    );
}

#[wasm_bindgen_test]
fn get_cartridge_manifest_is_a_plain_object() {
    let mut emu = with_plain_cartridge();
    let manifest = emu.get_cartridge_manifest();
    assert_plain_object(
        &manifest,
        &["memoryMap", "romRegions", "sramRegions", "mapperState"],
        "getCartridgeManifest",
    );
    let mapper = js_sys::Reflect::get(&manifest, &JsValue::from_str("mapperState")).unwrap();
    assert_plain_object(
        &mapper,
        &["mapper_type"],
        "getCartridgeManifest mapperState",
    );
}

#[wasm_bindgen_test]
fn get_coverage_stats_is_a_plain_object() {
    let emu = with_plain_cartridge();
    assert_plain_object(
        &emu.get_coverage_stats(0),
        &["totalBytes", "coveredBytes", "percent"],
        "getCoverageStats",
    );
}

#[wasm_bindgen_test]
fn get_ym2612_lfo_state_is_a_plain_object() {
    let emu = with_plain_cartridge();
    assert_plain_object(
        &emu.get_ym2612_lfo_state(),
        &["enabled", "frequency"],
        "getYm2612LfoState",
    );
}

#[wasm_bindgen_test]
fn get_ym2612_dac_state_is_a_plain_object() {
    let emu = with_plain_cartridge();
    assert_plain_object(
        &emu.get_ym2612_dac_state(),
        &["enabled"],
        "getYm2612DacState",
    );
}

#[wasm_bindgen_test]
fn get_ym2612_timer_a_state_is_a_plain_object() {
    let emu = with_plain_cartridge();
    assert_plain_object(
        &emu.get_ym2612_timer_a_state(),
        &["running", "period", "overflowed"],
        "getYm2612TimerAState",
    );
}

#[wasm_bindgen_test]
fn get_ym2612_timer_b_state_is_a_plain_object() {
    let emu = with_plain_cartridge();
    assert_plain_object(
        &emu.get_ym2612_timer_b_state(),
        &["running", "period", "overflowed"],
        "getYm2612TimerBState",
    );
}

/// Place `words` at `address` of `rom`, high byte first.
fn place(rom: &mut [u8], address: usize, words: &[u16]) {
    for (i, word) in words.iter().enumerate() {
        let at = address + i * 2;
        rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
}

/// `value[key]`.
fn field(value: &JsValue, key: &str) -> JsValue {
    js_sys::Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

/// Hold each value of `found` to the description recorded for it. When
/// they differ, the descriptions found are written to the console as
/// the entries of `expected`.
fn assert_shapes<S: AsRef<str>>(found: &[(S, JsValue)], expected: &[(&str, &str)]) {
    let shapes: Vec<(&str, String)> = found
        .iter()
        .map(|(what, value)| (what.as_ref(), retro_wasm::test_support::shape(value)))
        .collect();
    let shapes: Vec<(&str, &str)> = shapes
        .iter()
        .map(|(what, shape)| (*what, shape.as_str()))
        .collect();
    if shapes != expected {
        for (what, shape) in &shapes {
            console_log!("SHAPE ({what:?}, {shape:?}),");
        }
    }
    assert_eq!(shapes, expected);
}

mod capture;

/// What the debugger's readers, the steps and the Z80's trace methods
/// return, each held to a description recorded from the bindings, on a
/// program with an execute breakpoint, a write watch, an event
/// breakpoint, a stepped history and a chunk of trace on both CPUs.
mod debugger_shape_tests;
