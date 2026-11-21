//! What the bindings answer, method by method, recorded on the bindings as
//! they are: the machine's calls and what each leaves, the display after
//! every call that makes a frame, the audio, every input call in one
//! scripted session, the debugger's memory and disassembly readers, the
//! PPU's and the S-DSP's readers, and the cartridge's. Each value is held to
//! the description `retro_wasm::test_support::shape` gives of it, so a
//! change that moves a key, a value, a type or a byte a host reads fails
//! here: record a new description only for a change a host is meant to see.
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
//! which names the commit, and the framebuffer's address.

use js_sys::{Array, Map, Object, Reflect};
use retro_core::hash::fnv1a64;
use retro_core::savestate::read_header;
use retro_wasm::js::plain_object;
use retro_wasm::test_support::shape;

use super::capture::capture_program_rom;
#[cfg(feature = "chip-superfx")]
use super::debugger_shape_tests::super_fx_rom;
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

/// The machine and the display after `what`: two lines.
fn after(found: &mut Found, what: &str, emu: &RetroEmulator) {
    found.value(format!("after {what}"), machine(emu));
    found.value(format!("display after {what}"), display(emu));
}

/// The machine's calls, in the order a host makes them, and what each one
/// leaves, the display included: the program moves a sprite and changes a
/// colour every frame, so each frame shown has a picture of its own. Loads
/// refused and taken, the frames (a headless one leaves the picture the host
/// reads as it was), the region, the three resets, a savestate saved and
/// loaded back, and a frame of 239 lines cropped and shown.
#[wasm_bindgen_test]
fn the_machine_calls_and_what_each_leaves() {
    let mut found = Found::default();
    let mut emu = RetroEmulator::new("snes").unwrap();
    found.value("getSystemInfo, new", emu.get_system_info());
    found.value("getRomInfo, new", emu.get_rom_info());
    after(&mut found, "new", &emu);
    // The build id ends with the commit it was built from.
    let build = emu.get_build_id();
    let (named, commit) = build.split_once('+').unwrap();
    assert!(!commit.is_empty());
    found.value("getBuildId, up to the commit", format!("{named}+"));
    found.outcome("loadRom, no bytes", done(emu.load_rom(&[])));
    let bogus = concat!(
        r#"{"core":"snes1","version":"1.0","rom":{"sha256":"","size":0},"#,
        r#""mapping":{"preset":"bogus"}}"#
    );
    emu.set_cartridge_manifest(bogus).unwrap();
    let staged = done(emu.load_rom(&capture_program_rom()));
    found.outcome("loadRom, a staged manifest that fails", staged);
    emu.clear_cartridge_manifest();
    after(&mut found, "the refusals", &emu);
    // Ten bytes are no ROM, but the detector's best guess maps them.
    found.outcome("loadRom, ten bytes", done(emu.load_rom(&[0; 10])));
    found.value("getRomInfo, ten bytes", emu.get_rom_info());
    found.outcome("loadRom", done(emu.load_rom(&capture_program_rom())));
    found.value("getRomInfo", emu.get_rom_info());
    after(&mut found, "loadRom", &emu);
    emu.run_frame();
    after(&mut found, "runFrame", &emu);
    emu.run_frame();
    after(&mut found, "a second runFrame", &emu);
    emu.run_until_vblank();
    after(&mut found, "runUntilVBlank", &emu);
    found.value("saveState, held", savestate(&emu.save_state()));
    after(&mut found, "saveState", &emu);
    emu.run_frame_headless();
    after(&mut found, "runFrameHeadless", &emu);
    emu.run_frame();
    after(&mut found, "runFrame after it", &emu);
    found.outcome("setRegion('PAL')", done(emu.set_region("PAL")));
    after(&mut found, "setRegion('PAL')", &emu);
    emu.run_frame();
    found.outcome("setRegion('pal') again", done(emu.set_region("pal")));
    after(&mut found, "setRegion('pal') again", &emu);
    found.outcome("setRegion('ntsc')", done(emu.set_region("ntsc")));
    after(&mut found, "setRegion('ntsc')", &emu);
    for _ in 0..3 {
        emu.run_frame();
    }
    emu.reset();
    after(&mut found, "reset", &emu);
    emu.run_frame();
    emu.hard_reset();
    after(&mut found, "hardReset", &emu);
    emu.run_frame();
    emu.power_on_reset();
    after(&mut found, "powerOnReset", &emu);
    for _ in 0..3 {
        emu.run_frame();
    }
    let hash = emu.get_state_hash();
    let state = emu.save_state();
    emu.run_frame();
    found.outcome("loadState", done(emu.load_state(&state)));
    after(&mut found, "loadState", &emu);
    let as_saved = emu.get_state_hash() == hash;
    found.value("getStateHash after loadState, as saved", as_saved);
    emu.run_frame();
    found.outcome("loadStateAnyRom", done(emu.load_state_any_rom(&state)));
    after(&mut found, "loadStateAnyRom", &emu);
    found.value("isShowOverscanEnabled", emu.is_show_overscan_enabled());
    emu.write_memory_debug(0x2133, &[0x04]); // SETINI: 239 lines
    emu.run_frame();
    after(&mut found, "a frame of 239 lines", &emu);
    emu.set_show_overscan(true);
    let shown = emu.is_show_overscan_enabled();
    found.value("isShowOverscanEnabled, shown", shown);
    emu.run_frame();
    after(&mut found, "a frame of 239 lines shown", &emu);
    emu.set_show_overscan(false);
    let shown = emu.is_show_overscan_enabled();
    found.value("isShowOverscanEnabled, cropped", shown);
    after(&mut found, "setShowOverscan(false)", &emu);
    found.check(
        r#"
getSystemInfo, new => {audio: {bits_per_sample: 32, channels: 2, sample_rate: 44100}, cpus: [{address_width: 24, architect... (951 chars, fnv1a64 0x991c27304b198b2f)
getRomInfo, new => undefined
after new => {frame: 0n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after new => {data: Uint8ClampedArray(len 229376, fnv1a64 0xc9b1e505f9a82325), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0xecc4bf100c6d15e3)
getBuildId, up to the commit => "snes1@0.1.0+"
loadRom, no bytes => throws "ROM is empty after SMC strip"
loadRom, a staged manifest that fails => throws "manifest error: unsupported preset: bogus"
after the refusals => {frame: 0n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after the refusals => {data: Uint8ClampedArray(len 229376, fnv1a64 0xc9b1e505f9a82325), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0xecc4bf100c6d15e3)
loadRom, ten bytes => undefined
getRomInfo, ten bytes => undefined
loadRom => undefined
getRomInfo => {checksum: 4660, mapper: "LoROM", region: "USA", size: 131072, title: "CAPTURE PROGRAM", title_confidence: "high"}
after loadRom => {frame: 0n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after loadRom => {data: Uint8ClampedArray(len 229376, fnv1a64 0xc9b1e505f9a82325), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0xecc4bf100c6d15e3)
after runFrame => {frame: 1n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after runFrame => {data: Uint8ClampedArray(len 229376, fnv1a64 0xb0f0ebe99fb6c2ac), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0xb2f7b367c9d3286b)
after a second runFrame => {frame: 2n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after a second runFrame => {data: Uint8ClampedArray(len 229376, fnv1a64 0x784768a1b2b37560), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0x61a863ae27cc97b1)
after runUntilVBlank => {frame: 2n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after runUntilVBlank => {data: Uint8ClampedArray(len 229376, fnv1a64 0x75f379eed654dafa), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0x194867b673dff128)
saveState, held => {core: "snes1", coreVersion: 4, envelope: 1, rom: "a5346783813b83c2b6d5f38fd6606b44678654a3b6f52de3a07d66d101841885", stamped: false, type: "Uint8Array"}
after saveState => {frame: 3n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after saveState => {data: Uint8ClampedArray(len 229376, fnv1a64 0x75f379eed654dafa), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0x194867b673dff128)
after runFrameHeadless => {frame: 4n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after runFrameHeadless => {data: Uint8ClampedArray(len 229376, fnv1a64 0x75f379eed654dafa), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0xd74e17f3a0691db1)
after runFrame after it => {frame: 5n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after runFrame after it => {data: Uint8ClampedArray(len 229376, fnv1a64 0x99cbcd38246f0743), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0x77c43b7132eb9c96)
setRegion('PAL') => undefined
after setRegion('PAL') => {frame: 0n, region: "pal", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after setRegion('PAL') => {data: Uint8ClampedArray(len 229376, fnv1a64 0xc9b1e505f9a82325), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0x742963b3a5f30912)
setRegion('pal') again => undefined
after setRegion('pal') again => {frame: 1n, region: "pal", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after setRegion('pal') again => {data: Uint8ClampedArray(len 229376, fnv1a64 0xb0f0ebe99fb6c2ac), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0xb711725bedae74c6)
setRegion('ntsc') => undefined
after setRegion('ntsc') => {frame: 0n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after setRegion('ntsc') => {data: Uint8ClampedArray(len 229376, fnv1a64 0xc9b1e505f9a82325), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0xecc4bf100c6d15e3)
after reset => {frame: 0n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after reset => {data: Uint8ClampedArray(len 229376, fnv1a64 0xc9b1e505f9a82325), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0xecc4bf100c6d15e3)
after hardReset => {frame: 0n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after hardReset => {data: Uint8ClampedArray(len 229376, fnv1a64 0xc9b1e505f9a82325), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0xecc4bf100c6d15e3)
after powerOnReset => {frame: 0n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after powerOnReset => {data: Uint8ClampedArray(len 229376, fnv1a64 0xc9b1e505f9a82325), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0xecc4bf100c6d15e3)
loadState => undefined
after loadState => {frame: 3n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after loadState => {data: Uint8ClampedArray(len 229376, fnv1a64 0x063c1408b7656372), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0xa9e1246aed996d1e)
getStateHash after loadState, as saved => true
loadStateAnyRom => undefined
after loadStateAnyRom => {frame: 3n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after loadStateAnyRom => {data: Uint8ClampedArray(len 229376, fnv1a64 0x063c1408b7656372), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0xa9e1246aed996d1e)
isShowOverscanEnabled => false
after a frame of 239 lines => {frame: 4n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after a frame of 239 lines => {data: Uint8ClampedArray(len 229376, fnv1a64 0xab440d62f2dac5c3), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0xf9ef2b1486b088ee)
isShowOverscanEnabled, shown => true
after a frame of 239 lines shown => {frame: 5n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after a frame of 239 lines shown => {data: Uint8ClampedArray(len 245760, fnv1a64 0xd66f1f6f584a37ee), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0x39c467ddb0ad8e9f)
isShowOverscanEnabled, cropped => false
after setShowOverscan(false) => {frame: 5n, region: "ntsc", stateHash: "bigint", stateHashParts: ["cpu", "wram", "ppu", "apu", "dma", "system_regs", "controllers", "sram", "coprocessors", "bus", "scheduler", "timing", "frame_count"]}
display after setShowOverscan(false) => {data: Uint8ClampedArray(len 245760, fnv1a64 0xd66f1f6f584a37ee), displayInfo: {aspect_ratio: 1.1428... (424 chars, fnv1a64 0x7bcc3a54b8cf9ad5)
"#,
    );
}

/// Make the S-DSP play a square wave on voice 0 at full volume: one looping
/// BRR block at $0300 of the sound processor's memory, named by the sample
/// directory at $0200, and the DSP's registers written through its address
/// and data ports, voice 0 keyed on last.
fn sound_a_voice(emu: &mut RetroEmulator) {
    let apu = emu.snes.bus_mut().apu_mut();
    let directory = [0x00, 0x03, 0x00, 0x03];
    let block = [0xB3, 0x77, 0x77, 0x77, 0x77, 0x99, 0x99, 0x99, 0x99];
    for (address, byte) in (0x0200..).zip(directory).chain((0x0300..).zip(block)) {
        apu.spc_write(address, byte);
    }
    #[rustfmt::skip]
    let registers: [(u8, u8); 14] = [
        (0x5D, 0x02), // DIR: the directory at $0200
        (0x6C, 0x20), // FLG: no reset, no mute, no echo writes
        (0x0C, 0x7F), (0x1C, 0x7F), // the main volume
        (0x2C, 0x00), (0x3C, 0x00), // no echo
        (0x00, 0x7F), (0x01, 0x7F), // voice 0's volume
        (0x02, 0x00), (0x03, 0x10), // its pitch, $1000
        (0x04, 0x00), // its sample, 0
        (0x05, 0x00), (0x07, 0x7F), // no envelope, a fixed gain
        (0x4C, 0x01), // KON: voice 0
    ];
    for (register, value) in registers {
        apu.spc_write(0xF2, register);
        apu.spc_write(0xF3, value);
    }
}

/// A program that loops, with voice 0 of the S-DSP sounding.
fn sounding() -> RetroEmulator {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&synth_lorom("SOUNDING")).unwrap();
    sound_a_voice(&mut emu);
    emu
}

/// The host's audio with a voice sounding: a frame with audio off, then
/// five frames with it on, which overflow the ring, the ring drained part by
/// part, and a headless frame's audio discarded.
#[wasm_bindgen_test]
fn what_the_audio_readers_answer() {
    let mut found = Found::default();
    let emu = RetroEmulator::new("snes").unwrap();
    found.value("getAudioInfo", emu.get_audio_info());
    found.value("ring, new", ring(&emu));
    let mut emu = sounding();
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
getAudioInfo => {bits_per_sample: 32, channels: 2, sample_rate: 44100}
ring, new => {available: 0, enabled: false, overflows: 0n}
ring, a frame with audio off => {available: 0, enabled: false, overflows: 0n}
ring, five frames => {available: 8192, enabled: true, overflows: 612n}
getAudioSamplesN(64) => Float32Array(len 64, fnv1a64 0xef44c05101a0cb79)
ring, 64 taken => {available: 8128, enabled: true, overflows: 612n}
ring, clearAudioBuffer => {available: 8128, enabled: true, overflows: 612n}
ring, discardPendingAudio => {available: 8128, enabled: true, overflows: 612n}
getAudioSamples => Float32Array(len 8128, fnv1a64 0x386ed61906ab5db1)
ring, drained => {available: 0, enabled: true, overflows: 612n}
getAudioSamplesN(64), drained => Float32Array(len 0, fnv1a64 0xcbf29ce484222325)
getAudioSamples, drained => Float32Array(len 0, fnv1a64 0xcbf29ce484222325)
ring, a headless frame => {available: 0, enabled: true, overflows: 612n}
ring, discarded, then a frame => {available: 1466, enabled: true, overflows: 612n}
getAudioSamplesN(100000) => Float32Array(len 1466, fnv1a64 0x8c5b1fbb20de3fed)
"#,
    );
}

/// Voice 0's waveform and the mix, each named after `what`.
fn waveforms(found: &mut Found, what: &str, emu: &mut RetroEmulator) {
    let voice = emu.get_audio_chip_waveform(0, 0, 32);
    found.value(format!("getAudioChipWaveform(0, 0, 32){what}"), voice);
    let mixed = emu.get_audio_output_waveform(32);
    found.value(format!("getAudioOutputWaveform(32){what}"), mixed);
}

/// The S-DSP's readers with voice 0 sounding for three frames, then muted
/// for a frame, then unmuted, and after a headless frame, whose sound stays
/// in the sound processor's ring, where the waveform readers find it.
#[wasm_bindgen_test]
fn what_the_dsp_readers_answer() {
    let mut found = Found::default();
    let mut emu = sounding();
    emu.set_audio_enabled(true);
    for _ in 0..3 {
        emu.run_frame();
    }
    found.value("getAudioChipChannels(0)", emu.get_audio_chip_channels(0));
    waveforms(&mut found, "", &mut emu);
    let last = emu.get_audio_chip_waveform(0, 7, 8);
    found.value("getAudioChipWaveform(0, 7, 8)", last);
    let past = emu.get_audio_chip_waveform(0, 8, 8);
    found.value("getAudioChipWaveform(0, 8, 8)", past);
    found.value(
        "getAudioOutputWaveform(0)",
        emu.get_audio_output_waveform(0),
    );
    emu.mute_audio_chip_channel(0, 0, true);
    emu.run_frame();
    let muted = emu.get_audio_chip_channels(0);
    found.value("getAudioChipChannels(0), muted", muted);
    waveforms(&mut found, ", muted", &mut emu);
    emu.mute_audio_chip_channel(0, 0, false);
    let unmuted = emu.get_audio_chip_channels(0);
    found.value("getAudioChipChannels(0), unmuted", unmuted);
    emu.run_frame_headless();
    waveforms(&mut found, ", a headless frame", &mut emu);
    found.check(
        r#"
getAudioChipChannels(0) => [{attenuation: 127, channel_id: 0, channel_type: "Sample", current_output: 0.4296875, enabled: true,... (2195 chars, fnv1a64 0xde6753957e414906)
getAudioChipWaveform(0, 0, 32) => Float32Array(len 32, fnv1a64 0x8421ae126c7ced25)
getAudioOutputWaveform(32) => Float32Array(len 32, fnv1a64 0x8421ae126c7ced25)
getAudioChipWaveform(0, 7, 8) => Float32Array(len 8, fnv1a64 0x0c8210784d8af5a5)
getAudioChipWaveform(0, 8, 8) => Float32Array(len 8, fnv1a64 0x0c8210784d8af5a5)
getAudioOutputWaveform(0) => Float32Array(len 0, fnv1a64 0xcbf29ce484222325)
getAudioChipChannels(0), muted => [{attenuation: 127, channel_id: 0, channel_type: "Sample", current_output: -0.4375, enabled: true, e... (2192 chars, fnv1a64 0x19d2277ebbd3cd60)
getAudioChipWaveform(0, 0, 32), muted => Float32Array(len 32, fnv1a64 0x8421ae126c7ced25)
getAudioOutputWaveform(32), muted => Float32Array(len 32, fnv1a64 0x8421ae126c7ced25)
getAudioChipChannels(0), unmuted => [{attenuation: 127, channel_id: 0, channel_type: "Sample", current_output: -0.4375, enabled: true, e... (2193 chars, fnv1a64 0x8ea1edf393a14155)
getAudioChipWaveform(0, 0, 32), a headless frame => Float32Array(len 32, fnv1a64 0x1f8009ad9343ee95)
getAudioOutputWaveform(32), a headless frame => Float32Array(len 32, fnv1a64 0xcec7f8f42777d7c5)
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
    list.sort_by_key(|provider| property(provider, "id").as_string());
    list.into_iter().collect::<Array>().into()
}

/// Every input call, in one session: the keyboard, a direct input, two
/// gamepads placed and moved, the ports swapped and assigned, the maps and
/// the dead zone set (and a malformed map refused), and the split keyboard
/// turned on and off. After each step, what the readers answer.
#[wasm_bindgen_test]
fn an_input_session() {
    let mut found = Found::default();
    let mut emu = RetroEmulator::new("snes").unwrap();
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
    emu.load_rom(&synth_lorom("INPUT")).unwrap();
    emu.run_frame();
    found.value("getInputHistory(8), a frame", emu.get_input_history(8));
    found.check(
        r#"
getInputProviders, new => [{id: "keyboard", name: "Keyboard", provider_type: "Keyboard"}]
ports, new => [["keyboard", ["keyboard"], 0], [undefined, [], 0], [undefined, [], 0]]
getKeyboardMapping, new => {ArrowDown: 1, ArrowLeft: 2, ArrowRight: 3, ArrowUp: 0, Enter: 10, KeyA: 7, KeyQ: 8, KeyS: 6, KeyW: 9, KeyX: 4, KeyZ: 5, ShiftRight: 11}
getProviderPort('keyboard') => 0
isSplitKeyboardEnabled => false
getInputHistory(8) => []
ports, keyDown ArrowUp and KeyA => [["keyboard", ["keyboard"], 129], [undefined, [], 0], [undefined, [], 0]]
ports, keyUp ArrowUp => [["keyboard", ["keyboard"], 128], [undefined, [], 0], [undefined, [], 0]]
ports, setInput(1, 0x81) => [["keyboard", ["keyboard"], 128], [undefined, [], 129], [undefined, [], 0]]
getInputProviders, pad-a pressed => [{id: "keyboard", name: "Keyboard", provider_type: "Keyboard"}, {id: "pad-a", name: "Gamepad pad-a", provider_type: "Gamepad"}]
ports, pad-a pressed => [["keyboard", ["keyboard", "pad-a"], 160], [undefined, [], 0], [undefined, [], 0]]
ports, pad-a's axes => [["keyboard", ["keyboard", "pad-a"], 164], [undefined, [], 0], [undefined, [], 0]]
addGamepad('pad-b') => true
addGamepad('pad-b') again => false
getProviderPort('pad-b'), added => -1
ports, pad-b on port 1, pressed => [["keyboard", ["keyboard", "pad-a"], 164], ["pad-b", ["pad-b"], 16], [undefined, [], 0]]
ports, swapped => [["pad-b", ["pad-b"], 16], ["keyboard", ["keyboard", "pad-a"], 164], [undefined, [], 0]]
ports, keyboard alone on port 1 => [["pad-b", ["pad-b"], 16], ["keyboard", ["keyboard"], 128], [undefined, [], 0]]
ports, port 0 cleared => [[undefined, [], 0], ["keyboard", ["keyboard"], 128], [undefined, [], 0]]
getProviderPort('pad-a'), off => -1
ports, keyboard and pad-b back => [["keyboard", ["keyboard"], 128], ["pad-b", ["pad-b"], 16], [undefined, [], 0]]
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

/// A program that stores `$12` at `$0010` of work RAM, then loops back to
/// the store.
fn storing_lorom() -> Vec<u8> {
    // LDA #$12, STA $0010, BRA back to the store
    lorom_running("STORING", &[0xA9, 0x12, 0x8D, 0x10, 0x00, 0x80, 0xFB], &[])
}

/// The debugger's memory and disassembly readers on a program that stores a
/// byte to work RAM, after a frame: memory read and written on the main
/// CPU's bus, and the main CPU's and the sound CPU's code disassembled from
/// an address and around one.
#[wasm_bindgen_test]
fn what_the_memory_and_disassembly_readers_answer() {
    let mut found = Found::default();
    let mut emu = RetroEmulator::new("snes").unwrap();
    found.value("readMemory(0x8000, 16), new", emu.read_memory(0x8000, 16));
    emu.load_rom(&storing_lorom()).unwrap();
    emu.run_frame();
    let reads = [(0xFFC0, 32), (0x8000, 8), (0x7E_0010, 4), (0x2100, 4)];
    for (address, size) in reads.into_iter().chain([(0xFF_FFFE, 4), (0x7E_0000, 0)]) {
        let bytes = emu.read_memory(address, size);
        found.value(format!("readMemory({address:#X}, {size})"), bytes);
    }
    emu.write_memory_debug(0x7E_0020, &[1, 2, 3]);
    found.value("work RAM written", emu.read_memory(0x7E_0020, 4));
    found.value("its mirror in bank 0", emu.read_memory(0x0020, 4));
    found.value("disassembleAt(0x8000, 3)", emu.disassemble_at(0x8000, 3));
    for (cpu, address, count) in [(0, 0x8002, 2), (1, 0xFFC0, 3), (2, 0, 2)] {
        let rows = emu.disassemble_cpu(cpu, address, count);
        let what = format!("disassembleCpu({cpu}, {address:#X}, {count})");
        found.value(what, rows);
    }
    let windows = [(0, 0x8005, 2, 2), (0, 0x9000, 1, 1), (1, 0xFFC0, 1, 2)];
    for (cpu, pc, before, after) in windows.into_iter().chain([(2, 0, 1, 1)]) {
        let rows = emu.disassemble_window(cpu, pc, before, after);
        let what = format!("disassembleWindow({cpu}, {pc:#X}, {before}, {after})");
        found.value(what, rows);
    }
    found.check(
        r##"
readMemory(0x8000, 16), new => Uint8Array(len 16, fnv1a64 0x88201fb960ff6465)
readMemory(0xFFC0, 32) => Uint8Array(len 32, fnv1a64 0x1a912c454253a883)
readMemory(0x8000, 8) => Uint8Array(len 8, fnv1a64 0x5c9577f212ba8854)
readMemory(0x7E0010, 4) => Uint8Array(len 4, fnv1a64 0x8cc58f15ad955627)
readMemory(0x2100, 4) => Uint8Array(len 4, fnv1a64 0x4d25767f9dce13f5)
readMemory(0xFFFFFE, 4) => Uint8Array(len 4, fnv1a64 0x4d25767f9dce13f5)
readMemory(0x7E0000, 0) => Uint8Array(len 0, fnv1a64 0xcbf29ce484222325)
work RAM written => Uint8Array(len 4, fnv1a64 0xbe7a5a7751657191)
its mirror in bank 0 => Uint8Array(len 4, fnv1a64 0xbe7a5a7751657191)
disassembleAt(0x8000, 3) => [{address: 32768, bytes: [169, 18], comment: undefined, mnemonic: "lda", operands: "#$12"}, {address: 32770, bytes: [141, 16, 0], comment: undefined, mnemonic: "sta", operands: "$0010"}, {address: 32773, bytes: [128, 251], comment: "(-5)", mnemonic: "bra", operands: "$8002"}]
disassembleCpu(0, 0x8002, 2) => [{address: 32770, bytes: [141, 16, 0], comment: undefined, mnemonic: "sta", operands: "$0010"}, {address: 32773, bytes: [128, 251], comment: "(-5)", mnemonic: "bra", operands: "$8002"}]
disassembleCpu(1, 0xFFC0, 3) => [{address: 65472, bytes: [205, 239], comment: undefined, mnemonic: "mov", operands: "x, #$EF"}, {address: 65474, bytes: [189], comment: undefined, mnemonic: "mov", operands: "sp, x"}, {address: 65475, bytes: [232, 0], comment: undefined, mnemonic: "mov", operands: "a, #$00"}]
disassembleCpu(2, 0x0, 2) => []
disassembleWindow(0, 0x8005, 2, 2) => [{address: 32768, bytes: [169, 18], comment: undefined, kind: "code", mnemonic: "lda", operands: "#$... (412 chars, fnv1a64 0x8d04bb1c6a27cf04)
disassembleWindow(0, 0x9000, 1, 1) => [{address: 36863, bytes: [], comment: undefined, kind: "unknown", mnemonic: "", operands: ""}, {address: 36864, bytes: [0], comment: undefined, kind: "code", mnemonic: "brk", operands: ""}]
disassembleWindow(1, 0xFFC0, 1, 2) => [{address: 65471, bytes: [], comment: undefined, kind: "unknown", mnemonic: "", operands: ""}, {address: 65472, bytes: [205, 239], comment: undefined, kind: "code", mnemonic: "mov", operands: "x, #$EF"}, {address: 65474, bytes: [], comment: undefined, kind: "unknown", mnemonic: "", operands: ""}]
disassembleWindow(2, 0x0, 1, 1) => [{address: 0, bytes: [], comment: undefined, kind: "unknown", mnemonic: "", operands: ""}, {address: 0, bytes: [0], comment: undefined, kind: "unknown", mnemonic: "", operands: ""}]
"##,
    );
}

/// The PPU's readers on the capture program after three frames: video
/// memory, the palette and the sprite table (each written once by the
/// debugger), the sound processor's memory, the registers, the backgrounds'
/// layouts, the palette and the tiles in colour, the sprite table, the
/// sprite tile settings and the PPU's settings, the sprites as the picker
/// reads them, the layers (one hidden), single sprites as images, and runs
/// of tiles decoded, with the requests the PPU refuses.
#[wasm_bindgen_test]
fn what_the_ppu_readers_answer() {
    let mut found = Found::default();
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&capture_program_rom()).unwrap();
    for _ in 0..3 {
        emu.run_frame();
    }
    found.value("getVram", emu.get_vram());
    found.value("getPaletteMemory", emu.get_palette_memory());
    found.value("getOam", emu.get_oam());
    found.value("getAram", emu.get_aram());
    emu.write_palette_memory(3, &[0x7C00, 0x03E0]);
    let written = emu.get_palette_memory();
    found.value("getPaletteMemory, colours 3 and 4 written", written);
    emu.write_oam(4, &[100, 60]);
    found.value("getOam, sprite 1 moved", emu.get_oam());
    found.value("getVideoRegisters", emu.get_video_registers());
    for layer in [0, 1, 3, 7] {
        let layout = emu.get_layer_layout(layer);
        found.value(format!("getLayerLayout({layer})"), layout);
    }
    found.value("getPaletteColours", emu.get_palette_colours());
    for first_colour in [0, 128] {
        let tiles = emu.get_tile_colours(0, 0, 0, 256, 4, 0, first_colour);
        let what = format!("getTileColours(0, 0, 0, 256, 4, 0, {first_colour})");
        found.value(what, tiles);
    }
    found.value("getSpriteTable", emu.get_sprite_table());
    found.value("getSpriteTileSettings", emu.get_sprite_tile_settings());
    found.value("getPpuSettings", emu.get_ppu_settings());
    found.value("getSpriteData", emu.get_sprite_data());
    found.value("getColourLevels", emu.get_colour_levels());
    let order = emu.resolved_sprite_render_order();
    found.value("resolvedSpriteRenderOrder", order);
    found.value("getLayerInfo", emu.get_layer_info());
    emu.toggle_layer(0, false);
    found.value("getLayerInfo, BG1 hidden", emu.get_layer_info());
    emu.toggle_layer(0, true);
    found.value("getLayerInfo, shown again", emu.get_layer_info());
    for sprite in [0, 1, 127, 128] {
        let image = emu.export_sprite_image(sprite);
        found.value(format!("exportSpriteImage({sprite})"), image);
    }
    let requests = [
        (0, 0, 0, 2, 4, 0),
        (0, 0, 0, 2, 2, 0),
        (0, 0, 0, 1, 8, 0),
        (0, 0, 0, 1, 4, 1),
        (1, 0, 0, 2, 8, 0),
        (1, 0, 0, 1, 4, 0),
        (2, 0, 0, 1, 4, 0),
        (0, 0, 0, 1, 4, 2),
        (0, 0, 0, 0, 4, 0),
        (0, 0xFFE0, 0, 4, 4, 0),
    ];
    for (source, base, first, count, bpp, size) in requests {
        let sheet = emu.decode_tiles(source, base, first, count, bpp, size);
        let what = format!("decodeTiles({source}, {base:#X}, {first}, {count}, {bpp}, {size})");
        found.value(what, sheet);
    }
    found.check(
        r#"
getVram => Uint8Array(len 65536, fnv1a64 0xa0dc97483dd7b329)
getPaletteMemory => Uint16Array(len 256, fnv1a64 0xe0a864bf30509cc5)
getOam => Uint8Array(len 544, fnv1a64 0x2efc84e0437cba8f)
getAram => Uint8Array(len 65536, fnv1a64 0xeb05052ea5b62325)
getPaletteMemory, colours 3 and 4 written => Uint16Array(len 256, fnv1a64 0xbd185e8e6a49cf3a)
getOam, sprite 1 moved => Uint8Array(len 544, fnv1a64 0x9ea89672fa00393f)
getVideoRegisters => [{description: "BG mode + tile size", index: 8453, name: "BGMODE", value: 1}, {description: "Main sc... (882 chars, fnv1a64 0xfdb84b369804f8c8)
getLayerLayout(0) => {bits_per_pixel: 4, cell_height: 8, cell_width: 8, character_base: 0, first_colour: 0, id: 0, kind: "tile_map", map_base: 2048, map_height: 32, map_width: 32, scroll_x: 30, scroll_y: 0}
getLayerLayout(1) => {bits_per_pixel: 4, cell_height: 8, cell_width: 8, character_base: 0, first_colour: 0, id: 1, kind: "tile_map", map_base: 0, map_height: 32, map_width: 32, scroll_x: 0, scroll_y: 0}
getLayerLayout(3) => {bits_per_pixel: 0, cell_height: 8, cell_width: 8, character_base: 0, first_colour: 0, id: 3, kind: "off", map_base: 0, map_height: 32, map_width: 32, scroll_x: 0, scroll_y: 0}
getLayerLayout(7) => null
getPaletteColours => Uint8Array(len 1024, fnv1a64 0x5d511455216071f5)
getTileColours(0, 0, 0, 256, 4, 0, 0) => {data: Uint8Array(len 65536, fnv1a64 0x02921180a32eed05), height: 2048, width: 8}
getTileColours(0, 0, 0, 256, 4, 0, 128) => {data: Uint8Array(len 65536, fnv1a64 0xa39e25b8d8271f55), height: 2048, width: 8}
getSpriteTable => [{flip_h: false, flip_v: false, height: 8, index: 0, large: false, next: undefined, palette: 0, pict... (26651 chars, fnv1a64 0x9fe579ad64766e24)
getSpriteTileSettings => {first_table_base_word: 0, large_height: 16, large_width: 16, second_table_base_word: 4096, small_height: 8, small_width: 8}
getPpuSettings => {addsub_mode: false, bg3_priority: false, bg_main_enable: [true, false, false, false], bg_main_windo... (1786 chars, fnv1a64 0x9abb0ff1dbc4e28b)
getSpriteData => [{flip_h: false, flip_v: false, height: 8, index: 0, palette: 0, picture: {height: 8, width: 8, x: 3... (20506 chars, fnv1a64 0xfe9ed5eeadff4617)
getColourLevels => {levels: [0, 8, 16, 24, 33, 41, 49, 57, 66, 74, 82, 90, 99, 107, 115, 123, 132, 140, 148, 156, 165, 173, 181, 189, 198, 206, 214, 222, 231, 239, 247, 255], name: "snes-32"}
resolvedSpriteRenderOrder => [{index: 33, priority: 0}, {index: 32, priority: 0}, {index: 31, priority: 0}, {index: 30, priority:... (874 chars, fnv1a64 0x662689a95191546f)
getLayerInfo => [{enabled: true, height: 256, id: 0, name: "BG1", scroll_x: 30, scroll_y: 0, width: 256}, {enabled: ... (441 chars, fnv1a64 0xa6148a1925de5d23)
getLayerInfo, BG1 hidden => [{enabled: false, height: 256, id: 0, name: "BG1", scroll_x: 30, scroll_y: 0, width: 256}, {enabled:... (442 chars, fnv1a64 0xc4b8cb33035f14a4)
getLayerInfo, shown again => [{enabled: true, height: 256, id: 0, name: "BG1", scroll_x: 30, scroll_y: 0, width: 256}, {enabled: ... (441 chars, fnv1a64 0xa6148a1925de5d23)
exportSpriteImage(0) => {data: Uint8Array(len 256, fnv1a64 0x173f66a905ad5c05), height: 8, width: 8}
exportSpriteImage(1) => {data: Uint8Array(len 256, fnv1a64 0xa90f59b9611e8725), height: 8, width: 8}
exportSpriteImage(127) => {data: Uint8Array(len 256, fnv1a64 0xa90f59b9611e8725), height: 8, width: 8}
exportSpriteImage(128) => null
decodeTiles(0, 0x0, 0, 2, 4, 0) => {bpp: 4, height: 16, pixels: Uint8Array(len 128, fnv1a64 0xd76f35979b16e315), transparentIndex: 0, width: 8}
decodeTiles(0, 0x0, 0, 2, 2, 0) => {bpp: 2, height: 16, pixels: Uint8Array(len 128, fnv1a64 0x885b68e0d6d38ab5), transparentIndex: 0, width: 8}
decodeTiles(0, 0x0, 0, 1, 8, 0) => {bpp: 8, height: 8, pixels: Uint8Array(len 64, fnv1a64 0xa77a053e6d761895), transparentIndex: 0, width: 8}
decodeTiles(0, 0x0, 0, 1, 4, 1) => {bpp: 4, height: 16, pixels: Uint8Array(len 256, fnv1a64 0xa6597e424b581985), transparentIndex: 0, width: 16}
decodeTiles(1, 0x0, 0, 2, 8, 0) => {bpp: 8, height: 16, pixels: Uint8Array(len 128, fnv1a64 0xe083812a4199d056), transparentIndex: 0, width: 8}
decodeTiles(1, 0x0, 0, 1, 4, 0) => null
decodeTiles(2, 0x0, 0, 1, 4, 0) => null
decodeTiles(0, 0x0, 0, 1, 4, 2) => null
decodeTiles(0, 0x0, 0, 0, 4, 0) => null
decodeTiles(0, 0xFFE0, 0, 4, 4, 0) => {bpp: 4, height: 32, pixels: Uint8Array(len 256, fnv1a64 0xc82e328939af9d15), transparentIndex: 0, width: 8}
"#,
    );
}

/// A LoROM with 2 KiB of battery save RAM, whose program stores `$42` to
/// the first byte of it, then loops.
fn saving_lorom() -> Vec<u8> {
    // LDA #$42, STA $700000, BRA to itself
    let program = [0xA9, 0x42, 0x8F, 0x00, 0x00, 0x70, 0x80, 0xFE];
    let mut rom = lorom_running("SAVING", &program, &[]);
    rom[0x7FD6] = 0x02; // ROM, RAM and a battery
    rom[0x7FD8] = 0x01; // 2 KiB of RAM
    rom
}

/// The cartridge readers every cartridge answers, each named after `what`.
fn cartridge_readers(found: &mut Found, what: &str, emu: &mut RetroEmulator) {
    let info = emu.get_cartridge_info();
    found.outcome(format!("getCartridgeInfo, {what}"), info);
    let manifest = emu.get_cartridge_manifest();
    found.value(format!("getCartridgeManifest, {what}"), manifest);
    found.value(format!("getSaveRam, {what}"), emu.get_save_ram());
}

/// The cartridge's readers with no cartridge and on a cartridge with save
/// RAM whose program writes it; and the manifest detected, staged (a
/// malformed one and one of an unknown version refused) and cleared. A
/// staged manifest the load refuses is in the machine's test.
#[wasm_bindgen_test]
fn what_the_cartridge_readers_answer() {
    let mut found = Found::default();
    let mut emu = RetroEmulator::new("snes").unwrap();
    cartridge_readers(&mut found, "no cartridge", &mut emu);
    let rom = saving_lorom();
    let detected = emu.detect_manifest(&rom).unwrap();
    found.value("detectManifest", detected.clone());
    let refused = done(emu.set_cartridge_manifest("{ not json }"));
    found.outcome("setCartridgeManifest, not JSON", refused);
    let newer = concat!(
        r#"{"core":"snes1","version":"9.9","rom":{"sha256":"","size":0},"#,
        r#""mapping":{"preset":"lorom"}}"#
    );
    let refused = done(emu.set_cartridge_manifest(newer));
    found.outcome("setCartridgeManifest, version 9.9", refused);
    let staged = done(emu.set_cartridge_manifest(&detected));
    found.outcome("setCartridgeManifest", staged);
    emu.load_rom(&rom).unwrap();
    let manifest = emu.get_cartridge_manifest();
    found.value("getCartridgeManifest, staged", manifest);
    emu.clear_cartridge_manifest();
    emu.load_rom(&rom).unwrap();
    found.value("getDetectedManifest", emu.get_detected_manifest().unwrap());
    cartridge_readers(&mut found, "save RAM", &mut emu);
    emu.run_frame();
    found.value("getSaveRam, the program's byte", emu.get_save_ram());
    emu.set_save_ram(&[9, 8, 7, 6]);
    found.value("getSaveRam, set", emu.get_save_ram());
    found.check(
        r#"
getCartridgeInfo, no cartridge => throws "No cartridge loaded"
getCartridgeManifest, no cartridge => {mapperState: undefined, memoryMap: {regions: []}, romRegions: [], sramRegions: []}
getSaveRam, no cartridge => undefined
detectManifest => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"43c6444aeb2784a75c664403818fdde39d7d6273c9c6b7f9c8484d8a02875051\",\"size\":131072,\"title\":\"SAVING\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"lorom\",\"fastrom\":false,\"sram_size\":2048,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}"
setCartridgeManifest, not JSON => throws "invalid JSON: key must be a string at line 1 column 3"
setCartridgeManifest, version 9.9 => throws "unsupported manifest version: 9.9"
setCartridgeManifest => undefined
getCartridgeManifest, staged => {mapperState: {bank_config: {banks: [{bank_number: 0, cpu_address_end: 8257535, cpu_address_start: 3... (1231 chars, fnv1a64 0x86e60996c679db6f)
getDetectedManifest => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"43c6444aeb2784a75c664403818fdde39d7d6273c9c6b7f9c8484d8a02875051\",\"size\":131072,\"title\":\"SAVING\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"lorom\",\"fastrom\":false,\"sram_size\":2048,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}"
getCartridgeInfo, save RAM => {calculated_checksum: undefined, detection_source: "Header", expansion_chip_info: undefined, header_checksum: 4660, mapper_info: {description: "", features: [], mapper_id: "SnesLoROM", name: "LoROM"}, regions: [], rom_hash: "43c6444aeb2784a75c664403818fdde39d7d6273c9c6b7f9c8484d8a02875051", rom_size: 131072, serial: undefined, sram_size: 2048, sram_type: "Battery", system: "SNES", title: "SAVING"}
getCartridgeManifest, save RAM => {mapperState: {bank_config: {banks: [{bank_number: 0, cpu_address_end: 8257535, cpu_address_start: 3... (1231 chars, fnv1a64 0x86e60996c679db6f)
getSaveRam, save RAM => Uint8Array(len 2048, fnv1a64 0x28c31cf8df2ec325)
getSaveRam, the program's byte => Uint8Array(len 2048, fnv1a64 0x6c26daaf60470367)
getSaveRam, set => Uint8Array(len 2048, fnv1a64 0x0aa8857809824dfd)
"#,
    );
}

/// The cartridge's readers on a Super FX cartridge, in a build with the
/// chip.
#[cfg(feature = "chip-superfx")]
#[wasm_bindgen_test]
fn what_a_super_fx_cartridge_answers() {
    let mut found = Found::default();
    let mut emu = RetroEmulator::new("snes").unwrap();
    let rom = super_fx_rom();
    let detected = emu.detect_manifest(&rom).unwrap();
    found.value("detectManifest, Super FX", detected);
    emu.load_rom(&rom).unwrap();
    let detected = emu.get_detected_manifest().unwrap();
    found.value("getDetectedManifest, Super FX", detected);
    cartridge_readers(&mut found, "Super FX", &mut emu);
    found.check(
        r#"
detectManifest, Super FX => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"901c7b11acc4b77d4e5fd59c5e748e0eddeca3c9f0dcf27a325fae671e892a42\",\"size\":1048576,\"title\":\"SUPER FX TRACE\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"superfx\",\"fastrom\":false,\"sram_size\":32768,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}"
getDetectedManifest, Super FX => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"901c7b11acc4b77d4e5fd59c5e748e0eddeca3c9f0dcf27a325fae671e892a42\",\"size\":1048576,\"title\":\"SUPER FX TRACE\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"superfx\",\"fastrom\":false,\"sram_size\":32768,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}"
getCartridgeInfo, Super FX => {calculated_checksum: undefined, detection_source: "Header", expansion_chip_info: {chip_type: "Super... (535 chars, fnv1a64 0xd2e31d5d61f4c060)
getCartridgeManifest, Super FX => {mapperState: {bank_config: {banks: [{bank_number: 0, cpu_address_end: 4194303, cpu_address_start: 3... (3334 chars, fnv1a64 0xe739224af9143190)
getSaveRam, Super FX => undefined
"#,
    );
}

/// A system the crate does not emulate is refused with a text naming the
/// ones it does.
#[wasm_bindgen_test]
fn an_unknown_system_is_refused() {
    let mut found = Found::default();
    let refused = RetroEmulator::new("genesis").map(|_| JsValue::UNDEFINED);
    found.outcome("new RetroEmulator('genesis')", refused);
    found.check(
        r#"
new RetroEmulator('genesis') => throws "Unknown system: 'genesis'. Supported: snes, superfamicom, sfc"
"#,
    );
}
