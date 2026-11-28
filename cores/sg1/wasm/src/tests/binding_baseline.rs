//! What the bindings answer, method by method, recorded on the bindings as
//! they are: the machine's calls and what each leaves, the display, the
//! audio, every input call in one scripted session, the debugger's memory
//! and disassembly readers, the VDP's and the sound chips' readers, and the
//! cartridge's. Each value is held to the description
//! `retro_wasm::test_support::shape` gives of it, so a change that moves a
//! key, a value, a type or a byte a host reads fails here: record a new
//! description only for a change a host is meant to see.
//!
//! A test holds one line per value: its name, ` => `, and its description,
//! in the order the test reads them. When the lines differ, the lines found
//! are written to the console after `RECORDED`, and each long description
//! in full.
//!
//! Two rules keep the lines short. A plain array of more than
//! [`LONG_ARRAY`] numbers, such as the pixels `getFramebuffer` gives, is
//! written as the string `"Array(len N, bytes fnv1a64 H)"`, H the FNV-1a
//! hash of its numbers as bytes (`numbers`, and the hash of their bytes as
//! `f64`, when one is not a byte). A description longer than
//! [`LONG_DESCRIPTION`] characters is held by its first [`HEAD`]
//! characters, its length and the FNV-1a hash of the whole.
//!
//! What changes from one run to the next is held by its form: the build id,
//! which names the commit, the framebuffer's address, and the time a
//! savestate is stamped with.

use js_sys::{Array, Map, Object, Reflect};
use retro_core::hash::fnv1a64;
use retro_core::savestate::read_header;
use retro_wasm::js::plain_object;
use retro_wasm::test_support::shape;

use super::capture::capture_shapes_rom;
use super::*;

/// A plain array of more numbers than this is described by its hash.
const LONG_ARRAY: usize = 64;
/// A description longer than this is held by its start and its hash.
const LONG_DESCRIPTION: usize = 400;
/// How much of a long description is written out.
const HEAD: usize = 100;

/// `value` with every plain array of more than [`LONG_ARRAY`] numbers in it,
/// at any depth of arrays, plain objects and maps, replaced by a string
/// naming its length and the FNV-1a hash of its numbers.
fn with_long_arrays_hashed(value: &JsValue) -> JsValue {
    if Array::is_array(value) {
        let items = value.unchecked_ref::<Array>().to_vec();
        let numbers: Option<Vec<f64>> = items.iter().map(JsValue::as_f64).collect();
        if let Some(numbers) = numbers.filter(|numbers| numbers.len() > LONG_ARRAY) {
            return JsValue::from_str(&hashed(&numbers));
        }
        let copy: Array = items.iter().map(with_long_arrays_hashed).collect();
        copy.into()
    } else if let Some(map) = value.dyn_ref::<Map>() {
        let copy = Map::new();
        map.for_each(&mut |item, key| {
            copy.set(&key, &with_long_arrays_hashed(&item));
        });
        copy.into()
    } else if is_plain_object(value) {
        let copy = Object::create(&Object::get_prototype_of(value));
        for key in Object::keys(value.unchecked_ref()).iter() {
            let member = Reflect::get(value, &key).unwrap_or_default();
            let _ = Reflect::set(&copy, &key, &with_long_arrays_hashed(&member));
        }
        copy.into()
    } else {
        value.clone()
    }
}

/// Whether `value` is an object whose prototype is `Object`'s or none.
fn is_plain_object(value: &JsValue) -> bool {
    if !value.is_object() || value.is_function() {
        return false;
    }
    let prototype = JsValue::from(Object::get_prototype_of(value));
    prototype.is_null() || prototype == JsValue::from(Object::get_prototype_of(&Object::new()))
}

/// `numbers` as the string [`with_long_arrays_hashed`] writes for them.
fn hashed(numbers: &[f64]) -> String {
    let all_bytes = numbers
        .iter()
        .all(|n| n.fract() == 0.0 && (0.0..=255.0).contains(n));
    let bytes: Vec<u8> = if all_bytes {
        numbers.iter().map(|&n| n as u8).collect()
    } else {
        numbers.iter().flat_map(|n| n.to_le_bytes()).collect()
    };
    let kind = if all_bytes { "bytes" } else { "numbers" };
    let hash = fnv1a64(&bytes);
    format!("Array(len {}, {kind} fnv1a64 {hash:#018x})", numbers.len())
}

/// What a test found, in order: each value's name, its description, and the
/// whole description when the one kept is cut.
#[derive(Default)]
struct Found(Vec<(String, String, Option<String>)>);

impl Found {
    /// Record `value` as JavaScript receives it.
    fn value(&mut self, what: impl Into<String>, value: impl Into<JsValue>) {
        let full = shape(&with_long_arrays_hashed(&value.into()));
        if full.len() <= LONG_DESCRIPTION {
            self.0.push((what.into(), full, None));
            return;
        }
        let head: String = full.chars().take(HEAD).collect();
        let hash = fnv1a64(full.as_bytes());
        let cut = format!("{head}... ({} chars, fnv1a64 {hash:#018x})", full.len());
        self.0.push((what.into(), cut, Some(full)));
    }

    /// Record what a call that can refuse did: the value it returned, or
    /// `throws` and what it threw.
    fn outcome(&mut self, what: impl Into<String>, result: Result<JsValue, JsValue>) {
        match result {
            Ok(value) => self.value(what, value),
            Err(thrown) => self.0.push((what.into(), throws(&thrown), None)),
        }
    }

    /// Hold every value found to the line recorded for it: its name and its
    /// description, ` => ` between them, one line each, in order.
    fn check(self, recorded: &str) {
        let found: Vec<String> = self
            .0
            .iter()
            .map(|(what, description, _)| format!("{what} => {description}"))
            .collect();
        let recorded: Vec<&str> = recorded.lines().filter(|line| !line.is_empty()).collect();
        if found != recorded {
            for ((what, _, full), line) in self.0.iter().zip(&found) {
                console_log!("RECORDED {line}");
                if let Some(full) = full {
                    console_log!("IN FULL {what}: {full}");
                }
            }
        }
        assert_eq!(found, recorded);
    }
}

/// What a call threw: a text as itself, an `Error` with its own properties,
/// its name and its message.
fn throws(thrown: &JsValue) -> String {
    let Some(error) = thrown.dyn_ref::<js_sys::Error>() else {
        return format!("throws {}", shape(thrown));
    };
    let name = String::from(error.name());
    let message = String::from(error.message());
    format!("throws {} {name:?}: {message:?}", shape(thrown))
}

/// A call that returns nothing as JavaScript sees it: `undefined`.
fn done(result: Result<(), JsValue>) -> Result<JsValue, JsValue> {
    result.map(|()| JsValue::UNDEFINED)
}

/// The type of `value` a host sees: the name of its constructor for an
/// object, its `typeof` otherwise.
fn kind(value: &JsValue) -> JsValue {
    let constructor = Reflect::get(value, &"constructor".into()).unwrap_or_default();
    match constructor.dyn_ref::<js_sys::Function>() {
        Some(function) if value.is_object() => function.name().into(),
        _ => value.js_typeof(),
    }
}

/// What a call left the machine as a host reads it: `getFrameNumber`,
/// `getRegion`, the type of `getStateHash`, and the keys of the object
/// `getStateHashParts` writes, in its order.
fn machine(emu: &RetroEmulator) -> JsValue {
    let parts = js_sys::JSON::parse(&emu.get_state_hash_parts()).unwrap();
    let names = Object::keys(parts.unchecked_ref());
    plain_object(&[
        ("frame", emu.get_frame_number().into()),
        ("region", emu.get_region().into()),
        ("stateHash", kind(&emu.get_state_hash().into())),
        ("stateHashParts", names.into()),
    ])
}

/// Every display reader at once: `getFramebuffer`, `getFramebufferData`,
/// its width, height and length, whether `getFramebufferPtr` points at those
/// bytes, and `getDisplayInfo`.
fn display(emu: &RetroEmulator) -> JsValue {
    let data = emu.get_framebuffer_data();
    let pointer: *const u8 = emu.get_framebuffer_ptr();
    let length = emu.get_framebuffer_len();
    // SAFETY: the pointer and the length are the framebuffer's, which the
    // emulator, borrowed here, holds unchanged; wasm memory never moves.
    let at_pointer = unsafe { std::slice::from_raw_parts(pointer, length as usize) };
    let holds_data = at_pointer == data.to_vec().as_slice();
    plain_object(&[
        ("framebuffer", emu.get_framebuffer()),
        ("data", data.into()),
        ("width", emu.get_framebuffer_width().into()),
        ("height", emu.get_framebuffer_height().into()),
        ("length", length.into()),
        ("pointerHoldsData", holds_data.into()),
        ("displayInfo", emu.get_display_info()),
    ])
}

