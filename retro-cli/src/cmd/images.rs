//! `images`: run a ROM for a number of frames, the last held at its capture
//! boundary, and write the images the video chip's debugger gives of that
//! frame as PNGs: the picture, each layer alone, each layer's whole map,
//! each sprite the frame drew and those sprites together, the video
//! memory's characters in each palette line in use, and the palette; with
//! `--raw`, each video memory's bytes too. `images.json` lists every file,
//! its size and what it is. The images and their names are the ones
//! `retro_core::capture::images::frame_images` gives, and the PNGs are
//! written by `retro_core::capture::png`, as the golden-image harness writes
//! an image set's.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use retro_core::capture::images::{
    folder_frame, frame_folder, frame_images, ImageSubjects, IMAGE_SET_PROGRAM,
};
use retro_core::capture::png::write_png;
use retro_core::capture::{CaptureCore, CaptureRecording};
use serde_json::{json, Value};

use crate::console::Console;
use crate::input::{InputArgs, InputPlan, RunEnd};
use crate::rom::{load, write_banner, RomArgs};

#[derive(clap::Args, Debug)]
pub struct ImagesArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Frames to run; the images are of the last.
    #[arg(long, short)]
    pub frames: Option<u64>,
    /// The folder the files go to, made if missing.
    #[arg(long, short)]
    pub output: PathBuf,
    /// Each layer drawn alone.
    #[arg(long)]
    pub layers: bool,
    /// Each layer's whole map.
    #[arg(long)]
    pub maps: bool,
    /// Each sprite the frame drew, and those sprites together.
    #[arg(long)]
    pub sprites: bool,
    /// The video memory's characters in each palette line in use.
    #[arg(long)]
    pub tiles: bool,
    /// The palette.
    #[arg(long)]
    pub palette: bool,
    /// Also write each video memory's bytes.
    #[arg(long)]
    pub raw: bool,
    #[command(flatten)]
    pub input: InputArgs,
}

impl<B: clap::Args> ImagesArgs<B> {
    /// The images asked for: those the flags name, or every one when they
    /// name none.
    pub fn subjects(&self) -> ImageSubjects {
        let named = ImageSubjects {
            layers: self.layers,
            maps: self.maps,
            sprites: self.sprites,
            tiles: self.tiles,
            palette: self.palette,
        };
        match named == ImageSubjects::default() {
            true => ImageSubjects::ALL,
            false => named,
        }
    }
}

/// One file `images` wrote, as `images.json` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrittenFile {
    /// Its name in the output folder.
    pub file: String,
    /// What kind of file it is: an image's kind (`map`), or `memory`.
    pub kind: &'static str,
    /// An image's width and height in pixels; `None` for a memory.
    pub size: Option<(u32, u32)>,
    /// Its length in bytes.
    pub bytes: u64,
    /// What it is, in words.
    pub about: String,
}

pub fn run<C: Console>(args: ImagesArgs<C::BootArgs>) -> Result<()> {
    let Some(frames) = args.frames else {
        anyhow::bail!("specify --frames");
    };
    let input = args.input.plan::<C::Emulator>()?;
    let (_, mut emu) = load::<C>(&args.rom)?;
    write_banner(&mut emu, &mut std::io::stdout())?;
    let written = write_images(
        &mut emu,
        &input,
        frames,
        args.subjects(),
        args.raw,
        &args.output,
    )?;
    for file in &written {
        let path = args.output.join(&file.file);
        match file.size {
            Some((width, height)) => {
                println!("Saved {width}x{height} {} to {}", file.kind, path.display())
            }
            None => println!(
                "Saved {} bytes of video memory to {}",
                file.bytes,
                path.display()
            ),
        }
    }
    println!(
        "Wrote {} files to {}, listed in images.json, of frame {frames}, held at its capture \
         boundary",
        written.len(),
        args.output.display()
    );
    Ok(())
}

