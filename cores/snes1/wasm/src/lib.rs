//! WebAssembly bindings for the SNES1 emulator.
//!
//! Exposes a [`RetroEmulator`] wrapper whose method names and signatures
//! mirror `sg1-wasm`'s `RetroEmulator` for every generic API (lifecycle,
//! framebuffer, audio, input, savestate, debugger, tracing, coverage,
//! disassembly, memory). A browser host (its renderer, its audio and
//! input code, and the generic debugger panels) can therefore drive the
//! SNES core through the same calls it already makes against Genesis.
//! SNES-only debug surfaces (PPU mode, Mode 7 matrix, S-DSP voices, APU
//! mailbox, viewers) ship under their own `js_name`s.
//!
//! ## Framebuffer presentation
//!
//! The core renders into a worst-case 512x240 [`Framebuffer`] with the
//! active image in a top-left sub-rectangle at the full `fb.width`
//! stride. The renderer uploads a `width x height` texture straight from
//! [`RetroEmulator::get_framebuffer_ptr`] / `get_framebuffer_len`, so it
//! needs a tightly-packed buffer where `len == width * height * 4`. We
//! pack the active rectangle into a present buffer
//! ([`retro_wasm::display::PresentBuffer`]) whenever the core produces a
//! new frame and expose that buffer (and the active dimensions) through
//! the framebuffer accessors.

use retro_core::debug::TraceEntry;
use retro_core::video::VideoMemoryId;
use retro_core::*;
use retro_wasm::js::{plain_object, set_property, to_js, uint16_array, uint8_array};
use snes1_core::{
    CoprocessorMode, SnesEmulator, SnesSnapshotConsole, TileSetSource, TraceEntryBinaryCx4,
    TraceEntryBinaryGsu, CARTRIDGE_CPU, SOUND_CPU,
};
use wasm_bindgen::prelude::*;

/// The SNES's keys of a frame snapshot (`captureFrameSnapshot`), after
/// `palette`: `oam`, a `Uint8Array` copy of OAM as `getOam` returns it, and
/// `ppuSettings`, the PPU's settings as `getPpuSettings` returns them, a
/// plain object.
fn snapshot_console(console: &SnesSnapshotConsole, snapshot: &js_sys::Object) {
    set_property(snapshot, "oam", &uint8_array(&console.oam));
    set_property(snapshot, "ppuSettings", &to_js(&console.ppu_settings));
}

/// Browser-facing wrapper around [`SnesEmulator`].
#[wasm_bindgen]
pub struct RetroEmulator {
    snes: SnesEmulator,
    /// Tightly-packed active-rectangle RGBA, refreshed each time the core
    /// produces a frame. The framebuffer accessors return this buffer so
    /// the renderer never has to deal with the core's 512-wide stride.
    present: retro_wasm::display::PresentBuffer,
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
                    present: retro_wasm::display::PresentBuffer::new(),
                };
                emu.refresh_present();
                Ok(emu)
            }
            other => Err(retro_wasm::machine::unknown_system(
                other,
                &["snes", "superfamicom", "sfc"],
            )),
        }
    }

    /// Repack the active framebuffer rectangle into the present buffer.
    fn refresh_present(&mut self) {
        self.present.refresh(
            EmulatorCore::get_framebuffer(&self.snes),
            EmulatorCore::get_display_info(&self.snes),
        );
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
        to_js(&self.snes.ppu_settings())
    }

    // ===== Debugger =====
    //
    // The generic debugger methods (breakpoints, execution mode, history,
    // call stack, focus, steps, coverage and every CPU's trace) are
    // written by `retro_wasm::debugger_bindings!`, after this block.

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
        to_js(&obj)
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

    /// The four SPC700 -> main-CPU mailbox port values (`$2140-$2143`).
    #[wasm_bindgen(js_name = getApuPortValues)]
    pub fn get_apu_port_values(&self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.bus().apu().output_ports()).unwrap()
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

    // ===== Raw debug memory views =====

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
    /// `writeVideoMemory("vram", ...)` writes the same way from a byte
    /// address: this is it at byte `2 * word_address`.
    #[wasm_bindgen(js_name = writeVram)]
    pub fn write_vram(&mut self, word_address: u32, bytes: &[u8]) {
        self.snes.video_debugger().write_video_memory(
            VideoMemoryId::Vram,
            (word_address & 0x7FFF) * 2,
            bytes,
        );
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
}

