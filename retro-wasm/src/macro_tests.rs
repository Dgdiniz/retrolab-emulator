//! The binding macros, each expanded on a struct over [`FakeMachine`], and
//! what their methods hand a host: the values, the named refusals, the
//! present buffer, the load's retry and which calls refresh the picture.

use js_sys::{Array, Map, Object, Reflect, Uint8Array};
use retro_core::cartridge::{MapperId, MapperState};
use retro_core::EmulatorCore;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::*;

use crate::display::{pack_active_rgba, PresentBuffer};
use crate::fake_machine::{
    FakeMachine, FRAMEBUFFER_HEIGHT, FRAMEBUFFER_WIDTH, NEEDS_A_CHIP, PICTURE_HEIGHT,
    PICTURE_WIDTH, SCORES_TOO_LOW,
};
use crate::js::to_js;
use crate::test_support::shape;

/// Every macro on a machine with no present buffer and no hook.
#[wasm_bindgen]
pub struct PlainEmulator {
    core: FakeMachine,
}

crate::machine_bindings!(PlainEmulator, core);
crate::display_bindings!(PlainEmulator, core);
crate::audio_bindings!(PlainEmulator, core);
crate::input_bindings!(PlainEmulator, core);
crate::debugger_bindings!(PlainEmulator, core);
crate::video_bindings!(PlainEmulator, core);
crate::cartridge_bindings!(PlainEmulator, core, mapper_label = label_in_capitals);

/// The machine and picture macros with a present buffer, a hook that counts
/// its calls, and the load's retry.
#[wasm_bindgen]
pub struct PresentingEmulator {
    core: FakeMachine,
    present: PresentBuffer,
    refreshes: u32,
}

impl PresentingEmulator {
    fn refresh(&mut self) {
        self.refreshes += 1;
        self.present.refresh(
            EmulatorCore::get_framebuffer(&self.core),
            EmulatorCore::get_display_info(&self.core),
        );
    }
}

crate::machine_bindings!(
    PresentingEmulator,
    core,
    after_frame = refresh,
    retry_detected_mapping = true
);
crate::display_bindings!(PresentingEmulator, core, present = present);

/// A mapper label of the test's own: the default label in capitals.
fn label_in_capitals(id: &MapperId) -> String {
    crate::cartridge::default_mapper_label(id).to_uppercase()
}

fn plain() -> PlainEmulator {
    PlainEmulator {
        core: FakeMachine::new(),
    }
}

fn presenting() -> PresentingEmulator {
    PresentingEmulator {
        core: FakeMachine::new(),
        present: PresentBuffer::new(),
        refreshes: 0,
    }
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn number(value: &JsValue, key: &str) -> f64 {
    get(value, key).as_f64().unwrap()
}

/// The name of a refusal's `Error`, `None` for a refusal that is text.
fn error_name(refusal: &JsValue) -> Option<String> {
    refusal
        .dyn_ref::<js_sys::Error>()
        .map(|error| String::from(error.name()))
}

fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let obj = Object::new();
    for (key, value) in entries {
        Reflect::set(&obj, &JsValue::from_str(key), value).unwrap();
    }
    obj.into()
}

// ---- the machine

#[wasm_bindgen_test]
fn the_picture_is_refreshed_after_each_call_that_can_change_it() {
    let mut emu = presenting();
    let rom = [1, 2, 3];
    let count = |emu: &PresentingEmulator| emu.refreshes;

    emu.load_rom(&rom).unwrap();
    assert_eq!(count(&emu), 1, "a load");
    emu.run_frame();
    emu.run_until_vblank();
    assert_eq!(count(&emu), 3, "the frames");
    emu.reset();
    emu.hard_reset();
    emu.power_on_reset();
    assert_eq!(count(&emu), 6, "the three resets");
    emu.set_region("pal").unwrap();
    assert_eq!(count(&emu), 7, "a region change");
    let state = emu.save_state();
    assert_eq!(count(&emu), 8, "a save");
    emu.load_state(&state).unwrap();
    emu.load_state_any_rom(&state).unwrap();
    assert_eq!(count(&emu), 10, "the two loads");

    emu.run_frame_headless();
    emu.set_show_overscan(true);
    assert!(emu.load_rom(&[]).is_err());
    assert!(emu.set_region("secam").is_err());
    assert!(emu.load_state(&[1, 2, 3]).is_err());
    assert_eq!(
        count(&emu),
        10,
        "no refresh after a headless frame or a refusal"
    );
}

