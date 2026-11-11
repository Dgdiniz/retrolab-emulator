//! WebAssembly bindings for the SNES1 emulator.
//!
//! Exposes a [`RetroEmulator`] wrapper whose method names and signatures
//! mirror `sg1-wasm`'s `RetroEmulator` for every generic API (lifecycle,
//! framebuffer, audio, input, savestate, debugger, tracing, coverage,
//! disassembly, memory). A browser host (its renderer, its audio and
//! input code, and the generic debugger panels) can therefore drive the
//! SNES core through the same calls it already makes against Genesis.
//! SNES-only debug surfaces (PPU mode, Mode 7 matrix, S-DSP voices, APU
//! mailbox, viewers, cartridge manifest) ship under their own `js_name`s.
//!
//! ## Framebuffer presentation
//!
//! The core renders into a worst-case 512x240 [`Framebuffer`] with the
//! active image in a top-left sub-rectangle at the full `fb.width`
//! stride. The renderer uploads a `width x height` texture straight from
//! [`RetroEmulator::get_framebuffer_ptr`] / `get_framebuffer_len`, so it
//! needs a tightly-packed buffer where `len == width * height * 4`. We
//! pack the active rectangle into `present_buf` whenever the core
//! produces a new frame and expose that buffer (and the active
//! dimensions) through the framebuffer accessors.

use retro_core::cartridge::CartridgeDebugger;
use retro_core::input::{gamepad_map_from_text_keys, gamepad_map_with_text_keys};
use retro_core::*;
use snes1_core::{
    format_trace_line_65816, format_trace_line_spc700, parse_manifest, CoprocessorMode,
    SnesEmulator, TileSetSource, TraceEntryBinary65816, TraceEntryBinarySpc700,
    TRACE_ENTRY_SIZE_65816, TRACE_ENTRY_SIZE_CX4, TRACE_ENTRY_SIZE_GSU, TRACE_ENTRY_SIZE_SPC700,
};
#[cfg(feature = "chip-cx4")]
use snes1_core::{format_trace_line_cx4, TraceEntryBinaryCx4};
#[cfg(feature = "chip-superfx")]
use snes1_core::{format_trace_line_gsu, TraceEntryBinaryGsu};
use wasm_bindgen::prelude::*;

/// A `loadRom` refusal for a chip this build has no module for, as a JS
/// `Error` named `UnsupportedChip` whose `chip` property names the chip
/// (`"SA-1"`, `"Super FX"`, `"Cx4"`): what the platform's loader reads to
/// fall back to another core.
fn unsupported_chip_error(chip: &str, message: &str) -> JsValue {
    let err = js_sys::Error::new(message);
    err.set_name("UnsupportedChip");
    let _ = js_sys::Reflect::set(&err, &JsValue::from_str("chip"), &JsValue::from_str(chip));
    err.into()
}

/// A `loadState` refusal for JS. A savestate made on another ROM is an
/// `Error` named `SaveStateOtherRom`, what the platform reads to offer
/// loading it anyway (`loadStateAnyRom`); any other refusal is its reason
/// as text, as before.
fn load_state_refusal(error: LoadStateError) -> JsValue {
    match &error {
        LoadStateError::OtherRom { .. } => {
            let err = js_sys::Error::new(&error.to_string());
            err.set_name("SaveStateOtherRom");
            err.into()
        }
        LoadStateError::Unreadable(reason) => JsValue::from_str(reason),
    }
}

/// Install the panic-hook so Rust panics surface as readable browser
/// console messages instead of opaque `RuntimeError: unreachable`.
#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

/// Serialize a value with maps rendered as plain JS objects.
///
/// The default `serde_wasm_bindgen` serializer emits a Rust `HashMap` as an ES
/// `Map`, which a plain-object `x[key]` read on the JS side (e.g. the netplay
/// input folder) silently reads as `undefined`. The mapping getters here are
/// typed `Record<...>` on the JS side, so honour that contract.
///
/// Object keys must be text: a map with number keys is refused by the
/// serializer, so callers pass gamepad maps through
/// [`gamepad_map_with_text_keys`] first. A refusal returns `undefined`
/// instead of panicking, because a panic here leaves the whole emulator
/// refusing every later call.
fn map_as_object<T: serde::Serialize>(value: &T) -> JsValue {
    let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
    value.serialize(&serializer).unwrap_or(JsValue::UNDEFINED)
}

/// Read a gamepad button map sent from JS: a `Map` with number keys, or a
/// plain object, whose keys JavaScript always holds as text. `None` when it
/// is neither, so a malformed map never replaces a working one.
fn gamepad_map_from_js(value: JsValue) -> Option<std::collections::HashMap<u32, u32>> {
    if let Ok(map) = serde_wasm_bindgen::from_value(value.clone()) {
        return Some(map);
    }
    let map: std::collections::HashMap<String, u32> = serde_wasm_bindgen::from_value(value).ok()?;
    gamepad_map_from_text_keys(map)
}

/// Pack the active display rectangle out of the core's worst-case
/// 512x240 framebuffer (active image at the top-left, full `fb.width`
/// stride) into a tightly-packed RGBA buffer whose width equals the
/// active display width. The browser renderer uploads a
/// `width x height` texture directly from the packed buffer, so it
/// requires `len == width * height * 4` with no stride padding. Mirrors
/// the crop in the CLI `screenshot` command, keeping the alpha byte.
fn pack_active_rgba(fb: &Framebuffer, disp: &DisplayInfo, out: &mut Vec<u8>) -> (u32, u32) {
    let stride = fb.width as usize * 4;
    let backed_rows = fb.data.len().checked_div(stride).unwrap_or(0);
    let w = disp.width.min(fb.width);
    let h = disp.height.min(fb.height).min(backed_rows as u32);
    let row_bytes = w as usize * 4;
    out.clear();
    out.reserve(row_bytes * h as usize);
    for y in 0..h as usize {
        let base = y * stride;
        out.extend_from_slice(&fb.data[base..base + row_bytes]);
    }
    (w, h)
}

/// Browser-facing wrapper around [`SnesEmulator`].
#[wasm_bindgen]
pub struct RetroEmulator {
    snes: SnesEmulator,
    /// Tightly-packed active-rectangle RGBA, refreshed each time the core
    /// produces a frame. The framebuffer accessors return this buffer so
    /// the renderer never has to deal with the core's 512-wide stride.
    present_buf: Vec<u8>,
    present_w: u32,
    present_h: u32,
    /// Raw bytes of the last ROM passed to `loadRom`, retained so
    /// `getDetectedManifest` can re-run the heuristic detector for the
    /// running game without the host re-supplying the image. Empty
    /// before the first load.
    last_rom: Vec<u8>,
}

#[wasm_bindgen]
impl RetroEmulator {
    /// Build a new SNES emulator for the given system alias.
    ///
    /// Accepts `"snes"`, `"superfamicom"`, and `"sfc"`; any other value
    /// returns a JS `Error` so the host can fall back gracefully.
    #[wasm_bindgen(constructor)]
    pub fn new(system: &str) -> Result<RetroEmulator, JsValue> {
        match system {
            "snes" | "superfamicom" | "sfc" => {
                let mut emu = RetroEmulator {
                    snes: SnesEmulator::new(),
                    present_buf: Vec::new(),
                    present_w: 0,
                    present_h: 0,
                    last_rom: Vec::new(),
                };
                emu.refresh_present();
                Ok(emu)
            }
            other => Err(JsValue::from_str(&format!(
                "Unknown system: '{}'. Supported: snes, superfamicom, sfc",
                other
            ))),
        }
    }

    /// Repack the active framebuffer rectangle into `present_buf`.
    fn refresh_present(&mut self) {
        let (w, h) = pack_active_rgba(
            self.snes.get_framebuffer(),
            self.snes.get_display_info(),
            &mut self.present_buf,
        );
        self.present_w = w;
        self.present_h = h;
    }

    /// Get system information and metadata.
    #[wasm_bindgen(js_name = getSystemInfo)]
    pub fn get_system_info(&self) -> JsValue {
        serde_wasm_bindgen::to_value(self.snes.system_info()).unwrap()
    }

    /// The debug profile id the frontend should resolve for the loaded
    /// cartridge, keyed off the architecture of the steppable secondary CPU
    /// (debugger slot 2): `"snes-superfx"` for the Super FX GSU, `"snes-cx4"`
    /// for the Cx4 HG51B169, `"snes-sa1"` for the SA-1's second 65816, else
    /// `"snes"` (plain cart or a non-steppable coprocessor). The core owns
    /// per-cartridge profile selection, it reflects the CPU roster built at
    /// `loadRom` from the installed chip, so the frontend never re-detects the
    /// chip. Defaults to `"snes"` before a ROM is loaded.
    #[wasm_bindgen(js_name = getDebugProfileId)]
    pub fn get_debug_profile_id(&self) -> String {
        match self.snes.system_info().cpus.get(2).map(|c| c.architecture) {
            Some(retro_core::CpuArchitecture::SuperFx) => "snes-superfx".to_string(),
            Some(retro_core::CpuArchitecture::Hg51b) => "snes-cx4".to_string(),
            Some(retro_core::CpuArchitecture::Mos65816) => "snes-sa1".to_string(),
            _ => "snes".to_string(),
        }
    }

