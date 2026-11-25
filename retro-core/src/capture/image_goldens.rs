//! The golden-image harness: a core's program run from power-on to a list
//! of frames, each held at its capture boundary, and at each the images a
//! host reads of it ([`frame_images`]), held to the folder of an image set
//! the core's tests keep: the program, `program.bin`; a folder a frame,
//! `frame-00008/`, holding each image as a PNG; and `images.hashes`, a line
//! an image (its file, its size, the SHA-256 of its RGBA), its PNGs written
//! by [`super::png`].
//!
//! [`UPDATE_GOLDEN`] (`RETRO_UPDATE_GOLDEN=1`) writes the goldens again in
//! place of comparing them: an image changes only with a change meant to
//! change it, and is written again in its own commit, as the golden records
//! are. [`image_golden_tests!`](crate::image_golden_tests) writes one test
//! an image set in a core's test file.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::capture::images::{
    frame_folder, frame_images, FrameImage, ImageSubjects, IMAGE_SET_HASHES, IMAGE_SET_PROGRAM,
};
use crate::capture::png::{read_png, write_png};
use crate::capture::{CaptureCore, CaptureRecording};
use crate::golden::{GoldenConsole, GoldenEntry, GoldenSpec, GOLDEN_UNIX_TIME};
use crate::video::PixelImage;
use crate::InputState;

/// The variable that makes the harness write its goldens again in place of
/// comparing them, set to any value but `0`.
pub const UPDATE_GOLDEN: &str = "RETRO_UPDATE_GOLDEN";

/// Whether [`UPDATE_GOLDEN`] asks for the goldens to be written again.
pub fn updating() -> bool {
    asks_to_update(std::env::var_os(UPDATE_GOLDEN).as_deref())
}

/// Whether a value of [`UPDATE_GOLDEN`] asks for the goldens to be written
/// again: one that is set, not empty and not `0`.
fn asks_to_update(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty() && value != "0")
}

/// The images of one held frame ([`take_frames`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameImages {
    /// The frame, counted from 1 as a run of `--frames` counts them.
    pub frame: u64,
    /// Its images.
    pub images: Vec<FrameImage>,
}

/// Run `emu`, a machine with its program loaded and no frame run yet, frame
/// after frame up to the last of `frames` (counted from 1, in increasing
/// order), with the line table recorded from the first frame on: each
/// listed frame is held at its capture boundary, its images are taken there
/// ([`frame_images`], every one), and the frame is finished before the next
/// begins. `before_frame(emu, i)` runs before frame `i + 1` begins, where a
/// golden run applies its input and its resets. The line table stays
/// recorded after.
///
/// # Panics
///
/// When `frames` is not in increasing order from 1, or a frame stops before
/// its capture boundary.
pub fn take_frames<E: CaptureCore>(
    emu: &mut E,
    frames: &[u64],
    mut before_frame: impl FnMut(&mut E, u64),
) -> Vec<FrameImages> {
    assert!(
        frames.first().is_none_or(|&first| first >= 1)
            && frames.windows(2).all(|pair| pair[0] < pair[1]),
        "the frames are counted from 1, in increasing order: {frames:?}"
    );
    let recording = emu.capture_recording();
    emu.set_capture_recording(CaptureRecording {
        line_states: true,
        ..recording
    });
    let last = frames.last().copied().unwrap_or(0);
    let mut taken = Vec::with_capacity(frames.len());
    for i in 0..last {
        before_frame(emu, i);
        let frame = i + 1;
        if !frames.contains(&frame) {
            emu.run_frame();
            continue;
        }
        emu.run_to_capture_boundary();
        assert!(
            emu.at_capture_boundary(),
            "frame {frame} stopped before its capture boundary"
        );
        taken.push(FrameImages {
            frame,
            images: frame_images(emu, ImageSubjects::ALL),
        });
        if frame < last {
            emu.run_frame();
        }
    }
    taken
}