#[wasm_bindgen_test]
fn a_headless_frame_leaves_the_presented_picture_as_it_was() {
    let mut emu = presenting();
    emu.load_rom(&[1]).unwrap();
    emu.run_frame();
    let shown = emu.get_framebuffer_data().to_vec();
    assert!(shown.iter().all(|&byte| byte == 1));
    emu.run_frame_headless();
    assert_eq!(emu.get_framebuffer_data().to_vec(), shown);
    assert_eq!(emu.get_frame_number(), 2);
}

#[wasm_bindgen_test]
fn a_rom_detection_refuses_is_loaded_again_with_the_detected_mapping() {
    let rom = [SCORES_TOO_LOW, 0, 0];
    let mut retrying = presenting();
    retrying.load_rom(&rom).unwrap();
    assert!(
        retrying.core.cartridge_manifest_override().is_none(),
        "the detected mapping is staged for that load only"
    );

    let mut plain = plain();
    let refusal = plain.load_rom(&rom).unwrap_err();
    assert_eq!(
        refusal.as_string().as_deref(),
        Some("no mapping: detector score below threshold")
    );

    let mut staged = presenting();
    staged
        .core
        .set_cartridge_manifest_override(Some(serde_json::json!({ "preset": "mine" })));
    let refusal = staged.load_rom(&[]).unwrap_err();
    assert_eq!(refusal.as_string().as_deref(), Some("empty file"));
    assert_eq!(
        staged.core.cartridge_manifest_override(),
        Some(&serde_json::json!({ "preset": "mine" })),
        "a staged manifest is never replaced"
    );
}

#[wasm_bindgen_test]
fn a_rom_whose_chip_this_build_lacks_is_refused_by_name() {
    for refusal in [
        plain().load_rom(&[NEEDS_A_CHIP]).unwrap_err(),
        presenting().load_rom(&[NEEDS_A_CHIP]).unwrap_err(),
    ] {
        assert_eq!(error_name(&refusal).as_deref(), Some("UnsupportedChip"));
        assert_eq!(
            get(&refusal, "chip").as_string().as_deref(),
            Some("Fake chip")
        );
        assert_eq!(
            refusal
                .dyn_ref::<js_sys::Error>()
                .map(|e| String::from(e.message())),
            Some("this build has no module for the fake chip".to_string())
        );
    }
}

#[wasm_bindgen_test]
fn a_state_of_another_rom_is_refused_by_name_and_loads_when_asked() {
    let mut emu = plain();
    emu.load_rom(&[1]).unwrap();
    emu.run_frame();
    let state = emu.save_state();
    emu.load_rom(&[2]).unwrap();
    let refusal = emu.load_state(&state).unwrap_err();
    assert_eq!(error_name(&refusal).as_deref(), Some("SaveStateOtherRom"));
    emu.load_state_any_rom(&state).unwrap();
    assert_eq!(emu.get_frame_number(), 1);
}

#[wasm_bindgen_test]
fn the_machine_readers_answer_the_core() {
    let mut emu = plain();
    assert!(emu.get_rom_info().is_undefined(), "no ROM yet");
    emu.load_rom(&[7, 7]).unwrap();
    assert_eq!(
        get(&emu.get_rom_info(), "title").as_string().as_deref(),
        Some("FAKE")
    );
    assert_eq!(
        get(&emu.get_system_info(), "short_name")
            .as_string()
            .as_deref(),
        Some("fake")
    );
    assert_eq!(emu.get_region(), "ntsc");
    emu.set_region("PAL").unwrap();
    assert_eq!(emu.get_region(), "pal");
    assert_eq!(emu.get_build_id(), crate::build_id("fake"));
    emu.run_frame();
    assert_eq!(emu.get_state_hash(), EmulatorCore::state_hash(&emu.core));
    assert_eq!(emu.get_state_hash_parts(), "{\"frames\":\"1\"}");
    emu.set_save_ram(&[1, 2]);
    assert_eq!(emu.get_save_ram(), Some(vec![1, 2]));
    assert!(!emu.is_show_overscan_enabled());
    emu.set_show_overscan(true);
    assert!(emu.is_show_overscan_enabled());
}