    /// Reset the emulator. Cartridge ROM and SRAM are preserved.
    pub fn reset(&mut self) {
        self.snes.reset();
        self.refresh_present();
    }

    /// Power-on (cold) reset: a power cycle, not the reset button. Recreates the
    /// CPU fresh and reloads the ROM, clearing every volatile subsystem a soft
    /// reset leaves dirty (the 128 KiB WRAM, the 65816 registers, the PPU's
    /// VRAM/CGRAM/OAM, and the APU). Netplay uses this so two peers derive a
    /// byte-identical frame-0 state hash regardless of how long each ran the game
    /// before the session. Cartridge ROM is preserved; SRAM is a fresh image (a
    /// save resumes via the seeded savestate transfer).
    #[wasm_bindgen(js_name = powerOnReset)]
    pub fn power_on_reset(&mut self) {
        self.snes.power_on_reset();
        self.refresh_present();
    }

    /// Stage a coprocessor firmware dump (DSP-1 family: the 8 KB
    /// `dsp1b.bin` / `dsp1b.rom`) before [`load_rom`](Self::load_rom).
    /// Byte order and chip revision are auto-detected. With firmware
    /// staged a detected DSP-1 cart runs cycle-accurate LLE (the `nec-lle`
    /// feature, on in this build; a build without it keeps the dump but
    /// ignores it, and the firmware-free HLE renders the Mode-7 track).
    /// The backend is chosen when the cartridge
    /// is built, so this MUST be called before `loadRom` (calling it
    /// after has no effect until the next load).
    #[wasm_bindgen(js_name = setCoprocessorFirmware)]
    pub fn set_coprocessor_firmware(&mut self, firmware: &[u8]) {
        self.snes.set_coprocessor_firmware(Some(firmware.to_vec()));
    }

    /// Choose how a detected coprocessor is emulated on the next
    /// [`load_rom`](Self::load_rom): `"auto"` (LLE when firmware is
    /// staged and built in, otherwise HLE), `"lle"` (force low-level; a
    /// non-hanging idle stub if firmware is missing/invalid, or, in a
    /// build without `nec-lle`, one whose label says the LLE is not
    /// built), or `"hle"` (force the firmware-free HLE).
    /// Unrecognised values leave the default `auto` in place. MUST be
    /// called before `loadRom`.
    #[wasm_bindgen(js_name = setCoprocessorMode)]
    pub fn set_coprocessor_mode(&mut self, mode: &str) {
        let mode = match mode.to_ascii_lowercase().as_str() {
            "lle" => CoprocessorMode::Lle,
            "hle" => CoprocessorMode::Hle,
            _ => CoprocessorMode::Auto,
        };
        self.snes.set_coprocessor_mode(mode);
    }

    /// Load a ROM file.
    ///
    /// Any cartridge manifest staged via `setCartridgeManifest` is
    /// applied here (overriding heuristic detection). The raw bytes are
    /// cached so `getDetectedManifest` can report the detected mapping
    /// for the running game.
    ///
    /// When no manifest is staged and auto-detection rejects the ROM
    /// (a low-confidence header, such as a blank title or a bad checksum,
    /// that scores below the detector's confidence floor), the load retries
    /// once with the vector-implied best-guess mapping instead of
    /// failing. This is the automatic equivalent of the CLI's
    /// `--force-detect`: small homebrew / first-party diagnostic ROMs
    /// boot straight away rather than dead-ending on a
    /// `"no mapping: detector score below threshold"` console error
    /// that the user can only clear by hand-editing a manifest. A
    /// user-staged manifest is always authoritative, its failure is
    /// surfaced verbatim, never papered over.
    #[wasm_bindgen(js_name = loadRom)]
    pub fn load_rom(&mut self, data: &[u8]) -> Result<(), JsValue> {
        if let Err(err) = self.snes.load_rom(data) {
            // A chip this build has no module for: a typed error, so the
            // platform can run the game on another core. No retry, the
            // detector's mapping would need the same chip.
            if let Some(chip) = self.snes.last_unsupported_chip() {
                return Err(unsupported_chip_error(chip, &err));
            }
            // A user-staged manifest is authoritative: surface its
            // failure so the editor can show what's wrong.
            if self.snes.cartridge_manifest_override().is_some() {
                return Err(JsValue::from_str(&err));
            }
            // Otherwise retry with the detector's threshold-free
            // best-guess mapping. The override is transient, cleared
            // immediately so it never masquerades as a user edit in
            // `getCartridgeManifest` / a later reload.
            let fallback = self.snes.detected_manifest(data);
            self.snes.set_cartridge_manifest_override(Some(fallback));
            let retry = self.snes.load_rom(data);
            self.snes.set_cartridge_manifest_override(None);
            retry.map_err(|e| JsValue::from_str(&e))?;
        }
        self.last_rom = data.to_vec();
        self.refresh_present();
        Ok(())
    }