/// The older names of methods both consoles now export under one name each,
/// kept answering as they did: each is a call of its new name where the two
/// give the same value, else its own body. Each names its new name.
#[wasm_bindgen]
impl RetroEmulator {
    /// The debug profile id the frontend should resolve for the loaded
    /// cartridge, keyed off the architecture of the steppable secondary CPU
    /// (debugger slot 2): `"snes-superfx"` for the Super FX GSU, `"snes-cx4"`
    /// for the Cx4 HG51B169, `"snes-sa1"` for the SA-1's second 65816, else
    /// `"snes"` (plain cart or a non-steppable coprocessor). The core owns
    /// per-cartridge profile selection, it reflects the CPU roster built at
    /// `loadRom` from the installed chip, so the frontend never re-detects the
    /// chip. Defaults to `"snes"` before a ROM is loaded.
    ///
    /// @deprecated Use `getCpus()`: CPU 2's architecture names the profile.
    #[wasm_bindgen(js_name = getDebugProfileId)]
    pub fn get_debug_profile_id(&self) -> String {
        match self.snes.system_info().cpus.get(2).map(|c| c.architecture) {
            Some(retro_core::CpuArchitecture::SuperFx) => "snes-superfx".to_string(),
            Some(retro_core::CpuArchitecture::Hg51b) => "snes-cx4".to_string(),
            Some(retro_core::CpuArchitecture::Mos65816) => "snes-sa1".to_string(),
            _ => "snes".to_string(),
        }
    }

    /// Enable or disable SPC700 execution tracing.
    ///
    /// @deprecated Use `setCpuTracingEnabled(1, enabled)`.
    #[wasm_bindgen(js_name = setSpc700TracingEnabled)]
    pub fn set_spc700_tracing_enabled(&mut self, enabled: bool) {
        self.set_cpu_tracing_enabled(SOUND_CPU, enabled);
    }

    /// Check if SPC700 tracing is enabled.
    ///
    /// @deprecated Use `isCpuTracingEnabled(1)`.
    #[wasm_bindgen(js_name = isSpc700TracingEnabled)]
    pub fn is_spc700_tracing_enabled(&mut self) -> bool {
        self.is_cpu_tracing_enabled(SOUND_CPU)
    }

    /// Get SPC700 trace data ready for flushing (empty if none).
    ///
    /// @deprecated Use `getCpuTraceFlushData(1)`.
    #[wasm_bindgen(js_name = getSpc700TraceFlushData)]
    pub fn get_spc700_trace_flush_data(&mut self) -> Vec<u8> {
        self.get_cpu_trace_flush_data(SOUND_CPU)
    }

    /// Clear flushed SPC700 trace data.
    ///
    /// @deprecated Use `clearCpuTraceFlushed(1)`.
    #[wasm_bindgen(js_name = clearSpc700TraceFlushed)]
    pub fn clear_spc700_trace_flushed(&mut self) {
        self.clear_cpu_trace_flushed(SOUND_CPU);
    }

    /// SPC700 trace entry size in bytes.
    ///
    /// @deprecated Use `getCpuTraceEntrySize(1)`.
    #[wasm_bindgen(js_name = getSpc700TraceEntrySize)]
    pub fn get_spc700_trace_entry_size(&self) -> usize {
        self.get_cpu_trace_entry_size(SOUND_CPU)
    }

    /// Convert a binary SPC700 trace chunk to text (one line/instruction).
    ///
    /// @deprecated Use `convertCpuTraceChunkToText(1, data)`.
    #[wasm_bindgen(js_name = convertSpc700TraceChunkToText)]
    pub fn convert_spc700_trace_chunk_to_text(&mut self, data: &[u8]) -> String {
        self.convert_cpu_trace_chunk_to_text(SOUND_CPU, data)
    }