#[wasm_bindgen_test]
fn the_constructors_refusal_names_the_systems_it_answers_to() {
    let refusal = crate::machine::unknown_system("nes", &["genesis", "megadrive", "md"]);
    assert_eq!(
        refusal.as_string().as_deref(),
        Some("Unknown system: 'nes'. Supported: genesis, megadrive, md")
    );
}

#[wasm_bindgen_test]
fn the_hash_parts_are_a_json_object_of_hex_in_their_order() {
    assert_eq!(
        crate::machine::state_hash_parts_json(&[("cpu", 255), ("ram", 0)]),
        "{\"cpu\":\"ff\",\"ram\":\"0\"}"
    );
    assert_eq!(crate::machine::state_hash_parts_json(&[]), "{}");
}

// ---- the picture

#[wasm_bindgen_test]
fn a_present_buffer_packs_the_picture_out_of_a_wider_framebuffer() {
    let mut framebuffer = retro_core::Framebuffer::new(FRAMEBUFFER_WIDTH, FRAMEBUFFER_HEIGHT);
    for (i, byte) in framebuffer.data.iter_mut().enumerate() {
        *byte = i as u8;
    }
    let display = EmulatorCore::get_display_info(&FakeMachine::new()).clone();
    let mut present = PresentBuffer::new();
    present.refresh(&framebuffer, &display);
    assert_eq!(
        (present.width(), present.height()),
        (PICTURE_WIDTH, PICTURE_HEIGHT)
    );
    let stride = FRAMEBUFFER_WIDTH as usize * 4;
    let row = PICTURE_WIDTH as usize * 4;
    let expected: Vec<u8> = (0..PICTURE_HEIGHT as usize)
        .flat_map(|y| framebuffer.data[y * stride..y * stride + row].to_vec())
        .collect();
    assert_eq!(present.data(), &expected[..]);

    // Rows the framebuffer does not hold are left out, and a display wider
    // than the framebuffer is cut to it.
    framebuffer.data.truncate(stride * 2);
    let mut wide = display.clone();
    wide.width = 100;
    let mut out = Vec::new();
    assert_eq!(
        pack_active_rgba(&framebuffer, &wide, &mut out),
        (FRAMEBUFFER_WIDTH, 2)
    );
    assert_eq!(out, framebuffer.data);
}

#[wasm_bindgen_test]
fn the_fast_readers_read_the_framebuffer_or_the_present_buffer() {
    let mut framebuffer_reader = plain();
    framebuffer_reader.load_rom(&[1]).unwrap();
    framebuffer_reader.run_frame();
    assert_eq!(
        framebuffer_reader.get_framebuffer_width(),
        FRAMEBUFFER_WIDTH
    );
    assert_eq!(
        framebuffer_reader.get_framebuffer_height(),
        FRAMEBUFFER_HEIGHT
    );
    assert_eq!(
        framebuffer_reader.get_framebuffer_len(),
        FRAMEBUFFER_WIDTH * FRAMEBUFFER_HEIGHT * 4
    );

    let mut present_reader = presenting();
    present_reader.load_rom(&[1]).unwrap();
    present_reader.run_frame();
    assert_eq!(present_reader.get_framebuffer_width(), PICTURE_WIDTH);
    assert_eq!(present_reader.get_framebuffer_height(), PICTURE_HEIGHT);
    assert_eq!(
        present_reader.get_framebuffer_len(),
        PICTURE_WIDTH * PICTURE_HEIGHT * 4
    );
    assert_eq!(
        present_reader.get_framebuffer_ptr(),
        present_reader.present.data().as_ptr()
    );
    assert_eq!(
        number(&present_reader.get_display_info(), "width") as u32,
        PICTURE_WIDTH,
        "the display info is the core's own"
    );
    assert_eq!(
        number(&present_reader.get_framebuffer(), "width") as u32,
        FRAMEBUFFER_WIDTH,
        "the framebuffer object is the core's own"
    );
}

