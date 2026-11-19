//! WebAssembly bindings for the SG1 emulator.
//!
//! This crate provides a generic WASM interface that works with any
//! `EmulatorCore` implementation.

use retro_core::debug::trace_to_text;
use retro_core::*;
use retro_wasm::js::{set_property, to_js, uint16_array};
use sg1_core::debugger::{TraceEntryBinaryZ80, SOUND_CPU};
use sg1_core::system::GenesisSnapshotConsole;
use sg1_core::GenesisEmulator;
use wasm_bindgen::prelude::*;

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
            _ => Err(retro_wasm::machine::unknown_system(
                system,
                &["genesis", "megadrive", "md"],
            )),
        }
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

    // ===== Cartridge manifest =====

    /// Cartridge manifest view derived from the installed map plan:
    /// `{ memoryMap, romRegions, sramRegions, mapperState }`. `mapperState`
    /// is the live `{ mapper_type, registers, banks }` from the cartridge
    /// debugger (`mapper_type: "None"` for a plain cartridge).
    #[wasm_bindgen(js_name = getCartridgeManifest)]
    pub fn get_cartridge_manifest(&mut self) -> JsValue {
        let regions = self.genesis.cartridge_debugger().map_regions();
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
            "mapperState": retro_wasm::cartridge::mapper_state_value(
                mapper_state,
                genesis_mapper_type_label,
            ),
        });
        to_js(&manifest)
    }

    // ===== Debugger Methods =====

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
        to_js(&self.genesis.get_ym2612_lfo_state())
    }

    /// Get YM2612 DAC state.
    #[wasm_bindgen(js_name = getYm2612DacState)]
    pub fn get_ym2612_dac_state(&self) -> JsValue {
        to_js(&self.genesis.get_ym2612_dac_state())
    }

    /// Get YM2612 Timer A state.
    #[wasm_bindgen(js_name = getYm2612TimerAState)]
    pub fn get_ym2612_timer_a_state(&self) -> JsValue {
        to_js(&self.genesis.get_ym2612_timer_a_state())
    }

    /// Get YM2612 Timer B state.
    #[wasm_bindgen(js_name = getYm2612TimerBState)]
    pub fn get_ym2612_timer_b_state(&self) -> JsValue {
        to_js(&self.genesis.get_ym2612_timer_b_state())
    }
}

/// Writes the Mega Drive's part of a frame snapshot: `vsram`, a
/// `Uint16Array` of the vertical scroll RAM's 64 words, as `getVsram` gives
/// them.
fn snapshot_console(console: &GenesisSnapshotConsole, snapshot: &js_sys::Object) {
    set_property(snapshot, "vsram", &uint16_array(&console.vsram));
}

retro_wasm::module_start!();
retro_wasm::machine_bindings!(RetroEmulator, genesis);
retro_wasm::display_bindings!(RetroEmulator, genesis);
retro_wasm::audio_bindings!(RetroEmulator, genesis);
retro_wasm::input_bindings!(RetroEmulator, genesis);
retro_wasm::debugger_bindings!(RetroEmulator, genesis);
retro_wasm::video_bindings!(RetroEmulator, genesis);
retro_wasm::cartridge_bindings!(
    RetroEmulator,
    genesis,
    mapper_label = genesis_mapper_type_label
);
retro_wasm::capture_bindings!(RetroEmulator, genesis, console = snapshot_console);

#[cfg(test)]
mod tests;