    /// Enable or disable Super FX GSU execution tracing. On a cartridge
    /// whose CPU is not one that traces (a plain one, an SA-1), the switch
    /// stays off and the frames stay on the fast loop.
    ///
    /// @deprecated Use `setCpuTracingEnabled(2, enabled)`.
    #[wasm_bindgen(js_name = setSuperfxTracingEnabled)]
    pub fn set_superfx_tracing_enabled(&mut self, enabled: bool) {
        self.set_cpu_tracing_enabled(CARTRIDGE_CPU, enabled);
    }

    /// Check if Super FX GSU tracing is enabled.
    ///
    /// @deprecated Use `isCpuTracingEnabled(2)`.
    #[wasm_bindgen(js_name = isSuperfxTracingEnabled)]
    pub fn is_superfx_tracing_enabled(&mut self) -> bool {
        self.is_cpu_tracing_enabled(CARTRIDGE_CPU)
    }

    /// Get Super FX GSU trace data ready for flushing (empty if none).
    ///
    /// @deprecated Use `getCpuTraceFlushData(2)`.
    #[wasm_bindgen(js_name = getSuperfxTraceFlushData)]
    pub fn get_superfx_trace_flush_data(&mut self) -> Vec<u8> {
        self.get_cpu_trace_flush_data(CARTRIDGE_CPU)
    }

    /// Clear flushed Super FX GSU trace data.
    ///
    /// @deprecated Use `clearCpuTraceFlushed(2)`.
    #[wasm_bindgen(js_name = clearSuperfxTraceFlushed)]
    pub fn clear_superfx_trace_flushed(&mut self) {
        self.clear_cpu_trace_flushed(CARTRIDGE_CPU);
    }

    /// Super FX GSU trace entry size in bytes.
    ///
    /// @deprecated Use `getCpuTraceEntrySize(2)` on a cartridge whose CPU 2 is
    /// the Super FX.
    #[wasm_bindgen(js_name = getSuperfxTraceEntrySize)]
    pub fn get_superfx_trace_entry_size(&self) -> usize {
        <TraceEntryBinaryGsu as TraceEntry>::SIZE
    }

    /// Convert a binary Super FX GSU trace chunk to text (one line/instruction).
    /// Absent from a build without the chip.
    ///
    /// @deprecated Use `convertCpuTraceChunkToText(2, data)` on a cartridge
    /// whose CPU 2 is the Super FX.
    #[cfg(feature = "chip-superfx")]
    #[wasm_bindgen(js_name = convertSuperfxTraceChunkToText)]
    pub fn convert_superfx_trace_chunk_to_text(&self, data: &[u8]) -> String {
        retro_core::debug::trace_to_text::<TraceEntryBinaryGsu, ()>(data, &mut ())
    }

    /// Enable or disable Cx4 HG51B169 execution tracing. On a cartridge
    /// whose CPU is not one that traces (a plain one, an SA-1), the switch
    /// stays off and the frames stay on the fast loop.
    ///
    /// @deprecated Use `setCpuTracingEnabled(2, enabled)`.
    #[wasm_bindgen(js_name = setHg51bTracingEnabled)]
    pub fn set_hg51b_tracing_enabled(&mut self, enabled: bool) {
        self.set_cpu_tracing_enabled(CARTRIDGE_CPU, enabled);
    }

    /// Check if Cx4 HG51B169 tracing is enabled.
    ///
    /// @deprecated Use `isCpuTracingEnabled(2)`.
    #[wasm_bindgen(js_name = isHg51bTracingEnabled)]
    pub fn is_hg51b_tracing_enabled(&mut self) -> bool {
        self.is_cpu_tracing_enabled(CARTRIDGE_CPU)
    }

