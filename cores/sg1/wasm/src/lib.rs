//! WebAssembly bindings for the SG1 emulator.
//!
//! This crate provides a generic WASM interface that works with any
//! `EmulatorCore` implementation.

use retro_core::capture::CaptureCore;
use retro_core::debug::trace_to_text;
use retro_core::input::{gamepad_map_from_text_keys, gamepad_map_with_text_keys};
use retro_core::*;
use retro_wasm::js::{map_as_object, set_property, uint16_array};
use sg1_core::cartridge::parse_manifest;
use sg1_core::debugger::{TraceEntryBinaryZ80, SOUND_CPU};
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

    /// Run one frame on the tier the debugger's state picks, the core's own
    /// choice: the play tier with no debug feature active (highest
    /// performance), the light debug tier with breakpoints or traces while
    /// running, and the paused or stepping tier otherwise.
    #[wasm_bindgen(js_name = runFrame)]
    pub fn run_frame(&mut self) {
        self.genesis.run_frame();
    }

    /// Run to the start of vertical blanking and stop there: the capture
    /// boundary, as `runToCaptureBoundary`.
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

    // ===== Multi-CPU Debug Methods =====

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

    /// Disassemble instructions from a specific CPU at the given address.
    #[wasm_bindgen(js_name = disassembleCpu)]
    pub fn disassemble_cpu(&mut self, cpu_id: usize, addr: u32, count: u32) -> JsValue {
        serde_wasm_bindgen::to_value(&self.genesis.disassemble_cpu(cpu_id, addr, count)).unwrap()
    }

    /// Enable or disable the Z80's trace (CPU 1).
    #[wasm_bindgen(js_name = setZ80TracingEnabled)]
    pub fn set_z80_tracing_enabled(&mut self, enabled: bool) {
        DebuggerCore::set_tracing_enabled(self.genesis.debugger(), SOUND_CPU, enabled);
    }

    /// Whether the Z80 is traced.
    #[wasm_bindgen(js_name = isZ80TracingEnabled)]
    pub fn is_z80_tracing_enabled(&self) -> bool {
        DebuggerCore::is_tracing_enabled(self.genesis.debugger_ref(), SOUND_CPU)
    }

    /// Whether the Z80's trace ring asks to be flushed.
    #[wasm_bindgen(js_name = needsZ80TraceFlush)]
    pub fn needs_z80_trace_flush(&self) -> bool {
        DebuggerCore::needs_trace_flush(self.genesis.debugger_ref(), SOUND_CPU)
    }

    /// The Z80's trace entries waiting to be flushed, empty when none.
    #[wasm_bindgen(js_name = getZ80TraceFlushData)]
    pub fn get_z80_trace_flush_data(&self) -> Vec<u8> {
        DebuggerCore::get_trace_flush_data(self.genesis.debugger_ref(), SOUND_CPU)
            .unwrap_or_default()
    }

    /// Empty the Z80's trace ring once its entries are flushed.
    #[wasm_bindgen(js_name = clearZ80TraceFlushed)]
    pub fn clear_z80_trace_flushed(&mut self) {
        DebuggerCore::clear_trace_flushed(self.genesis.debugger(), SOUND_CPU);
    }

    /// The size of a Z80 trace entry in bytes.
    #[wasm_bindgen(js_name = getZ80TraceEntrySize)]
    pub fn get_z80_trace_entry_size(&self) -> usize {
        DebuggerCore::trace_entry_size(self.genesis.debugger_ref(), SOUND_CPU).unwrap_or(0)
    }

    /// The text of a chunk of Z80 trace entries, MAME's format, a line per
    /// whole entry: `PC:XXXX AF:XXXX BC:XXXX DE:XXXX HL:XXXX IX:XXXX
    /// IY:XXXX SP:XXXX I:XX R:XX IM:X IFF1:X IFF2:X HALT:X | mnemonic`, the
    /// instruction disassembled from the bytes each entry holds, so the
    /// text is right even after Z80 RAM has changed.
    #[wasm_bindgen(js_name = convertZ80TraceChunkToText)]
    pub fn convert_z80_trace_chunk_to_text(&self, data: &[u8]) -> String {
        trace_to_text::<TraceEntryBinaryZ80, ()>(data, &mut ())
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

    /// The sprites the held frame drew, as `SpriteOrderEntry[]` in
    /// back-to-front order (last entry draws on top), so a composite
    /// exporter blits members exactly as the VDP drew them: only the sprites
    /// of which some line drew a visible column, each once, with the
    /// priority bit of the line that drew it first; an empty array when the
    /// frame drew no sprite. Each line is read from the line table where it
    /// holds the line, else from the state as it stands.
    #[wasm_bindgen(js_name = resolvedSpriteRenderOrder)]
    pub fn resolved_sprite_render_order(&mut self) -> JsValue {
        use retro_core::video::VideoDebugger;
        let order = self.genesis.vdp_mut().resolved_sprite_render_order();
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
retro_wasm::debugger_bindings!(RetroEmulator, genesis);
retro_wasm::cartridge_bindings!(RetroEmulator, genesis);

#[cfg(test)]
mod tests;
