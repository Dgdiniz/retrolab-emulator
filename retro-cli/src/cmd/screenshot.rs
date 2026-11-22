//! `screenshot`: run a ROM for a number of frames and save the picture the
//! console shows as a PNG; optionally the layers hidden or one isolated
//! first, each layer of the picture's frame written alone, and the
//! cartridge's chips printed after.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use retro_core::capture::{CaptureCore, CaptureRecording};
use retro_core::EmulatorCore;

use crate::console::Console;
use crate::image::{write_frame_png, write_rgba_png};
use crate::input::{InputArgs, RunEnd};
use crate::progress;
use crate::report::write_chips;
use crate::rom::{load, write_banner, RomArgs};

#[derive(clap::Args, Debug)]
pub struct ScreenshotArgs<B: clap::Args, S: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Number of frames to run before capturing.
    #[arg(long, short)]
    pub frames: Option<u64>,
    /// Output PNG file path.
    #[arg(long, short, default_value = "screenshot.png")]
    pub output: PathBuf,
    /// Show a progress bar (on stderr).
    #[arg(long)]
    pub progress: bool,
    #[command(flatten)]
    pub input: InputArgs,
    /// Isolate a single layer: render only this one.
    #[arg(long)]
    pub show_only: Option<u32>,
    /// Hide these layers (comma-separated ids), keeping the rest.
    #[arg(long)]
    pub hide: Option<String>,
    /// Also write each layer alone beside the screenshot.
    #[arg(long, conflicts_with_all = ["show_only", "hide"])]
    pub render_layers: bool,
    /// Show the lines the console draws past its picture's usual height.
    #[arg(long)]
    pub overscan: bool,
    /// After capturing, print the installed coprocessor(s) and their live
    /// state.
    #[arg(long)]
    pub coprocessor_state: bool,
    #[command(flatten)]
    pub console: S,
}

pub fn run<C: Console>(args: ScreenshotArgs<C::BootArgs, C::ScreenshotArgs>) -> Result<()> {
    let Some(frames) = args.frames else {
        anyhow::bail!("specify --frames");
    };
    let input = args.input.plan::<C::Emulator>()?;

    let (_, mut emu) = load::<C>(&args.rom)?;
    write_banner(&mut emu, &mut std::io::stdout())?;
    let shown = layers_shown(&mut emu, args.show_only, args.hide.as_deref())?;
    emu.set_show_overscan(args.overscan);
    // The layers are drawn line by line from the state each line was drawn
    // with: record it from the first frame on. Recording changes nothing
    // the game does, so the picture is the same either way.
    if args.render_layers {
        emu.set_capture_recording(CaptureRecording {
            line_states: true,
            ..CaptureRecording::default()
        });
    }

    // The layers come from the screenshot's frame, held at its capture
    // boundary: its picture is the one the whole frame leaves, and its
    // video memory has not taken the next frame's uploads yet.
    let end = if args.render_layers {
        RunEnd::CaptureBoundary
    } else {
        RunEnd::WholeFrame
    };
    let bar = progress::bar(args.progress, frames, "frames");
    for frame in 0..frames {
        let last = frame + 1 == frames;
        if last {
            C::before_last_frame(&mut emu, &args.console)?;
        }
        input.before_frame(&mut emu, frame);
        end.run_frame(&mut emu, frame, frames);
        if let Some(bar) = &bar {
            bar.inc(1);
        }
    }
    progress::finish(bar);
    C::after_frames(&mut emu, &args.console)?;

    // Layers hidden by the debugger stay hidden in every frame drawn after,
    // and the game's own writes do not touch them: one more frame draws the
    // picture without them.
    if let Some(shown) = shown {
        let debugger = emu.video_debugger();
        for (layer, visible) in (0u32..).zip(shown) {
            debugger.toggle_layer(layer, visible);
        }
        emu.run_frame();
    }

    for note in C::picture_notes(&emu, frames) {
        eprintln!("warning: {note}");
    }
    let (width, height) = write_frame_png(&emu, &args.output)?;
    println!(
        "Saved {width}x{height} screenshot to {} (after {frames} frames)",
        args.output.display()
    );
    if args.render_layers {
        write_layers::<C>(&mut emu, &args.output)?;
    }
    if args.coprocessor_state {
        println!();
        write_chips(&mut emu, &mut std::io::stdout())?;
    }
    Ok(())
}

/// Change the machine once a screenshot's frames are done, and draw the
/// picture again by one more frame. A frame held at its capture boundary
/// is finished first, so the change lands where a whole frame leaves the
/// machine, and the new frame is held as that one was.
pub fn draw_again<E: CaptureCore>(emu: &mut E, change: impl FnOnce(&mut E)) {
    let held = emu.at_capture_boundary();
    if held {
        emu.run_frame();
    }
    change(emu);
    if held {
        emu.run_to_capture_boundary();
    } else {
        emu.run_frame();
    }
}