    /// Get Cx4 HG51B169 trace data ready for flushing (empty if none).
    ///
    /// @deprecated Use `getCpuTraceFlushData(2)`.
    #[wasm_bindgen(js_name = getHg51bTraceFlushData)]
    pub fn get_hg51b_trace_flush_data(&mut self) -> Vec<u8> {
        self.get_cpu_trace_flush_data(CARTRIDGE_CPU)
    }

    /// Clear flushed Cx4 HG51B169 trace data.
    ///
    /// @deprecated Use `clearCpuTraceFlushed(2)`.
    #[wasm_bindgen(js_name = clearHg51bTraceFlushed)]
    pub fn clear_hg51b_trace_flushed(&mut self) {
        self.clear_cpu_trace_flushed(CARTRIDGE_CPU);
    }

    /// Cx4 HG51B169 trace entry size in bytes.
    ///
    /// @deprecated Use `getCpuTraceEntrySize(2)` on a cartridge whose CPU 2 is
    /// the Cx4.
    #[wasm_bindgen(js_name = getHg51bTraceEntrySize)]
    pub fn get_hg51b_trace_entry_size(&self) -> usize {
        <TraceEntryBinaryCx4 as TraceEntry>::SIZE
    }

    /// Convert a binary Cx4 HG51B169 trace chunk to text (one line/instruction).
    /// Absent from a build without the chip.
    ///
    /// @deprecated Use `convertCpuTraceChunkToText(2, data)` on a cartridge
    /// whose CPU 2 is the Cx4.
    #[cfg(feature = "chip-cx4")]
    #[wasm_bindgen(js_name = convertHg51bTraceChunkToText)]
    pub fn convert_hg51b_trace_chunk_to_text(&self, data: &[u8]) -> String {
        retro_core::debug::trace_to_text::<TraceEntryBinaryCx4, ()>(data, &mut ())
    }

    /// State of all 8 S-DSP voices (`ChannelState[]`, `null` per voice if
    /// unavailable).
    ///
    /// @deprecated Use `getAudioChipChannels(0)`.
    #[wasm_bindgen(js_name = getDspVoiceStates)]
    pub fn get_dsp_voice_states(&mut self) -> JsValue {
        self.get_audio_chip_channels(0)
    }

    /// Mute/unmute one S-DSP voice (0-7) in the debugger without altering
    /// the game's key-on state.
    ///
    /// @deprecated Use `muteAudioChipChannel(0, channel, muted)`.
    #[wasm_bindgen(js_name = muteDspChannel)]
    pub fn mute_dsp_channel(&mut self, channel: u32, muted: bool) {
        self.mute_audio_chip_channel(0, channel, muted);
    }

    /// `samples` of one S-DSP voice's approximated waveform (the mixed
    /// output scaled by that voice's instantaneous amplitude; silence
    /// when the voice is muted or keyed off).
    ///
    /// @deprecated Use `getAudioChipWaveform(0, channel, samples)`.
    #[wasm_bindgen(js_name = getDspWaveform)]
    pub fn get_dsp_waveform(&mut self, channel: u32, samples: u32) -> Vec<f32> {
        self.get_audio_chip_waveform(0, channel, samples)
    }

    /// `samples` of the mixed S-DSP output as mono f32 in [-1, 1] for the
    /// waveform / spectrum views. Peeks the audio ring without draining
    /// it, so the host's normal audio pull is unaffected.
    ///
    /// @deprecated Use `getAudioOutputWaveform(samples)`.
    #[wasm_bindgen(js_name = getDspMixedWaveform)]
    pub fn get_dsp_mixed_waveform(&mut self, samples: u32) -> Vec<f32> {
        self.get_audio_output_waveform(samples)
    }

    /// The 256-entry CGRAM as raw BGR15 words (one per colour). A palette
    /// viewer decodes these into RGBA swatches.
    ///
    /// @deprecated Use `getPaletteMemory()`.
    #[wasm_bindgen(js_name = getCgram)]
    pub fn get_cgram(&mut self) -> Vec<u16> {
        self.get_palette_memory()
    }

