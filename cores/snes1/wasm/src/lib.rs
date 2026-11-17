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

use retro_core::capture::CaptureCore;
use retro_core::cartridge::CartridgeDebugger;
use retro_core::debug::{trace_to_text, TraceEntry};
use retro_core::input::{gamepad_map_from_text_keys, gamepad_map_with_text_keys};
use retro_core::*;
use retro_wasm::js::{map_as_object, plain_object, set_property, uint16_array, uint8_array};
use snes1_core::{
    parse_manifest, CoprocessorMode, SnesEmulator, SnesSnapshotConsole, TileSetSource,
    TraceEntryBinaryCx4, TraceEntryBinaryGsu, TraceEntryBinarySpc700, CARTRIDGE_CPU, SOUND_CPU,
};
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

/// The SNES's keys of a frame snapshot (`captureFrameSnapshot`), after
/// `palette`: `oam`, a `Uint8Array` copy of OAM as `getOam` returns it, and
/// `ppuSettings`, the PPU's settings as `getPpuSettings` returns them, a
/// plain object.
fn snapshot_console(console: &SnesSnapshotConsole, snapshot: &js_sys::Object) {
    set_property(snapshot, "oam", &uint8_array(&console.oam));
    set_property(
        snapshot,
        "ppuSettings",
        &map_as_object(&console.ppu_settings),
    );
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

    /// Run one frame. Tier selection (fast / light-debug / full-debug)
    /// is internal to the core based on active debug features.
    #[wasm_bindgen(js_name = runFrame)]
    pub fn run_frame(&mut self) {
        self.snes.run_frame();
        self.refresh_present();
    }

    /// Run to the start of vertical blanking and stop there: the capture
    /// boundary, as `runToCaptureBoundary`.
    #[wasm_bindgen(js_name = runUntilVBlank)]
    pub fn run_until_vblank(&mut self) {
        self.snes.run_until_vblank();
        self.refresh_present();
    }

    // ===== Capture: the readers only this core has =====

    /// Each line's window masks: `null` when no game is loaded, else
    /// `{ frame, fromLineStates, lines: Uint16Array, masks: Uint8Array }`.
    /// `lines` lists every line the frame drew, in order; `masks` holds 8
    /// masks a line of 32 bytes: the window regions of BG1, BG2, BG3, BG4
    /// and the sprites (where the layer's combined windows cover, whether or
    /// not the screens clip the layer there), the colour window's region,
    /// where the main screen keeps its pixels and where colour math may run.
    /// Bit `x & 7` of byte `x >> 3` is dot `x`; in a hires line output
    /// column `k` is dot `k >> 1`. Copies.
    #[wasm_bindgen(js_name = getWindowMasks)]
    pub fn get_window_masks(&self) -> JsValue {
        let Some(masks) = self.snes.window_masks() else {
            return JsValue::NULL;
        };
        plain_object(&[
            ("frame", JsValue::from_f64(masks.frame as f64)),
            ("fromLineStates", JsValue::from_bool(masks.from_line_states)),
            ("lines", uint16_array(&masks.lines)),
            ("masks", uint8_array(&masks.masks)),
        ])
    }

    /// The offset-per-tile columns of the held frame: `null` when no game is
    /// loaded, else `{ frame, fromLineStates, lines: Uint16Array, columns,
    /// scroll: Uint16Array, replaced: Uint8Array }`. One row for each line
    /// drawn in Mode 2, 4 or 6 outside forced blank (two for a line of an
    /// interlaced hires frame, one for each background line it wove);
    /// `lines` names each row's line. `columns` tile columns a row: 33, or
    /// 65 when a row is Mode 6. Per row, per background (BG1, BG2), per
    /// column: `scroll` holds the horizontal then the vertical tilemap
    /// coordinate the column was fetched at, absolute (the scroll already in
    /// them); `replaced` bit 0 BG3 replaced the horizontal scroll, bit 1 the
    /// vertical, bit 7 the column lies past the line's end. Copies.
    #[wasm_bindgen(js_name = getOffsetPerTile)]
    pub fn get_offset_per_tile(&self) -> JsValue {
        let Some(table) = self.snes.offset_per_tile() else {
            return JsValue::NULL;
        };
        plain_object(&[
            ("frame", JsValue::from_f64(table.frame as f64)),
            ("fromLineStates", JsValue::from_bool(table.from_line_states)),
            ("lines", uint16_array(&table.lines)),
            ("columns", JsValue::from_f64(f64::from(table.columns))),
            ("scroll", uint16_array(&table.scroll)),
            ("replaced", uint8_array(&table.replaced)),
        ])
    }

    /// The PPU's settings in the core's own field names (`PpuSettings`),
    /// a plain object: the display state, the sprites' tiles and sizes
    /// (`sprite_tiles`, as getSpriteTileSettings), the background mode,
    /// mosaic, Mode 7 (select bits, offsets, matrix and centre), the
    /// windows per layer, the screens and colour math, and the SETINI
    /// flags.
    #[wasm_bindgen(js_name = getPpuSettings)]
    pub fn get_ppu_settings(&self) -> JsValue {
        map_as_object(&self.snes.ppu_settings())
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

    /// Save emulator state (zstd-compressed envelope). Stopped at a
    /// capture boundary, the frame held there is finished first, so a
    /// savestate never holds it.
    #[wasm_bindgen(js_name = saveState)]
    pub fn save_state(&mut self) -> Vec<u8> {
        let state = self.snes.save_state_at_frame_end();
        self.refresh_present();
        state
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
    //
    // The generic debugger methods (breakpoints, execution mode, history,
    // call stack, focus, steps, coverage and the main CPU's trace) are
    // written by `retro_wasm::debugger_bindings!`, after this block.

    /// Read memory for debugging, as the 65C816 sees it, through the
    /// core's side-effect-free peek: work RAM, ROM and the cartridge's RAM
    /// give their bytes, a coprocessor's ROM or RAM window the bytes behind
    /// it, and a register 0 or the open-bus value. Nothing in the machine
    /// moves.
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

    // ===== Tracing (SPC700) =====

    /// Enable or disable SPC700 execution tracing.
    #[wasm_bindgen(js_name = setSpc700TracingEnabled)]
    pub fn set_spc700_tracing_enabled(&mut self, enabled: bool) {
        self.snes.debugger().set_tracing_enabled(SOUND_CPU, enabled);
    }

    /// Check if SPC700 tracing is enabled.
    #[wasm_bindgen(js_name = isSpc700TracingEnabled)]
    pub fn is_spc700_tracing_enabled(&mut self) -> bool {
        self.snes.debugger_ref().is_tracing_enabled(SOUND_CPU)
    }

    /// Get SPC700 trace data ready for flushing (empty if none).
    #[wasm_bindgen(js_name = getSpc700TraceFlushData)]
    pub fn get_spc700_trace_flush_data(&mut self) -> Vec<u8> {
        self.snes
            .debugger_ref()
            .get_trace_flush_data(SOUND_CPU)
            .unwrap_or_default()
    }

    /// Clear flushed SPC700 trace data.
    #[wasm_bindgen(js_name = clearSpc700TraceFlushed)]
    pub fn clear_spc700_trace_flushed(&mut self) {
        self.snes.debugger().clear_trace_flushed(SOUND_CPU);
    }

    /// SPC700 trace entry size in bytes.
    #[wasm_bindgen(js_name = getSpc700TraceEntrySize)]
    pub fn get_spc700_trace_entry_size(&self) -> usize {
        <TraceEntryBinarySpc700 as TraceEntry>::SIZE
    }

    /// Convert a binary SPC700 trace chunk to text (one line/instruction).
    #[wasm_bindgen(js_name = convertSpc700TraceChunkToText)]
    pub fn convert_spc700_trace_chunk_to_text(&self, data: &[u8]) -> String {
        trace_to_text::<TraceEntryBinarySpc700, ()>(data, &mut ())
    }

    // ===== Tracing (Super FX GSU, the cartridge's CPU) =====

    /// Enable or disable Super FX GSU execution tracing. On a cartridge
    /// whose CPU is not one that traces (a plain one, an SA-1), the switch
    /// stays off and the frames stay on the fast loop.
    #[wasm_bindgen(js_name = setSuperfxTracingEnabled)]
    pub fn set_superfx_tracing_enabled(&mut self, enabled: bool) {
        self.snes
            .debugger()
            .set_tracing_enabled(CARTRIDGE_CPU, enabled);
    }

    /// Check if Super FX GSU tracing is enabled.
    #[wasm_bindgen(js_name = isSuperfxTracingEnabled)]
    pub fn is_superfx_tracing_enabled(&mut self) -> bool {
        self.snes.debugger_ref().is_tracing_enabled(CARTRIDGE_CPU)
    }

    /// Get Super FX GSU trace data ready for flushing (empty if none).
    #[wasm_bindgen(js_name = getSuperfxTraceFlushData)]
    pub fn get_superfx_trace_flush_data(&mut self) -> Vec<u8> {
        self.snes
            .debugger_ref()
            .get_trace_flush_data(CARTRIDGE_CPU)
            .unwrap_or_default()
    }

    /// Clear flushed Super FX GSU trace data.
    #[wasm_bindgen(js_name = clearSuperfxTraceFlushed)]
    pub fn clear_superfx_trace_flushed(&mut self) {
        self.snes.debugger().clear_trace_flushed(CARTRIDGE_CPU);
    }

    /// Super FX GSU trace entry size in bytes.
    #[wasm_bindgen(js_name = getSuperfxTraceEntrySize)]
    pub fn get_superfx_trace_entry_size(&self) -> usize {
        <TraceEntryBinaryGsu as TraceEntry>::SIZE
    }

    /// Convert a binary Super FX GSU trace chunk to text (one line/instruction).
    /// Absent from a build without the chip.
    #[cfg(feature = "chip-superfx")]
    #[wasm_bindgen(js_name = convertSuperfxTraceChunkToText)]
    pub fn convert_superfx_trace_chunk_to_text(&self, data: &[u8]) -> String {
        trace_to_text::<TraceEntryBinaryGsu, ()>(data, &mut ())
    }

    // ===== Tracing (Cx4 HG51B169, the cartridge's CPU) =====

    /// Enable or disable Cx4 HG51B169 execution tracing. On a cartridge
    /// whose CPU is not one that traces (a plain one, an SA-1), the switch
    /// stays off and the frames stay on the fast loop.
    #[wasm_bindgen(js_name = setHg51bTracingEnabled)]
    pub fn set_hg51b_tracing_enabled(&mut self, enabled: bool) {
        self.snes
            .debugger()
            .set_tracing_enabled(CARTRIDGE_CPU, enabled);
    }

    /// Check if Cx4 HG51B169 tracing is enabled.
    #[wasm_bindgen(js_name = isHg51bTracingEnabled)]
    pub fn is_hg51b_tracing_enabled(&mut self) -> bool {
        self.snes.debugger_ref().is_tracing_enabled(CARTRIDGE_CPU)
    }

    /// Get Cx4 HG51B169 trace data ready for flushing (empty if none).
    #[wasm_bindgen(js_name = getHg51bTraceFlushData)]
    pub fn get_hg51b_trace_flush_data(&mut self) -> Vec<u8> {
        self.snes
            .debugger_ref()
            .get_trace_flush_data(CARTRIDGE_CPU)
            .unwrap_or_default()
    }

    /// Clear flushed Cx4 HG51B169 trace data.
    #[wasm_bindgen(js_name = clearHg51bTraceFlushed)]
    pub fn clear_hg51b_trace_flushed(&mut self) {
        self.snes.debugger().clear_trace_flushed(CARTRIDGE_CPU);
    }

    /// Cx4 HG51B169 trace entry size in bytes.
    #[wasm_bindgen(js_name = getHg51bTraceEntrySize)]
    pub fn get_hg51b_trace_entry_size(&self) -> usize {
        <TraceEntryBinaryCx4 as TraceEntry>::SIZE
    }

    /// Convert a binary Cx4 HG51B169 trace chunk to text (one line/instruction).
    /// Absent from a build without the chip.
    #[cfg(feature = "chip-cx4")]
    #[wasm_bindgen(js_name = convertHg51bTraceChunkToText)]
    pub fn convert_hg51b_trace_chunk_to_text(&self, data: &[u8]) -> String {
        trace_to_text::<TraceEntryBinaryCx4, ()>(data, &mut ())
    }

    // ===== SNES-specific debug surfaces =====

    /// Current PPU background mode (`0..=7`, the low 3 bits of `$2105`).
    #[wasm_bindgen(js_name = getPpuMode)]
    pub fn get_ppu_mode(&self) -> u8 {
        self.snes.ppu().bgmode() & 0x07
    }

    /// `$2105` breakdown: `{ mode, bg3Priority, bgTileSize16[4] }`, a plain
    /// object. The PPU keeps the mode and the two flags in fields of their
    /// own, so the flags are read from those, not from the mode.
    #[wasm_bindgen(js_name = getPpuModeBreakdown)]
    pub fn get_ppu_mode_breakdown(&self) -> JsValue {
        let settings = self.snes.ppu_settings();
        let obj = serde_json::json!({
            "mode": settings.bgmode & 0x07,
            "bg3Priority": settings.bg3_priority,
            "bgTileSize16": settings.bg_tile_size,
        });
        map_as_object(&obj)
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

    /// The sprites the held frame drew, as `SpriteOrderEntry[]` in
    /// back-to-front order (last entry draws on top), mirroring
    /// `render_obj_row`'s rotated OAM walk so a composite exporter blits
    /// members exactly as the PPU drew them: only the sprites of which some
    /// line that showed sprites drew a visible column, each once, with the
    /// priority the line that drew it first read; an empty array when the
    /// frame drew no sprite.
    #[wasm_bindgen(js_name = resolvedSpriteRenderOrder)]
    pub fn resolved_sprite_render_order(&mut self) -> JsValue {
        let order = self.snes.video_debugger().resolved_sprite_render_order();
        serde_wasm_bindgen::to_value(&order).unwrap()
    }

    /// Decode a run of VRAM tiles to an *indexed* sheet for host-side
    /// full-map assembly: `{ width, height, pixels: Uint8Array, bpp,
    /// transparentIndex }`, or `null` if the request is unsupported / out
    /// of range. Unlike Genesis (4bpp only) the SNES honours `bpp in
    /// {2,4,8}`; the core owns only the per-depth bitplane decode while the
    /// platform reads the tilemap and applies palette + flip + transparency.
    ///
    /// `source`: 0 = VRAM characters from `baseAddr` (a byte address), 1 =
    /// the Mode 7 characters (8bpp, 8x8; `baseAddr` 0 for the plane's own,
    /// at word 0). `tileSize`: 0 = 8x8; 1 = the 16x16 block a 16x16
    /// background tile names (`firstTile` the tile number, the block its
    /// characters N, N+1, N+16 and N+17 from the character base
    /// `baseAddr`, as the renderer reads them). `pixels` is one palette
    /// index per pixel, `count` tiles stacked vertically, `width` 8 or 16.
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
            1 => TileSource::Mode7,
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
    /// host's raw-layer (full-map) assembler, in the core's own field
    /// names: `{ bg, bg_mode, tilemap_base_byte, char_base_byte,
    /// screen_size, width_cells, height_cells, bpp, tile_size16,
    /// palette_base, scroll_x, scroll_y }`. `bpp` is `0` when the BG does
    /// not draw under the current mode (host falls back to the viewport).
    /// `bg` is clamped to `0..=3`.
    #[wasm_bindgen(js_name = getBgLayout)]
    pub fn get_bg_layout(&self, bg: usize) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.bg_layout(bg)).unwrap()
    }

    /// Where the renderer finds the sprites' tiles, and the two sprite
    /// sizes (`SpriteTileSettings`), in the core's own field names:
    /// `{ small_width, small_height, large_width, large_height,
    /// first_table_base_word, second_table_base_word }`. The sizes are in
    /// pixels and the addresses are VRAM word addresses; a sprite whose
    /// name-select bit is set (bit 0 of its fourth OAM byte, `getOam`)
    /// reads the second table. Read-only.
    #[wasm_bindgen(js_name = getSpriteTileSettings)]
    pub fn get_sprite_tile_settings(&self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.sprite_tile_settings()).unwrap()
    }

    /// Cartridge manifest: `{ memoryMap, romRegions, sramRegions,
    /// mapperState }` derived from the installed map plan, plain objects
    /// all the way down.
    #[wasm_bindgen(js_name = getCartridgeManifest)]
    pub fn get_cartridge_manifest(&mut self) -> JsValue {
        let dbg = self.snes.cartridge_debugger();
        let manifest = serde_json::json!({
            "memoryMap": dbg.get_memory_map(),
            "romRegions": dbg.get_rom_regions(),
            "sramRegions": dbg.get_sram_regions(),
            "mapperState": dbg.get_mapper_state(),
        });
        map_as_object(&manifest)
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

    /// Write VRAM for debugging, to edit a game's graphics: `bytes` from
    /// word `word_address` on, in the layout `getVram` returns (byte
    /// `2 * word_address + n`, the low half of each word first), so
    /// `getVram` gives them back. Straight into memory, through no port:
    /// the timing, the CPUs and the PPU's registers are untouched, and the
    /// write lands whatever line is being drawn. The word address wraps at
    /// 32 K words, a buffer longer than the 64 KiB is cut to it, and an
    /// empty one writes nothing. The renderer's cached tilemap rows and
    /// sprite tiles follow the write. A running game shows the change on
    /// its next frame, a paused one after `repaintFrame`.
    #[wasm_bindgen(js_name = writeVram)]
    pub fn write_vram(&mut self, word_address: u32, bytes: &[u8]) {
        self.snes.write_vram_debug(word_address, bytes);
    }

    /// Write CGRAM for debugging, to edit a palette: `colours` from colour
    /// `colour_index` (0-255) on, BGR555 words as `getCgram` returns them
    /// (bit 15 is dropped, as the port drops it). Straight into memory,
    /// through no port. The index wraps at 256, a buffer longer than CGRAM
    /// is cut to its 256 colours, and an empty one writes nothing. A
    /// running game shows the change on its next frame, a paused one after
    /// `repaintFrame`.
    #[wasm_bindgen(js_name = writeCgram)]
    pub fn write_cgram(&mut self, colour_index: u32, colours: &[u16]) {
        self.snes.write_cgram_debug(colour_index, colours);
    }

    /// Write OAM for debugging, to edit sprites: `bytes` from byte
    /// `byte_address` (0-543) on, in the layout `getOam` returns (four
    /// bytes a sprite in the 512-byte low table, then the 32-byte high
    /// table). Straight into memory, through no port. The address wraps at
    /// 544, a buffer longer than OAM is cut to its 544 bytes, and an empty
    /// one writes nothing. A running game shows the change on its next
    /// frame, a paused one after `repaintFrame`.
    #[wasm_bindgen(js_name = writeOam)]
    pub fn write_oam(&mut self, byte_address: u32, bytes: &[u8]) {
        self.snes.write_oam_debug(byte_address, bytes);
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

// The debugger methods every binding crate exports the same way
// (`getCpuStates`, the breakpoints, `setExecutionMode`, the history and the
// call stack, the focus, `step` and `stepCpu`, coverage and the main CPU's
// trace), over `EmulatorCore` and `DebuggerCore` on the emulator;
// `refresh_present` runs after each step, so the packed framebuffer shows
// it.
retro_wasm::debugger_bindings!(RetroEmulator, snes, after_step = refresh_present);

// The capture methods every core exports the same way (`runToCaptureBoundary`,
// `captureFrameSnapshot`, `renderLayer`, `getPixelSources`, `takeFrameWrites`
// and the rest), over `CaptureCore` on the emulator; `refresh_present` runs
// after the two that run or redraw a frame, so the packed framebuffer shows
// it.
retro_wasm::capture_bindings!(
    RetroEmulator,
    snes,
    console = snapshot_console,
    after_frame = refresh_present
);

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

    #[wasm_bindgen_test]
    fn the_frame_snapshot_is_a_plain_object() {
        let emu = RetroEmulator::new("snes").unwrap();
        let snapshot = emu.capture_frame_snapshot();
        assert_plain_object(&snapshot, "framebuffer");
        assert_plain_object(&property(&snapshot, "ppuSettings"), "bgmode");
        let registers = js_sys::Array::from(&property(&snapshot, "registers"));
        assert_plain_object(&registers.get(0), "name");
    }

    #[wasm_bindgen_test]
    fn a_capture_boundary_snapshot_carries_copies_under_its_names() {
        let mut emu = RetroEmulator::new("snes").unwrap();
        emu.load_rom(&synth_lorom("CAPTURE")).unwrap();
        assert!(!emu.at_capture_boundary());
        emu.run_to_capture_boundary();
        assert!(emu.at_capture_boundary());
        assert_eq!(
            emu.get_frame_number(),
            0,
            "the held frame is not counted yet"
        );

        let snapshot = emu.capture_frame_snapshot();
        assert_eq!(property(&snapshot, "frame").as_f64(), Some(1.0));
        assert_eq!(property(&snapshot, "atBoundary").as_bool(), Some(true));
        let width = property(&snapshot, "width").as_f64().unwrap() as u32;
        let height = property(&snapshot, "height").as_f64().unwrap() as u32;
        assert_eq!((width, height), (256, 224));
        let bytes =
            |key: &str| -> js_sys::Uint8Array { property(&snapshot, key).dyn_into().unwrap() };
        assert_eq!(bytes("framebuffer").length(), width * height * 4);
        assert_eq!(bytes("vram").length(), 0x10000);
        assert_eq!(bytes("oam").length(), 0x220);
        let palette: js_sys::Uint16Array = property(&snapshot, "palette").dyn_into().unwrap();
        assert_eq!(palette.length(), 0x100);
        assert!(js_sys::Array::is_array(&property(&snapshot, "registers")));
        let settings = property(&snapshot, "ppuSettings");
        assert_eq!(property(&settings, "bgmode").as_f64(), Some(0.0));
        assert!(property(&settings, "sprite_tiles").is_object());

        // A save finishes the held frame first.
        emu.save_state();
        assert!(!emu.at_capture_boundary());
        assert_eq!(emu.get_frame_number(), 1);
    }

    #[wasm_bindgen_test]
    fn a_layer_render_keys_what_the_layer_does_not_cover() {
        let mut emu = RetroEmulator::new("snes").unwrap();
        // Display on over a red backdrop, and no layer drawn anywhere: the
        // composite is red, every layer image empty.
        emu.write_cgram(0, &[0x001F]);
        emu.write_memory_debug(0x2100, &[0x0F]);
        emu.repaint_frame();
        let image = emu.render_layer(0);
        assert_eq!(property(&image, "width").as_f64(), Some(256.0));
        let data: js_sys::Uint8Array = property(&image, "data").dyn_into().unwrap();
        assert!(data
            .to_vec()
            .iter()
            .skip(3)
            .step_by(4)
            .all(|&alpha| alpha == 0));
        assert_eq!(
            &emu.get_framebuffer_data().to_vec()[..4],
            &[0xFF, 0x00, 0x00, 0xFF],
            "the composite the host shows is left as it was"
        );
        assert!(emu.render_layer(5).is_null());
    }

    #[wasm_bindgen_test]
    fn the_mode_breakdown_and_the_settings_read_the_flags_from_their_fields() {
        let mut emu = RetroEmulator::new("snes").unwrap();
        // BGMODE: mode 1, BG3 lifted, 16x16 tiles on BG2 and BG4.
        emu.write_memory_debug(0x2105, &[0b1010_1001]);
        let breakdown = emu.get_ppu_mode_breakdown();
        assert_eq!(property(&breakdown, "mode").as_f64(), Some(1.0));
        assert_eq!(property(&breakdown, "bg3Priority").as_bool(), Some(true));
        let sizes: Vec<Option<bool>> = js_sys::Array::from(&property(&breakdown, "bgTileSize16"))
            .iter()
            .map(|size| size.as_bool())
            .collect();
        assert_eq!(
            sizes,
            vec![Some(false), Some(true), Some(false), Some(true)]
        );
        let settings = emu.get_ppu_settings();
        assert_eq!(property(&settings, "bg3_priority").as_bool(), Some(true));
    }

    #[wasm_bindgen_test]
    fn decode_tiles_takes_the_mode7_source_and_16x16_blocks() {
        let mut emu = RetroEmulator::new("snes").unwrap();
        let mode7 = emu.decode_tiles(1, 0, 0, 256, 8, 0);
        assert_eq!(property(&mode7, "width").as_f64(), Some(8.0));
        assert_eq!(property(&mode7, "height").as_f64(), Some(2048.0));
        assert!(
            emu.decode_tiles(1, 0, 0, 1, 4, 0).is_null(),
            "Mode 7 is 8bpp"
        );
        let blocks = emu.decode_tiles(0, 0, 0, 4, 4, 1);
        assert_eq!(property(&blocks, "width").as_f64(), Some(16.0));
        assert_eq!(property(&blocks, "height").as_f64(), Some(64.0));
    }

    /// The options object `setCaptureRecording` takes.
    fn recording_options(entries: &[(&str, bool)]) -> JsValue {
        let options = js_sys::Object::new();
        for (key, on) in entries {
            js_sys::Reflect::set(&options, &JsValue::from_str(key), &JsValue::from_bool(*on))
                .unwrap();
        }
        options.into()
    }

    /// A machine running [`synth_lorom`] with the line table kept, the
    /// display lit over a red backdrop, held at the capture boundary of its
    /// second frame.
    fn recording_a_red_screen() -> RetroEmulator {
        let mut emu = RetroEmulator::new("snes").unwrap();
        emu.load_rom(&synth_lorom("LINE STATES")).unwrap();
        emu.set_capture_recording(recording_options(&[("lineStates", true)]));
        emu.write_cgram(0, &[0x001F]);
        emu.write_memory_debug(0x2100, &[0x0F]);
        emu.run_to_capture_boundary();
        emu.run_to_capture_boundary();
        emu
    }

    #[wasm_bindgen_test]
    fn the_recording_switch_is_a_plain_object_and_a_key_left_out_keeps_its_value() {
        let mut emu = RetroEmulator::new("snes").unwrap();
        let flags = |emu: &RetroEmulator| -> Vec<Option<bool>> {
            let switch = emu.get_capture_recording();
            assert_plain_object(&switch, "lineStates");
            retro_wasm::capture::RECORDING_KEYS
                .iter()
                .map(|key| property(&switch, key).as_bool())
                .collect()
        };
        assert_eq!(flags(&emu), vec![Some(false); 5], "all off at first");
        emu.set_capture_recording(recording_options(&[
            ("lineStates", true),
            ("frameWrites", true),
        ]));
        let on = vec![
            Some(true),
            Some(false),
            Some(false),
            Some(false),
            Some(true),
        ];
        assert_eq!(flags(&emu), on);
        emu.set_capture_recording(recording_options(&[]));
        assert_eq!(flags(&emu), on, "an empty object changes nothing");
        emu.set_capture_recording(JsValue::NULL);
        assert_eq!(flags(&emu), on, "nor does no object");
        emu.set_capture_recording(recording_options(&[("lineStates", false)]));
        let only_writes = vec![
            Some(false),
            Some(false),
            Some(false),
            Some(false),
            Some(true),
        ];
        assert_eq!(flags(&emu), only_writes);
        emu.load_rom(&synth_lorom("KEPT")).unwrap();
        emu.reset();
        assert_eq!(flags(&emu), only_writes, "a load and a reset keep it");
    }

    #[wasm_bindgen_test]
    fn the_line_states_are_a_plain_object_of_copies() {
        let emu = recording_a_red_screen();
        let table = emu.get_line_states();
        assert_plain_object(&table, "values");
        assert_eq!(property(&table, "frame").as_f64(), Some(2.0));
        for key in ["fromFirstLine", "complete", "memoryExact"] {
            assert_eq!(property(&table, key).as_bool(), Some(true), "{key}");
        }
        assert_eq!(property(&table, "rowsPerLine").as_f64(), Some(1.0));
        assert_eq!(property(&table, "firstRowLine").as_f64(), Some(1.0));
        let columns = js_sys::Array::from(&property(&table, "columns"));
        assert_eq!(columns.get(0).as_string().as_deref(), Some("line"));
        let values: js_sys::Uint32Array = property(&table, "values").dyn_into().unwrap();
        assert_eq!(values.length(), columns.length() * 224);
        assert_eq!(values.get_index(0), 1, "the first row is line 1");
        values.set_index(0, 999);
        let again: js_sys::Uint32Array = property(&emu.get_line_states(), "values")
            .dyn_into()
            .unwrap();
        assert_eq!(again.get_index(0), 1, "a copy, not a view");
    }

    #[wasm_bindgen_test]
    fn the_palettes_and_the_picture_come_from_the_line_states() {
        let mut emu = recording_a_red_screen();
        let palettes = emu.get_line_palettes();
        assert_plain_object(&palettes, "paletteOfLine");
        assert_eq!(
            property(&palettes, "coloursPerPalette").as_f64(),
            Some(256.0)
        );
        let colours: js_sys::Uint16Array = property(&palettes, "palettes").dyn_into().unwrap();
        assert_eq!(colours.length(), 256, "one palette for the whole frame");
        assert_eq!(colours.get_index(0), 0x001F);
        let of_line: js_sys::Uint16Array = property(&palettes, "paletteOfLine").dyn_into().unwrap();
        assert_eq!(of_line.length(), 224);
        assert!(of_line.to_vec().iter().all(|&palette| palette == 0));

        let picture = emu.picture_from_line_states();
        assert_plain_object(&picture, "data");
        assert_eq!(property(&picture, "width").as_f64(), Some(256.0));
        assert_eq!(property(&picture, "height").as_f64(), Some(224.0));
        let data: js_sys::Uint8Array = property(&picture, "data").dyn_into().unwrap();
        assert_eq!(data.to_vec(), emu.get_framebuffer_data().to_vec());
        assert_eq!(&data.to_vec()[..4], &[0xFF, 0x00, 0x00, 0xFF]);

        emu.set_capture_recording(recording_options(&[("lineStates", false)]));
        assert!(emu.get_line_states().is_null());
        assert!(emu.get_line_palettes().is_null());
        assert!(emu.picture_from_line_states().is_null());
    }

    #[wasm_bindgen_test]
    fn a_layer_render_says_whether_it_drew_from_the_line_states() {
        let mut emu = recording_a_red_screen();
        let from_line_states =
            |emu: &mut RetroEmulator| property(&emu.render_layer(0), "fromLineStates").as_bool();
        assert_eq!(from_line_states(&mut emu), Some(true));
        // A register written by hand: the registers as they stand show it.
        emu.write_memory_debug(0x2105, &[0x01]);
        assert_eq!(from_line_states(&mut emu), Some(false));
        emu.run_to_capture_boundary();
        assert_eq!(from_line_states(&mut emu), Some(true));
        emu.set_capture_recording(recording_options(&[("lineStates", false)]));
        assert_eq!(from_line_states(&mut emu), Some(false));
    }

    /// The typed array under `key`, which must be a `T`.
    fn typed<T: JsCast>(object: &JsValue, key: &str) -> T {
        property(object, key)
            .dyn_into()
            .unwrap_or_else(|_| panic!("{key} is not the typed array it should be"))
    }

    /// Whether `object` has a property `key`.
    fn has(object: &JsValue, key: &str) -> bool {
        js_sys::Reflect::has(object, &JsValue::from_str(key)).unwrap()
    }

    #[wasm_bindgen_test]
    fn the_pixel_sources_are_a_plain_object_of_copies() {
        let mut idle = RetroEmulator::new("snes").unwrap();
        assert!(
            idle.get_pixel_sources(None).is_null(),
            "no game, no sources"
        );

        let mut emu = recording_a_red_screen();
        let sources = emu.get_pixel_sources(None);
        assert_plain_object(&sources, "layer");
        assert_eq!(property(&sources, "frame").as_f64(), Some(2.0));
        assert_eq!(property(&sources, "width").as_f64(), Some(256.0));
        assert_eq!(property(&sources, "height").as_f64(), Some(224.0));
        assert_eq!(property(&sources, "fromLineStates").as_bool(), Some(true));
        assert_eq!(property(&sources, "matchesPicture").as_bool(), Some(true));
        let pixels = 256 * 224;
        // A red backdrop and no layer: the backdrop drew every pixel, no
        // colour math, no character, no cell.
        let layer: js_sys::Uint16Array = typed(&sources, "layer");
        assert_eq!(layer.length(), pixels);
        assert!(layer.to_vec().iter().all(|&value| value == 0x0070));
        let colour: js_sys::Uint16Array = typed(&sources, "colour");
        assert_eq!(colour.length(), pixels);
        let tile: js_sys::Uint32Array = typed(&sources, "tile");
        assert_eq!(tile.length(), pixels);
        let cell: js_sys::Uint16Array = typed(&sources, "cell");
        assert!(cell.to_vec().iter().all(|&value| value == 0xFFFF));
        assert!(!has(&sources, "subLayer") && !has(&sources, "subColour"));

        layer.set_index(0, 7);
        let again: js_sys::Uint16Array = typed(&emu.get_pixel_sources(None), "layer");
        assert_eq!(again.get_index(0), 0x0070, "a copy, not a view");

        // The layer and the sub screen alone.
        let some = emu.get_pixel_sources(Some(1 | 16));
        assert_plain_object(&some, "subLayer");
        let sub_layer: js_sys::Uint8Array = typed(&some, "subLayer");
        assert_eq!(sub_layer.length(), pixels);
        assert!(
            sub_layer.to_vec().iter().all(|&value| value == 7),
            "no colour math: the line built no sub screen"
        );
        let sub_colour: js_sys::Uint16Array = typed(&some, "subColour");
        assert_eq!(sub_colour.length(), pixels);
        let _: js_sys::Uint16Array = typed(&some, "layer");
        for key in ["colour", "tile", "cell"] {
            assert!(!has(&some, key), "{key} was not asked for");
        }
    }

    /// A machine running [`synth_lorom`] with the line table kept and the
    /// sprites shown over a red backdrop: OAM is all zero, so all 128
    /// sprites sit at (0, 0) as 8x8 sprites of character 0, which is
    /// filled with colour 1 (green in sprite palette 0). Held at the
    /// capture boundary of its second frame.
    fn recording_sprites() -> RetroEmulator {
        let mut emu = RetroEmulator::new("snes").unwrap();
        emu.load_rom(&synth_lorom("LINE SPRITES")).unwrap();
        emu.set_capture_recording(recording_options(&[("lineStates", true)]));
        emu.write_cgram(0, &[0x001F]);
        emu.write_cgram(129, &[0x03E0]);
        // Character 0, 4bpp: plane 0 set on every row, the rest clear.
        emu.write_vram(0, &[0x00FF; 8]);
        emu.write_memory_debug(0x212C, &[0x10]);
        emu.write_memory_debug(0x2100, &[0x0F]);
        emu.run_to_capture_boundary();
        emu.run_to_capture_boundary();
        emu
    }

    #[wasm_bindgen_test]
    fn the_line_sprites_are_a_plain_object_of_copies() {
        let mut idle = RetroEmulator::new("snes").unwrap();
        assert!(idle.get_line_sprites().is_null(), "no game, no sprites");

        let mut emu = recording_sprites();
        let sprites = emu.get_line_sprites();
        assert_plain_object(&sprites, "drawnWidth");
        assert_eq!(property(&sprites, "frame").as_f64(), Some(2.0));
        assert_eq!(property(&sprites, "fromLineStates").as_bool(), Some(true));
        let lines: js_sys::Uint16Array = typed(&sprites, "lines");
        assert_eq!(lines.length(), 224 * 4);
        let slots: js_sys::Uint8Array = typed(&sprites, "slots");
        let fates: js_sys::Uint8Array = typed(&sprites, "fates");
        let x: js_sys::Int16Array = typed(&sprites, "x");
        let width: js_sys::Uint8Array = typed(&sprites, "width");
        let drawn: js_sys::Uint8Array = typed(&sprites, "drawnWidth");
        // Lines 1-8 meet all 128 sprites: 32 kept and drawn whole, 96 over
        // the range limit; the other lines meet none.
        let entries = 8 * 128;
        for column in [
            slots.length(),
            fates.length(),
            x.length(),
            width.length(),
            drawn.length(),
        ] {
            assert_eq!(column, entries);
        }
        let lines = lines.to_vec();
        assert_eq!(&lines[..4], &[1, 0, 128, 1], "line 1, over range");
        assert_eq!(&lines[8 * 4..9 * 4], &[9, entries as u16, 0, 0]);
        let fates = fates.to_vec();
        assert!(fates[..32].iter().all(|&fate| fate == 0));
        assert!(fates[32..128].iter().all(|&fate| fate == 3));
        assert_eq!(&slots.to_vec()[..3], &[0, 1, 2]);
        assert!(drawn.to_vec()[..32].iter().all(|&pixels| pixels == 8));
        assert!(width.to_vec().iter().all(|&pixels| pixels == 8));

        x.set_index(0, 99);
        let again: js_sys::Int16Array = typed(&emu.get_line_sprites(), "x");
        assert_eq!(again.get_index(0), 0, "a copy, not a view");
    }

    #[wasm_bindgen_test]
    fn a_sprite_render_draws_the_chosen_sprites_after_priority() {
        let mut idle = RetroEmulator::new("snes").unwrap();
        assert!(idle.render_sprites(&[1]).is_null(), "no game, no render");

        let mut emu = recording_sprites();
        let alpha_at = |image: &JsValue, x: u32, y: u32| -> u8 {
            let data: js_sys::Uint8Array = typed(image, "data");
            data.get_index((y * 256 + x) * 4 + 3)
        };
        let every = emu.render_sprites(&[1u8; 128]);
        assert_plain_object(&every, "fromLineStates");
        assert_eq!(property(&every, "width").as_f64(), Some(256.0));
        assert_eq!(property(&every, "height").as_f64(), Some(224.0));
        assert_eq!(property(&every, "fromLineStates").as_bool(), Some(true));
        let data: js_sys::Uint8Array = typed(&every, "data");
        assert_eq!(data.length(), 256 * 224 * 4);
        assert_eq!(alpha_at(&every, 0, 0), 0xFF);
        assert_eq!(alpha_at(&every, 8, 0), 0);
        assert_eq!(alpha_at(&every, 0, 8), 0);
        // Slot 0 is in front of the others at (0, 0): chosen alone it
        // shows; slot 1 alone shows nothing, being behind slot 0.
        assert_eq!(alpha_at(&emu.render_sprites(&[1]), 0, 0), 0xFF);
        assert_eq!(alpha_at(&emu.render_sprites(&[0, 1]), 0, 0), 0);
        assert_eq!(alpha_at(&emu.render_sprites(&[]), 0, 0), 0);
    }

    #[wasm_bindgen_test]
    fn the_window_masks_are_a_plain_object_of_copies() {
        let idle = RetroEmulator::new("snes").unwrap();
        assert!(idle.get_window_masks().is_null(), "no game, no masks");

        let emu = recording_a_red_screen();
        let masks = emu.get_window_masks();
        assert_plain_object(&masks, "masks");
        assert_eq!(property(&masks, "frame").as_f64(), Some(2.0));
        assert_eq!(property(&masks, "fromLineStates").as_bool(), Some(true));
        let lines: js_sys::Uint16Array = typed(&masks, "lines");
        assert_eq!(lines.length(), 224);
        assert_eq!(lines.get_index(0), 1);
        let bytes: js_sys::Uint8Array = typed(&masks, "masks");
        assert_eq!(bytes.length(), 224 * 8 * 32);
        // No window on: the six regions are empty; the main screen is kept
        // and colour math allowed everywhere.
        let line = bytes.to_vec()[..256].to_vec();
        assert!(line[..6 * 32].iter().all(|&byte| byte == 0));
        assert!(line[6 * 32..].iter().all(|&byte| byte == 0xFF));
    }

    #[wasm_bindgen_test]
    fn the_offset_per_tile_columns_are_a_plain_object_of_copies() {
        let idle = RetroEmulator::new("snes").unwrap();
        assert!(idle.get_offset_per_tile().is_null(), "no game, no columns");

        let mut emu = recording_a_red_screen();
        let table = emu.get_offset_per_tile();
        assert_plain_object(&table, "replaced");
        assert_eq!(property(&table, "fromLineStates").as_bool(), Some(true));
        assert_eq!(property(&table, "columns").as_f64(), Some(33.0));
        let lines: js_sys::Uint16Array = typed(&table, "lines");
        assert_eq!(lines.length(), 0, "Mode 0 has no offset-per-tile");

        // Mode 2 from the next frame on.
        emu.write_memory_debug(0x2105, &[0x02]);
        emu.run_to_capture_boundary();
        let table = emu.get_offset_per_tile();
        assert_eq!(property(&table, "frame").as_f64(), Some(3.0));
        let lines: js_sys::Uint16Array = typed(&table, "lines");
        assert_eq!(lines.length(), 224);
        let scroll: js_sys::Uint16Array = typed(&table, "scroll");
        assert_eq!(scroll.length(), 224 * 2 * 33 * 2);
        let replaced: js_sys::Uint8Array = typed(&table, "replaced");
        assert_eq!(replaced.length(), 224 * 2 * 33);
        // No scroll: column 32 starts at dot 256, past the line.
        assert_eq!(replaced.get_index(32), 0x80);
        assert_eq!(replaced.get_index(0), 0);
    }

    #[wasm_bindgen_test]
    fn the_video_memory_sources_are_a_plain_object_of_copies() {
        let idle = RetroEmulator::new("snes").unwrap();
        assert!(
            idle.get_video_memory_sources().is_null(),
            "not recorded, no sources"
        );

        let mut emu = RetroEmulator::new("snes").unwrap();
        emu.load_rom(&synth_lorom("VRAM SOURCES")).unwrap();
        emu.set_capture_recording(recording_options(&[("videoMemorySources", true)]));
        assert_eq!(
            property(&emu.get_capture_recording(), "videoMemorySources").as_bool(),
            Some(true)
        );
        let sources = emu.get_video_memory_sources();
        assert_plain_object(&sources, "origins");
        let origins: js_sys::Uint32Array = typed(&sources, "origins");
        let frames: js_sys::Uint32Array = typed(&sources, "frames");
        assert_eq!(origins.length(), 0x1_0000);
        assert_eq!(frames.length(), 0x1_0000);
        assert!(
            origins.to_vec().iter().all(|&origin| origin == 0),
            "empty when turned on"
        );

        // A debugger write straight into VRAM, then one through the port
        // (the reset state is forced blank: the port is open).
        emu.write_vram(0x10, &[1, 2]);
        emu.write_memory_debug(0x2115, &[0x80]);
        emu.write_memory_debug(0x2116, &[0x00, 0x10]);
        emu.write_memory_debug(0x2118, &[0xAA]);
        let sources = emu.get_video_memory_sources();
        let origins: js_sys::Uint32Array = typed(&sources, "origins");
        let frames: js_sys::Uint32Array = typed(&sources, "frames");
        for byte in [0x20, 0x21, 0x2000] {
            assert_eq!(
                origins.get_index(byte),
                0x4000_0000,
                "byte {byte:#x}: the debugger"
            );
            assert_eq!(frames.get_index(byte), 1, "byte {byte:#x}: frame 1");
        }
        assert_eq!(origins.get_index(0x2001), 0, "not written");

        origins.set_index(0x20, 7);
        let again: js_sys::Uint32Array = typed(&emu.get_video_memory_sources(), "origins");
        assert_eq!(again.get_index(0x20), 0x4000_0000, "a copy, not a view");
    }

    #[wasm_bindgen_test]
    fn resolving_sources_answers_an_int32_array_of_rom_offsets_and_codes() {
        let mut emu = RetroEmulator::new("snes").unwrap();
        emu.load_rom(&synth_lorom("RESOLVE SOURCES")).unwrap();
        let addresses = [
            0x00_8000u32,
            0x81_8123,
            0x7E_0040,
            0x00_1FFF,
            0x00_2100,
            0x40_0000,
        ];
        let resolved = emu.resolve_sources(&addresses);
        let value: JsValue = resolved.clone().into();
        assert!(value.is_instance_of::<js_sys::Int32Array>());
        assert_eq!(
            resolved.to_vec(),
            vec![0, 0x8123, -2, -2, -1, -1],
            "ROM, its mirror, work RAM, its mirror, the B bus, open bus"
        );
        resolved.set_index(0, 99);
        assert_eq!(emu.resolve_sources(&addresses).get_index(0), 0, "a copy");
        assert_eq!(emu.resolve_sources(&[]).length(), 0);
    }

    #[wasm_bindgen_test]
    fn the_frame_writes_are_a_plain_object_of_copies_emptied_by_a_take() {
        let mut idle = RetroEmulator::new("snes").unwrap();
        assert!(
            idle.take_frame_writes().is_null(),
            "not recorded, no writes"
        );

        let mut emu = RetroEmulator::new("snes").unwrap();
        emu.load_rom(&synth_lorom("FRAME WRITES")).unwrap();
        emu.set_capture_recording(recording_options(&[("frameWrites", true)]));
        // Two colours from work RAM into CGRAM by a DMA on channel 0, run at
        // the end of the next instruction.
        emu.write_memory_debug(0x7E_0000, &[0x1F, 0x00, 0xE0, 0x03]);
        emu.write_memory_debug(0x2121, &[0x00]);
        emu.write_memory_debug(0x4300, &[0x00, 0x22, 0x00, 0x00, 0x7E, 0x04, 0x00]);
        emu.write_memory_debug(0x420B, &[0x01]);
        let _ = emu.step();

        let writes = emu.take_frame_writes();
        assert_plain_object(&writes, "records");
        assert_eq!(property(&writes, "wordsPerRecord").as_f64(), Some(8.0));
        assert_eq!(property(&writes, "dropped").as_f64(), Some(0.0));
        let records: js_sys::Uint32Array = typed(&writes, "records");
        assert_eq!(records.length(), 8, "one DMA block");
        let words = records.to_vec();
        assert_eq!(words[0], 1, "a DMA block on channel 0, A to B, unit 0");
        assert_eq!(words[1], 1, "frame 1");
        assert_eq!(words[3], 0x7E_0000);
        assert_eq!(words[4], 0x22 | (4 << 8));
        assert_eq!((words[5], words[6]), (0, 1), "colours 0 to 1");

        records.set_index(0, 0);
        let again = emu.take_frame_writes();
        let records: js_sys::Uint32Array = typed(&again, "records");
        assert_eq!(records.length(), 0, "a take empties the log");
    }

    #[wasm_bindgen_test]
    fn the_event_catalogue_lists_the_video_memory_events() {
        let mut emu = RetroEmulator::new("snes").unwrap();
        let events = js_sys::Array::from(&emu.get_available_events());
        let codes: Vec<f64> = events
            .iter()
            .map(|event| property(&event, "code").as_f64().unwrap())
            .collect();
        for code in [10.0, 11.0, 12.0] {
            assert!(codes.contains(&code), "event {code} listed");
        }
    }

    #[wasm_bindgen_test]
    fn write_oam_reads_back_and_moves_a_sprite() {
        let mut emu = RetroEmulator::new("snes").unwrap();
        // The last byte of the high table, then on round to sprite 0's X.
        emu.write_oam(543, &[0xAA, 100]);
        let oam = emu.get_oam();
        assert_eq!(oam[543], 0xAA);
        assert_eq!(oam[0], 100);

        let sprites: Vec<retro_core::video::SpriteData> =
            serde_wasm_bindgen::from_value(emu.get_sprite_data()).unwrap();
        assert_eq!(sprites[0].x, 100, "sprite 0 is at x 100");

        let long: Vec<u8> = (0..600).map(|i| if i < 544 { 1 } else { 2 }).collect();
        emu.write_oam(10, &long);
        assert!(
            emu.get_oam().iter().all(|&byte| byte == 1),
            "a buffer longer than OAM is cut to it"
        );
    }

    /// The few 65C816 instructions the capture program needs, assembled in
    /// emulation mode, the code starting at `$00:8000`.
    #[derive(Default)]
    struct Code(Vec<u8>);

    impl Code {
        /// The address of the next instruction.
        fn here(&self) -> u16 {
            0x8000 + self.0.len() as u16
        }

        /// Append `bytes` as they are.
        fn op(&mut self, bytes: &[u8]) -> &mut Self {
            self.0.extend_from_slice(bytes);
            self
        }

        /// `LDA #value`, `STA address`.
        fn store(&mut self, address: u16, value: u8) -> &mut Self {
            let [low, high] = address.to_le_bytes();
            self.op(&[0xA9, value, 0x8D, low, high])
        }

        /// `STZ address`.
        fn zero(&mut self, address: u16) -> &mut Self {
            let [low, high] = address.to_le_bytes();
            self.op(&[0x9C, low, high])
        }

        /// `LDA from`, `STA to`.
        fn copy(&mut self, from: u16, to: u16) -> &mut Self {
            let [from_low, from_high] = from.to_le_bytes();
            let [to_low, to_high] = to.to_le_bytes();
            self.op(&[0xAD, from_low, from_high, 0x8D, to_low, to_high])
        }

        /// A DMA channel's A-bus address, `$00:address`, into the three
        /// registers from `register` on.
        fn source(&mut self, register: u16, address: u16) -> &mut Self {
            let [low, high] = address.to_le_bytes();
            self.store(register, low)
                .store(register + 1, high)
                .zero(register + 2)
        }

        /// A branch, `opcode`, to `target`.
        fn branch(&mut self, opcode: u8, target: u16) -> &mut Self {
            let offset = i32::from(target) - i32::from(self.here()) - 2;
            self.op(&[opcode, offset as i8 as u8])
        }
    }

    /// Where the capture program's tables sit in bank `$00`.
    const PROGRAM_PALETTE: u16 = 0x9000;
    const PROGRAM_CHARACTERS: u16 = 0x9020;
    const PROGRAM_MAP: u16 = 0x9060;
    const PROGRAM_SCROLLS: u16 = 0x90A0;

    /// A program that draws, moves a sprite and writes video memory. It
    /// loads sixteen colours (for the backgrounds, then for the sprites), two
    /// characters and the first row of BG1's map by DMA, shows BG1 in Mode 1
    /// and the sprites, and scrolls BG1 line by line by HDMA (32 lines at 4,
    /// then 16 lines at 0, 2, ..., 30). In each vertical blank it counts a
    /// frame in work RAM and, through the ports, moves sprite 0 to that x,
    /// writes the count into the map entry at word `$0420` and into colour 2.
    fn capture_program_rom() -> Vec<u8> {
        let mut code = Code::default();
        code.op(&[0x78]); // SEI
        code.zero(0x2121).store(0x4300, 0x00).store(0x4301, 0x22);
        code.source(0x4302, PROGRAM_PALETTE)
            .store(0x4305, 32)
            .zero(0x4306)
            .store(0x420B, 0x01);
        code.store(0x2121, 0x80)
            .source(0x4302, PROGRAM_PALETTE)
            .store(0x4305, 32)
            .store(0x420B, 0x01);
        code.store(0x2115, 0x80).zero(0x2116).zero(0x2117);
        code.store(0x4300, 0x01).store(0x4301, 0x18);
        code.source(0x4302, PROGRAM_CHARACTERS)
            .store(0x4305, 64)
            .store(0x420B, 0x01);
        code.zero(0x2116).store(0x2117, 0x04);
        code.source(0x4302, PROGRAM_MAP)
            .store(0x4305, 64)
            .store(0x420B, 0x01);
        code.store(0x2105, 0x01)
            .store(0x2107, 0x04)
            .zero(0x210B)
            .store(0x212C, 0x11)
            .zero(0x2101);
        // Sprite 0 at (16, 32), character 1, priority 3.
        code.zero(0x2102).zero(0x2103);
        code.store(0x2104, 16)
            .store(0x2104, 32)
            .store(0x2104, 0x01)
            .store(0x2104, 0x30);
        code.store(0x4310, 0x02)
            .store(0x4311, 0x0D)
            .source(0x4312, PROGRAM_SCROLLS)
            .store(0x420C, 0x02);
        code.store(0x2100, 0x0F);
        let wait_for_blank = code.here();
        code.op(&[0xAD, 0x12, 0x42]).branch(0x10, wait_for_blank); // LDA $4212, BPL
        code.op(&[0xEE, 0x00, 0x00]); // INC $0000
        code.zero(0x2102)
            .zero(0x2103)
            .copy(0x0000, 0x2104)
            .store(0x2104, 32);
        code.store(0x2116, 0x20)
            .store(0x2117, 0x04)
            .copy(0x0000, 0x2118)
            .zero(0x2119);
        code.store(0x2121, 0x02).copy(0x0000, 0x2122).zero(0x2122);
        let wait_for_lines = code.here();
        code.op(&[0xAD, 0x12, 0x42]).branch(0x30, wait_for_lines); // LDA $4212, BMI
        code.branch(0x80, wait_for_blank); // BRA

        let palette: Vec<u8> = [
            0x2800u16, 0x03FF, 0x7C1F, 0x001F, 0x03E0, 0x7C00, 0x7FFF, 0x0421, 0x0842, 0x0C63,
            0x1084, 0x14A5, 0x18C6, 0x1CE7, 0x2108, 0x2529,
        ]
        .iter()
        .flat_map(|colour| colour.to_le_bytes())
        .collect();
        let mut characters = [0u8; 64];
        for row in 0..8 {
            characters[2 * row] = 0xFF >> row;
            characters[2 * row + 1] = 1 << row;
            characters[32 + 2 * row] = if row % 2 == 0 { 0xAA } else { 0x55 };
            characters[32 + 2 * row + 1] = 0xF0;
            characters[48 + 2 * row] = 0x0F;
        }
        let map: Vec<u8> = (0u16..32)
            .flat_map(|entry| {
                let word = (entry & 1)
                    | (((entry >> 1) & 7) << 10)
                    | (((entry >> 2) & 1) << 13)
                    | (((entry >> 3) & 1) << 14)
                    | (((entry >> 4) & 1) << 15);
                word.to_le_bytes()
            })
            .collect();
        let mut scrolls = vec![0x20, 0x04, 0x00, 0x90];
        for line in 0..16u8 {
            scrolls.extend([2 * line, 0x00]);
        }
        scrolls.push(0x00);
        lorom_running(
            "CAPTURE PROGRAM",
            &code.0,
            &[
                (PROGRAM_PALETTE, &palette),
                (PROGRAM_CHARACTERS, &characters),
                (PROGRAM_MAP, &map),
                (PROGRAM_SCROLLS, &scrolls),
            ],
        )
    }

    /// The capture program with every recorder armed before its first frame,
    /// held at the capture boundary of its third frame.
    fn running_the_capture_program() -> RetroEmulator {
        let mut emu = RetroEmulator::new("snes").unwrap();
        emu.load_rom(&capture_program_rom()).unwrap();
        emu.set_capture_recording(recording_options(&[
            ("lineStates", true),
            ("pixelSources", true),
            ("lineSprites", true),
            ("videoMemorySources", true),
            ("frameWrites", true),
        ]));
        for _ in 0..3 {
            emu.run_to_capture_boundary();
        }
        emu
    }

    /// What `shape` says of each value.
    fn shapes(values: &[JsValue]) -> Vec<String> {
        values.iter().map(retro_wasm::test_support::shape).collect()
    }

    #[wasm_bindgen_test]
    fn the_capture_boundary_reads_as_before() {
        let mut emu = RetroEmulator::new("snes").unwrap();
        emu.load_rom(&capture_program_rom()).unwrap();
        let mut held = vec![JsValue::from_bool(emu.at_capture_boundary())];
        emu.run_to_capture_boundary();
        held.push(JsValue::from_bool(emu.at_capture_boundary()));
        emu.run_frame();
        held.push(JsValue::from_bool(emu.at_capture_boundary()));
        let expected = ["false", "true", "false"];
        assert_eq!(shapes(&held), expected);
    }

    #[wasm_bindgen_test]
    fn the_frame_snapshot_reads_as_before() {
        let idle = RetroEmulator::new("snes").unwrap();
        let emu = running_the_capture_program();
        let expected = [
            concat!(
                "{atBoundary: false, frame: 0, framebuffer: Uint8Array(len 229376, ",
                "fnv1a64 0xc9b1e505f9a82325), height: 224, oam: Uint8Array(len 544, ",
                "fnv1a64 0x9986ba9af2021da5), palette: Uint16Array(len 256, ",
                "fnv1a64 0x7da144b97d054b25), ppuSettings: {addsub_mode: false, ",
                "bg3_priority: false, bg_main_enable: [false, false, false, false], ",
                "bg_main_window_disable: [false, false, false, false], ",
                "bg_mosaic_enable: [false, false, false, false], bg_sub_enable: [false, ",
                "false, false, false], bg_sub_window_disable: [false, false, false, ",
                "false], bg_tile_size: [false, false, false, false], bgmode: 0, ",
                "col_main_mask: 0, col_sub_mask: 0, color_math_back_enable: false, ",
                "color_math_bg_enable: [false, false, false, false], ",
                "color_math_halve: false, color_math_oam_enable: false, ",
                "color_math_subtract: false, direct_color: false, display_brightness: 0, ",
                "display_disable: true, field: false, fixed_color: 0, interlace: false, ",
                "mode7_centre: [0, 0], mode7_extbg: false, mode7_hflip: false, ",
                "mode7_hoffset: 0, mode7_matrix: [0, 0, 0, 0], mode7_repeat: 0, ",
                "mode7_vflip: false, mode7_voffset: 0, mosaic_size: 1, obj_base_size: 0, ",
                "obj_interlace: false, obj_main_enable: false, ",
                "obj_main_window_disable: false, obj_name_select: 0, ",
                "obj_sub_enable: false, obj_sub_window_disable: false, ",
                "obj_tiledata_addr: 0, overscan: false, pseudo_hires: false, ",
                "sprite_tiles: {first_table_base_word: 0, large_height: 16, ",
                "large_width: 16, second_table_base_word: 4096, small_height: 8, ",
                "small_width: 8}, window_combine: [0, 0, 0, 0, 0, 0], ",
                "window_enable: [{w1_enable: false, w1_invert: false, w2_enable: false, ",
                "w2_invert: false}, {w1_enable: false, w1_invert: false, ",
                "w2_enable: false, w2_invert: false}, {w1_enable: false, ",
                "w1_invert: false, w2_enable: false, w2_invert: false}, ",
                "{w1_enable: false, w1_invert: false, w2_enable: false, ",
                "w2_invert: false}, {w1_enable: false, w1_invert: false, ",
                "w2_enable: false, w2_invert: false}, {w1_enable: false, ",
                "w1_invert: false, w2_enable: false, w2_invert: false}], ",
                "window_one_left: 0, window_one_right: 0, window_two_left: 0, ",
                "window_two_right: 0}, registers: [{description: \"BG mode + tile size\", ",
                "index: 8453, name: \"BGMODE\", value: 0}, ",
                "{description: \"Main screen designation\", index: 8492, name: \"TM\", ",
                "value: 0}, {description: \"Sub screen designation\", index: 8493, ",
                "name: \"TS\", value: 0}, {description: \"Mode 7 settings\", index: 8474, ",
                "name: \"M7SEL\", value: 0}, {description: \"Mode 7 matrix A\", index: 8475, ",
                "name: \"M7A\", value: 0}, {description: \"Mode 7 matrix B\", index: 8476, ",
                "name: \"M7B\", value: 0}, {description: \"Mode 7 matrix C\", index: 8477, ",
                "name: \"M7C\", value: 0}, {description: \"Mode 7 matrix D\", index: 8478, ",
                "name: \"M7D\", value: 0}, {description: \"Mode 7 centre X\", index: 8479, ",
                "name: \"M7X\", value: 0}, {description: \"Mode 7 centre Y\", index: 8480, ",
                "name: \"M7Y\", value: 0}, {description: \"BG1 horizontal scroll\", ",
                "index: 8461, name: \"BG1HOFS\", value: 0}, ",
                "{description: \"BG1 vertical scroll\", index: 8462, name: \"BG1VOFS\", ",
                "value: 0}], vram: Uint8Array(len 65536, fnv1a64 0xeb05052ea5b62325), ",
                "width: 256}",
            ),
            concat!(
                "{atBoundary: true, frame: 3, framebuffer: Uint8Array(len 229376, ",
                "fnv1a64 0x75f379eed654dafa), height: 224, oam: Uint8Array(len 544, ",
                "fnv1a64 0xb3c3b956cfe107de), palette: Uint16Array(len 256, ",
                "fnv1a64 0x82bce97466c89cfc), ppuSettings: {addsub_mode: false, ",
                "bg3_priority: false, bg_main_enable: [true, false, false, false], ",
                "bg_main_window_disable: [false, false, false, false], ",
                "bg_mosaic_enable: [false, false, false, false], bg_sub_enable: [false, ",
                "false, false, false], bg_sub_window_disable: [false, false, false, ",
                "false], bg_tile_size: [false, false, false, false], bgmode: 1, ",
                "col_main_mask: 0, col_sub_mask: 0, color_math_back_enable: false, ",
                "color_math_bg_enable: [false, false, false, false], ",
                "color_math_halve: false, color_math_oam_enable: false, ",
                "color_math_subtract: false, direct_color: false, ",
                "display_brightness: 15, display_disable: false, field: false, ",
                "fixed_color: 0, interlace: false, mode7_centre: [0, 0], ",
                "mode7_extbg: false, mode7_hflip: false, mode7_hoffset: 30, ",
                "mode7_matrix: [0, 0, 0, 0], mode7_repeat: 0, mode7_vflip: false, ",
                "mode7_voffset: 0, mosaic_size: 1, obj_base_size: 0, ",
                "obj_interlace: false, obj_main_enable: true, ",
                "obj_main_window_disable: false, obj_name_select: 0, ",
                "obj_sub_enable: false, obj_sub_window_disable: false, ",
                "obj_tiledata_addr: 0, overscan: false, pseudo_hires: false, ",
                "sprite_tiles: {first_table_base_word: 0, large_height: 16, ",
                "large_width: 16, second_table_base_word: 4096, small_height: 8, ",
                "small_width: 8}, window_combine: [0, 0, 0, 0, 0, 0], ",
                "window_enable: [{w1_enable: false, w1_invert: false, w2_enable: false, ",
                "w2_invert: false}, {w1_enable: false, w1_invert: false, ",
                "w2_enable: false, w2_invert: false}, {w1_enable: false, ",
                "w1_invert: false, w2_enable: false, w2_invert: false}, ",
                "{w1_enable: false, w1_invert: false, w2_enable: false, ",
                "w2_invert: false}, {w1_enable: false, w1_invert: false, ",
                "w2_enable: false, w2_invert: false}, {w1_enable: false, ",
                "w1_invert: false, w2_enable: false, w2_invert: false}], ",
                "window_one_left: 0, window_one_right: 0, window_two_left: 0, ",
                "window_two_right: 0}, registers: [{description: \"BG mode + tile size\", ",
                "index: 8453, name: \"BGMODE\", value: 1}, ",
                "{description: \"Main screen designation\", index: 8492, name: \"TM\", ",
                "value: 17}, {description: \"Sub screen designation\", index: 8493, ",
                "name: \"TS\", value: 0}, {description: \"Mode 7 settings\", index: 8474, ",
                "name: \"M7SEL\", value: 0}, {description: \"Mode 7 matrix A\", index: 8475, ",
                "name: \"M7A\", value: 0}, {description: \"Mode 7 matrix B\", index: 8476, ",
                "name: \"M7B\", value: 0}, {description: \"Mode 7 matrix C\", index: 8477, ",
                "name: \"M7C\", value: 0}, {description: \"Mode 7 matrix D\", index: 8478, ",
                "name: \"M7D\", value: 0}, {description: \"Mode 7 centre X\", index: 8479, ",
                "name: \"M7X\", value: 0}, {description: \"Mode 7 centre Y\", index: 8480, ",
                "name: \"M7Y\", value: 0}, {description: \"BG1 horizontal scroll\", ",
                "index: 8461, name: \"BG1HOFS\", value: 30}, ",
                "{description: \"BG1 vertical scroll\", index: 8462, name: \"BG1VOFS\", ",
                "value: 0}], vram: Uint8Array(len 65536, fnv1a64 0x15727000ffe79428), ",
                "width: 256}",
            ),
        ];
        assert_eq!(
            shapes(&[idle.capture_frame_snapshot(), emu.capture_frame_snapshot()]),
            expected
        );
    }

    #[wasm_bindgen_test]
    fn the_layer_renders_read_as_before() {
        let mut emu = running_the_capture_program();
        let layers: Vec<JsValue> = (0..=5).map(|layer| emu.render_layer(layer)).collect();
        let expected = [
            concat!(
                "{data: Uint8Array(len 229376, fnv1a64 0xc995b76295b606ea), ",
                "fromLineStates: true, height: 224, width: 256}",
            ),
            concat!(
                "{data: Uint8Array(len 229376, fnv1a64 0xb293664dc5b90c09), ",
                "fromLineStates: true, height: 224, width: 256}",
            ),
            concat!(
                "{data: Uint8Array(len 229376, fnv1a64 0x85068eaa1516ba85), ",
                "fromLineStates: true, height: 224, width: 256}",
            ),
            concat!(
                "{data: Uint8Array(len 229376, fnv1a64 0xc9b1e505f9a82325), ",
                "fromLineStates: true, height: 224, width: 256}",
            ),
            concat!(
                "{data: Uint8Array(len 229376, fnv1a64 0x59b321ddc659a405), ",
                "fromLineStates: true, height: 224, width: 256}",
            ),
            "null",
        ];
        assert_eq!(shapes(&layers), expected);
    }

    #[wasm_bindgen_test]
    fn the_recording_reads_as_before() {
        let mut emu = RetroEmulator::new("snes").unwrap();
        let mut read = vec![emu.get_capture_recording()];
        emu.set_capture_recording(recording_options(&[
            ("lineStates", true),
            ("videoMemorySources", true),
        ]));
        read.push(emu.get_capture_recording());
        emu.set_capture_recording(recording_options(&[
            ("lineStates", false),
            ("frameWrites", true),
        ]));
        read.push(emu.get_capture_recording());
        let expected = [
            concat!(
                "{frameWrites: false, lineSprites: false, lineStates: false, ",
                "pixelSources: false, videoMemorySources: false}",
            ),
            concat!(
                "{frameWrites: false, lineSprites: false, lineStates: true, ",
                "pixelSources: false, videoMemorySources: true}",
            ),
            concat!(
                "{frameWrites: true, lineSprites: false, lineStates: false, ",
                "pixelSources: false, videoMemorySources: true}",
            ),
        ];
        assert_eq!(shapes(&read), expected);
    }

    #[wasm_bindgen_test]
    fn the_line_states_read_as_before() {
        let idle = RetroEmulator::new("snes").unwrap();
        let emu = running_the_capture_program();
        let expected = [
            "null",
            concat!(
                "{columns: [\"line\", \"display_disable\", \"display_brightness\", ",
                "\"obj_base_size\", \"obj_name_select\", \"obj_tiledata_addr\", ",
                "\"oam_first_sprite\", \"bgmode\", \"bg3_priority\", \"bg_tile_size[0]\", ",
                "\"bg_tile_size[1]\", \"bg_tile_size[2]\", \"bg_tile_size[3]\", \"mosaic_size\", ",
                "\"bg_mosaic_enable[0]\", \"bg_mosaic_enable[1]\", \"bg_mosaic_enable[2]\", ",
                "\"bg_mosaic_enable[3]\", \"bg_screen_addr[0]\", \"bg_screen_addr[1]\", ",
                "\"bg_screen_addr[2]\", \"bg_screen_addr[3]\", \"bg_screen_size[0]\", ",
                "\"bg_screen_size[1]\", \"bg_screen_size[2]\", \"bg_screen_size[3]\", ",
                "\"bg_tiledata_addr[0]\", \"bg_tiledata_addr[1]\", \"bg_tiledata_addr[2]\", ",
                "\"bg_tiledata_addr[3]\", \"bg_hoffset[0]\", \"bg_hoffset[1]\", ",
                "\"bg_hoffset[2]\", \"bg_hoffset[3]\", \"bg_voffset[0]\", \"bg_voffset[1]\", ",
                "\"bg_voffset[2]\", \"bg_voffset[3]\", \"mode7_repeat\", \"mode7_vflip\", ",
                "\"mode7_hflip\", \"mode7_matrix_a\", \"mode7_matrix_b\", \"mode7_matrix_c\", ",
                "\"mode7_matrix_d\", \"mode7_centre_x\", \"mode7_centre_y\", \"mode7_hoffset\", ",
                "\"mode7_voffset\", \"mode7_extbg\", \"window_enable[0].w1_enable\", ",
                "\"window_enable[0].w1_invert\", \"window_enable[0].w2_enable\", ",
                "\"window_enable[0].w2_invert\", \"window_enable[1].w1_enable\", ",
                "\"window_enable[1].w1_invert\", \"window_enable[1].w2_enable\", ",
                "\"window_enable[1].w2_invert\", \"window_enable[2].w1_enable\", ",
                "\"window_enable[2].w1_invert\", \"window_enable[2].w2_enable\", ",
                "\"window_enable[2].w2_invert\", \"window_enable[3].w1_enable\", ",
                "\"window_enable[3].w1_invert\", \"window_enable[3].w2_enable\", ",
                "\"window_enable[3].w2_invert\", \"window_enable[4].w1_enable\", ",
                "\"window_enable[4].w1_invert\", \"window_enable[4].w2_enable\", ",
                "\"window_enable[4].w2_invert\", \"window_enable[5].w1_enable\", ",
                "\"window_enable[5].w1_invert\", \"window_enable[5].w2_enable\", ",
                "\"window_enable[5].w2_invert\", \"window_one_left\", \"window_one_right\", ",
                "\"window_two_left\", \"window_two_right\", \"window_combine[0]\", ",
                "\"window_combine[1]\", \"window_combine[2]\", \"window_combine[3]\", ",
                "\"window_combine[4]\", \"window_combine[5]\", \"bg_main_enable[0]\", ",
                "\"bg_main_enable[1]\", \"bg_main_enable[2]\", \"bg_main_enable[3]\", ",
                "\"obj_main_enable\", \"bg_sub_enable[0]\", \"bg_sub_enable[1]\", ",
                "\"bg_sub_enable[2]\", \"bg_sub_enable[3]\", \"obj_sub_enable\", ",
                "\"bg_main_window_disable[0]\", \"bg_main_window_disable[1]\", ",
                "\"bg_main_window_disable[2]\", \"bg_main_window_disable[3]\", ",
                "\"obj_main_window_disable\", \"bg_sub_window_disable[0]\", ",
                "\"bg_sub_window_disable[1]\", \"bg_sub_window_disable[2]\", ",
                "\"bg_sub_window_disable[3]\", \"obj_sub_window_disable\", \"col_main_mask\", ",
                "\"col_sub_mask\", \"addsub_mode\", \"direct_color\", \"color_math_subtract\", ",
                "\"color_math_halve\", \"color_math_back_enable\", \"color_math_oam_enable\", ",
                "\"color_math_bg_enable[0]\", \"color_math_bg_enable[1]\", ",
                "\"color_math_bg_enable[2]\", \"color_math_bg_enable[3]\", \"fixed_color\", ",
                "\"pseudo_hires\", \"overscan\", \"obj_interlace\", \"interlace\", \"field\"], ",
                "complete: true, firstRowLine: 1, frame: 3, fromFirstLine: true, ",
                "memoryExact: true, rowsPerLine: 1, values: Uint32Array(len 27328, ",
                "fnv1a64 0x512f8064a14a9c85)}",
            ),
        ];
        assert_eq!(
            shapes(&[idle.get_line_states(), emu.get_line_states()]),
            expected
        );
    }

    #[wasm_bindgen_test]
    fn the_line_palettes_read_as_before() {
        let idle = RetroEmulator::new("snes").unwrap();
        let emu = running_the_capture_program();
        let expected = [
            "null",
            concat!(
                "{coloursPerPalette: 256, paletteOfLine: Uint16Array(len 224, ",
                "fnv1a64 0xddedd579bea76625), palettes: Uint16Array(len 256, ",
                "fnv1a64 0x82bce97466c89cfc)}",
            ),
        ];
        assert_eq!(
            shapes(&[idle.get_line_palettes(), emu.get_line_palettes()]),
            expected
        );
    }

    #[wasm_bindgen_test]
    fn the_picture_from_the_line_states_reads_as_before() {
        let mut idle = RetroEmulator::new("snes").unwrap();
        let mut emu = running_the_capture_program();
        let expected = [
            "null",
            concat!(
                "{data: Uint8Array(len 229376, fnv1a64 0x75f379eed654dafa), height: 224, ",
                "width: 256}",
            ),
        ];
        assert_eq!(
            shapes(&[
                idle.picture_from_line_states(),
                emu.picture_from_line_states()
            ]),
            expected
        );
    }

    #[wasm_bindgen_test]
    fn the_pixel_sources_read_as_before() {
        let mut idle = RetroEmulator::new("snes").unwrap();
        let mut emu = running_the_capture_program();
        let mut sources = vec![idle.get_pixel_sources(None)];
        for planes in [
            None,
            Some(0),
            Some(1),
            Some(2),
            Some(4),
            Some(8),
            Some(16),
            Some(15),
            Some(31),
            Some(0x8000_0011),
        ] {
            sources.push(emu.get_pixel_sources(planes));
        }
        let expected = [
            "null",
            concat!(
                "{cell: Uint16Array(len 57344, fnv1a64 0x2abaaefb122acfa1), ",
                "colour: Uint16Array(len 57344, fnv1a64 0x80879d5b3e4821fd), frame: 3, ",
                "fromLineStates: true, height: 224, layer: Uint16Array(len 57344, ",
                "fnv1a64 0x6e92b53c2738661d), matchesPicture: true, ",
                "tile: Uint32Array(len 57344, fnv1a64 0x5e95f5527dac8199), width: 256}",
            ),
            "{frame: 3, fromLineStates: true, height: 224, matchesPicture: true, width: 256}",
            concat!(
                "{frame: 3, fromLineStates: true, height: 224, ",
                "layer: Uint16Array(len 57344, fnv1a64 0x6e92b53c2738661d), ",
                "matchesPicture: true, width: 256}",
            ),
            concat!(
                "{colour: Uint16Array(len 57344, fnv1a64 0x80879d5b3e4821fd), frame: 3, ",
                "fromLineStates: true, height: 224, matchesPicture: true, width: 256}",
            ),
            concat!(
                "{frame: 3, fromLineStates: true, height: 224, matchesPicture: true, ",
                "tile: Uint32Array(len 57344, fnv1a64 0x5e95f5527dac8199), width: 256}",
            ),
            concat!(
                "{cell: Uint16Array(len 57344, fnv1a64 0x2abaaefb122acfa1), frame: 3, ",
                "fromLineStates: true, height: 224, matchesPicture: true, width: 256}",
            ),
            concat!(
                "{frame: 3, fromLineStates: true, height: 224, matchesPicture: true, ",
                "subColour: Uint16Array(len 57344, fnv1a64 0x045b5aaabee52325), ",
                "subLayer: Uint8Array(len 57344, fnv1a64 0xa7ceef1ebf7f8325), width: 256}",
            ),
            concat!(
                "{cell: Uint16Array(len 57344, fnv1a64 0x2abaaefb122acfa1), ",
                "colour: Uint16Array(len 57344, fnv1a64 0x80879d5b3e4821fd), frame: 3, ",
                "fromLineStates: true, height: 224, layer: Uint16Array(len 57344, ",
                "fnv1a64 0x6e92b53c2738661d), matchesPicture: true, ",
                "tile: Uint32Array(len 57344, fnv1a64 0x5e95f5527dac8199), width: 256}",
            ),
            concat!(
                "{cell: Uint16Array(len 57344, fnv1a64 0x2abaaefb122acfa1), ",
                "colour: Uint16Array(len 57344, fnv1a64 0x80879d5b3e4821fd), frame: 3, ",
                "fromLineStates: true, height: 224, layer: Uint16Array(len 57344, ",
                "fnv1a64 0x6e92b53c2738661d), matchesPicture: true, ",
                "subColour: Uint16Array(len 57344, fnv1a64 0x045b5aaabee52325), ",
                "subLayer: Uint8Array(len 57344, fnv1a64 0xa7ceef1ebf7f8325), ",
                "tile: Uint32Array(len 57344, fnv1a64 0x5e95f5527dac8199), width: 256}",
            ),
            concat!(
                "{frame: 3, fromLineStates: true, height: 224, ",
                "layer: Uint16Array(len 57344, fnv1a64 0x6e92b53c2738661d), ",
                "matchesPicture: true, subColour: Uint16Array(len 57344, ",
                "fnv1a64 0x045b5aaabee52325), subLayer: Uint8Array(len 57344, ",
                "fnv1a64 0xa7ceef1ebf7f8325), width: 256}",
            ),
        ];
        assert_eq!(shapes(&sources), expected);
    }

    #[wasm_bindgen_test]
    fn the_line_sprites_read_as_before() {
        let mut idle = RetroEmulator::new("snes").unwrap();
        let mut emu = running_the_capture_program();
        let expected = [
            "null",
            concat!(
                "{drawnWidth: Uint8Array(len 1024, fnv1a64 0x967865f30558c925), ",
                "fates: Uint8Array(len 1024, fnv1a64 0xb9c6dee5fd4ffedd), frame: 3, ",
                "fromLineStates: true, lines: Uint16Array(len 896, ",
                "fnv1a64 0xc2b4dc5df408de2c), slots: Uint8Array(len 1024, ",
                "fnv1a64 0x13c81b8067849c25), width: Uint8Array(len 1024, ",
                "fnv1a64 0x80e6ac3166837325), x: Int16Array(len 1024, ",
                "fnv1a64 0x41d05ddfe959cc65)}",
            ),
        ];
        assert_eq!(
            shapes(&[idle.get_line_sprites(), emu.get_line_sprites()]),
            expected
        );
    }

    #[wasm_bindgen_test]
    fn the_sprite_renders_read_as_before() {
        let mut idle = RetroEmulator::new("snes").unwrap();
        let mut emu = running_the_capture_program();
        let renders = [
            idle.render_sprites(&[1]),
            emu.render_sprites(&[]),
            emu.render_sprites(&[1, 0, 1]),
            emu.render_sprites(&[1; 128]),
        ];
        let expected = [
            "null",
            concat!(
                "{data: Uint8Array(len 229376, fnv1a64 0xc9b1e505f9a82325), ",
                "fromLineStates: true, height: 224, width: 256}",
            ),
            concat!(
                "{data: Uint8Array(len 229376, fnv1a64 0x472177febf22ec05), ",
                "fromLineStates: true, height: 224, width: 256}",
            ),
            concat!(
                "{data: Uint8Array(len 229376, fnv1a64 0x59b321ddc659a405), ",
                "fromLineStates: true, height: 224, width: 256}",
            ),
        ];
        assert_eq!(shapes(&renders), expected);
    }

    #[wasm_bindgen_test]
    fn the_video_memory_sources_read_as_before() {
        let idle = RetroEmulator::new("snes").unwrap();
        let emu = running_the_capture_program();
        let expected = [
            "null",
            concat!(
                "{frames: Uint32Array(len 65536, fnv1a64 0x7e0954272f497645), ",
                "origins: Uint32Array(len 65536, fnv1a64 0x1d5aaafb91b9d8e8)}",
            ),
        ];
        assert_eq!(
            shapes(&[
                idle.get_video_memory_sources(),
                emu.get_video_memory_sources()
            ]),
            expected
        );
    }

    #[wasm_bindgen_test]
    fn the_resolved_sources_read_as_before() {
        let emu = running_the_capture_program();
        let addresses = [
            0x00_8000u32,
            u32::from(PROGRAM_PALETTE),
            0x81_9020,
            0x7E_0000,
            0x00_0000,
            0x00_2100,
            0x40_0000,
            0xFF_FFFF,
            0xFF00_8000,
        ];
        let resolved = [
            JsValue::from(emu.resolve_sources(&addresses)),
            JsValue::from(emu.resolve_sources(&[])),
        ];
        let expected = [
            "Int32Array(len 9, fnv1a64 0x906e57eb2f638fa2)",
            "Int32Array(len 0, fnv1a64 0xcbf29ce484222325)",
        ];
        assert_eq!(shapes(&resolved), expected);
    }

    #[wasm_bindgen_test]
    fn the_frame_writes_read_as_before() {
        let mut idle = RetroEmulator::new("snes").unwrap();
        let mut emu = running_the_capture_program();
        let writes = [
            idle.take_frame_writes(),
            emu.take_frame_writes(),
            emu.take_frame_writes(),
        ];
        let expected = [
            "null",
            concat!(
                "{dropped: 0, records: Uint32Array(len 1384, ",
                "fnv1a64 0x0bd0b238c49f8713), wordsPerRecord: 8}",
            ),
            concat!(
                "{dropped: 0, records: Uint32Array(len 0, fnv1a64 0xcbf29ce484222325), ",
                "wordsPerRecord: 8}",
            ),
        ];
        assert_eq!(shapes(&writes), expected);
    }

    #[wasm_bindgen_test]
    fn a_repaint_draws_as_before() {
        let mut emu = running_the_capture_program();
        emu.toggle_layer(0, false);
        emu.repaint_frame();
        let mut pictures: Vec<JsValue> = vec![emu.get_framebuffer_data().into()];
        emu.toggle_layer(0, true);
        emu.toggle_layer(4, false);
        emu.repaint_frame();
        pictures.push(emu.get_framebuffer_data().into());
        let expected = [
            "Uint8ClampedArray(len 229376, fnv1a64 0x0f7bd3c3a27b6475)",
            "Uint8ClampedArray(len 229376, fnv1a64 0x5aa9123884e96d52)",
        ];
        assert_eq!(shapes(&pictures), expected);
    }

    /// The debugger's readers, each held to the shape it answers today, and
    /// what each step returns. Most run on one small program with an execute
    /// breakpoint, a write watch, an event breakpoint, the main CPU's trace
    /// and two instructions of history (`debugging_the_program`); the readers
    /// of the cartridge CPU's trace run on two cartridges whose chip runs a
    /// short program. A literal that stops matching means a value a host
    /// reads changed: record a new one only for a change a host is meant to
    /// see.
    mod debugger_shape_tests {
        use super::*;
        use snes1_core::TraceEntryBinary65816;

        /// Bytes of one entry of each trace.
        const TRACE_ENTRY_SIZE_65816: usize = <TraceEntryBinary65816 as TraceEntry>::SIZE;
        const TRACE_ENTRY_SIZE_SPC700: usize = <TraceEntryBinarySpc700 as TraceEntry>::SIZE;
        #[cfg(feature = "chip-superfx")]
        const TRACE_ENTRY_SIZE_GSU: usize = <TraceEntryBinaryGsu as TraceEntry>::SIZE;
        #[cfg(feature = "chip-cx4")]
        const TRACE_ENTRY_SIZE_CX4: usize = <TraceEntryBinaryCx4 as TraceEntry>::SIZE;

        /// A number as a host reads it.
        fn number(value: usize) -> JsValue {
            JsValue::from_f64(value as f64)
        }

        /// Bytes as a host reads them: a `Uint8Array` copy.
        fn byte_array(bytes: &[u8]) -> JsValue {
            js_sys::Uint8Array::from(bytes).into()
        }

        /// The length of a trace chunk, its first entry and the whole chunk.
        fn chunk(data: &[u8], entry_size: usize) -> Vec<JsValue> {
            vec![
                number(data.len()),
                byte_array(&data[..entry_size.min(data.len())]),
                byte_array(data),
            ]
        }

        /// A `JSR` to a routine that makes a `JSL` to another, which loads a
        /// value, stores it to work RAM at `$0010` and returns with `RTL`;
        /// the first returns with `RTS` to a loop.
        fn debugger_program_rom() -> Vec<u8> {
            lorom_running(
                "DEBUGGER PROGRAM",
                &[0x20, 0x10, 0x80, 0x80, 0xFE], // JSR $8010, BRA $8003
                &[
                    (0x8010, &[0x22, 0x20, 0x80, 0x00, 0x60]), // JSL $00:8020, RTS
                    (0x8020, &[0xA9, 0x12, 0x8D, 0x10, 0x00, 0x6B]), // LDA #$12, STA $0010, RTL
                ],
            )
        }

        /// The program with its three breakpoints (an execute breakpoint at
        /// `$8020`, a write watch on `$0010`, a VBlank event breakpoint) and
        /// the main CPU's trace on, run until the execute breakpoint stops it
        /// in front of the load: on the instrumented loop, through the `JSR`
        /// and the `JSL`.
        fn stopped_at_the_breakpoint() -> RetroEmulator {
            let mut emu = RetroEmulator::new("snes").unwrap();
            emu.load_rom(&debugger_program_rom()).unwrap();
            emu.add_breakpoint(0x00_8020, "execute".to_string(), None);
            emu.add_breakpoint(0x00_0010, "write".to_string(), None);
            emu.add_event_breakpoint(1, 0xFFFF_FFFF);
            emu.set_tracing_enabled(true);
            emu.run_frame();
            emu
        }

        /// One instruction on the full loop: the step mode, then a frame.
        fn step_into(emu: &mut RetroEmulator) {
            emu.set_execution_mode("StepInto".to_string());
            emu.run_frame();
        }

        /// The program stopped at its breakpoint, then stepped twice on the
        /// full loop: the load, and the store the write watch sees. It waits
        /// in front of the `RTL`.
        fn debugging_the_program() -> RetroEmulator {
            let mut emu = stopped_at_the_breakpoint();
            step_into(&mut emu);
            step_into(&mut emu);
            emu
        }

        /// Right after the stop on the instrumented loop, the states where it
        /// stopped: the main CPU in front of the load at `$8020`. After the two
        /// steps, the main CPU in front of the `RTL`.
        #[wasm_bindgen_test]
        fn the_cpu_states_read_as_before() {
            let mut emu = stopped_at_the_breakpoint();
            let mut states = vec![emu.get_cpu_states()];
            step_into(&mut emu);
            step_into(&mut emu);
            states.push(emu.get_cpu_states());
            let expected = [
                concat!(
                    r#"[{cpu_id: 0, flags: 52, pc: 32800, registers: [{name: "A", value: 0}, "#,
                    r#"{name: "X", value: 0}, {name: "Y", value: 0}, {name: "S", value: 504}, "#,
                    r#"{name: "D", value: 0}, {name: "DB", value: 0}, {name: "PB", value: 0}, "#,
                    r#"{name: "P", value: 52}, {name: "E", value: 1}]}, {cpu_id: 1, flags: 2, "#,
                    r#"pc: 65472, registers: [{name: "A", value: 0}, {name: "X", value: 0}, "#,
                    r#"{name: "Y", value: 0}, {name: "SP", value: 239}, {name: "YA", value: 0},"#,
                    r#" {name: "PSW", value: 2}]}]"#,
                ),
                concat!(
                    r#"[{cpu_id: 0, flags: 52, pc: 32805, registers: [{name: "A", value: 18}, "#,
                    r#"{name: "X", value: 0}, {name: "Y", value: 0}, {name: "S", value: 504}, "#,
                    r#"{name: "D", value: 0}, {name: "DB", value: 0}, {name: "PB", value: 0}, "#,
                    r#"{name: "P", value: 52}, {name: "E", value: 1}]}, {cpu_id: 1, flags: 2, "#,
                    r#"pc: 65472, registers: [{name: "A", value: 0}, {name: "X", value: 0}, "#,
                    r#"{name: "Y", value: 0}, {name: "SP", value: 239}, {name: "YA", value: 0},"#,
                    r#" {name: "PSW", value: 2}]}]"#,
                ),
            ];
            assert_eq!(shapes(&states), expected);
        }

        /// The three breakpoints once the program ran (the execute breakpoint and
        /// the write watch hit once each, the event breakpoint never); then with
        /// the write watch watching reads too, the event breakpoint disabled and the
        /// execute breakpoint given a value and a hit skip; then with the write
        /// watch removed.
        #[wasm_bindgen_test]
        fn the_breakpoint_list_reads_as_before() {
            let mut emu = debugging_the_program();
            let mut lists = vec![emu.list_breakpoints()];
            emu.set_breakpoint_access(1, false, true, true);
            emu.enable_breakpoint(2, false);
            let spec = MatchSpec {
                value: Some(0x12),
                hit_skip: 1,
                ..Default::default()
            };
            emu.set_breakpoint_match_spec(0, serde_wasm_bindgen::to_value(&spec).unwrap())
                .unwrap();
            lists.push(emu.list_breakpoints());
            emu.remove_breakpoint(1);
            lists.push(emu.list_breakpoints());
            let expected = [
                concat!(
                    r#"[{breakpoint: {address: 32800, bp_type: {Access: {exec: true, read: "#,
                    r#"false, write: false}}, condition: undefined, cpu_id: 0}, enabled: true, "#,
                    r#"hit_count: 1, id: 0, match_spec: {action: "break", address_end: "#,
                    r#"undefined, expr: undefined, hit_skip: 0, one_shot: false, value: "#,
                    r#"undefined, value_compare: "eq", value_mask: 0, value_size: 0}}, "#,
                    r#"{breakpoint: {address: 16, bp_type: {Access: {exec: false, read: false, "#,
                    r#"write: true}}, condition: undefined, cpu_id: 0}, enabled: true, "#,
                    r#"hit_count: 1, id: 1, match_spec: {action: "break", address_end: "#,
                    r#"undefined, expr: undefined, hit_skip: 0, one_shot: false, value: "#,
                    r#"undefined, value_compare: "eq", value_mask: 0, value_size: 0}}, "#,
                    r#"{breakpoint: {address: 0, bp_type: {Event: {code: 1, param: 4294967295}}"#,
                    r#", condition: undefined, cpu_id: undefined}, enabled: true, hit_count: 0,"#,
                    r#" id: 2, match_spec: {action: "break", address_end: undefined, expr: "#,
                    r#"undefined, hit_skip: 0, one_shot: false, value: undefined, "#,
                    r#"value_compare: "eq", value_mask: 0, value_size: 0}}]"#,
                ),
                concat!(
                    r#"[{breakpoint: {address: 32800, bp_type: {Access: {exec: true, read: "#,
                    r#"false, write: false}}, condition: undefined, cpu_id: 0}, enabled: true, "#,
                    r#"hit_count: 1, id: 0, match_spec: {action: "break", address_end: "#,
                    r#"undefined, expr: undefined, hit_skip: 1, one_shot: false, value: 18, "#,
                    r#"value_compare: "eq", value_mask: 0, value_size: 0}}, {breakpoint: "#,
                    r#"{address: 16, bp_type: {Access: {exec: false, read: true, write: true}},"#,
                    r#" condition: undefined, cpu_id: 0}, enabled: true, hit_count: 1, id: 1, "#,
                    r#"match_spec: {action: "break", address_end: undefined, expr: undefined, "#,
                    r#"hit_skip: 0, one_shot: false, value: undefined, value_compare: "eq", "#,
                    r#"value_mask: 0, value_size: 0}}, {breakpoint: {address: 0, bp_type: "#,
                    r#"{Event: {code: 1, param: 4294967295}}, condition: undefined, cpu_id: "#,
                    r#"undefined}, enabled: false, hit_count: 0, id: 2, match_spec: {action: "#,
                    r#""break", address_end: undefined, expr: undefined, hit_skip: 0, "#,
                    r#"one_shot: false, value: undefined, value_compare: "eq", value_mask: 0, "#,
                    r#"value_size: 0}}]"#,
                ),
                concat!(
                    r#"[{breakpoint: {address: 32800, bp_type: {Access: {exec: true, read: "#,
                    r#"false, write: false}}, condition: undefined, cpu_id: 0}, enabled: true, "#,
                    r#"hit_count: 1, id: 0, match_spec: {action: "break", address_end: "#,
                    r#"undefined, expr: undefined, hit_skip: 1, one_shot: false, value: 18, "#,
                    r#"value_compare: "eq", value_mask: 0, value_size: 0}}, {breakpoint: "#,
                    r#"{address: 0, bp_type: {Event: {code: 1, param: 4294967295}}, condition: "#,
                    r#"undefined, cpu_id: undefined}, enabled: false, hit_count: 0, id: 2, "#,
                    r#"match_spec: {action: "break", address_end: undefined, expr: undefined, "#,
                    r#"hit_skip: 0, one_shot: false, value: undefined, value_compare: "eq", "#,
                    r#"value_mask: 0, value_size: 0}}]"#,
                ),
            ];
            assert_eq!(shapes(&lists), expected);
        }

        /// The twelve event classes as a host reads them, each with its kind.
        #[wasm_bindgen_test]
        fn the_event_catalogue_reads_as_before() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            let expected = [concat!(
                r#"[{code: 1, description: "Entering vertical blank", kind: "#,
                r#""vertical_blank", label: "VBlank", param_label: undefined, "#,
                r#"param_options: []}, {code: 2, description: "V-blank NMI delivered to "#,
                r#"the 65C816", kind: "vertical_blank_interrupt", label: "NMI", "#,
                r#"param_label: undefined, param_options: []}, {code: 3, description: "#,
                r#""H/V-timer IRQ raised", kind: "raster_interrupt", label: "IRQ", "#,
                r#"param_label: undefined, param_options: []}, {code: 4, description: "#,
                r#""General-purpose DMA block started ($420B)", kind: "transfer_start", "#,
                r#"label: "DMA start", param_label: undefined, param_options: []}, {code: "#,
                r#"5, description: "Top-of-frame HDMA initialisation", kind: "#,
                r#""line_transfer_set_up", label: "HDMA start", param_label: undefined, "#,
                r#"param_options: []}, {code: 6, description: "A rendered scanline matched "#,
                r#"the V counter", kind: "line", label: "Scanline", param_label: "Scanline "#,
                r#"(V)", param_options: []}, {code: 7, description: "Write to a PPU "#,
                r#"register ($2100-$213F)", kind: "video_register_write", label: "PPU "#,
                r#"register write", param_label: "Register ($00-$3F)", param_options: []}, "#,
                r#"{code: 8, description: "65C816 write to an APU mailbox port "#,
                r#"($2140-$2143)", kind: "audio_port_write", label: "APU port write", "#,
                r#"param_label: "Port", param_options: [{label: "Port 0", value: 0}, "#,
                r#"{label: "Port 1", value: 1}, {label: "Port 2", value: 2}, {label: "Port "#,
                r#"3", value: 3}]}, {code: 9, description: "SPC700 write to an S-DSP "#,
                r#"register", kind: "audio_register_write", label: "S-DSP register write", "#,
                r#"param_label: "DSP register ($00-$7F)", param_options: []}, {code: 10, "#,
                r#"description: "A general DMA channel finished its block, by the B-bus "#,
                r#"port it wrote", kind: "transfer_done", label: "DMA done", param_label: "#,
                r#""B-bus port", param_options: [{label: "VRAM ($2118)", value: 24}, "#,
                r#"{label: "Palette ($2122)", value: 34}, {label: "Sprite table ($2104)", "#,
                r#"value: 4}, {label: "Work RAM ($2180)", value: 128}]}, {code: 11, "#,
                r#"description: "A colour written into CGRAM by the processor, a DMA or an "#,
                r#"HDMA", kind: "palette_write", label: "Palette write", param_label: "#,
                r#""Colour (0-255)", param_options: []}, {code: 12, description: "A write "#,
                r#"into VRAM changed a tile's bytes (the same bytes written again do not "#,
                r#"count)", kind: "tile_write", label: "Tile changed", param_label: "Tile "#,
                r#"(4bpp, 0-2047)", param_options: []}]"#,
            )];
            assert_eq!(shapes(&[emu.get_available_events()]), expected);
        }

        /// Running at first, paused by the execute breakpoint, paused after a step,
        /// then each mode set by its name; a name the core does not know reads as
        /// `Running`.
        #[wasm_bindgen_test]
        fn the_execution_mode_reads_as_before() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            emu.load_rom(&debugger_program_rom()).unwrap();
            emu.add_breakpoint(0x00_8020, "execute".to_string(), None);
            let mut modes = vec![emu.get_execution_mode()];
            emu.run_frame();
            modes.push(emu.get_execution_mode());
            step_into(&mut emu);
            modes.push(emu.get_execution_mode());
            for mode in ["Running", "StepOut", "StepOver", "Unknown"] {
                emu.set_execution_mode(mode.to_string());
                modes.push(emu.get_execution_mode());
            }
            let modes: Vec<JsValue> = modes.iter().map(|mode| JsValue::from_str(mode)).collect();
            let expected = [
                r#""Running""#,
                r#""Paused""#,
                r#""Paused""#,
                r#""Running""#,
                r#""StepOut""#,
                r#""StepOver""#,
                r#""Running""#,
            ];
            assert_eq!(shapes(&modes), expected);
        }

        /// With no game, then with the program loaded (the ROM has a size). OAM
        /// is named the sprite table and CGRAM the palette.
        #[wasm_bindgen_test]
        fn the_memory_regions_read_as_before() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            let mut regions = vec![emu.get_memory_regions()];
            emu.load_rom(&debugger_program_rom()).unwrap();
            regions.push(emu.get_memory_regions());
            let expected = [
                concat!(
                    r#"[{name: "WRAM", region: "MainRam", size: 131072, start_address: 8257536,"#,
                    r#" writable: true}, {name: "VRAM", region: "VideoRam", size: 65536, "#,
                    r#"start_address: 0, writable: true}, {name: "OAM", region: "SpriteTable", "#,
                    r#"size: 544, start_address: 0, writable: true}, {name: "CGRAM", region: "#,
                    r#""PaletteRam", size: 512, start_address: 0, writable: true}, {name: "#,
                    r#""ARAM", region: "AudioRam", size: 65536, start_address: 0, writable: "#,
                    r#"true}, {name: "ROM", region: "Rom", size: 0, start_address: 0, "#,
                    r#"writable: false}, {name: "SRAM", region: "SaveRam", size: 0, "#,
                    r#"start_address: 0, writable: true}]"#,
                ),
                concat!(
                    r#"[{name: "WRAM", region: "MainRam", size: 131072, start_address: 8257536,"#,
                    r#" writable: true}, {name: "VRAM", region: "VideoRam", size: 65536, "#,
                    r#"start_address: 0, writable: true}, {name: "OAM", region: "SpriteTable", "#,
                    r#"size: 544, start_address: 0, writable: true}, {name: "CGRAM", region: "#,
                    r#""PaletteRam", size: 512, start_address: 0, writable: true}, {name: "#,
                    r#""ARAM", region: "AudioRam", size: 65536, start_address: 0, writable: "#,
                    r#"true}, {name: "ROM", region: "Rom", size: 131072, start_address: 0, "#,
                    r#"writable: false}, {name: "SRAM", region: "SaveRam", size: 0, "#,
                    r#"start_address: 0, writable: true}]"#,
                ),
            ];
            assert_eq!(shapes(&regions), expected);
        }

        /// The two steps' records, the newest first; the newest alone; then the
        /// sound CPU's and a cartridge CPU's, which keep none.
        #[wasm_bindgen_test]
        fn the_instruction_history_reads_as_before() {
            let mut emu = debugging_the_program();
            let histories = [
                emu.get_instruction_history(0, 16),
                emu.get_instruction_history(0, 1),
                emu.get_instruction_history(1, 16),
                emu.get_instruction_history(2, 16),
            ];
            let expected = [
                concat!(
                    r#"[{address: 32802, cycle_count: 4, instruction: "sta $0010", "#,
                    r#"registers_before: [{name: "A", value: 18}, {name: "X", value: 0}, "#,
                    r#"{name: "Y", value: 0}, {name: "S", value: 504}, {name: "D", value: 0}, "#,
                    r#"{name: "DB", value: 0}, {name: "PB", value: 0}, {name: "P", value: 52}, "#,
                    r#"{name: "E", value: 1}]}, {address: 32800, cycle_count: 2, instruction: "#,
                    r#""lda #$12", registers_before: [{name: "A", value: 0}, {name: "X", "#,
                    r#"value: 0}, {name: "Y", value: 0}, {name: "S", value: 504}, {name: "D", "#,
                    r#"value: 0}, {name: "DB", value: 0}, {name: "PB", value: 0}, {name: "P", "#,
                    r#"value: 52}, {name: "E", value: 1}]}]"#,
                ),
                concat!(
                    r#"[{address: 32802, cycle_count: 4, instruction: "sta $0010", "#,
                    r#"registers_before: [{name: "A", value: 18}, {name: "X", value: 0}, "#,
                    r#"{name: "Y", value: 0}, {name: "S", value: 504}, {name: "D", value: 0}, "#,
                    r#"{name: "DB", value: 0}, {name: "PB", value: 0}, {name: "P", value: 52}, "#,
                    r#"{name: "E", value: 1}]}]"#,
                ),
                "[]",
                "[]",
            ];
            assert_eq!(shapes(&histories), expected);
        }

        /// The two calls the program made before its two steps, the outermost
        /// first; then the sound CPU's and a cartridge CPU's, which keep none.
        #[wasm_bindgen_test]
        fn the_call_stack_reads_as_before() {
            let mut emu = debugging_the_program();
            let stacks = [
                emu.get_call_stack(0),
                emu.get_call_stack(1),
                emu.get_call_stack(2),
            ];
            let expected = [
                concat!(
                    "[{call_address: 32768, function_name: undefined, return_address: 32771},",
                    " {call_address: 32784, function_name: undefined, return_address: 32788}]",
                ),
                "[]",
                "[]",
            ];
            assert_eq!(shapes(&stacks), expected);
        }

        /// The focus at first, then after asking for the sound CPU, for a cartridge
        /// CPU the program does not have (left as it was), and for the main CPU.
        #[wasm_bindgen_test]
        fn the_focused_cpu_reads_as_before() {
            let mut emu = debugging_the_program();
            let mut focus = vec![number(emu.get_focused_cpu())];
            for cpu in [1, 2, 0] {
                emu.set_focused_cpu(cpu);
                focus.push(number(emu.get_focused_cpu()));
            }
            let expected = ["0", "1", "1", "0"];
            assert_eq!(shapes(&focus), expected);
        }

        /// The main CPU at `$8000`, `$8003`, `$8010`, `$8020`, `$8022` and `$8025`
        /// (the program ran four of them), the sound CPU at its boot ROM's first
        /// instruction, which it has not run yet, and a cartridge CPU the program
        /// does not have; then the sound CPU at the same address after a frame in
        /// which it ran its boot ROM.
        #[wasm_bindgen_test]
        fn the_coverage_reads_as_before() {
            let emu = debugging_the_program();
            let mut covered: Vec<JsValue> = [
                (0, 0x00_8000),
                (0, 0x00_8003),
                (0, 0x00_8010),
                (0, 0x00_8020),
                (0, 0x00_8022),
                (0, 0x00_8025),
                (1, 0xFFC0),
                (2, 0x00_8000),
            ]
            .iter()
            .map(|&(cpu, address)| JsValue::from_bool(emu.is_covered(cpu, address)))
            .collect();
            covered.push(JsValue::from_bool(spc700_traced().is_covered(1, 0xFFC0)));
            let expected = [
                "true", "false", "true", "true", "true", "false", "false", "false", "false",
            ];
            assert_eq!(shapes(&covered), expected);
        }

        /// The main CPU's four addresses, the sound CPU's none, and a cartridge CPU
        /// the program does not have.
        #[wasm_bindgen_test]
        fn the_coverage_stats_read_as_before() {
            let emu = debugging_the_program();
            let stats = [
                emu.get_coverage_stats(0),
                emu.get_coverage_stats(1),
                emu.get_coverage_stats(2),
            ];
            let expected = [
                "{coveredBytes: 4, percent: 0.00002384185791015625, totalBytes: 16777216}",
                "{coveredBytes: 0, percent: 0, totalBytes: 65536}",
                "{coveredBytes: 0, percent: 0, totalBytes: 0}",
            ];
            assert_eq!(shapes(&stats), expected);
        }

        /// The flag after the program's four traced instructions, then on a
        /// program that loops on `NOP`s with the main CPU's trace on: each
        /// instruction is traced, and ten frames pass the trace's first
        /// eight MiB.
        #[wasm_bindgen_test]
        fn the_trace_flush_flag_reads_as_before() {
            let emu = debugging_the_program();
            let mut flags = vec![JsValue::from_bool(emu.needs_trace_flush())];
            let nops: Vec<u8> = [0xEA; 124].into_iter().chain([0x80, 0x82]).collect();
            let mut looping = RetroEmulator::new("snes").unwrap();
            let rom = lorom_running("LOOPING", &nops, &[]); // 124 NOPs, BRA $8000
            looping.load_rom(&rom).unwrap();
            looping.set_tracing_enabled(true);
            for _ in 0..10 {
                looping.run_frame();
            }
            flags.push(JsValue::from_bool(looping.needs_trace_flush()));
            looping.clear_trace_flushed();
            flags.push(JsValue::from_bool(looping.needs_trace_flush()));
            let expected = ["false", "true", "false"];
            assert_eq!(shapes(&flags), expected);
        }

        /// Off at first, on once set (the sound CPU's stays off), off once cleared.
        #[wasm_bindgen_test]
        fn the_trace_switch_reads_as_before() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            let mut switch = vec![JsValue::from_bool(emu.is_tracing_enabled())];
            emu.set_tracing_enabled(true);
            switch.push(JsValue::from_bool(emu.is_tracing_enabled()));
            switch.push(JsValue::from_bool(emu.is_spc700_tracing_enabled()));
            emu.set_tracing_enabled(false);
            switch.push(JsValue::from_bool(emu.is_tracing_enabled()));
            let expected = ["false", "true", "false", "false"];
            assert_eq!(shapes(&switch), expected);
        }

        /// The chunk after the program's four traced instructions (the `JSR`
        /// and the `JSL` on the instrumented loop, the load and the store on
        /// the full loop), then what is left once it is cleared.
        #[wasm_bindgen_test]
        fn the_trace_flush_data_reads_as_before() {
            let mut emu = debugging_the_program();
            let mut data = chunk(&emu.get_trace_flush_data(), TRACE_ENTRY_SIZE_65816);
            emu.clear_trace_flushed();
            data.push(byte_array(&emu.get_trace_flush_data()));
            let expected = [
                "176",
                "Uint8Array(len 44, fnv1a64 0xc518fa71d9f9da00)",
                "Uint8Array(len 176, fnv1a64 0xe3be799a7b74468b)",
                "Uint8Array(len 0, fnv1a64 0xcbf29ce484222325)",
            ];
            assert_eq!(shapes(&data), expected);
        }

        /// The main CPU's entry size in bytes.
        #[wasm_bindgen_test]
        fn the_trace_entry_size_reads_as_before() {
            let emu = RetroEmulator::new("snes").unwrap();
            let expected = ["44"];
            assert_eq!(shapes(&[number(emu.get_trace_entry_size())]), expected);
        }

        /// The program's four entries as text, the same chunk with ten bytes
        /// of a fifth entry after it, and nothing.
        #[wasm_bindgen_test]
        fn the_trace_text_reads_as_before() {
            let mut emu = debugging_the_program();
            let data = emu.get_trace_flush_data();
            let mut longer = data.clone();
            longer.extend_from_slice(&data[..10]);
            let texts: Vec<JsValue> = [&data[..], &longer[..], &[]]
                .iter()
                .map(|chunk| JsValue::from_str(&emu.convert_trace_chunk_to_text(chunk)))
                .collect();
            let expected = [
                concat!(
                    r#""008000 jsr $8010      [008010] A:0000 X:0000 Y:0000 S:01fd D:0000 "#,
                    r#"DB:00 ..1B.I.. V:  0 H:  0 F: 0\n008010 jsl $008020    [008020] A:0000 "#,
                    r#"X:0000 Y:0000 S:01fb D:0000 DB:00 ..1B.I.. V:  0 H: 11 F: 0\n008020 lda "#,
                    r#"#$12                A:0000 X:0000 Y:0000 S:01f8 D:0000 DB:00 ..1B.I.. "#,
                    r#"V:  0 H: 27 F: 0\n008022 sta $0010      [000010] A:0012 X:0000 Y:0000 "#,
                    r#"S:01f8 D:0000 DB:00 ..1B.I.. V:  0 H: 31 F: 0\n""#,
                ),
                concat!(
                    r#""008000 jsr $8010      [008010] A:0000 X:0000 Y:0000 S:01fd D:0000 "#,
                    r#"DB:00 ..1B.I.. V:  0 H:  0 F: 0\n008010 jsl $008020    [008020] A:0000 "#,
                    r#"X:0000 Y:0000 S:01fb D:0000 DB:00 ..1B.I.. V:  0 H: 11 F: 0\n008020 lda "#,
                    r#"#$12                A:0000 X:0000 Y:0000 S:01f8 D:0000 DB:00 ..1B.I.. "#,
                    r#"V:  0 H: 27 F: 0\n008022 sta $0010      [000010] A:0012 X:0000 Y:0000 "#,
                    r#"S:01f8 D:0000 DB:00 ..1B.I.. V:  0 H: 31 F: 0\n""#,
                ),
                r#""""#,
            ];
            assert_eq!(shapes(&texts), expected);
        }

        /// From in front of the `RTL`: `step()`, then `stepCpu` for the main
        /// CPU, the sound CPU, a cartridge CPU the program does not have and
        /// a CPU past the list.
        #[wasm_bindgen_test]
        fn a_step_returns_as_before() {
            let mut emu = debugging_the_program();
            let results = [
                emu.step(),
                emu.step_cpu(0),
                emu.step_cpu(1),
                emu.step_cpu(2),
                emu.step_cpu(7),
            ];
            let expected = [
                "{breakpoint_hit: false, cycles: 44, instruction_executed: true}",
                "{breakpoint_hit: false, cycles: 42, instruction_executed: true}",
                "{breakpoint_hit: false, cycles: 0, instruction_executed: false}",
                "{breakpoint_hit: false, cycles: 0, instruction_executed: false}",
                "{breakpoint_hit: false, cycles: 0, instruction_executed: false}",
            ];
            assert_eq!(shapes(&results), expected);
        }

        /// The program with only the sound CPU's trace on, run one frame:
        /// the SPC700 runs its boot ROM.
        fn spc700_traced() -> RetroEmulator {
            let mut emu = RetroEmulator::new("snes").unwrap();
            emu.load_rom(&debugger_program_rom()).unwrap();
            emu.set_spc700_tracing_enabled(true);
            emu.run_frame();
            emu
        }

        /// Set, the sound CPU's switch reads on and the main CPU's stays off; then
        /// cleared.
        #[wasm_bindgen_test]
        fn setting_the_spc700_trace_reads_as_before() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            emu.set_spc700_tracing_enabled(true);
            let mut switch = vec![
                JsValue::from_bool(emu.is_spc700_tracing_enabled()),
                JsValue::from_bool(emu.is_tracing_enabled()),
            ];
            emu.set_spc700_tracing_enabled(false);
            switch.push(JsValue::from_bool(emu.is_spc700_tracing_enabled()));
            let expected = ["true", "false", "false"];
            assert_eq!(shapes(&switch), expected);
        }

        /// Off at first, and on in a run with it set.
        #[wasm_bindgen_test]
        fn the_spc700_trace_switch_reads_as_before() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            let switch = [
                JsValue::from_bool(emu.is_spc700_tracing_enabled()),
                JsValue::from_bool(spc700_traced().is_spc700_tracing_enabled()),
            ];
            let expected = ["false", "true"];
            assert_eq!(shapes(&switch), expected);
        }

        /// The sound CPU's chunk after one frame of its boot ROM, then the main
        /// CPU's, empty: its trace is off.
        #[wasm_bindgen_test]
        fn the_spc700_trace_data_reads_as_before() {
            let mut emu = spc700_traced();
            let mut data = chunk(&emu.get_spc700_trace_flush_data(), TRACE_ENTRY_SIZE_SPC700);
            data.push(byte_array(&emu.get_trace_flush_data()));
            let expected = [
                "95376",
                "Uint8Array(len 24, fnv1a64 0x9587a08e0a5703c5)",
                "Uint8Array(len 95376, fnv1a64 0x8a6d5972165b0b8d)",
                "Uint8Array(len 0, fnv1a64 0xcbf29ce484222325)",
            ];
            assert_eq!(shapes(&data), expected);
        }

        /// Once cleared, the chunk is empty and the switch stays on.
        #[wasm_bindgen_test]
        fn clearing_the_spc700_trace_reads_as_before() {
            let mut emu = spc700_traced();
            emu.clear_spc700_trace_flushed();
            let data = [
                byte_array(&emu.get_spc700_trace_flush_data()),
                JsValue::from_bool(emu.is_spc700_tracing_enabled()),
            ];
            let expected = ["Uint8Array(len 0, fnv1a64 0xcbf29ce484222325)", "true"];
            assert_eq!(shapes(&data), expected);
        }

        /// The sound CPU's entry size in bytes.
        #[wasm_bindgen_test]
        fn the_spc700_trace_entry_size_reads_as_before() {
            let emu = RetroEmulator::new("snes").unwrap();
            let size = [number(emu.get_spc700_trace_entry_size())];
            let expected = ["24"];
            assert_eq!(shapes(&size), expected);
        }

        /// The boot ROM's first three entries as text, the same with ten
        /// bytes of a fourth after them, and nothing.
        #[wasm_bindgen_test]
        fn the_spc700_trace_text_reads_as_before() {
            let mut emu = spc700_traced();
            let data = emu.get_spc700_trace_flush_data();
            let three = &data[..3 * TRACE_ENTRY_SIZE_SPC700];
            let longer = &data[..3 * TRACE_ENTRY_SIZE_SPC700 + 10];
            let texts: Vec<JsValue> = [three, longer, &[]]
                .iter()
                .map(|chunk| JsValue::from_str(&emu.convert_spc700_trace_chunk_to_text(chunk)))
                .collect();
            let expected = [
                concat!(
                    r#""..ffc0 mov   x, #$ef           A:00 X:00 Y:00 SP:01ef YA:0000 "#,
                    r#"......Z.\n..ffc2 mov   sp, x             A:00 X:ef Y:00 SP:01ef YA:0000 "#,
                    r#"N.......\n..ffc3 mov   a, #$00           A:00 X:ef Y:00 SP:01ef YA:0000 "#,
                    r#"N.......\n""#,
                ),
                concat!(
                    r#""..ffc0 mov   x, #$ef           A:00 X:00 Y:00 SP:01ef YA:0000 "#,
                    r#"......Z.\n..ffc2 mov   sp, x             A:00 X:ef Y:00 SP:01ef YA:0000 "#,
                    r#"N.......\n..ffc3 mov   a, #$00           A:00 X:ef Y:00 SP:01ef YA:0000 "#,
                    r#"N.......\n""#,
                ),
                r#""""#,
            ];
            assert_eq!(shapes(&texts), expected);
        }

        /// A Super FX cartridge whose main CPU masks the GSU's interrupt,
        /// starts the GSU on `IWT R0,#$1234`, `IWT R1,#$5678`, `STOP` at
        /// `$0100`, then waits in a loop.
        #[cfg(feature = "chip-superfx")]
        fn super_fx_rom() -> Vec<u8> {
            let mut rom = vec![0u8; 0x10_0000];
            let start: [u8; 18] = [
                0xA9, 0x80, 0x8D, 0x37, 0x30, // LDA #$80, STA $3037: mask the interrupt
                0x58, // CLI
                0xA9, 0x00, 0x8D, 0x1E, 0x30, // LDA #$00, STA $301E: R15 low
                0xA9, 0x01, 0x8D, 0x1F, 0x30, // LDA #$01, STA $301F: R15 high, the start
                0x80, 0xFE, // BRA -2
            ];
            rom[..start.len()].copy_from_slice(&start);
            let kernel = [0xF0, 0x34, 0x12, 0xF1, 0x78, 0x56, 0x00];
            rom[0x0100..0x0100 + kernel.len()].copy_from_slice(&kernel);
            rom[0x7FC0..0x7FC0 + 21].copy_from_slice(b"SUPER FX TRACE       ");
            rom[0x7FD5] = 0x20; // LoROM
            rom[0x7FD6] = 0x13; // a Super FX cartridge
            rom[0x7FD7] = 0x0A; // 1 MiB
            rom[0x7FD9] = 0x01; // USA, NTSC
            rom[0x7FDC..0x7FDE].copy_from_slice(&(!0x1234u16).to_le_bytes());
            rom[0x7FDE..0x7FE0].copy_from_slice(&0x1234u16.to_le_bytes());
            rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
            rom[0x7FFE..0x8000].copy_from_slice(&0x8000u16.to_le_bytes());
            rom
        }

        /// The Super FX cartridge with the cartridge CPU's trace on, run
        /// three frames.
        #[cfg(feature = "chip-superfx")]
        fn super_fx_traced() -> RetroEmulator {
            let mut emu = RetroEmulator::new("snes").unwrap();
            emu.load_rom(&super_fx_rom()).unwrap();
            emu.set_superfx_tracing_enabled(true);
            for _ in 0..3 {
                emu.run_frame();
            }
            emu
        }

        /// The switch on the program, which has no cartridge CPU: set, it stays
        /// off under both names of the cartridge CPU's trace; then cleared.
        #[wasm_bindgen_test]
        fn setting_the_super_fx_trace_reads_as_before() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            emu.load_rom(&debugger_program_rom()).unwrap();
            emu.set_superfx_tracing_enabled(true);
            let mut switch = vec![
                JsValue::from_bool(emu.is_superfx_tracing_enabled()),
                JsValue::from_bool(emu.is_hg51b_tracing_enabled()),
            ];
            emu.set_superfx_tracing_enabled(false);
            switch.push(JsValue::from_bool(emu.is_superfx_tracing_enabled()));
            let expected = ["false", "false", "false"];
            assert_eq!(shapes(&switch), expected);
        }

        /// Off at first.
        #[wasm_bindgen_test]
        fn the_super_fx_trace_switch_reads_as_before() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            let switch = [JsValue::from_bool(emu.is_superfx_tracing_enabled())];
            let expected = ["false"];
            assert_eq!(shapes(&switch), expected);
        }

        /// What the Super FX cartridge's GSU traced, then the same reader on
        /// the program, which has no cartridge CPU, with the trace on.
        #[cfg(feature = "chip-superfx")]
        #[wasm_bindgen_test]
        fn the_super_fx_trace_data_reads_as_before() {
            let mut emu = super_fx_traced();
            let mut data = chunk(&emu.get_superfx_trace_flush_data(), TRACE_ENTRY_SIZE_GSU);
            let mut plain = RetroEmulator::new("snes").unwrap();
            plain.load_rom(&debugger_program_rom()).unwrap();
            plain.set_superfx_tracing_enabled(true);
            plain.run_frame();
            data.push(byte_array(&plain.get_superfx_trace_flush_data()));
            let expected = [
                "208",
                "Uint8Array(len 52, fnv1a64 0xfa42a3a7c400d865)",
                "Uint8Array(len 208, fnv1a64 0x89b43a09edfd5a65)",
                "Uint8Array(len 0, fnv1a64 0xcbf29ce484222325)",
            ];
            assert_eq!(shapes(&data), expected);
        }

        /// Once cleared, the chunk is empty and the switch stays on.
        #[cfg(feature = "chip-superfx")]
        #[wasm_bindgen_test]
        fn clearing_the_super_fx_trace_reads_as_before() {
            let mut emu = super_fx_traced();
            emu.clear_superfx_trace_flushed();
            let data = [
                byte_array(&emu.get_superfx_trace_flush_data()),
                JsValue::from_bool(emu.is_superfx_tracing_enabled()),
            ];
            let expected = ["Uint8Array(len 0, fnv1a64 0xcbf29ce484222325)", "true"];
            assert_eq!(shapes(&data), expected);
        }

        /// The Super FX's entry size in bytes.
        #[wasm_bindgen_test]
        fn the_super_fx_trace_entry_size_reads_as_before() {
            let emu = RetroEmulator::new("snes").unwrap();
            let size = [number(emu.get_superfx_trace_entry_size())];
            let expected = ["52"];
            assert_eq!(shapes(&size), expected);
        }

        /// Two entries built field by field (an `IWT` with only the running
        /// flag set, a `STOP` with every status flag the line shows set) as
        /// text, the same with ten bytes of a third after them, and nothing.
        #[cfg(feature = "chip-superfx")]
        #[wasm_bindgen_test]
        fn the_super_fx_trace_text_reads_as_before() {
            let mut registers = [0u16; 16];
            registers[0] = 0x1234;
            registers[15] = 0x0104;
            let first = TraceEntryBinaryGsu {
                master_cycle: 0,
                pbr_pc: 0x00_0100,
                sfr: 0x0020,
                rombr: 0,
                rambr: 0,
                bytes_len: 3,
                bytes: [0xF0, 0x34, 0x12],
                r: registers,
            };
            let second = TraceEntryBinaryGsu {
                master_cycle: 0x0123_4567_89AB_CDEF,
                pbr_pc: 0x5F_FFFF,
                sfr: 0x9F7E,
                rombr: 0x7E,
                rambr: 1,
                bytes_len: 1,
                bytes: [0x00, 0x00, 0x00],
                r: std::array::from_fn(|i| (0x1000 + i * 0x111) as u16),
            };
            let mut data = first.to_bytes().to_vec();
            data.extend_from_slice(&second.to_bytes());
            let mut longer = data.clone();
            longer.extend_from_slice(&data[..10]);
            let emu = RetroEmulator::new("snes").unwrap();
            let texts: Vec<JsValue> = [&data[..], &longer[..], &[]]
                .iter()
                .map(|chunk| JsValue::from_str(&emu.convert_superfx_trace_chunk_to_text(chunk)))
                .collect();
            let expected = [
                concat!(
                    r#""000100 iwt r0,#$1234        R0:1234 R1:0000 R2:0000 R3:0000 R4:0000 "#,
                    r#"R5:0000 R6:0000 R7:0000 R8:0000 R9:0000 R10:0000 R11:0000 R12:0000 "#,
                    r#"R13:0000 R14:0000 R15:0104 SFR:0020 .......g....\n5fffff stop           "#,
                    r#"      R0:1000 R1:1111 R2:1222 R3:1333 R4:1444 R5:1555 R6:1666 R7:1777 "#,
                    r#"R8:1888 R9:1999 R10:1aaa R11:1bbb R12:1ccc R13:1ddd R14:1eee R15:1fff "#,
                    r#"SFR:9f7e ibhl21rgvscz\n""#,
                ),
                concat!(
                    r#""000100 iwt r0,#$1234        R0:1234 R1:0000 R2:0000 R3:0000 R4:0000 "#,
                    r#"R5:0000 R6:0000 R7:0000 R8:0000 R9:0000 R10:0000 R11:0000 R12:0000 "#,
                    r#"R13:0000 R14:0000 R15:0104 SFR:0020 .......g....\n5fffff stop           "#,
                    r#"      R0:1000 R1:1111 R2:1222 R3:1333 R4:1444 R5:1555 R6:1666 R7:1777 "#,
                    r#"R8:1888 R9:1999 R10:1aaa R11:1bbb R12:1ccc R13:1ddd R14:1eee R15:1fff "#,
                    r#"SFR:9f7e ibhl21rgvscz\n""#,
                ),
                r#""""#,
            ];
            assert_eq!(shapes(&texts), expected);
        }

        /// A Cx4 cartridge whose main CPU points the Cx4 at a program at
        /// `$01:8000` (`LD A,#$12`, `ST RAMDATA,A`, `WRRAML`, `HALT`),
        /// starts it, then waits in a loop.
        #[cfg(feature = "chip-cx4")]
        fn cx4_rom() -> Vec<u8> {
            let mut rom = vec![0u8; 0x20_0000];
            let start: [u8; 20] = [
                0xA9, 0x00, 0x8D, 0x49, 0x7F, // LDA #$00, STA $7F49: program offset, low
                0xA9, 0x80, 0x8D, 0x4A, 0x7F, // LDA #$80, STA $7F4A: middle
                0xA9, 0x01, 0x8D, 0x4B, 0x7F, // LDA #$01, STA $7F4B: bank
                0x9C, 0x4F, 0x7F, // STZ $7F4F: start at the program's first word
                0x80, 0xFE, // BRA -2
            ];
            rom[..start.len()].copy_from_slice(&start);
            for (i, word) in [0x6412u16, 0xE00C, 0xEC00, 0xFC00].iter().enumerate() {
                rom[0x8000 + 2 * i..0x8000 + 2 * i + 2].copy_from_slice(&word.to_le_bytes());
            }
            let header = 0x7FB0;
            rom[header + 0x10..header + 0x25].copy_from_slice(b"CX4 TRACE            ");
            rom[header + 0x25] = 0x20; // LoROM
            rom[header + 0x26] = 0xF3; // a Cx4 cartridge
            rom[header + 0x27] = 0x0B; // 2 MiB
            rom[header + 0x29] = 0x01; // USA, NTSC
            rom[header + 0x2A] = 0x01;
            rom[header + 0x2C..header + 0x2E].copy_from_slice(&(!0x1234u16).to_le_bytes());
            rom[header + 0x2E..header + 0x30].copy_from_slice(&0x1234u16.to_le_bytes());
            rom[header + 0x4C..header + 0x4E].copy_from_slice(&0x8000u16.to_le_bytes());
            rom
        }

        /// The Cx4 cartridge with the cartridge CPU's trace on, run two
        /// frames.
        #[cfg(feature = "chip-cx4")]
        fn cx4_traced() -> RetroEmulator {
            let mut emu = RetroEmulator::new("snes").unwrap();
            emu.load_rom(&cx4_rom()).unwrap();
            emu.set_hg51b_tracing_enabled(true);
            for _ in 0..2 {
                emu.run_frame();
            }
            emu
        }

        /// The switch on the program, which has no cartridge CPU: set, it stays
        /// off under both names of the cartridge CPU's trace; then cleared.
        #[wasm_bindgen_test]
        fn setting_the_hg51b_trace_reads_as_before() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            emu.load_rom(&debugger_program_rom()).unwrap();
            emu.set_hg51b_tracing_enabled(true);
            let mut switch = vec![
                JsValue::from_bool(emu.is_hg51b_tracing_enabled()),
                JsValue::from_bool(emu.is_superfx_tracing_enabled()),
            ];
            emu.set_hg51b_tracing_enabled(false);
            switch.push(JsValue::from_bool(emu.is_hg51b_tracing_enabled()));
            let expected = ["false", "false", "false"];
            assert_eq!(shapes(&switch), expected);
        }

        /// Off at first.
        #[wasm_bindgen_test]
        fn the_hg51b_trace_switch_reads_as_before() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            let switch = [JsValue::from_bool(emu.is_hg51b_tracing_enabled())];
            let expected = ["false"];
            assert_eq!(shapes(&switch), expected);
        }

        /// What the Cx4 cartridge's chip traced, the chip running as it runs
        /// untraced, then the same reader on the program, which has no
        /// cartridge CPU, with the trace on.
        #[cfg(feature = "chip-cx4")]
        #[wasm_bindgen_test]
        fn the_hg51b_trace_data_reads_as_before() {
            let mut emu = cx4_traced();
            let mut data = chunk(&emu.get_hg51b_trace_flush_data(), TRACE_ENTRY_SIZE_CX4);
            let mut plain = RetroEmulator::new("snes").unwrap();
            plain.load_rom(&debugger_program_rom()).unwrap();
            plain.set_hg51b_tracing_enabled(true);
            plain.run_frame();
            data.push(byte_array(&plain.get_hg51b_trace_flush_data()));
            let expected = [
                "372",
                "Uint8Array(len 93, fnv1a64 0xfd09fdf8879cc4b8)",
                "Uint8Array(len 372, fnv1a64 0x80484c1efe13ed24)",
                "Uint8Array(len 0, fnv1a64 0xcbf29ce484222325)",
            ];
            assert_eq!(shapes(&data), expected);
        }

        /// Once cleared, the chunk is empty and the switch stays on.
        #[cfg(feature = "chip-cx4")]
        #[wasm_bindgen_test]
        fn clearing_the_hg51b_trace_reads_as_before() {
            let mut emu = cx4_traced();
            emu.clear_hg51b_trace_flushed();
            let data = [
                byte_array(&emu.get_hg51b_trace_flush_data()),
                JsValue::from_bool(emu.is_hg51b_tracing_enabled()),
            ];
            let expected = ["Uint8Array(len 0, fnv1a64 0xcbf29ce484222325)", "true"];
            assert_eq!(shapes(&data), expected);
        }

        /// The Cx4's entry size in bytes.
        #[wasm_bindgen_test]
        fn the_hg51b_trace_entry_size_reads_as_before() {
            let emu = RetroEmulator::new("snes").unwrap();
            let size = [number(emu.get_hg51b_trace_entry_size())];
            let expected = ["93"];
            assert_eq!(shapes(&size), expected);
        }

        /// Two entries built field by field (a load with every register
        /// clear, a `HALT` with every flag set) as text, the same with ten
        /// bytes of a third after them, and nothing.
        #[cfg(feature = "chip-cx4")]
        #[wasm_bindgen_test]
        fn the_hg51b_trace_text_reads_as_before() {
            let first = TraceEntryBinaryCx4 {
                cycles: 0,
                pc: 0,
                opcode: 0x6412,
                p: 0,
                flags: 0,
                a: 0,
                acch: 0,
                accl: 0,
                gpr: [0; 16],
            };
            let second = TraceEntryBinaryCx4 {
                cycles: 0x00FF_FFFF_FFFF,
                pc: 0x7F_FFFF,
                opcode: 0xFC00,
                p: 0x7FFF,
                flags: 0xF,
                a: 0xFF_FFFF,
                acch: 0x12_3456,
                accl: 0x65_4321,
                gpr: std::array::from_fn(|i| 0xFF_FFFF - i as u32),
            };
            let mut data = first.to_bytes().to_vec();
            data.extend_from_slice(&second.to_bytes());
            let mut longer = data.clone();
            longer.extend_from_slice(&data[..10]);
            let emu = RetroEmulator::new("snes").unwrap();
            let texts: Vec<JsValue> = [&data[..], &longer[..], &[]]
                .iter()
                .map(|chunk| JsValue::from_str(&emu.convert_hg51b_trace_chunk_to_text(chunk)))
                .collect();
            let expected = [
                concat!(
                    r#""000000 ld a,#$12            A:000000 P:0000 R0:000000 R1:000000 "#,
                    r#"R2:000000 R3:000000 R4:000000 R5:000000 R6:000000 R7:000000 R8:000000 "#,
                    r#"R9:000000 R10:000000 R11:000000 R12:000000 R13:000000 R14:000000 "#,
                    r#"R15:000000 ....\n7fffff halt                 A:ffffff P:7fff R0:ffffff "#,
                    r#"R1:fffffe R2:fffffd R3:fffffc R4:fffffb R5:fffffa R6:fffff9 R7:fffff8 "#,
                    r#"R8:fffff7 R9:fffff6 R10:fffff5 R11:fffff4 R12:fffff3 R13:fffff2 "#,
                    r#"R14:fffff1 R15:fffff0 nzvc\n""#,
                ),
                concat!(
                    r#""000000 ld a,#$12            A:000000 P:0000 R0:000000 R1:000000 "#,
                    r#"R2:000000 R3:000000 R4:000000 R5:000000 R6:000000 R7:000000 R8:000000 "#,
                    r#"R9:000000 R10:000000 R11:000000 R12:000000 R13:000000 R14:000000 "#,
                    r#"R15:000000 ....\n7fffff halt                 A:ffffff P:7fff R0:ffffff "#,
                    r#"R1:fffffe R2:fffffd R3:fffffc R4:fffffb R5:fffffa R6:fffff9 R7:fffff8 "#,
                    r#"R8:fffff7 R9:fffff6 R10:fffff5 R11:fffff4 R12:fffff3 R13:fffff2 "#,
                    r#"R14:fffff1 R15:fffff0 nzvc\n""#,
                ),
                r#""""#,
            ];
            assert_eq!(shapes(&texts), expected);
        }

        /// The property `key` of `value`.
        fn property(value: &JsValue, key: &str) -> JsValue {
            js_sys::Reflect::get(value, &JsValue::from_str(key)).unwrap()
        }

        /// Each event's code with its kind, the name a host finds the event
        /// by whatever the console numbers it.
        #[wasm_bindgen_test]
        fn every_event_names_its_kind() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            let kinds: Vec<(u32, String)> = js_sys::Array::from(&emu.get_available_events())
                .iter()
                .map(|event| {
                    let code = property(&event, "code").as_f64().unwrap() as u32;
                    (code, property(&event, "kind").as_string().unwrap())
                })
                .collect();
            let expected = [
                (1, "vertical_blank"),
                (2, "vertical_blank_interrupt"),
                (3, "raster_interrupt"),
                (4, "transfer_start"),
                (5, "line_transfer_set_up"),
                (6, "line"),
                (7, "video_register_write"),
                (8, "audio_port_write"),
                (9, "audio_register_write"),
                (10, "transfer_done"),
                (11, "palette_write"),
                (12, "tile_write"),
            ]
            .map(|(code, kind)| (code, kind.to_string()));
            assert_eq!(kinds, expected);
        }

        /// The history reads the newest record first: after the two steps,
        /// the store, then the load; one step more puts the `RTL` first.
        #[wasm_bindgen_test]
        fn the_history_reads_newest_first() {
            let mut emu = debugging_the_program();
            let addresses = |emu: &mut RetroEmulator| -> Vec<u32> {
                js_sys::Array::from(&emu.get_instruction_history(0, 16))
                    .iter()
                    .map(|entry| property(&entry, "address").as_f64().unwrap() as u32)
                    .collect()
            };
            assert_eq!(addresses(&mut emu), [0x00_8022, 0x00_8020]);
            step_into(&mut emu);
            assert_eq!(addresses(&mut emu), [0x00_8025, 0x00_8022, 0x00_8020]);
        }

        /// A stop on the instrumented loop refreshes the CPU states: the
        /// main CPU reads where it stopped.
        #[wasm_bindgen_test]
        fn the_cpu_states_are_fresh_after_a_stop() {
            let mut emu = stopped_at_the_breakpoint();
            let states = js_sys::Array::from(&emu.get_cpu_states());
            let pc = property(&states.get(0), "pc").as_f64().unwrap() as u32;
            assert_eq!(pc, 0x00_8020);
        }

        /// OAM reads as the sprite table and CGRAM as the palette.
        #[wasm_bindgen_test]
        fn the_sprite_table_and_the_palette_are_named_by_kind() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            let regions: Vec<(String, String)> = js_sys::Array::from(&emu.get_memory_regions())
                .iter()
                .map(|region| {
                    (
                        property(&region, "name").as_string().unwrap(),
                        property(&region, "region").as_string().unwrap_or_default(),
                    )
                })
                .collect();
            assert!(regions.contains(&("OAM".to_string(), "SpriteTable".to_string())));
            assert!(regions.contains(&("CGRAM".to_string(), "PaletteRam".to_string())));
        }

        /// On the program, which has no cartridge CPU, the cartridge CPU's
        /// trace switches stay off under both names, and a frame runs on
        /// the fast loop.
        #[wasm_bindgen_test]
        fn a_cartridge_without_a_cpu_that_traces_keeps_its_switches_off() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            emu.load_rom(&debugger_program_rom()).unwrap();
            emu.set_superfx_tracing_enabled(true);
            emu.set_hg51b_tracing_enabled(true);
            assert!(!emu.is_superfx_tracing_enabled());
            assert!(!emu.is_hg51b_tracing_enabled());
            emu.run_frame();
            let tier = emu.snes.debugger_ref().execution_tier();
            assert_eq!(tier, retro_core::ExecutionTier::Play);
        }

        /// On a Super FX cartridge the switch is on.
        #[cfg(feature = "chip-superfx")]
        #[wasm_bindgen_test]
        fn a_super_fx_cartridge_takes_the_cartridge_cpu_trace() {
            let mut emu = super_fx_traced();
            assert!(emu.is_superfx_tracing_enabled());
        }

        /// A traced Cx4 runs as an untraced one: the machine after the two
        /// frames is the same.
        #[cfg(feature = "chip-cx4")]
        #[wasm_bindgen_test]
        fn tracing_the_hg51b_leaves_the_machine_as_it_runs_untraced() {
            let traced = cx4_traced();
            let mut untraced = RetroEmulator::new("snes").unwrap();
            untraced.load_rom(&cx4_rom()).unwrap();
            for _ in 0..2 {
                untraced.run_frame();
            }
            assert_eq!(traced.get_state_hash(), untraced.get_state_hash());
        }

        /// `step()` is `stepCpu` of the focused CPU: the same result, the
        /// same machine, the same history; focused on the sound CPU, which
        /// no host steps, it runs nothing.
        #[wasm_bindgen_test]
        fn a_step_is_a_step_of_the_focused_cpu() {
            let mut by_step = debugging_the_program();
            let mut by_cpu = debugging_the_program();
            let focused = by_cpu.get_focused_cpu();
            let results = [by_step.step(), by_cpu.step_cpu(focused)];
            assert_eq!(shapes(&results[..1]), shapes(&results[1..]));
            assert_eq!(by_step.get_state_hash(), by_cpu.get_state_hash());
            assert_eq!(
                shapes(&[by_step.get_instruction_history(0, 16)]),
                shapes(&[by_cpu.get_instruction_history(0, 16)])
            );
            by_step.set_focused_cpu(1);
            let idle = property(&by_step.step(), "instruction_executed");
            assert_eq!(idle.as_bool(), Some(false));
        }

        /// A run from a stop at an execute breakpoint goes on: the
        /// instruction at the breakpoint runs, the program returns to its
        /// loop, and the frame ends with the debugger still running.
        #[wasm_bindgen_test]
        fn a_run_goes_on_from_an_execute_breakpoint() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            emu.load_rom(&debugger_program_rom()).unwrap();
            emu.add_breakpoint(0x00_8020, "execute".to_string(), None);
            emu.run_frame();
            assert_eq!(emu.get_execution_mode(), "Paused");
            emu.set_execution_mode("Running".to_string());
            emu.run_frame();
            assert_eq!(emu.get_execution_mode(), "Running");
        }

        /// A host's step keeps the history and the call stack: two steps
        /// through the `JSR` and the `JSL` leave two records and two calls.
        #[wasm_bindgen_test]
        fn a_host_step_fills_the_history_and_the_call_stack() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            emu.load_rom(&debugger_program_rom()).unwrap();
            emu.step_cpu(0);
            emu.step_cpu(0);
            let history = js_sys::Array::from(&emu.get_instruction_history(0, 16));
            let stack = js_sys::Array::from(&emu.get_call_stack(0));
            assert_eq!((history.length(), stack.length()), (2, 2));
        }

        /// The call stack is empty after a reset, a ROM load and a state
        /// load; the history, which records what ran, stays.
        #[wasm_bindgen_test]
        fn the_call_stack_is_empty_after_a_reset_or_a_load() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            emu.load_rom(&debugger_program_rom()).unwrap();
            let state = emu.save_state();
            let into_calls = |emu: &mut RetroEmulator| {
                emu.step_cpu(0);
                emu.step_cpu(0);
                assert_eq!(js_sys::Array::from(&emu.get_call_stack(0)).length(), 2);
            };
            let lengths = |emu: &mut RetroEmulator| {
                (
                    js_sys::Array::from(&emu.get_call_stack(0)).length(),
                    js_sys::Array::from(&emu.get_instruction_history(0, 64)).length(),
                )
            };
            into_calls(&mut emu);
            emu.reset();
            assert_eq!(lengths(&mut emu), (0, 2));
            into_calls(&mut emu);
            emu.load_rom(&debugger_program_rom()).unwrap();
            assert_eq!(lengths(&mut emu), (0, 4));
            into_calls(&mut emu);
            emu.load_state(&state).unwrap();
            assert_eq!(lengths(&mut emu), (0, 6));
        }

        /// What each CPU checks: the main CPU everything, the sound CPU and
        /// the cartridge CPU nothing.
        #[wasm_bindgen_test]
        fn the_breakpoint_checks_read_what_each_cpu_checks() {
            let emu = RetroEmulator::new("snes").unwrap();
            let checks = [
                emu.get_breakpoint_checks(0),
                emu.get_breakpoint_checks(1),
                emu.get_breakpoint_checks(2),
                emu.get_breakpoint_checks(7),
            ];
            let expected = [
                "{conditions: true, execute: true, memory: true}",
                "{conditions: false, execute: false, memory: false}",
                "{conditions: false, execute: false, memory: false}",
                "{conditions: false, execute: false, memory: false}",
            ];
            assert_eq!(shapes(&checks), expected);
        }

        /// The main CPU's trace has no ring, so no capacity, until its
        /// trace is first turned on.
        #[wasm_bindgen_test]
        fn the_trace_stats_read_no_capacity_until_traced() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            emu.load_rom(&debugger_program_rom()).unwrap();
            let before = property(&emu.get_trace_stats(), "buffer_capacity");
            assert_eq!(before.as_f64(), Some(0.0));
            emu.set_tracing_enabled(true);
            emu.run_frame();
            let after = property(&emu.get_trace_stats(), "buffer_capacity");
            assert!(after.as_f64().unwrap() > 0.0);
        }

        /// Clearing a CPU's coverage forgets every address it ran.
        #[wasm_bindgen_test]
        fn clearing_the_coverage_forgets_it() {
            let mut emu = RetroEmulator::new("snes").unwrap();
            emu.load_rom(&debugger_program_rom()).unwrap();
            emu.step_cpu(0);
            emu.step_cpu(0);
            let covered = |emu: &RetroEmulator| {
                property(&emu.get_coverage_stats(0), "coveredBytes")
                    .as_f64()
                    .unwrap()
            };
            assert_eq!(covered(&emu), 2.0);
            emu.clear_coverage(0);
            assert_eq!(covered(&emu), 0.0);
        }
    }
}
