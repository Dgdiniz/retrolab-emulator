//! The binding macros, each expanded on a struct over [`FakeMachine`], and
//! what their methods hand a host: the values, the named refusals, the
//! present buffer, the load's retry and which calls refresh the picture.

use js_sys::{Array, Map, Object, Reflect, Uint8Array};
use retro_core::cartridge::{
    AccessType, BankConfiguration, BankInfo, BreakpointId, CartridgeDebugger, ExpansionChipState,
    MapperId, MapperRegister, MapperState, MemoryMap, MemoryMapRegion, MemoryType, RomRegionInfo,
    SRAMType, SramRegionInfo, WatchType,
};
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

/// The machine, picture and sound macros with a present buffer, a hook that
/// counts its calls, and the load's retry.
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
crate::audio_bindings!(PresentingEmulator, core, after_frame = refresh);

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
fn the_render_refreshes_the_picture_after_its_frames() {
    let mut emu = presenting();
    emu.load_rom(&[1]).unwrap();
    let before = emu.refreshes;
    assert_eq!(
        shape(&emu.render_frames(3)),
        "{frames: 3, sound_turned_on: true}"
    );
    assert_eq!(emu.refreshes, before + 1, "one refresh, after the frames");
    assert!(
        emu.get_framebuffer_data()
            .to_vec()
            .iter()
            .all(|&byte| byte == 3),
        "the presented picture is the third frame's"
    );
    let mut plain = plain();
    assert_eq!(
        shape(&plain.render_frames(2)),
        "{frames: 2, sound_turned_on: true}",
        "the same answer with no hook"
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
    assert_eq!(
        shape(&Array::from(&emu.get_sprite_data()).get(1)),
        "{flip_h: false, flip_v: true, height: 16, index: 1, palette: 1, picture: {height: 16, \
         width: 16, x: 16, y: -4}, priority: 0, tile_index: 2, width: 8, x: 8, y: -4}"
    );
    let table = Array::from(&emu.get_sprite_table());
    assert_eq!(
        shape(&get(&table.get(1), "picture")),
        "{height: 16, width: 16, x: 16, y: -4}"
    );
    assert!(Array::is_array(&emu.resolved_sprite_render_order()));
    assert_eq!(
        shape(&emu.get_colour_levels()),
        "{levels: [0, 85, 170, 255], name: \"fake-4\"}"
    );

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
    assert!(emu.get_rom_bytes().is_empty(), "no cartridge, no image");
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
        get(&emu.get_cartridge_info().unwrap(), "rom_size").as_f64(),
        Some(4.0)
    );
    assert_eq!(emu.get_cartridge_title().as_deref(), Some("FAKE"));
    assert_eq!(
        emu.get_rom_bytes(),
        vec![1, 2, 3, 4],
        "the cartridge's image"
    );
    assert!(!emu.is_cartridge_sram_dirty());
    emu.core.write_save();
    assert!(emu.is_cartridge_sram_dirty());
    emu.clear_cartridge_sram_dirty();
    assert!(!emu.is_cartridge_sram_dirty());
}

/// `getCartridgeInfo` is what the cartridge declares, every field of
/// retro-core's `CartridgeInfo` in its order, not the ROM header's summary
/// `getRomInfo` gives.
#[wasm_bindgen_test]
fn the_cartridge_info_is_what_the_cartridge_declares() {
    let mut emu = plain();
    emu.load_rom(&[1, 2, 3, 4]).unwrap();
    let info = emu.get_cartridge_info().unwrap();
    assert!(!info.is_instance_of::<Map>());
    let keys: Vec<String> = Object::keys(info.unchecked_ref())
        .iter()
        .filter_map(|key| key.as_string())
        .collect();
    assert_eq!(
        keys,
        [
            "system",
            "title",
            "serial",
            "regions",
            "rom_size",
            "sram_size",
            "sram_type",
            "mapper_info",
            "expansion_chip_info",
            "header_checksum",
            "calculated_checksum",
            "rom_hash",
            "detection_source",
        ]
    );
    let declared = emu.core.cartridge_info().unwrap();
    assert_eq!(shape(&info), shape(&to_js(declared)));
    assert_eq!(
        get(&info, "rom_hash").as_string(),
        Some(retro_core::cartridge::rom_sha256(&[1, 2, 3, 4]))
    );
    assert!(get(&info, "size").is_undefined(), "not getRomInfo's");
}

