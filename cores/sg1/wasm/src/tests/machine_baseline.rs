//! What the bindings answer about the machine, recorded: the system
//! information a host reads, the region after a change, and every refusal
//! of a load. A value that stops matching is one a host reads that changed:
//! record a new one only for a change a host is meant to see. When they
//! differ, the values found are written to the console in the form of
//! `RECORDED`.

use retro_core::savestate::{deserialize_compressed, serialize_compressed};
use sg1_core::savestate::SystemSaveState;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

use super::super::RetroEmulator;

fn program() -> Vec<u8> {
    let mut rom = vec![0u8; 0x20000];
    let words: [(usize, u16); 4] = [(0, 0x00FF), (2, 0xFE00), (4, 0x0000), (6, 0x0200)];
    for (at, w) in words {
        rom[at..at + 2].copy_from_slice(&w.to_be_bytes());
    }
    rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
    rom[0x120..0x138].copy_from_slice(b"BINDING BASELINE        ");
    rom[0x1A0..0x1A8].copy_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0xFF, 0xFF]);
    // ADDQ.B #1,$FF0010 / BRA.S back
    for (i, w) in [0x46FCu16, 0x2700, 0x5239, 0x00FF, 0x0010, 0x60F8]
        .iter()
        .enumerate()
    {
        rom[0x200 + 2 * i..0x202 + 2 * i].copy_from_slice(&w.to_be_bytes());
    }
    rom
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
    let mut emu = RetroEmulator::new("genesis").unwrap();
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
    let envelope = deserialize_compressed::<SystemSaveState>(&state).unwrap();
    let reseal =
        |change: &dyn Fn(&mut retro_core::savestate::SaveStateEnvelope<SystemSaveState>)| {
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
            reseal(&|e| e.core_id = "snes1".to_string()),
        ),
        ("garbage", b"this is not a savestate".to_vec()),
        ("empty", Vec::new()),
    ];
    for (name, bytes) in &cases {
        let mut target = RetroEmulator::new("genesis").unwrap();
        target.load_rom(&rom).unwrap();
        out.push((
            format!("loadState: {name}"),
            answer(target.load_state(bytes)),
        ));
        let mut any = RetroEmulator::new("genesis").unwrap();
        any.load_rom(&rom).unwrap();
        out.push((
            format!("loadStateAnyRom: {name}"),
            answer(any.load_state_any_rom(bytes)),
        ));
    }
    let mut empty = RetroEmulator::new("genesis").unwrap();
    out.push((
        "loadState with no ROM".to_string(),
        answer(empty.load_state(&state)),
    ));
    out
}

/// The values the bindings give. Where one moved when both consoles' machine
/// interface became retro-core's, the comment above it says what for.
const RECORDED: &[(&str, &str)] = &[
    // Each button's id is the bit the six-button pad reads it from.
    // The display's grid says where the picture starts: `first_dot`, `first_line`.
    ("getSystemInfo", "{\"name\":\"Sega Genesis\",\"short_name\":\"genesis\",\"manufacturer\":\"Sega\",\"year\":1988,\"cpus\":[{\"id\":0,\"name\":\"Motorola 68000\",\"architecture\":\"M68000\",\"clock_speed\":7670454,\"registers\":[{\"name\":\"D0\",\"size_bits\":32,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"D1\",\"size_bits\":32,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"D2\",\"size_bits\":32,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"D3\",\"size_bits\":32,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"D4\",\"size_bits\":32,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"D5\",\"size_bits\":32,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"D6\",\"size_bits\":32,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"D7\",\"size_bits\":32,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"A0\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"A1\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"A2\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"A3\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"A4\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"A5\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"A6\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"A7\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"PC\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Special\"},{\"name\":\"SR\",\"size_bits\":16,\"display_format\":\"Binary\",\"category\":\"Flags\"}],\"location\":\"Motherboard\",\"address_width\":24},{\"id\":1,\"name\":\"Zilog Z80\",\"architecture\":\"Z80\",\"clock_speed\":3579545,\"registers\":[{\"name\":\"A\",\"size_bits\":8,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"F\",\"size_bits\":8,\"display_format\":\"Binary\",\"category\":\"Flags\"},{\"name\":\"B\",\"size_bits\":8,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"C\",\"size_bits\":8,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"D\",\"size_bits\":8,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"E\",\"size_bits\":8,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"H\",\"size_bits\":8,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"L\",\"size_bits\":8,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"PC\",\"size_bits\":16,\"display_format\":\"Address\",\"category\":\"Special\"},{\"name\":\"SP\",\"size_bits\":16,\"display_format\":\"Address\",\"category\":\"Special\"},{\"name\":\"IX\",\"size_bits\":16,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"IY\",\"size_bits\":16,\"display_format\":\"Address\",\"category\":\"Address\"}],\"location\":\"Motherboard\",\"address_width\":16}],\"display\":{\"width\":320,\"height\":224,\"aspect_ratio\":1.3333333730697632,\"refresh_rate\":59.92274475097656,\"pixel_format\":\"RGBA8888\",\"grid\":{\"columns_per_dot\":1,\"rows_per_line\":1,\"woven\":false,\"first_dot\":0,\"first_line\":0}},\"audio\":{\"sample_rate\":44100,\"channels\":2,\"bits_per_sample\":16},\"input\":{\"max_players\":2,\"button_layout\":[{\"name\":\"Up\",\"id\":0},{\"name\":\"Down\",\"id\":1},{\"name\":\"Left\",\"id\":2},{\"name\":\"Right\",\"id\":3},{\"name\":\"A\",\"id\":6},{\"name\":\"B\",\"id\":4},{\"name\":\"C\",\"id\":5},{\"name\":\"Start\",\"id\":7},{\"name\":\"X\",\"id\":10},{\"name\":\"Y\",\"id\":9},{\"name\":\"Z\",\"id\":8},{\"name\":\"Mode\",\"id\":11}]}}"),
    ("setRegion('secam')", "text \"unknown region 'secam' (expected 'ntsc' or 'pal')\""),
    ("setRegion('pal')", "Ok"),
    ("getRegion", "pal"),
    ("loadState: the state", "Ok"),
    ("loadStateAnyRom: the state", "Ok"),
    ("loadState: another ROM", "Error \"SaveStateOtherRom\": \"ROM hash mismatch: savestate is for '0000000000000000000000000000000000000000000000000000000000000000' but loaded ROM is 'a0bc9dfaaea6a164e72d5b1558fef1122774668a9ce05b2c66db81ceebe344e8'\""),
    ("loadStateAnyRom: another ROM", "Ok"),
    // A state of a newer build is refused (it loaded as this build's layout).
    ("loadState: core version + 1", "text \"sg1 save state core_version 4 is newer than supported version 3\""),
    // As the line above.
    ("loadStateAnyRom: core version + 1", "text \"sg1 save state core_version 4 is newer than supported version 3\""),
    // Another core's state is refused.
    ("loadState: another core's id", "text \"savestate is for core 'snes1', not 'sg1'\""),
    // As the line above.
    ("loadStateAnyRom: another core's id", "text \"savestate is for core 'snes1', not 'sg1'\""),
    ("loadState: garbage", "text \"Decompression failed: Unknown frame descriptor\""),
    ("loadStateAnyRom: garbage", "text \"Decompression failed: Unknown frame descriptor\""),
    ("loadState: empty", "text \"Decompression failed: incomplete frame\""),
    ("loadStateAnyRom: empty", "text \"Decompression failed: incomplete frame\""),
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