// ---- the host's sound

#[wasm_bindgen_test]
fn the_host_ring_gives_what_it_holds_and_counts_what_it_dropped() {
    let mut emu = plain();
    emu.load_rom(&[1]).unwrap();
    assert!(!emu.is_audio_enabled());
    emu.run_frame();
    assert_eq!(emu.get_audio_available(), 0, "no sound while it is off");
    emu.set_audio_enabled(true);
    for _ in 0..3 {
        emu.run_frame();
    }
    assert_eq!(emu.get_audio_available(), 6);
    assert_eq!(emu.get_audio_samples_n(4), vec![0.25, -0.25, 0.25, -0.25]);
    emu.clear_audio_buffer();
    assert_eq!(emu.get_audio_available(), 2, "clearing is a no-op");
    assert_eq!(
        emu.get_audio_samples_n(10).len(),
        2,
        "never more than queued"
    );
    for _ in 0..5 {
        emu.run_frame();
    }
    assert_eq!(emu.get_audio_ring_overflows(), 2);
    assert_eq!(emu.get_audio_samples().len(), 8);
    assert_eq!(emu.get_audio_available(), 0);
    emu.discard_pending_audio();
    assert_eq!(number(&emu.get_audio_info(), "sample_rate"), 44100.0);
}

// ---- the input

#[wasm_bindgen_test]
fn a_gamepad_plays_player_one_beside_the_keyboard_and_can_move_to_player_two() {
    let mut emu = plain();
    assert!(emu.add_gamepad("pad".to_string()));
    assert!(!emu.add_gamepad("pad".to_string()), "known already");
    emu.set_provider_port("pad".to_string(), 0);
    emu.gamepad_button("pad".to_string(), 12, true);
    emu.key_down("KeyZ".to_string());
    assert_eq!(emu.get_port_state(0), 0b1_0001, "both feed player 1");
    assert_eq!(emu.get_provider_port("pad".to_string()), 0);

    emu.set_provider_port("pad".to_string(), 1);
    assert_eq!(emu.get_port_state(0), 0b1_0000);
    assert_eq!(emu.get_port_state(1), 0b0_0001);
    assert_eq!(emu.get_provider_port("pad".to_string()), 1);
    let providers = Array::from(&emu.get_port_providers(1));
    assert_eq!(providers.get(0).as_string().as_deref(), Some("pad"));

    emu.set_provider_port("pad".to_string(), -1);
    assert_eq!(emu.get_provider_port("pad".to_string()), -1);
    emu.key_up("KeyZ".to_string());
    emu.clear_gamepad_state("pad".to_string());
    assert_eq!(emu.get_port_state(0), 0);
}

#[wasm_bindgen_test]
fn a_gamepad_map_is_taken_as_a_plain_object_or_a_map_with_number_keys() {
    let mut emu = plain();
    emu.add_gamepad("pad".to_string());
    assert!(emu.set_gamepad_mapping("pad".to_string(), object(&[("3", JsValue::from(6))])));
    let read = emu.get_gamepad_mapping("pad".to_string());
    assert!(!read.is_instance_of::<Map>(), "read back as a plain object");
    assert_eq!(number(&read, "3"), 6.0);

    let map = Map::new();
    map.set(&JsValue::from(5), &JsValue::from(2));
    assert!(emu.set_gamepad_mapping("pad".to_string(), map.into()));
    assert_eq!(
        number(&emu.get_gamepad_mapping("pad".to_string()), "5"),
        2.0
    );
}

