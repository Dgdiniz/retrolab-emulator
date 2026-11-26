//! What the bindings answer about the machine, recorded: the system
//! information a host reads, the region after a change, and every refusal
//! of a load. A value that stops matching is one a host reads that changed:
//! record a new one only for a change a host is meant to see. When they
//! differ, the values found are written to the console in the form of
//! `RECORDED`.

use retro_core::savestate::{deserialize_compressed, serialize_compressed};
use snes1_core::SnesSaveState;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

use super::super::RetroEmulator;

fn program() -> Vec<u8> {
    let mut chip = vec![0u8; 0x10_0000];
    // SEI / LDA #$81 / STA $4200 / loop: INC $10 / BRA loop
    chip[..10].copy_from_slice(&[0x78, 0xA9, 0x81, 0x8D, 0x00, 0x42, 0xE6, 0x10, 0x80, 0xFC]);
    chip[0x40..0x44].copy_from_slice(&[0xAD, 0x10, 0x42, 0x40]);
    chip[0x60] = 0x40;
    let h = 0x7FB0;
    chip[h + 0x10..h + 0x10 + 21].copy_from_slice(b"BINDING BASELINE     ");
    chip[h + 0x25] = 0x20;
    chip[h + 0x27] = 0x0A;
    chip[h + 0x29] = 0x01;
    chip[h + 0x2A] = 0x01;
    let checksum: u16 = 0x1234;
    chip[h + 0x2C..h + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    chip[h + 0x2E..h + 0x30].copy_from_slice(&checksum.to_le_bytes());
    chip[h + 0x4C..h + 0x4E].copy_from_slice(&0x8000u16.to_le_bytes());
    chip[0x7FFA..0x7FFC].copy_from_slice(&0x8040u16.to_le_bytes());
    chip[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    chip[0x7FFE..0x8000].copy_from_slice(&0x8060u16.to_le_bytes());
    chip
}

fn text(value: &JsValue) -> String {
    if let Some(s) = value.as_string() {
        return format!("text {s:?}");
    }
    let get = |key: &str| {
        js_sys::Reflect::get(value, &JsValue::from_str(key))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default()
    };
    format!("Error {:?}: {:?}", get("name"), get("message"))
}

fn answer(result: Result<(), JsValue>) -> String {
    match result {
        Ok(()) => "Ok".to_string(),
        Err(e) => text(&e),
    }
}

fn record() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let rom = program();
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&rom).unwrap();
    out.push((
        "getSystemInfo".to_string(),
        js_sys::JSON::stringify(&emu.get_system_info())
            .unwrap()
            .as_string()
            .unwrap(),
    ));
    out.push((
        "setRegion('secam')".to_string(),
        answer(emu.set_region("secam")),
    ));
    out.push((
        "setRegion('pal')".to_string(),
        answer(emu.set_region("pal")),
    ));
    out.push(("getRegion".to_string(), emu.get_region()));
    emu.set_region("ntsc").unwrap();
    for _ in 0..3 {
        emu.run_frame();
    }
    let state = emu.save_state();
    let envelope = deserialize_compressed::<SnesSaveState>(&state).unwrap();
    let reseal = |change: &dyn Fn(&mut retro_core::savestate::SaveStateEnvelope<SnesSaveState>)| {
        let mut e = envelope.clone();
        change(&mut e);
        serialize_compressed(&e, 3).unwrap()
    };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("the state", state.clone()),
        ("another ROM", reseal(&|e| e.rom_hash = "0".repeat(64))),
        ("core version + 1", reseal(&|e| e.core_version += 1)),
        (
            "another core's id",
            reseal(&|e| e.core_id = "sg1".to_string()),
        ),
        ("garbage", b"this is not a savestate".to_vec()),
        ("empty", Vec::new()),
    ];
    for (name, bytes) in &cases {
        let mut target = RetroEmulator::new("snes").unwrap();
        target.load_rom(&rom).unwrap();
        out.push((
            format!("loadState: {name}"),
            answer(target.load_state(bytes)),
        ));
        let mut any = RetroEmulator::new("snes").unwrap();
        any.load_rom(&rom).unwrap();
        out.push((
            format!("loadStateAnyRom: {name}"),
            answer(any.load_state_any_rom(bytes)),
        ));
    }
    let mut empty = RetroEmulator::new("snes").unwrap();
    out.push((
        "loadState with no ROM".to_string(),
        answer(empty.load_state(&state)),
    ));
    out
}

