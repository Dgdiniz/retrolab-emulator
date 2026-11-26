//! The capture contract (`retro_core::capture::CaptureCore`) on this
//! console, and the picture and snapshot helpers it reads.

use super::GenesisEmulator;
#[cfg(doc)]
use crate::debugger::GenesisDebugger;
use retro_core::capture::{
    CaptureCore, CaptureRecording, DrawOrder, FrameSnapshot, FrameWrites, LinePalettes,
    LineSprites, PixelSources, RenderedLayer, VideoMemorySources,
};
use retro_core::video::{crop_rgba, LineStateTable, PixelImage, VideoDebugger};
#[cfg(doc)]
use retro_core::EmulatorCore;

/// What only the Mega Drive has in a frame snapshot
/// ([`CaptureCore::capture_frame_snapshot`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct GenesisSnapshotConsole {
    /// The 64 words of the vertical scroll RAM.
    pub vsram: Vec<u16>,
}

impl GenesisEmulator {
    /// The number of the frame whose picture the host's framebuffer shows:
    /// the frames finished, plus one while a frame in progress is on its
    /// capture boundary's line or a later one (held at the boundary, or
    /// stopped in its vertical blanking), where the framebuffer holds that
    /// frame's finished picture (it is counted once it ends). A frame
    /// stopped before its boundary is numbered by the frames finished: the
    /// framebuffer shows its lines drawn so far over the frame before's.
    fn picture_frame(&self) -> u64 {
        let boundary = u32::from(self.bus.vdp().first_vblank_line());
        let finished = self.frame_open && self.frame_line >= boundary;
        self.frame_count + u64::from(finished)
    }

    /// The picture the host shows, as [`EmulatorCore::get_display_info`]
    /// sizes it: the framebuffer cut to the rows the VDP draws (its lines,
    /// two rows each in interlace mode 2).
    fn shown_picture(&self) -> (u32, u32, Vec<u8>) {
        let fb = &self.cached_framebuffer;
        let display = &self.system_info.display;
        let width = display.width.min(fb.width);
        let height = display.height.min(fb.height);
        (width, height, crop_rgba(&fb.data, fb.width, width, height))
    }
}

/// The capture contract on the Mega Drive. The VDP records the line table,
/// who wrote each byte of VRAM and the frame writes (`vdp::recording`,
/// `vdp::writes`); the pixel sources, the line sprites and the chosen
/// sprites are worked out on demand from the line table (`vdp::sources`).
///
/// How this core keeps the contract:
///
/// - A frame is held at its capture boundary by the capture call, and left
///   part of the way through by a stop or a host step; the snapshot, the
///   pixel sources and the line sprites number the frames finished, plus
///   one while the frame in progress is on the boundary's line or past it,
///   where the framebuffer shows its finished picture.
/// - A save ([`EmulatorCore::save_state`]) finishes a frame held at its
///   capture boundary on the play tier, whatever the debugger is doing, so
///   no breakpoint stops it; a frame stopped anywhere else is saved where
///   it stands, the extension after the payload holding it.
/// - The repaint, a layer, the pixel sources and the line sprites draw each
///   line the line table holds from its record (the lines below a stop part
///   of the way down from the frame before's) and every other line from the
///   state as it stands, and say whether every line had its record; the
///   picture from the line states answers only while the table holds the
///   frame from its first line to its finished picture.
/// - A debugger edit of video memory is not journaled: a line drawn again
///   reads it wherever the frame did not write over it later, and an edit
///   of a colour, a vertical scroll word or the sprite table is patched into
///   every recorded line, so the edit shows; the table's `memory_exact` is
///   false until the next picture's first line.
/// - The pixel sources, the line sprites and the chosen sprites are `None`
///   while no cartridge is loaded.
impl CaptureCore for GenesisEmulator {
    type SnapshotConsole = GenesisSnapshotConsole;

    /// This core records all five recorders, each as asked. The setting is
    /// applied again to a new VDP (a ROM load that rebuilds the hardware, a
    /// power cycle).
    fn set_capture_recording(&mut self, wanted: CaptureRecording) {
        self.capture_recording = wanted;
        self.bus.vdp_mut().set_capture_recording(wanted);
    }

    fn capture_recording(&self) -> CaptureRecording {
        self.bus.vdp().capture_recording()
    }

    /// On the tier the debugger's state picks
    /// ([`GenesisDebugger::execution_tier`]: the play tier while no debug
    /// feature is armed), through the frame driver
    /// (`retro_core::frame::run_to_capture_boundary`). A frame stopped
    /// before its boundary runs on to it.
    fn run_to_capture_boundary(&mut self) {
        retro_core::frame::run_to_capture_boundary(self);
    }

    fn at_capture_boundary(&self) -> bool {
        self.held_at_capture_boundary
    }