#[wasm_bindgen_test]
fn a_malformed_map_keeps_the_one_in_use() {
    let mut emu = plain();
    emu.add_gamepad("pad".to_string());
    assert!(emu.set_gamepad_mapping("pad".to_string(), object(&[("3", JsValue::from(6))])));
    assert!(!emu.set_gamepad_mapping("pad".to_string(), JsValue::from_str("nonsense")));
    assert_eq!(
        number(&emu.get_gamepad_mapping("pad".to_string()), "3"),
        6.0
    );

    assert!(!emu.set_keyboard_mapping(JsValue::from(7)));
    assert_eq!(number(&emu.get_keyboard_mapping(), "KeyZ"), 4.0);
    assert!(!emu.enable_split_keyboard(object(&[("KeyQ", JsValue::from(4))]), JsValue::NULL));
    assert!(!emu.is_split_keyboard_enabled());
}

#[wasm_bindgen_test]
fn the_keyboard_and_its_halves_answer_their_maps() {
    let mut emu = plain();
    assert!(emu.set_keyboard_mapping(object(&[("KeyX", JsValue::from(5))])));
    assert_eq!(number(&emu.get_keyboard_mapping(), "KeyX"), 5.0);
    assert!(emu.enable_split_keyboard(
        object(&[("KeyQ", JsValue::from(4))]),
        object(&[("KeyP", JsValue::from(4))]),
    ));
    assert!(emu.is_split_keyboard_enabled());
    emu.disable_split_keyboard();
    assert!(!emu.is_split_keyboard_enabled());
    emu.set_gamepad_deadzone("pad".to_string(), 0.5);
    emu.clear_keyboard_state();
    emu.set_input(1, 0b100);
    assert_eq!(emu.get_port_state(1), 0b100);
    emu.swap_port_assignments();
    assert!(Array::is_array(&emu.get_input_providers()));
    assert!(Array::is_array(&emu.get_input_history(4)));
    let _ = emu.get_port_assignment(0);
}

// ---- the video chip

#[wasm_bindgen_test]
fn the_video_readers_answer_the_chip() {
    let mut emu = plain();
    assert_eq!(emu.get_vram(), (0..32).collect::<Vec<u8>>());
    let layers = Array::from(&emu.get_layer_info());
    assert_eq!(layers.length(), 2);
    emu.toggle_layer(1, false);
    let layers = Array::from(&emu.get_layer_info());
    assert_eq!(get(&layers.get(1), "enabled").as_bool(), Some(false));
    assert_eq!(Array::from(&emu.get_sprite_data()).length(), 2);
    assert!(Array::is_array(&emu.resolved_sprite_render_order()));

    let sprite = emu.export_sprite_image(0);
    assert_eq!(
        (number(&sprite, "width"), number(&sprite, "height")),
        (1.0, 2.0)
    );
    assert_eq!(
        get(&sprite, "data")
            .dyn_into::<Uint8Array>()
            .unwrap()
            .to_vec(),
        vec![1, 2, 3, 255, 4, 5, 6, 0]
    );
    assert!(emu.export_sprite_image(1).is_null());
}

#[wasm_bindgen_test]
fn a_tile_decode_is_an_indexed_sheet_or_null() {
    let mut emu = plain();
    let sheet = emu.decode_tiles(0, 0, 3, 2, 4, 0);
    assert_eq!(
        (number(&sheet, "width"), number(&sheet, "height")),
        (8.0, 16.0)
    );
    assert_eq!(
        get(&sheet, "pixels")
            .dyn_into::<Uint8Array>()
            .unwrap()
            .length(),
        128
    );
    assert_eq!(number(&sheet, "bpp"), 4.0);
    assert_eq!(number(&sheet, "transparentIndex"), 0.0);
    assert!(
        emu.decode_tiles(1, 0, 0, 1, 8, 0).is_null(),
        "a chip with no Mode 7"
    );
    assert!(
        emu.decode_tiles(0, 0, 0, 1, 4, 1).is_null(),
        "a size the chip refuses"
    );
    assert!(
        emu.decode_tiles(2, 0, 0, 1, 4, 0).is_null(),
        "no such source"
    );
    assert!(emu.decode_tiles(0, 0, 0, 1, 4, 2).is_null(), "no such size");
}

