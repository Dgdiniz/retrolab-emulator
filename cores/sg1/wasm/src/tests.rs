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
fn the_cartridge_hash_is_the_sha256_of_the_file_loaded() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    assert_eq!(emu.get_cartridge_hash(), None, "no load yet");
    let rom = synth_genesis_rom("HASHED GAME");
    emu.load_rom(&rom).unwrap();
    assert_eq!(
        emu.get_cartridge_hash(),
        Some(retro_core::cartridge::rom_sha256(&rom))
    );
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

/// A pad's map as the host sends it: a plain object, whose keys
/// JavaScript holds as text.
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

/// What the bindings answer about the machine, recorded.
mod machine_baseline;

/// What the bindings answer, method by method, recorded: the machine and
/// what each call leaves, the display, the audio, an input session, the
/// debugger's memory and disassembly, the VDP's and the sound chips'
/// readers, and the cartridge's.
mod binding_baseline;
