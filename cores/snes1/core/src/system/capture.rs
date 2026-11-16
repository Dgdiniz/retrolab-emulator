//! The capture contract (`retro_core::capture::CaptureCore`) on this
//! console, and the picture and snapshot helpers it reads.

use retro_core::capture::{
    CaptureCore, CaptureRecording, FrameSnapshot, FrameWrites, LinePalettes, LineSprites,
    PixelSources, RenderedLayer, VideoMemorySources,
};
use retro_core::cartridge::{SourcePlace, SOURCE_UNKNOWN};
use retro_core::video::{
    crop_rgba, equals_cropped_rgba, LineStateTable, PixelImage, VideoDebugger,
};
use retro_core::*;

use super::SnesEmulator;
#[cfg(doc)]
use crate::bus::SnesBus;
use crate::cartridge::copier_header_length;
use crate::scheduler::EventKind;

/// The SNES's part of a frame snapshot ([`FrameSnapshot::console`],
/// [`CaptureCore::capture_frame_snapshot`]): the sprite table and the PPU's
/// settings the picture was drawn with. The snapshot's `vram` is VRAM, 64
/// KiB, each 16-bit word low byte first; its `palette` CGRAM, 256 BGR555
/// colours (`0BBBBBGGGGGRRRRR`); its `registers` the list
/// `VideoDebugger::get_registers` reports.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SnesSnapshotConsole {
    /// OAM, 544 bytes: the 512-byte table, then the 32-byte high table.
    pub oam: Vec<u8>,
    /// The PPU's settings, structured.
    pub ppu_settings: crate::ppu::PpuSettings,
}

impl SnesEmulator {
    /// Whether a game is loaded: the readers of the held frame answer
    /// nothing before one is.
    fn game_loaded(&self) -> bool {
        self.bus.cartridge_ref().is_some()
    }

    /// The number a snapshot of the picture the host holds carries
    /// ([`FrameSnapshot::frame`]): the number of the frame whose picture
    /// the framebuffer shows. The frames finished, plus one while the frame
    /// in progress has its picture finished: held at its capture boundary,
    /// or stopped past it in its vertical blank. A frame stopped part of
    /// the way through its picture, whose framebuffer shows the lines drawn
    /// so far over the frame before, is numbered by the frames finished.
    fn picture_frame(&self) -> u64 {
        self.frame_count + u64::from(self.picture_finished_in_frame())
    }

    /// Whether the frame in progress has its picture finished
    /// ([`SnesEmulator::finish_picture`] ran for it): it is held at its
    /// capture boundary, or its `VBlankStart` was handled, which leaves no
    /// `VBlankStart` queued until the frame's end arms the next frame's.
    /// Read off the saved scheduler, as
    /// [`SnesEmulator::frame_in_progress`] is.
    fn picture_finished_in_frame(&self) -> bool {
        self.held_at_capture_boundary
            || (self.frame_in_progress()
                && self.scheduler.earliest(&EventKind::VBlankStart).is_none())
    }

    /// The picture the host holds, as
    /// [`capture_frame_snapshot`](CaptureCore::capture_frame_snapshot) crops
    /// it: `(width, height)`.
    fn picture_size(&self) -> (u32, u32) {
        let display = &self.system_info.display;
        (
            display.width.min(self.framebuffer.width),
            display.height.min(self.framebuffer.height),
        )
    }

    /// Whether `rgba`, `width` x `height`, is the picture the host holds,
    /// byte for byte.
    fn is_held_picture(&self, width: u32, height: u32, rgba: &[u8]) -> bool {
        (width, height) == self.picture_size()
            && equals_cropped_rgba(
                &self.framebuffer.data,
                self.framebuffer.width,
                width,
                height,
                rgba,
            )
    }

    /// Each line's window masks ([`crate::ppu::Ppu::window_masks`]), the
    /// frame numbered as a frame snapshot numbers its picture. `None` when
    /// no game is loaded.
    pub fn window_masks(&self) -> Option<crate::ppu::WindowMasks> {
        self.game_loaded()
            .then(|| self.bus.ppu().window_masks(self.picture_frame()))
    }

    /// The offset-per-tile columns of the held frame
    /// ([`crate::ppu::Ppu::offset_per_tile_table`]), the frame numbered as
    /// a frame snapshot numbers its picture. `None` when no game is loaded.
    pub fn offset_per_tile(&self) -> Option<crate::ppu::OffsetPerTileTable> {
        self.game_loaded()
            .then(|| self.bus.ppu().offset_per_tile_table(self.picture_frame()))
    }
}

/// The capture contract over the mechanism the `system` module keeps: the boundary
/// stop in the frame loops (`capture_boundary_requested`,
/// `held_at_capture_boundary`), the PPU's line table and traced redraw, and
/// its live recorders of video memory.
impl CaptureCore for SnesEmulator {
    type SnapshotConsole = SnesSnapshotConsole;