// ---- the debugger's memory and disassembly

#[wasm_bindgen_test]
fn the_debuggers_memory_and_disassembly_answer_the_core() {
    let mut emu = plain();
    assert_eq!(emu.read_memory(4, 3), vec![4, 5, 6]);
    emu.write_memory_debug(5, &[9, 9]);
    assert_eq!(emu.read_memory(4, 3), vec![4, 9, 9]);
    assert_eq!(Array::from(&emu.disassemble_at(0x10, 2)).length(), 2);
    let rows = Array::from(&emu.disassemble_cpu(1, 0, 1));
    assert_eq!(
        get(&rows.get(0), "operands").as_string().as_deref(),
        Some("cpu1")
    );
    let window = Array::from(&emu.disassemble_window(0, 0x20, 1, 2));
    assert_eq!(window.length(), 4);
    assert_eq!(number(&window.get(0), "address"), 31.0);
}

// ---- the cartridge

#[wasm_bindgen_test]
fn the_cartridge_readers_answer_before_and_after_a_load() {
    let mut emu = plain();
    assert_eq!(emu.get_cartridge_hash(), None);
    assert_eq!(
        emu.get_cartridge_info().unwrap_err().as_string().as_deref(),
        Some("No cartridge loaded")
    );
    assert_eq!(emu.get_cartridge_title(), None);
    assert_eq!(emu.get_detected_manifest().unwrap(), "null");

    emu.load_rom(&[1, 2, 3, 4]).unwrap();
    assert_eq!(
        emu.get_cartridge_hash(),
        Some(format!("{:016x}", retro_core::hash::fnv1a64(&[1, 2, 3, 4])))
    );
    assert_eq!(
        get(&emu.get_cartridge_info().unwrap(), "size").as_f64(),
        Some(4.0)
    );
    assert_eq!(emu.get_cartridge_title().as_deref(), Some("FAKE"));
    assert!(!emu.is_cartridge_sram_dirty());
    emu.core.write_save();
    assert!(emu.is_cartridge_sram_dirty());
    emu.clear_cartridge_sram_dirty();
    assert!(!emu.is_cartridge_sram_dirty());
}

#[wasm_bindgen_test]
fn the_mapper_chips_and_map_are_plain_objects() {
    let mut emu = plain();
    emu.load_rom(&[1]).unwrap();
    let mapper = emu.get_mapper_state().unwrap();
    assert!(!mapper.is_instance_of::<Map>());
    assert_eq!(
        get(&mapper, "mapper_type").as_string().as_deref(),
        Some("None")
    );
    assert_eq!(Array::from(&get(&mapper, "registers")).length(), 0);
    assert_eq!(Array::from(&get(&mapper, "banks")).length(), 0);
    assert!(emu.get_chip_state().unwrap().is_null());
    assert_eq!(Array::from(&emu.get_chip_states().unwrap()).length(), 0);
    let map = emu.get_memory_map().unwrap();
    assert!(!map.is_instance_of::<Map>());
    assert_eq!(Array::from(&get(&map, "regions")).length(), 0);
}

#[wasm_bindgen_test]
fn a_mapper_is_labelled_by_the_crates_function_or_by_its_own_name() {
    let state = || {
        Some(MapperState {
            mapper_id: MapperId::Custom("svp".to_string()),
            registers: Vec::new(),
            bank_config: retro_core::cartridge::BankConfiguration { banks: Vec::new() },
        })
    };
    let own = crate::cartridge::mapper_state_value(state(), crate::cartridge::default_mapper_label);
    assert_eq!(own["mapper_type"], "svp");
    let crates = crate::cartridge::mapper_state_value(state(), label_in_capitals);
    assert_eq!(crates["mapper_type"], "SVP");
    let snes = crate::cartridge::default_mapper_label(&MapperId::SnesLoROM);
    assert_eq!(snes, "SnesLoROM");
}