/// The SHA-256 of `image`'s RGBA, in lower-case hex.
pub fn rgba_hash(image: &PixelImage) -> String {
    Sha256::digest(&image.rgba)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The path of an image's PNG inside an image set's folder:
/// `frame-00008/map_plane_a.png`.
pub fn image_path(frame: u64, image: &FrameImage) -> String {
    format!("{}/{}.png", frame_folder(frame), image.name)
}

/// The text of an image set's `images.hashes`: two lines of comment, then
/// a line an image, `<path> <width>x<height> <sha256>` ([`image_path`],
/// [`rgba_hash`]), frame after frame and each frame's images in the order
/// they were taken.
pub fn hashes_text(taken: &[FrameImages]) -> String {
    let mut text = String::from(
        "# The images of each frame, held at its capture boundary: the file,\n\
         # the size and the SHA-256 of the RGBA of each.\n",
    );
    for frame in taken {
        for image in &frame.images {
            text.push_str(&format!(
                "{} {}x{} {}\n",
                image_path(frame.frame, image),
                image.image.width,
                image.image.height,
                rgba_hash(&image.image)
            ));
        }
    }
    text
}

/// Each difference between the images `taken` and the hashes `expected` (an
/// `images.hashes`) lists, a line each: an image they do not list, one they
/// list that was not taken, an image of another size or other pixels. Empty
/// when they agree.
pub fn hash_differences(expected: &str, taken: &[FrameImages]) -> Vec<String> {
    let listed = hash_lines(expected);
    let actual = hash_lines(&hashes_text(taken));
    let mut differences = Vec::new();
    for (path, rest) in &actual {
        match listed.iter().find(|(listed_path, _)| listed_path == path) {
            None => differences.push(format!("{path}: taken, not in the hashes")),
            Some((_, listed_rest)) if listed_rest != rest => {
                differences.push(format!("{path}: {rest}, the hashes name {listed_rest}"))
            }
            Some(_) => {}
        }
    }
    for (path, _) in &listed {
        if !actual.iter().any(|(actual_path, _)| actual_path == path) {
            differences.push(format!("{path}: in the hashes, not taken"));
        }
    }
    differences
}

/// The lines of an `images.hashes` text that name an image, each split at
/// its first space into the image's path and the rest (its size and hash).
fn hash_lines(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once(' '))
        .map(|(path, rest)| (path.to_string(), rest.to_string()))
        .collect()
}

/// Hold the images `taken` to the image set in `folder`: to its
/// `images.hashes`, and each to its PNG, which must hold the image its hash
/// names. With [`UPDATE_GOLDEN`] set, write the set's frame folders and its
/// hashes again in place of comparing them, the frame folders it held
/// before taken away.
///
/// # Panics
///
/// With every difference found, and the variable that writes the set again.
pub fn hold_image_set(folder: &Path, taken: &[FrameImages]) {
    if updating() {
        write_image_set(folder, taken).unwrap_or_else(|e| panic!("{e}"));
        let count: usize = taken.iter().map(|frame| frame.images.len()).sum();
        eprintln!("wrote {count} images to {}", folder.display());
        return;
    }
    let hashes = folder.join(IMAGE_SET_HASHES);
    let expected = std::fs::read_to_string(&hashes).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; {UPDATE_GOLDEN}=1 writes the set",
            hashes.display()
        )
    });
    let mut differences = hash_differences(&expected, taken);
    for frame in taken {
        for image in &frame.images {
            let path = image_path(frame.frame, image);
            match read_png(&folder.join(&path)) {
                Ok(png) if png == image.image => {}
                Ok(_) => differences.push(format!("{path}: the PNG holds other pixels")),
                Err(e) => differences.push(e),
            }
        }
    }
    assert!(
        differences.is_empty(),
        "the images differ from the set in {} ({UPDATE_GOLDEN}=1 writes it again):\n  {}",
        folder.display(),
        differences.join("\n  ")
    );
}

/// Write the frame folders and the hashes of an image set into `folder`,
/// made if missing, the frame folders it held before taken away.
pub fn write_image_set(folder: &Path, taken: &[FrameImages]) -> Result<(), String> {
    let error = |path: &Path, e: std::io::Error| format!("{}: {e}", path.display());
    std::fs::create_dir_all(folder).map_err(|e| error(folder, e))?;
    for entry in std::fs::read_dir(folder).map_err(|e| error(folder, e))? {
        let path = entry.map_err(|e| error(folder, e))?.path();
        let is_frame = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("frame-"));
        if is_frame && path.is_dir() {
            std::fs::remove_dir_all(&path).map_err(|e| error(&path, e))?;
        }
    }
    for frame in taken {
        let frame_dir = folder.join(frame_folder(frame.frame));
        std::fs::create_dir_all(&frame_dir).map_err(|e| error(&frame_dir, e))?;
        for image in &frame.images {
            write_png(&image.image, &folder.join(image_path(frame.frame, image)))?;
        }
    }
    let hashes = folder.join(IMAGE_SET_HASHES);
    std::fs::write(&hashes, hashes_text(taken)).map_err(|e| error(&hashes, e))
}