    /// Applies at once and survives a ROM load and a reset. On this core the
    /// line table is kept for any of `line_states`, `pixel_sources` and
    /// `line_sprites` (the other two are worked out from it), and
    /// `video_memory_sources` and `frame_writes` arm the PPU's two live
    /// recorders.
    fn set_capture_recording(&mut self, wanted: CaptureRecording) {
        self.capture_recording = wanted;
        self.bus.set_capture_recording(wanted);
    }

    fn capture_recording(&self) -> CaptureRecording {
        self.capture_recording
    }

    /// Run to the next capture boundary and stop there: the moment the
    /// frame's active display has been drawn, in front of its
    /// `VBlankStart`. The framebuffer is final there (the hires and
    /// overscan normalisation done, `DisplayInfo` synced) while VRAM,
    /// CGRAM, OAM and the registers still hold what the picture was drawn
    /// from: no NMI is delivered and nothing of the vertical blank has run,
    /// so the game's uploads for the next frame have not landed.
    ///
    /// Held at a boundary already, the call first finishes that frame,
    /// then runs the next one to its boundary, so each call advances
    /// exactly one frame. A frame in progress that is past its
    /// `VBlankStart` (a breakpoint stopped it in the vertical blank) is
    /// run to its end and the next one to its boundary.
    ///
    /// The loops are the ones a frame call picks, by the same tier
    /// selection, and breakpoints stop where they stop: a breakpoint that
    /// hits on the way leaves the frame paused there instead, which
    /// [`Self::at_capture_boundary`] then reports as `false`. While the
    /// debugger is paused or stepping the call does what a frame call
    /// does then.
    ///
    /// The rest of a held frame runs on the next frame call (`run_frame`,
    /// `run_frame_headless`), which finishes that frame and returns, on
    /// the next boundary call (`run_until_vblank` is one), on the next
    /// step, which leaves the boundary first (`leave_capture_boundary`),
    /// or on the save path ([`Self::save_state_at_frame_end`]). A reset, a
    /// ROM load or a state load drops it.
    fn run_to_capture_boundary(&mut self) {
        self.sync_spc_trace_enable();
        self.sync_cartridge_trace_enable();
        self.sync_debug_arm();
        let tier = self.debugger.execution_tier();
        if tier == ExecutionTier::FullDebug {
            self.run_frame_full();
            return;
        }
        if self.held_at_capture_boundary {
            self.run_frame_on_tier(tier);
            if self.frame_call_stopped() {
                // A breakpoint stopped the rest of the held frame.
                return;
            }
        }
        self.capture_boundary_requested = true;
        // At most two frames: the rest of a frame already past its
        // `VBlankStart`, then the next frame up to its own.
        for _ in 0..2 {
            self.run_frame_on_tier(tier);
            if self.frame_call_stopped() {
                break;
            }
        }
        self.capture_boundary_requested = false;
    }

    fn at_capture_boundary(&self) -> bool {
        self.held_at_capture_boundary
    }

    /// The frame held at a capture boundary, if any, is finished first
    /// ([`SnesEmulator::finish_held_frame`]): on the fast loop, whatever the
    /// debugger is doing, so no breakpoint stops it and the save holds the
    /// frame's end, the state a player's run of that frame reaches.
    /// Anywhere else it is [`EmulatorCore::save_state`].
    fn save_state_at_frame_end(&mut self) -> Vec<u8> {
        self.finish_held_frame();
        EmulatorCore::save_state(self)
    }

    /// The console's part is OAM and the PPU's settings
    /// ([`SnesSnapshotConsole`]). A frame stopped by a breakpoint part of
    /// the way through its picture is numbered by the frames finished; one
    /// stopped in its vertical blank, whose picture is finished, one more,
    /// as at its capture boundary.
    fn capture_frame_snapshot(&self) -> FrameSnapshot<SnesSnapshotConsole> {
        let (width, height) = self.picture_size();
        let ppu = self.bus.ppu();
        FrameSnapshot {
            frame: self.picture_frame(),
            at_boundary: self.held_at_capture_boundary,
            width,
            height,
            framebuffer: crop_rgba(
                &self.framebuffer.data,
                self.framebuffer.width,
                width,
                height,
            ),
            vram: ppu.get_vram().to_vec(),
            palette: ppu.cgram().to_vec(),
            registers: ppu.get_registers(),
            console: SnesSnapshotConsole {
                oam: ppu.oam().to_vec(),
                ppu_settings: ppu.settings(),
            },
        }
    }

    /// Mirrors the live per-frame sequence (`Ppu::redraw_frame`): every
    /// visible line through the renderer, from line 1 so the per-frame
    /// resets run, then `finalize_frame`'s hires and overscan normalisation.
    /// The sprite range and time flags and the evaluator's OAM address,
    /// which drawing moves and a savestate holds, are put back as they were,
    /// so a repaint leaves the state hash where it was. The frame is drawn
    /// from the line table only when the table holds the whole frame and no
    /// register was edited since ([`Self::draws_from_line_states`]), and the
    /// answer says so.
    fn repaint_held_frame(&mut self) -> bool {
        let from_line_states = self.draws_from_line_states();
        self.bus.ppu_mut().redraw_frame(&mut self.framebuffer);
        from_line_states
    }