#[wasm_bindgen_test]
fn the_chip_rows_carry_each_chips_label() {
    let state = retro_core::cartridge::ExpansionChipState {
        chip_type: retro_core::cartridge::ExpansionChipType::SVP,
        registers: Vec::new(),
        internal_ram: None,
        program_counter: Some(4),
        cycle_count: 0,
    };
    let rows = Array::from(&crate::cartridge::chip_state_rows(
        vec![(0, state.clone()), (3, state)],
        vec![(3, "DSP".to_string())],
    ));
    assert_eq!(rows.length(), 2);
    assert_eq!(number(&rows.get(1), "chipId"), 3.0);
    assert_eq!(
        get(&rows.get(1), "label").as_string().as_deref(),
        Some("DSP")
    );
    assert_eq!(get(&rows.get(0), "label").as_string().as_deref(), Some(""));
    assert!(!get(&rows.get(0), "state").is_instance_of::<Map>());
}

#[wasm_bindgen_test]
fn a_manifest_is_detected_staged_and_cleared() {
    let mut emu = plain();
    let detected: serde_json::Value =
        serde_json::from_str(&emu.detect_manifest(&[1, 2]).unwrap()).unwrap();
    assert_eq!(
        detected,
        serde_json::json!({ "preset": "plain", "rom_size": 2 })
    );

    let refusal = emu.set_cartridge_manifest("{ not json").unwrap_err();
    assert!(refusal.as_string().is_some(), "the reason, as text");
    assert!(emu.core.cartridge_manifest_override().is_none());

    emu.set_cartridge_manifest("{\"preset\":\"mine\"}").unwrap();
    assert_eq!(
        emu.core.cartridge_manifest_override(),
        Some(&serde_json::json!({ "preset": "mine" }))
    );
    emu.clear_cartridge_manifest();
    assert!(emu.core.cartridge_manifest_override().is_none());

    emu.load_rom(&[1, 2, 3]).unwrap();
    let detected: serde_json::Value =
        serde_json::from_str(&emu.get_detected_manifest().unwrap()).unwrap();
    assert_eq!(detected["rom_size"], 3);
}

// ---- the values

#[wasm_bindgen_test]
fn a_value_the_serializer_refuses_is_undefined_and_the_module_goes_on() {
    assert!(to_js(&(1u64 << 60)).is_undefined());
    assert_eq!(to_js(&7u64).as_f64(), Some(7.0));
}

// ---- the names both consoles share: the CPUs, the sound chips, the video
// chip's memories, palette, sprite table and layers

#[wasm_bindgen_test]
fn the_cpus_are_described_with_what_the_debugger_does_with_them() {
    let emu = plain();
    assert_eq!(
        shape(&emu.get_cpus()),
        "[{architecture: \"M68000\", id: 0, location: \"Motherboard\", name: \"Fake CPU\", \
         records_coverage: true, steppable: true}]"
    );
}

#[wasm_bindgen_test]
fn the_sound_chips_are_read_by_their_index() {
    let mut emu = plain();
    let chips = Array::from(&emu.get_audio_chips());
    assert_eq!(chips.length(), 2);
    assert_eq!(
        shape(&chips.get(1)),
        "{channel_types: [\"Tone\", \"Tone\"], channels: 2, manufacturer: \"Nobody\", \
         name: \"Second\", sample_rate: 1000, stereo: false}"
    );
    assert_eq!(Array::from(&emu.get_audio_chip_channels(0)).length(), 3);
    emu.mute_audio_chip_channel(1, 1, true);
    let channels = Array::from(&emu.get_audio_chip_channels(1));
    assert_eq!(get(&channels.get(1), "muted").as_bool(), Some(true));
    assert_eq!(get(&channels.get(0), "muted").as_bool(), Some(false));
    assert!(emu.get_audio_chip_channels(2).is_null(), "no third chip");
    emu.mute_audio_chip_channel(2, 0, true);
    assert_eq!(emu.get_audio_chip_waveform(1, 1, 3), [1.0; 3]);
    assert!(emu.get_audio_chip_waveform(2, 0, 3).is_empty());
    assert_eq!(emu.get_audio_output_waveform(2), [0.25; 2]);
}