/// A cartridge debugger with one ROM window, one save window and a mapper,
/// for the manifest view's marshaller.
struct DescribedCartridge;

impl CartridgeDebugger for DescribedCartridge {
    fn get_memory_map(&self) -> MemoryMap {
        MemoryMap {
            regions: vec![MemoryMapRegion {
                cpu_address_start: 0,
                cpu_address_end: 0x7FFF,
                physical_offset: 0,
                size: 0x8000,
                memory_type: MemoryType::ROM,
                access: AccessType::ReadOnly,
                description: "ROM".to_string(),
            }],
        }
    }
    fn get_rom_regions(&self) -> Vec<RomRegionInfo> {
        vec![RomRegionInfo {
            start: 0,
            end: 0x7FFF,
            size: 0x8000,
            description: "ROM".to_string(),
        }]
    }
    fn get_sram_regions(&self) -> Vec<SramRegionInfo> {
        vec![SramRegionInfo {
            start: 0x20_0000,
            end: 0x20_1FFF,
            size: 0x2000,
            sram_type: SRAMType::Battery,
            description: "SRAM".to_string(),
        }]
    }
    fn get_mapper_state(&self) -> Option<MapperState> {
        Some(MapperState {
            mapper_id: MapperId::Custom("banked".to_string()),
            registers: vec![MapperRegister {
                address: 0xA1_30F1,
                name: "bank".to_string(),
                value: 3,
                description: String::new(),
            }],
            bank_config: BankConfiguration {
                banks: vec![BankInfo {
                    cpu_address_start: 0,
                    cpu_address_end: 0x7FFF,
                    physical_offset: 0x18000,
                    size: 0x8000,
                    memory_type: MemoryType::ROM,
                    bank_number: 3,
                }],
            },
        })
    }
    fn get_chip_state(&self) -> Option<ExpansionChipState> {
        None
    }
    fn add_rom_breakpoint(&mut self, _addr: u32) -> BreakpointId {
        unreachable!()
    }
    fn add_sram_watchpoint(&mut self, _addr: u32, _watch_type: WatchType) -> BreakpointId {
        unreachable!()
    }
    fn remove_breakpoint(&mut self, _id: BreakpointId) {
        unreachable!()
    }
}

