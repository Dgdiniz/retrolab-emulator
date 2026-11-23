//! WebAssembly bindings for the SG1 emulator.
//!
//! This crate provides a generic WASM interface that works with any
//! `EmulatorCore` implementation.

use retro_core::video::VideoMemoryId;
use retro_core::*;
use retro_wasm::js::{set_property, to_js, uint16_array};
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

    // ===== VDP Debugger Methods =====

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
    /// paused one after `repaintFrame`. `writeVideoMemory("vram", ...)`
    /// writes the same way, from the same byte.
    #[wasm_bindgen(js_name = writeVram)]
    pub fn write_vram(&mut self, byte_address: u32, bytes: &[u8]) {
        self.genesis
            .video_debugger()
            .write_video_memory(VideoMemoryId::Vram, byte_address, bytes);
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

/// The Mega Drive's sound files from a sound log a host took
/// (`takeSoundLog`).
#[wasm_bindgen]
impl RetroEmulator {
    /// A VGM 1.71 file of the log whose `records` (on a clock of `clockHz`)
    /// and `chips` `takeSoundLog` gave, written with `options` (`{ tag: {
    /// track, game, system, author, date, ripper, notes }, dac: "Raw" or
    /// "Bank", loop_entry, frame_rate }`, each field optional, `undefined`
    /// for none): its bytes, or an error naming why it cannot be written.
    #[wasm_bindgen(js_name = encodeVgm)]
    pub fn encode_vgm(
        records: &[u8],
        clock_hz: f64,
        chips: JsValue,
        options: JsValue,
    ) -> Result<Vec<u8>, JsValue> {
        let log = retro_wasm::audio::sound_log_of(records, clock_hz)?;
        let chips: Vec<retro_core::audio::SoundChipKind> = retro_wasm::audio::from_js(chips)?;
        let options = match options.is_undefined() || options.is_null() {
            true => retro_core::audio::VgmOptions::default(),
            false => retro_wasm::audio::from_js(options)?,
        };
        retro_core::audio::encode_vgm(&log, &chips, &options)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// A GYM file of the log whose `records` and `chips` `takeSoundLog`
    /// gave, with a GYMX header when `header` is given (`{ song, game,
    /// publisher, emulator, dumper, comment, loop_frame }`, each optional):
    /// its bytes, or an error naming why it cannot be written.
    #[wasm_bindgen(js_name = encodeGym)]
    pub fn encode_gym(records: &[u8], chips: JsValue, header: JsValue) -> Result<Vec<u8>, JsValue> {
        let log = retro_wasm::audio::sound_log_of(records, 0.0)?;
        let chips: Vec<retro_core::audio::SoundChipKind> = retro_wasm::audio::from_js(chips)?;
        let header: Option<sg1_core::sound::GymHeader> =
            match header.is_undefined() || header.is_null() {
                true => None,
                false => Some(retro_wasm::audio::from_js(header)?),
            };
        sg1_core::sound::encode_gym(&log, &chips, header.as_ref())
            .map_err(|e| JsValue::from_str(&e))
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