    /// `layer` is `0..=3` for BG1-4 and `4` for the sprites
    /// ([`crate::ppu::Ppu::render_layer`]): drawn at the master brightness,
    /// without colour math, whatever the screens, the windows and the
    /// debugger's layer mask say.
    fn render_layer(&mut self, layer: u32) -> Option<RenderedLayer> {
        let from_line_states = self.draws_from_line_states();
        let image = self.bus.ppu_mut().render_layer(layer as usize).ok()?;
        Some(RenderedLayer {
            image,
            from_line_states,
        })
    }

    /// Answers only when the line table holds a whole frame, from its line 1
    /// to its finished picture ([`crate::ppu::Ppu::picture_from_line_states`]).
    fn picture_from_line_states(&mut self) -> Option<PixelImage> {
        self.bus.ppu_mut().picture_from_line_states()
    }

    /// The columns are `line` (the scanline, 1 for the first drawn) then
    /// every setting of [`crate::ppu::LineState`], in its own names: a
    /// setting kept per background or per layer gives a column per element
    /// (`bg_hoffset[0]`), a window selection a column per flag
    /// (`window_enable[2].w1_enable`). A debugger edit of video memory since
    /// the table's line 1 clears [`LineStateTable::memory_exact`]. `None`
    /// when no table is kept or no line was drawn since it was started.
    fn line_states(&self) -> Option<LineStateTable> {
        self.bus.ppu().line_state_table()
    }

    /// One palette of the 256 CGRAM colours for each distinct CGRAM a line
    /// read ([`crate::ppu::Ppu::line_palettes`]); this core records no
    /// palette writes within a line.
    fn line_palettes(&self) -> Option<LinePalettes> {
        self.bus.ppu().line_palettes()
    }

    /// The planes as [`crate::ppu::TracedFrame`] documents them, the sub
    /// screen's included.
    fn pixel_sources(&mut self, planes: u32) -> Option<PixelSources> {
        if !self.game_loaded() {
            return None;
        }
        let frame = self.picture_frame();
        let traced = self.bus.ppu_mut().pixel_sources(planes);
        let matches_picture = self.is_held_picture(traced.width, traced.height, &traced.picture);
        Some(traced.into_sources(frame, matches_picture))
    }

    /// The fates and line flags this core gives are listed on
    /// [`crate::ppu::Ppu::line_sprites`].
    fn line_sprites(&mut self) -> Option<LineSprites> {
        self.game_loaded()
            .then(|| self.bus.ppu().line_sprites(self.picture_frame()))
    }

    /// The sprite table has 128 slots ([`crate::ppu::Ppu::render_sprites`]).
    fn render_sprites(&mut self, selection: &[u8]) -> Option<RenderedLayer> {
        if !self.game_loaded() {
            return None;
        }
        let mut chosen = [false; 128];
        for (flag, &byte) in chosen.iter_mut().zip(selection) {
            *flag = byte != 0;
        }
        let from_line_states = self.draws_from_line_states();
        let image = self.bus.ppu_mut().render_sprites(&chosen);
        Some(RenderedLayer {
            image,
            from_line_states,
        })
    }

    /// VRAM's 64 KiB, the record of each byte as `crate::ppu::writes`
    /// documents it.
    fn video_memory_sources(&self) -> Option<&VideoMemorySources> {
        self.bus.ppu().video_memory_sources()
    }

    /// The records as `crate::ppu::writes` documents them, at most
    /// [`crate::ppu::writes::FRAME_WRITES_CAP`] a take.
    fn take_frame_writes(&mut self) -> Option<FrameWrites> {
        self.bus.ppu_mut().take_frame_writes()
    }

    /// Each address is an A-bus address, routed as a read of it is
    /// ([`SnesBus::source_place`]). The ROM offset counts from the file's
    /// first byte, the copier header of a file that carries one included;
    /// an offset into the zeroes padding a ROM to a power of two is unknown.
    /// Work RAM is -2; the cartridge's RAM (SRAM, BW-RAM, Game Pak RAM) -3;
    /// a coprocessor's registers or memory, or ROM the SA-1, the S-DD1 or
    /// the SPC7110 reach through banks of their own, -4 (the Super FX's and
    /// the Cx4's ROM windows give the ROM's offset); open bus, the B bus
    /// and the 5A22's registers -1.
    fn resolve_sources(&self, addresses: &[u32]) -> Vec<i32> {
        let header = copier_header_length(self.rom_size);
        let data = self.rom_size - header;
        addresses
            .iter()
            .map(|&address| match self.bus.source_place(address) {
                SourcePlace::Rom(offset) if (offset as usize) < data => {
                    SourcePlace::Rom(offset + header as u32).code()
                }
                SourcePlace::Rom(_) => SOURCE_UNKNOWN,
                place => place.code(),
            })
            .collect()
    }
}