/// Run `frames` frames of `emu`, a machine with its ROM loaded and no frame
/// run yet, with `input` applied before each and the line table recorded
/// from the first on, the last frame held at its capture boundary; then
/// write the images `subjects` asks for of that frame into `folder`, made if
/// missing, each `<name>.png` as [`frame_images`] names it, with `raw` each
/// video memory's bytes as `<name>.bin` in the console's name of the memory,
/// and `images.json`. The files written, `images.json` left out.
pub fn write_images<E: CaptureCore>(
    emu: &mut E,
    input: &InputPlan,
    frames: u64,
    subjects: ImageSubjects,
    raw: bool,
    folder: &Path,
) -> Result<Vec<WrittenFile>> {
    if frames == 0 {
        anyhow::bail!("--frames 0 runs no frame: the images are of the last frame run");
    }
    emu.set_capture_recording(CaptureRecording {
        line_states: true,
        ..CaptureRecording::default()
    });
    input.run_frames(emu, frames, RunEnd::CaptureBoundary);
    let held = emu.at_capture_boundary();
    std::fs::create_dir_all(folder)
        .with_context(|| format!("failed to make the folder {}", folder.display()))?;

    let mut written = Vec::new();
    for image in frame_images(emu, subjects) {
        let file = format!("{}.png", image.name);
        let path = folder.join(&file);
        write_png(&image.image, &path).map_err(anyhow::Error::msg)?;
        written.push(WrittenFile {
            file,
            kind: image.kind.name(),
            size: Some((image.image.width, image.image.height)),
            bytes: file_length(&path)?,
            about: image.about,
        });
    }
    if raw {
        for memory in emu.video_debugger().video_memories() {
            let Some(bytes) = emu.video_debugger().read_video_memory(memory.id) else {
                continue;
            };
            let file = format!("{}.bin", memory.name.to_ascii_lowercase());
            let path = folder.join(&file);
            std::fs::write(&path, &bytes)
                .with_context(|| format!("failed to write {}", path.display()))?;
            let order = match memory.little_endian {
                true => "the low byte of a 16-bit word first",
                false => "the high byte of a 16-bit word first",
            };
            written.push(WrittenFile {
                file,
                kind: "memory",
                size: None,
                bytes: bytes.len() as u64,
                about: format!(
                    "{}, its {} bytes as the core keeps them, {order}",
                    memory.name,
                    bytes.len()
                ),
            });
        }
    }
    let list = images_json(frames, held, &written);
    let path = folder.join("images.json");
    let text = serde_json::to_string_pretty(&list)?;
    std::fs::write(&path, text + "\n")
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(written)
}

/// The length of the file at `path`.
fn file_length(path: &Path) -> Result<u64> {
    Ok(std::fs::metadata(path)
        .with_context(|| format!("failed to read {}", path.display()))?
        .len())
}

/// What `images.json` holds: the frames run, whether the last was held at
/// its capture boundary, and each file written.
fn images_json(frames: u64, held: bool, written: &[WrittenFile]) -> Value {
    let files: Vec<Value> = written
        .iter()
        .map(|file| {
            let mut entry = json!({ "file": file.file, "kind": file.kind });
            if let Some((width, height)) = file.size {
                entry["width"] = json!(width);
                entry["height"] = json!(height);
            }
            entry["bytes"] = json!(file.bytes);
            entry["about"] = json!(file.about);
            entry
        })
        .collect();
    json!({
        "frames": frames,
        "held_at_capture_boundary": held,
        "files": files,
    })
}

