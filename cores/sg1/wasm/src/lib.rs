//! WebAssembly bindings for the SG1 emulator.
//!
//! This crate provides a generic WASM interface that works with any
//! `EmulatorCore` implementation.

use retro_core::capture::CaptureCore;
use retro_core::input::{gamepad_map_from_text_keys, gamepad_map_with_text_keys};
use retro_core::*;
use retro_wasm::js::{map_as_object, set_property, uint16_array};
use sg1_core::cartridge::composable::parse_manifest;
use sg1_core::system::GenesisSnapshotConsole;
use sg1_core::GenesisEmulator;
use wasm_bindgen::prelude::*;

/// Initialize panic hook for better error messages in browser console.
#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
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

/// UI badge label for a Genesis mapper. retro-core's `MapperId` enumerates
/// SNES mappers, so every Genesis shape arrives as `Custom(plan_name)`; a
/// plain cartridge (or no cartridge) reads as `"None"` so the mapper viewer
/// hides its register/bank tables.
fn genesis_mapper_type_label(id: &retro_core::cartridge::MapperId) -> String {
    use retro_core::cartridge::MapperId;
    match id {
        MapperId::Custom(name) => match name.as_str() {
            "" | "plain" => "None".to_string(),
            "sega" => "SEGA".to_string(),
            "ssf2" => "SSF2".to_string(),
            "codemasters" => "Codemasters".to_string(),
            "eeprom" => "EEPROM".to_string(),
            "jcart" => "J-Cart".to_string(),
            "svp" => "SVP".to_string(),
            other => other.to_string(),
        },
        other => format!("{other:?}"),
    }
}

/// Map a live [`retro_core::cartridge::MapperState`] into the
/// `{ mapper_type, registers, banks }` JSON shape a host's mapper viewer
/// reads (retro-core nests banks under `bank_config` and keys the mapper
/// by an enum; the viewer wants a flat, string-keyed object).
fn mapper_state_json(state: Option<retro_core::cartridge::MapperState>) -> serde_json::Value {
    match state {
        Some(m) => serde_json::json!({
            "mapper_type": genesis_mapper_type_label(&m.mapper_id),
            "registers": m.registers,
            "banks": m.bank_config.banks,
        }),
        None => serde_json::json!({
            "mapper_type": "None",
            "registers": [],
            "banks": [],
        }),
    }
}

/// Generic retro emulator wrapper for WASM.
/// Currently supports Genesis/Mega Drive only.
#[wasm_bindgen]
pub struct RetroEmulator {
    genesis: GenesisEmulator,
}

#[wasm_bindgen]
impl RetroEmulator {
    /// Create a new emulator for the specified system.
    #[wasm_bindgen(constructor)]
    pub fn new(system: &str) -> Result<RetroEmulator, JsValue> {
        match system {
            "genesis" | "megadrive" | "md" => Ok(RetroEmulator {
                genesis: GenesisEmulator::new(),
            }),
            _ => Err(JsValue::from_str(&format!(
                "Unknown system: '{}'. Supported: genesis, megadrive, md",
                system
            ))),
        }
    }

    /// Get system information and metadata.
    #[wasm_bindgen(js_name = getSystemInfo)]
    pub fn get_system_info(&self) -> JsValue {
        serde_wasm_bindgen::to_value(self.genesis.system_info()).unwrap()
    }

    /// Reset the emulator (CPU, Z80, VDP, PSG, YM2612).
    /// Cartridge ROM and SRAM are preserved, matching real hardware reset behavior.
    pub fn reset(&mut self) {
        self.genesis.reset();
    }

    /// Power-on (cold) reset: a power cycle, not the reset button. Rebuilds the
    /// whole machine to a freshly-constructed + reloaded state, clearing every
    /// volatile subsystem a soft reset leaves dirty (work/Z80 RAM, the 68K user
    /// stack pointer, VDP VRAM/CRAM/VSRAM, the audio chips, and the I/O + bus
    /// latches). Netplay uses this so two peers derive a byte-identical frame-0
    /// state hash regardless of how long each ran the game before the session.
    /// Cartridge ROM is preserved; SRAM is a fresh image (a save resumes via the
    /// seeded savestate transfer).
    #[wasm_bindgen(js_name = powerOnReset)]
    pub fn power_on_reset(&mut self) {
        self.genesis.power_on_reset();
    }

    /// Hard reset (power cycle): clear every volatile subsystem the reset
    /// button leaves behind (work RAM, Z80 RAM, the VDP register file plus
    /// VRAM/CRAM/VSRAM, the audio chips, and the I/O + bus latches), while
    /// keeping the cartridge plugged in, so battery-backed SRAM survives.
    ///
    /// This is what a player means by "reset the game"; [`Self::reset`] is
    /// the hardware reset *button*, which preserves RAM. Unlike
    /// [`Self::power_on_reset`] (netplay's byte-identical cold boot) this
    /// does NOT blank SRAM, so it is safe to expose in the UI.
    #[wasm_bindgen(js_name = hardReset)]
    pub fn hard_reset(&mut self) {
        self.genesis.hard_reset();
    }

    /// Load a ROM file.
    #[wasm_bindgen(js_name = loadRom)]
    pub fn load_rom(&mut self, data: &[u8]) -> Result<(), JsValue> {
        self.genesis
            .load_rom(data)
            .map_err(|e| JsValue::from_str(&e))
    }