/// What `saveState` returned, by its form: its type, and the envelope's
/// header, the time it is stamped with written as whether there is one.
fn savestate(state: &[u8]) -> JsValue {
    let header = read_header(state).unwrap();
    plain_object(&[
        ("type", kind(&state.to_vec().into())),
        ("envelope", header.envelope_version.into()),
        ("core", header.core_id.into()),
        ("coreVersion", header.core_version.into()),
        ("rom", header.rom_hash.into()),
        ("stamped", (header.created_at > 0).into()),
    ])
}

/// The ring a host drains, as it reads it: `getAudioAvailable`,
/// `isAudioEnabled` and `getAudioRingOverflows`.
fn ring(emu: &RetroEmulator) -> JsValue {
    plain_object(&[
        ("available", emu.get_audio_available().into()),
        ("enabled", emu.is_audio_enabled().into()),
        ("overflows", emu.get_audio_ring_overflows().into()),
    ])
}

/// `MOVE.B #value,(address).L` for each `(address, value)`.
fn byte_stores(stores: &[(u32, u8)]) -> Vec<u16> {
    let store = |&(address, value): &(u32, u8)| {
        [0x13FC, value.into(), (address >> 16) as u16, address as u16]
    };
    stores.iter().flat_map(store).collect()
}

/// A program that sounds: the 68000 takes the Z80's bus, sets the YM2612's
/// first channel playing its first operator alone at full level (algorithm
/// 7) near 440 Hz, and the PSG's first tone channel near 440 Hz at full
/// volume, then loops with no interrupt on.
fn sounding_rom() -> Vec<u8> {
    let mut rom = synth_genesis_rom("SOUNDING");
    let mut code = vec![0x46FC, 0x2700]; // MOVE.W #$2700,SR
    code.extend([0x33FC, 0x0100, 0x00A1, 0x1100]); // the Z80's bus
    #[rustfmt::skip]
    let registers: [(u8, u8); 25] = [
        (0x22, 0x00), (0x27, 0x00), (0x28, 0x00), (0x2B, 0x00),
        (0x30, 0x01), (0x34, 0x01), (0x38, 0x01), (0x3C, 0x01),
        (0x40, 0x00), (0x44, 0x7F), (0x48, 0x7F), (0x4C, 0x7F),
        (0x50, 0x1F), (0x54, 0x1F), (0x58, 0x1F), (0x5C, 0x1F),
        (0x80, 0x0F), (0x84, 0x0F), (0x88, 0x0F), (0x8C, 0x0F),
        (0xB0, 0x07), (0xB4, 0xC0), (0xA4, 0x22), (0xA0, 0x69),
        (0x28, 0xF0), // every operator of the first channel keyed on
    ];
    for (register, value) in registers {
        code.extend(byte_stores(&[(0xA0_4000, register), (0xA0_4001, value)]));
    }
    let tone = [(0xC0_0011, 0x8E), (0xC0_0011, 0x0F), (0xC0_0011, 0x90)];
    code.extend(byte_stores(&tone));
    code.push(0x60FE); // BRA *
    place(&mut rom, 0x400, &code);
    rom
}

/// A cartridge with battery save RAM on the odd bytes from $200001 to
/// $203FFF ("RA" in the header), whose program turns the save RAM on,
/// writes a byte to it, then loops.
fn saving_rom() -> Vec<u8> {
    let mut rom = synth_genesis_rom("SAVING");
    rom[0x1B0..0x1B4].copy_from_slice(&[b'R', b'A', 0xF8, 0x20]);
    rom[0x1B4..0x1B8].copy_from_slice(&0x0020_0001u32.to_be_bytes());
    rom[0x1B8..0x1BC].copy_from_slice(&0x0020_3FFFu32.to_be_bytes());
    let mut code = byte_stores(&[(0xA1_30F1, 0x01), (0x20_0001, 0x42)]);
    code.push(0x60FE); // BRA *
    place(&mut rom, 0x400, &code);
    rom
}

