//! The capture contract: what a host asks a core to record while frames run,
//! and what it reads back about the frame held on screen.
//!
//! A host captures a frame where its picture is drawn and its vertical blank
//! has not begun, the capture boundary: there the video memory, the palette
//! and the registers still hold what the picture was drawn from. What a core
//! records while frames run ([`CaptureRecording`]) is a host setting: never
//! part of a savestate, and nothing the machine does depends on it.
//!
//! The shapes, codes and logs a core hands over are written once here: where
//! each pixel came from ([`PixelSources`], [`planes`], [`layer`],
//! [`tile_value`]), the sprites each line held ([`LineSprites`],
//! [`SpriteFate`], [`line_flags`]), who wrote each byte of video memory
//! ([`VideoMemorySources`], [`WriteOrigin`], [`Writer`]), what each frame
//! wrote ([`FrameWrite`], [`FrameWrites`], [`WriteLog`]), the line table's
//! helpers ([`LinePalettes`], [`distinct_palettes`], [`TableColumns`],
//! [`line_state!`](crate::line_state)), the frame snapshot
//! ([`FrameSnapshot`], [`RenderedLayer`]) and the order the layers are put
//! in ([`DrawSlot`], [`DrawOrder`]). A core keeps its mechanism: where it
//! records, the fields of a line's settings, how it draws a frame again and
//! how it reaches the boundary. [`CaptureCore`] is what a host calls.
//!
//! [`images`] names the images a host reads of a held frame; `png` (behind
//! the `png` feature) writes and reads them as PNGs, and the golden-image
//! harness (`image_goldens`, behind the `image-goldens` feature) holds a
//! core's to the ones its tests keep.

use serde::{Deserialize, Serialize};

use crate::emulator::EmulatorCore;
use crate::video::{LineStateTable, PixelImage};

mod frame;
pub mod images;
mod lines;
mod order;
mod pixels;
mod sprites;
mod writes;

#[cfg(feature = "capture-conformance")]
pub mod conformance;
#[cfg(feature = "image-goldens")]
pub mod image_goldens;
#[cfg(feature = "png")]
pub mod png;

pub use frame::{FrameSnapshot, RenderedLayer};
pub use lines::{distinct_palettes, LinePalettes, TableColumns, NO_COLUMN, NO_PALETTE};
pub use order::{draw_order_runs, DrawOrder, DrawSlot};
pub use pixels::{drawn_by, layer, planes, tile_value, NoTrace, PixelSources, PixelTrace, NO_CELL};
pub use sprites::{line_flags, LineSprites, SpriteFate, LINE_SPRITES_STRIDE};
pub use writes::{
    FrameWrite, FrameWrites, VideoMemorySources, WriteKind, WriteLog, WriteOrigin, Writer,
    WORDS_PER_RECORD,
};

/// What a core records while frames run, for a capture to read. All off by
/// default; a host setting, never part of a savestate.
///
/// `line_states` keeps the line table: the settings each line of the picture
/// was drawn with, from which a core draws the frame again as it was drawn.
/// A core that works the pixel sources and the line sprites out of that
/// table on demand keeps it for either of them too ([`Self::wants_line_table`]).
/// `video_memory_sources` and `frame_writes` are recorded as the writes land:
/// who wrote each byte of video memory, and the log of each frame's
/// transfers and palette and sprite-table writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRecording {
    /// The settings each line of the picture was drawn with, and what
    /// drawing the picture again from them needs.
    pub line_states: bool,
    /// Where each pixel of the picture came from ([`PixelSources`]).
    pub pixel_sources: bool,
    /// The sprites each line held and what became of each ([`LineSprites`]).
    pub line_sprites: bool,
    /// What wrote each byte of video memory, and in which frame
    /// ([`VideoMemorySources`]).
    pub video_memory_sources: bool,
    /// The transfers and the palette and sprite-table writes since the last
    /// take ([`FrameWrites`]).
    pub frame_writes: bool,
}

impl CaptureRecording {
    /// Whether what is asked for needs the line table: the line states, or
    /// the pixel sources or line sprites a core works out from it.
    pub fn wants_line_table(&self) -> bool {
        self.line_states || self.pixel_sources || self.line_sprites
    }
}

/// Arm or disarm one recorder kept in `slot`: one `wanted` and absent is made
/// with `make` and starts as `make` builds it (empty); one not wanted is
/// dropped; one wanted and present keeps what it holds.
pub fn arm_recorder<T>(slot: &mut Option<T>, wanted: bool, make: impl FnOnce() -> T) {
    match (wanted, slot.is_some()) {
        (true, false) => *slot = Some(make()),
        (false, true) => *slot = None,
        _ => {}
    }
}

/// What a host calls to capture the frame a core holds: the recording, the
/// capture boundary, the snapshot, the frame drawn again, and the readers of
/// what was recorded.
///
/// None of it advances emulation unless it says so, and none of it moves
/// anything a savestate or the state hash reads: a frame drawn again puts
/// back what drawing it changed. Each method's doc is the contract every
/// core keeps; what a core puts in the shapes (its layer codes, its
/// columns, its units) its own docs give.
pub trait CaptureCore: EmulatorCore {
    /// The console's part of a frame snapshot.
    type SnapshotConsole;