    /// Get ROM information (`null` until a ROM is loaded).
    #[wasm_bindgen(js_name = getRomInfo)]
    pub fn get_rom_info(&self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.get_rom_info()).unwrap()
    }

    /// Get cartridge information (title, region, mapper, checksum).
    #[wasm_bindgen(js_name = getCartridgeInfo)]
    pub fn get_cartridge_info(&self) -> Result<JsValue, JsValue> {
        if let Some(info) = self.snes.get_rom_info() {
            Ok(serde_wasm_bindgen::to_value(&info)?)
        } else {
            Err(JsValue::from_str("No cartridge loaded"))
        }
    }

    /// Step one 65C816 instruction.
    pub fn step(&mut self) -> JsValue {
        let result = serde_wasm_bindgen::to_value(&self.snes.step()).unwrap();
        self.refresh_present();
        result
    }

    /// Run one frame. Tier selection (fast / light-debug / full-debug)
    /// is internal to the core based on active debug features.
    #[wasm_bindgen(js_name = runFrame)]
    pub fn run_frame(&mut self) {
        self.snes.run_frame();
        self.refresh_present();
    }

    /// Run until VBLANK.
    #[wasm_bindgen(js_name = runUntilVBlank)]
    pub fn run_until_vblank(&mut self) {
        self.snes.run_until_vblank();
        self.refresh_present();
    }

    // ===== Netplay determinism primitives =====

    /// Run one frame headlessly: the full deterministic simulation with
    /// the framebuffer post-process and host-audio drain skipped. Used
    /// for netplay rollback/catch-up, so it deliberately does NOT call
    /// `refresh_present()`: the packed framebuffer is not needed for a
    /// re-simulated frame and skipping the repack is the whole point.
    #[wasm_bindgen(js_name = runFrameHeadless)]
    pub fn run_frame_headless(&mut self) {
        self.snes.run_frame_headless();
    }

    /// Canonical FNV-1a-64 hash of the full deterministic machine state.
    /// Two peers in the same state hash to the same value; a mismatch is
    /// a desync. `u64` in Rust maps to `bigint` in JS.
    #[wasm_bindgen(js_name = getStateHash)]
    pub fn get_state_hash(&self) -> u64 {
        self.snes.state_hash()
    }

    /// Per-subsystem hashes of the canonical savestate, as a JSON object of hex
    /// strings (`{"cpu":"..","wram":"..","sram":"..",...}`), for netplay desync
    /// triage: when two peers disagree on [`Self::get_state_hash`] this pinpoints
    /// which subsystem's serialized state diverged (e.g. `wram` vs `sram` at a
    /// refused committed start). Diagnostic only.
    #[wasm_bindgen(js_name = getStateHashParts)]
    pub fn get_state_hash_parts(&self) -> String {
        let mut json = String::from("{");
        for (i, (name, hash)) in self.snes.state_hash_parts().iter().enumerate() {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&format!("\"{name}\":\"{hash:x}\""));
        }
        json.push('}');
        json
    }

    /// Authoritative frame counter from the core (the serialized
    /// `frame_count`). `u64` in Rust maps to `bigint` in JS.
    #[wasm_bindgen(js_name = getFrameNumber)]
    pub fn get_frame_number(&self) -> u64 {
        self.snes.frame_count()
    }

    /// Set the video region (`"ntsc"` / `"pal"`). Call right after
    /// `loadRom`, before running frames: it resets the machine, and
    /// both netplay peers agree on region in the handshake. Errors on an
    /// unknown region string.
    #[wasm_bindgen(js_name = setRegion)]
    pub fn set_region(&mut self, region: &str) -> Result<(), JsValue> {
        self.snes
            .set_region_str(region)
            .map_err(|e| JsValue::from_str(&e))?;
        self.refresh_present();
        Ok(())
    }

    /// Current video region as `"ntsc"` / `"pal"`.
    #[wasm_bindgen(js_name = getRegion)]
    pub fn get_region(&self) -> String {
        self.snes.region_str().to_string()
    }

    /// Core build identity for the netplay handshake, e.g.
    /// `"snes1@0.1.0+ab12cd3"`. Peers refuse to connect on a mismatch,
    /// since a different build can diverge deterministically.
    #[wasm_bindgen(js_name = getBuildId)]
    pub fn get_build_id(&self) -> String {
        format!(
            "snes1@{}+{}",
            env!("CARGO_PKG_VERSION"),
            option_env!("GIT_HASH").unwrap_or("unknown")
        )
    }

    // ===== Framebuffer =====

    /// Get the full worst-case framebuffer as a JS object (SLOW -- uses
    /// serde, kept for parity with SG1). Prefer the packed
    /// `getFramebufferData`/`Ptr` accessors for rendering.
    #[wasm_bindgen(js_name = getFramebuffer)]
    pub fn get_framebuffer(&self) -> JsValue {
        serde_wasm_bindgen::to_value(self.snes.get_framebuffer()).unwrap()
    }

    /// Get the packed active-rectangle RGBA pixels as a
    /// `Uint8ClampedArray`, ready for `ImageData`.
    #[wasm_bindgen(js_name = getFramebufferData)]
    pub fn get_framebuffer_data(&self) -> js_sys::Uint8ClampedArray {
        let arr = js_sys::Uint8ClampedArray::new_with_length(self.present_buf.len() as u32);
        arr.copy_from(&self.present_buf);
        arr
    }

    /// Active framebuffer width in pixels (the packed buffer's width).
    #[wasm_bindgen(js_name = getFramebufferWidth)]
    pub fn get_framebuffer_width(&self) -> u32 {
        self.present_w
    }

    /// Active framebuffer height in pixels.
    #[wasm_bindgen(js_name = getFramebufferHeight)]
    pub fn get_framebuffer_height(&self) -> u32 {
        self.present_h
    }

    /// Pointer to the packed RGBA buffer in WASM linear memory
    /// (ZERO-COPY). Use with `getFramebufferLen()` to build a live
    /// `Uint8Array` view: `new Uint8Array(memory.buffer, ptr, len)`. The
    /// view must be recreated if WASM memory grows.
    #[wasm_bindgen(js_name = getFramebufferPtr)]
    pub fn get_framebuffer_ptr(&self) -> *const u8 {
        self.present_buf.as_ptr()
    }

    /// Length in bytes of the packed RGBA buffer (`width * height * 4`).
    #[wasm_bindgen(js_name = getFramebufferLen)]
    pub fn get_framebuffer_len(&self) -> u32 {
        self.present_buf.len() as u32
    }

    /// Get display info (active resolution, aspect, refresh, format).
    #[wasm_bindgen(js_name = getDisplayInfo)]
    pub fn get_display_info(&self) -> JsValue {
        serde_wasm_bindgen::to_value(self.snes.get_display_info()).unwrap()
    }

    /// Host display preference: when `false` (default) overscan frames
    /// are cropped to the 224-line NTSC TV window (matching bsnes-plus /
    /// snes9x); when `true` the full 240-line frame is shown. Applies
    /// immediately and survives reset / savestate restore.
    #[wasm_bindgen(js_name = setShowOverscan)]
    pub fn set_show_overscan(&mut self, show: bool) {
        self.snes.set_show_overscan(show);
    }

    /// Whether the full overscan frame is exposed (vs cropped to 224).
    #[wasm_bindgen(js_name = isShowOverscanEnabled)]
    pub fn is_show_overscan_enabled(&self) -> bool {
        self.snes.show_overscan_enabled()
    }

    // ===== Audio =====

    /// Drain **every queued sample** from the host audio ring into a
    /// fresh `Float32Array` (interleaved stereo). Destructive: each call
    /// empties the ring. For a fixed host lookahead prefer
    /// `getAudioSamplesN(n)`.
    #[wasm_bindgen(js_name = getAudioSamples)]
    pub fn get_audio_samples(&mut self) -> Vec<f32> {
        self.snes.get_audio_samples().to_vec()
    }

    /// Drain **at most `n`** queued samples from the host ring. Returns a
    /// `Float32Array` of length `min(n, getAudioAvailable())`.
    #[wasm_bindgen(js_name = getAudioSamplesN)]
    pub fn get_audio_samples_n(&mut self, n: usize) -> Vec<f32> {
        let take = n.min(self.snes.audio_available());
        let mut out = vec![0.0_f32; take];
        let written = self.snes.drain_audio_into(&mut out);
        out.truncate(written);
        out
    }

    /// Number of samples currently queued in the host ring (stereo
    /// interleaved: `1470` means 735 stereo pairs ready).
    #[wasm_bindgen(js_name = getAudioAvailable)]
    pub fn get_audio_available(&self) -> usize {
        self.snes.audio_available()
    }

    /// Cumulative count of samples dropped from the front because
    /// incoming data wouldn't fit (consumer behind).
    ///
    /// There is no underrun counterpart, on purpose. The ring can only
    /// record an underrun when a drain asks for more than it holds, and
    /// every path out of here sizes its request from `audio_available()`
    /// first, so such a counter could never move, and the host would read
    /// a constant zero as though it were a measurement.
    #[wasm_bindgen(js_name = getAudioRingOverflows)]
    pub fn get_audio_ring_overflows(&self) -> u64 {
        self.snes.audio_overflows()
    }

    /// **Deprecated, intentionally a no-op.** The `getAudioSamples`
    /// family drains destructively, so the host never needs to clear the
    /// ring. Kept exported for parity with SG1; do not call from new code.
    #[wasm_bindgen(js_name = clearAudioBuffer)]
    pub fn clear_audio_buffer(&mut self) {}

    /// Drop audio generated but not yet handed to the host.
    ///
    /// Distinct from `clearAudioBuffer`, which drops everything already
    /// queued *for* the host: that would discard audio the player is about
    /// to hear. This drops only what a headless frame left waiting, which
    /// after a rollback re-simulation is audio already heard in its
    /// mispredicted form. Safe mid-session, it touches no serialized state,
    /// so it cannot move the netplay desync hash.
    #[wasm_bindgen(js_name = discardPendingAudio)]
    pub fn discard_pending_audio(&mut self) {
        self.snes.discard_pending_audio();
    }

    /// Get audio system info (sample rate, channels, bits).
    #[wasm_bindgen(js_name = getAudioInfo)]
    pub fn get_audio_info(&self) -> JsValue {
        serde_wasm_bindgen::to_value(self.snes.get_audio_info()).unwrap()
    }

    /// Enable or disable audio generation (default: disabled).
    #[wasm_bindgen(js_name = setAudioEnabled)]
    pub fn set_audio_enabled(&mut self, enabled: bool) {
        self.snes.set_audio_enabled(enabled);
    }

    /// Check if audio generation is enabled.
    #[wasm_bindgen(js_name = isAudioEnabled)]
    pub fn is_audio_enabled(&self) -> bool {
        self.snes.is_audio_enabled()
    }

    // ===== Input =====

    /// Set the button bitmask for a controller port directly.
    #[wasm_bindgen(js_name = setInput)]
    pub fn set_input(&mut self, player: u8, buttons: u32) {
        self.snes.set_input(player, InputState { buttons });
    }

    /// Keyboard key down event.
    #[wasm_bindgen(js_name = keyDown)]
    pub fn key_down(&mut self, keycode: String) {
        self.snes.input_manager_mut().update_keyboard(keycode, true);
    }

    /// Keyboard key up event.
    #[wasm_bindgen(js_name = keyUp)]
    pub fn key_up(&mut self, keycode: String) {
        self.snes
            .input_manager_mut()
            .update_keyboard(keycode, false);
    }

    /// Gamepad button event.
    #[wasm_bindgen(js_name = gamepadButton)]
    pub fn gamepad_button(&mut self, gamepad_id: String, button: u32, pressed: bool) {
        self.snes
            .input_manager_mut()
            .update_gamepad_button(gamepad_id, button, pressed);
    }

    /// Gamepad axis event.
    #[wasm_bindgen(js_name = gamepadAxis)]
    pub fn gamepad_axis(&mut self, gamepad_id: String, axis: u32, value: f32) {
        self.snes
            .input_manager_mut()
            .update_gamepad_axis(gamepad_id, axis, value);
    }

    /// Get all registered input providers.
    #[wasm_bindgen(js_name = getInputProviders)]
    pub fn get_input_providers(&self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.input_manager().get_providers()).unwrap()
    }

    /// Get the provider assigned to a controller port.
    #[wasm_bindgen(js_name = getPortAssignment)]
    pub fn get_port_assignment(&self, port: u32) -> JsValue {
        let assignment = self.snes.input_manager().get_port_assignment(port as usize);
        serde_wasm_bindgen::to_value(&assignment).unwrap()
    }

    /// Get input history (last N snapshots).
    #[wasm_bindgen(js_name = getInputHistory)]
    pub fn get_input_history(&self, count: u32) -> JsValue {
        let history = self.snes.input_manager().get_history(count as usize);
        serde_wasm_bindgen::to_value(&history).unwrap()
    }

    /// Get current state of a port (button bitmask).
    #[wasm_bindgen(js_name = getPortState)]
    pub fn get_port_state(&self, port: u32) -> u32 {
        self.snes.input_manager().get_port_state(port as usize)
    }

    /// Make an input provider the only one on a controller port (an empty
    /// id clears the port). [`set_provider_port`](Self::set_provider_port)
    /// moves a provider and keeps the others.
    #[wasm_bindgen(js_name = assignProviderToPort)]
    pub fn assign_provider_to_port(&mut self, provider_id: String, port: u32) {
        self.snes
            .input_manager_mut()
            .assign_provider_to_port(provider_id, port as usize);
    }

    /// Put an input provider on a controller port, beside whatever else
    /// feeds it, after taking it off every other port. A negative `port`
    /// takes it off every port, so its input reaches no player.
    #[wasm_bindgen(js_name = setProviderPort)]
    pub fn set_provider_port(&mut self, provider_id: String, port: i32) {
        let port = usize::try_from(port).ok();
        self.snes
            .input_manager_mut()
            .set_provider_port(&provider_id, port);
    }

    /// The controller port an input provider feeds, or -1 when none.
    #[wasm_bindgen(js_name = getProviderPort)]
    pub fn get_provider_port(&self, provider_id: String) -> i32 {
        self.snes
            .input_manager()
            .provider_port(&provider_id)
            .map_or(-1, |port| port as i32)
    }

    /// Every input provider feeding a controller port (`string[]`).
    #[wasm_bindgen(js_name = getPortProviders)]
    pub fn get_port_providers(&self, port: u32) -> JsValue {
        let providers = self.snes.input_manager().port_providers(port as usize);
        serde_wasm_bindgen::to_value(providers).unwrap_or(JsValue::NULL)
    }

    /// Register a gamepad the core has not seen yet, with the core's
    /// default button map, feeding no player. Returns whether it was new.
    #[wasm_bindgen(js_name = addGamepad)]
    pub fn add_gamepad(&mut self, gamepad_id: String) -> bool {
        self.snes.input_manager_mut().add_gamepad(&gamepad_id)
    }

    /// Exchange everything feeding port 0 with everything feeding port 1.
    #[wasm_bindgen(js_name = swapPortAssignments)]
    pub fn swap_port_assignments(&mut self) {
        self.snes.input_manager_mut().swap_ports(0, 1);
    }

    /// Set keyboard button mapping: `{ "KeyA": 6, "ArrowUp": 4, ... }`.
    /// Returns `false`, keeping the current mapping, when `mapping` is not
    /// such an object.
    #[wasm_bindgen(js_name = setKeyboardMapping)]
    pub fn set_keyboard_mapping(&mut self, mapping: JsValue) -> bool {
        let Ok(map) = serde_wasm_bindgen::from_value(mapping) else {
            return false;
        };
        self.snes.input_manager_mut().set_keyboard_mapping(map);
        true
    }

    /// Get current keyboard button mapping.
    #[wasm_bindgen(js_name = getKeyboardMapping)]
    pub fn get_keyboard_mapping(&self) -> JsValue {
        map_as_object(&self.snes.input_manager().get_keyboard_mapping())
    }

    /// Clear all keyboard key state (release all keys).
    #[wasm_bindgen(js_name = clearKeyboardState)]
    pub fn clear_keyboard_state(&mut self) {
        self.snes.input_manager_mut().clear_keyboard_state();
    }

    /// Set gamepad button mapping (`{ gamepadButtonIdx: snesButtonIdx }`),
    /// whose keys JavaScript holds as text, or a `Map` with number keys.
    /// Returns `false`, keeping the pad's current map, when `mapping` is
    /// neither.
    #[wasm_bindgen(js_name = setGamepadMapping)]
    pub fn set_gamepad_mapping(&mut self, gamepad_id: String, mapping: JsValue) -> bool {
        let Some(map) = gamepad_map_from_js(mapping) else {
            return false;
        };
        self.snes
            .input_manager_mut()
            .set_gamepad_mapping(&gamepad_id, map);
        true
    }

    /// Get current gamepad button mapping, as a plain object keyed by the
    /// button index written as text (`{ "0": 5, ... }`).
    #[wasm_bindgen(js_name = getGamepadMapping)]
    pub fn get_gamepad_mapping(&self, gamepad_id: String) -> JsValue {
        let map = self.snes.input_manager().get_gamepad_mapping(&gamepad_id);
        map_as_object(&gamepad_map_with_text_keys(&map))
    }

    /// Set gamepad deadzone (0.0 to 1.0).
    #[wasm_bindgen(js_name = setGamepadDeadzone)]
    pub fn set_gamepad_deadzone(&mut self, gamepad_id: String, deadzone: f32) {
        self.snes
            .input_manager_mut()
            .set_gamepad_deadzone(&gamepad_id, deadzone);
    }

    /// Get gamepad deadzone.
    #[wasm_bindgen(js_name = getGamepadDeadzone)]
    pub fn get_gamepad_deadzone(&self, gamepad_id: String) -> f32 {
        self.snes.input_manager().get_gamepad_deadzone(&gamepad_id)
    }

    /// Clear all gamepad button/axis state (release everything).
    #[wasm_bindgen(js_name = clearGamepadState)]
    pub fn clear_gamepad_state(&mut self, gamepad_id: String) {
        self.snes
            .input_manager_mut()
            .clear_gamepad_state(&gamepad_id);
    }

    /// Enable split keyboard for local 2-player. Returns `false`, changing
    /// nothing, when either mapping is not a `{ "KeyA": 6, ... }` object.
    #[wasm_bindgen(js_name = enableSplitKeyboard)]
    pub fn enable_split_keyboard(&mut self, mapping_p1: JsValue, mapping_p2: JsValue) -> bool {
        let (Ok(map_p1), Ok(map_p2)) = (
            serde_wasm_bindgen::from_value(mapping_p1),
            serde_wasm_bindgen::from_value(mapping_p2),
        ) else {
            return false;
        };
        self.snes
            .input_manager_mut()
            .enable_split_keyboard(map_p1, map_p2);
        true
    }

    /// Disable split keyboard, revert to single keyboard.
    #[wasm_bindgen(js_name = disableSplitKeyboard)]
    pub fn disable_split_keyboard(&mut self) {
        self.snes.input_manager_mut().disable_split_keyboard();
    }

    /// Check if split keyboard is enabled.
    #[wasm_bindgen(js_name = isSplitKeyboardEnabled)]
    pub fn is_split_keyboard_enabled(&self) -> bool {
        self.snes.input_manager().is_split_keyboard_enabled()
    }

    // ===== Save state / SRAM =====

    /// Save emulator state (zstd-compressed envelope).
    #[wasm_bindgen(js_name = saveState)]
    pub fn save_state(&self) -> Vec<u8> {
        self.snes.save_state()
    }

    /// Load emulator state. A savestate made on another ROM is refused
    /// with an `Error` named `SaveStateOtherRom`.
    #[wasm_bindgen(js_name = loadState)]
    pub fn load_state(&mut self, data: &[u8]) -> Result<(), JsValue> {
        self.load_state_checking_rom(data, RomCheck::SameRom)
    }

    /// Load a savestate whatever ROM it was made on: for a player who chose
    /// to carry a state to another build of the game.
    #[wasm_bindgen(js_name = loadStateAnyRom)]
    pub fn load_state_any_rom(&mut self, data: &[u8]) -> Result<(), JsValue> {
        self.load_state_checking_rom(data, RomCheck::AnyRom)
    }

    fn load_state_checking_rom(&mut self, data: &[u8], check: RomCheck) -> Result<(), JsValue> {
        self.snes
            .load_state_with_rom_check(data, check)
            .map_err(load_state_refusal)?;
        self.refresh_present();
        Ok(())
    }

    /// Get save RAM (`null` if the cartridge has none).
    #[wasm_bindgen(js_name = getSaveRam)]
    pub fn get_save_ram(&self) -> Option<Vec<u8>> {
        self.snes.get_save_ram().map(<[u8]>::to_vec)
    }

    /// Set save RAM.
    #[wasm_bindgen(js_name = setSaveRam)]
    pub fn set_save_ram(&mut self, data: &[u8]) {
        self.snes.set_save_ram(data);
    }

    // ===== Debugger =====

    /// Get CPU states for both CPUs (65C816 = 0, SPC700 = 1).
    #[wasm_bindgen(js_name = getCpuStates)]
    pub fn get_cpu_states(&mut self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.debugger().get_cpu_states()).unwrap()
    }

    /// Add a breakpoint. `bp_type` is one of
    /// `"execute" | "read" | "write" | "readwrite"`; `cpu_id` selects the
    /// CPU (65C816 = 0, SPC700 = 1) and defaults to 0 when omitted.
    #[wasm_bindgen(js_name = addBreakpoint)]
    pub fn add_breakpoint(&mut self, address: u32, bp_type: String, cpu_id: Option<u32>) -> u32 {
        let bp_type = match bp_type.as_str() {
            "read" => BreakpointType::read(),
            "write" => BreakpointType::write(),
            "readwrite" => BreakpointType::read_write(),
            _ => BreakpointType::execute(),
        };
        let bp = Breakpoint {
            address,
            bp_type,
            cpu_id: Some(cpu_id.unwrap_or(0) as usize),
            condition: None,
        };
        self.snes.debugger().add_breakpoint(bp)
    }

    /// Remove a breakpoint.
    #[wasm_bindgen(js_name = removeBreakpoint)]
    pub fn remove_breakpoint(&mut self, id: u32) {
        self.snes.debugger().remove_breakpoint(id);
    }

    /// Enable or disable a breakpoint.
    #[wasm_bindgen(js_name = enableBreakpoint)]
    pub fn enable_breakpoint(&mut self, id: u32, enabled: bool) {
        self.snes.debugger().enable_breakpoint(id, enabled);
    }

    /// Replace the access flags (exec/read/write) of a breakpoint in
    /// place, preserving its hit count. Backs the per-row R/W/X
    /// checkboxes in the debugger UI.
    #[wasm_bindgen(js_name = setBreakpointAccess)]
    pub fn set_breakpoint_access(&mut self, id: u32, exec: bool, read: bool, write: bool) {
        self.snes
            .debugger()
            .set_breakpoint_access(id, AccessFlags::new(exec, read, write));
    }

    /// Replace the advanced match conditions (address range, value
    /// predicate, hit-skip, action, one-shot) of a breakpoint. `spec` is
    /// a plain object whose fields mirror `MatchSpec`; every field is
    /// optional and defaults to the bare-breakpoint behaviour. No-op on
    /// an unknown id; errors only on a malformed object.
    #[wasm_bindgen(js_name = setBreakpointMatchSpec)]
    pub fn set_breakpoint_match_spec(&mut self, id: u32, spec: JsValue) -> Result<(), JsValue> {
        let spec: MatchSpec =
            serde_wasm_bindgen::from_value(spec).map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.snes.debugger().set_breakpoint_match_spec(id, spec);
        Ok(())
    }

    /// Catalog of event classes this core can break on (VBlank, NMI, IRQ,
    /// DMA/HDMA start, scanline, PPU/APU/DSP register writes), for the
    /// event breakpoint picker.
    #[wasm_bindgen(js_name = getAvailableEvents)]
    pub fn get_available_events(&mut self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.debugger().available_events()).unwrap()
    }

    /// Add an event breakpoint for one of the codes from
    /// [`Self::get_available_events`]. `param` discriminates within a
    /// class (scanline, register, port); pass `0xFFFF_FFFF`
    /// (`BREAKPOINT_PARAM_ANY`) to match any. Returns the breakpoint id.
    #[wasm_bindgen(js_name = addEventBreakpoint)]
    pub fn add_event_breakpoint(&mut self, code: u32, param: u32) -> u32 {
        self.snes.debugger().add_breakpoint(Breakpoint {
            address: 0,
            bp_type: BreakpointType::Event { code, param },
            cpu_id: None,
            condition: None,
        })
    }

    /// List all breakpoints.
    #[wasm_bindgen(js_name = listBreakpoints)]
    pub fn list_breakpoints(&mut self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.debugger().list_breakpoints()).unwrap()
    }

    /// Get execution mode.
    #[wasm_bindgen(js_name = getExecutionMode)]
    pub fn get_execution_mode(&mut self) -> String {
        format!("{:?}", self.snes.debugger().get_execution_mode())
    }

    /// Set execution mode
    /// (`Running | Paused | StepInto | StepOver | StepOut`).
    #[wasm_bindgen(js_name = setExecutionMode)]
    pub fn set_execution_mode(&mut self, mode: String) {
        let exec_mode = match mode.as_str() {
            "Running" => ExecutionMode::Running,
            "Paused" => ExecutionMode::Paused,
            "StepInto" => ExecutionMode::StepInto,
            "StepOver" => ExecutionMode::StepOver,
            "StepOut" => ExecutionMode::StepOut,
            _ => ExecutionMode::Running,
        };
        self.snes.debugger().set_execution_mode(exec_mode);
    }

    /// Get memory regions (WRAM / VRAM / OAM / CGRAM / ARAM / ROM / SRAM).
    #[wasm_bindgen(js_name = getMemoryRegions)]
    pub fn get_memory_regions(&mut self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.debugger().get_memory_regions()).unwrap()
    }

    /// Get instruction history for a CPU (cpu 0 only).
    #[wasm_bindgen(js_name = getInstructionHistory)]
    pub fn get_instruction_history(&mut self, cpu_id: usize, count: u32) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.debugger().get_instruction_history(cpu_id, count))
            .unwrap()
    }

    /// Get call stack for a CPU (cpu 0 only).
    #[wasm_bindgen(js_name = getCallStack)]
    pub fn get_call_stack(&mut self, cpu_id: usize) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.debugger().get_call_stack(cpu_id)).unwrap()
    }

    /// Read memory for debugging (drives the live 65C816 bus).
    #[wasm_bindgen(js_name = readMemory)]
    pub fn read_memory(&mut self, addr: u32, size: u32) -> Vec<u8> {
        self.snes.read_memory_debug(addr, size)
    }

    /// Write memory for debugging (drives the live 65C816 bus; register
    /// writes take effect with their normal side effects).
    #[wasm_bindgen(js_name = writeMemoryDebug)]
    pub fn write_memory_debug(&mut self, addr: u32, data: &[u8]) {
        self.snes.write_memory_debug(addr, data);
    }

    /// Disassemble instructions starting at an address (focused CPU).
    #[wasm_bindgen(js_name = disassembleAt)]
    pub fn disassemble_at(&mut self, addr: u32, count: u32) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.disassemble_at(addr, count)).unwrap()
    }

    /// Disassemble instructions from a specific CPU at an address.
    #[wasm_bindgen(js_name = disassembleCpu)]
    pub fn disassemble_cpu(&mut self, cpu_id: usize, addr: u32, count: u32) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.disassemble_cpu(cpu_id, addr, count)).unwrap()
    }

    /// Bitmap-aware disassembly window. Returns `DisasmRow[]` with each
    /// row tagged `code` / `unknown`; backward rows walk the per-CPU
    /// coverage bitmap so they are real contiguous predecessors.
    #[wasm_bindgen(js_name = disassembleWindow)]
    pub fn disassemble_window(
        &mut self,
        cpu_id: usize,
        pc: u32,
        before_count: u32,
        after_count: u32,
    ) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.disassemble_window(
            cpu_id,
            pc,
            before_count as usize,
            after_count as usize,
        ))
        .unwrap()
    }

    /// Get the currently focused CPU id (65C816 = 0, SPC700 = 1).
    #[wasm_bindgen(js_name = getFocusedCpu)]
    pub fn get_focused_cpu(&self) -> usize {
        self.snes.focused_cpu()
    }

    /// Set the focused CPU. Affects `step`, `disassembleAt`, etc.
    #[wasm_bindgen(js_name = setFocusedCpu)]
    pub fn set_focused_cpu(&mut self, cpu_id: usize) {
        self.snes.set_focused_cpu(cpu_id);
    }

    /// Step a specific CPU by id.
    #[wasm_bindgen(js_name = stepCpu)]
    pub fn step_cpu(&mut self, cpu_id: usize) -> JsValue {
        let result = serde_wasm_bindgen::to_value(&self.snes.step_cpu(cpu_id)).unwrap();
        self.refresh_present();
        result
    }

    // ===== Coverage =====

    /// True iff `cpu_id` has executed an instruction from `addr`
    /// (65C816 = 0, SPC700 = 1).
    #[wasm_bindgen(js_name = isCovered)]
    pub fn is_covered(&self, cpu_id: usize, addr: u32) -> bool {
        self.snes.is_covered(cpu_id, addr)
    }

    /// `{ totalBytes, coveredBytes, percent }` for the given CPU.
    #[wasm_bindgen(js_name = getCoverageStats)]
    pub fn get_coverage_stats(&self, cpu_id: usize) -> JsValue {
        let (total, covered) = self.snes.coverage_stats(cpu_id);
        let percent = if total == 0 {
            0.0_f32
        } else {
            (covered as f32 / total as f32) * 100.0
        };
        let obj = serde_json::json!({
            "totalBytes": total,
            "coveredBytes": covered,
            "percent": percent,
        });
        serde_wasm_bindgen::to_value(&obj).unwrap()
    }

    // ===== Tracing (65C816 primary CPU) =====

    /// Enable or disable 65C816 execution tracing.
    #[wasm_bindgen(js_name = setTracingEnabled)]
    pub fn set_tracing_enabled(&mut self, enabled: bool) {
        self.snes.debugger().set_tracing_enabled(0, enabled);
    }

    /// Check if 65C816 tracing is enabled.
    #[wasm_bindgen(js_name = isTracingEnabled)]
    pub fn is_tracing_enabled(&mut self) -> bool {
        self.snes.debugger().is_tracing_enabled(0)
    }

    /// Check if any CPU's trace buffer needs to be flushed.
    #[wasm_bindgen(js_name = needsTraceFlush)]
    pub fn needs_trace_flush(&mut self) -> bool {
        self.snes.debugger().needs_trace_flush()
    }

    /// Get 65C816 trace data ready for flushing (empty if none).
    #[wasm_bindgen(js_name = getTraceFlushData)]
    pub fn get_trace_flush_data(&mut self) -> Vec<u8> {
        self.snes
            .debugger()
            .get_trace_flush_data(0)
            .unwrap_or_default()
    }

    /// Clear flushed 65C816 trace data.
    #[wasm_bindgen(js_name = clearTraceFlushed)]
    pub fn clear_trace_flushed(&mut self) {
        self.snes.debugger().clear_trace_flushed(0);
    }

    /// 65C816 trace entry size in bytes.
    #[wasm_bindgen(js_name = getTraceEntrySize)]
    pub fn get_trace_entry_size(&self) -> usize {
        TRACE_ENTRY_SIZE_65816
    }

    /// Convert a binary 65C816 trace chunk to text (one line/instruction).
    #[wasm_bindgen(js_name = convertTraceChunkToText)]
    pub fn convert_trace_chunk_to_text(&self, data: &[u8]) -> String {
        let count = data.len() / TRACE_ENTRY_SIZE_65816;
        let mut result = String::with_capacity(count * 96);
        for i in 0..count {
            let start = i * TRACE_ENTRY_SIZE_65816;
            if let Some(entry) =
                TraceEntryBinary65816::from_bytes(&data[start..start + TRACE_ENTRY_SIZE_65816])
            {
                result.push_str(&format_trace_line_65816(&entry));
                result.push('\n');
            }
        }
        result
    }

    // ===== Tracing (SPC700) =====

    /// Enable or disable SPC700 execution tracing.
    #[wasm_bindgen(js_name = setSpc700TracingEnabled)]
    pub fn set_spc700_tracing_enabled(&mut self, enabled: bool) {
        self.snes.debugger().set_tracing_enabled(1, enabled);
    }

    /// Check if SPC700 tracing is enabled.
    #[wasm_bindgen(js_name = isSpc700TracingEnabled)]
    pub fn is_spc700_tracing_enabled(&mut self) -> bool {
        self.snes.debugger().is_tracing_enabled(1)
    }

    /// Get SPC700 trace data ready for flushing (empty if none).
    #[wasm_bindgen(js_name = getSpc700TraceFlushData)]
    pub fn get_spc700_trace_flush_data(&mut self) -> Vec<u8> {
        self.snes
            .debugger()
            .get_trace_flush_data(1)
            .unwrap_or_default()
    }

    /// Clear flushed SPC700 trace data.
    #[wasm_bindgen(js_name = clearSpc700TraceFlushed)]
    pub fn clear_spc700_trace_flushed(&mut self) {
        self.snes.debugger().clear_trace_flushed(1);
    }

    /// SPC700 trace entry size in bytes.
    #[wasm_bindgen(js_name = getSpc700TraceEntrySize)]
    pub fn get_spc700_trace_entry_size(&self) -> usize {
        TRACE_ENTRY_SIZE_SPC700
    }

    /// Convert a binary SPC700 trace chunk to text (one line/instruction).
    #[wasm_bindgen(js_name = convertSpc700TraceChunkToText)]
    pub fn convert_spc700_trace_chunk_to_text(&self, data: &[u8]) -> String {
        let count = data.len() / TRACE_ENTRY_SIZE_SPC700;
        let mut result = String::with_capacity(count * 64);
        for i in 0..count {
            let start = i * TRACE_ENTRY_SIZE_SPC700;
            if let Some(entry) =
                TraceEntryBinarySpc700::from_bytes(&data[start..start + TRACE_ENTRY_SIZE_SPC700])
            {
                result.push_str(&format_trace_line_spc700(&entry));
                result.push('\n');
            }
        }
        result
    }

    // ===== Tracing (Super FX GSU, cpu 2) =====

    /// Enable or disable Super FX GSU execution tracing. A no-op capture on a
    /// non-Super-FX cart (the GSU trace ring simply stays empty).
    #[wasm_bindgen(js_name = setSuperfxTracingEnabled)]
    pub fn set_superfx_tracing_enabled(&mut self, enabled: bool) {
        self.snes.debugger().set_tracing_enabled(2, enabled);
    }

    /// Check if Super FX GSU tracing is enabled.
    #[wasm_bindgen(js_name = isSuperfxTracingEnabled)]
    pub fn is_superfx_tracing_enabled(&mut self) -> bool {
        self.snes.debugger().is_tracing_enabled(2)
    }

    /// Get Super FX GSU trace data ready for flushing (empty if none).
    #[wasm_bindgen(js_name = getSuperfxTraceFlushData)]
    pub fn get_superfx_trace_flush_data(&mut self) -> Vec<u8> {
        self.snes
            .debugger()
            .get_trace_flush_data(2)
            .unwrap_or_default()
    }

    /// Clear flushed Super FX GSU trace data.
    #[wasm_bindgen(js_name = clearSuperfxTraceFlushed)]
    pub fn clear_superfx_trace_flushed(&mut self) {
        self.snes.debugger().clear_trace_flushed(2);
    }

    /// Super FX GSU trace entry size in bytes.
    #[wasm_bindgen(js_name = getSuperfxTraceEntrySize)]
    pub fn get_superfx_trace_entry_size(&self) -> usize {
        TRACE_ENTRY_SIZE_GSU
    }

    /// Convert a binary Super FX GSU trace chunk to text (one line/instruction).
    /// Absent from a build without the chip.
    #[cfg(feature = "chip-superfx")]
    #[wasm_bindgen(js_name = convertSuperfxTraceChunkToText)]
    pub fn convert_superfx_trace_chunk_to_text(&self, data: &[u8]) -> String {
        let count = data.len() / TRACE_ENTRY_SIZE_GSU;
        let mut result = String::with_capacity(count * 128);
        for i in 0..count {
            let start = i * TRACE_ENTRY_SIZE_GSU;
            if let Some(entry) =
                TraceEntryBinaryGsu::from_bytes(&data[start..start + TRACE_ENTRY_SIZE_GSU])
            {
                result.push_str(&format_trace_line_gsu(&entry));
                result.push('\n');
            }
        }
        result
    }

    // ===== Tracing (Cx4 HG51B169, cpu 2) =====

    /// Enable or disable Cx4 HG51B169 execution tracing. A no-op capture on a
    /// non-Cx4 cart (the trace ring simply stays empty).
    #[wasm_bindgen(js_name = setHg51bTracingEnabled)]
    pub fn set_hg51b_tracing_enabled(&mut self, enabled: bool) {
        self.snes.debugger().set_tracing_enabled(2, enabled);
    }

    /// Check if Cx4 HG51B169 tracing is enabled.
    #[wasm_bindgen(js_name = isHg51bTracingEnabled)]
    pub fn is_hg51b_tracing_enabled(&mut self) -> bool {
        self.snes.debugger().is_tracing_enabled(2)
    }

    /// Get Cx4 HG51B169 trace data ready for flushing (empty if none).
    #[wasm_bindgen(js_name = getHg51bTraceFlushData)]
    pub fn get_hg51b_trace_flush_data(&mut self) -> Vec<u8> {
        self.snes
            .debugger()
            .get_trace_flush_data(2)
            .unwrap_or_default()
    }

    /// Clear flushed Cx4 HG51B169 trace data.
    #[wasm_bindgen(js_name = clearHg51bTraceFlushed)]
    pub fn clear_hg51b_trace_flushed(&mut self) {
        self.snes.debugger().clear_trace_flushed(2);
    }

    /// Cx4 HG51B169 trace entry size in bytes.
    #[wasm_bindgen(js_name = getHg51bTraceEntrySize)]
    pub fn get_hg51b_trace_entry_size(&self) -> usize {
        TRACE_ENTRY_SIZE_CX4
    }

    /// Convert a binary Cx4 HG51B169 trace chunk to text (one line/instruction).
    /// Absent from a build without the chip.
    #[cfg(feature = "chip-cx4")]
    #[wasm_bindgen(js_name = convertHg51bTraceChunkToText)]
    pub fn convert_hg51b_trace_chunk_to_text(&self, data: &[u8]) -> String {
        let count = data.len() / TRACE_ENTRY_SIZE_CX4;
        let mut result = String::with_capacity(count * 128);
        for i in 0..count {
            let start = i * TRACE_ENTRY_SIZE_CX4;
            if let Some(entry) =
                TraceEntryBinaryCx4::from_bytes(&data[start..start + TRACE_ENTRY_SIZE_CX4])
            {
                result.push_str(&format_trace_line_cx4(&entry));
                result.push('\n');
            }
        }
        result
    }

    // ===== SNES-specific debug surfaces =====

    /// Current PPU background mode (`0..=7`, the low 3 bits of `$2105`).
    #[wasm_bindgen(js_name = getPpuMode)]
    pub fn get_ppu_mode(&self) -> u8 {
        self.snes.ppu().bgmode() & 0x07
    }

    /// `$2105` breakdown: `{ mode, bg3Priority, bgTileSize16[4] }`.
    #[wasm_bindgen(js_name = getPpuModeBreakdown)]
    pub fn get_ppu_mode_breakdown(&self) -> JsValue {
        let bgmode = self.snes.ppu().bgmode();
        let obj = serde_json::json!({
            "mode": bgmode & 0x07,
            "bg3Priority": (bgmode & 0x08) != 0,
            "bgTileSize16": [
                (bgmode & 0x10) != 0,
                (bgmode & 0x20) != 0,
                (bgmode & 0x40) != 0,
                (bgmode & 0x80) != 0,
            ],
        });
        serde_wasm_bindgen::to_value(&obj).unwrap()
    }

    /// Live Mode 7 matrix as signed `[A, B, C, D, X, Y]`.
    #[wasm_bindgen(js_name = getMode7Matrix)]
    pub fn get_mode7_matrix(&self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.get_mode7_matrix()).unwrap()
    }

    /// Sample the logical 1024x1024 Mode 7 plane at world coord `(px, py)`
    /// straight from VRAM, honouring the current screen-over repeat mode.
    /// Returns the 8bpp colour index, or `undefined` for a transparent
    /// (repeat-mode-2 out-of-plane) pixel.
    ///
    /// Correctness/parity primitive only: the platform assembles the full
    /// 1024x1024 flat plane in bulk over a `getVram()` view, never via a
    /// per-pixel call.
    #[wasm_bindgen(js_name = mode7Sample)]
    pub fn mode7_sample(&self, px: i32, py: i32) -> Option<u8> {
        let ppu = self.snes.ppu();
        snes1_core::ppu::tile::mode7_sample(ppu.vram(), px, py, ppu.mode7_repeat())
    }

    /// State of all 8 S-DSP voices (`ChannelState[]`, `null` per voice if
    /// unavailable).
    #[wasm_bindgen(js_name = getDspVoiceStates)]
    pub fn get_dsp_voice_states(&mut self) -> JsValue {
        let voices: Vec<_> = (0..8)
            .map(|i| self.snes.audio_debugger().get_channel_state(i))
            .collect();
        serde_wasm_bindgen::to_value(&voices).unwrap()
    }

    /// The four SPC700 -> main-CPU mailbox port values (`$2140-$2143`).
    #[wasm_bindgen(js_name = getApuPortValues)]
    pub fn get_apu_port_values(&self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.bus().apu().output_ports()).unwrap()
    }

    /// 256-colour CGRAM rendered as a 256x256 RGBA8888 swatch grid.
    #[wasm_bindgen(js_name = exportPaletteImage)]
    pub fn export_palette_image(&self) -> Vec<u8> {
        self.snes.export_palette_image()
    }

    /// A 16x16 tile sheet decoded from a VRAM character base.
    /// `source`: 0 = background base, 1 = OBJ base. Returns
    /// `{ width, height, rgba }`.
    #[wasm_bindgen(js_name = exportTileImage)]
    pub fn export_tile_image(&self, source: u8, palette: u8) -> JsValue {
        let src = if source == 0 {
            TileSetSource::Bg
        } else {
            TileSetSource::Obj
        };
        serde_wasm_bindgen::to_value(&self.snes.export_tile_image(src, palette)).unwrap()
    }

    /// Background `bg` (`0..=3`) tilemap rendered to an RGBA image
    /// (`{ width, height, rgba }`).
    #[wasm_bindgen(js_name = exportTilemapImage)]
    pub fn export_tilemap_image(&self, bg: usize) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.export_tilemap_image(bg)).unwrap()
    }

    /// All 128 OAM sprites flattened into `SpriteDescriptor[]`.
    #[wasm_bindgen(js_name = exportOamSprites)]
    pub fn export_oam_sprites(&self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.export_oam_sprites()).unwrap()
    }

    /// Decode OAM sprite `index` (`0..=127`) into a tight RGBA cut-out:
    /// `{ width, height, data: Uint8Array }`, or `null` when the index is
    /// out of range / empty. The core resolves tile selection, OBSEL name
    /// page, flips and OBJ palette and applies palette index 0 -> alpha 0;
    /// the platform owns PNG encoding + UX. Mirrors `sg1`'s binding so the
    /// host's composite-sprite path is core-agnostic.
    #[wasm_bindgen(js_name = exportSpriteImage)]
    pub fn export_sprite_image(&mut self, index: u32) -> JsValue {
        match self.snes.video_debugger().export_sprite_image(index) {
            Ok(img) => {
                let data = js_sys::Uint8Array::new_with_length(img.rgba.len() as u32);
                data.copy_from(&img.rgba);
                let obj = js_sys::Object::new();
                let _ = js_sys::Reflect::set(
                    &obj,
                    &JsValue::from_str("width"),
                    &JsValue::from_f64(f64::from(img.width)),
                );
                let _ = js_sys::Reflect::set(
                    &obj,
                    &JsValue::from_str("height"),
                    &JsValue::from_f64(f64::from(img.height)),
                );
                let _ = js_sys::Reflect::set(&obj, &JsValue::from_str("data"), &data);
                obj.into()
            }
            Err(_) => JsValue::NULL,
        }
    }

    /// The canonical sprite list for the composite-object picker:
    /// `SpriteData[]` with **normalized signed screen-space** X (the 9-bit
    /// OBJ X wrap already undone); Y is the raw top scanline. Read-only;
    /// mirrors the `sg1` binding so the host's picker is core-agnostic.
    #[wasm_bindgen(js_name = getSpriteData)]
    pub fn get_sprite_data(&self) -> JsValue {
        use retro_core::video::VideoDebugger;
        let sprites = self.snes.ppu().get_sprite_data();
        serde_wasm_bindgen::to_value(&sprites).unwrap()
    }

    /// The renderer's resolved sprite paint order as `SpriteOrderEntry[]`
    /// in back-to-front order (last entry draws on top), mirroring
    /// `render_obj_row`'s rotated OAM walk so a composite exporter blits
    /// members exactly as the PPU drew them.
    #[wasm_bindgen(js_name = resolvedSpriteRenderOrder)]
    pub fn resolved_sprite_render_order(&self) -> JsValue {
        use retro_core::video::VideoDebugger;
        let order = self.snes.ppu().resolved_sprite_render_order();
        serde_wasm_bindgen::to_value(&order).unwrap()
    }

    /// Decode a run of VRAM tiles to an *indexed* sheet for host-side
    /// full-map assembly: `{ width, height, pixels: Uint8Array, bpp,
    /// transparentIndex }`, or `null` if the request is unsupported / out
    /// of range. Unlike Genesis (4bpp only) the SNES honours `bpp in
    /// {2,4,8}`; the core owns only the per-depth bitplane decode while the
    /// platform reads the tilemap and applies palette + flip + transparency.
    ///
    /// `source`: 0 = VRAM. `tileSize`: 0 = 8x8 (only 8x8 is decoded here;
    /// 16x16 modes are resolved host-side). `pixels` is one palette index
    /// per pixel, `count` tiles stacked vertically.
    #[wasm_bindgen(js_name = decodeTiles)]
    pub fn decode_tiles(
        &mut self,
        source: u32,
        base_addr: u32,
        first_tile: u32,
        count: u32,
        bpp: u8,
        tile_size: u32,
    ) -> JsValue {
        use retro_core::video::{TileDecodeRequest, TileSize, TileSource};
        let source = match source {
            0 => TileSource::Vram,
            _ => return JsValue::NULL,
        };
        let tile_size = match tile_size {
            0 => TileSize::Px8,
            1 => TileSize::Px16,
            _ => return JsValue::NULL,
        };
        let request = TileDecodeRequest {
            source,
            base_addr,
            first_tile,
            count,
            bpp,
            tile_size,
        };
        match self.snes.video_debugger().decode_tiles(&request) {
            Ok(img) => {
                let data = js_sys::Uint8Array::new_with_length(img.pixels.len() as u32);
                data.copy_from(&img.pixels);
                let obj = js_sys::Object::new();
                let _ = js_sys::Reflect::set(
                    &obj,
                    &JsValue::from_str("width"),
                    &JsValue::from_f64(f64::from(img.width)),
                );
                let _ = js_sys::Reflect::set(
                    &obj,
                    &JsValue::from_str("height"),
                    &JsValue::from_f64(f64::from(img.height)),
                );
                let _ = js_sys::Reflect::set(&obj, &JsValue::from_str("pixels"), &data);
                let _ = js_sys::Reflect::set(
                    &obj,
                    &JsValue::from_str("bpp"),
                    &JsValue::from_f64(f64::from(img.bpp)),
                );
                let transparent = match img.transparent_index {
                    Some(v) => JsValue::from_f64(f64::from(v)),
                    None => JsValue::NULL,
                };
                let _ = js_sys::Reflect::set(
                    &obj,
                    &JsValue::from_str("transparentIndex"),
                    &transparent,
                );
                obj.into()
            }
            Err(_) => JsValue::NULL,
        }
    }

    /// Per-background addressing + format snapshot (`BgLayout`) for the
    /// host's raw-layer (full-map) assembler: `{ bg, bgMode,
    /// tilemapBaseByte, charBaseByte, screenSize, widthCells, heightCells,
    /// bpp, tileSize16, paletteBase, scrollX, scrollY }`. `bpp` is `0`
    /// when the BG does not draw under the current mode (host falls back
    /// to the viewport). `bg` is clamped to `0..=3`.
    #[wasm_bindgen(js_name = getBgLayout)]
    pub fn get_bg_layout(&self, bg: usize) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.bg_layout(bg)).unwrap()
    }

    /// Cartridge manifest: `{ memoryMap, romRegions, sramRegions,
    /// mapperState }` derived from the installed map plan.
    #[wasm_bindgen(js_name = getCartridgeManifest)]
    pub fn get_cartridge_manifest(&mut self) -> JsValue {
        let dbg = self.snes.cartridge_debugger();
        let manifest = serde_json::json!({
            "memoryMap": dbg.get_memory_map(),
            "romRegions": dbg.get_rom_regions(),
            "sramRegions": dbg.get_sram_regions(),
            "mapperState": dbg.get_mapper_state(),
        });
        serde_wasm_bindgen::to_value(&manifest).unwrap()
    }

    /// Detect the editable `cartridge-manifest-v1` for an arbitrary ROM
    /// image, as JSON. Side-effect-free: builds nothing, installs
    /// nothing, and does not disturb a running game, the hub modal uses
    /// it to show the heuristic mapping before the user edits it.
    #[wasm_bindgen(js_name = detectManifest)]
    pub fn detect_manifest(&self, data: &[u8]) -> Result<String, JsValue> {
        let manifest = self.snes.detected_manifest(data);
        serde_json::to_string(&manifest).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Detect the editable `cartridge-manifest-v1` for the *currently
    /// loaded* ROM, as JSON. Returns `null` (JSON) when no ROM is
    /// loaded. Reflects pure detection, never any staged override, so
    /// the editor can diff the user's override against the baseline.
    #[wasm_bindgen(js_name = getDetectedManifest)]
    pub fn get_detected_manifest(&self) -> Result<String, JsValue> {
        if self.last_rom.is_empty() {
            return Ok("null".to_string());
        }
        let manifest = self.snes.detected_manifest(&self.last_rom);
        serde_json::to_string(&manifest).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Stage a `cartridge-manifest-v1` (JSON) applied on the next
    /// `loadRom`, overriding heuristic detection. Returns a JS `Error`
    /// when the JSON is malformed or the schema version is unsupported,
    /// so the editor can surface validation failures. The override does
    /// not affect the currently installed cartridge, reload the ROM.
    #[wasm_bindgen(js_name = setCartridgeManifest)]
    pub fn set_cartridge_manifest(&mut self, json: &str) -> Result<(), JsValue> {
        let manifest = parse_manifest(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.snes.set_cartridge_manifest_override(Some(manifest));
        Ok(())
    }

    /// Clear any staged manifest override; the next `loadRom` reverts to
    /// pure heuristic detection.
    #[wasm_bindgen(js_name = clearCartridgeManifest)]
    pub fn clear_cartridge_manifest(&mut self) {
        self.snes.set_cartridge_manifest_override(None);
    }

    // ===== Raw debug memory views (parity with sg1's getVram/getCram) =====

    /// Raw 64 KiB VRAM as host-endian bytes, for a read-only hex view.
    /// Reuses the `VideoDebugger` byte reinterpretation so the dump can
    /// never diverge from what the renderer reads.
    #[wasm_bindgen(js_name = getVram)]
    pub fn get_vram(&mut self) -> Vec<u8> {
        self.snes.video_debugger().get_vram().to_vec()
    }

    /// The 256-entry CGRAM as raw BGR15 words (one per colour). A palette
    /// viewer decodes these into RGBA swatches.
    #[wasm_bindgen(js_name = getCgram)]
    pub fn get_cgram(&self) -> Vec<u16> {
        self.snes.ppu().cgram().to_vec()
    }

    /// The 544-byte OAM (sprite attribute table) as raw bytes.
    #[wasm_bindgen(js_name = getOam)]
    pub fn get_oam(&self) -> Vec<u8> {
        self.snes.ppu().oam().to_vec()
    }

    /// The SPC700's 64 KiB audio RAM (ARAM) as raw bytes, the address
    /// space the SPC700 + S-DSP execute against, surfaced as an ARAM
    /// memory tab when the SPC700 is the focused CPU.
    #[wasm_bindgen(js_name = getAram)]
    pub fn get_aram(&self) -> Vec<u8> {
        self.snes.bus().apu().aram().to_vec()
    }

    /// PPU register snapshot (`VideoRegister[]`: BGMODE, TM/TS, Mode 7
    /// matrix + centre, BG1 scroll) for the PPU state panel.
    #[wasm_bindgen(js_name = getPpuRegisters)]
    pub fn get_ppu_registers(&mut self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.video_debugger().get_registers()).unwrap()
    }

    /// Per-layer info (`LayerData[]`: BG1-4 + OBJ, each with
    /// size/scroll/enabled) for the layer-toggle list.
    #[wasm_bindgen(js_name = getLayerInfo)]
    pub fn get_layer_info(&mut self) -> JsValue {
        let count = self.snes.video_debugger().get_layer_count();
        let mut layers = Vec::with_capacity(count as usize);
        for id in 0..count {
            if let Some(layer) = self.snes.video_debugger().get_layer_data(id) {
                layers.push(layer);
            }
        }
        serde_wasm_bindgen::to_value(&layers).unwrap()
    }

    /// Show/hide a layer in the debug renderer (BG1-4 by index, OBJ last).
    /// A hidden layer is suppressed from composition without touching the
    /// game's real TM/TS screen-designation registers.
    #[wasm_bindgen(js_name = toggleLayer)]
    pub fn toggle_layer(&mut self, layer_id: u32, visible: bool) {
        self.snes.video_debugger().toggle_layer(layer_id, visible);
    }

    /// Re-render the held frame from present PPU state plus the debugger
    /// layer mask, without advancing emulation. The host calls this after
    /// a `toggleLayer` while paused so the canvas reflects the change at
    /// once (a running core repaints on its own next frame; a paused one
    /// never would). Drives [`SnesEmulator::repaint_held_frame`], the
    /// SNES equivalent of `sg1`'s `repaintFrame`.
    #[wasm_bindgen(js_name = repaintFrame)]
    pub fn repaint_frame(&mut self) {
        self.snes.repaint_held_frame();
        // Re-pack the freshly rendered active rectangle so the next
        // framebuffer read sees the repaint (the live loop refreshes the
        // present buffer after each frame; a paused repaint must do it
        // explicitly).
        self.refresh_present();
    }

    /// Mute/unmute one S-DSP voice (0-7) in the debugger without altering
    /// the game's key-on state.
    #[wasm_bindgen(js_name = muteDspChannel)]
    pub fn mute_dsp_channel(&mut self, channel: u32, muted: bool) {
        self.snes.audio_debugger().mute_channel(channel, muted);
    }

    /// `samples` of the mixed S-DSP output as mono f32 in [-1, 1] for the
    /// waveform / spectrum views. Peeks the audio ring without draining
    /// it, so the host's normal audio pull is unaffected.
    #[wasm_bindgen(js_name = getDspMixedWaveform)]
    pub fn get_dsp_mixed_waveform(&mut self, samples: u32) -> Vec<f32> {
        self.snes.audio_debugger().get_output_waveform(samples)
    }

    /// `samples` of one S-DSP voice's approximated waveform (the mixed
    /// output scaled by that voice's instantaneous amplitude; silence
    /// when the voice is muted or keyed off).
    #[wasm_bindgen(js_name = getDspWaveform)]
    pub fn get_dsp_waveform(&mut self, channel: u32, samples: u32) -> Vec<f32> {
        self.snes.audio_debugger().get_waveform(channel, samples)
    }
}

#[cfg(test)]
mod tests {
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
        let mut rom = vec![0u8; 0x2_0000];
        rom[..2].copy_from_slice(&[0x80, 0xFE]); // BRA -2
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
        let list: Vec<BreakpointInfo> =
            serde_wasm_bindgen::from_value(emu.list_breakpoints()).unwrap();
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
        assert_eq!(events.len(), 9);
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
}