/// Whether each layer of the machine stays shown, by id, under
/// `--show-only` or `--hide`; `None` when neither changes anything.
fn layers_shown<E: EmulatorCore>(
    emu: &mut E,
    show_only: Option<u32>,
    hide: Option<&str>,
) -> Result<Option<Vec<bool>>> {
    let count = emu.video_debugger().get_layer_count();
    shown_of(count, show_only, hide)
}

/// Whether each of `count` layers stays shown, by id, under `--show-only`
/// or `--hide`; `None` when neither changes anything. A value that is not
/// a layer id is refused.
fn shown_of(count: u32, show_only: Option<u32>, hide: Option<&str>) -> Result<Option<Vec<bool>>> {
    let layers = format!("the layers are 0 to {}", count.saturating_sub(1));
    if let Some(only) = show_only {
        if only >= count {
            anyhow::bail!("--show-only {only}: not a layer ({layers})");
        }
        return Ok(Some((0..count).map(|layer| layer == only).collect()));
    }
    let mut hidden = Vec::new();
    for item in hide.unwrap_or_default().split(',').map(str::trim) {
        if item.is_empty() {
            continue;
        }
        match item.parse::<u32>() {
            Ok(layer) if layer < count => hidden.push(layer),
            _ => anyhow::bail!("--hide {item}: not a layer ({layers})"),
        }
    }
    if hidden.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        (0..count).map(|layer| !hidden.contains(&layer)).collect(),
    ))
}

/// The file suffix of a layer named `name`: `Plane A` -> `plane_a`.
pub(crate) fn layer_suffix(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace(' ', "_")
}

/// Write each layer of the held frame alone, then the console's other
/// images of it, beside `output`: `<stem>_<layer>.<ext>`. Nothing runs
/// between them, so every one comes from the frame of the screenshot, each
/// line from the line table the run recorded.
fn write_layers<C: Console>(emu: &mut C::Emulator, output: &Path) -> Result<()> {
    let stem = output
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "screenshot".to_string());
    let ext = output
        .extension()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "png".to_string());
    let parent = output.parent().unwrap_or_else(|| Path::new("."));

    let count = emu.video_debugger().get_layer_count();
    let mut images = Vec::new();
    let mut from_line_states = true;
    for layer in 0..count {
        let suffix = emu
            .video_debugger()
            .get_layer_data(layer)
            .map_or_else(|| format!("layer{layer}"), |data| layer_suffix(&data.name));
        let rendered = emu
            .render_layer(layer)
            .ok_or_else(|| anyhow::anyhow!("failed to render layer {suffix}: no such layer"))?;
        from_line_states &= rendered.from_line_states;
        images.push((suffix, rendered.image));
    }
    images.extend(
        C::extra_layer_images(emu)
            .into_iter()
            .map(|(suffix, image)| (suffix.to_string(), image)),
    );

    let source = if from_line_states {
        "drawn from the frame's line states"
    } else {
        "drawn from the registers as they stand"
    };
    println!("Layers {source}");
    for (suffix, image) in images {
        let path = parent.join(format!("{stem}_{suffix}.{ext}"));
        write_rgba_png(&image, &path).with_context(|| format!("layer {suffix}"))?;
        println!(
            "Saved {}x{} layer {suffix} to {}",
            image.width,
            image.height,
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_only_and_hide_name_layers_by_id_and_refuse_any_other_value() {
        assert_eq!(shown_of(5, None, None).unwrap(), None);
        assert_eq!(shown_of(5, None, Some("")).unwrap(), None);
        assert_eq!(
            shown_of(5, Some(4), None).unwrap(),
            Some(vec![false, false, false, false, true])
        );
        assert_eq!(
            shown_of(4, None, Some("0, 2,")).unwrap(),
            Some(vec![false, true, false, true])
        );
        let err = |only, hide| shown_of(4, only, hide).unwrap_err().to_string();
        assert_eq!(
            err(Some(4), None),
            "--show-only 4: not a layer (the layers are 0 to 3)"
        );
        assert_eq!(
            err(None, Some("1,x")),
            "--hide x: not a layer (the layers are 0 to 3)"
        );
        assert_eq!(
            err(None, Some("9")),
            "--hide 9: not a layer (the layers are 0 to 3)"
        );
    }

    #[test]
    fn a_layer_file_is_named_after_its_layer() {
        assert_eq!(layer_suffix("Plane A"), "plane_a");
        assert_eq!(layer_suffix("BG1"), "bg1");
        assert_eq!(layer_suffix("OBJ"), "obj");
        assert_eq!(layer_suffix("Sprites"), "sprites");
    }
}
