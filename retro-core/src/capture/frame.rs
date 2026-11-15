//! The frame a host captures: copies of its picture and of what it was drawn
//! from, and a layer of it drawn again.

use serde::Serialize;

use crate::video::{PixelImage, VideoRegister};

/// What a capture reads, copied out of the core: a frame's picture, and the
/// video memory, palette and registers it was drawn from
/// ([`super::CaptureCore::capture_frame_snapshot`]). What only one console
/// has is `console` (the Mega Drive's vertical scroll memory; the SNES's
/// sprite table and its PPU's settings).
///
/// Taken at a capture boundary, the picture and the memory belong to the
/// same frame. Taken anywhere else, the memory may already hold what the
/// game wrote for the next frame while the picture shows the frame before;
/// `at_boundary` says which.
#[derive(Debug, Clone, Serialize)]
pub struct FrameSnapshot<C> {
    /// The number of the frame whose picture `framebuffer` is: the frames
    /// finished, plus one while the frame is held at its capture boundary or
    /// stopped past it before its end (it is counted once it ends). A
    /// snapshot taken at the boundary and one taken after the same frame ran
    /// whole carry the same number.
    pub frame: u64,
    /// The core was held at a capture boundary.
    pub at_boundary: bool,
    /// The picture's width in pixels, as the core's display information
    /// gives it.
    pub width: u32,
    /// The picture's height in pixels.
    pub height: u32,
    /// The picture, RGBA, `width * height * 4` bytes, row after row.
    pub framebuffer: Vec<u8>,
    /// The video memory, in the byte order the core's debugger gives it.
    pub vram: Vec<u8>,
    /// The palette memory, the console's own colour words.
    pub palette: Vec<u16>,
    /// The video registers, as the core's debugger lists them.
    pub registers: Vec<VideoRegister>,
    /// What only this console has.
    pub console: C,
}

/// A layer of the held frame, or the chosen sprites, drawn again
/// ([`super::CaptureCore::render_layer`],
/// [`super::CaptureCore::render_sprites`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedLayer {
    /// The picture, RGBA: a pixel the layer covers is opaque, every other
    /// pixel 0 in all four bytes.
    pub image: PixelImage,
    /// Every line was drawn from the settings it was recorded with in the
    /// line table, not from the state as it stands.
    pub from_line_states: bool,
}