/// The frames of the image set in the folder `set`: the number of each of
/// its frame folders (`frame-00008`), in increasing order.
pub fn image_set_frames(set: &Path) -> Result<Vec<u64>> {
    let mut frames: Vec<u64> = std::fs::read_dir(set)
        .with_context(|| format!("failed to read {}", set.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| folder_frame(&entry.file_name().to_string_lossy()))
        .collect();
    frames.sort_unstable();
    Ok(frames)
}

/// Hold this command to the image set in the folder `set`: run on its
/// `program.bin` to each of its frames, with no input and every image
/// asked for, into a folder of `scratch` a frame, each PNG it writes must
/// be the set's file of the same frame and name, byte for byte, and it must
/// write every one the set holds. The error lists each difference.
pub fn check_against_image_set<C: Console>(set: &Path, scratch: &Path) -> Result<()> {
    let frames = image_set_frames(set)?;
    if frames.is_empty() {
        anyhow::bail!("{} holds no frame folder", set.display());
    }
    let rom = RomArgs::new(set.join(IMAGE_SET_PROGRAM));
    let mut differences = Vec::new();
    for frame in frames {
        let (_, mut emu) = load::<C>(&rom)?;
        let folder = scratch.join(frame_folder(frame));
        let written = write_images(
            &mut emu,
            &InputPlan::default(),
            frame,
            ImageSubjects::ALL,
            false,
            &folder,
        )?;
        let golden = set.join(frame_folder(frame));
        let mut held: Vec<String> = std::fs::read_dir(&golden)
            .with_context(|| format!("failed to read {}", golden.display()))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".png"))
            .collect();
        held.sort_unstable();
        let mut wrote: Vec<String> = written.into_iter().map(|file| file.file).collect();
        wrote.sort_unstable();
        for name in &held {
            if !wrote.contains(name) {
                differences.push(format!("frame {frame}: {name} is in the set, not written"));
            }
        }
        for name in &wrote {
            if !held.contains(name) {
                differences.push(format!("frame {frame}: {name} was written, not in the set"));
                continue;
            }
            let ours = std::fs::read(folder.join(name))?;
            let theirs = std::fs::read(golden.join(name))?;
            if ours != theirs {
                differences.push(format!("frame {frame}: {name} differs from the set's"));
            }
        }
    }
    match differences.is_empty() {
        true => Ok(()),
        false => anyhow::bail!(
            "images differ from the set in {}:\n  {}",
            set.display(),
            differences.join("\n  ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(clap::Args, Clone, Debug, Default)]
    struct NoBoot {}

    fn args(flags: &[&str]) -> ImagesArgs<NoBoot> {
        #[derive(clap::Parser)]
        struct Command {
            #[command(flatten)]
            images: ImagesArgs<NoBoot>,
        }
        let mut words = vec!["images", "game.bin", "--frames", "1", "-o", "out"];
        words.extend_from_slice(flags);
        <Command as clap::Parser>::parse_from(words).images
    }

    #[test]
    fn every_image_is_asked_for_when_no_flag_names_one() {
        assert_eq!(args(&[]).subjects(), ImageSubjects::ALL);
        assert_eq!(args(&["--raw"]).subjects(), ImageSubjects::ALL);
        let maps = args(&["--maps", "--palette"]).subjects();
        assert!(maps.maps && maps.palette);
        assert!(!maps.layers && !maps.sprites && !maps.tiles);
    }

    #[test]
    fn the_list_gives_each_file_its_kind_size_and_bytes() {
        let written = [
            WrittenFile {
                file: "picture.png".to_string(),
                kind: "picture",
                size: Some((320, 224)),
                bytes: 1234,
                about: "The picture".to_string(),
            },
            WrittenFile {
                file: "vram.bin".to_string(),
                kind: "memory",
                size: None,
                bytes: 65536,
                about: "VRAM".to_string(),
            },
        ];
        let list = images_json(8, true, &written);
        assert_eq!(list["frames"], 8);
        assert_eq!(list["held_at_capture_boundary"], true);
        assert_eq!(
            list["files"][0],
            json!({ "file": "picture.png", "kind": "picture", "width": 320, "height": 224,
                    "bytes": 1234, "about": "The picture" })
        );
        assert_eq!(
            list["files"][1],
            json!({ "file": "vram.bin", "kind": "memory", "bytes": 65536, "about": "VRAM" })
        );
    }

    #[test]
    fn a_set_s_frames_are_its_frame_folders_in_order() {
        let set = std::env::temp_dir().join(format!("retro-cli-image-set-{}", std::process::id()));
        for folder in ["frame-00010", "frame-00004", "other"] {
            std::fs::create_dir_all(set.join(folder)).unwrap();
        }
        std::fs::write(set.join("frame-00099"), b"a file, not a folder").unwrap();
        std::fs::write(set.join(IMAGE_SET_PROGRAM), b"").unwrap();
        assert_eq!(image_set_frames(&set).unwrap(), [4, 10]);
        std::fs::remove_dir_all(&set).unwrap();
        assert!(
            image_set_frames(&set).is_err(),
            "a folder that is not there"
        );
    }
}