/// Run the program in `folder`'s `program.bin` on a new machine from
/// power-on to `frames` ([`take_frames`], no input), and hold its images to
/// the set in `folder` ([`hold_image_set`]).
///
/// # Panics
///
/// When the program cannot be read or loaded, and as [`hold_image_set`]
/// does.
pub fn check_image_set<E: CaptureCore + GoldenConsole>(folder: &Path, frames: &[u64]) {
    let path = folder.join(IMAGE_SET_PROGRAM);
    let program = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut emu = E::new_for_golden();
    emu.load_rom(&program)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let taken = take_frames(&mut emu, frames, |_, _| {});
    hold_image_set(folder, &taken);
}

/// Run a game of a golden manifest, its ROM `rom` loaded on a new machine
/// as the entry's flags say ([`GoldenConsole::load_for_golden`]), the
/// presses and the resets of `input` applied before each frame (the
/// entry's own, [`crate::golden::entry_spec_of`], or a run of the caller's;
/// past its last set-point the last stays held) and the real-time clock
/// pinned to [`GOLDEN_UNIX_TIME`], and hold the images of `frames`
/// ([`take_frames`]) to the hashes in `record`, the game's file of the
/// records' folder. With [`UPDATE_GOLDEN`] set, write `record` again, and
/// each image as a PNG in an image set's folder `pictures`, to look at. The
/// number of images taken, or each difference.
pub fn hold_game_images<E: CaptureCore + GoldenConsole>(
    entry: &GoldenEntry,
    rom: &[u8],
    input: &GoldenSpec,
    frames: &[u64],
    record: &Path,
    pictures: &Path,
) -> Result<usize, Vec<String>> {
    let mut emu = E::new_for_golden();
    emu.load_for_golden(entry, rom).map_err(|e| vec![e])?;
    let previous = E::pin_real_time_clock(Some(GOLDEN_UNIX_TIME));
    let taken = take_frames(&mut emu, frames, |emu, i| {
        if input.resets.contains(&i) {
            emu.reset();
        }
        if let Some(buttons) = input
            .presses
            .as_ref()
            .and_then(|presses| presses.mask_at(i))
        {
            emu.set_input(0, InputState { buttons });
        }
    });
    E::pin_real_time_clock(previous);
    let count = taken.iter().map(|frame| frame.images.len()).sum();
    if updating() {
        let error = |e: std::io::Error| vec![format!("{}: {e}", record.display())];
        if let Some(folder) = record.parent() {
            std::fs::create_dir_all(folder).map_err(error)?;
        }
        std::fs::write(record, hashes_text(&taken)).map_err(error)?;
        write_image_set(pictures, &taken).map_err(|e| vec![e])?;
        return Ok(count);
    }
    let expected =
        std::fs::read_to_string(record).map_err(|e| vec![format!("{}: {e}", record.display())])?;
    let differences = hash_differences(&expected, &taken);
    match differences.is_empty() {
        true => Ok(count),
        false => Err(differences),
    }
}

/// Hold `program` to the bytes of the file at `path`, an image set's
/// `program.bin`, so that what the tests run and what the tools and a host
/// load from it are one program; with [`UPDATE_GOLDEN`] set, write them
/// there.
///
/// # Panics
///
/// When the file cannot be read or written, or holds other bytes.
pub fn hold_program(path: &Path, program: &[u8]) {
    if updating() {
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder).unwrap_or_else(|e| panic!("{}: {e}", folder.display()));
        }
        std::fs::write(path, program).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        return;
    }
    let held = std::fs::read(path)
        .unwrap_or_else(|e| panic!("{}: {e}; {UPDATE_GOLDEN}=1 writes it", path.display()));
    assert!(
        held == program,
        "{} is not the program its generator writes ({UPDATE_GOLDEN}=1 writes it again)",
        path.display()
    );
}