    /// Ask the core to record what `wanted` names while frames run. A host
    /// setting: never part of a savestate, kept across a reset, a ROM load
    /// and a savestate load, and nothing the machine does depends on it. A
    /// recorder turned on starts empty, one already on keeps what it holds,
    /// one turned off is dropped. A line table armed in the middle of a frame
    /// holds that frame only from there on.
    fn set_capture_recording(&mut self, wanted: CaptureRecording);

    /// What the core records while frames run: what the host asked for that
    /// this core records.
    fn capture_recording(&self) -> CaptureRecording;

    /// Run to the next capture boundary and stop there: the frame's picture
    /// drawn and its vertical blank not begun, so the video memory, the
    /// palette and the registers still hold what the picture was drawn from.
    /// A frame already held at its boundary is finished first, so each call
    /// moves the game on one frame. The frame loop is the one a frame call
    /// picks, and a breakpoint stops where it stops, leaving the core held at
    /// no boundary. The frame stays held until the next frame call runs the
    /// rest of it; a reset, a ROM load and a savestate load drop it.
    fn run_to_capture_boundary(&mut self);

    /// Whether the core is held at a capture boundary.
    fn at_capture_boundary(&self) -> bool;

    /// Copies of the picture the host shows and of the video memory, the
    /// palette and the registers it was drawn from ([`FrameSnapshot`]). At a
    /// capture boundary both belong to the frame held there; anywhere else
    /// the memory may already hold what the game wrote for the next frame,
    /// which [`FrameSnapshot::at_boundary`] tells the host.
    ///
    /// [`FrameSnapshot::frame`] numbers the frame whose picture the host's
    /// framebuffer shows: the frames finished, plus one while the frame is
    /// held at its capture boundary or stopped past it before its end, where
    /// the framebuffer shows the held frame's finished picture. So a
    /// snapshot taken there and one taken after the same frame ran whole
    /// carry the same number. A frame stopped before its boundary (an event
    /// breakpoint part of the way down) is numbered by the frames finished:
    /// the framebuffer shows the lines drawn so far over the frame before.
    fn capture_frame_snapshot(&self) -> FrameSnapshot<Self::SnapshotConsole>;

    /// Draw the held frame again into the framebuffer the host shows, with
    /// the debugger's layer mask, from the line table where it holds the
    /// frame, without advancing emulation: after a host toggles a layer or
    /// edits video memory while paused, the change shows at once. Returns
    /// whether every line was drawn from the line table, as
    /// [`RenderedLayer::from_line_states`] says of a layer.
    fn repaint_held_frame(&mut self) -> bool;

    /// One layer of the held frame drawn alone, alpha-keyed: `layer` is an id
    /// of the core's layer list (the ids the debugger's layer toggles use),
    /// `None` for any other. A pixel the layer covers is opaque, in the
    /// layer's own colour; every other pixel is 0 in all four bytes, the
    /// backdrop's included. Drawn from the line table where it holds the
    /// frame, as [`RenderedLayer::from_line_states`] says.
    fn render_layer(&mut self, layer: u32) -> Option<RenderedLayer>;

    /// Layer `layer` of the held frame drawn alone with only its pixels of
    /// priority `priority`, as [`Self::render_layer`] draws the layer: a
    /// pixel of the layer whose priority is `priority` is opaque, in the
    /// layer's own colour, and every other pixel is 0 in all four bytes.
    /// What a pixel's priority is on a console (a cell's priority bit, a
    /// sprite's priority) the core documents; [`Self::draw_order`] names
    /// the priorities each layer has and where the console puts them. The
    /// images of a layer's priorities add up to the layer's: each pixel
    /// [`Self::render_layer`] draws is in one of them. `None` where
    /// [`Self::render_layer`] gives none.
    ///
    /// Default: `None`, for a core that does not split its layers.
    fn render_layer_priority(&mut self, layer: u32, priority: u8) -> Option<RenderedLayer> {
        let _ = (layer, priority);
        None
    }

    /// The order the held frame's lines put the layers in, each place the
    /// pixels of one priority of one layer ([`DrawOrder`], [`DrawSlot`]):
    /// one entry for each run of lines in one order, in the order the lines
    /// were drawn, covering every line the frame draws. Each line's order
    /// is read from the settings it was drawn with where the line table
    /// holds the frame (as [`Self::render_layer`] draws), else from the
    /// state as it stands. Stacked from the last place to the first, the
    /// images [`Self::render_layer_priority`] draws give the layers as the
    /// console puts them on a line of the run.
    ///
    /// Default: empty, for a core that does not say.
    fn draw_order(&self) -> Vec<DrawOrder> {
        Vec::new()
    }

    /// The picture drawn again from the line table into a buffer of its own,
    /// cut as the picture the host shows is: only while the table holds the
    /// frame from its first line to its finished picture
    /// ([`LineStateTable::from_first_line`] and [`LineStateTable::complete`]),
    /// `None` otherwise. Where its memory is exact
    /// ([`LineStateTable::memory_exact`]) it is the frame's own picture,
    /// byte for byte. [`Self::render_layer`], [`Self::pixel_sources`] and
    /// [`Self::line_sprites`] answer for a table that holds less, and say
    /// whether every line was drawn from it.
    fn picture_from_line_states(&mut self) -> Option<PixelImage>;