    /// Write CGRAM for debugging, to edit a palette: `colours` from colour
    /// `colour_index` (0-255) on, BGR555 words as `getCgram` returns them
    /// (bit 15 is dropped, as the port drops it). Straight into memory,
    /// through no port. The index wraps at 256, a buffer longer than CGRAM
    /// is cut to its 256 colours, and an empty one writes nothing. A
    /// running game shows the change on its next frame, a paused one after
    /// `repaintFrame`.
    ///
    /// @deprecated Use `writePaletteMemory(colour_index, colours)`.
    #[wasm_bindgen(js_name = writeCgram)]
    pub fn write_cgram(&mut self, colour_index: u32, colours: &[u16]) {
        self.write_palette_memory(colour_index, colours);
    }

    /// PPU register snapshot (`VideoRegister[]`: BGMODE, TM/TS, Mode 7
    /// matrix + centre, BG1 scroll) for the PPU state panel.
    ///
    /// @deprecated Use `getVideoRegisters()`.
    #[wasm_bindgen(js_name = getPpuRegisters)]
    pub fn get_ppu_registers(&mut self) -> JsValue {
        self.get_video_registers()
    }

    /// 256-colour CGRAM rendered as a 256x256 RGBA8888 swatch grid.
    ///
    /// @deprecated Use `getPaletteColours()`, each colour's RGBA once.
    #[wasm_bindgen(js_name = exportPaletteImage)]
    pub fn export_palette_image(&self) -> Vec<u8> {
        self.snes.export_palette_image()
    }

    /// A 16x16 tile sheet decoded from a VRAM character base.
    /// `source`: 0 = background base, 1 = OBJ base. Returns
    /// `{ width, height, rgba }`.
    ///
    /// @deprecated Use `getTileColours`.
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
    ///
    /// @deprecated Use `getLayerLayout(bg)` with `getTileColours`.
    #[wasm_bindgen(js_name = exportTilemapImage)]
    pub fn export_tilemap_image(&self, bg: usize) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.export_tilemap_image(bg)).unwrap()
    }

    /// All 128 OAM sprites flattened into `SpriteDescriptor[]`.
    ///
    /// @deprecated Use `getSpriteTable()`.
    #[wasm_bindgen(js_name = exportOamSprites)]
    pub fn export_oam_sprites(&self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.export_oam_sprites()).unwrap()
    }

    /// Per-background addressing + format snapshot (`BgLayout`) for the
    /// host's raw-layer (full-map) assembler, in the core's own field
    /// names: `{ bg, bg_mode, tilemap_base_byte, char_base_byte,
    /// screen_size, width_cells, height_cells, bpp, tile_size16,
    /// palette_base, scroll_x, scroll_y }`. `bpp` is `0` when the BG does
    /// not draw under the current mode (host falls back to the viewport).
    /// `bg` is clamped to `0..=3`.
    ///
    /// @deprecated Use `getLayerLayout(bg)`, field for field.
    #[wasm_bindgen(js_name = getBgLayout)]
    pub fn get_bg_layout(&self, bg: usize) -> JsValue {
        serde_wasm_bindgen::to_value(&self.snes.bg_layout(bg)).unwrap()
    }
}

retro_wasm::module_start!();
retro_wasm::machine_bindings!(
    RetroEmulator,
    snes,
    after_frame = refresh_present,
    retry_detected_mapping = true
);
retro_wasm::display_bindings!(RetroEmulator, snes, present = present);
retro_wasm::audio_bindings!(RetroEmulator, snes);
retro_wasm::input_bindings!(RetroEmulator, snes);
retro_wasm::debugger_bindings!(RetroEmulator, snes, after_step = refresh_present);
retro_wasm::video_bindings!(RetroEmulator, snes);
retro_wasm::cartridge_bindings!(RetroEmulator, snes);
retro_wasm::capture_bindings!(
    RetroEmulator,
    snes,
    console = snapshot_console,
    after_frame = refresh_present
);

#[cfg(test)]
mod tests;
