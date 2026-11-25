//! The PNG files of a held frame's images ([`super::images`]): RGBA with
//! its alpha, compressed as hard as the encoder goes, so that the images a
//! repository keeps stay small. What writes an image through
//! [`write_png`] writes the same bytes of the same image.

use std::path::Path;

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};

use crate::video::PixelImage;

/// `image` as the bytes of a PNG.
pub fn encode_png(image: &PixelImage) -> Result<Vec<u8>, String> {
    let pixels = u64::from(image.width) * u64::from(image.height);
    if image.rgba.len() as u64 != pixels * 4 {
        return Err(format!(
            "a {}x{} image of {} bytes",
            image.width,
            image.height,
            image.rgba.len()
        ));
    }
    let mut bytes = Vec::new();
    PngEncoder::new_with_quality(&mut bytes, CompressionType::Best, FilterType::Adaptive)
        .write_image(
            &image.rgba,
            image.width,
            image.height,
            ExtendedColorType::Rgba8,
        )
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Write `image` to `path` as a PNG ([`encode_png`]).
pub fn write_png(image: &PixelImage, path: &Path) -> Result<(), String> {
    let bytes = encode_png(image).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// The image the PNG at `path` holds, as RGBA.
pub fn read_png(path: &Path) -> Result<PixelImage, String> {
    let png = image::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .to_rgba8();
    Ok(PixelImage {
        width: png.width(),
        height: png.height(),
        rgba: png.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_png_holds_the_image_it_was_written_from() {
        let path = std::env::temp_dir().join(format!("retro-core-png-{}.png", std::process::id()));
        let image = PixelImage {
            width: 3,
            height: 2,
            rgba: (0..24).map(|byte| byte * 10).collect(),
        };
        write_png(&image, &path).expect("written");
        assert_eq!(read_png(&path).expect("read"), image);
        assert_eq!(
            std::fs::read(&path).expect("the file"),
            encode_png(&image).expect("encoded"),
            "the file is the image's encoding"
        );
        std::fs::remove_file(&path).expect("removed");
        assert!(read_png(&path).is_err(), "a PNG that is not there");
    }

    #[test]
    fn an_image_of_the_wrong_length_is_refused() {
        let image = PixelImage {
            width: 2,
            height: 2,
            rgba: vec![0; 12],
        };
        assert_eq!(
            encode_png(&image),
            Err("a 2x2 image of 12 bytes".to_string())
        );
    }
}