    /// Get ROM information.
    #[wasm_bindgen(js_name = getRomInfo)]
    pub fn get_rom_info(&self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.genesis.get_rom_info()).unwrap()
    }

    // ===== Cartridge Methods =====

    /// Get cartridge information (title, mapper, chip, etc).
    #[wasm_bindgen(js_name = getCartridgeInfo)]
    pub fn get_cartridge_info(&self) -> Result<JsValue, JsValue> {
        if let Some(info) = self.genesis.get_rom_info() {
            Ok(serde_wasm_bindgen::to_value(&info)?)
        } else {
            Err(JsValue::from_str("No cartridge loaded"))
        }
    }

    /// Step one instruction.
    pub fn step(&mut self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.genesis.step()).unwrap()
    }

    /// Run one frame with automatic path selection:
    /// - Fast path: no debug features active (highest performance)
    /// - Light debug: breakpoints and/or trace active in Running mode (minimal overhead)
    /// - Full debug: Paused/StepInto/StepOver modes (full disassembly, history, state updates)
    #[wasm_bindgen(js_name = runFrame)]
    pub fn run_frame(&mut self) {
        match self.genesis.debugger_ref().execution_tier() {
            ExecutionTier::Fast => self.genesis.run_frame_fast(),
            ExecutionTier::LightDebug => self.genesis.run_frame_debug_light(),
            ExecutionTier::FullDebug => self.genesis.run_frame(),
        }
    }

    /// Run to the start of vertical blanking and stop there, on the full
    /// debug loop: `runToCaptureBoundary` on that loop.
    #[wasm_bindgen(js_name = runUntilVBlank)]
    pub fn run_until_vblank(&mut self) {
        self.genesis.run_until_vblank();
    }

    // ===== Netplay determinism primitives =====

    /// Run one full frame headlessly: the complete deterministic
    /// simulation with the audio **mix** and the host-visible framebuffer
    /// copy skipped. Audio synthesis itself still runs, the rate
    /// converter and each chip's accumulators are serialized state the
    /// netplay hash covers, so freezing them here would desync a peer that
    /// rolled back against one that did not. What a headless frame drops is
    /// the mixed result, which is why it must never be used for a frame the
    /// player has not heard yet. Always the fast path, it bypasses the
    /// debug-tier dispatch that `runFrame` performs.
    #[wasm_bindgen(js_name = runFrameHeadless)]
    pub fn run_frame_headless(&mut self) {
        self.genesis.run_frame_headless();
    }

    /// Canonical hash of the full deterministic machine state. Two peers
    /// in the same state hash to the same value; a mismatch is a desync.
    /// Maps to a JS `bigint`.
    #[wasm_bindgen(js_name = getStateHash)]
    pub fn get_state_hash(&self) -> u64 {
        self.genesis.state_hash()
    }

    /// Per-subsystem hashes of the canonical savestate, as a JSON object of hex
    /// strings (`{"cpu68k":"..","ram":"..","cartridge":"..",...}`), for netplay
    /// desync triage: when two peers disagree on [`Self::get_state_hash`] this
    /// pinpoints which subsystem's serialized state diverged (e.g. `ram` vs
    /// `cartridge`/SRAM at a refused committed start). Diagnostic only.
    #[wasm_bindgen(js_name = getStateHashParts)]
    pub fn get_state_hash_parts(&self) -> String {
        let mut json = String::from("{");
        for (i, (name, hash)) in self.genesis.state_hash_parts().iter().enumerate() {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&format!("\"{name}\":\"{hash:x}\""));
        }
        json.push('}');
        json
    }

    /// Authoritative frame counter from the core (the serialized
    /// `frame_count`), not the JS-side frame ref. Maps to a JS `bigint`.
    #[wasm_bindgen(js_name = getFrameNumber)]
    pub fn get_frame_number(&self) -> u64 {
        self.genesis.frame_count()
    }

    /// Set the video region from `"ntsc"` | `"pal"` (case-insensitive).
    /// Call right after `loadRom`, before running frames: both netplay
    /// peers agree on region in the session handshake.
    #[wasm_bindgen(js_name = setRegion)]
    pub fn set_region(&mut self, region: &str) -> Result<(), JsValue> {
        self.genesis
            .set_region_str(region)
            .map_err(|e| JsValue::from_str(&e))
    }

    /// Current video region as `"ntsc"` | `"pal"`.
    #[wasm_bindgen(js_name = getRegion)]
    pub fn get_region(&self) -> String {
        self.genesis.region_str().to_string()
    }

    /// Core build identity for the netplay handshake, e.g.
    /// `"sg1@0.1.0+ab12cd3"`. Two peers refuse to connect on a mismatch,
    /// since a different build can diverge deterministically. The git
    /// hash is injected by `build.rs` and is `"unknown"` outside a git
    /// checkout.
    #[wasm_bindgen(js_name = getBuildId)]
    pub fn get_build_id(&self) -> String {
        format!(
            "sg1@{}+{}",
            env!("CARGO_PKG_VERSION"),
            option_env!("GIT_HASH").unwrap_or("unknown")
        )
    }

    /// Get the framebuffer as a JavaScript object (SLOW -- uses serde, kept for compatibility).
    /// Prefer getFramebufferData/Width/Height for rendering.
    #[wasm_bindgen(js_name = getFramebuffer)]
    pub fn get_framebuffer(&self) -> JsValue {
        serde_wasm_bindgen::to_value(self.genesis.get_framebuffer()).unwrap()
    }

    /// Get the framebuffer pixel data as a Uint8ClampedArray (FAST -- direct memcpy).
    /// The returned array is RGBA format, ready for use with ImageData.
    #[wasm_bindgen(js_name = getFramebufferData)]
    pub fn get_framebuffer_data(&self) -> js_sys::Uint8ClampedArray {
        let fb = self.genesis.get_framebuffer();
        let arr = js_sys::Uint8ClampedArray::new_with_length(fb.data.len() as u32);
        arr.copy_from(&fb.data);
        arr
    }

    /// Get the framebuffer width in pixels.
    #[wasm_bindgen(js_name = getFramebufferWidth)]
    pub fn get_framebuffer_width(&self) -> u32 {
        self.genesis.get_framebuffer().width
    }

    /// Get the framebuffer height in pixels.
    #[wasm_bindgen(js_name = getFramebufferHeight)]
    pub fn get_framebuffer_height(&self) -> u32 {
        self.genesis.get_framebuffer().height
    }

    /// Get the framebuffer data pointer in WASM linear memory (ZERO-COPY).
    /// Use with getFramebufferLen() to create a JS Uint8Array view:
    ///   new Uint8Array(wasm.memory.buffer, ptr, len)
    /// The view is live (no copy), but must be recreated if WASM memory grows.
    #[wasm_bindgen(js_name = getFramebufferPtr)]
    pub fn get_framebuffer_ptr(&self) -> *const u8 {
        self.genesis.get_framebuffer().data.as_ptr()
    }

    /// Get the framebuffer data length in bytes.
    #[wasm_bindgen(js_name = getFramebufferLen)]
    pub fn get_framebuffer_len(&self) -> u32 {
        self.genesis.get_framebuffer().data.len() as u32
    }

    /// Display info: the size of the picture `getFramebuffer` holds,
    /// `width` 256 or 320 from the mode and `height` the lines the VDP
    /// draws into its framebuffer, which holds 224 (in the 30-cell mode too,
    /// whose last 16 lines it does not draw). It follows the framebuffer,
    /// refreshed with it each frame shown.
    #[wasm_bindgen(js_name = getDisplayInfo)]
    pub fn get_display_info(&self) -> JsValue {
        serde_wasm_bindgen::to_value(self.genesis.get_display_info()).unwrap()
    }

    /// Drain **every queued sample** from the audio ring and return
    /// a freshly-allocated `Float32Array` to JS.
    ///
    /// Drains destructively: each call empties the ring. The host
    /// must not also call `clearAudioBuffer` between drains. For
    /// tight host-controlled drains (e.g. an `AudioWorklet` that
    /// knows exactly how many samples it wants this tick), prefer
    /// `getAudioSamplesN(n)` instead.
    #[wasm_bindgen(js_name = getAudioSamples)]
    pub fn get_audio_samples(&mut self) -> Vec<f32> {
        self.genesis.get_audio_buffer()
    }

    /// Drain **at most `n`** queued samples from the audio ring.
    /// Returns a `Float32Array` of length `min(n, audio_available())`.
    ///
    /// Preferred over `getAudioSamples` for AudioWorklets that pull a
    /// fixed lookahead each tick, avoids over-drain (silence on the
    /// next tick) and over-allocate (returning a 5880-sample buffer
    /// when 4096 is all the worklet wanted).
    #[wasm_bindgen(js_name = getAudioSamplesN)]
    pub fn get_audio_samples_n(&mut self, n: usize) -> Vec<f32> {
        let available = self.genesis.audio_available();
        let take = n.min(available);
        let mut out = vec![0.0_f32; take];
        let written = self.genesis.drain_audio_into(&mut out);
        out.truncate(written);
        out
    }

    /// Number of samples currently queued in the audio ring (stereo
    /// interleaved: a value of `1470` means 735 stereo pairs ready).
    ///
    /// Used by the host to right-size a `getAudioSamplesN` pull.
    #[wasm_bindgen(js_name = getAudioAvailable)]
    pub fn get_audio_available(&self) -> usize {
        self.genesis.audio_available()
    }

    /// Cumulative count of samples the ring dropped from the front
    /// because incoming data wouldn't fit. > 0 -> the consumer (host
    /// worklet) is behind.
    ///
    /// There is no underrun counterpart, on purpose. The ring can only
    /// record an underrun when a drain asks for more than it holds, and
    /// every path out of here sizes its request from `audio_available()`
    /// first, so such a counter could never move, and the host would read
    /// a constant zero as though it were a measurement.
    #[wasm_bindgen(js_name = getAudioRingOverflows)]
    pub fn get_audio_ring_overflows(&self) -> u64 {
        self.genesis.audio_ring_overflows()
    }

    /// **Deprecated and intentionally a no-op.** `getAudioSamples`
    /// and `getAudioSamplesN` drain the ring destructively, so the
    /// host never needs to clear it. Calling this between drains
    /// races the producer and silently reintroduces audible tick
    /// artifacts. Kept exported only so JS callers that still
    /// invoke it don't break. **Do not call from new code.**
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
        self.genesis.discard_pending_audio();
    }

    /// Get audio system information
    #[wasm_bindgen(js_name = getAudioInfo)]
    pub fn get_audio_info(&self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.genesis.system_info().audio).unwrap()
    }

    /// Enable or disable audio generation.
    /// When disabled, audio chips still respond to register reads/writes,
    /// but no samples are generated. Default: disabled.
    #[wasm_bindgen(js_name = setAudioEnabled)]
    pub fn set_audio_enabled(&mut self, enabled: bool) {
        use retro_core::EmulatorCore;
        self.genesis.set_audio_enabled(enabled);
    }

    /// Check if audio generation is enabled.
    #[wasm_bindgen(js_name = isAudioEnabled)]
    pub fn is_audio_enabled(&self) -> bool {
        use retro_core::EmulatorCore;
        self.genesis.is_audio_enabled()
    }

    // ===== Audio Debugger Methods (Generic) =====

    /// Get audio chip metadata (generic for any chip).
    #[wasm_bindgen(js_name = getAudioChipInfo)]
    pub fn get_audio_chip_info(&self) -> JsValue {
        // Describes the PSG only; the YM2612 has calls of its own further down.
        use retro_core::audio::AudioChipInfo;

        let info = AudioChipInfo {
            name: "SN76489 PSG",
            manufacturer: "Texas Instruments",
            channels: 4,
            channel_types: vec![
                retro_core::audio::ChannelType::Tone,
                retro_core::audio::ChannelType::Tone,
                retro_core::audio::ChannelType::Tone,
                retro_core::audio::ChannelType::Noise,
            ],
            sample_rate: 44100,
            stereo: false,
        };

        serde_wasm_bindgen::to_value(&info).unwrap()
    }

    /// Get number of audio channels (generic).
    #[wasm_bindgen(js_name = getAudioChannelCount)]
    pub fn get_audio_channel_count(&self) -> u32 {
        use retro_core::audio::AudioDebugger;
        self.genesis.psg().get_channel_count()
    }

    /// Get state of a specific audio channel (generic).
    #[wasm_bindgen(js_name = getAudioChannelState)]
    pub fn get_audio_channel_state(&self, channel: u32) -> JsValue {
        use retro_core::audio::AudioDebugger;

        match self.genesis.psg().get_channel_state(channel) {
            Some(state) => serde_wasm_bindgen::to_value(&state).unwrap(),
            None => JsValue::NULL,
        }
    }

    /// Mute/unmute a specific audio channel (generic).
    #[wasm_bindgen(js_name = muteAudioChannel)]
    pub fn mute_audio_channel(&mut self, channel: u32, muted: bool) {
        use retro_core::audio::AudioDebugger;
        self.genesis.psg_mut().mute_channel(channel, muted);
    }

    /// Get waveform data for a specific channel (generic).
    #[wasm_bindgen(js_name = getAudioWaveform)]
    pub fn get_audio_waveform(&self, channel: u32, samples: u32) -> Vec<f32> {
        use retro_core::audio::AudioDebugger;
        self.genesis.psg().get_waveform(channel, samples)
    }

    /// Get mixed output waveform (actual PSG+YM2612 mix, mono).
    #[wasm_bindgen(js_name = getMixedWaveform)]
    pub fn get_mixed_waveform(&self, samples: u32) -> Vec<f32> {
        self.genesis.get_mixed_waveform(samples)
    }

    /// Set input for a player.
    #[wasm_bindgen(js_name = setInput)]
    pub fn set_input(&mut self, player: u8, buttons: u32) {
        self.genesis.set_input(player, InputState { buttons });
    }

    /// Save emulator state, where a frame ends: a frame held at a capture
    /// boundary or an event breakpoint is finished first, on the loop a
    /// player runs.
    #[wasm_bindgen(js_name = saveState)]
    pub fn save_state(&mut self) -> Vec<u8> {
        self.genesis.save_state_at_frame_end()
    }

    /// Load emulator state. A savestate made on another ROM is refused
    /// with an `Error` named `SaveStateOtherRom`.
    #[wasm_bindgen(js_name = loadState)]
    pub fn load_state(&mut self, data: &[u8]) -> Result<(), JsValue> {
        self.genesis
            .load_state_with_rom_check(data, RomCheck::SameRom)
            .map_err(load_state_refusal)
    }

    /// Load a savestate whatever ROM it was made on: for a player who chose
    /// to carry a state to another build of the game.
    #[wasm_bindgen(js_name = loadStateAnyRom)]
    pub fn load_state_any_rom(&mut self, data: &[u8]) -> Result<(), JsValue> {
        self.genesis
            .load_state_with_rom_check(data, RomCheck::AnyRom)
            .map_err(load_state_refusal)
    }

    /// Get save RAM.
    #[wasm_bindgen(js_name = getSaveRam)]
    pub fn get_save_ram(&self) -> Option<Vec<u8>> {
        self.genesis.get_save_ram().map(|s| s.to_vec())
    }

    /// Set save RAM.
    #[wasm_bindgen(js_name = setSaveRam)]
    pub fn set_save_ram(&mut self, data: &[u8]) {
        self.genesis.set_save_ram(data);
    }

    // ===== Cartridge Save State Methods =====

    /// Get cartridge save state
    /// Returns serialized cartridge state including SRAM, mapper, and chip state
    #[wasm_bindgen(js_name = getCartridgeSaveState)]
    pub fn get_cartridge_save_state(&self, frame_count: u64) -> Result<Vec<u8>, JsValue> {
        self.genesis
            .get_cartridge_save_state(frame_count)
            .map_err(|e| JsValue::from_str(&e))
    }

    /// Load cartridge save state
    #[wasm_bindgen(js_name = loadCartridgeSaveState)]
    pub fn load_cartridge_save_state(&mut self, data: &[u8]) -> Result<(), JsValue> {
        self.genesis
            .load_cartridge_save_state(data)
            .map_err(|e| JsValue::from_str(&e))
    }

    /// Get cartridge ROM hash (SHA-256)
    #[wasm_bindgen(js_name = getCartridgeHash)]
    pub fn get_cartridge_hash(&self) -> Option<String> {
        self.genesis.get_cartridge_hash()
    }

    /// Get cartridge title
    #[wasm_bindgen(js_name = getCartridgeTitle)]
    pub fn get_cartridge_title(&self) -> Option<String> {
        self.genesis.get_cartridge_title()
    }

    /// Check if cartridge has dirty SRAM (needs saving)
    #[wasm_bindgen(js_name = isCartridgeSramDirty)]
    pub fn is_cartridge_sram_dirty(&self) -> bool {
        self.genesis.is_cartridge_sram_dirty()
    }

    /// Clear cartridge SRAM dirty flag
    #[wasm_bindgen(js_name = clearCartridgeSramDirty)]
    pub fn clear_cartridge_sram_dirty(&mut self) {
        self.genesis.clear_cartridge_sram_dirty();
    }

    /// Get mapper state (registers + bank configuration) for the installed
    /// cartridge, as `{ mapper_type, registers, banks }`. Live via the
    /// cartridge debugger; a plain cartridge reports `mapper_type: "None"`.
    #[wasm_bindgen(js_name = getMapperState)]
    pub fn get_mapper_state(&mut self) -> Result<JsValue, JsValue> {
        use retro_core::cartridge::CartridgeDebugger;
        let state = self.genesis.cartridge_debugger().get_mapper_state();
        Ok(map_as_object(&mapper_state_json(state)))
    }

    /// Get the primary coprocessor's live debug snapshot (`chip_type`,
    /// `registers`, `internal_ram`, `program_counter`, `cycle_count`), the
    /// SVP's SSP1601 state on Virtua Racing. `null` on a plain cartridge.
    #[wasm_bindgen(js_name = getChipState)]
    pub fn get_chip_state(&self) -> Result<JsValue, JsValue> {
        match self.genesis.coprocessor_debug_state() {
            Some(state) => Ok(map_as_object(&state)),
            None => Ok(JsValue::NULL),
        }
    }

    /// Get every coprocessor's live debug snapshot, keyed by `chip_id`, as
    /// `[{ chipId, label, state }]`. Empty on a plain cartridge. Lets a
    /// multi-chip cartridge surface all chips (Genesis ships single-chip
    /// today, but the shape matches the SNES multi-coprocessor debugger).
    #[wasm_bindgen(js_name = getChipStates)]
    pub fn get_chip_states(&self) -> Result<JsValue, JsValue> {
        let labels: std::collections::HashMap<u8, String> =
            self.genesis.coprocessor_labels().into_iter().collect();
        let rows: Vec<serde_json::Value> = self
            .genesis
            .coprocessors_debug_state()
            .into_iter()
            .map(|(chip_id, state)| {
                serde_json::json!({
                    "chipId": chip_id,
                    "label": labels.get(&chip_id).cloned().unwrap_or_default(),
                    "state": state,
                })
            })
            .collect();
        Ok(map_as_object(&rows))
    }

    /// Get memory map visualization data, the installed cartridge's
    /// resolved map-plan regions (`{ start, end, label, detail }`), first-wins
    /// order. Empty region list when no ROM is loaded.
    #[wasm_bindgen(js_name = getMemoryMap)]
    pub fn get_memory_map(&self) -> Result<JsValue, JsValue> {
        let regions = self.genesis.cartridge_map_regions();
        let map = serde_json::json!({ "regions": regions });
        Ok(map_as_object(&map))
    }

    // ===== Cartridge manifest =====

    /// Cartridge manifest view derived from the installed map plan:
    /// `{ memoryMap, romRegions, sramRegions, mapperState }`. `mapperState`
    /// is the live `{ mapper_type, registers, banks }` from the cartridge
    /// debugger (`mapper_type: "None"` for a plain cartridge).
    #[wasm_bindgen(js_name = getCartridgeManifest)]
    pub fn get_cartridge_manifest(&mut self) -> JsValue {
        use retro_core::cartridge::CartridgeDebugger;
        let regions = self.genesis.cartridge_map_regions();
        let rom_regions: Vec<_> = regions
            .iter()
            .filter(|r| r.label == "ROM")
            .cloned()
            .collect();
        let sram_regions: Vec<_> = regions
            .iter()
            .filter(|r| r.label == "SRAM")
            .cloned()
            .collect();
        let mapper_state = self.genesis.cartridge_debugger().get_mapper_state();
        let manifest = serde_json::json!({
            "memoryMap": regions,
            "romRegions": rom_regions,
            "sramRegions": sram_regions,
            "mapperState": mapper_state_json(mapper_state),
        });
        map_as_object(&manifest)
    }

    /// Detect the editable `cartridge-manifest-v1` for arbitrary ROM bytes,
    /// as JSON. Side-effect-free: builds nothing, installs nothing, and does
    /// not disturb a running game, the hub modal uses it to show the
    /// heuristic mapping before the user edits it.
    #[wasm_bindgen(js_name = detectManifest)]
    pub fn detect_manifest(&self, data: &[u8]) -> Result<String, JsValue> {
        let manifest = self.genesis.detected_manifest(data);
        serde_json::to_string(&manifest).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Detect the editable `cartridge-manifest-v1` for the *currently loaded*
    /// ROM, as JSON. Returns `"null"` (JSON) when no ROM is loaded. Reflects
    /// pure detection, never a staged override, so the editor can diff the
    /// user's override against the baseline.
    #[wasm_bindgen(js_name = getDetectedManifest)]
    pub fn get_detected_manifest(&self) -> Result<String, JsValue> {
        if self.genesis.last_rom().is_empty() {
            return Ok("null".to_string());
        }
        let manifest = self.genesis.detected_manifest(self.genesis.last_rom());
        serde_json::to_string(&manifest).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Stage a `cartridge-manifest-v1` (JSON) applied on the next `loadRom`,
    /// overriding heuristic detection. Returns a JS `Error` when the JSON is
    /// malformed or the schema version is unsupported, so the editor can
    /// surface validation failures. The override does not affect the
    /// currently installed cartridge, reload the ROM.
    #[wasm_bindgen(js_name = setCartridgeManifest)]
    pub fn set_cartridge_manifest(&mut self, json: &str) -> Result<(), JsValue> {
        let manifest = parse_manifest(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.genesis.set_cartridge_manifest_override(Some(manifest));
        Ok(())
    }

    /// Clear any staged manifest override; the next `loadRom` reverts to pure
    /// heuristic detection.
    #[wasm_bindgen(js_name = clearCartridgeManifest)]
    pub fn clear_cartridge_manifest(&mut self) {
        self.genesis.set_cartridge_manifest_override(None);
    }

    // ===== Debugger Methods =====

    /// Get CPU states.
    #[wasm_bindgen(js_name = getCpuStates)]
    pub fn get_cpu_states(&mut self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.genesis.debugger().get_cpu_states()).unwrap()
    }

    /// Add a breakpoint. `bp_type` is one of
    /// `"execute" | "read" | "write" | "readwrite"`; `cpu_id` selects the
    /// CPU (68000 = 0, Z80 = 1) and defaults to 0 when omitted.
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

        self.genesis.debugger().add_breakpoint(bp)
    }

    /// Remove a breakpoint.
    #[wasm_bindgen(js_name = removeBreakpoint)]
    pub fn remove_breakpoint(&mut self, id: u32) {
        self.genesis.debugger().remove_breakpoint(id);
    }

    /// Enable or disable a breakpoint.
    #[wasm_bindgen(js_name = enableBreakpoint)]
    pub fn enable_breakpoint(&mut self, id: u32, enabled: bool) {
        self.genesis.debugger().enable_breakpoint(id, enabled);
    }

    /// Replace the access flags (exec/read/write) of a breakpoint in
    /// place, preserving its hit count. Backs the per-row R/W/X
    /// checkboxes in the debugger UI.
    #[wasm_bindgen(js_name = setBreakpointAccess)]
    pub fn set_breakpoint_access(&mut self, id: u32, exec: bool, read: bool, write: bool) {
        self.genesis
            .debugger()
            .set_breakpoint_access(id, AccessFlags::new(exec, read, write));
    }

    /// Replace the advanced match conditions (address range, value
    /// predicate + size, conditional `expr`, hit-skip, action, one-shot) of
    /// a breakpoint. `spec` is a plain object whose fields mirror
    /// `MatchSpec`; every field is optional and defaults to the
    /// bare-breakpoint behaviour. `expr` is an optional condition string
    /// over live registers/memory (e.g. `"d0 == 5 && [0xFF0000].w > 0"`)
    /// evaluated when the base match holds. No-op on an unknown id; errors
    /// only on a malformed object.
    #[wasm_bindgen(js_name = setBreakpointMatchSpec)]
    pub fn set_breakpoint_match_spec(&mut self, id: u32, spec: JsValue) -> Result<(), JsValue> {
        let spec: MatchSpec =
            serde_wasm_bindgen::from_value(spec).map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.genesis.debugger().set_breakpoint_match_spec(id, spec);
        Ok(())
    }

    /// Catalog of event classes this core can break on, for the event
    /// breakpoint picker: 1 VBlank, 2 VInt and 3 HInt (the interrupts the
    /// 68000 takes), 4 a scanline (param the V counter), 5 a transfer (a
    /// DMA from 68000 memory, a fill or a copy) finished (param the memory:
    /// 0 VRAM, 1 CRAM, 2 VSRAM), 6 a palette colour written (param the
    /// colour, 0-63), 7 a transfer into the sprite table, and 8 a tile
    /// rewritten (param the tile, 0-2047; writing the bytes it holds again
    /// does not count). Events 5-8 stop after the instruction that made the
    /// write. A breakpoint on any of them runs the frame on the light
    /// debugger loop; with none the loop a player runs checks nothing.
    #[wasm_bindgen(js_name = getAvailableEvents)]
    pub fn get_available_events(&mut self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.genesis.debugger().available_events()).unwrap()
    }

    /// Add an event breakpoint for one of the codes from
    /// [`Self::get_available_events`]. `param` discriminates within a
    /// class (the scanline V counter); pass `0xFFFF_FFFF`
    /// (`BREAKPOINT_PARAM_ANY`) to match any. Returns the breakpoint id.
    #[wasm_bindgen(js_name = addEventBreakpoint)]
    pub fn add_event_breakpoint(&mut self, code: u32, param: u32) -> u32 {
        self.genesis.debugger().add_breakpoint(Breakpoint {
            address: 0,
            bp_type: BreakpointType::Event { code, param },
            cpu_id: None,
            condition: None,
        })
    }

    /// List all breakpoints.
    #[wasm_bindgen(js_name = listBreakpoints)]
    pub fn list_breakpoints(&mut self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.genesis.debugger().list_breakpoints()).unwrap()
    }

    /// Get execution mode.
    #[wasm_bindgen(js_name = getExecutionMode)]
    pub fn get_execution_mode(&mut self) -> String {
        format!("{:?}", self.genesis.debugger().get_execution_mode())
    }

    /// Set execution mode. On `Paused` the CPU states `getCpuStates` returns
    /// are written at once, wherever the game stopped.
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

        self.genesis.set_execution_mode(exec_mode);
    }

    /// Get memory regions.
    #[wasm_bindgen(js_name = getMemoryRegions)]
    pub fn get_memory_regions(&mut self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.genesis.debugger().get_memory_regions()).unwrap()
    }

    /// Get instruction history.
    #[wasm_bindgen(js_name = getInstructionHistory)]
    pub fn get_instruction_history(&mut self, cpu_id: usize, count: u32) -> JsValue {
        serde_wasm_bindgen::to_value(
            &self
                .genesis
                .debugger()
                .get_instruction_history(cpu_id, count),
        )
        .unwrap()
    }

    /// Get call stack.
    #[wasm_bindgen(js_name = getCallStack)]
    pub fn get_call_stack(&mut self, cpu_id: usize) -> JsValue {
        serde_wasm_bindgen::to_value(&self.genesis.debugger().get_call_stack(cpu_id)).unwrap()
    }

    /// Read memory for debugging.
    #[wasm_bindgen(js_name = readMemory)]
    pub fn read_memory(&mut self, addr: u32, size: u32) -> Vec<u8> {
        self.genesis.read_memory_debug(addr, size)
    }

    /// Write memory for debugging (bypasses normal bus logic).
    #[wasm_bindgen(js_name = writeMemoryDebug)]
    pub fn write_memory_debug(&mut self, addr: u32, data: &[u8]) {
        self.genesis.write_memory_debug(addr, data);
    }

    /// Read the full ROM binary from the cartridge (for "Save ROM" feature).
    #[wasm_bindgen(js_name = getRomBytes)]
    pub fn get_rom_bytes(&mut self) -> Vec<u8> {
        let size = self
            .genesis
            .get_rom_info()
            .map(|info| info.size)
            .unwrap_or(0);
        if size == 0 {
            return Vec::new();
        }
        self.genesis.read_memory_debug(0, size as u32)
    }

    /// Disassemble instructions starting at address.
    #[wasm_bindgen(js_name = disassembleAt)]
    pub fn disassemble_at(&mut self, addr: u32, count: u32) -> JsValue {
        serde_wasm_bindgen::to_value(&self.genesis.disassemble_at(addr, count)).unwrap()
    }

    /// Disassemble around PC using known instruction boundaries (bsnes-style).
    /// This uses the boundary map built during execution to show accurate
    /// disassembly even after branches/jumps.
    ///
    /// Deprecated: prefer `disassembleWindow`, which is bitmap-aware
    /// and renders never-executed regions as `unknown` rows instead
    /// of returning fake `ori` instructions for data.
    #[wasm_bindgen(js_name = disassembleAroundPc)]
    pub fn disassemble_around_pc(
        &mut self,
        pc: u32,
        before_count: u32,
        after_count: u32,
    ) -> JsValue {
        serde_wasm_bindgen::to_value(&self.genesis.disassemble_around_pc(
            pc,
            before_count as usize,
            after_count as usize,
        ))
        .unwrap()
    }

    /// Bitmap-aware disassembly window. Returns `DisasmRow[]` with
    /// each row tagged `kind: "code" | "unknown"`. Backward rows are
    /// walked through the per-CPU coverage bitmap so they are real
    /// contiguous predecessors (no stale entries above PC after a
    /// far branch); forward rows past a data hole render as
    /// `unknown` instead of fake instructions.
    #[wasm_bindgen(js_name = disassembleWindow)]
    pub fn disassemble_window(
        &mut self,
        cpu_id: usize,
        pc: u32,
        before_count: u32,
        after_count: u32,
    ) -> JsValue {
        serde_wasm_bindgen::to_value(&self.genesis.disassemble_window(
            cpu_id,
            pc,
            before_count as usize,
            after_count as usize,
        ))
        .unwrap()
    }

    /// Get known instruction boundaries before an address.
    #[wasm_bindgen(js_name = getBoundariesBefore)]
    pub fn get_boundaries_before(&self, addr: u32, count: u32) -> Vec<u32> {
        self.genesis.get_boundaries_before(addr, count as usize)
    }

    // ===== Coverage Bitmap =====
    //
    // Per-CPU code-coverage bitmap. One set-bit per address the CPU
    // has executed an instruction from. Powers the bitmap-aware
    // `disassembleWindow` so the disassembly panel renders `....` for
    // never-executed regions instead of fake `ori` rows, and so the
    // backward walk from PC cannot return stale predecessors after a
    // far branch. The bitmap lives in memory only, nothing persists it;
    // it is cleared on a different-ROM load and survives `reset()` and
    // hot-reload.

    /// True iff the given address has been observed as an instruction
    /// start on the requested CPU (0 = 68K, 1 = Z80).
    #[wasm_bindgen(js_name = isCovered)]
    pub fn is_covered(&self, cpu_id: usize, addr: u32) -> bool {
        self.genesis.debugger_ref().is_covered(cpu_id, addr)
    }

    /// Returns `{ totalBytes, coveredBytes, percent }` for the given
    /// CPU. `percent` is `coveredBytes / totalBytes` as a 0..100 f32
    /// so JS can render it without doing the division.
    #[wasm_bindgen(js_name = getCoverageStats)]
    pub fn get_coverage_stats(&self, cpu_id: usize) -> JsValue {
        let (total, covered) = self.genesis.debugger_ref().coverage_stats(cpu_id);
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
        map_as_object(&obj)
    }

    /// Wipe every bit of the requested CPU's coverage bitmap. The
    /// only user-driven reset path; `load_rom` of a different ROM
    /// also clears it, but transparently.
    #[wasm_bindgen(js_name = clearCoverage)]
    pub fn clear_coverage(&mut self, cpu_id: usize) {
        self.genesis.debugger().clear_coverage(cpu_id);
    }

    // ===== Tracing Methods =====

    /// Enable or disable execution tracing.
    /// When enabled, captures PC, SR, D0-D7, A0-A7 at each instruction.
    #[wasm_bindgen(js_name = setTracingEnabled)]
    pub fn set_tracing_enabled(&mut self, enabled: bool) {
        self.genesis.set_tracing_enabled(enabled);
    }

    /// Check if tracing is enabled.
    #[wasm_bindgen(js_name = isTracingEnabled)]
    pub fn is_tracing_enabled(&self) -> bool {
        self.genesis.is_tracing_enabled()
    }

    /// Check if trace buffer needs to be flushed.
    /// JS should check this after runFrame() and flush if true.
    #[wasm_bindgen(js_name = needsTraceFlush)]
    pub fn needs_trace_flush(&self) -> bool {
        self.genesis.needs_trace_flush()
    }

    /// Get trace data ready for flushing.
    /// Returns a copy of the buffer data as Vec<u8>.
    #[wasm_bindgen(js_name = getTraceFlushData)]
    pub fn get_trace_flush_data(&self) -> Vec<u8> {
        self.genesis.get_trace_flush_data().to_vec()
    }

    /// Clear flushed trace data and reset buffer.
    /// Call this after successfully writing data to OPFS.
    #[wasm_bindgen(js_name = clearTraceFlushed)]
    pub fn clear_trace_flushed(&mut self) {
        self.genesis.clear_trace_flushed();
    }

    /// Get tracing statistics.
    /// Returns: { total_entries, flushed_entries, buffered_entries, buffer_bytes, buffer_capacity, enabled }
    #[wasm_bindgen(js_name = getTraceStats)]
    pub fn get_trace_stats(&self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.genesis.get_trace_stats()).unwrap()
    }

    /// Convert binary trace data to Exodus-compatible text format.
    /// Format: PC:XXXXXX SR:XXXX D0:XXXXXXXX ... A7:XXXXXXXX | MNEMONIC
    /// Get the trace entry size in bytes (for debugging).
    #[wasm_bindgen(js_name = getTraceEntrySize)]
    pub fn get_trace_entry_size(&self) -> usize {
        sg1_core::debugger::TRACE_ENTRY_SIZE
    }

    /// Note: Mnemonic requires disassembly which we skip for performance.
    /// The output format is compatible with trace comparison tools.
    #[wasm_bindgen(js_name = convertTraceChunkToText)]
    pub fn convert_trace_chunk_to_text(&mut self, data: &[u8]) -> String {
        use sg1_core::debugger::{TraceEntryBinary, TRACE_ENTRY_SIZE};

        let entry_count = data.len() / TRACE_ENTRY_SIZE;
        let mut result = String::with_capacity(entry_count * 200); // ~200 chars per line

        for i in 0..entry_count {
            let start = i * TRACE_ENTRY_SIZE;
            let end = start + TRACE_ENTRY_SIZE;

            if let Some(entry) = TraceEntryBinary::from_bytes(&data[start..end]) {
                // Disassemble the instruction at this PC
                let instructions = self.genesis.disassemble_at(entry.pc, 1);
                let mnemonic = if !instructions.is_empty() {
                    format!("{} {}", instructions[0].mnemonic, instructions[0].operands)
                } else {
                    format!("DC.W ${:04X}", entry.opcode)
                };

                // Format: PC:XXXXXX SR:XXXX D0:XXXXXXXX D1:... A0:... | MNEMONIC
                result.push_str(&format!(
                    "PC:{:06X} SR:{:04X} D0:{:08X} D1:{:08X} D2:{:08X} D3:{:08X} D4:{:08X} D5:{:08X} D6:{:08X} D7:{:08X} A0:{:08X} A1:{:08X} A2:{:08X} A3:{:08X} A4:{:08X} A5:{:08X} A6:{:08X} A7:{:08X} | {}\n",
                    entry.pc,
                    entry.sr,
                    entry.d[0], entry.d[1], entry.d[2], entry.d[3],
                    entry.d[4], entry.d[5], entry.d[6], entry.d[7],
                    entry.a[0], entry.a[1], entry.a[2], entry.a[3],
                    entry.a[4], entry.a[5], entry.a[6], entry.a[7],
                    mnemonic
                ));
            }
        }

        result
    }

    // ===== Multi-CPU Debug Methods =====

    /// Get the currently focused CPU id.
    #[wasm_bindgen(js_name = getFocusedCpu)]
    pub fn get_focused_cpu(&self) -> usize {
        self.genesis.focused_cpu()
    }

    /// Set the focused CPU (0 = 68K, 1 = Z80).
    /// Affects step(), disassembleAt(), etc.
    #[wasm_bindgen(js_name = setFocusedCpu)]
    pub fn set_focused_cpu(&mut self, cpu_id: usize) {
        self.genesis.set_focused_cpu(cpu_id);
    }

    /// Check if Z80 is held in reset (controlled by 68K writes to 0xA11200).
    /// When true, Z80 cannot execute and stepping has no effect.
    #[wasm_bindgen(js_name = isZ80Reset)]
    pub fn is_z80_reset(&self) -> bool {
        self.genesis.z80_is_reset()
    }

    /// Check if 68K has requested the Z80 bus (controlled by 68K writes to 0xA11100).
    /// When true, Z80 cannot execute and stepping has no effect.
    #[wasm_bindgen(js_name = isZ80BusRequested)]
    pub fn is_z80_bus_requested(&self) -> bool {
        self.genesis.z80_bus_requested()
    }

    /// Step a specific CPU by id.
    #[wasm_bindgen(js_name = stepCpu)]
    pub fn step_cpu(&mut self, cpu_id: usize) -> JsValue {
        serde_wasm_bindgen::to_value(&self.genesis.step_cpu(cpu_id)).unwrap()
    }

    /// Disassemble instructions from a specific CPU at the given address.
    #[wasm_bindgen(js_name = disassembleCpu)]
    pub fn disassemble_cpu(&mut self, cpu_id: usize, addr: u32, count: u32) -> JsValue {
        serde_wasm_bindgen::to_value(&self.genesis.disassemble_cpu(cpu_id, addr, count)).unwrap()
    }

    /// Enable or disable Z80 tracing.
    #[wasm_bindgen(js_name = setZ80TracingEnabled)]
    pub fn set_z80_tracing_enabled(&mut self, enabled: bool) {
        self.genesis.set_z80_tracing_enabled(enabled);
    }

    /// Check if Z80 tracing is enabled.
    #[wasm_bindgen(js_name = isZ80TracingEnabled)]
    pub fn is_z80_tracing_enabled(&self) -> bool {
        self.genesis.is_z80_tracing_enabled()
    }

    /// Check if Z80 trace buffer needs flushing.
    #[wasm_bindgen(js_name = needsZ80TraceFlush)]
    pub fn needs_z80_trace_flush(&self) -> bool {
        self.genesis.needs_z80_trace_flush()
    }

    /// Get Z80 trace data for flushing.
    #[wasm_bindgen(js_name = getZ80TraceFlushData)]
    pub fn get_z80_trace_flush_data(&self) -> Vec<u8> {
        self.genesis.get_z80_trace_flush_data().to_vec()
    }

    /// Clear flushed Z80 trace data.
    #[wasm_bindgen(js_name = clearZ80TraceFlushed)]
    pub fn clear_z80_trace_flushed(&mut self) {
        self.genesis.clear_z80_trace_flushed();
    }

    /// Get the Z80 trace entry size in bytes.
    #[wasm_bindgen(js_name = getZ80TraceEntrySize)]
    pub fn get_z80_trace_entry_size(&self) -> usize {
        sg1_core::debugger::Z80_TRACE_ENTRY_SIZE
    }

    /// Convert binary Z80 trace data to MAME-compatible text format.
    /// Format: PC:XXXX AF:XXXX BC:XXXX DE:XXXX HL:XXXX IX:XXXX IY:XXXX SP:XXXX I:XX R:XX IM:X IFF1:X IFF2:X HALT:X | MNEMONIC
    ///
    /// Uses the raw instruction bytes stored in each trace entry for disassembly,
    /// so the output is correct even if Z80 RAM has changed since the trace was captured.
    #[wasm_bindgen(js_name = convertZ80TraceChunkToText)]
    pub fn convert_z80_trace_chunk_to_text(&self, data: &[u8]) -> String {
        use sg1_core::debugger::{TraceEntryBinaryZ80, Z80_TRACE_ENTRY_SIZE};
        use sg1_core::z80::disasm;

        let entry_count = data.len() / Z80_TRACE_ENTRY_SIZE;
        let mut result = String::with_capacity(entry_count * 120);

        for i in 0..entry_count {
            let start = i * Z80_TRACE_ENTRY_SIZE;
            let end = start + Z80_TRACE_ENTRY_SIZE;

            if let Some(entry) = TraceEntryBinaryZ80::from_bytes(&data[start..end]) {
                // Disassemble from the stored raw bytes (captured at trace time)
                let (text, _) = disasm::disassemble_from_bytes(entry.pc, &entry.raw_bytes);
                let mnemonic = disasm::to_trace_format(&text);

                result.push_str(&entry.to_text(&mnemonic));
                result.push('\n');
            }
        }

        result
    }

    // ===== VDP Debugger Methods =====

    /// Get tile as RGBA (8x8 pixels x 4 bytes = 256 bytes).
    #[wasm_bindgen(js_name = getTileRgba)]
    pub fn get_tile_rgba(&mut self, tile_index: u32, palette_index: u8) -> Vec<u8> {
        self.genesis.vdp().get_tile_rgba(tile_index, palette_index)
    }

    /// Get all palettes as RGBA (64 colors x 4 bytes = 256 bytes).
    #[wasm_bindgen(js_name = getAllPalettesRgba)]
    pub fn get_all_palettes_rgba(&self) -> Vec<u8> {
        self.genesis.vdp().get_all_palettes_rgba()
    }

    /// Get VRAM (read-only, 64KB).
    #[wasm_bindgen(js_name = getVram)]
    pub fn get_vram(&self) -> Vec<u8> {
        self.genesis.vdp().get_vram().to_vec()
    }

    /// Get CRAM (read-only, 64 colors x 2 bytes = 128 bytes).
    #[wasm_bindgen(js_name = getCram)]
    pub fn get_cram(&self) -> Vec<u16> {
        self.genesis.vdp().get_cram().to_vec()
    }

    /// Get VSRAM (read-only). 64 addressable words: 40 physical 11-bit scroll
    /// entries plus the upper data-port-reachable slots.
    #[wasm_bindgen(js_name = getVsram)]
    pub fn get_vsram(&self) -> Vec<u16> {
        self.genesis.vdp().get_vsram().to_vec()
    }

    /// Write VRAM for debugging, to edit a game's graphics: `bytes` from
    /// byte `byte_address` on, in the layout `getVram` returns, so
    /// `getVram` gives them back. Straight into memory, through no port,
    /// FIFO or DMA: the timing, the CPUs and the VDP's registers and
    /// status are untouched. The address wraps at the end of the 64 KiB, a
    /// buffer longer than VRAM is cut to it, and an empty one writes
    /// nothing. A write into the sprite table reaches the renderer's
    /// sprite list. A running game shows the change on its next frame, a
    /// paused one after `repaintFrame`.
    #[wasm_bindgen(js_name = writeVram)]
    pub fn write_vram(&mut self, byte_address: u32, bytes: &[u8]) {
        self.genesis.vdp_mut().write_vram_debug(byte_address, bytes);
    }

    /// Write CRAM for debugging, to edit a palette: `colours` from colour
    /// `colour_index` (0-63) on, in the format `getCram` returns
    /// (`0000 BBB0 GGG0 RRR0`; the other bits are dropped, as the data
    /// port drops them). Straight into memory, through no port or DMA.
    /// The index wraps at 64, a buffer longer than CRAM is cut to its 64
    /// colours, and an empty one writes nothing. A running game shows the
    /// change on its next frame, a paused one after `repaintFrame`.
    #[wasm_bindgen(js_name = writeCram)]
    pub fn write_cram(&mut self, colour_index: u32, colours: &[u16]) {
        self.genesis
            .vdp_mut()
            .write_cram_debug(colour_index, colours);
    }

    /// Write VSRAM for debugging: `values` from word `index` (0-63) on,
    /// each keeping its low 11 bits, as `getVsram` returns them. Straight
    /// into memory, through no port or DMA. The index wraps at 64, a
    /// buffer longer than the 64 words is cut to them, and an empty one
    /// writes nothing.
    #[wasm_bindgen(js_name = writeVsram)]
    pub fn write_vsram(&mut self, index: u32, values: &[u16]) {
        self.genesis.vdp_mut().write_vsram_debug(index, values);
    }

    /// Get plane nametable addresses.
    #[wasm_bindgen(js_name = getPlaneAddresses)]
    pub fn get_plane_addresses(&self) -> JsValue {
        use serde::Serialize;

        #[derive(Serialize)]
        struct PlaneAddresses {
            plane_a: u16,
            plane_b: u16,
            window: u16,
        }

        let addresses = PlaneAddresses {
            plane_a: self.genesis.vdp().get_plane_a_nametable(),
            plane_b: self.genesis.vdp().get_plane_b_nametable(),
            window: self.genesis.vdp().get_window_nametable(),
        };

        serde_wasm_bindgen::to_value(&addresses).unwrap()
    }

    /// Get sprite count for debugging.
    #[wasm_bindgen(js_name = getSpriteCount)]
    pub fn get_sprite_count(&self) -> usize {
        self.genesis.vdp().get_sprite_count_debug()
    }

    /// Get the parsed Sprite Attribute Table (SAT).
    ///
    /// Walks the linked sprite list starting at index 0, following each
    /// entry's `link` field, and stops where the renderer stops: after a
    /// link of 0, before a link past the table (64 entries in the 32-cell
    /// mode, 80 in the 40-cell mode), and after that many entries. The SAT
    /// base address is derived from VDP register 5 with the H40-aware mask
    /// used by `rebuild_sprite_cache`.
    ///
    /// This is read-only / debug-only, it does not touch the live
    /// sprite cache or any rendering buffers.
    #[wasm_bindgen(js_name = getSpriteAttrTable)]
    pub fn get_sprite_attr_table(&self) -> JsValue {
        use serde::Serialize;

        #[derive(Serialize)]
        struct SpriteAttr {
            index: u8,
            x: i16,
            y: i16,
            h_tiles: u8,
            v_tiles: u8,
            tile: u16,
            palette: u8,
            priority: bool,
            h_flip: bool,
            v_flip: bool,
            link: u8,
        }

        let sprites: Vec<SpriteAttr> = self
            .genesis
            .vdp()
            .sprite_table_chain()
            .into_iter()
            .map(|(index, s)| SpriteAttr {
                index,
                x: s.x,
                y: s.y,
                h_tiles: s.width_tiles,
                v_tiles: s.height_tiles,
                tile: s.pattern_base,
                palette: s.palette,
                priority: s.priority,
                h_flip: s.flip_h,
                v_flip: s.flip_v,
                link: s.link,
            })
            .collect();

        serde_wasm_bindgen::to_value(&sprites).unwrap()
    }

    /// Export a single sprite (by SAT slot index) as a tight, transparent
    /// RGBA image: `{ width, height, data: Uint8Array }`, or `null` if the
    /// index is out of range / empty.
    ///
    /// The core does the hardware decode (tile selection, flips, palette
    /// index 0 -> alpha 0); the platform owns PNG encoding and UX. `data` is
    /// a real `Uint8Array` (not a plain JS array) so the host can blit it
    /// straight into an `ImageData`.
    #[wasm_bindgen(js_name = exportSpriteImage)]
    pub fn export_sprite_image(&mut self, index: u32) -> JsValue {
        match self.genesis.video_debugger().export_sprite_image(index) {
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
    /// `SpriteData[]` with **normalized signed screen-space** coordinates
    /// (the Genesis `-128` bias already undone). Read-only / debug-only;
    /// mirrors the `snes1` binding so the host's picker is core-agnostic.
    #[wasm_bindgen(js_name = getSpriteData)]
    pub fn get_sprite_data(&self) -> JsValue {
        use retro_core::video::VideoDebugger;
        let sprites = self.genesis.vdp().get_sprite_data();
        serde_wasm_bindgen::to_value(&sprites).unwrap()
    }

    /// The renderer's resolved sprite paint order as `SpriteOrderEntry[]`
    /// in back-to-front order (last entry draws on top), so a composite
    /// exporter blits members exactly as the VDP drew them.
    #[wasm_bindgen(js_name = resolvedSpriteRenderOrder)]
    pub fn resolved_sprite_render_order(&self) -> JsValue {
        use retro_core::video::VideoDebugger;
        let order = self.genesis.vdp().resolved_sprite_render_order();
        serde_wasm_bindgen::to_value(&order).unwrap()
    }

    /// Decode a run of VRAM tiles to an *indexed* sheet for host-side
    /// full-map assembly: `{ width, height, pixels: Uint8Array, bpp,
    /// transparentIndex }`, or `null` if the request is unsupported / out
    /// of range. The core owns only the 4bpp nibble decode; the platform
    /// reads the nametable and applies the per-cell palette + flip +
    /// transparency policy itself (so colour never enters the core).
    ///
    /// `source`: 0 = VRAM. `tileSize`: 0 = 8x8, 1 = 16x16. `pixels` is a
    /// real `Uint8Array` (one palette index per pixel), `count` tiles
    /// stacked vertically.
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
        match self.genesis.video_debugger().decode_tiles(&request) {
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

    /// VDP control registers (`$00..=$17`) as `VideoRegister[]`
    /// (`{ index, name, value, description }`) for the State tab.
    #[wasm_bindgen(js_name = getVdpRegisters)]
    pub fn get_vdp_registers(&mut self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.genesis.video_debugger().get_registers()).unwrap()
    }

    /// Side-effect-free 16-bit VDP status word (vblank/hblank/dma/fifo/
    /// sprite flags). Unlike a real status-port read this does NOT reset
    /// the VDP control latch, so the platform's debugger and its lessons can
    /// sample it every frame without perturbing a running game. RetroLab's
    /// lessons test VDP status flags with it.
    #[wasm_bindgen(js_name = getVdpStatus)]
    pub fn get_vdp_status(&self) -> u16 {
        self.genesis.vdp().status_word()
    }

    /// Per-layer info (`LayerData[]`: Plane A, Plane B, Window, Sprites,
    /// each with size/scroll/enabled) for the layer-toggle list. Mirrors
    /// the SNES `getLayerInfo`.
    #[wasm_bindgen(js_name = getLayerInfo)]
    pub fn get_layer_info(&mut self) -> JsValue {
        let count = self.genesis.video_debugger().get_layer_count();
        let mut layers = Vec::with_capacity(count as usize);
        for id in 0..count {
            if let Some(layer) = self.genesis.video_debugger().get_layer_data(id) {
                layers.push(layer);
            }
        }
        serde_wasm_bindgen::to_value(&layers).unwrap()
    }

    /// Show/hide a VDP layer in the live composite. A hidden layer is
    /// suppressed from composition without touching the game's real VDP
    /// registers. Mirrors the SNES `toggleLayer`.
    #[wasm_bindgen(js_name = toggleLayer)]
    pub fn toggle_layer(&mut self, layer_id: u32, visible: bool) {
        self.genesis
            .video_debugger()
            .toggle_layer(layer_id, visible);
    }

    // ===== YM2612 Audio Debugger Methods =====

    /// Get YM2612 channel count (always 6).
    #[wasm_bindgen(js_name = getYm2612ChannelCount)]
    pub fn get_ym2612_channel_count(&self) -> u32 {
        6
    }

    /// Get YM2612 channel state.
    #[wasm_bindgen(js_name = getYm2612ChannelState)]
    pub fn get_ym2612_channel_state(&self, channel: u32) -> JsValue {
        self.genesis
            .get_ym2612_channel_state(channel)
            .map(|state| serde_wasm_bindgen::to_value(&state).unwrap())
            .unwrap_or(JsValue::NULL)
    }

    /// Mute/unmute YM2612 channel.
    #[wasm_bindgen(js_name = muteYm2612Channel)]
    pub fn mute_ym2612_channel(&mut self, channel: u32, muted: bool) {
        self.genesis.mute_ym2612_channel(channel, muted);
    }

    /// Get YM2612 LFO state.
    #[wasm_bindgen(js_name = getYm2612LfoState)]
    pub fn get_ym2612_lfo_state(&self) -> JsValue {
        map_as_object(&self.genesis.get_ym2612_lfo_state())
    }

    /// Get YM2612 DAC state.
    #[wasm_bindgen(js_name = getYm2612DacState)]
    pub fn get_ym2612_dac_state(&self) -> JsValue {
        map_as_object(&self.genesis.get_ym2612_dac_state())
    }

    /// Get YM2612 Timer A state.
    #[wasm_bindgen(js_name = getYm2612TimerAState)]
    pub fn get_ym2612_timer_a_state(&self) -> JsValue {
        map_as_object(&self.genesis.get_ym2612_timer_a_state())
    }

    /// Get YM2612 Timer B state.
    #[wasm_bindgen(js_name = getYm2612TimerBState)]
    pub fn get_ym2612_timer_b_state(&self) -> JsValue {
        map_as_object(&self.genesis.get_ym2612_timer_b_state())
    }

    // ========================================================================
    // Input Methods
    // ========================================================================

    /// Keyboard key down event.
    #[wasm_bindgen(js_name = keyDown)]
    pub fn key_down(&mut self, keycode: String) {
        self.genesis
            .input_manager_mut()
            .update_keyboard(keycode, true);
    }

    /// Keyboard key up event.
    #[wasm_bindgen(js_name = keyUp)]
    pub fn key_up(&mut self, keycode: String) {
        self.genesis
            .input_manager_mut()
            .update_keyboard(keycode, false);
    }

    /// Gamepad button event.
    #[wasm_bindgen(js_name = gamepadButton)]
    pub fn gamepad_button(&mut self, gamepad_id: String, button: u32, pressed: bool) {
        self.genesis
            .input_manager_mut()
            .update_gamepad_button(gamepad_id, button, pressed);
    }

    /// Gamepad axis event.
    #[wasm_bindgen(js_name = gamepadAxis)]
    pub fn gamepad_axis(&mut self, gamepad_id: String, axis: u32, value: f32) {
        self.genesis
            .input_manager_mut()
            .update_gamepad_axis(gamepad_id, axis, value);
    }

    /// Get all registered input providers.
    #[wasm_bindgen(js_name = getInputProviders)]
    pub fn get_input_providers(&self) -> JsValue {
        let providers = self.genesis.input_manager().get_providers();
        serde_wasm_bindgen::to_value(&providers).unwrap()
    }

    /// Get port state (which provider is assigned to which port).
    #[wasm_bindgen(js_name = getPortAssignment)]
    pub fn get_port_assignment(&self, port: u32) -> JsValue {
        let assignment = self
            .genesis
            .input_manager()
            .get_port_assignment(port as usize);
        serde_wasm_bindgen::to_value(&assignment).unwrap()
    }

    /// Get input history (last N snapshots).
    #[wasm_bindgen(js_name = getInputHistory)]
    pub fn get_input_history(&self, count: u32) -> JsValue {
        let history = self.genesis.input_manager().get_history(count as usize);
        serde_wasm_bindgen::to_value(&history).unwrap()
    }

    /// Get current state of a port (button bitmask).
    #[wasm_bindgen(js_name = getPortState)]
    pub fn get_port_state(&self, port: u32) -> u32 {
        self.genesis.input_manager().get_port_state(port as usize)
    }

    /// Make an input provider the only one on a controller port (an empty
    /// id clears the port). [`set_provider_port`](Self::set_provider_port)
    /// moves a provider and keeps the others.
    #[wasm_bindgen(js_name = assignProviderToPort)]
    pub fn assign_provider_to_port(&mut self, provider_id: String, port: u32) {
        self.genesis
            .input_manager_mut()
            .assign_provider_to_port(provider_id, port as usize);
    }

    /// Put an input provider on a controller port, beside whatever else
    /// feeds it, after taking it off every other port. A negative `port`
    /// takes it off every port, so its input reaches no player.
    #[wasm_bindgen(js_name = setProviderPort)]
    pub fn set_provider_port(&mut self, provider_id: String, port: i32) {
        let port = usize::try_from(port).ok();
        self.genesis
            .input_manager_mut()
            .set_provider_port(&provider_id, port);
    }

    /// The controller port an input provider feeds, or -1 when none.
    #[wasm_bindgen(js_name = getProviderPort)]
    pub fn get_provider_port(&self, provider_id: String) -> i32 {
        self.genesis
            .input_manager()
            .provider_port(&provider_id)
            .map_or(-1, |port| port as i32)
    }

    /// Every input provider feeding a controller port (`string[]`).
    #[wasm_bindgen(js_name = getPortProviders)]
    pub fn get_port_providers(&self, port: u32) -> JsValue {
        let providers = self.genesis.input_manager().port_providers(port as usize);
        serde_wasm_bindgen::to_value(providers).unwrap_or(JsValue::NULL)
    }

    /// Register a gamepad the core has not seen yet, with the core's
    /// default button map, feeding no player. Returns whether it was new.
    #[wasm_bindgen(js_name = addGamepad)]
    pub fn add_gamepad(&mut self, gamepad_id: String) -> bool {
        self.genesis.input_manager_mut().add_gamepad(&gamepad_id)
    }

    /// Exchange everything feeding port 0 with everything feeding port 1.
    #[wasm_bindgen(js_name = swapPortAssignments)]
    pub fn swap_port_assignments(&mut self) {
        self.genesis.input_manager_mut().swap_ports(0, 1);
    }

    // ========================================================================
    // Input Remapping & Configuration
    // ========================================================================

    /// Set keyboard button mapping.
    /// mapping: JSON object { "KeyA": 6, "ArrowUp": 0, ... }
    /// Returns `false`, keeping the current mapping, when `mapping` is not
    /// such an object.
    #[wasm_bindgen(js_name = setKeyboardMapping)]
    pub fn set_keyboard_mapping(&mut self, mapping: JsValue) -> bool {
        let Ok(map) = serde_wasm_bindgen::from_value(mapping) else {
            return false;
        };
        self.genesis.input_manager_mut().set_keyboard_mapping(map);
        true
    }

    /// Get current keyboard button mapping.
    #[wasm_bindgen(js_name = getKeyboardMapping)]
    pub fn get_keyboard_mapping(&self) -> JsValue {
        let map = self.genesis.input_manager().get_keyboard_mapping();
        map_as_object(&map)
    }

    /// Clear all keyboard key state (release all keys).
    #[wasm_bindgen(js_name = clearKeyboardState)]
    pub fn clear_keyboard_state(&mut self) {
        self.genesis.input_manager_mut().clear_keyboard_state();
    }

    /// Set gamepad button mapping.
    /// mapping: JSON object { 0: 6, 1: 4, ... } (gamepad button index -> Genesis
    /// button index), whose keys JavaScript holds as text, or a `Map` with
    /// number keys. Returns `false`, keeping the pad's current map, when
    /// `mapping` is neither.
    #[wasm_bindgen(js_name = setGamepadMapping)]
    pub fn set_gamepad_mapping(&mut self, gamepad_id: String, mapping: JsValue) -> bool {
        let Some(map) = gamepad_map_from_js(mapping) else {
            return false;
        };
        self.genesis
            .input_manager_mut()
            .set_gamepad_mapping(&gamepad_id, map);
        true
    }

    /// Get current gamepad button mapping, as a plain object keyed by the
    /// button index written as text (`{ "0": 6, ... }`).
    #[wasm_bindgen(js_name = getGamepadMapping)]
    pub fn get_gamepad_mapping(&self, gamepad_id: String) -> JsValue {
        let map = self
            .genesis
            .input_manager()
            .get_gamepad_mapping(&gamepad_id);
        map_as_object(&gamepad_map_with_text_keys(&map))
    }

    /// Set gamepad deadzone (0.0 to 1.0).
    #[wasm_bindgen(js_name = setGamepadDeadzone)]
    pub fn set_gamepad_deadzone(&mut self, gamepad_id: String, deadzone: f32) {
        self.genesis
            .input_manager_mut()
            .set_gamepad_deadzone(&gamepad_id, deadzone);
    }

    /// Get gamepad deadzone.
    #[wasm_bindgen(js_name = getGamepadDeadzone)]
    pub fn get_gamepad_deadzone(&self, gamepad_id: String) -> f32 {
        self.genesis
            .input_manager()
            .get_gamepad_deadzone(&gamepad_id)
    }

    /// Clear all gamepad button/axis state (release everything).
    #[wasm_bindgen(js_name = clearGamepadState)]
    pub fn clear_gamepad_state(&mut self, gamepad_id: String) {
        self.genesis
            .input_manager_mut()
            .clear_gamepad_state(&gamepad_id);
    }

    /// Enable split keyboard for local 2-player.
    /// mapping_p1/mapping_p2: JSON objects { "KeyA": 6, ... }
    /// Returns `false`, changing nothing, when either is not such an object.
    #[wasm_bindgen(js_name = enableSplitKeyboard)]
    pub fn enable_split_keyboard(&mut self, mapping_p1: JsValue, mapping_p2: JsValue) -> bool {
        let (Ok(map_p1), Ok(map_p2)) = (
            serde_wasm_bindgen::from_value(mapping_p1),
            serde_wasm_bindgen::from_value(mapping_p2),
        ) else {
            return false;
        };
        self.genesis
            .input_manager_mut()
            .enable_split_keyboard(map_p1, map_p2);
        true
    }

    /// Disable split keyboard, revert to single keyboard.
    #[wasm_bindgen(js_name = disableSplitKeyboard)]
    pub fn disable_split_keyboard(&mut self) {
        self.genesis.input_manager_mut().disable_split_keyboard();
    }

    /// Check if split keyboard is enabled.
    #[wasm_bindgen(js_name = isSplitKeyboardEnabled)]
    pub fn is_split_keyboard_enabled(&self) -> bool {
        self.genesis.input_manager().is_split_keyboard_enabled()
    }
}

/// Writes the Mega Drive's part of a frame snapshot: `vsram`, a
/// `Uint16Array` of the vertical scroll RAM's 64 words, as `getVsram` gives
/// them.
fn snapshot_console(console: &GenesisSnapshotConsole, snapshot: &js_sys::Object) {
    set_property(snapshot, "vsram", &uint16_array(&console.vsram));
}

retro_wasm::capture_bindings!(RetroEmulator, genesis, console = snapshot_console);

#[cfg(test)]
mod tests {
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
        let list: Vec<BreakpointInfo> =
            serde_wasm_bindgen::from_value(emu.list_breakpoints()).unwrap();
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
        let regions = js_sys::Array::from(
            &js_sys::Reflect::get(&map, &JsValue::from_str("regions")).unwrap(),
        );
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

    /// A ROM whose 68000 loops at $400 with no interrupt on.
    fn looping_rom() -> Vec<u8> {
        let mut rom = synth_genesis_rom("LOOPING");
        rom[0x400..0x402].copy_from_slice(&[0x60, 0xFE]); // BRA *
        rom
    }

    #[wasm_bindgen_test]
    fn a_frame_held_at_its_capture_boundary_is_read_drawn_and_saved() {
        let mut emu = RetroEmulator::new("genesis").unwrap();
        emu.load_rom(&looping_rom()).unwrap();
        emu.set_execution_mode("Running".to_string());
        emu.run_frame();
        emu.run_to_capture_boundary();
        assert!(emu.at_capture_boundary());
        let frame = emu.get_frame_number();

        let snapshot = emu.capture_frame_snapshot();
        assert_plain_object(
            &snapshot,
            &[
                "frame",
                "atBoundary",
                "width",
                "height",
                "framebuffer",
                "vram",
                "palette",
                "vsram",
                "registers",
            ],
            "captureFrameSnapshot",
        );
        let get = |key: &str| js_sys::Reflect::get(&snapshot, &JsValue::from_str(key)).unwrap();
        // The held frame is counted once it ends: the picture is the next one.
        assert_eq!(get("frame").as_f64(), Some((frame + 1) as f64));
        assert_eq!(get("atBoundary").as_bool(), Some(true));
        assert_eq!(get("width").as_f64(), Some(320.0));
        assert_eq!(get("height").as_f64(), Some(224.0));
        for (key, length) in [("framebuffer", 320 * 224 * 4), ("vram", 0x10000)] {
            let bytes: js_sys::Uint8Array = get(key).dyn_into().expect(key);
            assert_eq!(bytes.length(), length, "{key}");
        }
        for key in ["palette", "vsram"] {
            let words: js_sys::Uint16Array = get(key).dyn_into().expect(key);
            assert_eq!(words.length(), 64, "{key}");
        }
        assert_eq!(js_sys::Array::from(&get("registers")).length(), 24);
        // Copies, not views into this module's memory.
        let vram: js_sys::Uint8Array = get("vram").dyn_into().unwrap();
        let memory: js_sys::WebAssembly::Memory = wasm_bindgen::memory().dyn_into().unwrap();
        assert!(!js_sys::Object::is(&vram.buffer(), &memory.buffer()));

        let layer = emu.render_layer(0);
        let data: js_sys::Uint8Array = js_sys::Reflect::get(&layer, &JsValue::from_str("data"))
            .unwrap()
            .dyn_into()
            .unwrap();
        assert_eq!(data.length(), 320 * 224 * 4);
        assert!(emu.render_layer(7).is_null());
        assert!(emu.at_capture_boundary(), "drawing a layer runs nothing");

        // A save finishes the held frame first.
        assert!(!emu.save_state().is_empty());
        assert!(!emu.at_capture_boundary());
        assert_eq!(emu.get_frame_number(), frame + 1);
    }

    /// `{ key: value, ... }` as the options a host passes.
    fn options(pairs: &[(&str, bool)]) -> JsValue {
        let obj = js_sys::Object::new();
        for &(key, on) in pairs {
            js_sys::Reflect::set(&obj, &JsValue::from_str(key), &JsValue::from_bool(on)).unwrap();
        }
        obj.into()
    }

    /// `value[key]`.
    fn field(value: &JsValue, key: &str) -> JsValue {
        js_sys::Reflect::get(value, &JsValue::from_str(key)).unwrap()
    }

    /// Whether `array` is a view into this module's memory.
    fn views_module_memory(buffer: &js_sys::ArrayBuffer) -> bool {
        let memory: js_sys::WebAssembly::Memory = wasm_bindgen::memory().dyn_into().unwrap();
        js_sys::Object::is(buffer, &memory.buffer())
    }

    #[wasm_bindgen_test]
    fn capture_recording_is_a_plain_object_and_a_key_left_out_keeps_its_value() {
        let mut emu = RetroEmulator::new("genesis").unwrap();
        let keys = [
            "lineStates",
            "pixelSources",
            "lineSprites",
            "videoMemorySources",
            "frameWrites",
        ];
        assert_plain_object(&emu.get_capture_recording(), &keys, "getCaptureRecording");
        let on =
            |emu: &RetroEmulator, key: &str| field(&emu.get_capture_recording(), key).as_bool();
        for key in keys {
            assert_eq!(on(&emu, key), Some(false), "{key} starts off");
        }

        emu.set_capture_recording(options(&[("lineStates", true)]));
        assert_eq!(on(&emu, "lineStates"), Some(true));
        emu.set_capture_recording(options(&[]));
        assert_eq!(
            on(&emu, "lineStates"),
            Some(true),
            "a key left out keeps its value"
        );
        emu.set_capture_recording(options(&[("pixelSources", true), ("lineSprites", true)]));
        assert_eq!(on(&emu, "lineStates"), Some(true));
        assert_eq!(on(&emu, "pixelSources"), Some(true), "read back as asked");
        assert_eq!(on(&emu, "lineSprites"), Some(true), "read back as asked");
        emu.set_capture_recording(options(&[
            ("videoMemorySources", true),
            ("frameWrites", true),
        ]));
        assert_eq!(
            on(&emu, "videoMemorySources"),
            Some(true),
            "read back as asked"
        );
        assert_eq!(on(&emu, "frameWrites"), Some(true), "read back as asked");
        emu.set_capture_recording(options(&[("lineStates", false)]));
        assert_eq!(on(&emu, "lineStates"), Some(false));
        assert_eq!(on(&emu, "pixelSources"), Some(true));
        // The pixel sources keep the line table on their own.
        emu.load_rom(&looping_rom()).unwrap();
        emu.set_execution_mode("Running".to_string());
        emu.run_frame();
        assert!(!emu.get_line_states().is_null());
        emu.set_capture_recording(options(&[("pixelSources", false), ("lineSprites", false)]));
        assert!(emu.get_line_states().is_null());
    }

    #[wasm_bindgen_test]
    fn line_states_their_palettes_and_the_picture_drawn_from_them_are_plain_objects_of_copies() {
        let mut emu = RetroEmulator::new("genesis").unwrap();
        emu.load_rom(&looping_rom()).unwrap();
        emu.set_execution_mode("Running".to_string());
        assert!(emu.get_line_states().is_null(), "nothing recorded yet");
        assert!(emu.get_line_palettes().is_null());
        assert!(emu.picture_from_line_states().is_null());
        let layer = emu.render_layer(0);
        assert_plain_object(
            &layer,
            &["width", "height", "data", "fromLineStates"],
            "renderLayer",
        );
        assert_eq!(field(&layer, "fromLineStates").as_bool(), Some(false));

        emu.set_capture_recording(options(&[("lineStates", true)]));
        emu.run_frame();
        emu.run_to_capture_boundary();

        let states = emu.get_line_states();
        assert_plain_object(
            &states,
            &[
                "frame",
                "fromFirstLine",
                "complete",
                "memoryExact",
                "rowsPerLine",
                "firstRowLine",
                "columns",
                "values",
            ],
            "getLineStates",
        );
        assert_eq!(
            field(&states, "frame").as_f64(),
            Some((emu.get_frame_number() + 1) as f64)
        );
        assert_eq!(field(&states, "complete").as_bool(), Some(true));
        assert_eq!(field(&states, "fromFirstLine").as_bool(), Some(true));
        assert_eq!(field(&states, "rowsPerLine").as_f64(), Some(1.0));
        let columns = js_sys::Array::from(&field(&states, "columns"));
        assert_eq!(columns.length(), 72);
        assert_eq!(columns.get(0).as_string().as_deref(), Some("line"));
        assert_eq!(columns.get(71).as_string().as_deref(), Some("palette"));
        let values: js_sys::Uint32Array = field(&states, "values").dyn_into().unwrap();
        assert_eq!(values.length(), 72 * 224);
        assert!(!views_module_memory(&values.buffer()));

        let palettes = emu.get_line_palettes();
        assert_plain_object(
            &palettes,
            &["coloursPerPalette", "palettes", "paletteOfLine", "changes"],
            "getLinePalettes",
        );
        assert_eq!(field(&palettes, "coloursPerPalette").as_f64(), Some(64.0));
        let of_line: js_sys::Uint16Array = field(&palettes, "paletteOfLine").dyn_into().unwrap();
        assert_eq!(of_line.length(), 224);
        let all: js_sys::Uint16Array = field(&palettes, "palettes").dyn_into().unwrap();
        assert!(all.length() > 0 && all.length().is_multiple_of(64));
        let changes: js_sys::Uint16Array = field(&palettes, "changes").dyn_into().unwrap();
        assert!(changes.length().is_multiple_of(5));
        assert!(!views_module_memory(&all.buffer()));

        let picture = emu.picture_from_line_states();
        assert_plain_object(
            &picture,
            &["width", "height", "data"],
            "pictureFromLineStates",
        );
        assert_eq!(field(&picture, "width").as_f64(), Some(320.0));
        assert_eq!(field(&picture, "height").as_f64(), Some(224.0));
        let data: js_sys::Uint8Array = field(&picture, "data").dyn_into().unwrap();
        assert!(!views_module_memory(&data.buffer()));
        assert_eq!(data.to_vec(), emu.get_framebuffer_data().to_vec());

        let layer = emu.render_layer(0);
        assert_eq!(field(&layer, "fromLineStates").as_bool(), Some(true));
        assert!(emu.at_capture_boundary(), "nothing ran");
    }

    /// The typed array `value[key]` holds, its length checked and that it
    /// is a copy, not a view into this module's memory.
    fn typed_array<A>(value: &JsValue, key: &str, length: u32) -> A
    where
        A: JsCast + AsRef<js_sys::Object>,
    {
        let array: A = field(value, key)
            .dyn_into()
            .unwrap_or_else(|_| panic!("`{key}` is the typed array named"));
        let view: &js_sys::Object = array.as_ref();
        let buffer: js_sys::ArrayBuffer = js_sys::Reflect::get(view, &JsValue::from_str("buffer"))
            .unwrap()
            .dyn_into()
            .unwrap();
        assert!(!views_module_memory(&buffer), "`{key}` is a copy");
        let found = js_sys::Reflect::get(view, &JsValue::from_str("length"))
            .unwrap()
            .as_f64();
        assert_eq!(found, Some(f64::from(length)), "`{key}`'s length");
        array
    }

    #[wasm_bindgen_test]
    fn pixel_sources_are_a_plain_object_of_copies_with_the_planes_asked_for() {
        let mut emu = RetroEmulator::new("genesis").unwrap();
        assert!(emu.get_pixel_sources(None).is_null(), "no game loaded");
        emu.load_rom(&looping_rom()).unwrap();
        emu.set_execution_mode("Running".to_string());
        emu.set_capture_recording(options(&[("pixelSources", true)]));
        emu.run_frame();
        emu.run_to_capture_boundary();

        let sources = emu.get_pixel_sources(None);
        assert_plain_object(
            &sources,
            &[
                "frame",
                "width",
                "height",
                "fromLineStates",
                "matchesPicture",
                "layer",
                "colour",
                "tile",
                "cell",
            ],
            "getPixelSources",
        );
        assert_eq!(
            field(&sources, "frame").as_f64(),
            Some((emu.get_frame_number() + 1) as f64)
        );
        assert_eq!(field(&sources, "width").as_f64(), Some(320.0));
        assert_eq!(field(&sources, "height").as_f64(), Some(224.0));
        assert_eq!(field(&sources, "fromLineStates").as_bool(), Some(true));
        assert_eq!(field(&sources, "matchesPicture").as_bool(), Some(true));
        let pixels = 320 * 224;
        typed_array::<js_sys::Uint16Array>(&sources, "layer", pixels);
        typed_array::<js_sys::Uint16Array>(&sources, "colour", pixels);
        typed_array::<js_sys::Uint32Array>(&sources, "tile", pixels);
        typed_array::<js_sys::Uint16Array>(&sources, "cell", pixels);

        // A plane not asked for is not there; bit 16 is ignored.
        let layer_only = emu.get_pixel_sources(Some(1 | 16));
        typed_array::<js_sys::Uint16Array>(&layer_only, "layer", pixels);
        for key in ["colour", "tile", "cell", "subLayer", "subColour"] {
            assert!(
                field(&layer_only, key).is_undefined(),
                "{key} not asked for"
            );
        }
        assert!(emu.at_capture_boundary(), "nothing ran");
    }

    #[wasm_bindgen_test]
    fn line_sprites_are_a_plain_object_of_copies_in_columns() {
        let mut emu = RetroEmulator::new("genesis").unwrap();
        assert!(emu.get_line_sprites().is_null(), "no game loaded");
        emu.load_rom(&looping_rom()).unwrap();
        emu.set_execution_mode("Running".to_string());
        emu.set_capture_recording(options(&[("lineSprites", true)]));
        emu.run_frame();
        emu.run_to_capture_boundary();

        let sprites = emu.get_line_sprites();
        assert_plain_object(
            &sprites,
            &[
                "frame",
                "fromLineStates",
                "lines",
                "slots",
                "fates",
                "x",
                "width",
                "drawnWidth",
            ],
            "getLineSprites",
        );
        assert_eq!(field(&sprites, "fromLineStates").as_bool(), Some(true));
        let lines: js_sys::Uint16Array = typed_array(&sprites, "lines", 224 * 4);
        let lines = lines.to_vec();
        let lines = lines.as_chunks::<4>().0;
        let entries: u32 = lines.iter().map(|line| u32::from(line[2])).sum();
        for (row, line) in lines.iter().enumerate() {
            assert_eq!(usize::from(line[0]), row, "every line in order");
        }
        typed_array::<js_sys::Uint8Array>(&sprites, "slots", entries);
        typed_array::<js_sys::Uint8Array>(&sprites, "fates", entries);
        typed_array::<js_sys::Int16Array>(&sprites, "x", entries);
        typed_array::<js_sys::Uint8Array>(&sprites, "width", entries);
        typed_array::<js_sys::Uint8Array>(&sprites, "drawnWidth", entries);
    }

    #[wasm_bindgen_test]
    fn chosen_sprites_drawn_alone_have_the_shape_of_a_layer() {
        let mut emu = RetroEmulator::new("genesis").unwrap();
        assert!(emu.render_sprites(&[1; 80]).is_null(), "no game loaded");
        emu.load_rom(&looping_rom()).unwrap();
        emu.set_execution_mode("Running".to_string());
        emu.set_capture_recording(options(&[("lineStates", true)]));
        emu.run_frame();
        emu.run_to_capture_boundary();

        let all = emu.render_sprites(&[1; 80]);
        assert_plain_object(
            &all,
            &["width", "height", "data", "fromLineStates"],
            "renderSprites",
        );
        assert_eq!(field(&all, "width").as_f64(), Some(320.0));
        assert_eq!(field(&all, "height").as_f64(), Some(224.0));
        assert_eq!(field(&all, "fromLineStates").as_bool(), Some(true));
        let data: js_sys::Uint8Array = typed_array(&all, "data", 320 * 224 * 4);
        let layer = emu.render_layer(3);
        let layer_data: js_sys::Uint8Array = field(&layer, "data").dyn_into().unwrap();
        assert_eq!(data.to_vec(), layer_data.to_vec(), "every slot: the layer");
        let none = emu.render_sprites(&[]);
        let none: js_sys::Uint8Array = field(&none, "data").dyn_into().unwrap();
        assert!(none.to_vec().iter().all(|&byte| byte == 0));
    }

    #[wasm_bindgen_test]
    fn the_sprite_table_stops_where_the_renderer_stops() {
        let mut emu = RetroEmulator::new("genesis").unwrap();
        // 40 cells and the table at 0, the VDP's state after a reset: slot 0
        // links to slot 100, past the table's 80 slots.
        emu.write_vram(3, &[100]);
        assert_eq!(
            js_sys::Array::from(&emu.get_sprite_attr_table()).length(),
            1
        );
    }

    #[wasm_bindgen_test]
    fn write_vram_reads_back_through_get_vram_wrapping_at_the_end() {
        let mut emu = RetroEmulator::new("genesis").unwrap();
        emu.write_vram(0xFFFE, &[1, 2, 3, 4]);
        let vram = emu.get_vram();
        assert_eq!(&vram[0xFFFE..], &[1, 2]);
        assert_eq!(&vram[..2], &[3, 4], "the run goes on from the start");

        emu.write_vram(0x1_0005, &[9]);
        assert_eq!(emu.get_vram()[5], 9, "an address past the end wraps");

        let before = emu.get_vram();
        emu.write_vram(0x10, &[]);
        assert_eq!(emu.get_vram(), before, "an empty write changes nothing");
    }

    #[wasm_bindgen_test]
    fn write_cram_reads_back_and_shows_after_a_repaint() {
        let mut emu = RetroEmulator::new("genesis").unwrap();
        // Colour 63, then on round to colour 0, keeping only the colour bits.
        emu.write_cram(63, &[0xFFFF, 0x000E]);
        let cram = emu.get_cram();
        assert_eq!(cram[63], 0x0EEE);
        assert_eq!(cram[0], 0x000E);

        // A fresh VDP has its display off, so the screen is the backdrop:
        // colour 0, red now.
        emu.repaint_frame();
        let (r, g, b) = sg1_core::vdp::Vdp::convert_genesis_color(0x000E);
        assert_eq!(&emu.get_framebuffer_data().to_vec()[..4], &[r, g, b, 255]);
    }

    #[wasm_bindgen_test]
    fn write_vsram_reads_back_eleven_bits_wrapping_at_the_end() {
        let mut emu = RetroEmulator::new("genesis").unwrap();
        emu.write_vsram(63, &[0xFFFF, 0x0123]);
        let vsram = emu.get_vsram();
        assert_eq!(vsram[63], 0x07FF);
        assert_eq!(vsram[0], 0x0123);

        let long: Vec<u16> = (0..70).map(|i| if i < 64 { 1 } else { 2 }).collect();
        emu.write_vsram(10, &long);
        assert!(
            emu.get_vsram().iter().all(|&value| value == 1),
            "a buffer longer than VSRAM is cut to it"
        );
    }

    /// A ROM whose 68000 stores colour 0 through the data port, then loops.
    fn colour_store_rom() -> Vec<u8> {
        let mut rom = synth_genesis_rom("COLOUR STORE");
        #[rustfmt::skip]
        let code: [u16; 10] = [
            0x23FC, 0xC000, 0x0000, 0x00C0, 0x0004, // MOVE.L #$C0000000,($C00004).L
            0x33FC, 0x0EEE, 0x00C0, 0x0000,         // MOVE.W #$0EEE,($C00000).L
            0x60FE,                                 // BRA *
        ];
        for (i, word) in code.iter().enumerate() {
            rom[0x400 + i * 2..0x402 + i * 2].copy_from_slice(&word.to_be_bytes());
        }
        rom
    }

    #[wasm_bindgen_test]
    fn the_writes_to_video_memory_are_plain_objects_of_copies() {
        let mut emu = RetroEmulator::new("genesis").unwrap();
        emu.load_rom(&colour_store_rom()).unwrap();
        assert!(emu.get_video_memory_sources().is_null(), "not recorded");
        assert!(emu.take_frame_writes().is_null(), "not recorded");
        emu.set_capture_recording(options(&[
            ("videoMemorySources", true),
            ("frameWrites", true),
        ]));
        emu.set_execution_mode("Running".to_string());
        emu.run_frame();
        emu.write_vram(0x20, &[1, 2]);

        let sources = emu.get_video_memory_sources();
        assert_plain_object(&sources, &["origins", "frames"], "getVideoMemorySources");
        let origins: js_sys::Uint32Array = field(&sources, "origins").dyn_into().unwrap();
        let frames: js_sys::Uint32Array = field(&sources, "frames").dyn_into().unwrap();
        assert_eq!((origins.length(), frames.length()), (0x1_0000, 0x1_0000));
        assert!(!views_module_memory(&origins.buffer()));
        assert!(!views_module_memory(&frames.buffer()));
        assert_eq!(origins.get_index(0x20) >> 28, 4, "the debugger wrote it");
        assert_eq!(origins.get_index(0x22), 0, "nothing wrote it");

        let writes = emu.take_frame_writes();
        assert_plain_object(
            &writes,
            &["records", "wordsPerRecord", "dropped"],
            "takeFrameWrites",
        );
        assert_eq!(field(&writes, "wordsPerRecord").as_f64(), Some(8.0));
        assert_eq!(field(&writes, "dropped").as_f64(), Some(0.0));
        let records: js_sys::Uint32Array = field(&writes, "records").dyn_into().unwrap();
        assert!(!views_module_memory(&records.buffer()));
        assert_eq!(records.length(), 8, "the colour store");
        assert_eq!(records.get_index(0), 4 | (3 << 16));
        assert_eq!(records.get_index(4), 1 | (2 << 8));
        assert_eq!((records.get_index(5), records.get_index(6)), (0, 0x0EEE));
        let again = emu.take_frame_writes();
        let again: js_sys::Uint32Array = field(&again, "records").dyn_into().unwrap();
        assert_eq!(again.length(), 0, "a take empties the record");

        let resolved = emu.resolve_sources(&[0x40C, 0xFF_0000, 0xC0_0000]);
        assert!(resolved.is_instance_of::<js_sys::Int32Array>());
        assert!(!views_module_memory(&resolved.buffer()));
        assert_eq!(resolved.to_vec(), [0x40C, -2, -1]);
    }

    // ===== What each capture reader returns, whole =====
    //
    // Each reader's value on a program that draws, moves its sprites and
    // writes video memory every way the 68000 can, described in full by
    // `retro_wasm::test_support::shape` (every key, every value, each typed
    // array's constructor, length and a hash of its bytes) and held to the
    // description recorded from these bindings. A change that moves a key,
    // a value, an array's type or one byte of what a host reads fails here.

    /// Place `words` at `address` of `rom`, high byte first.
    fn place(rom: &mut [u8], address: usize, words: &[u16]) {
        for (i, word) in words.iter().enumerate() {
            let at = address + i * 2;
            rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
        }
    }

    /// A program that draws both planes and 48 sprites, moves two of the
    /// sprites every frame, and writes video memory every way the 68000
    /// can. As it boots it sends the tiles, plane A's map, the palette and
    /// the sprite table by transfer, fills four tiles, copies two of them,
    /// and stores plane B's cells, a colour, the vertical scroll and a
    /// sprite's Y through the data port. Its vertical interrupt moves
    /// sprites 0 and 1, puts colour 1 and plane A's horizontal scroll back
    /// and sends a tile again by transfer; its horizontal interrupt, every
    /// 100 lines, changes colour 1 and plane A's horizontal scroll part of
    /// the way down the picture. The sprites meet the line's sprite limit
    /// on lines 120 to 127, run out of dots on lines 160 to 167, meet the
    /// sprite mask on lines 200 to 207 and lie off screen on lines 60 to
    /// 67.
    fn capture_shapes_rom() -> Vec<u8> {
        let mut rom = synth_genesis_rom("CAPTURE SHAPES");
        place(&mut rom, 0x00, &[0x00FF, 0xFE00]); // the stack
        place(&mut rom, 0x70, &[0x0000, 0x0700]); // level 4: the horizontal interrupt
        place(&mut rom, 0x78, &[0x0000, 0x0600]); // level 6: the vertical interrupt
        #[rustfmt::skip]
        place(&mut rom, 0x400, &[
            0x46FC, 0x2700,                 // MOVE.W #$2700,SR
            0x41F9, 0x00C0, 0x0004,         // LEA $C00004,A0: control port
            0x43F9, 0x00C0, 0x0000,         // LEA $C00000,A1: data port
            0x30BC, 0x8014,                 // register 0: horizontal interrupt on
            0x30BC, 0x8174,                 // register 1: display, vertical interrupt, transfers
            0x30BC, 0x8230,                 // register 2: plane A at $C000
            0x30BC, 0x8407,                 // register 4: plane B at $E000
            0x30BC, 0x8578,                 // register 5: sprites at $F000
            0x30BC, 0x8700,                 // register 7: backdrop colour 0
            0x30BC, 0x8A63,                 // register 10: an interrupt every 100 lines
            0x30BC, 0x8B00,                 // register 11: whole-screen scrolls
            0x30BC, 0x8C81,                 // register 12: 40 cells
            0x30BC, 0x8D3F,                 // register 13: horizontal scroll at $FC00
            0x30BC, 0x8F02,                 // register 15: auto-increment 2
            0x30BC, 0x9001,                 // register 16: planes of 64 by 32 cells
            // Tiles 1 to 16 from ROM $1000: 256 words.
            0x30BC, 0x9300, 0x30BC, 0x9401,
            0x30BC, 0x9500, 0x30BC, 0x9608, 0x30BC, 0x9700,
            0x20BC, 0x4020, 0x0080,         // VRAM $0020 by transfer
            // Plane A's map from ROM $2000: 2048 words.
            0x30BC, 0x9300, 0x30BC, 0x9408,
            0x30BC, 0x9500, 0x30BC, 0x9610, 0x30BC, 0x9700,
            0x20BC, 0x4000, 0x0083,         // VRAM $C000 by transfer
            // Tiles 20 to 23 filled with colour 3, increment 1.
            0x30BC, 0x8F01,
            0x30BC, 0x9380, 0x30BC, 0x9400,
            0x30BC, 0x9780,                 // fill
            0x20BC, 0x4280, 0x0080,         // VRAM $0280 by fill
            0x32BC, 0x3333,                 // MOVE.W #$3333,(A1): starts the fill
            0x3010,                         // MOVE.W (A0),D0: the status
            0x0800, 0x0001,                 // BTST #1,D0: a transfer running
            0x66F8,                         // BNE.S back to the status
            // Tiles 24 and 25 copied from tiles 20 and 21.
            0x30BC, 0x9340, 0x30BC, 0x9400,
            0x30BC, 0x9580, 0x30BC, 0x9602, // from $0280
            0x30BC, 0x97C0,                 // copy
            0x20BC, 0x0300, 0x00C0,         // VRAM $0300 by copy
            0x3010,
            0x0800, 0x0001,
            0x66F8,
            0x30BC, 0x8F02,                 // register 15: auto-increment 2
            // Eight cells of plane B on row 5, tiles 20 and 24 by turns.
            0x20BC, 0x6280, 0x0003,         // VRAM $E280
            0x32BC, 0x0014, 0x32BC, 0x0018, 0x32BC, 0x0014, 0x32BC, 0x0018,
            0x32BC, 0x0014, 0x32BC, 0x0018, 0x32BC, 0x0014, 0x32BC, 0x0018,
            // The palette from ROM $1200: 64 words.
            0x30BC, 0x9340, 0x30BC, 0x9400,
            0x30BC, 0x9500, 0x30BC, 0x9609, 0x30BC, 0x9700,
            0x20BC, 0xC000, 0x0080,         // CRAM colour 0 by transfer
            0x20BC, 0xC002, 0x0000,         // CRAM colour 1
            0x32BC, 0x000E,                 // MOVE.W #$000E,(A1)
            0x20BC, 0x4000, 0x0010,         // VSRAM word 0
            0x32BC, 0x0004,                 // plane A four lines up
            0x32BC, 0x0000,                 // plane B
            // The 48 sprites from ROM $1300: 192 words.
            0x30BC, 0x93C0, 0x30BC, 0x9400,
            0x30BC, 0x9580, 0x30BC, 0x9609, 0x30BC, 0x9700,
            0x20BC, 0x7000, 0x0083,         // VRAM $F000 by transfer
            0x20BC, 0x7000, 0x0003,         // VRAM $F000
            0x32BC, 0x0098,                 // sprite 0's Y: 24
            0x46FC, 0x2000,                 // MOVE.W #$2000,SR: interrupts on
            0x60FE,                         // BRA *
        ]);
        #[rustfmt::skip]
        place(&mut rom, 0x600, &[
            0x5279, 0x00FF, 0x0000,         // ADDQ.W #1,($FF0000).L: the frames
            0x3039, 0x00FF, 0x0000,         // MOVE.W ($FF0000).L,D0
            0x0640, 0x00A0,                 // ADDI.W #$A0,D0
            0x20BC, 0x7006, 0x0003,         // VRAM $F006: sprite 0's X
            0x3280,                         // MOVE.W D0,(A1): 32 + the frames
            0x0640, 0x0028,                 // ADDI.W #40,D0
            0x20BC, 0x700E, 0x0003,         // VRAM $F00E: sprite 1's X
            0x3280,                         // MOVE.W D0,(A1)
            0x20BC, 0xC002, 0x0000,         // CRAM colour 1
            0x32BC, 0x000E,                 // back to red
            0x20BC, 0x7C00, 0x0003,         // VRAM $FC00: plane A's horizontal scroll
            0x32BC, 0x0000,                 // back to 0
            // Tile 1 sent again, as tile 40: 16 words.
            0x30BC, 0x9310, 0x30BC, 0x9400,
            0x30BC, 0x9500, 0x30BC, 0x9608, 0x30BC, 0x9700,
            0x20BC, 0x4500, 0x0080,         // VRAM $0500 by transfer
            0x4E73,                         // RTE
        ]);
        #[rustfmt::skip]
        place(&mut rom, 0x700, &[
            0x20BC, 0xC002, 0x0000,         // CRAM colour 1
            0x32BC, 0x00E0,                 // green from here on
            0x20BC, 0x7C00, 0x0003,         // VRAM $FC00
            0x32BC, 0x0010,                 // plane A sixteen pixels right
            0x4E73,                         // RTE
        ]);
        // Tiles 1 to 16: diagonal stripes of every colour, with gaps.
        for tile in 0..16usize {
            for row in 0..8usize {
                let colour = |x: usize| -> u8 {
                    if (x + row + tile).is_multiple_of(5) {
                        0
                    } else {
                        ((tile + x * 2 + row) % 15 + 1) as u8
                    }
                };
                for pair in 0..4usize {
                    rom[0x1000 + tile * 32 + row * 4 + pair] =
                        (colour(pair * 2) << 4) | colour(pair * 2 + 1);
                }
            }
        }
        // The palette: 64 colours.
        for colour in 0..64u16 {
            let word = ((colour % 8) << 1) | (((colour / 8) % 8) << 5) | (((colour * 3) % 8) << 9);
            place(&mut rom, 0x1200 + usize::from(colour) * 2, &[word]);
        }
        // The sprites, linked in slot order, each (y, size, the table's x).
        for slot in 0..48u16 {
            let (y, size, x) = match slot {
                0..=7 => (
                    16 + slot * 24,
                    ((slot % 4) << 2) | (slot % 4),
                    152 + slot * 36,
                ),
                8..=29 => (120, 0, 128 + (slot - 8) * 14),
                30..=41 => (160, 0b1100, 128 + (slot - 30) * 30),
                42 => (200, 0, 228),
                43 => (200, 0, 0),
                44 => (200, 0, 278),
                45 => (60, 0, 88),
                46 => (60, 0b0101, 458),
                _ => (60, 0, 300),
            };
            let link = if slot == 47 { 0 } else { slot + 1 };
            let pattern = ((slot % 2) << 15)
                | ((slot % 4) << 13)
                | ((slot % 3 / 2) << 12)
                | ((slot % 5 / 4) << 11)
                | (1 + slot % 16);
            let entry = [128 + y, (size << 8) | link, pattern, x];
            place(&mut rom, 0x1300 + usize::from(slot) * 8, &entry);
        }
        // Plane A's map, 64 by 32 cells.
        for row in 0..32u16 {
            for column in 0..64u16 {
                let cell = (u16::from(column % 7 == 0) << 15)
                    | (((row / 8) % 4) << 13)
                    | (u16::from(column % 3 == 0) << 12)
                    | (u16::from(row % 5 == 0) << 11)
                    | (1 + (row + column) % 16);
                place(
                    &mut rom,
                    0x2000 + usize::from(row * 64 + column) * 2,
                    &[cell],
                );
            }
        }
        rom
    }

    /// Every recorder on.
    const EVERY_RECORDER: [(&str, bool); 5] = [
        ("lineStates", true),
        ("pixelSources", true),
        ("lineSprites", true),
        ("videoMemorySources", true),
        ("frameWrites", true),
    ];

    /// [`capture_shapes_rom`] with `recorded` on from its first frame, run
    /// five frames and the sixth to its capture boundary.
    fn held_capture_shapes(recorded: &[(&str, bool)]) -> RetroEmulator {
        let mut emu = RetroEmulator::new("genesis").unwrap();
        emu.load_rom(&capture_shapes_rom()).unwrap();
        emu.set_execution_mode("Running".to_string());
        emu.set_capture_recording(options(recorded));
        for _ in 0..5 {
            emu.run_frame();
        }
        emu.run_to_capture_boundary();
        emu
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

    #[wasm_bindgen_test]
    fn what_a_frame_snapshot_holds() {
        let mut emu = held_capture_shapes(&EVERY_RECORDER);
        let held = emu.capture_frame_snapshot();
        emu.run_frame();
        let after_the_frame = emu.capture_frame_snapshot();
        assert_shapes(
            &[
                ("held at the boundary", held),
                ("after the frame", after_the_frame),
            ],
            &[
                (
                    "held at the boundary",
                    "{atBoundary: true, frame: 6, framebuffer: Uint8Array(len 286720, \
                     fnv1a64 0x90450a1e4b4824fc), height: 224, palette: Uint16Array(len 64, \
                     fnv1a64 0xd9034ab48a655301), registers: [{description: \"\", index: 0, \
                     name: \"Mode Register 1\", value: 20}, {description: \"\", index: 1, \
                     name: \"Mode Register 2\", value: 116}, {description: \"\", index: 2, \
                     name: \"Plane A Name Table\", value: 48}, {description: \"\", index: 3, \
                     name: \"Window Name Table\", value: 0}, {description: \"\", index: 4, \
                     name: \"Plane B Name Table\", value: 7}, {description: \"\", index: 5, \
                     name: \"Sprite Table\", value: 120}, {description: \"\", index: 6, \
                     name: \"Sprite Pattern Generator\", value: 0}, {description: \"\", index: 7, \
                     name: \"Background Color\", value: 0}, {description: \"\", index: 8, \
                     name: \"Unused ($08)\", value: 0}, {description: \"\", index: 9, \
                     name: \"Unused ($09)\", value: 0}, {description: \"\", index: 10, \
                     name: \"H Interrupt Counter\", value: 99}, {description: \"\", index: 11, \
                     name: \"Mode Register 3\", value: 0}, {description: \"\", index: 12, \
                     name: \"Mode Register 4\", value: 129}, {description: \"\", index: 13, \
                     name: \"H Scroll Table\", value: 63}, {description: \"\", index: 14, \
                     name: \"Plane Pattern Generator\", value: 0}, {description: \"\", index: 15, \
                     name: \"Auto Increment\", value: 2}, {description: \"\", index: 16, \
                     name: \"Plane Size\", value: 1}, {description: \"\", index: 17, \
                     name: \"Window H Position\", value: 0}, {description: \"\", index: 18, \
                     name: \"Window V Position\", value: 0}, {description: \"\", index: 19, \
                     name: \"DMA Length Low\", value: 0}, {description: \"\", index: 20, \
                     name: \"DMA Length High\", value: 0}, {description: \"\", index: 21, \
                     name: \"DMA Source Low\", value: 16}, {description: \"\", index: 22, \
                     name: \"DMA Source Mid\", value: 8}, {description: \"\", index: 23, \
                     name: \"DMA Source High\", value: 0}], vram: Uint8Array(len 65536, \
                     fnv1a64 0x8a29d79466aa96ef), vsram: Uint16Array(len 64, \
                     fnv1a64 0xbb0bb9ec6512e521), width: 320}",
                ),
                (
                    "after the frame",
                    "{atBoundary: false, frame: 6, framebuffer: Uint8Array(len 286720, \
                     fnv1a64 0x90450a1e4b4824fc), height: 224, palette: Uint16Array(len 64, \
                     fnv1a64 0x80a6cbe3ccc7196f), registers: [{description: \"\", index: 0, \
                     name: \"Mode Register 1\", value: 20}, {description: \"\", index: 1, \
                     name: \"Mode Register 2\", value: 116}, {description: \"\", index: 2, \
                     name: \"Plane A Name Table\", value: 48}, {description: \"\", index: 3, \
                     name: \"Window Name Table\", value: 0}, {description: \"\", index: 4, \
                     name: \"Plane B Name Table\", value: 7}, {description: \"\", index: 5, \
                     name: \"Sprite Table\", value: 120}, {description: \"\", index: 6, \
                     name: \"Sprite Pattern Generator\", value: 0}, {description: \"\", index: 7, \
                     name: \"Background Color\", value: 0}, {description: \"\", index: 8, \
                     name: \"Unused ($08)\", value: 0}, {description: \"\", index: 9, \
                     name: \"Unused ($09)\", value: 0}, {description: \"\", index: 10, \
                     name: \"H Interrupt Counter\", value: 99}, {description: \"\", index: 11, \
                     name: \"Mode Register 3\", value: 0}, {description: \"\", index: 12, \
                     name: \"Mode Register 4\", value: 129}, {description: \"\", index: 13, \
                     name: \"H Scroll Table\", value: 63}, {description: \"\", index: 14, \
                     name: \"Plane Pattern Generator\", value: 0}, {description: \"\", index: 15, \
                     name: \"Auto Increment\", value: 2}, {description: \"\", index: 16, \
                     name: \"Plane Size\", value: 1}, {description: \"\", index: 17, \
                     name: \"Window H Position\", value: 0}, {description: \"\", index: 18, \
                     name: \"Window V Position\", value: 0}, {description: \"\", index: 19, \
                     name: \"DMA Length Low\", value: 0}, {description: \"\", index: 20, \
                     name: \"DMA Length High\", value: 0}, {description: \"\", index: 21, \
                     name: \"DMA Source Low\", value: 16}, {description: \"\", index: 22, \
                     name: \"DMA Source Mid\", value: 8}, {description: \"\", index: 23, \
                     name: \"DMA Source High\", value: 0}], vram: Uint8Array(len 65536, \
                     fnv1a64 0x1c28c67504e3498b), vsram: Uint16Array(len 64, \
                     fnv1a64 0xbb0bb9ec6512e521), width: 320}",
                ),
            ],
        );
    }

    #[wasm_bindgen_test]
    fn what_each_layer_drawn_alone_holds() {
        let mut emu = held_capture_shapes(&EVERY_RECORDER);
        let mut found = Vec::new();
        for (what, layer) in [
            ("plane A", 0),
            ("plane B", 1),
            ("window", 2),
            ("sprites", 3),
            ("layer 4", 4),
            ("layer 99", 99),
        ] {
            found.push((what, emu.render_layer(layer)));
        }
        let mut unrecorded = held_capture_shapes(&[]);
        found.push(("plane A, nothing recorded", unrecorded.render_layer(0)));
        assert_shapes(
            &found,
            &[
                (
                    "plane A",
                    "{data: Uint8Array(len 286720, fnv1a64 0xbea5713f2915922e), \
                     fromLineStates: true, height: 224, width: 320}",
                ),
                (
                    "plane B",
                    "{data: Uint8Array(len 286720, fnv1a64 0x4bbb1fba9a66c325), \
                     fromLineStates: true, height: 224, width: 320}",
                ),
                (
                    "window",
                    "{data: Uint8Array(len 286720, fnv1a64 0xe97846eb7709a325), \
                     fromLineStates: true, height: 224, width: 320}",
                ),
                (
                    "sprites",
                    "{data: Uint8Array(len 286720, fnv1a64 0xa2af1da2713f146f), \
                     fromLineStates: true, height: 224, width: 320}",
                ),
                ("layer 4", "null"),
                ("layer 99", "null"),
                (
                    "plane A, nothing recorded",
                    "{data: Uint8Array(len 286720, fnv1a64 0xf1ef581015fff896), \
                     fromLineStates: false, height: 224, width: 320}",
                ),
            ],
        );
    }

    #[wasm_bindgen_test]
    fn what_the_capture_recording_reads_back() {
        let mut emu = RetroEmulator::new("genesis").unwrap();
        let mut found = vec![("at first", emu.get_capture_recording())];
        emu.set_capture_recording(options(&[("lineStates", true), ("frameWrites", true)]));
        found.push(("two turned on", emu.get_capture_recording()));
        emu.set_capture_recording(options(&[("pixelSources", true), ("frameWrites", false)]));
        found.push(("a partial object", emu.get_capture_recording()));
        emu.set_capture_recording(options(&[]));
        found.push(("an empty object", emu.get_capture_recording()));
        emu.set_capture_recording(JsValue::UNDEFINED);
        found.push(("no object", emu.get_capture_recording()));
        let not_booleans = js_sys::Object::new();
        for (key, value) in [
            ("lineSprites", JsValue::from_f64(1.0)),
            ("lineStates", JsValue::from_str("false")),
            ("videoMemorySources", JsValue::NULL),
        ] {
            js_sys::Reflect::set(&not_booleans, &JsValue::from_str(key), &value).unwrap();
        }
        emu.set_capture_recording(not_booleans.into());
        found.push(("values that are not booleans", emu.get_capture_recording()));
        emu.set_capture_recording(options(&EVERY_RECORDER));
        found.push(("every recorder", emu.get_capture_recording()));
        assert_shapes(
            &found,
            &[
                (
                    "at first",
                    "{frameWrites: false, lineSprites: false, lineStates: false, \
                     pixelSources: false, videoMemorySources: false}",
                ),
                (
                    "two turned on",
                    "{frameWrites: true, lineSprites: false, lineStates: true, \
                     pixelSources: false, videoMemorySources: false}",
                ),
                (
                    "a partial object",
                    "{frameWrites: false, lineSprites: false, lineStates: true, \
                     pixelSources: true, videoMemorySources: false}",
                ),
                (
                    "an empty object",
                    "{frameWrites: false, lineSprites: false, lineStates: true, \
                     pixelSources: true, videoMemorySources: false}",
                ),
                (
                    "no object",
                    "{frameWrites: false, lineSprites: false, lineStates: true, \
                     pixelSources: true, videoMemorySources: false}",
                ),
                (
                    "values that are not booleans",
                    "{frameWrites: false, lineSprites: false, lineStates: true, \
                     pixelSources: true, videoMemorySources: false}",
                ),
                (
                    "every recorder",
                    "{frameWrites: true, lineSprites: true, lineStates: true, pixelSources: true, \
                     videoMemorySources: true}",
                ),
            ],
        );
    }

    #[wasm_bindgen_test]
    fn what_the_line_states_hold() {
        let recorded = held_capture_shapes(&EVERY_RECORDER);
        let for_the_sprites = held_capture_shapes(&[("lineSprites", true)]);
        let unrecorded = held_capture_shapes(&[]);
        assert_shapes(
            &[
                ("recorded", recorded.get_line_states()),
                (
                    "kept for the line sprites",
                    for_the_sprites.get_line_states(),
                ),
                ("not recorded", unrecorded.get_line_states()),
            ],
            &[
                (
                    "recorded",
                    "{columns: [\"line\", \"frame\", \"kind\", \"exact\", \"register_0\", \
                     \"register_1\", \"register_2\", \"register_3\", \"register_4\", \
                     \"register_5\", \"register_6\", \"register_7\", \"register_8\", \
                     \"register_9\", \"register_10\", \"register_11\", \"register_12\", \
                     \"register_13\", \"register_14\", \"register_15\", \"register_16\", \
                     \"register_17\", \"register_18\", \"register_19\", \"register_20\", \
                     \"register_21\", \"register_22\", \"register_23\", \
                     \"plane_a_horizontal_scroll\", \"plane_b_horizontal_scroll\", \
                     \"vertical_scroll_0\", \"vertical_scroll_1\", \"vertical_scroll_2\", \
                     \"vertical_scroll_3\", \"vertical_scroll_4\", \"vertical_scroll_5\", \
                     \"vertical_scroll_6\", \"vertical_scroll_7\", \"vertical_scroll_8\", \
                     \"vertical_scroll_9\", \"vertical_scroll_10\", \"vertical_scroll_11\", \
                     \"vertical_scroll_12\", \"vertical_scroll_13\", \"vertical_scroll_14\", \
                     \"vertical_scroll_15\", \"vertical_scroll_16\", \"vertical_scroll_17\", \
                     \"vertical_scroll_18\", \"vertical_scroll_19\", \"vertical_scroll_20\", \
                     \"vertical_scroll_21\", \"vertical_scroll_22\", \"vertical_scroll_23\", \
                     \"vertical_scroll_24\", \"vertical_scroll_25\", \"vertical_scroll_26\", \
                     \"vertical_scroll_27\", \"vertical_scroll_28\", \"vertical_scroll_29\", \
                     \"vertical_scroll_30\", \"vertical_scroll_31\", \"vertical_scroll_32\", \
                     \"vertical_scroll_33\", \"vertical_scroll_34\", \"vertical_scroll_35\", \
                     \"vertical_scroll_36\", \"vertical_scroll_37\", \"vertical_scroll_38\", \
                     \"vertical_scroll_39\", \"sprite_mask_carried_in\", \"palette\"], \
                     complete: true, firstRowLine: 0, frame: 6, fromFirstLine: true, \
                     memoryExact: true, rowsPerLine: 1, values: Uint32Array(len 16128, \
                     fnv1a64 0x6ff1a0129c1a30e5)}",
                ),
                (
                    "kept for the line sprites",
                    "{columns: [\"line\", \"frame\", \"kind\", \"exact\", \"register_0\", \
                     \"register_1\", \"register_2\", \"register_3\", \"register_4\", \
                     \"register_5\", \"register_6\", \"register_7\", \"register_8\", \
                     \"register_9\", \"register_10\", \"register_11\", \"register_12\", \
                     \"register_13\", \"register_14\", \"register_15\", \"register_16\", \
                     \"register_17\", \"register_18\", \"register_19\", \"register_20\", \
                     \"register_21\", \"register_22\", \"register_23\", \
                     \"plane_a_horizontal_scroll\", \"plane_b_horizontal_scroll\", \
                     \"vertical_scroll_0\", \"vertical_scroll_1\", \"vertical_scroll_2\", \
                     \"vertical_scroll_3\", \"vertical_scroll_4\", \"vertical_scroll_5\", \
                     \"vertical_scroll_6\", \"vertical_scroll_7\", \"vertical_scroll_8\", \
                     \"vertical_scroll_9\", \"vertical_scroll_10\", \"vertical_scroll_11\", \
                     \"vertical_scroll_12\", \"vertical_scroll_13\", \"vertical_scroll_14\", \
                     \"vertical_scroll_15\", \"vertical_scroll_16\", \"vertical_scroll_17\", \
                     \"vertical_scroll_18\", \"vertical_scroll_19\", \"vertical_scroll_20\", \
                     \"vertical_scroll_21\", \"vertical_scroll_22\", \"vertical_scroll_23\", \
                     \"vertical_scroll_24\", \"vertical_scroll_25\", \"vertical_scroll_26\", \
                     \"vertical_scroll_27\", \"vertical_scroll_28\", \"vertical_scroll_29\", \
                     \"vertical_scroll_30\", \"vertical_scroll_31\", \"vertical_scroll_32\", \
                     \"vertical_scroll_33\", \"vertical_scroll_34\", \"vertical_scroll_35\", \
                     \"vertical_scroll_36\", \"vertical_scroll_37\", \"vertical_scroll_38\", \
                     \"vertical_scroll_39\", \"sprite_mask_carried_in\", \"palette\"], \
                     complete: true, firstRowLine: 0, frame: 6, fromFirstLine: true, \
                     memoryExact: true, rowsPerLine: 1, values: Uint32Array(len 16128, \
                     fnv1a64 0x6ff1a0129c1a30e5)}",
                ),
                ("not recorded", "null"),
            ],
        );
    }

    #[wasm_bindgen_test]
    fn what_the_line_palettes_hold() {
        let recorded = held_capture_shapes(&EVERY_RECORDER);
        let unrecorded = held_capture_shapes(&[]);
        assert_shapes(
            &[
                ("recorded", recorded.get_line_palettes()),
                ("not recorded", unrecorded.get_line_palettes()),
            ],
            &[
                (
                    "recorded",
                    "{changes: Uint16Array(len 10, fnv1a64 0x978cad4acc7ab705), \
                     coloursPerPalette: 64, paletteOfLine: Uint16Array(len 224, \
                     fnv1a64 0x5f8fc5aa294a6f75), palettes: Uint16Array(len 128, \
                     fnv1a64 0x0b27d5654f63eefb)}",
                ),
                ("not recorded", "null"),
            ],
        );
    }

    #[wasm_bindgen_test]
    fn what_the_picture_drawn_from_the_line_states_holds() {
        let mut recorded = held_capture_shapes(&EVERY_RECORDER);
        let mut unrecorded = held_capture_shapes(&[]);
        assert_shapes(
            &[
                ("recorded", recorded.picture_from_line_states()),
                ("not recorded", unrecorded.picture_from_line_states()),
            ],
            &[
                (
                    "recorded",
                    "{data: Uint8Array(len 286720, fnv1a64 0x90450a1e4b4824fc), height: 224, \
                     width: 320}",
                ),
                ("not recorded", "null"),
            ],
        );
    }

    #[wasm_bindgen_test]
    fn what_the_pixel_sources_hold_for_every_plane_mask() {
        let mut emu = held_capture_shapes(&EVERY_RECORDER);
        let mut found = vec![("planes left out".to_string(), emu.get_pixel_sources(None))];
        for planes in (0..16).chain([16, 31]) {
            found.push((
                format!("planes {planes}"),
                emu.get_pixel_sources(Some(planes)),
            ));
        }
        let mut unrecorded = held_capture_shapes(&[]);
        found.push((
            "nothing recorded".to_string(),
            unrecorded.get_pixel_sources(None),
        ));
        let mut no_game = RetroEmulator::new("genesis").unwrap();
        found.push(("no game".to_string(), no_game.get_pixel_sources(None)));
        assert_shapes(
            &found,
            &[
                (
                    "planes left out",
                    "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), \
                     colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
                ),
                (
                    "planes 0",
                    "{frame: 6, fromLineStates: true, height: 224, matchesPicture: true, \
                     width: 320}",
                ),
                (
                    "planes 1",
                    "{frame: 6, fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, width: 320}",
                ),
                (
                    "planes 2",
                    "{colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, matchesPicture: true, width: 320}",
                ),
                (
                    "planes 3",
                    "{colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, width: 320}",
                ),
                (
                    "planes 4",
                    "{frame: 6, fromLineStates: true, height: 224, matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
                ),
                (
                    "planes 5",
                    "{frame: 6, fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
                ),
                (
                    "planes 6",
                    "{colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
                ),
                (
                    "planes 7",
                    "{colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
                ),
                (
                    "planes 8",
                    "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), frame: 6, \
                     fromLineStates: true, height: 224, matchesPicture: true, width: 320}",
                ),
                (
                    "planes 9",
                    "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), frame: 6, \
                     fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, width: 320}",
                ),
                (
                    "planes 10",
                    "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), \
                     colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, matchesPicture: true, width: 320}",
                ),
                (
                    "planes 11",
                    "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), \
                     colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, width: 320}",
                ),
                (
                    "planes 12",
                    "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), frame: 6, \
                     fromLineStates: true, height: 224, matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
                ),
                (
                    "planes 13",
                    "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), frame: 6, \
                     fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
                ),
                (
                    "planes 14",
                    "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), \
                     colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
                ),
                (
                    "planes 15",
                    "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), \
                     colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
                ),
                (
                    "planes 16",
                    "{frame: 6, fromLineStates: true, height: 224, matchesPicture: true, \
                     width: 320}",
                ),
                (
                    "planes 31",
                    "{cell: Uint16Array(len 71680, fnv1a64 0x3672315cebf45476), \
                     colour: Uint16Array(len 71680, fnv1a64 0x0927060996b8ea60), frame: 6, \
                     fromLineStates: true, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x4f47f23a1f1bd0e5), matchesPicture: true, \
                     tile: Uint32Array(len 71680, fnv1a64 0x16545b43d85629c6), width: 320}",
                ),
                (
                    "nothing recorded",
                    "{cell: Uint16Array(len 71680, fnv1a64 0x1534eb7b2a7cb9e3), \
                     colour: Uint16Array(len 71680, fnv1a64 0xbe0328133f1361c0), frame: 6, \
                     fromLineStates: false, height: 224, layer: Uint16Array(len 71680, \
                     fnv1a64 0x6093cce1db1c529b), matchesPicture: false, \
                     tile: Uint32Array(len 71680, fnv1a64 0xbc22a43e29ecf66a), width: 320}",
                ),
                ("no game", "null"),
            ],
        );
    }

    #[wasm_bindgen_test]
    fn what_the_line_sprites_hold() {
        let mut recorded = held_capture_shapes(&EVERY_RECORDER);
        let mut unrecorded = held_capture_shapes(&[]);
        let mut no_game = RetroEmulator::new("genesis").unwrap();
        assert_shapes(
            &[
                ("recorded", recorded.get_line_sprites()),
                ("nothing recorded", unrecorded.get_line_sprites()),
                ("no game", no_game.get_line_sprites()),
            ],
            &[
                (
                    "recorded",
                    "{drawnWidth: Uint8Array(len 488, fnv1a64 0x8a6c997556324945), \
                     fates: Uint8Array(len 488, fnv1a64 0x170fe62d918f96c5), frame: 6, \
                     fromLineStates: true, lines: Uint16Array(len 896, \
                     fnv1a64 0x4bd753302fe7e706), slots: Uint8Array(len 488, \
                     fnv1a64 0xe9ff911bdf30e495), width: Uint8Array(len 488, \
                     fnv1a64 0x2129b8dd2dc27785), x: Int16Array(len 488, \
                     fnv1a64 0x6f77b8bbd663dea5)}",
                ),
                (
                    "nothing recorded",
                    "{drawnWidth: Uint8Array(len 488, fnv1a64 0x8a6c997556324945), \
                     fates: Uint8Array(len 488, fnv1a64 0x170fe62d918f96c5), frame: 6, \
                     fromLineStates: false, lines: Uint16Array(len 896, \
                     fnv1a64 0x4bd753302fe7e706), slots: Uint8Array(len 488, \
                     fnv1a64 0xe9ff911bdf30e495), width: Uint8Array(len 488, \
                     fnv1a64 0x2129b8dd2dc27785), x: Int16Array(len 488, \
                     fnv1a64 0x6f77b8bbd663dea5)}",
                ),
                ("no game", "null"),
            ],
        );
    }

    #[wasm_bindgen_test]
    fn what_the_chosen_sprites_drawn_alone_hold() {
        let mut emu = held_capture_shapes(&EVERY_RECORDER);
        let mut some = [0u8; 30];
        for (slot, flag) in [(0, 1), (3, 7), (9, 1), (25, 255), (29, 1)] {
            some[slot] = flag;
        }
        let mut unrecorded = held_capture_shapes(&[]);
        let mut no_game = RetroEmulator::new("genesis").unwrap();
        assert_shapes(
            &[
                ("none chosen", emu.render_sprites(&[])),
                ("some chosen", emu.render_sprites(&some)),
                ("every slot chosen", emu.render_sprites(&[1; 80])),
                ("longer than the table", emu.render_sprites(&[1; 100])),
                ("nothing recorded", unrecorded.render_sprites(&[1; 80])),
                ("no game", no_game.render_sprites(&[1; 80])),
            ],
            &[
                (
                    "none chosen",
                    "{data: Uint8Array(len 286720, fnv1a64 0xe97846eb7709a325), \
                     fromLineStates: true, height: 224, width: 320}",
                ),
                (
                    "some chosen",
                    "{data: Uint8Array(len 286720, fnv1a64 0x7fd870991a18c139), \
                     fromLineStates: true, height: 224, width: 320}",
                ),
                (
                    "every slot chosen",
                    "{data: Uint8Array(len 286720, fnv1a64 0xa2af1da2713f146f), \
                     fromLineStates: true, height: 224, width: 320}",
                ),
                (
                    "longer than the table",
                    "{data: Uint8Array(len 286720, fnv1a64 0xa2af1da2713f146f), \
                     fromLineStates: true, height: 224, width: 320}",
                ),
                (
                    "nothing recorded",
                    "{data: Uint8Array(len 286720, fnv1a64 0x295ae25c4132c021), \
                     fromLineStates: false, height: 224, width: 320}",
                ),
                ("no game", "null"),
            ],
        );
    }

    #[wasm_bindgen_test]
    fn what_the_video_memory_sources_hold() {
        let recorded = held_capture_shapes(&EVERY_RECORDER);
        let unrecorded = held_capture_shapes(&[]);
        assert_shapes(
            &[
                ("recorded", recorded.get_video_memory_sources()),
                ("not recorded", unrecorded.get_video_memory_sources()),
            ],
            &[
                (
                    "recorded",
                    "{frames: Uint32Array(len 65536, fnv1a64 0x79b47c3c923896a5), \
                     origins: Uint32Array(len 65536, fnv1a64 0xf16512049df41557)}",
                ),
                ("not recorded", "null"),
            ],
        );
    }

    #[wasm_bindgen_test]
    fn what_the_resolved_sources_hold() {
        let emu = held_capture_shapes(&EVERY_RECORDER);
        let no_game = RetroEmulator::new("genesis").unwrap();
        let addresses = [
            0x0000_1000,
            0x0000_1235,
            0x0007_FFFF,
            0x0008_0000,
            0x0020_0001,
            0x003F_FFFF,
            0x0040_0000,
            0x00A0_0000,
            0x00A1_3000,
            0x00C0_0000,
            0x00E0_0000,
            0x00FF_0000,
            0x00FF_FFFF,
            0x1200_1234,
            0xFFFF_0000,
        ];
        assert_shapes(
            &[
                ("a game", emu.resolve_sources(&addresses).into()),
                ("no address", emu.resolve_sources(&[]).into()),
                ("no game", no_game.resolve_sources(&addresses).into()),
            ],
            &[
                ("a game", "Int32Array(len 15, fnv1a64 0xb4c0be8f60bbbc95)"),
                (
                    "no address",
                    "Int32Array(len 0, fnv1a64 0xcbf29ce484222325)",
                ),
                ("no game", "Int32Array(len 15, fnv1a64 0xada63fd6c9a763c9)"),
            ],
        );
    }

    #[wasm_bindgen_test]
    fn what_the_frame_writes_hold() {
        let mut emu = RetroEmulator::new("genesis").unwrap();
        emu.load_rom(&capture_shapes_rom()).unwrap();
        emu.set_execution_mode("Running".to_string());
        emu.set_capture_recording(options(&EVERY_RECORDER));
        emu.run_frame();
        let first_frame = emu.take_frame_writes();
        for _ in 0..4 {
            emu.run_frame();
        }
        let _ = emu.take_frame_writes();
        emu.run_to_capture_boundary();
        let to_the_boundary = emu.take_frame_writes();
        let taken_again = emu.take_frame_writes();
        emu.run_to_capture_boundary();
        let boundary_to_boundary = emu.take_frame_writes();
        let mut unrecorded = held_capture_shapes(&[]);
        assert_shapes(
            &[
                ("the first frame", first_frame),
                ("to the boundary", to_the_boundary),
                ("taken again", taken_again),
                ("from one boundary to the next", boundary_to_boundary),
                ("not recorded", unrecorded.take_frame_writes()),
            ],
            &[
                (
                    "the first frame",
                    "{dropped: 0, records: Uint32Array(len 48, fnv1a64 0x3e2be344a8da7c51), \
                     wordsPerRecord: 8}",
                ),
                (
                    "to the boundary",
                    "{dropped: 0, records: Uint32Array(len 16, fnv1a64 0x3a4a1295ad40bb60), \
                     wordsPerRecord: 8}",
                ),
                (
                    "taken again",
                    "{dropped: 0, records: Uint32Array(len 0, fnv1a64 0xcbf29ce484222325), \
                     wordsPerRecord: 8}",
                ),
                (
                    "from one boundary to the next",
                    "{dropped: 0, records: Uint32Array(len 48, fnv1a64 0xb5d5d9292f29c7e0), \
                     wordsPerRecord: 8}",
                ),
                ("not recorded", "null"),
            ],
        );
    }

    #[wasm_bindgen_test]
    fn when_the_core_is_at_a_capture_boundary() {
        let mut emu = RetroEmulator::new("genesis").unwrap();
        let mut found = vec![("no game", emu.at_capture_boundary().into())];
        emu.load_rom(&capture_shapes_rom()).unwrap();
        emu.set_execution_mode("Running".to_string());
        emu.run_frame();
        found.push(("after a frame", emu.at_capture_boundary().into()));
        emu.run_to_capture_boundary();
        found.push(("held there", emu.at_capture_boundary().into()));
        let _ = emu.render_layer(0);
        let _ = emu.get_pixel_sources(None);
        found.push(("after reading", emu.at_capture_boundary().into()));
        emu.run_to_capture_boundary();
        found.push(("at the next boundary", emu.at_capture_boundary().into()));
        emu.run_frame();
        found.push(("after the frame call", emu.at_capture_boundary().into()));
        emu.run_to_capture_boundary();
        let _ = emu.save_state();
        found.push(("after a save", emu.at_capture_boundary().into()));
        assert_shapes(
            &found,
            &[
                ("no game", "false"),
                ("after a frame", "false"),
                ("held there", "true"),
                ("after reading", "true"),
                ("at the next boundary", "true"),
                ("after the frame call", "false"),
                ("after a save", "false"),
            ],
        );
    }

    #[wasm_bindgen_test]
    fn what_a_repaint_draws() {
        // `repaintFrame` returns nothing to JavaScript.
        let repaint: fn(&mut RetroEmulator) = RetroEmulator::repaint_frame;
        let mut recorded = held_capture_shapes(&EVERY_RECORDER);
        recorded.toggle_layer(0, false);
        repaint(&mut recorded);
        let mut unrecorded = held_capture_shapes(&[]);
        unrecorded.toggle_layer(1, false);
        repaint(&mut unrecorded);
        assert_shapes(
            &[
                ("plane A hidden", recorded.get_framebuffer_data().into()),
                (
                    "plane B hidden, nothing recorded",
                    unrecorded.get_framebuffer_data().into(),
                ),
            ],
            &[
                (
                    "plane A hidden",
                    "Uint8ClampedArray(len 286720, fnv1a64 0x478376a41bb9c42b)",
                ),
                (
                    "plane B hidden, nothing recorded",
                    "Uint8ClampedArray(len 286720, fnv1a64 0xf1074741c02b84f3)",
                ),
            ],
        );
    }
}
