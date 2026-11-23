//! WebAssembly bindings for the SNES1 emulator.
//!
//! Exposes a [`RetroEmulator`] wrapper whose method names and signatures
//! mirror `sg1-wasm`'s `RetroEmulator` for every generic API (lifecycle,
//! framebuffer, audio, input, savestate, debugger, tracing, coverage,
//! disassembly, memory). A browser host (its renderer, its audio and
//! input code, and the generic debugger panels) can therefore drive the
//! SNES core through the same calls it already makes against Genesis.
//! SNES-only debug surfaces (the PPU's mode, settings and window masks,
//! Mode 7, the APU mailbox, OAM and ARAM) ship under their own `js_name`s.
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

use retro_core::video::VideoMemoryId;
use retro_core::*;
use retro_wasm::js::{int16_array, plain_object, set_property, to_js, uint16_array, uint8_array};
use snes1_core::sound::Id666;
use snes1_core::{CoprocessorMode, SnesEmulator, SnesSnapshotConsole};
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

/// The ID666 tag a host gives (`{ title, game, dumper, comment, date,
/// seconds, fade_ms, artist }`, each optional), or none for `undefined`.
fn spc_tag(tag: JsValue) -> Result<Id666, JsValue> {
    match tag.is_undefined() || tag.is_null() {
        true => Ok(Id666::default()),
        false => retro_wasm::audio::from_js(tag),
    }
}

/// The SPC, the APU's state as an SPC player runs it; the BRR samples.
#[wasm_bindgen]
impl RetroEmulator {
    /// The sample directory the S-DSP's registers name, 256 entries: `[{
    /// source, start, loop_start, voices }]`, `voices` a bit a voice whose
    /// source number names it.
    #[wasm_bindgen(js_name = getSampleDirectory)]
    pub fn get_sample_directory(&self) -> JsValue {
        to_js(&self.snes.sample_directory())
    }

    /// The BRR sample at `start` of ARAM decoded as a voice decodes it,
    /// `loopStart` its loop address: `{ pcm, blocks, loop_sample, ends }`,
    /// `pcm` an `Int16Array` of its values in the DSP's form (each doubled),
    /// `loop_sample` the first value of its loop block (`undefined` when
    /// the loop names none of its blocks), `ends` `"Loops"`, `"Stops"` or
    /// `"Unended"`.
    #[wasm_bindgen(js_name = decodeBrr)]
    pub fn decode_brr(&self, start: u16, loop_start: u16) -> JsValue {
        let sample = self.snes.decode_brr(start, loop_start);
        plain_object(&[
            ("pcm", int16_array(&sample.pcm)),
            ("blocks", JsValue::from(sample.blocks as u32)),
            (
                "loop_sample",
                sample
                    .loop_sample
                    .map_or(JsValue::UNDEFINED, |at| JsValue::from(at as u32)),
            ),
            ("ends", to_js(&sample.ends)),
        ])
    }

    /// The files of sample `source` (0-255) of the directory the S-DSP's
    /// registers name, read from ARAM as it stands, keyed on at `pitches`
    /// by `voices` (each once, in the order they came): `{ brr,
    /// brr_looped, wav, info }`, `brr` its blocks as they sit in ARAM,
    /// `brr_looped` the same after the 2-byte loop header AddmusicK and
    /// BRRtools read, `wav` its values as a 32 kHz WAV with its loop as a
    /// voice plays it, pass after pass until it repeats, the passes it
    /// repeats in a `smpl` chunk (each a `Uint8Array`), and `info` the text
    /// of its JSON description; the bytes `brr-dump` writes of the sample.
    #[wasm_bindgen(js_name = encodeBrrSample)]
    pub fn encode_brr_sample(&self, source: u8, pitches: Vec<u32>, voices: Vec<u8>) -> JsValue {
        let files = self.snes.sample_files(source, pitches, voices);
        plain_object(&[
            ("brr", uint8_array(&files.brr)),
            ("brr_looped", uint8_array(&files.brr_looped)),
            ("wav", uint8_array(&files.wav)),
            ("info", JsValue::from_str(&files.info.to_json())),
        ])
    }

    /// The APU as an SPC file (version 0.30) a player plays: its bytes,
    /// tagged with `tag` (`{ title, game, dumper, comment, date, seconds,
    /// fade_ms, artist }`, each optional; `undefined` for no tag). Between
    /// two frames it is the APU at the frame's end.
    #[wasm_bindgen(js_name = exportSpc)]
    pub fn export_spc(&self, tag: JsValue) -> Result<Vec<u8>, JsValue> {
        Ok(self.snes.export_spc(&spc_tag(tag)?))
    }

    /// Keep the APU as it stands after the first SPC700 instruction that
    /// keys a voice on, from now on; what was kept and not taken is
    /// dropped.
    #[wasm_bindgen(js_name = armSpcAtKeyOn)]
    pub fn arm_spc_at_key_on(&mut self) {
        self.snes.arm_spc_at_key_on();
    }

    /// The SPC file of the APU the key-on kept, tagged as `exportSpc`'s,
    /// once; `undefined` while the key-on has not come or nothing was
    /// armed.
    #[wasm_bindgen(js_name = takeKeyOnSpc)]
    pub fn take_key_on_spc(&mut self, tag: JsValue) -> Result<Option<Vec<u8>>, JsValue> {
        let tag = spc_tag(tag)?;
        Ok(self.snes.take_key_on_spc(&tag))
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
