//! The PNGs the commands write: the picture a console shows, and an RGBA
//! image (a layer drawn alone).

use std::path::Path;

use anyhow::{Context, Result};
use retro_core::golden::crop_framebuffer_rgb;
use retro_core::video::PixelImage;
use retro_core::EmulatorCore;

/// Write the picture the machine shows to `path` as an RGB PNG: the
/// framebuffer cut to the display info, as the golden harness hashes it.
/// A console whose framebuffer is wider or taller than what it shows (a
/// buffer kept at its largest mode, a frame of more lines than are drawn)
/// saves only what it shows. Its width and height.
pub fn write_frame_png<E: EmulatorCore>(emu: &E, path: &Path) -> Result<(u32, u32)> {
    let (shown_width, shown_height) = {
        let display = emu.get_display_info();
        (display.width, display.height)
    };
    let fb = emu.get_framebuffer();
    if fb.data.is_empty() || fb.width == 0 || fb.height == 0 {
        anyhow::bail!("framebuffer is empty ({}x{})", fb.width, fb.height);
    }
    let (width, height, rgb) =
        crop_framebuffer_rgb(&fb.data, fb.width, fb.height, shown_width, shown_height);
    let image = ::image::RgbImage::from_raw(width, height, rgb).ok_or_else(|| {
        anyhow::anyhow!("failed to build {width}x{height} image from framebuffer")
    })?;
    image
        .save(path)
        .with_context(|| format!("failed to save screenshot: {}", path.display()))?;
    Ok((width, height))
}

/// Write an RGBA image to `path` as a PNG with its alpha.
pub fn write_rgba_png(image: &PixelImage, path: &Path) -> Result<()> {
    let (width, height) = (image.width, image.height);
    let png = ::image::RgbaImage::from_raw(width, height, image.rgba.clone())
        .ok_or_else(|| anyhow::anyhow!("failed to build a {width}x{height} image"))?;
    png.save(path)
        .with_context(|| format!("failed to save {}", path.display()))?;
    Ok(())
}