#[wasm_bindgen_test]
fn the_video_memories_are_read_and_written_by_name_in_bytes() {
    let mut emu = plain();
    assert_eq!(
        shape(&emu.get_video_memories()),
        "[{address_unit: 1, id: \"vram\", little_endian: true, name: \"VRAM\", size: 32}, \
         {address_unit: 2, id: \"palette\", little_endian: true, name: \"PAL\", size: 8}]"
    );
    assert_eq!(emu.read_video_memory("vram"), Some(emu.get_vram()));
    assert_eq!(
        emu.read_video_memory("palette"),
        Some(vec![2, 1, 4, 3, 6, 5, 8, 7])
    );
    assert_eq!(
        emu.read_video_memory("sprite_table"),
        None,
        "a memory the chip lacks"
    );
    assert_eq!(emu.read_video_memory("VRAM"), None, "a name no console has");
    assert!(emu.write_video_memory("vram", 31, &[0xAA, 0xBB]));
    let vram = emu.get_vram();
    assert_eq!((vram[31], vram[0]), (0xAA, 0xBB), "the offset wraps");
    assert!(!emu.write_video_memory("vertical_scroll", 0, &[1, 2]));
    assert!(!emu.write_video_memory("nothing", 0, &[1, 2]));
}

#[wasm_bindgen_test]
fn the_palette_is_its_memory_s_words_and_their_colours() {
    let mut emu = plain();
    assert_eq!(emu.get_palette_memory(), [0x0102, 0x0304, 0x0506, 0x0708]);
    emu.write_palette_memory(3, &[0xFFFF, 0x0010]);
    assert_eq!(
        emu.get_palette_memory(),
        [0x0010, 0x0304, 0x0506, 0x7FFF],
        "the colour wraps, the port's bits kept"
    );
    assert_eq!(
        emu.get_palette_colours(),
        [0, 0, 0x10, 255, 0, 3, 4, 255, 0, 5, 6, 255, 0, 0x7F, 0xFF, 255]
    );
    // Every pixel of the fake's tile 2 is index 2: colour 1 + 2.
    let tile = emu.get_tile_colours(0, 0, 2, 1, 2, 0, 1);
    assert_eq!(
        (number(&tile, "width"), number(&tile, "height")),
        (8.0, 8.0)
    );
    let data = get(&tile, "data")
        .dyn_into::<Uint8Array>()
        .unwrap()
        .to_vec();
    assert_eq!(data.len(), 8 * 8 * 4);
    assert_eq!(&data[..4], &[0, 0x7F, 0xFF, 255]);
    assert!(
        emu.get_tile_colours(1, 0, 0, 1, 8, 0, 0).is_null(),
        "a decode the chip refuses"
    );
    assert!(
        emu.get_tile_colours(0, 0, 0, 1, 4, 2, 0).is_null(),
        "no such size"
    );
}

#[wasm_bindgen_test]
fn the_sprite_table_the_layers_and_the_registers_answer_the_chip() {
    let mut emu = plain();
    let table = Array::from(&emu.get_sprite_table());
    assert_eq!(table.length(), 2);
    assert_eq!(number(&table.get(0), "next"), 1.0);
    assert!(
        get(&table.get(1), "next").is_undefined(),
        "the last one links to none"
    );
    assert_eq!(get(&table.get(1), "large").as_bool(), Some(true));
    assert_eq!(number(&table.get(1), "tile_address"), 64.0);
    assert_eq!(
        shape(&emu.get_layer_layout(1)),
        "{bits_per_pixel: 2, cell_height: 8, cell_width: 8, character_base: 0, first_colour: 0, \
         id: 1, kind: \"tile_map\", map_base: 16, map_height: 2, map_width: 4, scroll_x: 0, \
         scroll_y: 0}"
    );
    assert!(emu.get_layer_layout(2).is_null(), "a layer the chip lacks");
    assert_eq!(shape(&emu.get_video_registers()), "[]");
}