/// The values the bindings give. Where one moved when both consoles' machine
/// interface became retro-core's, the comment above it says what for.
const RECORDED: &[(&str, &str)] = &[
    // The display's grid says where the picture starts: `first_dot`, `first_line`.
    ("getSystemInfo", "{\"name\":\"Super Nintendo Entertainment System\",\"short_name\":\"SNES\",\"manufacturer\":\"Nintendo\",\"year\":1990,\"cpus\":[{\"id\":0,\"name\":\"Ricoh 5A22 (WDC 65C816)\",\"architecture\":\"Mos65816\",\"clock_speed\":3580000,\"registers\":[],\"location\":\"Motherboard\",\"address_width\":24},{\"id\":1,\"name\":\"Sony SPC700\",\"architecture\":\"Spc700\",\"clock_speed\":1024000,\"registers\":[],\"location\":\"Motherboard\",\"address_width\":16}],\"display\":{\"width\":256,\"height\":224,\"aspect_ratio\":1.1428571939468384,\"refresh_rate\":60.09846878051758,\"pixel_format\":\"RGBA8888\",\"grid\":{\"columns_per_dot\":1,\"rows_per_line\":1,\"woven\":false,\"first_dot\":0,\"first_line\":0}},\"audio\":{\"sample_rate\":44100,\"channels\":2,\"bits_per_sample\":32},\"input\":{\"max_players\":2,\"button_layout\":[{\"name\":\"Up\",\"id\":0},{\"name\":\"Down\",\"id\":1},{\"name\":\"Left\",\"id\":2},{\"name\":\"Right\",\"id\":3},{\"name\":\"A\",\"id\":4},{\"name\":\"B\",\"id\":5},{\"name\":\"X\",\"id\":6},{\"name\":\"Y\",\"id\":7},{\"name\":\"L\",\"id\":8},{\"name\":\"R\",\"id\":9},{\"name\":\"Start\",\"id\":10},{\"name\":\"Select\",\"id\":11}]}}"),
    ("setRegion('secam')", "text \"unknown region 'secam' (expected 'ntsc' or 'pal')\""),
    ("setRegion('pal')", "Ok"),
    ("getRegion", "pal"),
    ("loadState: the state", "Ok"),
    ("loadStateAnyRom: the state", "Ok"),
    ("loadState: another ROM", "Error \"SaveStateOtherRom\": \"ROM hash mismatch: savestate is for '0000000000000000000000000000000000000000000000000000000000000000' but loaded ROM is '2d30dc89e98aca0bf772fb7322fff23f0d3b149a095d704018394f40586dbb60'\""),
    ("loadStateAnyRom: another ROM", "Ok"),
    // The refusal names the core by its id.
    ("loadState: core version + 1", "text \"snes1 save state core_version 5 is newer than supported version 4\""),
    // As the line above.
    ("loadStateAnyRom: core version + 1", "text \"snes1 save state core_version 5 is newer than supported version 4\""),
    // Another core's state is refused.
    ("loadState: another core's id", "text \"savestate is for core 'sg1', not 'snes1'\""),
    // As the line above.
    ("loadStateAnyRom: another core's id", "text \"savestate is for core 'sg1', not 'snes1'\""),
    ("loadState: garbage", "text \"not a retro-core savestate (missing magic)\""),
    ("loadStateAnyRom: garbage", "text \"not a retro-core savestate (missing magic)\""),
    ("loadState: empty", "text \"not a retro-core savestate (missing magic)\""),
    ("loadStateAnyRom: empty", "text \"not a retro-core savestate (missing magic)\""),
    // With no ROM loaded there is no ROM to refuse a state for.
    ("loadState with no ROM", "Ok"),
];

#[wasm_bindgen_test]
fn the_bindings_answer_the_recorded_values() {
    let found = record();
    let found: Vec<(&str, &str)> = found
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    if found != RECORDED {
        for (key, value) in &found {
            console_log!("RECORDED ({key:?}, {value:?}),");
        }
    }
    assert_eq!(found, RECORDED);
}