/// `getCartridgeManifest` is the cartridge debugger's view in retro-core's
/// names, plain objects all the way down; with no cartridge every list is
/// empty and there is no mapper.
#[wasm_bindgen_test]
fn the_cartridge_manifest_is_the_debuggers_view_in_plain_objects() {
    assert_eq!(
        shape(&plain().get_cartridge_manifest()),
        "{mapperState: undefined, memoryMap: {regions: []}, romRegions: [], sramRegions: []}"
    );
    let view = crate::cartridge::cartridge_manifest_object(&DescribedCartridge);
    let mapper = get(&view, "mapperState");
    let banks = get(&get(&mapper, "bank_config"), "banks");
    let memory_map = get(&view, "memoryMap");
    for (what, value) in [
        ("the view", view.clone()),
        ("memoryMap", memory_map.clone()),
        ("a region", Array::from(&get(&memory_map, "regions")).get(0)),
        (
            "romRegions[0]",
            Array::from(&get(&view, "romRegions")).get(0),
        ),
        (
            "sramRegions[0]",
            Array::from(&get(&view, "sramRegions")).get(0),
        ),
        ("mapperState", mapper.clone()),
        ("mapper_id", get(&mapper, "mapper_id")),
        ("a register", Array::from(&get(&mapper, "registers")).get(0)),
        ("bank_config", get(&mapper, "bank_config")),
        ("a bank", Array::from(&banks).get(0)),
    ] {
        assert!(value.is_object(), "{what} is an object");
        assert!(!value.is_instance_of::<Map>(), "{what} is not a Map");
    }
    assert_eq!(
        shape(&view),
        concat!(
            "{mapperState: {bank_config: {banks: [{bank_number: 3, cpu_address_end: 32767, ",
            "cpu_address_start: 0, memory_type: \"ROM\", physical_offset: 98304, size: 32768}]}, ",
            "mapper_id: {Custom: \"banked\"}, registers: [{address: 10563825, description: \"\", ",
            "name: \"bank\", value: 3}]}, memoryMap: {regions: [{access: \"ReadOnly\", ",
            "cpu_address_end: 32767, cpu_address_start: 0, description: \"ROM\", ",
            "memory_type: \"ROM\", physical_offset: 0, size: 32768}]}, romRegions: [{description: ",
            "\"ROM\", end: 32767, size: 32768, start: 0}], sramRegions: [{description: \"SRAM\", ",
            "end: 2105343, size: 8192, sram_type: \"Battery\", start: 2097152}]}"
        )
    );
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
fn the_render_runs_frames_with_the_sound_on() {
    let mut emu = plain();
    assert!(!emu.is_audio_enabled());
    assert_eq!(
        shape(&emu.render_frames(3)),
        "{frames: 3, sound_turned_on: true}"
    );
    assert!(emu.is_audio_enabled(), "the sound stays on");
    assert_eq!(emu.get_frame_number(), 3);
    assert_eq!(
        emu.get_audio_samples(),
        [0.25, -0.25, 0.25, -0.25, 0.25, -0.25]
    );
    assert_eq!(
        shape(&emu.render_frames(1)),
        "{frames: 1, sound_turned_on: false}"
    );
    assert_eq!(
        PlainEmulator::encode_wav(&[1.0, -1.0], 32_000, 1),
        retro_core::audio::encode_wav(&[1.0, -1.0], 32_000, 1)
    );
}

/// The bytes of a typed array, as a host streaming a WAV to disk writes
/// them after the header: a view of its buffer.
fn buffer_bytes(buffer: &js_sys::ArrayBuffer, offset: u32, length: u32) -> Vec<u8> {
    Uint8Array::new_with_byte_offset_and_length(buffer, offset, length).to_vec()
}

#[wasm_bindgen_test]
fn the_wav_writers_write_samples_of_their_kind_and_the_header_alone() {
    let pcm = js_sys::Int16Array::from(&[1i16, -2, 32767, -32768, 0, 5][..]);
    let floats = js_sys::Float32Array::from(&[0.5f32, -1.25, 2.0, 0.0, -0.0, 1.0e-3][..]);
    let cases = [
        (
            JsValue::from(pcm.clone()),
            "I16",
            retro_core::audio::SoundSamples::I16(pcm.to_vec()),
            buffer_bytes(&pcm.buffer(), pcm.byte_offset(), pcm.byte_length()),
        ),
        (
            JsValue::from(floats.clone()),
            "F32",
            retro_core::audio::SoundSamples::F32(floats.to_vec()),
            buffer_bytes(&floats.buffer(), floats.byte_offset(), floats.byte_length()),
        ),
    ];
    for (samples, kind, own, bytes) in cases {
        let wav =
            PlainEmulator::encode_wav_samples(samples.clone(), 44_100, 2, None, None).unwrap();
        assert_eq!(
            wav,
            retro_core::audio::encode_wav_samples(&own, 44_100, 2),
            "{kind}"
        );
        // A file streamed to disk: the header, then the array's own bytes.
        let mut streamed = PlainEmulator::wav_header(kind, 44_100, 2, 3, None).unwrap();
        streamed.extend(bytes);
        assert_eq!(streamed, wav, "{kind}");
        // With a loop: the same file, then its `smpl` chunk.
        assert_eq!(
            PlainEmulator::encode_wav_samples(samples, 44_100, 2, Some(1), Some(2)).unwrap(),
            retro_core::audio::wav_with_loop(wav, 44_100, 1, 2),
            "{kind}"
        );
    }
}

#[wasm_bindgen_test]
fn the_wav_writers_refuse_what_no_wav_holds() {
    let refusal = |result: Result<Vec<u8>, JsValue>| result.unwrap_err().as_string().unwrap();
    // Three 16-bit samples, as `channels` channels, with a loop.
    let three = |channels: u16, first: Option<u32>, last: Option<u32>| {
        let samples = js_sys::Int16Array::from(&[1i16, 2, 3][..]);
        PlainEmulator::encode_wav_samples(samples.into(), 32_000, channels, first, last)
    };
    let plain_array = Array::of2(&JsValue::from(1), &JsValue::from(2));
    assert_eq!(
        refusal(PlainEmulator::encode_wav_samples(
            plain_array.into(),
            32_000,
            1,
            None,
            None
        )),
        "the samples are not an Int16Array or a Float32Array"
    );
    assert_eq!(
        refusal(three(2, None, None)),
        "3 samples are not whole frames of 2 channels"
    );
    assert_eq!(
        refusal(three(0, None, None)),
        "a WAV has at least one channel"
    );
    assert_eq!(
        refusal(three(1, Some(0), None)),
        "a loop needs its first and its last frame"
    );
    assert_eq!(
        refusal(three(1, Some(1), Some(3))),
        "the loop from frame 1 to frame 3 does not run forward inside the 3 frames"
    );
    assert!(
        three(1, Some(2), Some(1)).is_err(),
        "a loop that ends before it starts"
    );
    assert!(three(1, Some(0), Some(2)).is_ok());
    assert_eq!(
        refusal(PlainEmulator::wav_header("i16", 32_000, 1, 1, None)),
        "\"i16\" is not a sample kind: \"I16\" or \"F32\""
    );
    assert_eq!(
        refusal(PlainEmulator::wav_header("I16", 32_000, 0, 1, None)),
        "a WAV has at least one channel"
    );
    assert_eq!(
        refusal(PlainEmulator::wav_header("F32", 44_100, 2, u32::MAX, None)),
        "a WAV's header cannot hold 4294967295 frames of 2 channels of F32 samples at 44100 Hz"
    );
    assert_eq!(
        refusal(PlainEmulator::wav_header(
            "I16",
            32_000,
            1,
            0,
            Some(u32::MAX)
        )),
        "a WAV's header cannot hold 0 frames of 1 channels of I16 samples at 32000 Hz and \
         4294967295 bytes after them"
    );
    // A refusal leaves the module answering.
    let header = PlainEmulator::wav_header("F32", 44_100, 2, 0, None).unwrap();
    assert_eq!(header.len(), 58);
}

/// The marks a host hands `wavTrailer`: a loop, and two cues.
fn host_marks() -> JsValue {
    let cue = |frame: u32, label: &str| {
        object(&[
            ("frame", JsValue::from(frame)),
            ("label", JsValue::from_str(label)),
        ])
    };
    object(&[
        (
            "loop",
            Array::of2(&JsValue::from(1), &JsValue::from(2)).into(),
        ),
        ("cues", Array::of2(&cue(0, "start"), &cue(2, "hit")).into()),
    ])
}

#[wasm_bindgen_test]
fn a_wav_s_marks_are_its_trailer_and_a_streamed_file_ends_with_it() {
    let marks = retro_core::audio::WavMarks {
        r#loop: Some((1, 2)),
        cues: vec![
            retro_core::audio::WavCue {
                frame: 0,
                label: "start".into(),
            },
            retro_core::audio::WavCue {
                frame: 2,
                label: "hit".into(),
            },
        ],
    };
    let trailer = PlainEmulator::wav_trailer(44_100, host_marks()).unwrap();
    assert_eq!(
        trailer,
        retro_core::audio::wav_trailer(44_100, &marks),
        "the marks as the host hands them"
    );
    // A file streamed to disk: the header counting the trailer, the
    // samples' own bytes, then the trailer, the whole file at once.
    let pcm = js_sys::Int16Array::from(&[1i16, -2, 32767, -32768, 0, 5][..]);
    let mut streamed =
        PlainEmulator::wav_header("I16", 44_100, 2, 3, Some(trailer.len() as u32)).unwrap();
    streamed.extend(buffer_bytes(
        &pcm.buffer(),
        pcm.byte_offset(),
        pcm.byte_length(),
    ));
    streamed.extend(&trailer);
    let whole = PlainEmulator::encode_wav_samples(pcm.into(), 44_100, 2, None, None).unwrap();
    assert_eq!(
        streamed,
        retro_core::audio::wav_with_marks(whole.clone(), 44_100, &marks)
    );
    // The loop alone is the chunk `encodeWavSamples` writes for its loop.
    let loop_alone = object(&[(
        "loop",
        Array::of2(&JsValue::from(1), &JsValue::from(2)).into(),
    )]);
    let looped = PlainEmulator::encode_wav_samples(
        js_sys::Int16Array::from(&[1i16, -2, 32767, -32768, 0, 5][..]).into(),
        44_100,
        2,
        Some(1),
        Some(2),
    )
    .unwrap();
    assert_eq!(
        PlainEmulator::wav_trailer(44_100, loop_alone).unwrap(),
        &looped[whole.len()..]
    );
    assert!(PlainEmulator::wav_trailer(44_100, object(&[]))
        .unwrap()
        .is_empty());
    assert_eq!(
        PlainEmulator::wav_header("I16", 44_100, 2, 3, Some(0)).unwrap(),
        PlainEmulator::wav_header("I16", 44_100, 2, 3, None).unwrap(),
        "no trailer is a trailer of none"
    );
}

#[wasm_bindgen_test]
fn the_trailer_refuses_marks_it_cannot_write() {
    let refusal = |result: Result<Vec<u8>, JsValue>| result.unwrap_err().as_string().unwrap();
    let backwards = object(&[(
        "loop",
        Array::of2(&JsValue::from(3), &JsValue::from(2)).into(),
    )]);
    assert_eq!(
        refusal(PlainEmulator::wav_trailer(32_000, backwards)),
        "the loop from frame 3 to frame 2 ends before it starts"
    );
    let unlabelled = object(&[(
        "cues",
        Array::of1(&object(&[("frame", JsValue::from(4))])).into(),
    )]);
    assert!(
        refusal(PlainEmulator::wav_trailer(32_000, unlabelled)).contains("label"),
        "a cue with no label"
    );
    assert!(PlainEmulator::wav_trailer(32_000, JsValue::from(7)).is_err());
    // A refusal leaves the module answering.
    assert!(PlainEmulator::wav_trailer(32_000, host_marks()).is_ok());
}

#[wasm_bindgen_test]
fn the_stems_are_armed_by_mask_and_taken_by_whole_frames() {
    let mut emu = plain();
    assert_eq!(
        shape(&emu.get_stem_layout()),
        "{channels: [{channel: 0, chip: 0, in_master: true, name: \"main\", stereo: true}, \
         {channel: 0, chip: 1, in_master: true, name: \"second\", stereo: false}], kind: \"F32\", \
         master: {channel: undefined, chip: undefined, in_master: false, name: \"master\", \
         stereo: true}, rate: 1000, sum: \"Exact\"}"
    );
    assert!(emu.take_stems().is_null(), "nothing armed");
    emu.arm_stems(0b10);
    emu.render_frames(2);
    let take = emu.take_stems();
    assert_eq!(number(&take, "frames"), 2.0);
    assert_eq!(number(&take, "armed"), 2.0);
    let stems = Array::from(&get(&take, "stems"));
    assert_eq!(stems.length(), 1);
    assert_eq!(
        stems
            .get(0)
            .dyn_into::<js_sys::Float32Array>()
            .unwrap()
            .to_vec(),
        [0.5, 0.5]
    );
    assert_eq!(
        get(&take, "master")
            .dyn_into::<js_sys::Float32Array>()
            .unwrap()
            .to_vec(),
        [0.75, 0.0, 0.75, 0.0]
    );
    assert_eq!(
        get(&take, "departures")
            .dyn_into::<js_sys::Uint32Array>()
            .unwrap()
            .length(),
        0
    );
    emu.arm_stems(0);
    assert!(emu.take_stems().is_null(), "disarmed");
}

#[wasm_bindgen_test]
fn the_sound_log_is_taken_as_records_and_its_chips() {
    let mut emu = plain();
    assert!(emu.take_sound_log().is_null(), "no log runs");
    emu.start_sound_log(true, false, None);
    emu.render_frames(2);
    let log = emu.take_sound_log();
    assert_eq!(number(&log, "clockHz"), 1000.0);
    assert_eq!(number(&log, "imageLen"), 1.0);
    assert_eq!(get(&log, "interrupted").as_bool(), Some(false));
    assert_eq!(shape(&get(&log, "chips")), "[{Sn76489: {clock_hz: 1000}}]");
    let records = get(&log, "records")
        .dyn_into::<Uint8Array>()
        .unwrap()
        .to_vec();
    assert_eq!(records.len(), 3 * 24, "the image and two frames' ends");
    assert_eq!(
        &records[8..10],
        &[0x00, 0x9F],
        "a write of the first chip, its value"
    );
    assert_eq!(records[24 + 8], 2, "a frame's end");
    assert_eq!(records[24], 10, "at its time");
    emu.stop_sound_log();
    assert!(emu.take_sound_log().is_null(), "stopped and taken");
}

#[wasm_bindgen_test]
fn the_sound_log_takes_the_mailbox_down_when_asked() {
    let records = |emu: &mut PlainEmulator| {
        get(&emu.take_sound_log(), "records")
            .dyn_into::<Uint8Array>()
            .unwrap()
            .to_vec()
    };
    let mut emu = plain();
    emu.start_sound_log(false, false, Some(true));
    emu.render_frames(2);
    let log = records(&mut emu);
    assert_eq!(log.len(), 4 * 24, "a write and a frame's end, a frame");
    assert_eq!(
        &log[..12],
        &[5, 0, 0, 0, 0, 0, 0, 0, 6, 1, 1, 0],
        "kind 6 at its time: the value, then the port"
    );
    assert_eq!(log[24 + 8], 2, "the frame's end");
    assert_eq!(&log[48 + 8..48 + 12], &[6, 2, 1, 0], "the second frame's");
    emu.start_sound_log(true, false, Some(false));
    emu.render_frames(1);
    assert_eq!(
        records(&mut emu).len(),
        2 * 24,
        "no mailbox: the image and the end"
    );
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
         id: 1, kind: \"tile_map\", line_scroll: [], map_base: 16, map_height: 2, map_width: 4, \
         scroll_x: 0, scroll_y: 0}"
    );
    assert!(emu.get_layer_layout(2).is_null(), "a layer the chip lacks");
    assert_eq!(shape(&emu.get_video_registers()), "[]");
}

#[wasm_bindgen_test]
fn a_layer_s_map_is_a_picture_or_null() {
    let mut emu = plain();
    let map = emu.layer_map(0);
    assert_eq!((number(&map, "width"), number(&map, "height")), (2.0, 1.0));
    assert_eq!(
        get(&map, "data").dyn_into::<Uint8Array>().unwrap().to_vec(),
        vec![7, 8, 9, 255, 0, 0, 0, 0]
    );
    assert!(emu.layer_map(1).is_null(), "a layer with no map");
    assert!(emu.layer_map(2).is_null(), "a layer the chip lacks");
}