    /// The picture cut to the display info (the mode's width by
    /// [`crate::vdp::Vdp::picture_height`]: the lines drawn, two rows each
    /// in interlace mode 2), VRAM, CRAM's 64 colours of
    /// `0000 BBB0 GGG0 RRR0`, the 24 VDP registers, and, as the console's
    /// part, the vertical scroll RAM's 64 words.
    fn capture_frame_snapshot(&self) -> FrameSnapshot<GenesisSnapshotConsole> {
        let (width, height, framebuffer) = self.shown_picture();
        let vdp = self.bus.vdp();
        FrameSnapshot {
            // A frame held part of the way through is counted once it ends,
            // so its picture is the next one.
            frame: self.picture_frame(),
            at_boundary: self.at_capture_boundary(),
            width,
            height,
            grid: self.system_info.display.grid,
            framebuffer,
            vram: vdp.get_vram().to_vec(),
            palette: vdp.get_cram().to_vec(),
            registers: vdp.get_registers(),
            console: GenesisSnapshotConsole {
                vsram: vdp.get_vsram().to_vec(),
            },
        }
    }

    /// The host reads frames through [`EmulatorCore::get_framebuffer`], the
    /// cached copy the frame loops refresh once a frame: this refreshes it
    /// too after the VDP draws the frame again.
    fn repaint_held_frame(&mut self) -> bool {
        let from_line_states = self.bus.vdp_mut().rerender_current_frame();
        self.update_cached_framebuffer();
        from_line_states
    }

    /// The layers are those of the debugger's layer list: 0 plane A, 1
    /// plane B, 2 the window, 3 the sprites
    /// ([`crate::vdp::Vdp::render_layer_image`]).
    fn render_layer(&mut self, layer: u32) -> Option<RenderedLayer> {
        self.bus.vdp_mut().render_layer_image(layer as usize)
    }

    /// `priority` is a plane's or the window's map cell priority bit, or a
    /// sprite's priority bit: 1 high, 0 low
    /// ([`crate::vdp::Vdp::render_layer_priority`]).
    fn render_layer_priority(&mut self, layer: u32, priority: u8) -> Option<RenderedLayer> {
        self.bus
            .vdp_mut()
            .render_layer_priority(layer as usize, priority)
    }

    /// One order in every line ([`crate::vdp::Vdp::draw_order`]), front to
    /// back: the high sprites, plane A's and the window's high cells, plane
    /// B's high cells, the low sprites, plane A's and the window's low
    /// cells, plane B's low cells.
    fn draw_order(&self) -> Vec<DrawOrder> {
        self.bus.vdp().draw_order()
    }

    fn picture_from_line_states(&mut self) -> Option<PixelImage> {
        self.bus.vdp_mut().picture_from_line_states()
    }

    /// The table [`crate::vdp::Vdp::line_state_table`] gives: one row for
    /// each visible line, from line 0, the picture's first row.
    fn line_states(&self) -> Option<LineStateTable> {
        self.bus.vdp().line_state_table()
    }

    fn line_palettes(&self) -> Option<LinePalettes> {
        self.bus.vdp().line_palettes()
    }

    /// Packed as `vdp::sources` describes; no sub screen.
    fn pixel_sources(&mut self, planes: u32) -> Option<PixelSources> {
        self.bus.cartridge()?;
        let frame = self.picture_frame();
        let mut sources = self.bus.vdp_mut().pixel_sources(planes);
        sources.frame = frame;
        Some(sources)
    }

    fn line_sprites(&mut self) -> Option<LineSprites> {
        self.bus.cartridge()?;
        let frame = self.picture_frame();
        let mut sprites = self.bus.vdp_mut().line_sprites();
        sprites.frame = frame;
        Some(sprites)
    }

    /// The sprite table has 80 slots (64 in the 32-cell mode).
    fn render_sprites(&mut self, selection: &[u8]) -> Option<RenderedLayer> {
        self.bus.cartridge()?;
        Some(self.bus.vdp_mut().render_sprites(selection))
    }

    /// VRAM's 65,536 bytes, as `vdp::writes` describes them.
    fn video_memory_sources(&self) -> Option<&VideoMemorySources> {
        self.bus.vdp().video_memory_sources()
    }

    fn take_frame_writes(&mut self) -> Option<FrameWrites> {
        self.bus.vdp_mut().take_frame_writes()
    }

    /// 68000 addresses ([`crate::bus::AddressBus::resolve_sources`]): the
    /// cartridge window through the cartridge's mapping as it stands (the
    /// SEGA or SSF2 mapper's banks, the ROM mirrored past its end, the ROM
    /// showing through switched-off SRAM), work RAM -2, anything else -1.
    fn resolve_sources(&self, addresses: &[u32]) -> Vec<i32> {
        self.bus.resolve_sources(addresses)
    }
}