    /// The settings each line of the frame was drawn with, while the line
    /// table is kept ([`LineStateTable`]): one column per setting, in the
    /// core's own names, one row per line. A setting kept per layer or per
    /// index gives a column per element, named `name[index]`
    /// ([`TableColumns`]). Lines keep the console's own numbering:
    /// [`LineStateTable::first_row_line`] is the line drawn into the
    /// picture's first row. This is the one way to read the table.
    ///
    /// A debugger write into video memory (its tiles and maps, the palette,
    /// the scroll or sprite memory) since the table's frame began makes
    /// [`LineStateTable::memory_exact`] false until the next frame's first
    /// line; a frame drawn again still shows the edit.
    fn line_states(&self) -> Option<LineStateTable>;

    /// The palettes the lines of the line table were drawn with
    /// ([`LinePalettes`]), while the line table is kept.
    fn line_palettes(&self) -> Option<LinePalettes>;

    /// Where each pixel of the held frame came from, in the planes `planes`
    /// asks for (bits of [`planes`]; other bits are ignored); `None` while no
    /// game is loaded. The frame is drawn again with a trace, from the line
    /// table where it holds the frame; [`PixelSources::matches_picture`]
    /// says whether that picture is the one the host holds, and
    /// [`PixelSources::frame`] numbers it as a snapshot does.
    fn pixel_sources(&mut self, planes: u32) -> Option<PixelSources>;

    /// The sprites each line of the held frame met and what became of each
    /// ([`LineSprites`], numbered as a snapshot is); `None` while no game is
    /// loaded.
    fn line_sprites(&mut self) -> Option<LineSprites>;

    /// The chosen sprites of the held frame drawn alone, as the frame drew
    /// them (after each line's limits and priority among sprites), alpha-keyed
    /// as [`Self::render_layer`] draws; `None` while no game is loaded.
    /// `selection` holds one flag a slot of the sprite table:
    /// `selection[slot] != 0` chooses the sprite, and a short array chooses
    /// nothing past its end.
    fn render_sprites(&mut self, selection: &[u8]) -> Option<RenderedLayer>;

    /// Who wrote each byte of video memory, and in which frame, while video
    /// memory sources are recorded ([`VideoMemorySources`]). Turned on it
    /// starts empty; a reset, a savestate load and a ROM load empty it.
    fn video_memory_sources(&self) -> Option<&VideoMemorySources>;

    /// The writes recorded since the last take, the log then empty, while
    /// frame writes are recorded ([`FrameWrites`]). Taken at each capture
    /// boundary, they are what built the frame held there: the uploads of
    /// the vertical blank before it and the writes made while its picture was
    /// drawn. Turned on the log starts empty; a reset, a savestate load and a
    /// ROM load empty it.
    fn take_frame_writes(&mut self) -> Option<FrameWrites>;

    /// For each address, as a write origin's source holds it (bits 24-31 are
    /// ignored), where a read of it comes from now, through the cartridge's
    /// mapping as it stands: the offset into the ROM file, or a negative code
    /// ([`crate::cartridge::SourcePlace::code`]). Reads the mapping and
    /// changes nothing.
    fn resolve_sources(&self, addresses: &[u32]) -> Vec<i32>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_table_is_wanted_by_the_three_readers_that_need_it() {
        let none = CaptureRecording::default();
        assert!(!none.wants_line_table());
        for wanted in [
            CaptureRecording {
                line_states: true,
                ..none
            },
            CaptureRecording {
                pixel_sources: true,
                ..none
            },
            CaptureRecording {
                line_sprites: true,
                ..none
            },
        ] {
            assert!(wanted.wants_line_table());
        }
        for live in [
            CaptureRecording {
                video_memory_sources: true,
                ..none
            },
            CaptureRecording {
                frame_writes: true,
                ..none
            },
        ] {
            assert!(!live.wants_line_table());
        }
    }

    #[test]
    fn a_recorder_is_made_when_wanted_kept_while_wanted_and_dropped_after() {
        let mut slot: Option<Vec<u32>> = None;
        let mut made = 0;
        arm_recorder(&mut slot, false, || {
            made += 1;
            Vec::new()
        });
        assert_eq!((slot.clone(), made), (None, 0), "not wanted, not made");

        arm_recorder(&mut slot, true, || {
            made += 1;
            Vec::new()
        });
        assert_eq!((slot.clone(), made), (Some(Vec::new()), 1));

        slot.as_mut().expect("armed").push(7);
        arm_recorder(&mut slot, true, || {
            made += 1;
            Vec::new()
        });
        assert_eq!(
            (slot.clone(), made),
            (Some(vec![7]), 1),
            "one already on keeps what it holds"
        );

        arm_recorder(&mut slot, false, || {
            made += 1;
            Vec::new()
        });
        assert_eq!((slot, made), (None, 1), "one turned off is dropped");
    }
}