/// The machine's calls, in the order a host makes them, and what each one
/// leaves: loads refused and one taken, the frames, the region, the three
/// resets, and a savestate saved and loaded back.
#[wasm_bindgen_test]
fn the_machine_calls_and_what_each_leaves() {
    let mut found = Found::default();
    let mut emu = RetroEmulator::new("genesis").unwrap();
    found.value("getSystemInfo, new", emu.get_system_info());
    found.value("getRomInfo, new", emu.get_rom_info());
    found.value("after new", machine(&emu));
    // The build id ends with the commit it was built from.
    let build = emu.get_build_id();
    let (named, commit) = build.split_once('+').unwrap();
    assert!(!commit.is_empty());
    found.value("getBuildId, up to the commit", format!("{named}+"));
    found.outcome("loadRom, ten bytes", done(emu.load_rom(&[0; 10])));
    found.outcome("loadRom, no bytes", done(emu.load_rom(&[])));
    found.value("after the refusals", machine(&emu));
    found.outcome("loadRom", done(emu.load_rom(&sounding_rom())));
    found.value("getRomInfo", emu.get_rom_info());
    found.value("after loadRom", machine(&emu));
    emu.run_frame();
    found.value("after runFrame", machine(&emu));
    emu.run_until_vblank();
    found.value("after runUntilVBlank", machine(&emu));
    emu.run_frame_headless();
    found.value("after runFrameHeadless", machine(&emu));
    found.outcome("setRegion('PAL')", done(emu.set_region("PAL")));
    found.value("after setRegion('PAL')", machine(&emu));
    emu.run_frame();
    found.outcome("setRegion('pal') again", done(emu.set_region("pal")));
    found.value("after setRegion('pal') again", machine(&emu));
    found.outcome("setRegion('ntsc')", done(emu.set_region("ntsc")));
    for _ in 0..3 {
        emu.run_frame();
    }
    found.value("after three frames", machine(&emu));
    emu.reset();
    found.value("after reset", machine(&emu));
    emu.run_frame();
    emu.hard_reset();
    found.value("after hardReset", machine(&emu));
    emu.run_frame();
    emu.power_on_reset();
    found.value("after powerOnReset", machine(&emu));
    for _ in 0..3 {
        emu.run_frame();
    }
    let hash = emu.get_state_hash();
    let state = emu.save_state();
    found.value("saveState", savestate(&state));
    found.value("after saveState", machine(&emu));
    emu.run_frame();
    found.outcome("loadState", done(emu.load_state(&state)));
    found.value("after loadState", machine(&emu));
    let as_saved = emu.get_state_hash() == hash;
    found.value("getStateHash after loadState, as saved", as_saved);
    emu.run_frame();
    found.outcome("loadStateAnyRom", done(emu.load_state_any_rom(&state)));
    found.value("after loadStateAnyRom", machine(&emu));
    found.check(
        r#"
getSystemInfo, new => {audio: {bits_per_sample: 16, channels: 2, sample_rate: 44100}, cpus: [{address_width: 24, architect... (3282 chars, fnv1a64 0x88fe86091b83318d)
getRomInfo, new => undefined
after new => {frame: 0n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu68k", "z80", "vdp", "psg", "ym2612", "io", "bus", "timing", "ram", "z80_ram", "cartridge", "frame_count", "extension"]}
getBuildId, up to the commit => "sg1@0.1.0+"
loadRom, ten bytes => throws "ROM too small (must be at least 512 bytes)"
loadRom, no bytes => throws "ROM too small (must be at least 512 bytes)"
after the refusals => {frame: 0n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu68k", "z80", "vdp", "psg", "ym2612", "io", "bus", "timing", "ram", "z80_ram", "cartridge", "frame_count", "extension"]}
loadRom => undefined
getRomInfo => {checksum: 0, mapper: undefined, region: "Japan, USA, Europe", size: 524288, title: "SOUNDING", title_confidence: "high"}
after loadRom => {frame: 0n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu68k", "z80", "vdp", "psg", "ym2612", "io", "bus", "timing", "ram", "z80_ram", "cartridge", "frame_count", "extension"]}
after runFrame => {frame: 1n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu68k", "z80", "vdp", "psg", "ym2612", "io", "bus", "timing", "ram", "z80_ram", "cartridge", "frame_count"]}
after runUntilVBlank => {frame: 1n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu68k", "z80", "vdp", "psg", "ym2612", "io", "bus", "timing", "ram", "z80_ram", "cartridge", "frame_count", "extension"]}
after runFrameHeadless => {frame: 2n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu68k", "z80", "vdp", "psg", "ym2612", "io", "bus", "timing", "ram", "z80_ram", "cartridge", "frame_count"]}
setRegion('PAL') => undefined
after setRegion('PAL') => {frame: 0n, region: "pal", stateHash: "bigint", stateHashParts: ["cpu68k", "z80", "vdp", "psg", "ym2612", "io", "bus", "timing", "ram", "z80_ram", "cartridge", "frame_count", "extension"]}
setRegion('pal') again => undefined
after setRegion('pal') again => {frame: 1n, region: "pal", stateHash: "bigint", stateHashParts: ["cpu68k", "z80", "vdp", "psg", "ym2612", "io", "bus", "timing", "ram", "z80_ram", "cartridge", "frame_count"]}
setRegion('ntsc') => undefined
after three frames => {frame: 3n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu68k", "z80", "vdp", "psg", "ym2612", "io", "bus", "timing", "ram", "z80_ram", "cartridge", "frame_count"]}
after reset => {frame: 0n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu68k", "z80", "vdp", "psg", "ym2612", "io", "bus", "timing", "ram", "z80_ram", "cartridge", "frame_count", "extension"]}
after hardReset => {frame: 0n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu68k", "z80", "vdp", "psg", "ym2612", "io", "bus", "timing", "ram", "z80_ram", "cartridge", "frame_count", "extension"]}
after powerOnReset => {frame: 0n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu68k", "z80", "vdp", "psg", "ym2612", "io", "bus", "timing", "ram", "z80_ram", "cartridge", "frame_count", "extension"]}
saveState => {core: "sg1", coreVersion: 3, envelope: 1, rom: "06c4e9786233bc19f9daf61e7f91dcb5b84bc4819c0830fa273261741757896a", stamped: true, type: "Uint8Array"}
after saveState => {frame: 3n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu68k", "z80", "vdp", "psg", "ym2612", "io", "bus", "timing", "ram", "z80_ram", "cartridge", "frame_count"]}
loadState => undefined
after loadState => {frame: 3n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu68k", "z80", "vdp", "psg", "ym2612", "io", "bus", "timing", "ram", "z80_ram", "cartridge", "frame_count"]}
getStateHash after loadState, as saved => true
loadStateAnyRom => undefined
after loadStateAnyRom => {frame: 3n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu68k", "z80", "vdp", "psg", "ym2612", "io", "bus", "timing", "ram", "z80_ram", "cartridge", "frame_count"]}
"#,
    );
}

/// The display readers on a new emulator, then on the capture program after
/// six frames, and after a headless frame, which does not copy its picture
/// to the host's framebuffer.
#[wasm_bindgen_test]
fn what_the_display_readers_answer() {
    let mut found = Found::default();
    let mut emu = RetroEmulator::new("genesis").unwrap();
    found.value("display, new", display(&emu));
    emu.load_rom(&capture_shapes_rom()).unwrap();
    for _ in 0..6 {
        emu.run_frame();
    }
    found.value("display, six frames", display(&emu));
    emu.run_frame_headless();
    found.value("display, headless frame", display(&emu));
    found.check(
        r#"
display, new => {data: Uint8ClampedArray(len 286720, fnv1a64 0xe97846eb7709a325), displayInfo: {aspect_ratio: 1.3333... (453 chars, fnv1a64 0xe3fcd670b6143355)
display, six frames => {data: Uint8ClampedArray(len 286720, fnv1a64 0x90450a1e4b4824fc), displayInfo: {aspect_ratio: 1.3333... (453 chars, fnv1a64 0x4bf0f195cf679ca9)
display, headless frame => {data: Uint8ClampedArray(len 286720, fnv1a64 0x90450a1e4b4824fc), displayInfo: {aspect_ratio: 1.3333... (453 chars, fnv1a64 0x4bf0f195cf679ca9)
"#,
    );
}

/// The host's audio on the sounding program: a frame with audio off, then
/// five frames with it on, which overflow the ring, the ring drained part by
/// part, and a headless frame's audio discarded.
#[wasm_bindgen_test]
fn what_the_audio_readers_answer() {
    let mut found = Found::default();
    let mut emu = RetroEmulator::new("genesis").unwrap();
    found.value("getAudioInfo", emu.get_audio_info());
    found.value("ring, new", ring(&emu));
    emu.load_rom(&sounding_rom()).unwrap();
    emu.set_audio_enabled(false);
    emu.run_frame();
    found.value("ring, a frame with audio off", ring(&emu));
    emu.set_audio_enabled(true);
    for _ in 0..5 {
        emu.run_frame();
    }
    found.value("ring, five frames", ring(&emu));
    found.value("getAudioSamplesN(64)", emu.get_audio_samples_n(64));
    found.value("ring, 64 taken", ring(&emu));
    emu.clear_audio_buffer();
    found.value("ring, clearAudioBuffer", ring(&emu));
    emu.discard_pending_audio();
    found.value("ring, discardPendingAudio", ring(&emu));
    found.value("getAudioSamples", emu.get_audio_samples());
    found.value("ring, drained", ring(&emu));
    found.value("getAudioSamplesN(64), drained", emu.get_audio_samples_n(64));
    found.value("getAudioSamples, drained", emu.get_audio_samples());
    emu.run_frame_headless();
    found.value("ring, a headless frame", ring(&emu));
    emu.discard_pending_audio();
    emu.run_frame();
    found.value("ring, discarded, then a frame", ring(&emu));
    found.value("getAudioSamplesN(100000)", emu.get_audio_samples_n(100_000));
    found.check(
        r#"
getAudioInfo => {bits_per_sample: 16, channels: 2, sample_rate: 44100}
ring, new => {available: 0, enabled: true, overflows: 0n}
ring, a frame with audio off => {available: 0, enabled: false, overflows: 0n}
ring, five frames => {available: 5880, enabled: true, overflows: 2950n}
getAudioSamplesN(64) => Float32Array(len 64, fnv1a64 0x3d9feb425890c2cd)
ring, 64 taken => {available: 5816, enabled: true, overflows: 2950n}
ring, clearAudioBuffer => {available: 5816, enabled: true, overflows: 2950n}
ring, discardPendingAudio => {available: 5816, enabled: true, overflows: 2950n}
getAudioSamples => Float32Array(len 5816, fnv1a64 0x6247670a728028a5)
ring, drained => {available: 0, enabled: true, overflows: 2950n}
getAudioSamplesN(64), drained => Float32Array(len 0, fnv1a64 0xcbf29ce484222325)
getAudioSamples, drained => Float32Array(len 0, fnv1a64 0xcbf29ce484222325)
ring, a headless frame => {available: 0, enabled: true, overflows: 2950n}
ring, discarded, then a frame => {available: 1472, enabled: true, overflows: 2950n}
getAudioSamplesN(100000) => Float32Array(len 1472, fnv1a64 0xb3f629ec89614149)
"#,
    );
}

/// Entry `index` of the array `value`.
fn entry(value: &JsValue, index: u32) -> JsValue {
    Array::from(value).get(index)
}

/// The length of the array `value`.
fn length(value: &JsValue) -> u32 {
    Array::from(value).length()
}

/// Channel 0 of the PSG and of the YM2612, its waveform and the mix, each
/// named after `what`.
fn first_channels(found: &mut Found, what: &str, emu: &mut RetroEmulator) {
    let tone = entry(&emu.get_audio_chip_channels(1), 0);
    found.value(format!("getAudioChipChannels(1)[0]{what}"), tone);
    let wave = emu.get_audio_chip_waveform(1, 0, 32);
    found.value(format!("getAudioChipWaveform(1, 0, 32){what}"), wave);
    let fm = entry(&emu.get_audio_chip_channels(0), 0);
    found.value(format!("getAudioChipChannels(0)[0]{what}"), fm);
    let mixed = emu.get_audio_output_waveform(32);
    found.value(format!("getAudioOutputWaveform(32){what}"), mixed);
}

/// The PSG's and the YM2612's readers on the sounding program after three
/// frames, then with channel 0 of each muted for a frame, then unmuted.
#[wasm_bindgen_test]
fn what_the_sound_chips_readers_answer() {
    let mut found = Found::default();
    let mut emu = RetroEmulator::new("genesis").unwrap();
    let chips = emu.get_audio_chips();
    found.value("getAudioChips()[1]", entry(&chips, 1));
    let psg = field(&entry(&chips, 1), "channels");
    found.value("getAudioChips()[1].channels", psg);
    let fm = field(&entry(&chips, 0), "channels");
    found.value("getAudioChips()[0].channels", fm);
    emu.load_rom(&sounding_rom()).unwrap();
    for _ in 0..3 {
        emu.run_frame();
    }
    first_channels(&mut found, "", &mut emu);
    let noise = entry(&emu.get_audio_chip_channels(1), 3);
    found.value("getAudioChipChannels(1)[3]", noise);
    let psg = length(&emu.get_audio_chip_channels(1));
    found.value("getAudioChipChannels(1).length", psg);
    found.value("getAudioChipChannels(2)", emu.get_audio_chip_channels(2));
    let past = emu.get_audio_chip_waveform(1, 4, 8);
    found.value("getAudioChipWaveform(1, 4, 8)", past);
    let last = entry(&emu.get_audio_chip_channels(0), 5);
    found.value("getAudioChipChannels(0)[5]", last);
    let fm = length(&emu.get_audio_chip_channels(0));
    found.value("getAudioChipChannels(0).length", fm);
    found.value(
        "getAudioOutputWaveform(0)",
        emu.get_audio_output_waveform(0),
    );
    emu.mute_audio_chip_channel(1, 0, true);
    emu.mute_audio_chip_channel(0, 0, true);
    emu.run_frame();
    first_channels(&mut found, ", muted", &mut emu);
    emu.mute_audio_chip_channel(1, 0, false);
    emu.mute_audio_chip_channel(0, 0, false);
    first_channels(&mut found, ", unmuted", &mut emu);
    found.check(
        r#"
getAudioChips()[1] => {channel_types: ["Tone", "Tone", "Tone", "Noise"], channels: 4, manufacturer: "Texas Instruments", name: "SN76489", sample_rate: 44100, stereo: false}
getAudioChips()[1].channels => 4
getAudioChips()[0].channels => 6
getAudioChipChannels(1)[0] => {attenuation: 0, channel_id: 0, channel_type: "Tone", current_output: 1, enabled: true, envelope: undefined, frequency: 440.39678955078125, frequency_raw: 254, muted: false, noise_params: undefined, pan: 0, period: 8128, volume: 1}
getAudioChipWaveform(1, 0, 32) => Float32Array(len 32, fnv1a64 0xd41626e6a7e46725)
getAudioChipChannels(0)[0] => {attenuation: 0, channel_id: 0, channel_type: "FM", current_output: -0.1068115234375, enabled: true, envelope: {attack: 1, current_phase: "Sustain", decay: 0, release: 1, sustain: 1}, frequency: 250.745849609375, frequency_raw: 617, muted: false, noise_params: undefined, pan: 0, period: 0, volume: 1}
getAudioOutputWaveform(32) => Float32Array(len 32, fnv1a64 0x8cbe0043c5603ea0)
getAudioChipChannels(1)[3] => {attenuation: 15, channel_id: 3, channel_type: "Noise", current_output: -0, enabled: false, envelope: undefined, frequency: 0, frequency_raw: 0, muted: false, noise_params: {lfsr_state: 2, mode: "Periodic", shift_rate: "N512"}, pan: 0, period: 512, volume: 0}
getAudioChipChannels(1).length => 4
getAudioChipChannels(2) => null
getAudioChipWaveform(1, 4, 8) => Float32Array(len 0, fnv1a64 0xcbf29ce484222325)
getAudioChipChannels(0)[5] => {attenuation: 127, channel_id: 5, channel_type: "FM", current_output: 0, enabled: false, envelope: {attack: 1, current_phase: "Off", decay: 0, release: 0.46666666865348816, sustain: 1}, frequency: 0, frequency_raw: 0, muted: false, noise_params: undefined, pan: 0, period: 0, volume: 0}
getAudioChipChannels(0).length => 6
getAudioOutputWaveform(0) => Float32Array(len 0, fnv1a64 0xcbf29ce484222325)
getAudioChipChannels(1)[0], muted => {attenuation: 0, channel_id: 0, channel_type: "Tone", current_output: 0, enabled: true, envelope: undefined, frequency: 440.39678955078125, frequency_raw: 254, muted: true, noise_params: undefined, pan: 0, period: 8128, volume: 1}
getAudioChipWaveform(1, 0, 32), muted => Float32Array(len 32, fnv1a64 0x8421ae126c7ced25)
getAudioChipChannels(0)[0], muted => {attenuation: 0, channel_id: 0, channel_type: "FM", current_output: -0.95263671875, enabled: true, envelope: {attack: 1, current_phase: "Sustain", decay: 0, release: 1, sustain: 1}, frequency: 250.745849609375, frequency_raw: 617, muted: true, noise_params: undefined, pan: 0, period: 0, volume: 1}
getAudioOutputWaveform(32), muted => Float32Array(len 32, fnv1a64 0x8421ae126c7ced25)
getAudioChipChannels(1)[0], unmuted => {attenuation: 0, channel_id: 0, channel_type: "Tone", current_output: -1, enabled: true, envelope: undefined, frequency: 440.39678955078125, frequency_raw: 254, muted: false, noise_params: undefined, pan: 0, period: 8128, volume: 1}
getAudioChipWaveform(1, 0, 32), unmuted => Float32Array(len 32, fnv1a64 0x8421ae126c7ced25)
getAudioChipChannels(0)[0], unmuted => {attenuation: 0, channel_id: 0, channel_type: "FM", current_output: -0.95263671875, enabled: true, envelope: {attack: 1, current_phase: "Sustain", decay: 0, release: 1, sustain: 1}, frequency: 250.745849609375, frequency_raw: 617, muted: false, noise_params: undefined, pan: 0, period: 0, volume: 1}
getAudioOutputWaveform(32), unmuted => Float32Array(len 32, fnv1a64 0x8421ae126c7ced25)
"#,
    );
}

/// `{ name: number }` as a host sends a key map.
fn map_of(pairs: &[(&str, u32)]) -> JsValue {
    let entries: Vec<(&str, JsValue)> = pairs
        .iter()
        .map(|&(key, value)| (key, value.into()))
        .collect();
    plain_object(&entries)
}

/// The ports as a host reads them, each `[getPortAssignment,
/// getPortProviders, getPortState]`, for ports 0 and 1 and port 5, which
/// the console does not have.
fn ports(emu: &RetroEmulator) -> JsValue {
    let read = |port| {
        let assignment = emu.get_port_assignment(port);
        let providers = emu.get_port_providers(port);
        Array::of3(&assignment, &providers, &emu.get_port_state(port).into())
    };
    [0, 1, 5].into_iter().map(read).collect::<Array>().into()
}

/// `getInputProviders`, sorted by id: the order a map holds them in is not
/// part of what the call answers.
fn providers(emu: &RetroEmulator) -> JsValue {
    let mut list = Array::from(&emu.get_input_providers()).to_vec();
    list.sort_by_key(|provider| field(provider, "id").as_string());
    list.into_iter().collect::<Array>().into()
}

/// Every input call, in one session: the keyboard, a direct input, two
/// gamepads placed and moved, the ports swapped and assigned, the maps and
/// the dead zone set (and a malformed map refused), and the split keyboard
/// turned on and off. After each step, what the readers answer.
#[wasm_bindgen_test]
fn an_input_session() {
    let mut found = Found::default();
    let mut emu = RetroEmulator::new("genesis").unwrap();
    let (keyboard, pad_a, pad_b) = ("keyboard", "pad-a", "pad-b");
    found.value("getInputProviders, new", providers(&emu));
    found.value("ports, new", ports(&emu));
    found.value("getKeyboardMapping, new", emu.get_keyboard_mapping());
    let port = emu.get_provider_port(keyboard.into());
    found.value("getProviderPort('keyboard')", port);
    found.value("isSplitKeyboardEnabled", emu.is_split_keyboard_enabled());
    found.value("getInputHistory(8)", emu.get_input_history(8));
    emu.key_down("ArrowUp".into());
    emu.key_down("KeyA".into());
    found.value("ports, keyDown ArrowUp and KeyA", ports(&emu));
    emu.key_up("ArrowUp".into());
    found.value("ports, keyUp ArrowUp", ports(&emu));
    emu.set_input(1, 0x81);
    found.value("ports, setInput(1, 0x81)", ports(&emu));
    emu.set_input(1, 0);
    emu.gamepad_button(pad_a.into(), 0, true);
    found.value("getInputProviders, pad-a pressed", providers(&emu));
    found.value("ports, pad-a pressed", ports(&emu));
    emu.gamepad_axis(pad_a.into(), 0, -0.8);
    emu.gamepad_axis(pad_a.into(), 1, 0.3);
    found.value("ports, pad-a's axes", ports(&emu));
    found.value("addGamepad('pad-b')", emu.add_gamepad(pad_b.into()));
    found.value("addGamepad('pad-b') again", emu.add_gamepad(pad_b.into()));
    let port = emu.get_provider_port(pad_b.into());
    found.value("getProviderPort('pad-b'), added", port);
    emu.set_provider_port(pad_b.into(), 1);
    emu.gamepad_button(pad_b.into(), 1, true);
    found.value("ports, pad-b on port 1, pressed", ports(&emu));
    emu.swap_port_assignments();
    found.value("ports, swapped", ports(&emu));
    emu.assign_provider_to_port(keyboard.into(), 1);
    found.value("ports, keyboard alone on port 1", ports(&emu));
    emu.assign_provider_to_port(String::new(), 0);
    found.value("ports, port 0 cleared", ports(&emu));
    emu.set_provider_port(pad_a.into(), -1);
    let port = emu.get_provider_port(pad_a.into());
    found.value("getProviderPort('pad-a'), off", port);
    emu.set_provider_port(keyboard.into(), 0);
    emu.set_provider_port(pad_b.into(), 1);
    found.value("ports, keyboard and pad-b back", ports(&emu));
    let keys = map_of(&[("KeyZ", 5), ("KeyX", 4)]);
    found.value("setKeyboardMapping", emu.set_keyboard_mapping(keys));
    found.value("getKeyboardMapping, set", emu.get_keyboard_mapping());
    emu.key_down("KeyZ".into());
    found.value("ports, keyDown KeyZ", ports(&emu));
    let refused = emu.set_keyboard_mapping("nope".into());
    found.value("setKeyboardMapping, not a map", refused);
    found.value("getKeyboardMapping, kept", emu.get_keyboard_mapping());
    emu.clear_keyboard_state();
    found.value("ports, clearKeyboardState", ports(&emu));
    let buttons = plain_gamepad_map(&[(1, 5), (2, 7)]);
    let taken = emu.set_gamepad_mapping(pad_b.into(), buttons);
    found.value("setGamepadMapping('pad-b')", taken);
    let map = emu.get_gamepad_mapping(pad_b.into());
    found.value("getGamepadMapping('pad-b')", map);
    found.value("ports, pad-b remapped", ports(&emu));
    let refused = emu.set_gamepad_mapping(pad_b.into(), "nope".into());
    found.value("setGamepadMapping, not a map", refused);
    let map = emu.get_gamepad_mapping(pad_b.into());
    found.value("getGamepadMapping('pad-b'), kept", map);
    let map = emu.get_gamepad_mapping("none".into());
    found.value("getGamepadMapping('none')", map);
    let deadzone = emu.get_gamepad_deadzone(pad_b.into());
    found.value("getGamepadDeadzone('pad-b')", deadzone);
    emu.set_gamepad_deadzone(pad_b.into(), 0.5);
    let deadzone = emu.get_gamepad_deadzone(pad_b.into());
    found.value("getGamepadDeadzone('pad-b'), set", deadzone);
    let deadzone = emu.get_gamepad_deadzone("none".into());
    found.value("getGamepadDeadzone('none')", deadzone);
    emu.clear_gamepad_state(pad_b.into());
    found.value("ports, clearGamepadState('pad-b')", ports(&emu));
    let (first, second) = (map_of(&[("KeyA", 6)]), map_of(&[("KeyL", 6)]));
    let split = emu.enable_split_keyboard(first, second);
    found.value("enableSplitKeyboard", split);
    let split = emu.is_split_keyboard_enabled();
    found.value("isSplitKeyboardEnabled, on", split);
    found.value("getInputProviders, split", providers(&emu));
    emu.key_down("KeyL".into());
    found.value("ports, split, keyDown KeyL", ports(&emu));
    // Which half's map this reads is the order a map holds the halves in.
    let map = shape(&emu.get_keyboard_mapping());
    let a_half = ["{KeyA: 6}", "{KeyL: 6}"].contains(&map.as_str());
    found.value("getKeyboardMapping, split, a half's", a_half);
    let refused = emu.enable_split_keyboard("nope".into(), map_of(&[]));
    found.value("enableSplitKeyboard, not a map", refused);
    emu.disable_split_keyboard();
    let split = emu.is_split_keyboard_enabled();
    found.value("isSplitKeyboardEnabled, off", split);
    found.value("getInputProviders, split off", providers(&emu));
    found.value("ports, split off", ports(&emu));
    let map = emu.get_keyboard_mapping();
    found.value("getKeyboardMapping, split off", map);
    emu.load_rom(&sounding_rom()).unwrap();
    emu.run_frame();
    found.value("getInputHistory(8), a frame", emu.get_input_history(8));
    found.check(
        r#"
getInputProviders, new => [{id: "keyboard", name: "Keyboard", provider_type: "Keyboard"}]
ports, new => [["keyboard", ["keyboard"], 0], [undefined, [], 0], [undefined, [], 0]]
getKeyboardMapping, new => {ArrowDown: 1, ArrowLeft: 2, ArrowRight: 3, ArrowUp: 0, Enter: 7, KeyA: 6, KeyD: 5, KeyE: 8, KeyQ: 10, KeyS: 4, KeyW: 9, ShiftLeft: 11}
getProviderPort('keyboard') => 0
isSplitKeyboardEnabled => false
getInputHistory(8) => []
ports, keyDown ArrowUp and KeyA => [["keyboard", ["keyboard"], 65], [undefined, [], 0], [undefined, [], 0]]
ports, keyUp ArrowUp => [["keyboard", ["keyboard"], 64], [undefined, [], 0], [undefined, [], 0]]
ports, setInput(1, 0x81) => [["keyboard", ["keyboard"], 64], [undefined, [], 129], [undefined, [], 0]]
getInputProviders, pad-a pressed => [{id: "keyboard", name: "Keyboard", provider_type: "Keyboard"}, {id: "pad-a", name: "Gamepad pad-a", provider_type: "Gamepad"}]
ports, pad-a pressed => [["keyboard", ["keyboard", "pad-a"], 64], [undefined, [], 0], [undefined, [], 0]]
ports, pad-a's axes => [["keyboard", ["keyboard", "pad-a"], 68], [undefined, [], 0], [undefined, [], 0]]
addGamepad('pad-b') => true
addGamepad('pad-b') again => false
getProviderPort('pad-b'), added => -1
ports, pad-b on port 1, pressed => [["keyboard", ["keyboard", "pad-a"], 68], ["pad-b", ["pad-b"], 16], [undefined, [], 0]]
ports, swapped => [["pad-b", ["pad-b"], 16], ["keyboard", ["keyboard", "pad-a"], 68], [undefined, [], 0]]
ports, keyboard alone on port 1 => [["pad-b", ["pad-b"], 16], ["keyboard", ["keyboard"], 64], [undefined, [], 0]]
ports, port 0 cleared => [[undefined, [], 0], ["keyboard", ["keyboard"], 64], [undefined, [], 0]]
getProviderPort('pad-a'), off => -1
ports, keyboard and pad-b back => [["keyboard", ["keyboard"], 64], ["pad-b", ["pad-b"], 16], [undefined, [], 0]]
setKeyboardMapping => true
getKeyboardMapping, set => {KeyX: 4, KeyZ: 5}
ports, keyDown KeyZ => [["keyboard", ["keyboard"], 32], ["pad-b", ["pad-b"], 16], [undefined, [], 0]]
setKeyboardMapping, not a map => false
getKeyboardMapping, kept => {KeyX: 4, KeyZ: 5}
ports, clearKeyboardState => [["keyboard", ["keyboard"], 0], ["pad-b", ["pad-b"], 16], [undefined, [], 0]]
setGamepadMapping('pad-b') => true
getGamepadMapping('pad-b') => {1: 5, 2: 7}
ports, pad-b remapped => [["keyboard", ["keyboard"], 0], ["pad-b", ["pad-b"], 32], [undefined, [], 0]]
setGamepadMapping, not a map => false
getGamepadMapping('pad-b'), kept => {1: 5, 2: 7}
getGamepadMapping('none') => {}
getGamepadDeadzone('pad-b') => 0.15000000596046448
getGamepadDeadzone('pad-b'), set => 0.5
getGamepadDeadzone('none') => 0.15000000596046448
ports, clearGamepadState('pad-b') => [["keyboard", ["keyboard"], 0], ["pad-b", ["pad-b"], 0], [undefined, [], 0]]
enableSplitKeyboard => true
isSplitKeyboardEnabled, on => true
getInputProviders, split => [{id: "keyboard-p1", name: "Keyboard (keyboard-p1)", provider_type: "Keyboard"}, {id: "keyboard-p2", name: "Keyboard (keyboard-p2)", provider_type: "Keyboard"}, {id: "pad-a", name: "Gamepad pad-a", provider_type: "Gamepad"}, {id: "pad-b", name: "Gamepad pad-b", provider_type: "Gamepad"}]
ports, split, keyDown KeyL => [["keyboard-p1", ["keyboard-p1"], 0], ["pad-b", ["pad-b", "keyboard-p2"], 64], [undefined, [], 0]]
getKeyboardMapping, split, a half's => true
enableSplitKeyboard, not a map => false
isSplitKeyboardEnabled, off => false
getInputProviders, split off => [{id: "keyboard", name: "Keyboard", provider_type: "Keyboard"}, {id: "pad-a", name: "Gamepad pad-a", provider_type: "Gamepad"}, {id: "pad-b", name: "Gamepad pad-b", provider_type: "Gamepad"}]
ports, split off => [["keyboard", ["keyboard"], 0], ["pad-b", ["pad-b"], 0], [undefined, [], 0]]
getKeyboardMapping, split off => {KeyX: 4, KeyZ: 5}
getInputHistory(8), a frame => []
"#,
    );
}

/// The debugger's memory and disassembly readers on the sounding program
/// after a frame: memory read and written on each bus, and the 68000's and
/// the Z80's code disassembled from an address and around one.
#[wasm_bindgen_test]
fn what_the_memory_and_disassembly_readers_answer() {
    let mut found = Found::default();
    let mut emu = RetroEmulator::new("genesis").unwrap();
    found.value("readMemory(0x100, 16), new", emu.read_memory(0x100, 16));
    emu.load_rom(&sounding_rom()).unwrap();
    emu.run_frame();
    let reads = [(0x100, 16), (0x400, 12), (0xFF_0000, 8), (0xA0_0000, 8)];
    for (address, size) in reads.into_iter().chain([(0xFF_FFFE, 4), (0x400, 0)]) {
        let bytes = emu.read_memory(address, size);
        found.value(format!("readMemory({address:#X}, {size})"), bytes);
    }
    emu.write_memory_debug(0xFF_0000, &[1, 2, 3, 4]);
    emu.write_memory_debug(0xA0_0002, &[0xAA]);
    found.value("work RAM written", emu.read_memory(0xFF_0000, 8));
    found.value("Z80 RAM written", emu.read_memory(0xA0_0000, 8));
    found.value("disassembleAt(0x400, 4)", emu.disassemble_at(0x400, 4));
    for (cpu, address, count) in [(0, 0x5B4, 2), (1, 0, 3), (2, 0, 3)] {
        let rows = emu.disassemble_cpu(cpu, address, count);
        let what = format!("disassembleCpu({cpu}, {address:#X}, {count})");
        found.value(what, rows);
    }
    let windows = [
        (0, 0x5B4, 2, 2),
        (0, 0x400, 2, 2),
        (1, 0, 1, 2),
        (2, 0, 1, 2),
    ];
    for (cpu, pc, before, after) in windows {
        let rows = emu.disassemble_window(cpu, pc, before, after);
        let what = format!("disassembleWindow({cpu}, {pc:#X}, {before}, {after})");
        found.value(what, rows);
    }
    found.check(
        r##"
readMemory(0x100, 16), new => Uint8Array(len 16, fnv1a64 0xd6607508f5a1e855)
readMemory(0x100, 16) => Uint8Array(len 16, fnv1a64 0x2fceb20b2740f6db)
readMemory(0x400, 12) => Uint8Array(len 12, fnv1a64 0x6f78afb3e11a7838)
readMemory(0xFF0000, 8) => Uint8Array(len 8, fnv1a64 0xa8c7f832281a39c5)
readMemory(0xA00000, 8) => Uint8Array(len 8, fnv1a64 0xa8c7f832281a39c5)
readMemory(0xFFFFFE, 4) => Uint8Array(len 4, fnv1a64 0x4d25767f9dce13f5)
readMemory(0x400, 0) => Uint8Array(len 0, fnv1a64 0xcbf29ce484222325)
work RAM written => Uint8Array(len 8, fnv1a64 0x6a9750b4c81e78ad)
Z80 RAM written => Uint8Array(len 8, fnv1a64 0x9f42ff2bdd53b0cf)
disassembleAt(0x400, 4) => [{address: 1024, bytes: [70, 252, 39, 0], comment: undefined, mnemonic: "MOVE", operands: "#$2700, S... (482 chars, fnv1a64 0xffeec56d0bf2295b)
disassembleCpu(0, 0x5B4, 2) => [{address: 1460, bytes: [96, 254], comment: "target=$0005B4", mnemonic: "BRA", operands: "$000005B4"}, {address: 1462, bytes: [0, 0, 0, 0], comment: undefined, mnemonic: "ORI.B", operands: "#$00, D0"}]
disassembleCpu(1, 0x0, 3) => [{address: 0, bytes: [0], comment: undefined, mnemonic: "nop", operands: ""}, {address: 1, bytes: [0], comment: undefined, mnemonic: "nop", operands: ""}, {address: 2, bytes: [170], comment: undefined, mnemonic: "xor", operands: "d"}]
disassembleCpu(2, 0x0, 3) => []
disassembleWindow(0, 0x5B4, 2, 2) => [{address: 1444, bytes: [19, 252, 0, 15, 0, 192, 0, 17], comment: undefined, kind: "code", mnemonic:... (490 chars, fnv1a64 0x1067a56d32ba61e6)
disassembleWindow(0, 0x400, 2, 2) => [{address: 1022, bytes: [], comment: undefined, kind: "unknown", mnemonic: "", operands: ""}, {addre... (445 chars, fnv1a64 0xd5c0e1e8fbe37166)
disassembleWindow(1, 0x0, 1, 2) => [{address: 0, bytes: [], comment: undefined, kind: "unknown", mnemonic: "", operands: ""}, {address: 0, bytes: [0], comment: undefined, kind: "code", mnemonic: "nop", operands: ""}, {address: 1, bytes: [], comment: undefined, kind: "unknown", mnemonic: "", operands: ""}]
disassembleWindow(2, 0x0, 1, 2) => [{address: 0, bytes: [], comment: undefined, kind: "unknown", mnemonic: "", operands: ""}, {address: 0, bytes: [0, 0, 0, 0], comment: undefined, kind: "code", mnemonic: "ORI.B", operands: "#$00, D0"}, {address: 4, bytes: [], comment: undefined, kind: "unknown", mnemonic: "", operands: ""}]
"##,
    );
}

/// The VDP's readers on the capture program after six frames: video memory,
/// the colours and the vertical scroll (each written once by the debugger),
/// tiles and palettes as pixels, the layouts of the planes and the window,
/// the sprite table, the registers and the status, the layers (one hidden),
/// single sprites as images, and runs of tiles decoded, with the requests
/// the VDP refuses.
#[wasm_bindgen_test]
fn what_the_vdp_readers_answer() {
    let mut found = Found::default();
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.load_rom(&capture_shapes_rom()).unwrap();
    for _ in 0..6 {
        emu.run_frame();
    }
    found.value("getVram", emu.get_vram());
    found.value("getPaletteMemory", emu.get_palette_memory());
    found.value("getVsram", emu.get_vsram());
    emu.write_palette_memory(1, &[0x0EEE, 0x0E0E]);
    let written = emu.get_palette_memory();
    found.value("getPaletteMemory, colours 1 and 2 written", written);
    emu.write_vsram(0, &[0x0123]);
    found.value("getVsram, word 0 written", emu.get_vsram());
    for (tile, first_colour) in [(1, 0), (20, 48)] {
        let picture = emu.get_tile_colours(0, 0, tile, 1, 4, 0, first_colour);
        let what = format!("getTileColours(0, 0, {tile}, 1, 4, 0, {first_colour}).data");
        found.value(what, field(&picture, "data"));
    }
    let past = emu.get_tile_colours(0, 0, 4096, 1, 4, 0, 0);
    found.value("getTileColours(0, 0, 4096, 1, 4, 0, 0)", past);
    found.value("getPaletteColours", emu.get_palette_colours());
    for layer in 0..3 {
        let layout = emu.get_layer_layout(layer);
        found.value(format!("getLayerLayout({layer})"), layout);
    }
    let table = emu.get_sprite_table();
    let count = length(&table);
    found.value("getSpriteTable", table);
    found.value("getSpriteTable().length", count);
    found.value("getSpriteData", emu.get_sprite_data());
    found.value("getColourLevels", emu.get_colour_levels());
    found.value("getVideoRegisters", emu.get_video_registers());
    found.value("getVdpStatus", emu.get_vdp_status());
    found.value("getLayerInfo", emu.get_layer_info());
    emu.toggle_layer(1, false);
    found.value("getLayerInfo, plane B hidden", emu.get_layer_info());
    emu.toggle_layer(1, true);
    found.value("getLayerInfo, shown again", emu.get_layer_info());
    for sprite in [0, 3, 47, 79, 80] {
        let image = emu.export_sprite_image(sprite);
        found.value(format!("exportSpriteImage({sprite})"), image);
    }
    let requests = [
        (0, 0, 1, 4, 4, 0),
        (0, 0x20, 19, 2, 4, 0),
        (0, 0, 1, 2, 4, 1),
        (1, 0, 0, 2, 4, 0),
        (0, 0, 1, 2, 8, 0),
        (0, 0, 1, 2, 4, 2),
        (0, 0, 1, 0, 4, 0),
        (0, 0, 2047, 2, 4, 0),
    ];
    for (source, base, first, count, bpp, size) in requests {
        let sheet = emu.decode_tiles(source, base, first, count, bpp, size);
        let what = format!("decodeTiles({source}, {base:#X}, {first}, {count}, {bpp}, {size})");
        found.value(what, sheet);
    }
    found.check(
        r#"
getVram => Uint8Array(len 65536, fnv1a64 0x1c28c67504e3498b)
getPaletteMemory => Uint16Array(len 64, fnv1a64 0x80a6cbe3ccc7196f)
getVsram => Uint16Array(len 64, fnv1a64 0xbb0bb9ec6512e521)
getPaletteMemory, colours 1 and 2 written => Uint16Array(len 64, fnv1a64 0x0f2a23eab6051b01)
getVsram, word 0 written => Uint16Array(len 64, fnv1a64 0x1240cb3fe0a43a81)
getTileColours(0, 0, 1, 1, 4, 0, 0).data => Uint8Array(len 256, fnv1a64 0xe0ebd3888c0a6a8e)
getTileColours(0, 0, 20, 1, 4, 0, 48).data => Uint8Array(len 256, fnv1a64 0xe6e17db534277b25)
getTileColours(0, 0, 4096, 1, 4, 0, 0) => null
getPaletteColours => Uint8Array(len 256, fnv1a64 0x04a222ae447406c7)
getLayerLayout(0) => {bits_per_pixel: 4, cell_height: 8, cell_width: 8, character_base: 0, first_colour: 0, id: 0, kind: "tile_map", line_scroll: [], map_base: 49152, map_height: 32, map_width: 64, scroll_x: 0, scroll_y: 291}
getLayerLayout(1) => {bits_per_pixel: 4, cell_height: 8, cell_width: 8, character_base: 0, first_colour: 0, id: 1, kind: "tile_map", line_scroll: [], map_base: 57344, map_height: 32, map_width: 64, scroll_x: 0, scroll_y: 0}
getLayerLayout(2) => {bits_per_pixel: 4, cell_height: 8, cell_width: 8, character_base: 0, first_colour: 0, id: 2, kind: "window", line_scroll: [], map_base: 0, map_height: 32, map_width: 64, scroll_x: 0, scroll_y: 0}
getSpriteTable => [{flip_h: false, flip_v: false, height: 8, index: 0, large: false, next: 1, palette: 0, picture: {he... (10109 chars, fnv1a64 0xe68eaad726e710e5)
getSpriteTable().length => 48
getSpriteData => [{flip_h: false, flip_v: false, height: 8, index: 0, palette: 0, picture: {height: 8, width: 8, x: 3... (13560 chars, fnv1a64 0xe17ae78d3f223d43)
getColourLevels => {levels: [0, 18, 36, 55, 73, 91, 109, 128, 146, 164, 182, 200, 219, 237, 255], name: "mega-drive-15"}
getVideoRegisters => [{description: "", index: 0, name: "Mode Register 1", value: 20}, {description: "", index: 1, name: ... (1576 chars, fnv1a64 0xbef5942b44db4bf4)
getVdpStatus => 13932
getLayerInfo => [{enabled: true, height: 256, id: 0, line_scroll: [], name: "Plane A", scroll_x: 0, scroll_y: 291, w... (437 chars, fnv1a64 0xfd79da23cb91b968)
getLayerInfo, plane B hidden => [{enabled: true, height: 256, id: 0, line_scroll: [], name: "Plane A", scroll_x: 0, scroll_y: 291, w... (438 chars, fnv1a64 0xdbbf556cca996dbb)
getLayerInfo, shown again => [{enabled: true, height: 256, id: 0, line_scroll: [], name: "Plane A", scroll_x: 0, scroll_y: 291, w... (437 chars, fnv1a64 0xfd79da23cb91b968)
exportSpriteImage(0) => {data: Uint8Array(len 256, fnv1a64 0xe0ebd3888c0a6a8e), height: 8, width: 8}
exportSpriteImage(3) => {data: Uint8Array(len 4096, fnv1a64 0x12adf2e19c864b76), height: 32, width: 32}
exportSpriteImage(47) => {data: Uint8Array(len 256, fnv1a64 0xafbf60ce88c540ec), height: 8, width: 8}
exportSpriteImage(79) => {data: Uint8Array(len 256, fnv1a64 0xd80ac658736bb725), height: 8, width: 8}
exportSpriteImage(80) => null
decodeTiles(0, 0x0, 1, 4, 4, 0) => {bpp: 4, height: 32, pixels: Uint8Array(len 256, fnv1a64 0xee2485c001ddd31a), transparentIndex: 0, width: 8}
decodeTiles(0, 0x20, 19, 2, 4, 0) => {bpp: 4, height: 16, pixels: Uint8Array(len 128, fnv1a64 0x7deeb9a02d4379a5), transparentIndex: 0, width: 8}
decodeTiles(0, 0x0, 1, 2, 4, 1) => null
decodeTiles(1, 0x0, 0, 2, 4, 0) => null
decodeTiles(0, 0x0, 1, 2, 8, 0) => null
decodeTiles(0, 0x0, 1, 2, 4, 2) => null
decodeTiles(0, 0x0, 1, 0, 4, 0) => null
decodeTiles(0, 0x0, 2047, 2, 4, 0) => null
"#,
    );
}

/// The cartridge readers every cartridge answers, each named after `what`.
fn cartridge_readers(found: &mut Found, what: &str, emu: &mut RetroEmulator) {
    let info = emu.get_cartridge_info();
    found.outcome(format!("getCartridgeInfo, {what}"), info);
    let title = emu.get_cartridge_title();
    found.value(format!("getCartridgeTitle, {what}"), title);
    found.outcome(format!("getMapperState, {what}"), emu.get_mapper_state());
    found.outcome(format!("getChipState, {what}"), emu.get_chip_state());
    found.outcome(format!("getChipStates, {what}"), emu.get_chip_states());
    found.outcome(format!("getMemoryMap, {what}"), emu.get_memory_map());
    let manifest = emu.get_cartridge_manifest();
    found.value(format!("getCartridgeManifest, {what}"), manifest);
}

/// The cartridge's readers with no cartridge, on a cartridge with save RAM
/// whose program writes it, and on an SVP cartridge; and the manifest
/// detected, staged (a malformed one and one of an unknown version refused,
/// one whose preset the load refuses) and cleared.
#[wasm_bindgen_test]
fn what_the_cartridge_readers_answer() {
    let mut found = Found::default();
    let mut emu = RetroEmulator::new("genesis").unwrap();
    cartridge_readers(&mut found, "no cartridge", &mut emu);
    found.value("getSaveRam, no cartridge", emu.get_save_ram());
    let rom = saving_rom();
    let detected = emu.detect_manifest(&rom).unwrap();
    found.value("detectManifest", detected.clone());
    let refused = done(emu.set_cartridge_manifest("{ not json }"));
    found.outcome("setCartridgeManifest, not JSON", refused);
    let newer = concat!(
        r#"{"core":"sg1","version":"9.9","rom":{"sha256":"x","size":1},"#,
        r#""mapping":{"preset":"plain"}}"#
    );
    let refused = done(emu.set_cartridge_manifest(newer));
    found.outcome("setCartridgeManifest, version 9.9", refused);
    let staged = done(emu.set_cartridge_manifest(&detected));
    found.outcome("setCartridgeManifest", staged);
    emu.load_rom(&rom).unwrap();
    let manifest = emu.get_cartridge_manifest();
    found.value("getCartridgeManifest, staged", manifest);
    let bogus = concat!(
        r#"{"core":"sg1","version":"1.0","rom":{"sha256":"x","size":1},"#,
        r#""mapping":{"preset":"bogus"}}"#
    );
    emu.set_cartridge_manifest(bogus).unwrap();
    let refused = done(emu.load_rom(&rom));
    found.outcome("loadRom, a staged manifest that fails", refused);
    emu.clear_cartridge_manifest();
    emu.load_rom(&rom).unwrap();
    found.value("getDetectedManifest", emu.get_detected_manifest().unwrap());
    cartridge_readers(&mut found, "save RAM", &mut emu);
    found.value("getSaveRam", emu.get_save_ram());
    let dirty = emu.is_cartridge_sram_dirty();
    found.value("isCartridgeSramDirty, loaded", dirty);
    emu.run_frame();
    found.value("getSaveRam, the program's byte", emu.get_save_ram());
    let dirty = emu.is_cartridge_sram_dirty();
    found.value("isCartridgeSramDirty, written", dirty);
    emu.clear_cartridge_sram_dirty();
    let dirty = emu.is_cartridge_sram_dirty();
    found.value("isCartridgeSramDirty, cleared", dirty);
    emu.set_save_ram(&[9, 8, 7, 6]);
    found.value("getSaveRam, set", emu.get_save_ram());
    let dirty = emu.is_cartridge_sram_dirty();
    found.value("isCartridgeSramDirty, set", dirty);
    cartridge_readers(&mut found, "SVP", &mut with_svp_cartridge());
    found.check(
        r#"
getCartridgeInfo, no cartridge => throws "No cartridge loaded"
getCartridgeTitle, no cartridge => undefined
getMapperState, no cartridge => {banks: [], mapper_type: "None", registers: []}
getChipState, no cartridge => null
getChipStates, no cartridge => []
getMemoryMap, no cartridge => {regions: []}
getCartridgeManifest, no cartridge => {mapperState: {bank_config: {banks: []}, mapper_id: {Custom: ""}, registers: []}, memoryMap: {regions: []}, romRegions: [], sramRegions: []}
getSaveRam, no cartridge => undefined
detectManifest => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"sg1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"3e8ea16f75fdddd05ce46ad2acda5d99b2614035778606ed807dfaed1a64a042\",\"size\":524288,\"title\":\"SAVING\",\"region\":\"Japan\"},\"mapping\":{\"preset\":\"plain\",\"sram_size\":16384,\"slots\":[]},\"chips\":[],\"metadata\":null}"
setCartridgeManifest, not JSON => throws "invalid JSON: key must be a string at line 1 column 3"
setCartridgeManifest, version 9.9 => throws "unsupported manifest version: 9.9"
setCartridgeManifest => undefined
getCartridgeManifest, staged => {mapperState: {bank_config: {banks: [{bank_number: 0, cpu_address_end: 2113535, cpu_address_start: 2... (861 chars, fnv1a64 0x10fc8814668efb76)
loadRom, a staged manifest that fails => throws "unsupported preset: bogus"
getDetectedManifest => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"sg1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"3e8ea16f75fdddd05ce46ad2acda5d99b2614035778606ed807dfaed1a64a042\",\"size\":524288,\"title\":\"SAVING\",\"region\":\"Japan\"},\"mapping\":{\"preset\":\"plain\",\"sram_size\":16384,\"slots\":[]},\"chips\":[],\"metadata\":null}"
getCartridgeInfo, save RAM => {calculated_checksum: 47852, detection_source: "Header", expansion_chip_info: undefined, header_checksum: 0, mapper_info: undefined, regions: ["Japan", "USA", "Europe"], rom_hash: "3e8ea16f75fdddd05ce46ad2acda5d99b2614035778606ed807dfaed1a64a042", rom_size: 524288, serial: undefined, sram_size: 16384, sram_type: "Battery", system: "Genesis", title: "SAVING"}
getCartridgeTitle, save RAM => "SAVING"
getMapperState, save RAM => {banks: [{bank_number: 0, cpu_address_end: 2113535, cpu_address_start: 2097152, memory_type: "SRAM", physical_offset: 0, size: 16384}, {bank_number: 1, cpu_address_end: 4194303, cpu_address_start: 0, memory_type: "ROM", physical_offset: 0, size: 4194304}], mapper_type: "None", registers: []}
getChipState, save RAM => null
getChipStates, save RAM => []
getMemoryMap, save RAM => {regions: [{detail: "", end: 2113535, label: "SRAM", start: 2097152}, {detail: "mask=0x07FFFF", end: 4194303, label: "ROM", start: 0}]}
getCartridgeManifest, save RAM => {mapperState: {bank_config: {banks: [{bank_number: 0, cpu_address_end: 2113535, cpu_address_start: 2... (861 chars, fnv1a64 0x10fc8814668efb76)
getSaveRam => Uint8Array(len 16384, fnv1a64 0x0d65b7532d396325)
isCartridgeSramDirty, loaded => false
getSaveRam, the program's byte => Uint8Array(len 16384, fnv1a64 0x2240852aae1cdd9e)
isCartridgeSramDirty, written => true
isCartridgeSramDirty, cleared => false
getSaveRam, set => Uint8Array(len 16384, fnv1a64 0x041c8c104a963a61)
isCartridgeSramDirty, set => false
getCartridgeInfo, SVP => {calculated_checksum: 0, detection_source: "Header", expansion_chip_info: {chip_type: "SVP", descrip... (425 chars, fnv1a64 0xca0dc5889557b76c)
getCartridgeTitle, SVP => "SVP GAME"
getMapperState, SVP => {banks: [{bank_number: 0, cpu_address_end: 10571787, cpu_address_start: 10571776, memory_type: "Mapp... (703 chars, fnv1a64 0x6b6b053f378337ab)
getChipState, SVP => {chip_type: "SVP", cycle_count: 0, internal_ram: "Array(len 1024, bytes fnv1a64 0x51d88627df287325)"... (676 chars, fnv1a64 0x9e08010f46ae4286)
getChipStates, SVP => [{chipId: 0, label: "SVP", state: {chip_type: "SVP", cycle_count: 0, internal_ram: "Array(len 1024, ... (712 chars, fnv1a64 0x4cc066c62b82c2a7)
getMemoryMap, SVP => {regions: [{detail: "base=0x100000", end: 10571787, label: "Chip 0", start: 10571776}, {detail: "", end: 3276799, label: "Chip 0", start: 3145728}, {detail: "base=0x200000", end: 3801087, label: "Chip 0", start: 3735552}, {detail: "base=0x300000", end: 3866623, label: "Chip 0", start: 3801088}, {detail: "mask=0x07FFFF", end: 2097151, label: "ROM", start: 0}]}
getCartridgeManifest, SVP => {mapperState: {bank_config: {banks: [{bank_number: 0, cpu_address_end: 10571787, cpu_address_start: ... (1695 chars, fnv1a64 0x3f43c530eff4ccd7)
"#,
    );
}

/// A system the crate does not emulate is refused with a text naming the
/// ones it does.
#[wasm_bindgen_test]
fn an_unknown_system_is_refused() {
    let mut found = Found::default();
    let refused = RetroEmulator::new("nes").map(|_| JsValue::UNDEFINED);
    found.outcome("new RetroEmulator('nes')", refused);
    found.check(
        r#"
new RetroEmulator('nes') => throws "Unknown system: 'nes'. Supported: genesis, megadrive, md"
"#,
    );
}