/// Hold `image` to the PNG at `path`; with [`UPDATE_GOLDEN`] set, write it
/// there.
///
/// # Panics
///
/// When the PNG cannot be read or written, or holds another image.
pub fn hold_png(path: &Path, image: &PixelImage) {
    if updating() {
        write_png(image, path).unwrap_or_else(|e| panic!("{e}"));
        return;
    }
    let held = read_png(path).unwrap_or_else(|e| panic!("{e}; {UPDATE_GOLDEN}=1 writes it"));
    assert!(
        held == *image,
        "{} holds another image ({UPDATE_GOLDEN}=1 writes it again)",
        path.display()
    );
}

/// Write one test an image set in a core's test file, each running
/// [`check_image_set`] on the set's folder, `tests/golden/images/<set>/` in
/// the crate, from power-on to the frames it lists:
///
/// ```text
/// retro_core::image_golden_tests!(MyEmulator;
///     conformance => [4, 5, 10],
///     mode7 => [3, 30],
/// );
/// ```
///
/// Each test is named after its set. The core's test crate enables
/// `retro-core`'s `image-goldens` feature, among its dev-dependencies.
#[macro_export]
macro_rules! image_golden_tests {
    ($emulator:ty; $($set:ident => [$($frame:expr),+ $(,)?]),+ $(,)?) => {
        $(
            #[test]
            fn $set() {
                $crate::capture::image_goldens::check_image_set::<$emulator>(
                    &::std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/golden/images")
                        .join(stringify!($set)),
                    &[$($frame),+],
                );
            }
        )+
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::images::ImageKind;

    fn an_image(name: &str, width: u32, fill: u8) -> FrameImage {
        FrameImage {
            name: name.to_string(),
            kind: ImageKind::Map,
            about: String::new(),
            image: PixelImage {
                width,
                height: 1,
                rgba: vec![fill; width as usize * 4],
            },
        }
    }

    fn taken() -> Vec<FrameImages> {
        vec![
            FrameImages {
                frame: 4,
                images: vec![an_image("picture", 2, 1), an_image("map_bg1", 1, 2)],
            },
            FrameImages {
                frame: 10,
                images: vec![an_image("picture", 2, 3)],
            },
        ]
    }

    #[test]
    fn the_hashes_name_each_image_its_size_and_the_hash_of_its_rgba() {
        let text = hashes_text(&taken());
        let lines: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[0],
            format!(
                "frame-00004/picture.png 2x1 {}",
                rgba_hash(&taken()[0].images[0].image)
            )
        );
        assert!(lines[1].starts_with("frame-00004/map_bg1.png 1x1 "));
        assert!(lines[2].starts_with("frame-00010/picture.png 2x1 "));
        // The SHA-256 of eight bytes of 1.
        assert_eq!(
            rgba_hash(&taken()[0].images[0].image),
            "04abc8821a06e5a30937967d11ad10221cb5ac3b5273e434f1284ee87129a061"
        );
    }

    #[test]
    fn the_hashes_agree_with_the_images_they_were_written_from() {
        let taken = taken();
        assert!(hash_differences(&hashes_text(&taken), &taken).is_empty());
    }

    #[test]
    fn a_difference_names_the_image_and_what_differs() {
        let expected = hashes_text(&taken());
        let mut other = taken();
        other[0].images[1].image.rgba[0] ^= 1;
        other[1].images.push(an_image("palette", 1, 0));
        other[0].images.remove(0);
        let differences = hash_differences(&expected, &other);
        assert_eq!(differences.len(), 3, "{differences:#?}");
        assert!(differences[0].starts_with("frame-00004/map_bg1.png: 1x1 "));
        assert!(differences[0].contains(", the hashes name 1x1 "));
        assert_eq!(
            differences[1],
            "frame-00010/palette.png: taken, not in the hashes"
        );
        assert_eq!(
            differences[2],
            "frame-00004/picture.png: in the hashes, not taken"
        );
    }

    #[test]
    fn the_update_switch_is_any_value_but_0() {
        let reads = |value: Option<&str>| asks_to_update(value.map(std::ffi::OsStr::new));
        assert!(reads(Some("1")) && reads(Some("yes")));
        assert!(!reads(Some("0")) && !reads(Some("")) && !reads(None));
    }
}
